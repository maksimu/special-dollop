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
use crate::handler_helpers::{
    handle_quit, parse_display_size, render_connection_error, render_connection_success,
    render_help, send_render, HelpSection,
};
use crate::query_executor::{execute_with_timing, QueryExecutor};
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
    record_query_output,
};
use crate::security::{
    check_csv_export_allowed, check_csv_import_allowed, check_query_allowed,
    DatabaseSecuritySettings,
};

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(5000);

/// Generic ODBC database handler
///
/// Provides interactive SQL terminal access to any ODBC-compatible database.
/// Covers IBM DB2, SAP HANA, Teradata, Informix, Vertica, Greenplum, Netezza,
/// and any other database accessible via ODBC drivers.
///
/// IMPORTANT: odbc-api is a synchronous library. All ODBC calls are wrapped
/// in tokio::task::spawn_blocking() to avoid blocking the async runtime.
pub struct OdbcHandler {
    config: OdbcConfig,
}

#[derive(Debug, Clone)]
pub struct OdbcConfig {
    pub default_port: u16,
    pub connection_timeout_secs: u64,
}

impl Default for OdbcConfig {
    fn default() -> Self {
        Self {
            default_port: 5432, // No universal default; PostgreSQL is common for testing
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl OdbcHandler {
    pub fn new(config: OdbcConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(OdbcConfig::default())
    }
}

/// Build an ODBC connection string from handler parameters.
///
/// Supports three modes (in priority order):
/// 1. DSN-based: Uses a pre-configured Data Source Name
/// 2. Raw connection string: Uses a user-provided connection string directly
/// 3. Component-based: Builds from individual hostname, port, driver, etc.
fn build_connection_string(params: &HashMap<String, String>, default_port: u16) -> String {
    if let Some(dsn) = params.get("dsn") {
        // DSN-based connection
        format!(
            "DSN={};UID={};PWD={}",
            dsn,
            params.get("username").map(|s| s.as_str()).unwrap_or(""),
            params.get("password").map(|s| s.as_str()).unwrap_or("")
        )
    } else if let Some(conn_str) = params.get("connection-string") {
        // Raw connection string provided directly
        conn_str.clone()
    } else {
        // Build from individual components
        let driver = params
            .get("driver")
            .map(|s| s.as_str())
            .unwrap_or("PostgreSQL");
        let hostname = params
            .get("hostname")
            .map(|s| s.as_str())
            .unwrap_or("localhost");
        let default_port_str = default_port.to_string();
        let port = params
            .get("port")
            .map(|s| s.as_str())
            .unwrap_or(&default_port_str);
        let database = params.get("database").map(|s| s.as_str()).unwrap_or("");
        let username = params.get("username").map(|s| s.as_str()).unwrap_or("");
        let password = params.get("password").map(|s| s.as_str()).unwrap_or("");

        format!(
            "DRIVER={{{}}};SERVER={};PORT={};DATABASE={};UID={};PWD={}",
            driver, hostname, port, database, username, password
        )
    }
}

/// Redact password from a connection string for safe logging.
fn redact_connection_string(conn_str: &str) -> String {
    let mut result = conn_str.to_string();
    // Case-insensitive search for PWD=
    if let Some(pwd_start) = result.to_uppercase().find("PWD=") {
        let after_pwd = pwd_start + 4;
        let pwd_end = result[after_pwd..]
            .find(';')
            .map(|pos| after_pwd + pos)
            .unwrap_or(result.len());
        result.replace_range(after_pwd..pwd_end, "***");
    }
    result
}

/// Check if an SQL statement is a modifying operation.
///
/// This supplements the shared security module's classifier with additional
/// database-vendor-specific keywords that ODBC-connected databases may use.
fn is_sql_modifying_command(query: &str) -> bool {
    let trimmed = query.trim().to_uppercase();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    matches!(
        first_word,
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "DROP"
            | "TRUNCATE"
            | "ALTER"
            | "CREATE"
            | "GRANT"
            | "REVOKE"
            | "MERGE"
            | "UPSERT"
            | "REPLACE"
            | "RENAME"
            | "CALL"
            | "EXEC"
            | "EXECUTE"
    )
}

/// Execute an SQL query via ODBC and return a QueryResult.
///
/// This function runs synchronously inside spawn_blocking because odbc-api
/// is not async. The Environment is created per-call to avoid Send/Sync issues
/// with ODBC handles.
async fn execute_odbc_query(
    connection_string: String,
    query: String,
) -> Result<QueryResult, String> {
    tokio::task::spawn_blocking(move || {
        use odbc_api::buffers::TextRowSet;
        use odbc_api::{ConnectionOptions, Cursor, Environment, ResultSetMetadata};

        let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

        let conn = env
            .connect_with_connection_string(&connection_string, ConnectionOptions::default())
            .map_err(|e| format!("ODBC connection error: {}", e))?;

        let maybe_cursor = conn
            .execute(&query, (), None)
            .map_err(|e| format!("ODBC query error: {}", e))?;

        match maybe_cursor {
            Some(mut cursor) => {
                // Get column count
                let num_cols = cursor
                    .num_result_cols()
                    .map_err(|e| format!("Column count error: {}", e))?
                    as u16;

                if num_cols == 0 {
                    return Ok(QueryResult {
                        columns: vec!["Result".to_string()],
                        rows: vec![vec!["OK".to_string()]],
                        affected_rows: None,
                        execution_time_ms: None,
                    });
                }

                // Get column names
                let mut columns = Vec::with_capacity(num_cols as usize);
                for i in 1..=num_cols {
                    let name = cursor
                        .col_name(i)
                        .map_err(|e| format!("Column name error: {}", e))?;
                    columns.push(if name.is_empty() {
                        format!("col{}", i)
                    } else {
                        name
                    });
                }

                // Bind text buffers and fetch rows
                // batch_size=1000 rows per fetch, max 8192 bytes per field
                let buffer = TextRowSet::for_cursor(1000, &mut cursor, Some(8192))
                    .map_err(|e| format!("Buffer allocation error: {}", e))?;

                let mut row_set_cursor = cursor
                    .bind_buffer(buffer)
                    .map_err(|e| format!("Buffer bind error: {}", e))?;

                let mut rows: Vec<Vec<String>> = Vec::new();

                while let Some(batch) = row_set_cursor
                    .fetch()
                    .map_err(|e| format!("Fetch error: {}", e))?
                {
                    for row_idx in 0..batch.num_rows() {
                        let mut row: Vec<String> = Vec::with_capacity(num_cols as usize);
                        for col_idx in 0..(num_cols as usize) {
                            let value = match batch.at(col_idx, row_idx) {
                                Some(bytes) => String::from_utf8_lossy(bytes).to_string(),
                                None => "NULL".to_string(),
                            };
                            row.push(value);
                        }
                        rows.push(row);
                    }
                }

                Ok(QueryResult {
                    columns,
                    rows,
                    affected_rows: None,
                    execution_time_ms: None,
                })
            }
            None => {
                // Non-SELECT statement (INSERT, UPDATE, DELETE, DDL, etc.)
                Ok(QueryResult {
                    columns: vec!["Result".to_string()],
                    rows: vec![vec!["OK".to_string()]],
                    affected_rows: None,
                    execution_time_ms: None,
                })
            }
        }
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// List tables via the ODBC catalog function SQLTables.
async fn execute_odbc_list_tables(connection_string: String) -> Result<QueryResult, String> {
    tokio::task::spawn_blocking(move || {
        use odbc_api::buffers::TextRowSet;
        use odbc_api::{ConnectionOptions, Cursor, Environment};

        let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

        let conn = env
            .connect_with_connection_string(&connection_string, ConnectionOptions::default())
            .map_err(|e| format!("ODBC connection error: {}", e))?;

        // SQLTables returns: TABLE_CAT, TABLE_SCHEM, TABLE_NAME, TABLE_TYPE, REMARKS
        let mut cursor = conn
            .tables("", "", "", "TABLE")
            .map_err(|e| format!("SQLTables error: {}", e))?;

        let columns = vec![
            "Catalog".to_string(),
            "Schema".to_string(),
            "Table".to_string(),
            "Type".to_string(),
            "Remarks".to_string(),
        ];

        let num_cols: usize = 5;

        let buffer = TextRowSet::for_cursor(1000, &mut cursor, Some(4096))
            .map_err(|e| format!("Buffer allocation error: {}", e))?;

        let mut row_set_cursor = cursor
            .bind_buffer(buffer)
            .map_err(|e| format!("Buffer bind error: {}", e))?;

        let mut rows: Vec<Vec<String>> = Vec::new();

        while let Some(batch) = row_set_cursor
            .fetch()
            .map_err(|e| format!("Fetch error: {}", e))?
        {
            for row_idx in 0..batch.num_rows() {
                let mut row = Vec::with_capacity(num_cols);
                for col_idx in 0..num_cols {
                    let value = match batch.at(col_idx, row_idx) {
                        Some(bytes) => String::from_utf8_lossy(bytes).to_string(),
                        None => "NULL".to_string(),
                    };
                    row.push(value);
                }
                rows.push(row);
            }
        }

        Ok(QueryResult {
            columns,
            rows,
            affected_rows: None,
            execution_time_ms: None,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// List columns for a specific table via the ODBC catalog function SQLColumns.
async fn execute_odbc_list_columns(
    connection_string: String,
    table_name: String,
) -> Result<QueryResult, String> {
    tokio::task::spawn_blocking(move || {
        use odbc_api::buffers::TextRowSet;
        use odbc_api::{ConnectionOptions, Cursor, Environment, ResultSetMetadata};

        let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

        let conn = env
            .connect_with_connection_string(&connection_string, ConnectionOptions::default())
            .map_err(|e| format!("ODBC connection error: {}", e))?;

        // SQLColumns returns many columns. The most useful are:
        // Index 3: COLUMN_NAME
        // Index 5: TYPE_NAME
        // Index 6: COLUMN_SIZE
        // Index 10: NULLABLE (0=NO, 1=YES, 2=UNKNOWN)
        let mut cursor = conn
            .columns("", "", &table_name, "")
            .map_err(|e| format!("SQLColumns error: {}", e))?;

        let result_columns = vec![
            "Column".to_string(),
            "Type".to_string(),
            "Size".to_string(),
            "Nullable".to_string(),
        ];

        let num_result_cols = cursor
            .num_result_cols()
            .map_err(|e| format!("Column count error: {}", e))?
            as usize;

        let buffer = TextRowSet::for_cursor(1000, &mut cursor, Some(4096))
            .map_err(|e| format!("Buffer allocation error: {}", e))?;

        let mut row_set_cursor = cursor
            .bind_buffer(buffer)
            .map_err(|e| format!("Buffer bind error: {}", e))?;

        let mut rows: Vec<Vec<String>> = Vec::new();

        while let Some(batch) = row_set_cursor
            .fetch()
            .map_err(|e| format!("Fetch error: {}", e))?
        {
            for row_idx in 0..batch.num_rows() {
                // Extract the four fields we care about from the catalog result
                let col_name = if 3 < num_result_cols {
                    batch
                        .at(3, row_idx)
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_else(|| "NULL".to_string())
                } else {
                    "?".to_string()
                };

                let type_name = if 5 < num_result_cols {
                    batch
                        .at(5, row_idx)
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_else(|| "NULL".to_string())
                } else {
                    "?".to_string()
                };

                let col_size = if 6 < num_result_cols {
                    batch
                        .at(6, row_idx)
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_else(|| "NULL".to_string())
                } else {
                    "?".to_string()
                };

                let nullable = if 10 < num_result_cols {
                    batch
                        .at(10, row_idx)
                        .map(|b| {
                            let val = String::from_utf8_lossy(b).to_string();
                            match val.as_str() {
                                "0" => "NO".to_string(),
                                "1" => "YES".to_string(),
                                "2" => "UNKNOWN".to_string(),
                                _ => val,
                            }
                        })
                        .unwrap_or_else(|| "UNKNOWN".to_string())
                } else {
                    "?".to_string()
                };

                rows.push(vec![col_name, type_name, col_size, nullable]);
            }
        }

        Ok(QueryResult {
            columns: result_columns,
            rows,
            affected_rows: None,
            execution_time_ms: None,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// List installed ODBC drivers on the system.
async fn execute_odbc_list_drivers() -> Result<QueryResult, String> {
    tokio::task::spawn_blocking(move || {
        use odbc_api::Environment;

        let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

        let mut rows: Vec<Vec<String>> = Vec::new();

        for driver_info in env
            .drivers()
            .map_err(|e| format!("Driver list error: {}", e))?
        {
            let name = driver_info.description.clone();
            let attrs = driver_info
                .attributes
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("; ");
            rows.push(vec![name, attrs]);
        }

        Ok(QueryResult {
            columns: vec!["Driver".to_string(), "Attributes".to_string()],
            rows,
            affected_rows: None,
            execution_time_ms: None,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// List configured ODBC Data Source Names on the system.
async fn execute_odbc_list_dsns() -> Result<QueryResult, String> {
    tokio::task::spawn_blocking(move || {
        use odbc_api::Environment;

        let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

        let mut rows: Vec<Vec<String>> = Vec::new();

        for dsn_info in env
            .data_sources()
            .map_err(|e| format!("DSN list error: {}", e))?
        {
            rows.push(vec![dsn_info.server_name.clone(), dsn_info.driver.clone()]);
        }

        Ok(QueryResult {
            columns: vec!["DSN".to_string(), "Driver".to_string()],
            rows,
            affected_rows: None,
            execution_time_ms: None,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

#[async_trait]
impl ProtocolHandler for OdbcHandler {
    fn name(&self) -> &str {
        "odbc"
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
        info!("ODBC handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("ODBC: Read-only mode enabled");
        }

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);

        // Build the ODBC connection string
        let connection_string = build_connection_string(&params, self.config.default_port);

        let database = params.get("database").map(|s| s.as_str()).unwrap_or("");
        let driver = params.get("driver").map(|s| s.as_str()).unwrap_or("ODBC");

        info!(
            "ODBC: Connecting with: {}",
            redact_connection_string(&connection_string)
        );

        // Parse display size from parameters (like SSH does)
        let (width, height, cols, rows) = parse_display_size(&params);

        info!(
            "ODBC: Display size {}x{} px -> {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with ODBC prompt and correct dimensions
        let prompt = if !database.is_empty() {
            if security.base.read_only {
                format!("odbc [{}] [RO]> ", database)
            } else {
                format!("odbc [{}]> ", database)
            }
        } else if security.base.read_only {
            "odbc [RO]> ".to_string()
        } else {
            "odbc> ".to_string()
        };
        let mut executor = QueryExecutor::new_with_size(&prompt, "odbc", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "ODBC", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("ODBC: Sent display init instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Test the connection by performing a lightweight operation
        let conn_str_for_test = connection_string.clone();
        let connect_result: Result<(), String> = tokio::task::spawn_blocking(move || {
            use odbc_api::{ConnectionOptions, Environment};

            let env = Environment::new().map_err(|e| format!("ODBC environment error: {}", e))?;

            let _conn = env
                .connect_with_connection_string(&conn_str_for_test, ConnectionOptions::default())
                .map_err(|e| format!("ODBC connection error: {}", e))?;

            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))
        .and_then(|r| r);

        match connect_result {
            Ok(()) => {
                info!("ODBC: Connected successfully via driver '{}'", driver);

                let conn_line = format!("Connected via ODBC (driver: {})", driver);
                let db_line = if !database.is_empty() {
                    Some(format!("Database: {}", database))
                } else {
                    None
                };
                let ro_line = if security.base.read_only {
                    Some("Mode: READ-ONLY (modifications disabled)".to_string())
                } else {
                    None
                };
                let export_line = if security.disable_csv_export {
                    Some("CSV Export: DISABLED".to_string())
                } else {
                    None
                };

                let mut info_lines: Vec<&str> = vec![&conn_line];
                if let Some(ref dl) = db_line {
                    info_lines.push(dl);
                }
                if let Some(ref rl) = ro_line {
                    info_lines.push(rl);
                }
                if let Some(ref el) = export_line {
                    info_lines.push(el);
                }
                info_lines.push("Type 'help' for available commands.");

                render_connection_success(&mut executor, &to_client, &info_lines).await?;
                debug!("ODBC: Initial screen sent successfully");
            }
            Err(e) => {
                let error_msg = format!("Connection failed: {}", e);
                warn!("ODBC: {}", error_msg);

                render_connection_error(
                    &mut executor,
                    &to_client,
                    &mut from_client,
                    &error_msg,
                    &[
                        "Verify the ODBC driver is installed (\\drivers)",
                        "Check DSN configuration (\\dsns)",
                        "Verify hostname and port are correct",
                        "Check credentials are correct",
                        "Ensure the database server is running",
                    ],
                )
                .await?;
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
        }

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
                        debug!("ODBC: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            // Break if send fails (client disconnected)
                            if to_client.send(instr).await.is_err() {
                                debug!("ODBC: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("ODBC: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(query) = pending_query {
                        info!("ODBC: Executing query: {}", query);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &query);

                        // Handle built-in commands
                        if handle_builtin_command(
                            &query,
                            &mut executor,
                            &to_client,
                            &security,
                            &connection_string,
                        )
                        .await?
                        {
                            continue;
                        }

                        // Check for export command: \e <query>
                        if query.to_lowercase().starts_with("\\e ") {
                            let export_query = query[3..].trim();
                            handle_csv_export(
                                export_query,
                                &connection_string,
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
                                &connection_string,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Check read-only mode via shared security module
                        if let Err(msg) = check_query_allowed(&query, &security) {
                            executor
                                .write_error(&msg)
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            send_render(&mut executor, &to_client).await?;
                            continue;
                        }

                        // Additional read-only check for vendor-specific commands
                        // the generic classifier might miss (EXEC, CALL, MERGE, etc.)
                        if security.base.read_only && is_sql_modifying_command(&query) {
                            executor
                                .write_error("Command blocked: read-only mode is enabled.")
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            send_render(&mut executor, &to_client).await?;
                            continue;
                        }

                        // Execute query via ODBC
                        let conn_str = connection_string.clone();
                        let q = query.clone();
                        match execute_with_timing(|| execute_odbc_query(conn_str, q)).await {
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

                        send_render(&mut executor, &to_client).await?;
                        continue;
                    }

                    if needs_render {
                        // Render immediately for special cases (Enter, Escape, etc.)
                        for instr in instructions {
                            to_client
                                .send(instr)
                                .await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }
                    }
                    // For regular keystrokes, debounce timer will handle rendering
                        }
                        Err(e) => {
                            warn!("ODBC: Input processing error: {}", e);
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
        finalize_recording(recorder, "ODBC");

        send_disconnect(&to_client).await;
        info!("ODBC handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Handle built-in commands (help, quit, \tables, \columns, \drivers, \dsns)
async fn handle_builtin_command(
    command: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    connection_string: &str,
) -> guacr_handlers::Result<bool> {
    let command_lower = command.to_lowercase();
    let command_trimmed = command_lower.trim();

    match command_trimmed {
        "help" | "?" | "\\h" | "\\?" => {
            let mut sections = vec![
                HelpSection {
                    title: "Navigation",
                    commands: vec![("help", "Show this help"), ("quit, exit", "Disconnect")],
                },
                HelpSection {
                    title: "Catalog commands",
                    commands: vec![
                        ("\\tables", "List tables"),
                        ("\\columns TBL", "List columns of a table"),
                        ("\\drivers", "List installed ODBC drivers"),
                        ("\\dsns", "List configured DSNs"),
                    ],
                },
            ];

            let mut export_cmds = Vec::new();
            if !security.disable_csv_export {
                export_cmds.push(("\\e <query>", "Export query results as CSV"));
            }
            if !security.disable_csv_import && !security.base.read_only {
                export_cmds.push(("\\i <table>", "Import CSV data into table"));
            }
            if !export_cmds.is_empty() {
                sections.push(HelpSection {
                    title: "Export/Import",
                    commands: export_cmds,
                });
            }

            let mut sql_cmds: Vec<(&'static str, &'static str)> =
                vec![("Type any SQL statement and press Enter to execute.", "")];
            if security.base.read_only {
                sql_cmds.push(("", ""));
                sql_cmds.push(("Note: READ-ONLY mode is enabled.", ""));
                sql_cmds.push(("INSERT/UPDATE/DELETE/DROP are disabled.", ""));
            }
            sections.push(HelpSection {
                title: "SQL",
                commands: sql_cmds,
            });

            render_help(executor, to_client, "ODBC SQL Terminal", &sections).await?;
            return Ok(true);
        }
        "quit" | "exit" | "\\q" => {
            return Err(handle_quit(executor, to_client).await);
        }
        "\\tables" => {
            let conn_str = connection_string.to_string();
            match execute_odbc_list_tables(conn_str).await {
                Ok(result) => {
                    executor
                        .write_result(&result)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
                Err(e) => {
                    executor
                        .write_error(&e)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
            }
            send_render(executor, to_client).await?;
            return Ok(true);
        }
        "\\drivers" => {
            match execute_odbc_list_drivers().await {
                Ok(result) => {
                    executor
                        .write_result(&result)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
                Err(e) => {
                    executor
                        .write_error(&e)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
            }
            send_render(executor, to_client).await?;
            return Ok(true);
        }
        "\\dsns" => {
            match execute_odbc_list_dsns().await {
                Ok(result) => {
                    executor
                        .write_result(&result)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
                Err(e) => {
                    executor
                        .write_error(&e)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
            }
            send_render(executor, to_client).await?;
            return Ok(true);
        }
        _ => {}
    }

    // Handle \columns <table> (has an argument so can't match exactly above)
    if command_trimmed.starts_with("\\columns") {
        let table_name = command_trimmed
            .strip_prefix("\\columns")
            .unwrap_or("")
            .trim();

        if table_name.is_empty() {
            executor
                .terminal
                .write_error("Usage: \\columns <table_name>")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_prompt()
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        } else {
            let conn_str = connection_string.to_string();
            match execute_odbc_list_columns(conn_str, table_name.to_string()).await {
                Ok(result) => {
                    if result.rows.is_empty() {
                        executor
                            .terminal
                            .write_line(&format!("No columns found for table '{}'.", table_name))
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        executor
                            .terminal
                            .write_prompt()
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    } else {
                        executor
                            .write_result(&result)
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    }
                }
                Err(e) => {
                    executor
                        .write_error(&e)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
            }
        }

        send_render(executor, to_client).await?;
        return Ok(true);
    }

    Ok(false)
}

/// Handle CSV export for an ODBC query
async fn handle_csv_export(
    query: &str,
    connection_string: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use std::sync::atomic::Ordering;

    // Check if export is allowed
    if let Err(msg) = check_csv_export_allowed(security) {
        executor
            .terminal
            .write_error(&msg)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client).await?;
        return Ok(());
    }

    executor
        .terminal
        .write_line("Executing query for export...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    send_render(executor, to_client).await?;

    // Execute the query via ODBC
    let conn_str = connection_string.to_string();
    let q = query.to_string();
    match execute_odbc_query(conn_str, q).await {
        Ok(result) => {
            if result.rows.is_empty() {
                executor
                    .terminal
                    .write_line("Query returned no results to export.")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            } else {
                // Generate filename and create exporter
                let filename = generate_csv_filename(query, "odbc");
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
    send_render(executor, to_client).await?;

    Ok(())
}

/// Handle CSV import for an ODBC table
async fn handle_csv_import(
    table_name: &str,
    connection_string: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    // Check if import is allowed
    if let Err(msg) = check_csv_import_allowed(security) {
        executor
            .terminal
            .write_error(&msg)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client).await?;
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
        send_render(executor, to_client).await?;
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
        send_render(executor, to_client).await?;
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

    // Generate INSERT statements using ANSI SQL syntax (double-quoted identifiers).
    // This is the most portable approach for ODBC since we don't know the
    // underlying database vendor at compile time.
    let inserts = importer
        .generate_postgres_inserts(table_name)
        .map_err(HandlerError::ProtocolError)?;

    let mut success_count = 0;
    let mut error_count = 0;

    for insert in &inserts {
        let conn_str = connection_string.to_string();
        let q = insert.clone();
        match execute_odbc_query(conn_str, q).await {
            Ok(_) => success_count += 1,
            Err(e) => {
                error_count += 1;
                warn!("ODBC import error: {}", e);
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
    send_render(executor, to_client).await?;

    Ok(())
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for OdbcHandler {
    fn name(&self) -> &str {
        "odbc"
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
    fn test_odbc_handler_new() {
        let handler = OdbcHandler::with_defaults();
        assert_eq!(<OdbcHandler as ProtocolHandler>::name(&handler), "odbc");
    }

    #[test]
    fn test_odbc_config() {
        let config = OdbcConfig::default();
        assert_eq!(config.default_port, 5432);
    }

    #[test]
    fn test_build_connection_string_dsn() {
        let mut params = HashMap::new();
        params.insert("dsn".to_string(), "MyDSN".to_string());
        params.insert("username".to_string(), "admin".to_string());
        params.insert("password".to_string(), "secret".to_string());

        let result = build_connection_string(&params, 5432);
        assert_eq!(result, "DSN=MyDSN;UID=admin;PWD=secret");
    }

    #[test]
    fn test_build_connection_string_raw() {
        let mut params = HashMap::new();
        params.insert(
            "connection-string".to_string(),
            "DRIVER={PostgreSQL};SERVER=db.example.com;PORT=5432".to_string(),
        );

        let result = build_connection_string(&params, 5432);
        assert_eq!(
            result,
            "DRIVER={PostgreSQL};SERVER=db.example.com;PORT=5432"
        );
    }

    #[test]
    fn test_build_connection_string_components() {
        let mut params = HashMap::new();
        params.insert("driver".to_string(), "IBM DB2 ODBC DRIVER".to_string());
        params.insert("hostname".to_string(), "db2.example.com".to_string());
        params.insert("port".to_string(), "50000".to_string());
        params.insert("database".to_string(), "SAMPLE".to_string());
        params.insert("username".to_string(), "db2admin".to_string());
        params.insert("password".to_string(), "password123".to_string());

        let result = build_connection_string(&params, 5432);
        assert_eq!(
            result,
            "DRIVER={IBM DB2 ODBC DRIVER};SERVER=db2.example.com;\
             PORT=50000;DATABASE=SAMPLE;UID=db2admin;PWD=password123"
        );
    }

    #[test]
    fn test_build_connection_string_defaults() {
        let params = HashMap::new();
        let result = build_connection_string(&params, 5432);
        assert_eq!(
            result,
            "DRIVER={PostgreSQL};SERVER=localhost;PORT=5432;DATABASE=;UID=;PWD="
        );
    }

    #[test]
    fn test_dsn_priority_over_components() {
        // When DSN is provided, it should take priority over hostname/port/etc.
        let mut params = HashMap::new();
        params.insert("dsn".to_string(), "MyDSN".to_string());
        params.insert("hostname".to_string(), "ignored.example.com".to_string());
        params.insert("port".to_string(), "9999".to_string());
        params.insert("username".to_string(), "user".to_string());
        params.insert("password".to_string(), "pass".to_string());

        let result = build_connection_string(&params, 5432);
        assert!(result.starts_with("DSN=MyDSN"));
        assert!(!result.contains("ignored.example.com"));
        assert!(!result.contains("9999"));
    }

    #[test]
    fn test_connection_string_priority_over_components() {
        // When connection-string is provided (but no DSN), it takes priority
        let mut params = HashMap::new();
        params.insert(
            "connection-string".to_string(),
            "DRIVER={Custom};SERVER=custom.host".to_string(),
        );
        params.insert("hostname".to_string(), "ignored.example.com".to_string());

        let result = build_connection_string(&params, 5432);
        assert_eq!(result, "DRIVER={Custom};SERVER=custom.host");
    }

    #[test]
    fn test_redact_connection_string() {
        let conn = "DRIVER={PostgreSQL};SERVER=localhost;PWD=supersecret;UID=admin";
        let redacted = redact_connection_string(conn);
        assert!(redacted.contains("PWD=***"));
        assert!(!redacted.contains("supersecret"));
        assert!(redacted.contains("UID=admin"));
    }

    #[test]
    fn test_redact_connection_string_pwd_at_end() {
        let conn = "DRIVER={PostgreSQL};UID=admin;PWD=supersecret";
        let redacted = redact_connection_string(conn);
        assert!(redacted.contains("PWD=***"));
        assert!(!redacted.contains("supersecret"));
    }

    #[test]
    fn test_redact_no_password() {
        let conn = "DRIVER={PostgreSQL};SERVER=localhost;UID=admin";
        let redacted = redact_connection_string(conn);
        assert_eq!(redacted, conn);
    }

    #[test]
    fn test_is_sql_modifying_command() {
        // Modifying commands
        assert!(is_sql_modifying_command("INSERT INTO t VALUES (1)"));
        assert!(is_sql_modifying_command("UPDATE t SET x = 1"));
        assert!(is_sql_modifying_command("DELETE FROM t"));
        assert!(is_sql_modifying_command("DROP TABLE t"));
        assert!(is_sql_modifying_command("TRUNCATE TABLE t"));
        assert!(is_sql_modifying_command("ALTER TABLE t ADD col INT"));
        assert!(is_sql_modifying_command("CREATE TABLE t (id INT)"));
        assert!(is_sql_modifying_command("GRANT SELECT ON t TO user1"));
        assert!(is_sql_modifying_command("REVOKE SELECT ON t FROM user1"));
        assert!(is_sql_modifying_command(
            "MERGE INTO t USING s ON t.id=s.id"
        ));
        assert!(is_sql_modifying_command("EXEC sp_my_procedure"));
        assert!(is_sql_modifying_command("EXECUTE my_proc"));
        assert!(is_sql_modifying_command("CALL my_proc()"));
        assert!(is_sql_modifying_command("UPSERT INTO t VALUES (1)"));
        assert!(is_sql_modifying_command("REPLACE INTO t VALUES (1)"));
        assert!(is_sql_modifying_command("RENAME TABLE t TO t2"));

        // Case insensitive
        assert!(is_sql_modifying_command("  insert INTO t VALUES (1)"));
        assert!(is_sql_modifying_command("  Delete FROM t"));

        // Non-modifying commands
        assert!(!is_sql_modifying_command("SELECT * FROM t"));
        assert!(!is_sql_modifying_command("SHOW TABLES"));
        assert!(!is_sql_modifying_command("DESCRIBE t"));
        assert!(!is_sql_modifying_command("EXPLAIN SELECT 1"));
        assert!(!is_sql_modifying_command("USE mydb"));
        assert!(!is_sql_modifying_command("SET search_path TO public"));
    }

    #[test]
    fn test_prompt_with_database() {
        let database = "mydb";
        let read_only = false;
        let prompt = if !database.is_empty() {
            if read_only {
                format!("odbc [{}] [RO]> ", database)
            } else {
                format!("odbc [{}]> ", database)
            }
        } else {
            "odbc> ".to_string()
        };
        assert_eq!(prompt, "odbc [mydb]> ");
    }

    #[test]
    fn test_prompt_with_read_only() {
        let database = "mydb";
        let read_only = true;
        let prompt = if !database.is_empty() {
            if read_only {
                format!("odbc [{}] [RO]> ", database)
            } else {
                format!("odbc [{}]> ", database)
            }
        } else if read_only {
            "odbc [RO]> ".to_string()
        } else {
            "odbc> ".to_string()
        };
        assert_eq!(prompt, "odbc [mydb] [RO]> ");
    }

    #[test]
    fn test_prompt_without_database() {
        let database = "";
        let read_only = false;
        let prompt = if !database.is_empty() {
            format!("odbc [{}]> ", database)
        } else if read_only {
            "odbc [RO]> ".to_string()
        } else {
            "odbc> ".to_string()
        };
        assert_eq!(prompt, "odbc> ");
    }

    #[test]
    fn test_prompt_read_only_without_database() {
        let database = "";
        let read_only = true;
        let prompt = if !database.is_empty() {
            if read_only {
                format!("odbc [{}] [RO]> ", database)
            } else {
                format!("odbc [{}]> ", database)
            }
        } else if read_only {
            "odbc [RO]> ".to_string()
        } else {
            "odbc> ".to_string()
        };
        assert_eq!(prompt, "odbc [RO]> ");
    }

    #[test]
    fn test_build_connection_string_dsn_empty_credentials() {
        let mut params = HashMap::new();
        params.insert("dsn".to_string(), "MyDSN".to_string());

        let result = build_connection_string(&params, 5432);
        assert_eq!(result, "DSN=MyDSN;UID=;PWD=");
    }

    #[test]
    fn test_build_connection_string_sap_hana() {
        let mut params = HashMap::new();
        params.insert("driver".to_string(), "HDBODBC".to_string());
        params.insert("hostname".to_string(), "hana.example.com".to_string());
        params.insert("port".to_string(), "30015".to_string());
        params.insert("database".to_string(), "HDB".to_string());
        params.insert("username".to_string(), "SYSTEM".to_string());
        params.insert("password".to_string(), "Manager1".to_string());

        let result = build_connection_string(&params, 5432);
        assert!(result.contains("DRIVER={HDBODBC}"));
        assert!(result.contains("SERVER=hana.example.com"));
        assert!(result.contains("PORT=30015"));
        assert!(result.contains("DATABASE=HDB"));
    }

    #[test]
    fn test_build_connection_string_teradata() {
        let mut params = HashMap::new();
        params.insert(
            "driver".to_string(),
            "Teradata Database ODBC Driver 17.20".to_string(),
        );
        params.insert("hostname".to_string(), "td.example.com".to_string());
        params.insert("port".to_string(), "1025".to_string());
        params.insert("database".to_string(), "DBC".to_string());
        params.insert("username".to_string(), "dbc".to_string());
        params.insert("password".to_string(), "dbc".to_string());

        let result = build_connection_string(&params, 5432);
        assert!(result.contains("DRIVER={Teradata Database ODBC Driver 17.20}"));
        assert!(result.contains("SERVER=td.example.com"));
        assert!(result.contains("PORT=1025"));
    }

    #[test]
    fn test_build_connection_string_db2() {
        let mut params = HashMap::new();
        params.insert("driver".to_string(), "IBM DB2 ODBC DRIVER".to_string());
        params.insert("hostname".to_string(), "db2.example.com".to_string());
        params.insert("port".to_string(), "50000".to_string());
        params.insert("database".to_string(), "SAMPLE".to_string());
        params.insert("username".to_string(), "db2inst1".to_string());
        params.insert("password".to_string(), "ibmdb2".to_string());

        let result = build_connection_string(&params, 5432);
        assert!(result.contains("DRIVER={IBM DB2 ODBC DRIVER}"));
        assert!(result.contains("PORT=50000"));
        assert!(result.contains("DATABASE=SAMPLE"));
    }
}
