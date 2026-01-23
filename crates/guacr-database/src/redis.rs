use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, ProtocolHandler,
    RecordingConfig,
};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::csv_export::{generate_csv_filename, CsvExporter};
use crate::query_executor::QueryExecutor;
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
};
use crate::security::DatabaseSecuritySettings;

use std::sync::atomic::AtomicI32;

/// Global stream index counter for unique stream IDs
static STREAM_INDEX: AtomicI32 = AtomicI32::new(4000);

/// Redis handler
///
/// Provides interactive Redis CLI access for key-value operations.
pub struct RedisHandler {
    config: RedisConfig,
}

#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub require_auth: bool,
    pub connection_timeout_secs: u64,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            default_port: 6379,
            require_tls: false, // Redis typically doesn't use TLS by default
            require_auth: true, // Security: Password required
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl RedisHandler {
    pub fn new(config: RedisConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(RedisConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for RedisHandler {
    fn name(&self) -> &str {
        "redis"
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
        info!("Redis handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("Redis: Read-only mode enabled");
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
        let password = params.get("password");
        let database: u8 = params
            .get("database")
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);

        // Security check
        if self.config.require_auth && password.is_none() {
            return Err(HandlerError::MissingParameter(
                "password (required for security)".to_string(),
            ));
        }

        info!("Redis: Connecting to {}:{} db={}", hostname, port, database);

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
            "Redis: Display size {}x{} px → {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with Redis prompt and correct dimensions
        let prompt = if security.base.read_only {
            "redis [RO]> "
        } else {
            "redis> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "redis", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "Redis", cols, rows);

        // Send display initialization instructions (ready + size)
        let (ready_instr, size_instr) =
            QueryExecutor::create_display_init_instructions(width, height);
        to_client
            .send(ready_instr)
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        to_client
            .send(size_instr)
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        debug!("Redis: Sent ready and size instructions");

        // NOTE: Don't render initial screen yet - wait until after connection
        // This matches SSH behavior and prevents rendering at wrong dimensions

        // Build Redis connection URL with proper URL encoding for special characters
        // This is critical for passwords containing: | & ? @ : / # %
        let connection_url = if let Some(pwd) = password {
            let encoded_password = urlencoding::encode(pwd);
            if self.config.require_tls {
                format!(
                    "rediss://:{}@{}:{}/{}",
                    encoded_password, hostname, port, database
                )
            } else {
                format!(
                    "redis://:{}@{}:{}/{}",
                    encoded_password, hostname, port, database
                )
            }
        } else if self.config.require_tls {
            format!("rediss://{}:{}/{}", hostname, port, database)
        } else {
            format!("redis://{}:{}/{}", hostname, port, database)
        };

        debug!(
            "Redis: Connection URL: {}",
            connection_url.replace(
                &password
                    .map(|p| urlencoding::encode(p).to_string())
                    .unwrap_or_default(),
                "***"
            )
        );

        // Connect to Redis
        let client = match redis::Client::open(connection_url.clone()) {
            Ok(client) => client,
            Err(e) => {
                let error_msg = format!("Failed to create Redis client: {}", e);
                warn!("Redis: {}", error_msg);

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

        let mut con = match client.get_multiplexed_async_connection().await {
            Ok(con) => {
                info!("Redis: Connected successfully");

                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Connected to Redis at {}:{}", hostname, port))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Database: {}", database))
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

                // Render the connection success screen
                debug!("Redis: Rendering initial screen with prompt");
                let (_, instructions) = executor
                    .render_screen()
                    .await
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                debug!(
                    "Redis: Sending {} instructions to client",
                    instructions.len()
                );
                for instr in instructions {
                    to_client
                        .send(instr)
                        .await
                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                }
                debug!("Redis: Initial screen sent successfully");

                con
            }
            Err(e) => {
                let error_msg = format!("Redis connection failed: {}", e);
                warn!("Redis: {}", error_msg);

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
                    .write_line("  2. Verify Redis server is running")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  3. Check password if AUTH is required")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  4. Check firewall rules")
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
                        debug!("Redis: Client disconnected, stopping debounce timer");
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
                                debug!("Redis: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                Some(msg) = from_client.recv() => {
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(command) = pending_query {
                        info!("Redis: Executing command: {}", command);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &command);

                        // Handle built-in commands
                        if handle_builtin_command(&command, &mut executor, &to_client, &security)
                            .await?
                        {
                            continue;
                        }

                        // Check for export command: \e <pattern>
                        if command.to_lowercase().starts_with("\\e ") {
                            let pattern = command[3..].trim();
                            handle_csv_export(
                                pattern,
                                &mut con,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Check for import command: \i (imports key-value pairs from CSV)
                        if command.to_lowercase().starts_with("\\i") {
                            handle_csv_import(&mut con, &mut executor, &to_client, &security)
                                .await?;
                            continue;
                        }

                        // Check read-only mode for modifying commands
                        if security.base.read_only && is_redis_modifying_command(&command) {
                            executor
                                .terminal
                                .write_error("Command blocked: read-only mode is enabled.")
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
                                to_client
                                    .send(instr)
                                    .await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }
                            continue;
                        }

                        // Execute Redis command
                        match execute_redis_command(&mut con, &command).await {
                            Ok(result) => {
                                // Write result line by line
                                for line in result.lines() {
                                    executor
                                        .terminal
                                        .write_line(line)
                                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                }
                            }
                            Err(e) => {
                                // Record error output
                                record_error_output(&mut recorder, &e);

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
                            to_client
                                .send(instr)
                                .await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }
                        continue;
                    }

                    if needs_render {
                        for instr in instructions {
                            to_client
                                .send(instr)
                                .await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }
                    }
                }
                        Err(e) => {
                            warn!("Redis: Input processing error: {}", e);
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
        finalize_recording(recorder, "Redis");

        info!("Redis handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute Redis command and return result as string
async fn execute_redis_command(
    con: &mut redis::aio::MultiplexedConnection,
    command: &str,
) -> Result<String, String> {
    // Parse command into parts
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Ok("".to_string());
    }

    let cmd = parts[0].to_uppercase();
    let args: Vec<&str> = parts[1..].to_vec();

    // Execute command using raw redis command
    let result: redis::RedisResult<redis::Value> =
        redis::cmd(&cmd).arg(&args).query_async(con).await;

    match result {
        Ok(value) => Ok(format_redis_value(&value)),
        Err(e) => Err(format!("(error) {}", e)),
    }
}

/// Format Redis value for display
fn format_redis_value(value: &redis::Value) -> String {
    match value {
        redis::Value::Nil => "(nil)".to_string(),
        redis::Value::Int(i) => format!("(integer) {}", i),
        redis::Value::BulkString(data) => match String::from_utf8(data.clone()) {
            Ok(s) => format!("\"{}\"", s),
            Err(_) => format!("(binary) {} bytes", data.len()),
        },
        redis::Value::Array(arr) => {
            if arr.is_empty() {
                "(empty array)".to_string()
            } else {
                let mut lines = Vec::new();
                for (i, item) in arr.iter().enumerate() {
                    let formatted = format_redis_value(item);
                    lines.push(format!("{}) {}", i + 1, formatted));
                }
                lines.join("\n")
            }
        }
        redis::Value::SimpleString(s) => s.clone(),
        redis::Value::Okay => "OK".to_string(),
        redis::Value::Map(map) => {
            let mut lines = Vec::new();
            for (i, (key, val)) in map.iter().enumerate() {
                lines.push(format!(
                    "{}) {} -> {}",
                    i + 1,
                    format_redis_value(key),
                    format_redis_value(val)
                ));
            }
            lines.join("\n")
        }
        redis::Value::Attribute {
            data,
            attributes: _,
        } => format_redis_value(data),
        redis::Value::Set(set) => {
            let mut lines = Vec::new();
            for (i, item) in set.iter().enumerate() {
                lines.push(format!("{}) {}", i + 1, format_redis_value(item)));
            }
            lines.join("\n")
        }
        redis::Value::Double(d) => format!("(double) {}", d),
        redis::Value::Boolean(b) => format!("(boolean) {}", b),
        redis::Value::VerbatimString { format: _, text } => format!("\"{}\"", text),
        redis::Value::BigNumber(n) => format!("(bignumber) {}", n),
        redis::Value::Push { kind, data } => {
            format!(
                "(push:{}) {}",
                kind,
                data.iter()
                    .map(format_redis_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        redis::Value::ServerError(err) => format!("(error) {}", err.details().unwrap_or("unknown")),
    }
}

/// Check if a Redis command modifies data
fn is_redis_modifying_command(command: &str) -> bool {
    let cmd = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase();
    matches!(
        cmd.as_str(),
        "SET"
            | "SETNX"
            | "SETEX"
            | "PSETEX"
            | "MSET"
            | "MSETNX"
            | "DEL"
            | "UNLINK"
            | "EXPIRE"
            | "EXPIREAT"
            | "PEXPIRE"
            | "PEXPIREAT"
            | "INCR"
            | "INCRBY"
            | "INCRBYFLOAT"
            | "DECR"
            | "DECRBY"
            | "APPEND"
            | "SETRANGE"
            | "GETSET"
            | "GETDEL"
            | "GETEX"
            | "LPUSH"
            | "RPUSH"
            | "LPOP"
            | "RPOP"
            | "LSET"
            | "LINSERT"
            | "LREM"
            | "LTRIM"
            | "SADD"
            | "SREM"
            | "SPOP"
            | "SMOVE"
            | "ZADD"
            | "ZREM"
            | "ZINCRBY"
            | "ZPOPMIN"
            | "ZPOPMAX"
            | "HSET"
            | "HSETNX"
            | "HMSET"
            | "HDEL"
            | "HINCRBY"
            | "HINCRBYFLOAT"
            | "PFADD"
            | "PFMERGE"
            | "XADD"
            | "XDEL"
            | "XTRIM"
            | "RENAME"
            | "RENAMENX"
            | "COPY"
            | "MOVE"
            | "FLUSHDB"
            | "FLUSHALL"
            | "SAVE"
            | "BGSAVE"
            | "BGREWRITEAOF"
            | "CONFIG"
            | "DEBUG"
            | "SHUTDOWN"
            | "SLAVEOF"
            | "REPLICAOF"
    )
}

/// Handle built-in commands
async fn handle_builtin_command(
    command: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<bool> {
    let command_lower = command.to_lowercase();

    match command_lower.as_str() {
        "help" | "?" => {
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Redis CLI - Available commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("String commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  GET key              Get value")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  SET key value        Set value")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  DEL key              Delete key")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Key commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  KEYS pattern         List keys")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  TYPE key             Get type")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  TTL key              Get TTL")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Server commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  INFO                 Server info")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  DBSIZE               Key count")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  PING                 Test connection")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Export/Import:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            if !security.disable_csv_export {
                executor
                    .terminal
                    .write_line("  \\e <pattern>         Export keys as CSV")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            if !security.disable_csv_import && !security.base.read_only {
                executor
                    .terminal
                    .write_line("  \\i                   Import key-value pairs from CSV")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Type 'quit' to disconnect")
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
        "quit" | "exit" => {
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

/// Handle CSV export for Redis keys matching a pattern
async fn handle_csv_export(
    pattern: &str,
    connection: &mut redis::aio::MultiplexedConnection,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use redis::AsyncCommands;
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

    let scan_pattern = if pattern.is_empty() { "*" } else { pattern };

    executor
        .terminal
        .write_line(&format!("Scanning keys matching '{}'...", scan_pattern))
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

    // Get keys matching pattern
    let keys: Vec<String> = connection
        .keys(scan_pattern)
        .await
        .map_err(|e| HandlerError::ProtocolError(format!("KEYS error: {}", e)))?;

    if keys.is_empty() {
        executor
            .terminal
            .write_line("No keys found matching pattern.")
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

    // Build result with key-value pairs
    let mut rows = Vec::new();
    for key in &keys {
        let value: Result<String, _> = connection.get(key).await;
        let value_str = match value {
            Ok(v) => v,
            Err(_) => {
                // Try other types
                let type_result: Result<String, _> =
                    redis::cmd("TYPE").arg(key).query_async(connection).await;
                match type_result {
                    Ok(t) => format!("<{}>", t),
                    Err(_) => "<error>".to_string(),
                }
            }
        };
        rows.push(vec![key.clone(), value_str]);
    }

    let result = guacr_terminal::QueryResult {
        columns: vec!["key".to_string(), "value".to_string()],
        rows,
        affected_rows: None,
        execution_time_ms: None,
    };

    // Generate filename and create exporter
    let filename = generate_csv_filename(&format!("KEYS {}", scan_pattern), "redis");
    let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
    let mut exporter = CsvExporter::new(stream_idx);

    executor
        .terminal
        .write_line(&format!(
            "Beginning CSV download ({} keys). Press Ctrl+C to cancel.",
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

/// Handle CSV import for Redis (imports key-value pairs)
async fn handle_csv_import(
    con: &mut redis::aio::MultiplexedConnection,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use redis::AsyncCommands;

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

    executor
        .terminal
        .write_line("Redis CSV Import")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Demo: Importing sample key-value data...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Demo data - for Redis we import key,value pairs
    let sample_csv = "key,value\nuser:1,Alice\nuser:2,Bob\ncounter,42";
    let mut importer = crate::csv_import::CsvImporter::new(1);

    importer
        .receive_blob(sample_csv.as_bytes())
        .map_err(HandlerError::ProtocolError)?;

    let csv_data = importer
        .finish_receive()
        .map_err(HandlerError::ProtocolError)?;

    // Validate CSV has key and value columns
    let key_idx = csv_data
        .headers
        .iter()
        .position(|h| h.to_lowercase() == "key");
    let value_idx = csv_data
        .headers
        .iter()
        .position(|h| h.to_lowercase() == "value");

    if key_idx.is_none() || value_idx.is_none() {
        executor
            .terminal
            .write_error("CSV must have 'key' and 'value' columns")
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

    let key_idx = key_idx.unwrap();
    let value_idx = value_idx.unwrap();

    executor
        .terminal
        .write_line(&format!("Parsed {} key-value pairs", csv_data.row_count()))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    let mut success_count = 0;
    let mut error_count = 0;

    for row in &csv_data.rows {
        if let (Some(key), Some(value)) = (row.get(key_idx), row.get(value_idx)) {
            let result: redis::RedisResult<()> = con.set(key, value).await;
            match result {
                Ok(_) => success_count += 1,
                Err(e) => {
                    error_count += 1;
                    warn!("Redis import error: {}", e);
                }
            }
        }
    }

    executor
        .terminal
        .write_success(&format!(
            "Import complete: {} keys set, {} errors",
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
impl EventBasedHandler for RedisHandler {
    fn name(&self) -> &str {
        "redis"
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
    fn test_redis_handler_new() {
        let handler = RedisHandler::with_defaults();
        assert_eq!(<RedisHandler as ProtocolHandler>::name(&handler), "redis");
    }

    #[test]
    fn test_redis_config() {
        let config = RedisConfig::default();
        assert_eq!(config.default_port, 6379);
        assert!(config.require_auth);
    }

    #[test]
    fn test_format_redis_value() {
        assert_eq!(format_redis_value(&redis::Value::Nil), "(nil)");
        assert_eq!(format_redis_value(&redis::Value::Int(42)), "(integer) 42");
        assert_eq!(format_redis_value(&redis::Value::Okay), "OK");
    }
}
