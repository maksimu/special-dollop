use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    send_disconnect, EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus,
    ProtocolHandler, RecordingConfig,
};
use guacr_terminal::QueryResult;
use log::{debug, info, warn};
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
    check_csv_export_allowed, check_csv_import_allowed, check_query_allowed, is_mysql_export_query,
    is_mysql_import_query, DatabaseSecuritySettings,
};

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(1);

/// MySQL protocol handler
///
/// Provides interactive SQL terminal access to MySQL/MariaDB databases.
pub struct MySqlHandler {
    config: MySqlConfig,
}

#[derive(Debug, Clone)]
pub struct MySqlConfig {
    pub default_port: u16,
    pub connection_timeout_secs: u64,
    pub query_timeout_secs: u64,
}

impl Default for MySqlConfig {
    fn default() -> Self {
        Self {
            default_port: 3306,
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
            query_timeout_secs: 300,
        }
    }
}

impl MySqlHandler {
    pub fn new(config: MySqlConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(MySqlConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for MySqlHandler {
    fn name(&self) -> &str {
        "mysql"
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
        info!("MySQL handler starting");

        // Initialize rustls crypto provider (required for rustls 0.23+)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("MySQL: Read-only mode enabled");
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
        let database = params.get("database").map(|s| s.as_str()).unwrap_or("");

        info!("MySQL: Connecting to {}@{}:{}", username, hostname, port);

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
            "MySQL: Display size {}x{} px → {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with MySQL prompt and correct dimensions
        let prompt = if security.base.read_only {
            "mysql [RO]> "
        } else {
            "mysql> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "mysql", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "MySQL", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("MySQL: Sent display init instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Build MySQL connection URL with proper URL encoding for special characters
        // This is critical for passwords containing: | & ? @ : / # %
        let encoded_username = urlencoding::encode(username);
        let encoded_password = urlencoding::encode(password);
        let connection_url = if database.is_empty() {
            format!(
                "mysql://{}:{}@{}:{}",
                encoded_username, encoded_password, hostname, port
            )
        } else {
            let encoded_database = urlencoding::encode(database);
            format!(
                "mysql://{}:{}@{}:{}/{}",
                encoded_username, encoded_password, hostname, port, encoded_database
            )
        };

        debug!(
            "MySQL: Connection URL: {}",
            connection_url.replace(&encoded_password.to_string(), "***")
        );

        // Connect to MySQL using mysql_async
        let opts = mysql_async::Opts::from_url(&connection_url)
            .map_err(|e| HandlerError::ConnectionFailed(format!("Invalid MySQL URL: {}", e)))?;
        let pool = mysql_async::Pool::new(opts);

        // Test connection
        match pool.get_conn().await {
            Ok(conn) => {
                info!("MySQL: Connected successfully");
                drop(conn);

                // Show connection success
                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!(
                        "Connected to MySQL server at {}:{}",
                        hostname, port
                    ))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                if !database.is_empty() {
                    executor
                        .terminal
                        .write_line(&format!("Database: {}", database))
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }

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
                        .write_line("CSV Export: DISABLED")
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
                debug!("MySQL: Rendering initial screen with prompt");
                let (_, instructions) = executor
                    .render_screen()
                    .await
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                debug!(
                    "MySQL: Sending {} instructions to client",
                    instructions.len()
                );
                for instr in instructions {
                    to_client
                        .send(instr)
                        .await
                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                }
                debug!("MySQL: Initial screen sent successfully");
            }
            Err(e) => {
                let error_msg = format!("Connection failed: {}", e);
                warn!("MySQL: {}", error_msg);

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
                    .write_line("  2. Verify MySQL server is running")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  3. Check credentials are correct")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  4. Verify network connectivity")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_prompt()
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                // Send error screen
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

                // Keep connection open so user can see the error
                while from_client.recv().await.is_some() {}
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
        }

        // Event loop - process queries
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
                        debug!("MySQL: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        debug!("MySQL: Debounce tick - rendering dirty terminal");
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        debug!("MySQL: Sending {} instructions from debounce", instructions.len());
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if send_and_record(&to_client, &mut recorder, instr).await.is_err() {
                                debug!("MySQL: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("MySQL: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                Ok((needs_render, instructions, pending_query)) => {
                    if let Some(query) = pending_query {
                        info!("MySQL: Executing query: {}", query);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &query);

                        // Handle built-in commands
                        match handle_builtin_command(&query, &mut executor, &to_client, &security)
                            .await
                        {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(HandlerError::Disconnected(_)) => break 'outer,
                            Err(e) => return Err(e),
                        }

                        // Check for export command: \e <query>
                        if query.to_lowercase().starts_with("\\e ") {
                            let export_query = query[3..].trim();
                            handle_csv_export(export_query, &pool, &mut executor, &to_client)
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
                        // Check for export query
                        if is_mysql_export_query(&query) {
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

                        // Check for import query
                        if is_mysql_import_query(&query) {
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

                        // Execute query with timing
                        match execute_with_timing(|| execute_mysql_query(&pool, &query)).await {
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

                        // Re-render with results immediately (query results should show right away)
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
                    warn!("MySQL: Input processing error: {}", e);
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
        finalize_recording(recorder, "MySQL");

        send_disconnect(&to_client).await;
        info!("MySQL handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute MySQL query and return results
async fn execute_mysql_query(pool: &mysql_async::Pool, query: &str) -> Result<QueryResult, String> {
    use mysql_async::prelude::*;

    let mut conn = pool
        .get_conn()
        .await
        .map_err(|e| format!("Connection error: {}", e))?;

    let result: Vec<mysql_async::Row> = conn
        .query(query)
        .await
        .map_err(|e| format!("Query error: {}", e))?;

    if result.is_empty() {
        // Non-SELECT query (INSERT, UPDATE, DELETE, etc.)
        return Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            affected_rows: Some(0), // TODO: Get actual affected rows
            execution_time_ms: None,
        });
    }

    // Get column names from first row
    let columns: Vec<String> = result[0]
        .columns_ref()
        .iter()
        .map(|col| col.name_str().to_string())
        .collect();

    let mut query_result = QueryResult::new(columns);

    for row in result {
        let mut row_data = Vec::new();
        for value in row.unwrap() {
            let str_value = mysql_value_to_string(value);
            row_data.push(str_value);
        }
        query_result.add_row(row_data);
    }

    Ok(query_result)
}

/// Convert MySQL value to string
fn mysql_value_to_string(value: mysql_async::Value) -> String {
    match value {
        mysql_async::Value::NULL => "NULL".to_string(),
        mysql_async::Value::Bytes(b) => String::from_utf8_lossy(&b).to_string(),
        mysql_async::Value::Int(i) => i.to_string(),
        mysql_async::Value::UInt(u) => u.to_string(),
        mysql_async::Value::Float(f) => f.to_string(),
        mysql_async::Value::Double(d) => format!("{:.6}", d),
        mysql_async::Value::Date(y, m, d, h, min, s, _) => {
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
        }
        mysql_async::Value::Time(neg, d, h, m, s, _) => {
            let sign = if neg { "-" } else { "" };
            if d > 0 {
                format!("{}{}d {:02}:{:02}:{:02}", sign, d, h, m, s)
            } else {
                format!("{}{:02}:{:02}:{:02}", sign, h, m, s)
            }
        }
    }
}

/// Handle built-in commands (help, quit, etc.)
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
                .write_line("  clear, \\c    Clear screen")
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
                .write_line("  SHOW DATABASES;")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SHOW TABLES;")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  DESCRIBE table_name;")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT * FROM table_name LIMIT 10;")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

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
            // Return error to exit the handler
            return Err(HandlerError::Disconnected(
                "User requested disconnect".to_string(),
            ));
        }
        "clear" | "\\c" => {
            // Clear screen by writing many newlines
            for _ in 0..24 {
                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
            return Ok(true);
        }
        _ => {}
    }

    // Check for export command: \e <query>
    if query_lower.starts_with("\\e ") {
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
            return Ok(true);
        }

        // Extract the query after \e
        let export_query = query[3..].trim();
        if export_query.is_empty() {
            executor
                .terminal
                .write_error("Usage: \\e <SELECT query>")
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

        // Export command detected - will be handled by caller
        return Ok(false);
    }

    Ok(false)
}

/// Handle CSV export for a query
async fn handle_csv_export(
    query: &str,
    pool: &mysql_async::Pool,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
) -> guacr_handlers::Result<()> {
    use std::sync::atomic::Ordering;

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
    match execute_mysql_query(pool, query).await {
        Ok(result) => {
            if result.rows.is_empty() {
                executor
                    .terminal
                    .write_line("Query returned no results to export.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            } else {
                // Generate filename and create exporter
                let filename = generate_csv_filename(query, "mysql");
                let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
                let mut exporter = CsvExporter::new(stream_idx);

                // Start the download
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
///
/// Note: This is a simplified implementation that parses CSV and generates INSERT statements.
/// For large imports, the native LOAD DATA INFILE would be more efficient, but requires
/// special server permissions and file access.
async fn handle_csv_import(
    table_name: &str,
    pool: &mysql_async::Pool,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use mysql_async::prelude::*;

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
        .write_line("Paste CSV data and press Enter twice to import, or Ctrl+C to cancel:")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
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

    // For now, provide sample usage since full Guacamole file upload requires more infrastructure
    executor
        .terminal
        .write_line("Note: File upload via drag-and-drop requires WebRTC file channel support.")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Alternative: Use LOAD DATA INFILE directly:")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line(&format!(
            "  LOAD DATA INFILE '/path/to/file.csv' INTO TABLE `{}`",
            table_name
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("  FIELDS TERMINATED BY ',' ENCLOSED BY '\"'")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("  LINES TERMINATED BY '\\n'")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("  IGNORE 1 LINES;")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Example with sample data for demonstration
    let sample_csv = "id,name,value\n1,Test,100\n2,Demo,200";
    executor
        .terminal
        .write_line("Demo: Importing sample data...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    let mut importer = CsvImporter::new(1);

    // Simulate receiving CSV data
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
        .generate_mysql_inserts(table_name)
        .map_err(HandlerError::ProtocolError)?;

    let mut success_count = 0;
    let mut error_count = 0;

    for insert in &inserts {
        let mut conn = pool
            .get_conn()
            .await
            .map_err(|e| HandlerError::ProtocolError(format!("Connection error: {}", e)))?;

        match conn.query_drop(insert).await {
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
impl EventBasedHandler for MySqlHandler {
    fn name(&self) -> &str {
        "mysql"
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
    fn test_mysql_handler_new() {
        let handler = MySqlHandler::with_defaults();
        assert_eq!(<MySqlHandler as ProtocolHandler>::name(&handler), "mysql");
    }

    #[test]
    fn test_mysql_config() {
        let config = MySqlConfig::default();
        assert_eq!(config.default_port, 3306);
    }

    #[test]
    fn test_mysql_value_to_string() {
        assert_eq!(mysql_value_to_string(mysql_async::Value::NULL), "NULL");
        assert_eq!(mysql_value_to_string(mysql_async::Value::Int(42)), "42");
        assert_eq!(mysql_value_to_string(mysql_async::Value::UInt(100)), "100");
    }
}
