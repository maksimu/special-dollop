use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    send_disconnect, EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus,
    ProtocolHandler, RecordingConfig,
};
use guacr_terminal::QueryResult;
use log::{debug, info, warn};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{Column, Row, TypeInfo};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::csv_export::{generate_csv_filename, CsvExporter};
use crate::csv_import::CsvImporter;
use crate::query_executor::{execute_with_timing, QueryExecutor};
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
    record_query_output, send_and_record,
};
use crate::security::{
    check_csv_export_allowed, check_csv_import_allowed, check_query_allowed, is_postgres_copy_in,
    is_postgres_copy_out, DatabaseSecuritySettings,
};

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(1000);

/// PostgreSQL protocol handler
///
/// Provides interactive SQL terminal access to PostgreSQL databases.
pub struct PostgreSqlHandler {
    config: PostgreSqlConfig,
}

#[derive(Debug, Clone)]
pub struct PostgreSqlConfig {
    pub default_port: u16,
    pub connection_timeout_secs: u64,
    pub max_connections: u32,
}

impl Default for PostgreSqlConfig {
    fn default() -> Self {
        Self {
            default_port: 5432,
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
            max_connections: 5,
        }
    }
}

impl PostgreSqlHandler {
    pub fn new(config: PostgreSqlConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(PostgreSqlConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for PostgreSqlHandler {
    fn name(&self) -> &str {
        "postgresql"
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
        info!("PostgreSQL handler starting");

        // Initialize rustls crypto provider (required for rustls 0.23+)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("PostgreSQL: Read-only mode enabled");
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
        let username = params
            .get("username")
            .ok_or_else(|| HandlerError::MissingParameter("username".to_string()))?;
        let password = params
            .get("password")
            .ok_or_else(|| HandlerError::MissingParameter("password".to_string()))?;
        let database = params
            .get("database")
            .map(|s| s.as_str())
            .unwrap_or("postgres");

        info!(
            "PostgreSQL: Connecting to {}@{}:{}/{}",
            username, hostname, port, database
        );

        // Parse display size from parameters (like SSH does)
        let size_params = params
            .get("size")
            .map(|s| s.as_str())
            .unwrap_or("1024,768,96");
        let size_parts: Vec<&str> = size_params.split(',').collect();
        let width: u32 = size_parts
            .first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024);
        let height: u32 = size_parts
            .get(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(768);

        // Calculate terminal dimensions (9x18 pixels per character cell)
        let cols = (width / 9).max(80) as u16;
        let rows = (height / 18).max(24) as u16;

        info!(
            "PostgreSQL: Display size {}x{} px → {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with PostgreSQL prompt and correct dimensions
        let prompt = if security.base.read_only {
            "postgres [RO]=# "
        } else {
            "postgres=# "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "postgresql", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "PostgreSQL", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("PostgreSQL: Sent display init instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Build PostgreSQL connection URL with proper URL encoding for special characters
        // This is critical for passwords containing: | & ? @ : / # %
        let encoded_username = urlencoding::encode(username);
        let encoded_password = urlencoding::encode(password);
        let encoded_database = urlencoding::encode(database);
        let connection_url = format!(
            "postgres://{}:{}@{}:{}/{}",
            encoded_username, encoded_password, hostname, port, encoded_database
        );

        debug!(
            "PostgreSQL: Connection URL: {}",
            connection_url.replace(&encoded_password.to_string(), "***")
        );

        // Connect to PostgreSQL using sqlx
        let pool = match PgPoolOptions::new()
            .max_connections(self.config.max_connections)
            .acquire_timeout(std::time::Duration::from_secs(
                self.config.connection_timeout_secs,
            ))
            .connect(&connection_url)
            .await
        {
            Ok(pool) => {
                info!("PostgreSQL: Connected successfully");

                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Connected to PostgreSQL at {}:{}", hostname, port))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Database: {}", database))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                // Show security status
                if security.base.read_only {
                    executor
                        .terminal
                        .write_line("Mode: READ-ONLY (modifications disabled)")
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
                if security.disable_csv_export {
                    executor
                        .terminal
                        .write_line("COPY TO: DISABLED")
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }

                executor
                    .terminal
                    .write_line("Type 'help' for available commands.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_prompt()
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                // Render the connection success screen
                debug!("PostgreSQL: Rendering initial screen with prompt");
                let (_, instructions) = executor
                    .render_screen()
                    .await
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                debug!(
                    "PostgreSQL: Sending {} instructions to client",
                    instructions.len()
                );
                for instr in instructions {
                    to_client
                        .send(instr)
                        .await
                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                }
                debug!("PostgreSQL: Initial screen sent successfully");

                pool
            }
            Err(e) => {
                let error_msg = format!("Connection failed: {}", e);
                warn!("PostgreSQL: {}", error_msg);

                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_error(&error_msg)
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("Troubleshooting:")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  1. Check hostname is resolvable")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  2. Verify PostgreSQL server is running")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  3. Check pg_hba.conf allows connections")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  4. Verify credentials are correct")
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

                while from_client.recv().await.is_some() {}
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
        };

        // Event loop
        // NOTE: Screen was already rendered above after connection success

        // Debounce timer for batching screen updates (60 FPS)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        'outer: loop {
            tokio::select! {
                // Debounce tick - render if terminal or input changed
                _ = debounce.tick() => {
                    // Check if client is still connected before rendering
                    if to_client.is_closed() {
                        debug!("PostgreSQL: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if send_and_record(&to_client, &mut recorder, instr).await.is_err() {
                                debug!("PostgreSQL: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("PostgreSQL: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(query) = pending_query {
                        info!("PostgreSQL: Executing query: {}", query);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &query);

                        // Handle built-in commands
                        if handle_builtin_command(&query, &mut executor, &to_client, &security)
                            .await?
                        {
                            continue;
                        }

                        // Check for export command: \e <query>
                        if query.to_lowercase().starts_with("\\e ") {
                            let export_query = query[3..].trim();
                            handle_csv_export(
                                export_query,
                                &pool,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Check for import command: \i <table>
                        if query.to_lowercase().starts_with("\\i ") {
                            let table_name = query[3..].trim();
                            handle_csv_import(
                                table_name,
                                &pool,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Security checks
                        // Check for COPY TO (export)
                        if is_postgres_copy_out(&query) {
                            if let Err(msg) = check_csv_export_allowed(&security) {
                                executor
                                    .write_error(&msg)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                let (_, result_instructions) = executor
                                    .render_screen()
                                    .await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                for instr in result_instructions {
                                    to_client
                                        .send(instr)
                                        .await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                }
                                continue;
                            }
                        }

                        // Check for COPY FROM (import)
                        if is_postgres_copy_in(&query) {
                            if let Err(msg) = check_csv_import_allowed(&security) {
                                executor
                                    .write_error(&msg)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                let (_, result_instructions) = executor
                                    .render_screen()
                                    .await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                for instr in result_instructions {
                                    to_client
                                        .send(instr)
                                        .await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                }
                                continue;
                            }
                        }

                        // Check read-only mode
                        if let Err(msg) = check_query_allowed(&query, &security) {
                            executor
                                .write_error(&msg)
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            let (_, result_instructions) = executor
                                .render_screen()
                                .await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            for instr in result_instructions {
                                to_client
                                    .send(instr)
                                    .await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }
                            continue;
                        }

                        // Execute query
                        match execute_with_timing(|| execute_postgres_query(&pool, &query)).await {
                            Ok(exec_result) => {
                                let result = exec_result.into_query_result();

                                // Record query output
                                record_query_output(&mut recorder, &result);

                                executor
                                    .write_result(&result)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                            Err(e) => {
                                // Record error output
                                record_error_output(&mut recorder, &e);

                                executor
                                    .write_error(&e)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }

                        let (_, result_instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in result_instructions {
                            send_and_record(&to_client, &mut recorder, instr)
                                .await
                                .map_err(HandlerError::ChannelError)?;
                        }
                        continue;
                    }

                    if needs_render {
                        // Render immediately for special cases (Enter, Escape, etc.)
                        for instr in instructions {
                            let _ = send_and_record(&to_client, &mut recorder, instr).await;
                        }
                    }
                    // For regular keystrokes, debounce timer will handle rendering
                        }
                        Err(e) => {
                            warn!("PostgreSQL: Input processing error: {}", e);
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
        finalize_recording(recorder, "PostgreSQL");

        send_disconnect(&to_client).await;
        info!("PostgreSQL handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute PostgreSQL query and return results
async fn execute_postgres_query(pool: &sqlx::PgPool, query: &str) -> Result<QueryResult, String> {
    // Use sqlx::query for raw SQL execution
    let rows: Vec<PgRow> = sqlx::query(query)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Query error: {}", e))?;

    if rows.is_empty() {
        return Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            affected_rows: Some(0),
            execution_time_ms: None,
        });
    }

    // Get column names from first row
    let columns: Vec<String> = rows[0]
        .columns()
        .iter()
        .map(|col| col.name().to_string())
        .collect();

    let mut query_result = QueryResult::new(columns.clone());

    for row in &rows {
        let mut row_data = Vec::new();
        for (idx, col) in row.columns().iter().enumerate() {
            let value = pg_value_to_string(row, idx, col.type_info());
            row_data.push(value);
        }
        query_result.add_row(row_data);
    }

    Ok(query_result)
}

/// Convert PostgreSQL value to string
fn pg_value_to_string(row: &PgRow, idx: usize, type_info: &sqlx::postgres::PgTypeInfo) -> String {
    let type_name = type_info.name();

    // Try to get value based on type
    match type_name {
        "INT2" | "INT4" | "INT8" => row
            .try_get::<i64, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        "FLOAT4" | "FLOAT8" | "NUMERIC" => row
            .try_get::<f64, _>(idx)
            .map(|v| format!("{:.6}", v))
            .unwrap_or_else(|_| "NULL".to_string()),
        "BOOL" => row
            .try_get::<bool, _>(idx)
            .map(|v| if v { "true" } else { "false" }.to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => row
            .try_get::<String, _>(idx)
            .unwrap_or_else(|_| "NULL".to_string()),
        "TIMESTAMP" | "TIMESTAMPTZ" => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .map(|v| v.format("%Y-%m-%d %H:%M:%S").to_string())
            .or_else(|_| {
                row.try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
                    .map(|v| v.format("%Y-%m-%d %H:%M:%S %Z").to_string())
            })
            .unwrap_or_else(|_| "NULL".to_string()),
        "DATE" => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .map(|v| v.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        "TIME" => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .map(|v| v.format("%H:%M:%S").to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        "UUID" => row
            .try_get::<uuid::Uuid, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        "JSON" | "JSONB" => row
            .try_get::<serde_json::Value, _>(idx)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "NULL".to_string()),
        _ => {
            // Fallback: try as string
            row.try_get::<String, _>(idx)
                .unwrap_or_else(|_| format!("<{}>", type_name))
        }
    }
}

/// Handle built-in commands
async fn handle_builtin_command(
    query: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<bool> {
    let query_lower = query.to_lowercase();

    match query_lower.as_str() {
        "help" | "\\h" | "\\?" => {
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Available commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  help, \\h     Show this help")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  quit, \\q     Disconnect")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  \\l           List databases")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  \\dt          List tables")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  \\d table     Describe table")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

            // Show export/import commands if not disabled
            if !security.disable_csv_export {
                executor
                    .terminal
                    .write_line("  \\e <query>   Export query results as CSV")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            if !security.disable_csv_import && !security.base.read_only {
                executor
                    .terminal
                    .write_line("  \\i <table>   Import CSV data into table")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }

            // Show security status in help
            if security.base.read_only {
                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("Note: READ-ONLY mode is enabled.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("      INSERT/UPDATE/DELETE/DROP are disabled.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }

            executor
                .terminal
                .write_line("")
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
            return Ok(true);
        }
        "quit" | "exit" | "\\q" => {
            executor
                .terminal
                .write_line("Bye")
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
            return Err(HandlerError::Disconnected(
                "User requested disconnect".to_string(),
            ));
        }
        _ => {}
    }

    Ok(false)
}

/// Handle CSV export for a query
async fn handle_csv_export(
    query: &str,
    pool: &sqlx::PgPool,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
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

    executor
        .terminal
        .write_line("Executing query for export...")
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

    // Execute the query
    match execute_postgres_query(pool, query).await {
        Ok(result) => {
            if result.rows.is_empty() {
                executor
                    .terminal
                    .write_line("Query returned no results to export.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            } else {
                // Generate filename and create exporter
                let filename = generate_csv_filename(query, "postgres");
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
            }
        }
        Err(e) => {
            executor
                .terminal
                .write_error(&format!("Query failed: {}", e))
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
    }

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

    Ok(())
}

/// Handle CSV import for a table
async fn handle_csv_import(
    table_name: &str,
    pool: &sqlx::PgPool,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
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

    if table_name.is_empty() {
        executor
            .terminal
            .write_error("Usage: \\i <table_name>")
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

    executor
        .terminal
        .write_line(&format!("Import into table: {}", table_name))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Demo: Importing sample data...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Demo data
    let sample_csv = "id,name,value\n1,Test,100\n2,Demo,200";
    let mut importer = CsvImporter::new(1);

    importer
        .receive_blob(sample_csv.as_bytes())
        .map_err(HandlerError::ProtocolError)?;

    let csv_data = importer
        .finish_receive()
        .map_err(HandlerError::ProtocolError)?;

    executor
        .terminal
        .write_line(&format!(
            "Parsed {} columns, {} rows",
            csv_data.headers.len(),
            csv_data.row_count()
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Generate and execute INSERT statements
    let inserts = importer
        .generate_postgres_inserts(table_name)
        .map_err(HandlerError::ProtocolError)?;

    let mut success_count = 0;
    let mut error_count = 0;

    for insert in &inserts {
        match sqlx::query(insert).execute(pool).await {
            Ok(_) => success_count += 1,
            Err(e) => {
                error_count += 1;
                warn!("Import error: {}", e);
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

    Ok(())
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for PostgreSqlHandler {
    fn name(&self) -> &str {
        "postgresql"
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
    fn test_postgresql_handler_new() {
        let handler = PostgreSqlHandler::with_defaults();
        assert_eq!(
            <PostgreSqlHandler as ProtocolHandler>::name(&handler),
            "postgresql"
        );
    }

    #[test]
    fn test_postgresql_config() {
        let config = PostgreSqlConfig::default();
        assert_eq!(config.default_port, 5432);
    }
}
