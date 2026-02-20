use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    send_disconnect, EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus,
    ProtocolHandler, RecordingConfig,
};
use log::{debug, info, warn};
use scylla::frame::response::result::CqlValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::csv_export::{generate_csv_filename, CsvExporter};
use crate::handler_helpers::{
    handle_quit, parse_display_size, render_connection_error, render_connection_success,
    render_help, send_render, HelpSection,
};
use crate::query_executor::QueryExecutor;
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
};
use crate::security::DatabaseSecuritySettings;

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(6000);

/// Cassandra/ScyllaDB handler
///
/// Provides interactive CQL shell access for Cassandra and ScyllaDB clusters.
/// Uses the scylla-rust-driver which is compatible with both Cassandra and ScyllaDB.
pub struct CassandraHandler {
    config: CassandraConfig,
}

#[derive(Debug, Clone)]
pub struct CassandraConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub require_auth: bool,
    pub connection_timeout_secs: u64,
}

impl Default for CassandraConfig {
    fn default() -> Self {
        Self {
            default_port: 9042,
            require_tls: false,
            require_auth: false, // Cassandra can run without auth
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl CassandraHandler {
    pub fn new(config: CassandraConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(CassandraConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for CassandraHandler {
    fn name(&self) -> &str {
        "cassandra"
    }

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("Cassandra handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("Cassandra: Read-only mode enabled");
        }

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);

        // Parse connection parameters
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;
        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);
        let username = params.get("username");
        let password = params.get("password");
        let keyspace = params.get("keyspace").or(params.get("database"));

        // Security check
        if self.config.require_auth && (username.is_none() || password.is_none()) {
            return Err(HandlerError::MissingParameter(
                "username and password (required for security)".to_string(),
            ));
        }

        info!("Cassandra: Connecting to {}:{}", hostname, port);

        // Parse display size from parameters (like SSH does)
        let (width, height, cols, rows) = parse_display_size(&params);

        info!(
            "Cassandra: Display size {}x{} px -> {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with CQL prompt and correct dimensions
        let prompt = if security.base.read_only {
            "cql [RO]> "
        } else {
            "cql> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "cassandra", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "Cassandra", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("Cassandra: Sent display init instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Build session with scylla driver
        let contact_point = format!("{}:{}", hostname, port);
        let mut session_builder = scylla::SessionBuilder::new().known_node(&contact_point);

        // Add authentication if credentials are provided
        if let (Some(user), Some(pass)) = (username, password) {
            session_builder = session_builder.user(user, pass);
        }

        // Add default keyspace if provided
        if let Some(ks) = keyspace {
            if !ks.is_empty() {
                session_builder = session_builder.use_keyspace(ks, false);
            }
        }

        // Connect to Cassandra/ScyllaDB
        let session = match tokio::time::timeout(
            std::time::Duration::from_secs(self.config.connection_timeout_secs),
            session_builder.build(),
        )
        .await
        {
            Ok(Ok(session)) => {
                info!("Cassandra: Connected successfully");

                let conn_line = format!("Connected to Cassandra/ScyllaDB at {}:{}", hostname, port);
                let ks_line = keyspace
                    .filter(|ks| !ks.is_empty())
                    .map(|ks| format!("Keyspace: {}", ks));
                let mut info_lines: Vec<&str> = vec![&conn_line];
                if let Some(ref ks_l) = ks_line {
                    info_lines.push(ks_l);
                }
                info_lines.push("Type 'help' for available commands.");
                render_connection_success(&mut executor, &to_client, &info_lines, &mut recorder)
                    .await?;
                debug!("Cassandra: Initial screen sent successfully");

                session
            }
            Ok(Err(e)) => {
                let error_msg = format!("Cassandra connection failed: {}", e);
                warn!("Cassandra: {}", error_msg);

                render_connection_error(
                    &mut executor,
                    &to_client,
                    &mut from_client,
                    &error_msg,
                    &[
                        "Check hostname and port (default: 9042)",
                        "Verify Cassandra/ScyllaDB is running",
                        "Check authentication credentials",
                        "Check firewall rules",
                    ],
                    &mut recorder,
                )
                .await?;
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
            Err(_) => {
                let error_msg = format!(
                    "Connection timed out after {}s",
                    self.config.connection_timeout_secs
                );
                warn!("Cassandra: {}", error_msg);

                executor
                    .terminal
                    .write_error(&error_msg)
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_prompt()
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                send_render(&mut executor, &to_client, &mut recorder).await?;

                while from_client.recv().await.is_some() {}
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
        };

        // Track current keyspace for USE commands
        let mut current_keyspace: Option<String> =
            keyspace.and_then(|k| if k.is_empty() { None } else { Some(k.clone()) });

        // Event loop
        // NOTE: Screen was already rendered above after connection success

        // Debounce timer for batching screen updates (60 FPS)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        'outer: loop {
            tokio::select! {
                // Debounce tick - render if terminal changed
                _ = debounce.tick() => {
                    // Check if client is still connected before rendering
                    if to_client.is_closed() {
                        debug!("Cassandra: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if crate::recording::send_and_record(&to_client, &mut recorder, instr).await.is_err() {
                                debug!("Cassandra: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Cassandra: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(command) = pending_query {
                        info!("Cassandra: Executing command: {}", command);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &command);

                        // Handle built-in commands
                        if handle_builtin_command(
                            &command,
                            &mut executor,
                            &to_client,
                            &security,
                            &mut recorder,
                        )
                        .await?
                        {
                            continue;
                        }

                        // Check for export command: \e <query>
                        if command.to_lowercase().starts_with("\\e ") {
                            let query = command[3..].trim();
                            handle_csv_export(
                                query,
                                &session,
                                &mut executor,
                                &to_client,
                                &security,
                                &mut recorder,
                            )
                            .await?;
                            continue;
                        }

                        // Check for import command: \i <table_name>
                        if command.to_lowercase().starts_with("\\i") {
                            let table_name = command[2..].trim();
                            handle_csv_import(
                                table_name,
                                &session,
                                &mut executor,
                                &to_client,
                                &security,
                                &current_keyspace,
                                &mut recorder,
                            )
                            .await?;
                            continue;
                        }

                        // Handle USE <keyspace> command
                        let command_upper = command.trim().to_uppercase();
                        if command_upper.starts_with("USE ") {
                            let ks_name =
                                command.trim()[4..].trim().trim_end_matches(';').trim();
                            if !ks_name.is_empty() {
                                match session
                                    .query_unpaged(format!("USE {}", ks_name), &[])
                                    .await
                                {
                                    Ok(_) => {
                                        current_keyspace = Some(ks_name.to_string());
                                        executor
                                            .terminal
                                            .write_line(&format!(
                                                "Now using keyspace '{}'",
                                                ks_name
                                            ))
                                            .map_err(|e| {
                                                HandlerError::ProtocolError(e.to_string())
                                            })?;
                                    }
                                    Err(e) => {
                                        record_error_output(
                                            &mut recorder,
                                            &format!("{}", e),
                                        );
                                        executor
                                            .terminal
                                            .write_error(&format!(
                                                "Failed to use keyspace '{}': {}",
                                                ks_name, e
                                            ))
                                            .map_err(|e| {
                                                HandlerError::ProtocolError(e.to_string())
                                            })?;
                                    }
                                }
                                executor
                                    .terminal
                                    .write_prompt()
                                    .map_err(|e| {
                                        HandlerError::ProtocolError(e.to_string())
                                    })?;
                                send_render(&mut executor, &to_client, &mut recorder).await?;
                                continue;
                            }
                        }

                        // Check read-only mode for modifying commands
                        if security.base.read_only && is_cql_modifying_command(&command) {
                            executor
                                .terminal
                                .write_error(
                                    "Command blocked: read-only mode is enabled.",
                                )
                                .map_err(|e| {
                                    HandlerError::ProtocolError(e.to_string())
                                })?;
                            executor
                                .terminal
                                .write_prompt()
                                .map_err(|e| {
                                    HandlerError::ProtocolError(e.to_string())
                                })?;
                            send_render(&mut executor, &to_client, &mut recorder).await?;
                            continue;
                        }

                        // Handle DESCRIBE commands
                        if command_upper.starts_with("DESCRIBE ")
                            || command_upper.starts_with("DESC ")
                        {
                            match handle_describe_command(
                                &command,
                                &session,
                                &current_keyspace,
                            )
                            .await
                            {
                                Ok(output) => {
                                    for line in output.lines() {
                                        executor
                                            .terminal
                                            .write_line(line)
                                            .map_err(|e| {
                                                HandlerError::ProtocolError(
                                                    e.to_string(),
                                                )
                                            })?;
                                    }
                                }
                                Err(e) => {
                                    record_error_output(&mut recorder, &e);
                                    executor
                                        .terminal
                                        .write_error(&e)
                                        .map_err(|e| {
                                            HandlerError::ProtocolError(e.to_string())
                                        })?;
                                }
                            }
                            executor
                                .terminal
                                .write_prompt()
                                .map_err(|e| {
                                    HandlerError::ProtocolError(e.to_string())
                                })?;
                            send_render(&mut executor, &to_client, &mut recorder).await?;
                            continue;
                        }

                        // Execute CQL query
                        match execute_cql_query(&session, &command).await {
                            Ok(result) => {
                                // Write result line by line
                                for line in result.lines() {
                                    executor
                                        .terminal
                                        .write_line(line)
                                        .map_err(|e| {
                                            HandlerError::ProtocolError(e.to_string())
                                        })?;
                                }
                            }
                            Err(e) => {
                                // Record error output
                                record_error_output(&mut recorder, &e);

                                executor
                                    .terminal
                                    .write_error(&e)
                                    .map_err(|e| {
                                        HandlerError::ProtocolError(e.to_string())
                                    })?;
                            }
                        }

                        executor
                            .terminal
                            .write_prompt()
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                        send_render(&mut executor, &to_client, &mut recorder).await?;
                        continue;
                    }

                    if needs_render {
                        for instr in instructions {
                            let _ = crate::recording::send_and_record(&to_client, &mut recorder, instr).await;
                        }
                    }
                }
                        Err(e) => {
                            warn!("Cassandra: Input processing error: {}", e);
                        }
                    }
                }

                // Client disconnected
                else => {
                    break;
                }
            }
        }

        // Finalize recording
        finalize_recording(recorder, "Cassandra");

        send_disconnect(&to_client).await;
        info!("Cassandra handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute a CQL query and return formatted result string
async fn execute_cql_query(session: &scylla::Session, query: &str) -> Result<String, String> {
    let start = Instant::now();
    let result = session
        .query_unpaged(query, &[])
        .await
        .map_err(|e| format!("CQL error: {}", e))?;

    let elapsed = start.elapsed();

    // Extract column names from column specs BEFORE matching on rows
    // (matching on result.rows partially moves result, preventing later borrows)
    let columns: Vec<String> = result
        .col_specs()
        .iter()
        .map(|spec| spec.name.clone())
        .collect();

    // Try to get rows (SELECT queries return rows)
    match result.rows {
        Some(rows) => {
            // Convert rows to string representation
            let mut row_data: Vec<Vec<String>> = Vec::new();
            for row in rows {
                let mut string_row = Vec::with_capacity(columns.len());
                for col_value in &row.columns {
                    string_row.push(format_cql_value(col_value));
                }
                row_data.push(string_row);
            }

            // Format as tabular output
            Ok(format_tabular_output(
                &columns,
                &row_data,
                elapsed.as_millis() as u64,
            ))
        }
        None => {
            // Non-SELECT statement (INSERT, UPDATE, DELETE, CREATE, etc.)
            Ok(format!(
                "Query executed successfully. ({:.1}ms)",
                elapsed.as_secs_f64() * 1000.0
            ))
        }
    }
}

/// Execute a CQL query and return a QueryResult struct (for CSV export)
async fn execute_cql_query_as_result(
    session: &scylla::Session,
    query: &str,
) -> Result<guacr_terminal::QueryResult, String> {
    let start = Instant::now();
    let result = session
        .query_unpaged(query, &[])
        .await
        .map_err(|e| format!("CQL error: {}", e))?;

    let elapsed = start.elapsed();

    // Extract column names from column specs BEFORE matching on rows
    let columns: Vec<String> = result
        .col_specs()
        .iter()
        .map(|spec| spec.name.clone())
        .collect();

    match result.rows {
        Some(result_rows) => {
            let mut rows: Vec<Vec<String>> = Vec::new();
            for row in result_rows {
                let mut string_row = Vec::with_capacity(columns.len());
                for col_value in &row.columns {
                    string_row.push(format_cql_value(col_value));
                }
                rows.push(string_row);
            }

            Ok(guacr_terminal::QueryResult {
                columns,
                rows,
                affected_rows: None,
                execution_time_ms: Some(elapsed.as_millis() as u64),
            })
        }
        None => Ok(guacr_terminal::QueryResult {
            columns: vec![],
            rows: vec![],
            affected_rows: None,
            execution_time_ms: Some(elapsed.as_millis() as u64),
        }),
    }
}

/// Format a CqlValue as a human-readable string
fn format_cql_value(value: &Option<CqlValue>) -> String {
    match value {
        None => "NULL".to_string(),
        Some(v) => match v {
            CqlValue::Ascii(s) => s.clone(),
            CqlValue::Text(s) => s.clone(),
            CqlValue::Boolean(b) => b.to_string(),
            CqlValue::Int(i) => i.to_string(),
            CqlValue::BigInt(i) => i.to_string(),
            CqlValue::SmallInt(i) => i.to_string(),
            CqlValue::TinyInt(i) => i.to_string(),
            CqlValue::Float(f) => f.to_string(),
            CqlValue::Double(d) => d.to_string(),
            CqlValue::Uuid(u) => u.to_string(),
            CqlValue::Timeuuid(u) => format!("{:?}", u),
            CqlValue::Inet(addr) => addr.to_string(),
            CqlValue::Blob(bytes) => format!("0x{}", hex::encode(bytes)),
            CqlValue::Date(d) => format!("{:?}", d),
            CqlValue::Timestamp(t) => format!("{:?}", t),
            CqlValue::Time(t) => format!("{:?}", t),
            CqlValue::Duration(d) => format!("{:?}", d),
            CqlValue::Counter(c) => format!("{:?}", c),
            CqlValue::Decimal(d) => format!("{:?}", d),
            CqlValue::Varint(v) => format!("{:?}", v),
            CqlValue::List(items) => {
                let formatted: Vec<String> = items
                    .iter()
                    .map(|i| format_cql_value(&Some(i.clone())))
                    .collect();
                format!("[{}]", formatted.join(", "))
            }
            CqlValue::Set(items) => {
                let formatted: Vec<String> = items
                    .iter()
                    .map(|i| format_cql_value(&Some(i.clone())))
                    .collect();
                format!("{{{}}}", formatted.join(", "))
            }
            CqlValue::Map(entries) => {
                let formatted: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}: {}",
                            format_cql_value(&Some(k.clone())),
                            format_cql_value(&Some(v.clone()))
                        )
                    })
                    .collect();
                format!("{{{}}}", formatted.join(", "))
            }
            CqlValue::Tuple(elements) => {
                let formatted: Vec<String> = elements.iter().map(format_cql_value).collect();
                format!("({})", formatted.join(", "))
            }
            CqlValue::UserDefinedType { fields, .. } => {
                let formatted: Vec<String> = fields
                    .iter()
                    .map(|(name, value)| format!("{}: {}", name, format_cql_value(value)))
                    .collect();
                format!("{{{}}}", formatted.join(", "))
            }
            CqlValue::Empty => "".to_string(),
        },
    }
}

/// Format query results as tabular output (similar to cqlsh)
fn format_tabular_output(columns: &[String], rows: &[Vec<String>], time_ms: u64) -> String {
    if columns.is_empty() {
        return format!("(0 rows) ({:.1}ms)", time_ms as f64);
    }

    // Calculate column widths
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Cap column widths at a reasonable maximum to prevent overflow
    let max_col_width = 60;
    for w in &mut widths {
        if *w > max_col_width {
            *w = max_col_width;
        }
    }

    let mut output = String::new();

    // Header
    let header: String = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                " {:width$}",
                c,
                width = widths.get(i).copied().unwrap_or(10)
            )
        })
        .collect::<Vec<_>>()
        .join(" |");
    output.push_str(&header);
    output.push('\n');

    // Separator
    let sep: String = widths
        .iter()
        .map(|w| "-".repeat(*w + 1))
        .collect::<Vec<_>>()
        .join("-+-");
    output.push_str(&format!("-{}", sep));
    output.push('\n');

    // Rows
    for row in rows {
        let row_str: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = widths.get(i).copied().unwrap_or(10);
                let truncated = if cell.len() > width {
                    format!("{}...", &cell[..width.saturating_sub(3)])
                } else {
                    cell.clone()
                };
                format!(" {:width$}", truncated, width = width)
            })
            .collect::<Vec<_>>()
            .join(" |");
        output.push_str(&row_str);
        output.push('\n');
    }

    // Row count and timing
    output.push('\n');
    let row_word = if rows.len() == 1 { "row" } else { "rows" };
    output.push_str(&format!(
        "({} {}) ({:.1}ms)",
        rows.len(),
        row_word,
        time_ms as f64
    ));

    output
}

/// Check if a CQL command modifies data
fn is_cql_modifying_command(command: &str) -> bool {
    let cmd = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase();
    matches!(
        cmd.as_str(),
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "DROP"
            | "TRUNCATE"
            | "ALTER"
            | "CREATE"
            | "GRANT"
            | "REVOKE"
            | "BATCH"
    )
}

/// Handle DESCRIBE commands by querying system tables
async fn handle_describe_command(
    command: &str,
    session: &scylla::Session,
    current_keyspace: &Option<String>,
) -> Result<String, String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(
            "Usage: DESCRIBE KEYSPACES | DESCRIBE TABLES | DESCRIBE TABLE <name>".to_string(),
        );
    }

    let target = parts[1].to_uppercase();
    let target = target.trim_end_matches(';');

    match target {
        "KEYSPACES" => {
            // List all keyspaces
            let result = session
                .query_unpaged("SELECT keyspace_name FROM system_schema.keyspaces", &[])
                .await
                .map_err(|e| format!("Failed to describe keyspaces: {}", e))?;

            match result.rows_typed::<(String,)>() {
                Ok(rows_iter) => {
                    let mut keyspaces = Vec::new();
                    for row in rows_iter {
                        let (ks_name,) = row.map_err(|e| format!("Row error: {}", e))?;
                        keyspaces.push(ks_name);
                    }
                    keyspaces.sort();

                    let mut output = String::from("Keyspaces:\n");
                    for ks in &keyspaces {
                        output.push_str(&format!("  {}\n", ks));
                    }
                    output.push_str(&format!("\n({} keyspaces)", keyspaces.len()));
                    Ok(output)
                }
                Err(_) => Err("Failed to parse keyspace list".to_string()),
            }
        }
        "TABLES" => {
            // List tables in current keyspace
            let ks = current_keyspace
                .as_deref()
                .ok_or("No keyspace selected. Use: USE <keyspace> first.")?;

            let query = format!(
                "SELECT table_name FROM system_schema.tables WHERE keyspace_name = '{}'",
                ks
            );
            let result = session
                .query_unpaged(query, &[])
                .await
                .map_err(|e| format!("Failed to describe tables: {}", e))?;

            match result.rows_typed::<(String,)>() {
                Ok(rows_iter) => {
                    let mut tables = Vec::new();
                    for row in rows_iter {
                        let (table_name,) = row.map_err(|e| format!("Row error: {}", e))?;
                        tables.push(table_name);
                    }
                    tables.sort();

                    let mut output = format!("Tables in keyspace '{}':\n", ks);
                    for table in &tables {
                        output.push_str(&format!("  {}\n", table));
                    }
                    output.push_str(&format!("\n({} tables)", tables.len()));
                    Ok(output)
                }
                Err(_) => Err("Failed to parse table list".to_string()),
            }
        }
        "TABLE" if parts.len() >= 3 => {
            // Describe a specific table: DESCRIBE TABLE <name>
            let table_name = parts[2].trim_end_matches(';');
            describe_table(session, current_keyspace, table_name).await
        }
        _ => {
            // Try as table name: DESCRIBE <tablename>
            let table_name = parts[1].trim_end_matches(';');
            describe_table(session, current_keyspace, table_name).await
        }
    }
}

/// Describe a specific table's schema
async fn describe_table(
    session: &scylla::Session,
    current_keyspace: &Option<String>,
    table_name: &str,
) -> Result<String, String> {
    // Determine keyspace.table
    let (ks, tbl) = if table_name.contains('.') {
        let parts: Vec<&str> = table_name.splitn(2, '.').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        let ks = current_keyspace
            .as_deref()
            .ok_or("No keyspace selected. Use: USE <keyspace> first, or specify keyspace.table")?;
        (ks.to_string(), table_name.to_string())
    };

    let query = format!(
        "SELECT column_name, type, kind FROM system_schema.columns \
         WHERE keyspace_name = '{}' AND table_name = '{}'",
        ks, tbl
    );
    let result = session
        .query_unpaged(query, &[])
        .await
        .map_err(|e| format!("Failed to describe table: {}", e))?;

    match result.rows_typed::<(String, String, String)>() {
        Ok(rows_iter) => {
            let mut columns: Vec<(String, String, String)> = Vec::new();
            for row in rows_iter {
                let (col_name, col_type, col_kind) =
                    row.map_err(|e| format!("Row error: {}", e))?;
                columns.push((col_name, col_type, col_kind));
            }

            if columns.is_empty() {
                return Err(format!("Table '{}.{}' not found", ks, tbl));
            }

            // Sort: partition keys first, then clustering, then static, then regular
            columns.sort_by(|a, b| {
                let order = |kind: &str| -> u8 {
                    match kind {
                        "partition_key" => 0,
                        "clustering" => 1,
                        "static" => 2,
                        "regular" => 3,
                        _ => 4,
                    }
                };
                order(&a.2).cmp(&order(&b.2)).then(a.0.cmp(&b.0))
            });

            let mut output = format!("Table: {}.{}\n\n", ks, tbl);

            // Calculate column widths for formatting
            let max_name_len = columns.iter().map(|c| c.0.len()).max().unwrap_or(10);
            let max_type_len = columns.iter().map(|c| c.1.len()).max().unwrap_or(10);

            output.push_str(&format!(
                " {:name_w$} | {:type_w$} | Kind\n",
                "Column",
                "Type",
                name_w = max_name_len,
                type_w = max_type_len
            ));
            output.push_str(&format!(
                "-{}-+-{}-+------\n",
                "-".repeat(max_name_len),
                "-".repeat(max_type_len)
            ));

            for (col_name, col_type, col_kind) in &columns {
                let kind_display = match col_kind.as_str() {
                    "partition_key" => "PK",
                    "clustering" => "CK",
                    "static" => "S",
                    "regular" => "",
                    other => other,
                };
                output.push_str(&format!(
                    " {:name_w$} | {:type_w$} | {}\n",
                    col_name,
                    col_type,
                    kind_display,
                    name_w = max_name_len,
                    type_w = max_type_len
                ));
            }

            output.push_str(&format!("\n({} columns)", columns.len()));
            Ok(output)
        }
        Err(_) => Err(format!("Failed to describe table '{}.{}'", ks, tbl)),
    }
}

/// Handle built-in commands
async fn handle_builtin_command(
    command: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<bool> {
    let command_lower = command.to_lowercase();
    let command_trimmed = command_lower.trim().trim_end_matches(';');

    match command_trimmed {
        "help" | "?" => {
            let mut sections = vec![
                HelpSection {
                    title: "CQL statements",
                    commands: vec![
                        ("SELECT ... FROM ...", "Query data"),
                        ("INSERT INTO ...", "Insert data"),
                        ("UPDATE ... SET ...", "Update data"),
                        ("DELETE FROM ...", "Delete data"),
                        ("CREATE TABLE ...", "Create table"),
                        ("DROP TABLE ...", "Drop table"),
                    ],
                },
                HelpSection {
                    title: "Navigation",
                    commands: vec![
                        ("USE <keyspace>", "Switch keyspace"),
                        ("DESCRIBE KEYSPACES", "List keyspaces"),
                        ("DESCRIBE TABLES", "List tables in current keyspace"),
                        ("DESCRIBE <table>", "Show table schema"),
                    ],
                },
            ];

            let mut export_cmds = Vec::new();
            if !security.disable_csv_export {
                export_cmds.push(("\\e <query>", "Export query results as CSV"));
            }
            if !security.disable_csv_import && !security.base.read_only {
                export_cmds.push(("\\i <table_name>", "Import data from CSV into table"));
            }
            if !export_cmds.is_empty() {
                sections.push(HelpSection {
                    title: "Export/Import",
                    commands: export_cmds,
                });
            }

            render_help(executor, to_client, "CQL Shell", &sections, recorder).await?;
            return Ok(true);
        }
        "quit" | "exit" => {
            return Err(handle_quit(executor, to_client, recorder).await);
        }
        _ => {}
    }

    Ok(false)
}

/// Handle CSV export for CQL query results
async fn handle_csv_export(
    query: &str,
    session: &scylla::Session,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<()> {
    use std::sync::atomic::Ordering;

    // Check if export is allowed
    if security.disable_csv_export {
        executor
            .terminal
            .write_error("CSV export is disabled by your administrator.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    if query.is_empty() {
        executor
            .terminal
            .write_error("Usage: \\e <CQL query>")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    executor
        .terminal
        .write_line(&format!("Executing query for export: {}...", query))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    send_render(executor, to_client, recorder).await?;

    // Execute query and get result
    let result = match execute_cql_query_as_result(session, query).await {
        Ok(r) => r,
        Err(e) => {
            executor
                .terminal
                .write_error(&format!("Query failed: {}", e))
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_prompt()
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            let (_, instructions) = executor
                .render_screen()
                .await
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            for instr in instructions {
                to_client
                    .send(instr)
                    .await
                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
            }
            return Ok(());
        }
    };

    if result.columns.is_empty() {
        executor
            .terminal
            .write_error("Query returned no columns. Only SELECT queries can be exported.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    // Generate filename and create exporter
    let filename = generate_csv_filename(query, "cassandra");
    let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
    let mut exporter = CsvExporter::new(stream_idx);

    executor
        .terminal
        .write_line(&format!(
            "Beginning CSV download ({} rows). Press Ctrl+C to cancel.",
            result.rows.len()
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Send file instruction to start download
    let file_instr = exporter.start_download(&filename);
    to_client
        .send(file_instr)
        .await
        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

    // Export the data
    match exporter.export_query_result(&result, to_client).await {
        Ok(()) => {
            executor
                .terminal
                .write_line("Download complete.")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
        Err(e) => {
            executor
                .terminal
                .write_error(&format!("Export failed: {}", e))
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
    }

    executor
        .terminal
        .write_prompt()
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    send_render(executor, to_client, recorder).await?;

    Ok(())
}

/// Handle CSV import for Cassandra (imports rows into a table)
async fn handle_csv_import(
    table_name: &str,
    session: &scylla::Session,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    current_keyspace: &Option<String>,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<()> {
    // Check if import is allowed
    if security.disable_csv_import {
        executor
            .terminal
            .write_error("CSV import is disabled by your administrator.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    // Check read-only mode
    if security.base.read_only {
        executor
            .terminal
            .write_error("Import blocked: read-only mode is enabled.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    // Require explicit table name
    if table_name.is_empty() {
        executor
            .terminal
            .write_error("Usage: \\i <table_name>")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    let ks = match current_keyspace {
        Some(ks) => ks.clone(),
        None => {
            executor
                .terminal
                .write_error("No keyspace selected. Use: USE <keyspace> first.")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_prompt()
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            let (_, instructions) = executor
                .render_screen()
                .await
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            for instr in instructions {
                to_client
                    .send(instr)
                    .await
                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
            }
            return Ok(());
        }
    };

    // Verify the table exists before importing
    let check_query = format!(
        "SELECT table_name FROM system_schema.tables WHERE keyspace_name = '{}' AND table_name = '{}'",
        ks, table_name
    );
    let table_exists = match session.query_unpaged(check_query, &[]).await {
        Ok(result) => result.first_row_typed::<(String,)>().is_ok(),
        Err(_) => false,
    };

    if !table_exists {
        executor
            .terminal
            .write_error(&format!(
                "Table '{}' not found in keyspace '{}'. Use DESCRIBE TABLES to list available tables.",
                table_name, ks
            ))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    executor
        .terminal
        .write_line(&format!("Cassandra CSV Import into {}.{}", ks, table_name))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Demo: Importing sample data...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Demo data - for Cassandra we need a target table
    // In production, the CSV upload mechanism would provide the data
    let sample_csv = "id,name,value\n1,Alice,100\n2,Bob,200";
    let mut importer = crate::csv_import::CsvImporter::new(1);

    importer
        .receive_blob(sample_csv.as_bytes())
        .map_err(HandlerError::ProtocolError)?;

    let csv_data = importer
        .finish_receive()
        .map_err(HandlerError::ProtocolError)?;

    executor
        .terminal
        .write_line(&format!(
            "Parsed {} rows with columns: {}",
            csv_data.row_count(),
            csv_data.headers.join(", ")
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Generate CQL INSERT statements and execute them
    // Note: table/column names come from CSV headers (not user input) and
    // cannot be parameterized in CQL. Values use standard CQL single-quote
    // escaping ('' for literal single quotes).
    let mut success_count = 0;
    let mut error_count = 0;

    let columns = csv_data.headers.join(", ");
    for row in &csv_data.rows {
        let values: Vec<String> = row
            .iter()
            .map(|v| {
                if v.is_empty() || v.eq_ignore_ascii_case("null") {
                    "NULL".to_string()
                } else if v.parse::<f64>().is_ok() {
                    v.clone()
                } else {
                    // Escape single quotes for CQL
                    format!("'{}'", v.replace('\'', "''"))
                }
            })
            .collect();

        let insert_query = format!(
            "INSERT INTO {}.{} ({}) VALUES ({})",
            ks,
            table_name,
            columns,
            values.join(", ")
        );

        match session.query_unpaged(insert_query, &[]).await {
            Ok(_) => success_count += 1,
            Err(e) => {
                error_count += 1;
                warn!("Cassandra import error: {}", e);
            }
        }
    }

    executor
        .terminal
        .write_success(&format!(
            "Import complete: {} rows inserted, {} errors",
            success_count, error_count
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    executor
        .terminal
        .write_prompt()
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    send_render(executor, to_client, recorder).await?;

    Ok(())
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for CassandraHandler {
    fn name(&self) -> &str {
        "cassandra"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        guacr_handlers::connect_with_event_adapter(
            |params, to_client, from_client| self.connect(params, to_client, from_client),
            params,
            callback,
            from_client,
            4096,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cassandra_handler_new() {
        let handler = CassandraHandler::with_defaults();
        assert_eq!(
            <CassandraHandler as ProtocolHandler>::name(&handler),
            "cassandra"
        );
    }

    #[test]
    fn test_cassandra_config() {
        let config = CassandraConfig::default();
        assert_eq!(config.default_port, 9042);
        assert!(!config.require_auth);
        assert!(!config.require_tls);
    }

    #[test]
    fn test_format_cql_value_primitives() {
        assert_eq!(format_cql_value(&None), "NULL");
        assert_eq!(
            format_cql_value(&Some(CqlValue::Text("hello".to_string()))),
            "hello"
        );
        assert_eq!(format_cql_value(&Some(CqlValue::Int(42))), "42");
        assert_eq!(
            format_cql_value(&Some(CqlValue::BigInt(1234567890))),
            "1234567890"
        );
        assert_eq!(format_cql_value(&Some(CqlValue::Boolean(true))), "true");
        assert_eq!(format_cql_value(&Some(CqlValue::Float(3.25_f32))), "3.25");
        assert_eq!(format_cql_value(&Some(CqlValue::Double(2.5_f64))), "2.5");
        assert_eq!(format_cql_value(&Some(CqlValue::SmallInt(16))), "16");
        assert_eq!(format_cql_value(&Some(CqlValue::TinyInt(8))), "8");
        assert_eq!(
            format_cql_value(&Some(CqlValue::Ascii("ascii".to_string()))),
            "ascii"
        );
        assert_eq!(format_cql_value(&Some(CqlValue::Empty)), "");
    }

    #[test]
    fn test_format_cql_value_blob() {
        assert_eq!(
            format_cql_value(&Some(CqlValue::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]))),
            "0xdeadbeef"
        );
    }

    #[test]
    fn test_format_cql_value_collections() {
        let list = CqlValue::List(vec![CqlValue::Int(1), CqlValue::Int(2), CqlValue::Int(3)]);
        assert_eq!(format_cql_value(&Some(list)), "[1, 2, 3]");

        let set = CqlValue::Set(vec![
            CqlValue::Text("a".to_string()),
            CqlValue::Text("b".to_string()),
        ]);
        assert_eq!(format_cql_value(&Some(set)), "{a, b}");

        let map = CqlValue::Map(vec![
            (CqlValue::Text("key1".to_string()), CqlValue::Int(100)),
            (CqlValue::Text("key2".to_string()), CqlValue::Int(200)),
        ]);
        assert_eq!(format_cql_value(&Some(map)), "{key1: 100, key2: 200}");
    }

    #[test]
    fn test_format_cql_value_tuple() {
        let tuple = CqlValue::Tuple(vec![
            Some(CqlValue::Int(1)),
            Some(CqlValue::Text("hello".to_string())),
            None,
        ]);
        assert_eq!(format_cql_value(&Some(tuple)), "(1, hello, NULL)");
    }

    #[test]
    fn test_format_cql_value_inet() {
        let addr: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(format_cql_value(&Some(CqlValue::Inet(addr))), "127.0.0.1");
    }

    #[test]
    fn test_is_cql_modifying_command() {
        assert!(is_cql_modifying_command(
            "INSERT INTO users (id) VALUES (1)"
        ));
        assert!(is_cql_modifying_command("UPDATE users SET name = 'x'"));
        assert!(is_cql_modifying_command("DELETE FROM users WHERE id = 1"));
        assert!(is_cql_modifying_command("DROP TABLE users"));
        assert!(is_cql_modifying_command("TRUNCATE users"));
        assert!(is_cql_modifying_command("ALTER TABLE users ADD col text"));
        assert!(is_cql_modifying_command(
            "CREATE TABLE t (id int PRIMARY KEY)"
        ));
        assert!(is_cql_modifying_command("GRANT SELECT ON users TO user1"));
        assert!(is_cql_modifying_command(
            "REVOKE SELECT ON users FROM user1"
        ));
        assert!(is_cql_modifying_command("BATCH USING TIMESTAMP 1234"));

        assert!(!is_cql_modifying_command("SELECT * FROM users"));
        assert!(!is_cql_modifying_command("DESCRIBE KEYSPACES"));
        assert!(!is_cql_modifying_command("USE mykeyspace"));
        assert!(!is_cql_modifying_command("help"));
    }

    #[test]
    fn test_format_tabular_output_empty() {
        let result = format_tabular_output(&[], &[], 5);
        assert!(result.contains("(0 rows)"));
    }

    #[test]
    fn test_format_tabular_output_with_data() {
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec!["1".to_string(), "Alice".to_string()],
            vec!["2".to_string(), "Bob".to_string()],
        ];
        let result = format_tabular_output(&columns, &rows, 10);

        assert!(result.contains("id"));
        assert!(result.contains("name"));
        assert!(result.contains("Alice"));
        assert!(result.contains("Bob"));
        assert!(result.contains("(2 rows)"));
    }

    #[test]
    fn test_format_tabular_output_single_row() {
        let columns = vec!["count".to_string()];
        let rows = vec![vec!["42".to_string()]];
        let result = format_tabular_output(&columns, &rows, 1);

        assert!(result.contains("(1 row)"));
    }

    #[test]
    fn test_format_tabular_output_long_values_truncated() {
        let columns = vec!["data".to_string()];
        let long_value = "x".repeat(100);
        let rows = vec![vec![long_value]];
        let result = format_tabular_output(&columns, &rows, 1);

        // Should be truncated with "..."
        assert!(result.contains("..."));
    }
}
