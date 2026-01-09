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
static STREAM_INDEX: AtomicI32 = AtomicI32::new(5000);

/// MongoDB handler
///
/// Provides interactive MongoDB shell access for NoSQL operations.
pub struct MongoDbHandler {
    config: MongoDbConfig,
}

#[derive(Debug, Clone)]
pub struct MongoDbConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub connection_timeout_secs: u64,
}

impl Default for MongoDbConfig {
    fn default() -> Self {
        Self {
            default_port: 27017,
            require_tls: false, // TLS configurable
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl MongoDbHandler {
    pub fn new(config: MongoDbConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(MongoDbConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for MongoDbHandler {
    fn name(&self) -> &str {
        "mongodb"
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
        info!("MongoDB handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("MongoDB: Read-only mode enabled");
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
            .unwrap_or("admin");
        let auth_source = params
            .get("authSource")
            .map(|s| s.as_str())
            .unwrap_or("admin");

        info!(
            "MongoDB: Connecting to {}@{}:{}/{}",
            username, hostname, port, database
        );

        // Create query executor with MongoDB prompt
        let prompt = if security.base.read_only {
            "[RO]> "
        } else {
            "> "
        };
        let mut executor = QueryExecutor::new(prompt, "mongodb")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Get terminal dimensions for recording
        let (rows, cols) = executor.terminal.size();

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "MongoDB", cols, rows);

        // Send initial screen
        executor
            .terminal
            .write_line("Connecting to MongoDB server...")
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

        // Build MongoDB connection URI with proper URL encoding for special characters
        // This is critical for passwords containing: | & ? @ : / # %
        let encoded_username = urlencoding::encode(username);
        let encoded_password = urlencoding::encode(password);
        let encoded_database = urlencoding::encode(database);
        let encoded_auth_source = urlencoding::encode(auth_source);
        let tls_param = if self.config.require_tls {
            "&tls=true"
        } else {
            ""
        };
        let connection_uri = format!(
            "mongodb://{}:{}@{}:{}/{}?authSource={}{}",
            encoded_username,
            encoded_password,
            hostname,
            port,
            encoded_database,
            encoded_auth_source,
            tls_param
        );

        debug!(
            "MongoDB: Connection URI: {}",
            connection_uri.replace(&encoded_password.to_string(), "***")
        );

        // Connect to MongoDB
        use mongodb::{options::ClientOptions, Client};

        let client_options = match ClientOptions::parse(&connection_uri).await {
            Ok(mut opts) => {
                opts.connect_timeout = Some(std::time::Duration::from_secs(
                    self.config.connection_timeout_secs,
                ));
                opts.app_name = Some("guacr-mongodb".to_string());
                opts
            }
            Err(e) => {
                let error_msg = format!("Failed to parse MongoDB URI: {}", e);
                warn!("MongoDB: {}", error_msg);

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

        let client = match Client::with_options(client_options) {
            Ok(client) => client,
            Err(e) => {
                let error_msg = format!("Failed to create MongoDB client: {}", e);
                warn!("MongoDB: {}", error_msg);

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

        // Test connection by pinging the database
        let db = client.database(database);
        match db.run_command(mongodb::bson::doc! { "ping": 1 }).await {
            Ok(_) => {
                info!("MongoDB: Connected successfully");

                executor
                    .terminal
                    .write_line("")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line(&format!("Connected to MongoDB at {}:{}", hostname, port))
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
            }
            Err(e) => {
                let error_msg = format!("MongoDB connection test failed: {}", e);
                warn!("MongoDB: {}", error_msg);

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
                    .write_line("  2. Verify MongoDB server is running")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  3. Check credentials and authSource")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                executor
                    .terminal
                    .write_line("  4. Verify network connectivity")
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
        }

        // Send updated screen
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

        // Track current database
        let mut current_db = database.to_string();

        // Event loop
        while let Some(msg) = from_client.recv().await {
            match executor.process_input(&msg).await {
                Ok((needs_render, instructions, pending_query)) => {
                    if let Some(command) = pending_query {
                        info!("MongoDB: Executing command: {}", command);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &command);

                        // Handle built-in commands
                        if let Some(new_db) = handle_builtin_command(
                            &command,
                            &mut executor,
                            &to_client,
                            &client,
                            &current_db,
                            &security,
                        )
                        .await?
                        {
                            current_db = new_db;
                            continue;
                        }

                        // Check for export command: \e <collection>
                        if command.to_lowercase().starts_with("\\e ") {
                            let collection = command[3..].trim();
                            handle_csv_export(
                                collection,
                                &client,
                                &current_db,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Check for import command: \i <collection>
                        if command.to_lowercase().starts_with("\\i ") {
                            let collection = command[3..].trim();
                            handle_csv_import(
                                collection,
                                &client,
                                &current_db,
                                &mut executor,
                                &to_client,
                                &security,
                            )
                            .await?;
                            continue;
                        }

                        // Check read-only mode for modifying commands
                        if security.base.read_only && is_mongodb_modifying_command(&command) {
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

                        // Execute MongoDB command
                        let db = client.database(&current_db);
                        match execute_mongodb_command(&db, &command).await {
                            Ok(result) => {
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
                    warn!("MongoDB: Input processing error: {}", e);
                }
            }
        }

        // Finalize recording
        finalize_recording(recorder, "MongoDB");

        info!("MongoDB handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Execute MongoDB command and return result as string
async fn execute_mongodb_command(db: &mongodb::Database, command: &str) -> Result<String, String> {
    use mongodb::bson::doc;

    // Parse simple commands
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Ok("".to_string());
    }

    let cmd = parts[0].to_lowercase();

    match cmd.as_str() {
        "show" if parts.len() > 1 => match parts[1].to_lowercase().as_str() {
            "collections" | "tables" => {
                let collections = db
                    .list_collection_names()
                    .await
                    .map_err(|e| format!("Failed to list collections: {}", e))?;

                if collections.is_empty() {
                    Ok("No collections found".to_string())
                } else {
                    Ok(collections.join("\n"))
                }
            }
            "dbs" | "databases" => {
                let client = db.client();
                let dbs = client
                    .list_database_names()
                    .await
                    .map_err(|e| format!("Failed to list databases: {}", e))?;
                Ok(dbs.join("\n"))
            }
            _ => Err(format!("Unknown show command: {}", parts[1])),
        },
        "db" => Ok(format!("Current database: {}", db.name())),
        "stats" => {
            let result = db
                .run_command(doc! { "dbStats": 1 })
                .await
                .map_err(|e| format!("Failed to get stats: {}", e))?;
            Ok(format_bson_document(&result, 0))
        }
        _ => {
            // Try to parse as JSON and run as command
            if command.starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(command) {
                    Ok(json) => {
                        let bson_doc = mongodb::bson::to_document(&json)
                            .map_err(|e| format!("Failed to convert to BSON: {}", e))?;
                        let result = db
                            .run_command(bson_doc)
                            .await
                            .map_err(|e| format!("Command error: {}", e))?;
                        Ok(format_bson_document(&result, 0))
                    }
                    Err(e) => Err(format!("Invalid JSON: {}", e)),
                }
            } else {
                Err(format!(
                    "Unknown command: {}. Try 'help' for available commands.",
                    cmd
                ))
            }
        }
    }
}

/// Format BSON document for display
fn format_bson_document(doc: &mongodb::bson::Document, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);
    let mut lines = Vec::new();
    lines.push(format!("{}{{", indent_str));

    for (key, value) in doc.iter() {
        let formatted_value = format_bson_value(value, indent + 1);
        lines.push(format!("{}  \"{}\": {}", indent_str, key, formatted_value));
    }

    lines.push(format!("{}}}", indent_str));
    lines.join("\n")
}

/// Format BSON value for display
fn format_bson_value(value: &mongodb::bson::Bson, indent: usize) -> String {
    use mongodb::bson::Bson;

    match value {
        Bson::Null => "null".to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => format!("{:.6}", d),
        Bson::String(s) => format!("\"{}\"", s),
        Bson::Array(arr) => {
            if arr.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = arr.iter().map(|v| format_bson_value(v, indent)).collect();
                format!("[{}]", items.join(", "))
            }
        }
        Bson::Document(doc) => format_bson_document(doc, indent),
        Bson::ObjectId(id) => format!("ObjectId(\"{}\")", id),
        Bson::DateTime(dt) => format!("ISODate(\"{}\")", dt),
        Bson::Binary(bin) => format!("Binary({} bytes)", bin.bytes.len()),
        Bson::Timestamp(ts) => format!("Timestamp({}, {})", ts.time, ts.increment),
        Bson::RegularExpression(regex) => format!("/{}/{}", regex.pattern, regex.options),
        Bson::Decimal128(d) => d.to_string(),
        _ => format!("{:?}", value),
    }
}

/// Check if a MongoDB command modifies data
fn is_mongodb_modifying_command(command: &str) -> bool {
    let cmd = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    // Check for JSON commands with modifying operations
    if command.starts_with('{') {
        let command_lower = command.to_lowercase();
        return command_lower.contains("\"insert\"")
            || command_lower.contains("\"update\"")
            || command_lower.contains("\"delete\"")
            || command_lower.contains("\"drop\"")
            || command_lower.contains("\"create\"")
            || command_lower.contains("\"renamecollection\"");
    }

    matches!(
        cmd.as_str(),
        "insert" | "update" | "delete" | "drop" | "create" | "rename"
    )
}

/// Handle built-in commands. Returns Some(new_db) if database changed.
async fn handle_builtin_command(
    command: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    _client: &mongodb::Client,
    current_db: &str,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<Option<String>> {
    let command_lower = command.to_lowercase();
    let parts: Vec<&str> = command.split_whitespace().collect();

    // Check for "use <database>" command
    if parts.len() >= 2 && parts[0].to_lowercase() == "use" {
        let new_db = parts[1].to_string();
        executor
            .terminal
            .write_line(&format!("switched to db {}", new_db))
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
        return Ok(Some(new_db));
    }

    match command_lower.as_str() {
        "help" => {
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("MongoDB Shell - Available commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("Database commands:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  show dbs             List databases")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  use <database>       Switch database")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  db                   Show current database")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  show collections     List collections")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  stats                Database statistics")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            if !security.disable_csv_export {
                executor
                    .terminal
                    .write_line("  \\e <collection>      Export collection as CSV")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            if !security.disable_csv_import && !security.base.read_only {
                executor
                    .terminal
                    .write_line("  \\i <collection>      Import CSV into collection")
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            executor
                .terminal
                .write_line("")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("You can also run raw MongoDB commands as JSON:")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            executor
                .terminal
                .write_line("  {\"find\": \"collection\", \"limit\": 10}")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
            return Ok(Some(current_db.to_string())); // Keep current db
        }
        "quit" | "exit" => {
            executor
                .terminal
                .write_line("bye")
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

    Ok(None)
}

/// Handle CSV export for MongoDB collection
async fn handle_csv_export(
    collection_name: &str,
    client: &mongodb::Client,
    current_db: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use mongodb::bson::doc;
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

    if collection_name.is_empty() {
        executor
            .terminal
            .write_error("Usage: \\e <collection_name>")
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
        .write_line(&format!("Exporting collection '{}'...", collection_name))
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

    // Get documents from collection
    let db = client.database(current_db);
    let collection = db.collection::<mongodb::bson::Document>(collection_name);

    use futures_util::StreamExt;
    let mut cursor = collection
        .find(doc! {})
        .await
        .map_err(|e| HandlerError::ProtocolError(format!("Find error: {}", e)))?;

    // Collect all documents and determine columns
    let mut documents = Vec::new();
    let mut all_keys = std::collections::HashSet::new();

    while let Some(result) = cursor.next().await {
        match result {
            Ok(doc) => {
                for key in doc.keys() {
                    all_keys.insert(key.clone());
                }
                documents.push(doc);
            }
            Err(e) => {
                warn!("Error reading document: {}", e);
            }
        }
    }

    if documents.is_empty() {
        executor
            .terminal
            .write_line("Collection is empty or not found.")
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

    // Build columns list (sorted for consistency)
    let mut columns: Vec<String> = all_keys.into_iter().collect();
    columns.sort();

    // Build rows
    let mut rows = Vec::new();
    for doc in &documents {
        let mut row = Vec::new();
        for col in &columns {
            let value = doc
                .get(col)
                .map(|v| format_bson_value(v, 0))
                .unwrap_or_default();
            row.push(value);
        }
        rows.push(row);
    }

    let result = guacr_terminal::QueryResult {
        columns,
        rows,
        affected_rows: None,
        execution_time_ms: None,
    };

    // Generate filename and create exporter
    let filename = generate_csv_filename(&format!("collection {}", collection_name), "mongodb");
    let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
    let mut exporter = CsvExporter::new(stream_idx);

    executor
        .terminal
        .write_line(&format!(
            "Beginning CSV download ({} documents). Press Ctrl+C to cancel.",
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

/// Handle CSV import for a collection
async fn handle_csv_import(
    collection_name: &str,
    client: &mongodb::Client,
    database: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
) -> guacr_handlers::Result<()> {
    use mongodb::bson::Document;

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

    if collection_name.is_empty() {
        executor
            .terminal
            .write_error("Usage: \\i <collection_name>")
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
        .write_line(&format!("Import into collection: {}", collection_name))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Demo: Importing sample data...")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Demo data - for MongoDB we convert CSV rows to BSON documents
    let sample_csv = "name,age,city\nAlice,30,NYC\nBob,25,LA";
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
            "Parsed {} columns, {} rows",
            csv_data.headers.len(),
            csv_data.row_count()
        ))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    // Convert rows to BSON documents
    let db = client.database(database);
    let collection = db.collection::<Document>(collection_name);

    let mut documents: Vec<Document> = Vec::new();
    for row in &csv_data.rows {
        let mut doc = Document::new();
        for (i, header) in csv_data.headers.iter().enumerate() {
            if let Some(value) = row.get(i) {
                // Try to parse as number, otherwise use string
                if let Ok(num) = value.parse::<i64>() {
                    doc.insert(header.clone(), num);
                } else if let Ok(num) = value.parse::<f64>() {
                    doc.insert(header.clone(), num);
                } else {
                    doc.insert(header.clone(), value.clone());
                }
            }
        }
        documents.push(doc);
    }

    // Insert documents
    match collection.insert_many(documents.clone()).await {
        Ok(result) => {
            executor
                .terminal
                .write_success(&format!(
                    "Import complete: {} documents inserted",
                    result.inserted_ids.len()
                ))
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
        Err(e) => {
            executor
                .terminal
                .write_error(&format!("Import failed: {}", e))
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
impl EventBasedHandler for MongoDbHandler {
    fn name(&self) -> &str {
        "mongodb"
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
    fn test_mongodb_handler_new() {
        let handler = MongoDbHandler::with_defaults();
        assert_eq!(
            <MongoDbHandler as ProtocolHandler>::name(&handler),
            "mongodb"
        );
    }

    #[test]
    fn test_mongodb_config() {
        let config = MongoDbConfig::default();
        assert_eq!(config.default_port, 27017);
    }
}
