use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    send_disconnect, EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus,
    ProtocolHandler, RecordingConfig,
};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::csv_export::{generate_csv_filename, CsvExporter};
use crate::csv_import::CsvImporter;
use crate::query_executor::QueryExecutor;
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input, send_and_record,
};
use crate::security::{check_query_allowed, DatabaseSecuritySettings};

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(3000);

/// Environment variable to enable Oracle Instant Client
/// Set to the path of your Oracle Instant Client installation
/// Example: ORACLE_HOME=/opt/oracle/instantclient_21_1
pub const ORACLE_HOME_ENV: &str = "ORACLE_HOME";

/// Alternative environment variable for Oracle library path
pub const OCI_LIB_DIR_ENV: &str = "OCI_LIB_DIR";

/// Check if Oracle Instant Client is available at runtime
pub fn oracle_client_available() -> bool {
    std::env::var(ORACLE_HOME_ENV).is_ok() || std::env::var(OCI_LIB_DIR_ENV).is_ok()
}

/// Get Oracle client path for logging/diagnostics
pub fn oracle_client_path() -> Option<String> {
    std::env::var(ORACLE_HOME_ENV)
        .ok()
        .or_else(|| std::env::var(OCI_LIB_DIR_ENV).ok())
}

/// Oracle Database handler
///
/// Provides interactive SQL*Plus-like terminal access to Oracle databases.
///
/// # Oracle Instant Client Setup
///
/// To enable real Oracle connectivity, you must:
/// 1. Download Oracle Instant Client from Oracle's website
/// 2. Accept Oracle's OTN License
/// 3. Set environment variable: `ORACLE_HOME=/path/to/instantclient`
///    or `OCI_LIB_DIR=/path/to/instantclient`
///
/// Without these environment variables, the handler runs in simulation mode.
pub struct OracleHandler {
    config: OracleConfig,
}

#[derive(Debug, Clone)]
pub struct OracleConfig {
    pub default_port: u16,
    pub service_name: String,
    pub require_encryption: bool,
    pub connection_timeout_secs: u64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            default_port: 1521,
            service_name: "ORCL".to_string(),
            require_encryption: true,
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl OracleHandler {
    pub fn new(config: OracleConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(OracleConfig::default())
    }

    /// Check if real Oracle mode is available
    pub fn is_real_mode_available(&self) -> bool {
        oracle_client_available()
    }
}

#[async_trait]
impl ProtocolHandler for OracleHandler {
    fn name(&self) -> &str {
        "oracle"
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
        info!("Oracle handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("Oracle: Read-only mode enabled");
        }

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);

        // Parse connection parameters
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?
            .clone();
        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);
        let username = params
            .get("username")
            .ok_or_else(|| HandlerError::MissingParameter("username".to_string()))?
            .clone();
        let password = params
            .get("password")
            .ok_or_else(|| HandlerError::MissingParameter("password".to_string()))?
            .clone();
        let service = params
            .get("service")
            .cloned()
            .unwrap_or_else(|| self.config.service_name.clone());

        info!(
            "Oracle: Connecting to {}@{}:{}/{}",
            username, hostname, port, service
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
            "Oracle: Display size {}x{} px → {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with SQL*Plus prompt and correct dimensions
        let prompt = if security.base.read_only {
            "SQL [RO]> "
        } else {
            "SQL> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "oracle", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "Oracle", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("Oracle: Sent display init instructions");

        // Check if real Oracle mode is available
        let real_mode = oracle_client_available();

        if real_mode {
            // Real Oracle connection
            info!(
                "Oracle: Using Oracle Instant Client from {:?}",
                oracle_client_path()
            );

            // Try to connect - Oracle connection is blocking and not Send,
            // so we run it in a spawn_blocking context
            let connect_string = format!("//{}:{}/{}", hostname, port, service);
            let username_clone = username.clone();
            let password_clone = password.clone();

            let conn_result = tokio::task::spawn_blocking(move || {
                oracle::Connection::connect(&username_clone, &password_clone, &connect_string)
            })
            .await
            .map_err(|e| HandlerError::ProtocolError(format!("Task join error: {}", e)))?;

            match conn_result {
                Ok(conn) => {
                    info!("Oracle: Connected successfully");

                    // Run the real mode session
                    // Note: oracle::Connection is not Send, so we need to handle it
                    // in a way that keeps it on the same thread
                    self.run_real_mode_session(
                        conn,
                        &security,
                        &recording_config,
                        &mut recorder,
                        &mut executor,
                        &to_client,
                        &mut from_client,
                    )
                    .await
                }
                Err(e) => {
                    let err_msg = format!("Oracle connection failed: {}", e);
                    warn!("{}", err_msg);

                    executor
                        .terminal
                        .write_error(&err_msg)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    executor
                        .terminal
                        .write_line("")
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    executor
                        .terminal
                        .write_line("Falling back to simulation mode...")
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

                    // Fall back to simulation
                    self.run_simulation_mode(
                        &hostname,
                        port,
                        &username,
                        &service,
                        &security,
                        &recording_config,
                        &mut recorder,
                        &mut executor,
                        &to_client,
                        &mut from_client,
                    )
                    .await
                }
            }
        } else {
            // Simulation mode
            info!("Oracle: Running in simulation mode (set ORACLE_HOME or OCI_LIB_DIR to enable real connections)");
            self.run_simulation_mode(
                &hostname,
                port,
                &username,
                &service,
                &security,
                &recording_config,
                &mut recorder,
                &mut executor,
                &to_client,
                &mut from_client,
            )
            .await
        }
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Result of executing a query against Oracle
struct OracleQueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    #[allow(dead_code)]
    affected_rows: Option<u64>,
    is_error: bool,
    message: String,
}

use guacr_handlers::MultiFormatRecorder;

impl OracleHandler {
    /// Run a real Oracle session
    ///
    /// Note: This uses spawn_blocking for each query since oracle::Connection is not Send
    #[allow(clippy::too_many_arguments)]
    async fn run_real_mode_session(
        &self,
        conn: oracle::Connection,
        security: &DatabaseSecuritySettings,
        recording_config: &RecordingConfig,
        recorder: &mut Option<MultiFormatRecorder>,
        executor: &mut QueryExecutor,
        to_client: &mpsc::Sender<Bytes>,
        from_client: &mut mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        // Wrap connection in Arc for sharing with spawn_blocking
        // Using parking_lot::Mutex which doesn't poison on panic
        use parking_lot::Mutex;
        let conn = Arc::new(Mutex::new(conn));

        // Send welcome message
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("Connected to Oracle Database")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Get database version
        {
            let conn_clone = Arc::clone(&conn);
            let version = tokio::task::spawn_blocking(move || {
                let conn = conn_clone.lock();
                conn.query_row_as::<String>("SELECT banner FROM v$version WHERE ROWNUM = 1", &[])
                    .ok()
            })
            .await
            .ok()
            .flatten();

            if let Some(ver) = version {
                executor
                    .terminal
                    .write_line(&ver)
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
        }

        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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

        // Main event loop

        // Debounce timer for batching screen updates (60 FPS)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        'outer: loop {
            tokio::select! {
                // Debounce tick - render if terminal changed
                _ = debounce.tick() => {
                    // Check if client is still connected before rendering
                    if to_client.is_closed() {
                        debug!("Oracle: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if send_and_record(to_client, recorder, instr).await.is_err() {
                                debug!("Oracle: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Oracle: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                            if let Some(query) = pending_query {
                                info!("Oracle: Query: {}", query);

                        // Record query input
                        record_query_input(recorder, recording_config, &query);

                        // Handle built-in commands
                        if handle_builtin_command(&query, executor, to_client, security).await? {
                            continue;
                        }

                        // Check for export command
                        if query.to_lowercase().starts_with("\\e ") {
                            let export_query = query[3..].trim().to_string();
                            handle_csv_export_real(
                                &export_query,
                                &conn,
                                executor,
                                to_client,
                                security,
                            )
                            .await?;
                            continue;
                        }

                        // Check for import command
                        if query.to_lowercase().starts_with("\\i ") {
                            let table_name = query[3..].trim().to_string();
                            handle_csv_import_real(
                                &table_name,
                                &conn,
                                executor,
                                to_client,
                                security,
                            )
                            .await?;
                            continue;
                        }

                        // Check read-only mode
                        if let Err(msg) = check_query_allowed(&query, security) {
                            executor
                                .terminal
                                .write_error(&msg)
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            executor
                                .terminal
                                .write_prompt()
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            let (_, result_instructions) = executor
                                .render_screen()
                                .await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            for instr in result_instructions {
                                send_and_record(to_client, recorder, instr)
                                    .await
                                    .map_err(HandlerError::ChannelError)?;
                            }
                            continue;
                        }

                        // Execute real query
                        execute_real_query(&query, &conn, executor, to_client).await?;
                        continue;
                    }

                            if needs_render {
                                // Render immediately for special cases (Enter, Escape, etc.)
                                for instr in instructions {
                                    let _ = send_and_record(to_client, recorder, instr).await;
                                }
                            }
                            // For regular keystrokes, debounce timer will handle rendering
                        }
                        Err(e) => {
                            warn!("Oracle: Input processing error: {}", e);
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
        finalize_recording(recorder.take(), "Oracle");

        send_disconnect(to_client).await;
        info!("Oracle handler ended");
        Ok(())
    }

    /// Run in simulation mode (no Oracle client installed)
    #[allow(clippy::too_many_arguments)]
    async fn run_simulation_mode(
        &self,
        hostname: &str,
        port: u16,
        username: &str,
        service: &str,
        security: &DatabaseSecuritySettings,
        recording_config: &RecordingConfig,
        recorder: &mut Option<MultiFormatRecorder>,
        executor: &mut QueryExecutor,
        to_client: &mpsc::Sender<Bytes>,
        from_client: &mut mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        // Send initial screen with Oracle information
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("Oracle Database Handler - SIMULATION MODE")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line(&format!(
                "Target: {}@{}:{}/{}",
                username, hostname, port, service
            ))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Note about Oracle client requirements
        executor
            .terminal
            .write_line("To enable real Oracle connections:")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("  1. Download Oracle Instant Client from oracle.com")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("  2. Accept Oracle's OTN License")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("  3. Set environment variable:")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("     export ORACLE_HOME=/path/to/instantclient")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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

        // Send initial screen
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

        // Event loop - process commands in simulation mode

        // Debounce timer for batching screen updates (60 FPS)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        'outer: loop {
            tokio::select! {
                // Debounce tick - render if terminal changed
                _ = debounce.tick() => {
                    // Check if client is still connected before rendering
                    if to_client.is_closed() {
                        debug!("Oracle: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if send_and_record(to_client, recorder, instr).await.is_err() {
                                debug!("Oracle: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Oracle: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                            if let Some(query) = pending_query {
                                info!("Oracle: Command: {}", query);

                        // Record query input
                        record_query_input(recorder, recording_config, &query);

                        // Handle built-in commands
                        if handle_builtin_command(&query, executor, to_client, security).await? {
                            continue;
                        }

                        // Check for export command: \e <query>
                        if query.to_lowercase().starts_with("\\e ") {
                            let export_query = query[3..].trim();
                            handle_csv_export_simulated(
                                export_query,
                                executor,
                                to_client,
                                security,
                            )
                            .await?;
                            continue;
                        }

                        // Check read-only mode
                        if let Err(msg) = check_query_allowed(&query, security) {
                            executor
                                .terminal
                                .write_error(&msg)
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            executor
                                .terminal
                                .write_prompt()
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            let (_, result_instructions) = executor
                                .render_screen()
                                .await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            for instr in result_instructions {
                                send_and_record(to_client, recorder, instr)
                                    .await
                                    .map_err(HandlerError::ChannelError)?;
                            }
                            continue;
                        }

                        // Simulate Oracle response
                        let result = simulate_oracle_query(&query);
                        match result {
                            Ok(output) => {
                                for line in output.lines() {
                                    executor
                                        .terminal
                                        .write_line(line)
                                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                }
                            }
                            Err(e) => {
                                // Record error output
                                record_error_output(recorder, &e);

                                executor
                                    .terminal
                                    .write_error(&e)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }

                        executor
                            .terminal
                            .write_prompt()
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                        let (_, result_instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in result_instructions {
                            send_and_record(to_client, recorder, instr)
                                .await
                                .map_err(HandlerError::ChannelError)?;
                        }
                                continue;
                            }

                            if needs_render {
                                // Render immediately for special cases (Enter, Escape, etc.)
                                for instr in instructions {
                                    let _ = send_and_record(to_client, recorder, instr).await;
                                }
                            }
                            // For regular keystrokes, debounce timer will handle rendering
                        }
                        Err(e) => {
                            warn!("Oracle: Input processing error: {}", e);
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
        finalize_recording(recorder.take(), "Oracle");

        info!("Oracle handler ended");
        Ok(())
    }
}

/// Execute a real Oracle query using spawn_blocking
async fn execute_real_query(
    query: &str,
    conn: &Arc<parking_lot::Mutex<oracle::Connection>>,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
) -> guacr_handlers::Result<()> {
    let query_owned = query.to_string();
    let conn_clone = Arc::clone(conn);

    // Execute query in blocking context
    // Using parking_lot::Mutex which doesn't poison on panic
    let result = tokio::task::spawn_blocking(move || {
        let conn = conn_clone.lock();
        let start_time = std::time::Instant::now();
        let query_upper = query_owned.trim().to_uppercase();

        let is_select = query_upper.starts_with("SELECT")
            || query_upper.starts_with("WITH")
            || query_upper.starts_with("DESCRIBE")
            || query_upper.starts_with("DESC")
            || query_upper.starts_with("SHOW");

        if is_select {
            match conn.query(&query_owned, &[]) {
                Ok(rows) => {
                    let column_info = rows.column_info();
                    let col_count = column_info.len();
                    let col_names: Vec<String> =
                        column_info.iter().map(|c| c.name().to_string()).collect();

                    let mut data_rows: Vec<Vec<String>> = Vec::new();
                    for row_result in rows {
                        match row_result {
                            Ok(row) => {
                                let mut row_data = Vec::new();
                                for i in 0..col_count {
                                    let value: Option<String> = row.get(i).ok();
                                    row_data.push(value.unwrap_or_else(|| "NULL".to_string()));
                                }
                                data_rows.push(row_data);
                            }
                            Err(e) => {
                                warn!("Oracle: Row fetch error: {}", e);
                                break;
                            }
                        }
                    }

                    let elapsed = start_time.elapsed();
                    OracleQueryResult {
                        columns: col_names,
                        rows: data_rows,
                        affected_rows: None,
                        is_error: false,
                        message: format!("{:.3}s", elapsed.as_secs_f64()),
                    }
                }
                Err(e) => OracleQueryResult {
                    columns: vec![],
                    rows: vec![],
                    affected_rows: None,
                    is_error: true,
                    message: format!("ORA-Error: {}", e),
                },
            }
        } else {
            match conn.execute(&query_owned, &[]) {
                Ok(stmt) => {
                    let affected = stmt.row_count().unwrap_or(0);
                    let elapsed = start_time.elapsed();

                    // Auto-commit
                    if let Err(e) = conn.commit() {
                        return OracleQueryResult {
                            columns: vec![],
                            rows: vec![],
                            affected_rows: Some(affected),
                            is_error: true,
                            message: format!("Commit failed: {}", e),
                        };
                    }

                    let msg = if query_upper.starts_with("INSERT")
                        || query_upper.starts_with("UPDATE")
                        || query_upper.starts_with("DELETE")
                    {
                        format!(
                            "{} row(s) affected. ({:.3}s)",
                            affected,
                            elapsed.as_secs_f64()
                        )
                    } else {
                        format!("Statement executed. ({:.3}s)", elapsed.as_secs_f64())
                    };

                    OracleQueryResult {
                        columns: vec![],
                        rows: vec![],
                        affected_rows: Some(affected),
                        is_error: false,
                        message: msg,
                    }
                }
                Err(e) => OracleQueryResult {
                    columns: vec![],
                    rows: vec![],
                    affected_rows: None,
                    is_error: true,
                    message: format!("ORA-Error: {}", e),
                },
            }
        }
    })
    .await
    .map_err(|e| HandlerError::ProtocolError(format!("Task join error: {}", e)))?;

    // Display results
    if result.is_error {
        executor
            .terminal
            .write_error(&result.message)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    } else if !result.columns.is_empty() {
        // Display as table
        let query_result = guacr_terminal::QueryResult {
            columns: result.columns,
            rows: result.rows.clone(),
            affected_rows: None,
            execution_time_ms: None,
        };

        executor
            .write_result(&query_result)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        executor
            .terminal
            .write_line(&format!(
                "\n{} row(s) selected. ({})",
                result.rows.len(),
                result.message
            ))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    } else {
        executor
            .terminal
            .write_success(&result.message)
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

    Ok(())
}

/// Handle CSV export with real Oracle connection
async fn handle_csv_export_real(
    query: &str,
    conn: &Arc<parking_lot::Mutex<oracle::Connection>>,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use std::sync::atomic::Ordering;

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

    // Execute query in blocking context
    let query_owned = query.to_string();
    let conn_clone = Arc::clone(conn);

    let result = tokio::task::spawn_blocking(move || {
        let conn = conn_clone.lock();
        match conn.query(&query_owned, &[]) {
            Ok(rows) => {
                let column_info = rows.column_info();
                let col_count = column_info.len();
                let col_names: Vec<String> =
                    column_info.iter().map(|c| c.name().to_string()).collect();

                let mut data_rows: Vec<Vec<String>> = Vec::new();
                for row_result in rows {
                    match row_result {
                        Ok(row) => {
                            let mut row_data = Vec::new();
                            for i in 0..col_count {
                                let value: Option<String> = row.get(i).ok();
                                row_data.push(value.unwrap_or_default());
                            }
                            data_rows.push(row_data);
                        }
                        Err(e) => {
                            warn!("Oracle: Row fetch error during export: {}", e);
                            break;
                        }
                    }
                }

                Ok((col_names, data_rows))
            }
            Err(e) => Err(format!("Query failed: {}", e)),
        }
    })
    .await
    .map_err(|e| HandlerError::ProtocolError(format!("Task join error: {}", e)))?;

    match result {
        Ok((columns, rows)) => {
            let query_result = guacr_terminal::QueryResult {
                columns,
                rows: rows.clone(),
                affected_rows: None,
                execution_time_ms: None,
            };

            let filename = generate_csv_filename(query, "oracle");
            let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
            let mut exporter = CsvExporter::new(stream_idx);

            executor
                .terminal
                .write_line(&format!("Beginning CSV download ({} rows)...", rows.len()))
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

            let file_instr = exporter.start_download(&filename);
            to_client
                .send(file_instr)
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

            match exporter.export_query_result(&query_result, to_client).await {
                Ok(()) => {
                    executor
                        .terminal
                        .write_success("Download complete.")
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
        Err(e) => {
            executor
                .terminal
                .write_error(&e)
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

/// Handle CSV import with real Oracle connection
async fn handle_csv_import_real(
    table_name: &str,
    conn: &Arc<parking_lot::Mutex<oracle::Connection>>,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
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
    let sample_csv = "ID,NAME,VALUE\n1,Test,100\n2,Demo,200";
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

    // Generate Oracle INSERT statements and execute
    let table_name_owned = table_name.to_string();
    let headers = csv_data.headers.clone();
    let rows = csv_data.rows.clone();
    let conn_clone = Arc::clone(conn);

    let (success_count, error_count) = tokio::task::spawn_blocking(move || {
        let conn = conn_clone.lock();
        let columns = headers.join("\", \"");
        let mut success = 0;
        let mut errors = 0;

        for row in &rows {
            if row.len() != headers.len() {
                continue;
            }

            let values: Vec<String> = row
                .iter()
                .map(|v| {
                    if v.eq_ignore_ascii_case("null") || v.is_empty() {
                        "NULL".to_string()
                    } else if v.parse::<f64>().is_ok() {
                        v.to_string()
                    } else {
                        format!("'{}'", v.replace('\'', "''"))
                    }
                })
                .collect();

            let insert = format!(
                "INSERT INTO \"{}\" (\"{}\") VALUES ({})",
                table_name_owned,
                columns,
                values.join(", ")
            );

            match conn.execute(&insert, &[]) {
                Ok(_) => success += 1,
                Err(e) => {
                    errors += 1;
                    warn!("Oracle import error: {}", e);
                }
            }
        }

        if let Err(e) = conn.commit() {
            warn!("Oracle commit error: {}", e);
        }

        (success, errors)
    })
    .await
    .map_err(|e| HandlerError::ProtocolError(format!("Task join error: {}", e)))?;

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

/// Simulate Oracle query response (demonstration mode)
fn simulate_oracle_query(query: &str) -> Result<String, String> {
    let query_upper = query.to_uppercase();

    // Simulate common Oracle commands
    if query_upper.starts_with("SELECT") {
        if query_upper.contains("DUAL") {
            return Ok("DUMMY\n-----\nX\n\n1 row selected.".to_string());
        }
        if query_upper.contains("SYSDATE") {
            return Ok(format!(
                "SYSDATE\n---------\n{}\n\n1 row selected.",
                chrono::Local::now().format("%d-%b-%Y")
            ));
        }
        if query_upper.contains("USER") {
            return Ok("USER\n----\nSYSTEM\n\n1 row selected.".to_string());
        }
        return Ok(
            "[Simulation mode - query would execute against Oracle]\n\nno rows selected"
                .to_string(),
        );
    }

    if query_upper.starts_with("DESCRIBE") || query_upper.starts_with("DESC") {
        return Ok("[Simulation mode - DESCRIBE requires Oracle connection]\n\nName                              Type\n--------------------------------  ----\n...".to_string());
    }

    if query_upper.starts_with("SHOW") {
        if query_upper.contains("USER") {
            return Ok("USER is \"SYSTEM\"".to_string());
        }
        if query_upper.contains("PARAMETER") {
            return Ok("[Simulation mode - requires Oracle connection]".to_string());
        }
    }

    Ok(
        "[Simulation mode - command acknowledged]\n\nPL/SQL procedure successfully completed."
            .to_string(),
    )
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
        "help" | "?" => {
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Oracle SQL*Plus Commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("SQL Commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT ... FROM ...   Execute query")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  DESC table_name       Describe table")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("SQL*Plus Commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SHOW USER             Show current user")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  QUIT/EXIT             Disconnect")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

            if !security.disable_csv_export {
                executor
                    .terminal
                    .write_line("  \\e <query>            Export query as CSV")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            if !security.disable_csv_import && !security.base.read_only {
                executor
                    .terminal
                    .write_line("  \\i <table>            Import CSV into table")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }

            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Example queries:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT SYSDATE FROM DUAL;")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SELECT * FROM ALL_TABLES WHERE ROWNUM <= 10;")
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
        "quit" | "exit" | "bye" => {
            executor
                .terminal
                .write_line("Disconnected from Oracle Database.")
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

/// Handle CSV export in simulation mode
async fn handle_csv_export_simulated(
    query: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use std::sync::atomic::Ordering;

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
        .write_line("Executing query for export (simulation)...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // In simulation mode, create sample data
    let result = guacr_terminal::QueryResult {
        columns: vec![
            "COLUMN1".to_string(),
            "COLUMN2".to_string(),
            "COLUMN3".to_string(),
        ],
        rows: vec![
            vec![
                "Value1".to_string(),
                "Value2".to_string(),
                "Value3".to_string(),
            ],
            vec!["Sample".to_string(), "Data".to_string(), "Row".to_string()],
        ],
        affected_rows: None,
        execution_time_ms: None,
    };

    let filename = generate_csv_filename(query, "oracle");
    let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
    let mut exporter = CsvExporter::new(stream_idx);

    executor
        .terminal
        .write_line(&format!(
            "Beginning CSV download ({} rows). Press Ctrl+C to cancel.",
            result.rows.len()
        ))
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

    let file_instr = exporter.start_download(&filename);
    to_client
        .send(file_instr)
        .await
        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

    match exporter.export_query_result(&result, to_client).await {
        Ok(()) => {
            executor
                .terminal
                .write_line("Download complete (simulation data).")
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
impl EventBasedHandler for OracleHandler {
    fn name(&self) -> &str {
        "oracle"
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
    fn test_oracle_handler_new() {
        let handler = OracleHandler::with_defaults();
        assert_eq!(<OracleHandler as ProtocolHandler>::name(&handler), "oracle");
    }

    #[test]
    fn test_oracle_config() {
        let config = OracleConfig::default();
        assert_eq!(config.default_port, 1521);
        assert_eq!(config.service_name, "ORCL");
        assert!(config.require_encryption);
    }

    #[test]
    fn test_simulate_oracle_query() {
        let result = simulate_oracle_query("SELECT SYSDATE FROM DUAL");
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains("row selected") || output.contains("-"),
            "Expected date output: {}",
            output
        );
    }

    #[test]
    fn test_oracle_client_check() {
        // This will vary based on environment
        let available = oracle_client_available();
        let path = oracle_client_path();

        if available {
            assert!(path.is_some());
        }
    }

    #[test]
    fn test_env_var_names() {
        assert_eq!(ORACLE_HOME_ENV, "ORACLE_HOME");
        assert_eq!(OCI_LIB_DIR_ENV, "OCI_LIB_DIR");
    }
}
