use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    send_disconnect, EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus,
    ProtocolHandler, RecordingConfig,
};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config, Row};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::csv_export::{generate_csv_filename, CsvExporter};
use crate::csv_import::CsvImporter;
use crate::query_executor::{execute_with_timing, QueryExecutor};
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
    record_query_output, send_and_record,
};
use crate::security::{check_query_allowed, DatabaseSecuritySettings};

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(2000);

/// SQL Server protocol handler
///
/// Provides interactive SQL terminal access to Microsoft SQL Server databases.
pub struct SqlServerHandler {
    config: SqlServerConfig,
}

#[derive(Debug, Clone)]
pub struct SqlServerConfig {
    pub default_port: u16,
    pub connection_timeout_secs: u64,
    pub trust_server_certificate: bool,
}

impl Default for SqlServerConfig {
    fn default() -> Self {
        Self {
            default_port: 1433,
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
            trust_server_certificate: true, // For development; should be false in production
        }
    }
}

impl SqlServerHandler {
    pub fn new(config: SqlServerConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(SqlServerConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for SqlServerHandler {
    fn name(&self) -> &str {
        "sqlserver"
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
        info!("SQL Server handler starting");

        // Initialize rustls crypto provider (required for rustls 0.23+)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("SQL Server: Read-only mode enabled");
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
        let database = params.get("database").map(|s| s.as_str());

        info!(
            "SQL Server: Connecting to {}@{}:{}",
            username, hostname, port
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
            "SQL Server: Display size {}x{} px → {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with SQL Server prompt and correct dimensions
        let prompt = if security.base.read_only {
            "1 [RO]> "
        } else {
            "1> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "sqlserver", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "SQLServer", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("SQL Server: Sent display init instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Build tiberius config
        let mut config = Config::new();
        config.host(hostname);
        config.port(port);
        config.authentication(AuthMethod::sql_server(username, password));

        if let Some(db) = database {
            config.database(db);
        }

        if self.config.trust_server_certificate {
            config.trust_cert();
        }

        debug!("SQL Server: Connecting with config");

        // Connect to SQL Server
        let tcp = match TcpStream::connect(config.get_addr()).await {
            Ok(tcp) => {
                tcp.set_nodelay(true).ok();
                tcp
            }
            Err(e) => {
                let error_msg = format!("TCP connection failed: {}", e);
                warn!("SQL Server: {}", error_msg);

                executor
                    .terminal
                    .write_error(&error_msg)
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

        let mut client = match Client::connect(config, tcp.compat_write()).await {
            Ok(client) => {
                info!("SQL Server: Connected successfully");

                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Connected to SQL Server at {}:{}", hostname, port))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                if let Some(db) = database {
                    executor
                        .terminal
                        .write_line(&format!("Database: {}", db))
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
                debug!("SQL Server: Rendering initial screen with prompt");
                let (_, instructions) = executor
                    .render_screen()
                    .await
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                debug!(
                    "SQL Server: Sending {} instructions to client",
                    instructions.len()
                );
                for instr in instructions {
                    to_client
                        .send(instr)
                        .await
                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                }
                debug!("SQL Server: Initial screen sent successfully");

                client
            }
            Err(e) => {
                let error_msg = format!("SQL Server connection failed: {}", e);
                warn!("SQL Server: {}", error_msg);

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
                    .write_line("  1. Check hostname and port")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  2. Verify SQL Server is running")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  3. Check SQL authentication is enabled")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  4. Verify credentials")
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
                // Debounce tick - render if terminal changed
                _ = debounce.tick() => {
                    // Check if client is still connected before rendering
                    if to_client.is_closed() {
                        debug!("SQL Server: Client disconnected, stopping debounce timer");
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
                                debug!("SQL Server: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("SQL Server: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(query) = pending_query {
                        info!("SQL Server: Executing query: {}", query);

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
                                &mut client,
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
                                &mut client,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
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
                        match execute_with_timing(|| execute_sqlserver_query(&mut client, &query))
                            .await
                        {
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
                            warn!("SQL Server: Input processing error: {}", e);
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
        finalize_recording(recorder, "SQLServer");

        send_disconnect(&to_client).await;
        info!("SQL Server handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute SQL Server query and return results
async fn execute_sqlserver_query(
    client: &mut Client<Compat<TcpStream>>,
    query: &str,
) -> Result<guacr_terminal::QueryResult, String> {
    let stream = client
        .query(query, &[])
        .await
        .map_err(|e| format!("Query error: {}", e))?;

    // Get all results
    let rows: Vec<Row> = stream
        .into_results()
        .await
        .map_err(|e| format!("Result error: {}", e))?
        .into_iter()
        .flat_map(|result_set| result_set.into_iter())
        .collect();

    if rows.is_empty() {
        return Ok(guacr_terminal::QueryResult {
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
        .map(|c| c.name().to_string())
        .collect();

    let mut result_rows: Vec<Vec<String>> = Vec::new();

    for row in &rows {
        let mut row_data = Vec::new();
        for i in 0..row.len() {
            // Try to get as string for each column
            let value: Option<&str> = row.try_get(i).ok().flatten();
            row_data.push(value.unwrap_or("NULL").to_string());
        }
        result_rows.push(row_data);
    }

    Ok(guacr_terminal::QueryResult {
        columns,
        rows: result_rows,
        affected_rows: None,
        execution_time_ms: None,
    })
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
        "help" | "\\h" | "\\?" | ":help" => {
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
                .write_line("  help         Show this help")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  quit         Disconnect")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Common SQL commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT name FROM sys.databases")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT * FROM sys.tables")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  sp_help 'table_name'")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
        "quit" | "exit" | ":quit" | "go quit" => {
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
    client: &mut Client<Compat<TcpStream>>,
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
    match execute_sqlserver_query(client, query).await {
        Ok(result) => {
            if result.rows.is_empty() {
                executor
                    .terminal
                    .write_line("Query returned no results to export.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            } else {
                // Generate filename and create exporter
                let filename = generate_csv_filename(query, "sqlserver");
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
    client: &mut Client<Compat<TcpStream>>,
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
        .generate_sqlserver_inserts(table_name)
        .map_err(HandlerError::ProtocolError)?;

    let mut success_count = 0;
    let mut error_count = 0;

    for insert in &inserts {
        match client.execute(insert, &[]).await {
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
impl EventBasedHandler for SqlServerHandler {
    fn name(&self) -> &str {
        "sqlserver"
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
    fn test_sqlserver_handler_new() {
        let handler = SqlServerHandler::with_defaults();
        assert_eq!(
            <SqlServerHandler as ProtocolHandler>::name(&handler),
            "sqlserver"
        );
    }

    #[test]
    fn test_sqlserver_config() {
        let config = SqlServerConfig::default();
        assert_eq!(config.default_port, 1433);
    }
}
