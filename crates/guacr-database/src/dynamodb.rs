use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client;
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

/// DynamoDB handler
///
/// Provides interactive DynamoDB CLI access via PartiQL queries and
/// DynamoDB-specific commands. Supports both DynamoDB Local (development)
/// and AWS-hosted DynamoDB endpoints.
pub struct DynamoDbHandler {
    config: DynamoDbConfig,
}

#[derive(Debug, Clone)]
pub struct DynamoDbConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub connection_timeout_secs: u64,
}

impl Default for DynamoDbConfig {
    fn default() -> Self {
        Self {
            default_port: 8000,
            require_tls: false, // DynamoDB Local uses HTTP; AWS uses HTTPS automatically
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl DynamoDbHandler {
    pub fn new(config: DynamoDbConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(DynamoDbConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for DynamoDbHandler {
    fn name(&self) -> &str {
        "dynamodb"
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
        info!("DynamoDB handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("DynamoDB: Read-only mode enabled");
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
        let region = params
            .get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string());
        let access_key = params.get("access-key").or_else(|| params.get("username"));
        let secret_key = params.get("secret-key").or_else(|| params.get("password"));
        let endpoint_url = params.get("endpoint-url").cloned();

        info!(
            "DynamoDB: Connecting to {}:{} region={}",
            hostname, port, region
        );

        // Parse display size from parameters
        let (width, height, cols, rows) = parse_display_size(&params);

        info!(
            "DynamoDB: Display size {}x{} px -> {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with DynamoDB prompt and correct dimensions
        let prompt = if security.base.read_only {
            "dynamodb [RO]> "
        } else {
            "dynamodb> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "dynamodb", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "DynamoDB", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("DynamoDB: Sent display init instructions");

        // Build the DynamoDB client
        let client = match build_dynamodb_client(
            hostname,
            port,
            &region,
            access_key.map(|s| s.as_str()),
            secret_key.map(|s| s.as_str()),
            endpoint_url.as_deref(),
        )
        .await
        {
            Ok(client) => client,
            Err(e) => {
                let error_msg = format!("Failed to create DynamoDB client: {}", e);
                warn!("DynamoDB: {}", error_msg);

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

        // Verify connectivity by listing tables
        match client.list_tables().limit(1).send().await {
            Ok(_) => {
                info!("DynamoDB: Connected successfully");

                let default_endpoint = format!("https://dynamodb.{}.amazonaws.com", region);
                let endpoint_display = endpoint_url.as_deref().unwrap_or(&default_endpoint);

                let conn_line = format!("Connected to DynamoDB at {}", endpoint_display);
                let region_line = format!("Region: {}", region);
                render_connection_success(
                    &mut executor,
                    &to_client,
                    &[
                        &conn_line,
                        &region_line,
                        "Type 'help' for available commands.",
                    ],
                    &mut recorder,
                )
                .await?;
                debug!("DynamoDB: Initial screen sent successfully");
            }
            Err(e) => {
                let error_msg = format!("DynamoDB connection failed: {}", e);
                warn!("DynamoDB: {}", error_msg);

                render_connection_error(
                    &mut executor,
                    &to_client,
                    &mut from_client,
                    &error_msg,
                    &[
                        "Check endpoint URL or hostname and port",
                        "Verify AWS credentials (access key / secret key)",
                        "Check region setting",
                        "For DynamoDB Local, ensure the service is running",
                    ],
                    &mut recorder,
                )
                .await?;
                return Err(HandlerError::ConnectionFailed(error_msg));
            }
        }

        // Event loop
        // Debounce timer for batching screen updates (60 FPS)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        'outer: loop {
            tokio::select! {
                // Debounce tick - render if terminal changed
                _ = debounce.tick() => {
                    if to_client.is_closed() {
                        debug!("DynamoDB: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            if crate::recording::send_and_record(&to_client, &mut recorder, instr).await.is_err() {
                                debug!("DynamoDB: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("DynamoDB: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(command) = pending_query {
                        info!("DynamoDB: Executing command: {}", command);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &command);

                        // Handle built-in commands
                        if handle_builtin_command(&command, &mut executor, &to_client, &security, &mut recorder)
                            .await?
                        {
                            continue;
                        }

                        // Check for export command: \e <query>
                        if command.to_lowercase().starts_with("\\e ") {
                            let query = command[3..].trim();
                            handle_csv_export(
                                query,
                                &client,
                                &mut executor,
                                &to_client,
                                &security,
                                &mut recorder,
                            )
                            .await?;
                            continue;
                        }

                        // Handle DynamoDB-specific commands
                        if let Some(result) = handle_dynamodb_command(
                            &command,
                            &client,
                            &mut executor,
                            &to_client,
                            &security,
                            &mut recorder,
                        )
                        .await?
                        {
                            if result {
                                continue;
                            }
                        }

                        // Check read-only mode for modifying statements
                        if security.base.read_only && is_dynamodb_modifying_statement(&command) {
                            executor
                                .terminal
                                .write_error("Statement blocked: read-only mode is enabled.")
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            executor
                                .terminal
                                .write_prompt()
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            send_render(&mut executor, &to_client, &mut recorder).await?;
                            continue;
                        }

                        // Execute PartiQL statement
                        match execute_partiql(&client, &command).await {
                            Ok(result) => {
                                for line in result.lines() {
                                    executor
                                        .terminal
                                        .write_line(line)
                                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                }
                            }
                            Err(e) => {
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
                            warn!("DynamoDB: Input processing error: {}", e);
                        }
                    }
                }

                else => {
                    break;
                }
            }
        }

        // Finalize recording
        finalize_recording(recorder, "DynamoDB");

        send_disconnect(&to_client).await;
        info!("DynamoDB handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Build a DynamoDB client from connection parameters.
///
/// Supports both DynamoDB Local (via explicit endpoint URL) and
/// AWS-hosted DynamoDB (via region and credentials).
async fn build_dynamodb_client(
    hostname: &str,
    port: u16,
    region: &str,
    access_key: Option<&str>,
    secret_key: Option<&str>,
    endpoint_url: Option<&str>,
) -> Result<Client, String> {
    use aws_config::BehaviorVersion;
    use aws_sdk_dynamodb::config::Credentials;
    use aws_types::region::Region;

    let resolved_endpoint = if let Some(url) = endpoint_url {
        Some(url.to_string())
    } else if hostname == "localhost"
        || hostname == "127.0.0.1"
        || hostname.starts_with("192.168.")
        || hostname.starts_with("10.")
        || hostname.starts_with("172.")
    {
        Some(format!("http://{}:{}", hostname, port))
    } else if port != 443 && port != 8000 {
        Some(format!("https://{}:{}", hostname, port))
    } else if port == 8000 {
        Some(format!("http://{}:{}", hostname, port))
    } else {
        None
    };

    let credentials = if let (Some(ak), Some(sk)) = (access_key, secret_key) {
        Some(Credentials::new(ak, sk, None, None, "guacr-dynamodb"))
    } else if resolved_endpoint.is_some() {
        // For DynamoDB Local, use dummy credentials
        Some(Credentials::new(
            "fakeAccessKey",
            "fakeSecretKey",
            None,
            None,
            "guacr-dynamodb-local",
        ))
    } else {
        None
    };

    let region = Region::new(region.to_string());
    let mut config_loader = aws_config::defaults(BehaviorVersion::latest()).region(region);

    if let Some(creds) = credentials {
        config_loader = config_loader.credentials_provider(creds);
    }

    if let Some(ref endpoint) = resolved_endpoint {
        config_loader = config_loader.endpoint_url(endpoint);
    }

    let sdk_config = config_loader.load().await;
    let client = Client::new(&sdk_config);

    Ok(client)
}

/// Format a DynamoDB AttributeValue as a human-readable string.
fn format_attribute_value(av: &AttributeValue) -> String {
    match av {
        AttributeValue::S(s) => s.clone(),
        AttributeValue::N(n) => n.clone(),
        AttributeValue::Bool(b) => b.to_string(),
        AttributeValue::Null(_) => "NULL".to_string(),
        AttributeValue::L(list) => {
            format!(
                "[{}]",
                list.iter()
                    .map(format_attribute_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        AttributeValue::M(map) => {
            format!(
                "{{{}}}",
                map.iter()
                    .map(|(k, v)| format!("{}: {}", k, format_attribute_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        AttributeValue::B(blob) => format!("<binary {} bytes>", blob.as_ref().len()),
        AttributeValue::Ss(set) => format!("[{}]", set.join(", ")),
        AttributeValue::Ns(set) => format!("[{}]", set.join(", ")),
        AttributeValue::Bs(set) => format!("[{} binary items]", set.len()),
        _ => "<unknown>".to_string(),
    }
}

/// Maximum number of rows to fetch across all pages to prevent runaway pagination.
const MAX_PARTIQL_ROWS: usize = 10_000;

/// Execute a PartiQL statement against DynamoDB and return formatted output.
///
/// Paginates automatically using `next_token` since DynamoDB limits responses
/// to 1MB per call. A safety limit of MAX_PARTIQL_ROWS rows prevents runaway
/// pagination on very large tables.
async fn execute_partiql(client: &Client, statement: &str) -> Result<String, String> {
    // Paginate through all result pages (DynamoDB returns max 1MB per call)
    let mut all_items: Vec<HashMap<String, AttributeValue>> = Vec::new();
    let mut next_token: Option<String> = None;

    loop {
        let mut request = client.execute_statement().statement(statement);
        if let Some(ref token) = next_token {
            request = request.next_token(token);
        }

        let result = request
            .send()
            .await
            .map_err(|e| format!("PartiQL error: {}", e))?;

        all_items.extend(result.items().iter().cloned());

        // Safety limit to prevent runaway pagination
        if all_items.len() >= MAX_PARTIQL_ROWS {
            warn!(
                "DynamoDB: Result set truncated at {} rows (safety limit)",
                MAX_PARTIQL_ROWS
            );
            break;
        }

        next_token = result.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    if all_items.is_empty() {
        let trimmed = statement.trim().to_uppercase();
        if trimmed.starts_with("INSERT")
            || trimmed.starts_with("UPDATE")
            || trimmed.starts_with("DELETE")
        {
            return Ok("Statement executed successfully.".to_string());
        }
        return Ok("(empty result set)".to_string());
    }

    // Collect all unique column names from all items (sorted for consistency)
    let mut column_names: Vec<String> = Vec::new();
    let mut seen_columns: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &all_items {
        for key in item.keys() {
            if seen_columns.insert(key.clone()) {
                column_names.push(key.clone());
            }
        }
    }
    column_names.sort();

    // Calculate column widths
    let mut widths: Vec<usize> = column_names.iter().map(|c| c.len()).collect();
    let mut formatted_rows: Vec<Vec<String>> = Vec::new();

    for item in &all_items {
        let mut row = Vec::new();
        for (i, col) in column_names.iter().enumerate() {
            let value = if let Some(av) = item.get(col) {
                format_attribute_value(av)
            } else {
                "NULL".to_string()
            };
            if value.len() > widths[i] {
                widths[i] = value.len();
            }
            row.push(value);
        }
        formatted_rows.push(row);
    }

    // Cap column widths at 50 characters for readability
    for w in &mut widths {
        if *w > 50 {
            *w = 50;
        }
    }

    let mut output = String::new();

    // Header row
    let header: String = column_names
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = widths[i];
            if c.len() > w {
                c[..w].to_string()
            } else {
                format!("{:width$}", c, width = w)
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    output.push_str(&header);
    output.push('\n');

    // Separator
    let sep: String = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");
    output.push_str(&sep);
    output.push('\n');

    // Data rows
    for row in &formatted_rows {
        let row_str: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let w = widths[i];
                if cell.len() > w {
                    cell[..w].to_string()
                } else {
                    format!("{:width$}", cell, width = w)
                }
            })
            .collect::<Vec<_>>()
            .join(" | ");
        output.push_str(&row_str);
        output.push('\n');
    }

    if all_items.len() >= MAX_PARTIQL_ROWS {
        output.push_str(&format!(
            "({} row(s)) -- truncated at {} row limit",
            formatted_rows.len(),
            MAX_PARTIQL_ROWS
        ));
    } else {
        output.push_str(&format!("({} row(s))", formatted_rows.len()));
    }

    Ok(output)
}

/// List all DynamoDB tables.
async fn list_tables(client: &Client) -> Result<String, String> {
    let mut table_names: Vec<String> = Vec::new();
    let mut last_table: Option<String> = None;

    loop {
        let mut request = client.list_tables();
        if let Some(ref last) = last_table {
            request = request.exclusive_start_table_name(last);
        }

        let result = request
            .send()
            .await
            .map_err(|e| format!("ListTables error: {}", e))?;

        for name in result.table_names() {
            table_names.push(name.to_string());
        }

        last_table = result.last_evaluated_table_name().map(|s| s.to_string());
        if last_table.is_none() {
            break;
        }
    }

    if table_names.is_empty() {
        return Ok("No tables found.".to_string());
    }

    let mut output = String::new();
    output.push_str(&format!("Tables ({}):\n", table_names.len()));
    for name in &table_names {
        output.push_str(&format!("  {}\n", name));
    }
    Ok(output)
}

/// Describe a DynamoDB table schema.
async fn describe_table(client: &Client, table_name: &str) -> Result<String, String> {
    let result = client
        .describe_table()
        .table_name(table_name)
        .send()
        .await
        .map_err(|e| format!("DescribeTable error: {}", e))?;

    let table = result
        .table()
        .ok_or_else(|| "Table description not available".to_string())?;

    let mut output = String::new();
    output.push_str(&format!("Table: {}\n", table_name));

    if let Some(status) = table.table_status() {
        output.push_str(&format!("Status: {:?}\n", status));
    }

    if let Some(count) = table.item_count() {
        output.push_str(&format!("Item count: {}\n", count));
    }

    if let Some(size) = table.table_size_bytes() {
        output.push_str(&format!("Size: {} bytes\n", size));
    }

    let key_schema = table.key_schema();
    if !key_schema.is_empty() {
        output.push_str("\nKey schema:\n");
        for key in key_schema {
            let key_type = format!("{:?}", key.key_type());
            output.push_str(&format!("  {} ({})\n", key.attribute_name(), key_type));
        }
    }

    let attrs = table.attribute_definitions();
    if !attrs.is_empty() {
        output.push_str("\nAttribute definitions:\n");
        for attr in attrs {
            let attr_type = format!("{:?}", attr.attribute_type());
            output.push_str(&format!("  {} ({})\n", attr.attribute_name(), attr_type));
        }
    }

    let gsis = table.global_secondary_indexes();
    if !gsis.is_empty() {
        output.push_str("\nGlobal secondary indexes:\n");
        for gsi in gsis {
            if let Some(name) = gsi.index_name() {
                output.push_str(&format!("  {}\n", name));
                let keys = gsi.key_schema();
                for key in keys {
                    let key_type = format!("{:?}", key.key_type());
                    output.push_str(&format!("    {} ({})\n", key.attribute_name(), key_type));
                }
            }
        }
    }

    let lsis = table.local_secondary_indexes();
    if !lsis.is_empty() {
        output.push_str("\nLocal secondary indexes:\n");
        for lsi in lsis {
            if let Some(name) = lsi.index_name() {
                output.push_str(&format!("  {}\n", name));
                let keys = lsi.key_schema();
                for key in keys {
                    let key_type = format!("{:?}", key.key_type());
                    output.push_str(&format!("    {} ({})\n", key.attribute_name(), key_type));
                }
            }
        }
    }

    if let Some(billing) = table.billing_mode_summary() {
        if let Some(mode) = billing.billing_mode() {
            output.push_str(&format!("\nBilling mode: {:?}\n", mode));
        }
    }

    if let Some(throughput) = table.provisioned_throughput() {
        if let Some(read) = throughput.read_capacity_units() {
            if let Some(write) = throughput.write_capacity_units() {
                output.push_str(&format!(
                    "Provisioned throughput: {} RCU / {} WCU\n",
                    read, write
                ));
            }
        }
    }

    Ok(output)
}

/// Check if a PartiQL statement or DynamoDB command is modifying.
fn is_dynamodb_modifying_statement(command: &str) -> bool {
    let trimmed = command.trim().to_uppercase();
    trimmed.starts_with("INSERT")
        || trimmed.starts_with("UPDATE")
        || trimmed.starts_with("DELETE")
        || trimmed.starts_with("CREATE TABLE")
        || trimmed.starts_with("DROP TABLE")
}

/// Handle DynamoDB-specific commands (not PartiQL).
///
/// Returns Ok(Some(true)) if the command was handled, Ok(None) if not recognized.
async fn handle_dynamodb_command(
    command: &str,
    client: &Client,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<Option<bool>> {
    let command_lower = command.trim().to_lowercase();

    // \tables or LIST TABLES
    if command_lower == "\\tables" || command_lower == "list tables" {
        match list_tables(client).await {
            Ok(result) => {
                for line in result.lines() {
                    executor
                        .terminal
                        .write_line(line)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
        send_render(executor, to_client, recorder).await?;
        return Ok(Some(true));
    }

    // \describe <table> or DESCRIBE TABLE <table>
    if command_lower.starts_with("\\describe ") || command_lower.starts_with("describe table ") {
        let table_name = if command_lower.starts_with("\\describe ") {
            command[10..].trim()
        } else {
            command[15..].trim()
        };

        if table_name.is_empty() {
            executor
                .terminal
                .write_error("Usage: \\describe <table_name>")
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        } else {
            match describe_table(client, table_name).await {
                Ok(result) => {
                    for line in result.lines() {
                        executor
                            .terminal
                            .write_line(line)
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    }
                }
                Err(e) => {
                    executor
                        .terminal
                        .write_error(&e)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
            }
        }

        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(Some(true));
    }

    // CREATE TABLE (DynamoDB API)
    if command_lower.starts_with("create table ") {
        if security.base.read_only {
            executor
                .terminal
                .write_error("CREATE TABLE blocked: read-only mode is enabled.")
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
            return Ok(Some(true));
        }

        match parse_and_create_table(client, command).await {
            Ok(msg) => {
                executor
                    .terminal
                    .write_line(&msg)
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
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
        send_render(executor, to_client, recorder).await?;
        return Ok(Some(true));
    }

    // DROP TABLE <name>
    if command_lower.starts_with("drop table ") {
        if security.base.read_only {
            executor
                .terminal
                .write_error("DROP TABLE blocked: read-only mode is enabled.")
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
            return Ok(Some(true));
        }

        let table_name = command[11..].trim().trim_matches('"');
        match client.delete_table().table_name(table_name).send().await {
            Ok(_) => {
                executor
                    .terminal
                    .write_line(&format!("Table '{}' deleted.", table_name))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
            Err(e) => {
                executor
                    .terminal
                    .write_error(&format!("DROP TABLE error: {}", e))
                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            }
        }

        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(Some(true));
    }

    Ok(None)
}

/// Parse a simple CREATE TABLE command and execute it via the DynamoDB API.
///
/// Syntax: CREATE TABLE <name> (<attr> <type> HASH [, <attr> <type> RANGE])
/// Where <type> is one of: S (string), N (number), B (binary)
async fn parse_and_create_table(client: &Client, command: &str) -> Result<String, String> {
    use aws_sdk_dynamodb::types::{
        AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
    };

    let rest = command[13..].trim();

    let paren_start = rest
        .find('(')
        .ok_or("Syntax: CREATE TABLE <name> (<attr> <type> HASH [, <attr> <type> RANGE])")?;
    let table_name = rest[..paren_start].trim().trim_matches('"');
    if table_name.is_empty() {
        return Err("Table name is required.".to_string());
    }

    let paren_end = rest.rfind(')').ok_or("Missing closing parenthesis")?;
    let columns_str = &rest[paren_start + 1..paren_end];

    let columns: Vec<&str> = columns_str.split(',').collect();
    if columns.is_empty() {
        return Err("At least one key attribute is required.".to_string());
    }

    let mut key_schema: Vec<KeySchemaElement> = Vec::new();
    let mut attr_defs: Vec<AttributeDefinition> = Vec::new();

    for col_def in &columns {
        let parts: Vec<&str> = col_def.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(format!(
                "Invalid column definition '{}'. Expected: <name> <type> <HASH|RANGE>",
                col_def.trim()
            ));
        }

        let attr_name = parts[0].trim_matches('"');
        let attr_type_str = parts[1].to_uppercase();
        let key_type_str = parts[2].to_uppercase();

        let attr_type = match attr_type_str.as_str() {
            "S" | "STRING" => ScalarAttributeType::S,
            "N" | "NUMBER" => ScalarAttributeType::N,
            "B" | "BINARY" => ScalarAttributeType::B,
            _ => {
                return Err(format!(
                    "Unknown attribute type '{}'. Use S (string), N (number), or B (binary).",
                    parts[1]
                ))
            }
        };

        let key_type = match key_type_str.as_str() {
            "HASH" => KeyType::Hash,
            "RANGE" => KeyType::Range,
            _ => {
                return Err(format!(
                    "Unknown key type '{}'. Use HASH or RANGE.",
                    parts[2]
                ))
            }
        };

        attr_defs.push(
            AttributeDefinition::builder()
                .attribute_name(attr_name)
                .attribute_type(attr_type)
                .build()
                .map_err(|e| format!("Failed to build attribute definition: {}", e))?,
        );

        key_schema.push(
            KeySchemaElement::builder()
                .attribute_name(attr_name)
                .key_type(key_type)
                .build()
                .map_err(|e| format!("Failed to build key schema: {}", e))?,
        );
    }

    client
        .create_table()
        .table_name(table_name)
        .set_key_schema(Some(key_schema))
        .set_attribute_definitions(Some(attr_defs))
        .billing_mode(BillingMode::PayPerRequest)
        .send()
        .await
        .map_err(|e| format!("CreateTable error: {}", e))?;

    Ok(format!("Table '{}' created successfully.", table_name))
}

/// Handle built-in commands (help, quit, exit)
async fn handle_builtin_command(
    command: &str,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<bool> {
    let command_lower = command.to_lowercase();

    match command_lower.as_str() {
        "help" | "?" => {
            let mut sections = vec![
                HelpSection {
                    title: "Navigation",
                    commands: vec![
                        ("\\tables", "List all tables"),
                        ("LIST TABLES", "List all tables"),
                        ("\\describe <table>", "Describe table schema"),
                        ("DESCRIBE TABLE <t>", "Describe table schema"),
                    ],
                },
                HelpSection {
                    title: "PartiQL queries",
                    commands: vec![
                        ("SELECT * FROM \"Table\"", ""),
                        ("SELECT col FROM \"T\" WHERE pk = 'val'", ""),
                        ("INSERT INTO \"T\" VALUE {'pk': 'v', ...}", ""),
                        ("UPDATE \"T\" SET col = 'v' WHERE pk = 'k'", ""),
                        ("DELETE FROM \"T\" WHERE pk = 'k'", ""),
                    ],
                },
                HelpSection {
                    title: "Table management",
                    commands: vec![
                        ("CREATE TABLE <n> (<attr> <type> HASH[, ...])", ""),
                        ("DROP TABLE <name>", "Delete a table"),
                    ],
                },
            ];

            let mut export_cmds = Vec::new();
            if !security.disable_csv_export {
                export_cmds.push(("\\e <PartiQL query>", "Export results as CSV"));
            }
            if !export_cmds.is_empty() {
                sections.push(HelpSection {
                    title: "Export",
                    commands: export_cmds,
                });
            }

            render_help(executor, to_client, "DynamoDB CLI", &sections, recorder).await?;
            return Ok(true);
        }
        "quit" | "exit" => {
            return Err(handle_quit(executor, to_client, recorder).await);
        }
        _ => {}
    }

    Ok(false)
}

/// Handle CSV export for DynamoDB query results.
async fn handle_csv_export(
    query: &str,
    client: &Client,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    security: &DatabaseSecuritySettings,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
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
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    executor
        .terminal
        .write_line(&format!("Executing query: {}...", query))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    send_render(executor, to_client, recorder).await?;

    // Paginate through all result pages (DynamoDB returns max 1MB per call)
    let mut all_items: Vec<HashMap<String, AttributeValue>> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut truncated = false;

    loop {
        let mut request = client.execute_statement().statement(query);
        if let Some(ref token) = next_token {
            request = request.next_token(token);
        }

        let partiql_result = request
            .send()
            .await
            .map_err(|e| HandlerError::ProtocolError(format!("PartiQL error: {}", e)))?;

        all_items.extend(partiql_result.items().iter().cloned());

        // Safety limit to prevent runaway pagination
        if all_items.len() >= MAX_PARTIQL_ROWS {
            warn!(
                "DynamoDB: CSV export result set truncated at {} rows (safety limit)",
                MAX_PARTIQL_ROWS
            );
            truncated = true;
            break;
        }

        next_token = partiql_result.next_token().map(|s| s.to_string());
        if next_token.is_none() {
            break;
        }
    }

    if all_items.is_empty() {
        executor
            .terminal
            .write_line("No results to export.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        executor
            .terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        send_render(executor, to_client, recorder).await?;
        return Ok(());
    }

    // Collect column names
    let mut column_names: Vec<String> = Vec::new();
    let mut seen_columns: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &all_items {
        for key in item.keys() {
            if seen_columns.insert(key.clone()) {
                column_names.push(key.clone());
            }
        }
    }
    column_names.sort();

    // Build QueryResult for CSV export
    let mut rows = Vec::new();
    for item in &all_items {
        let mut row = Vec::new();
        for col in &column_names {
            let value = if let Some(av) = item.get(col) {
                format_attribute_value(av)
            } else {
                "NULL".to_string()
            };
            row.push(value);
        }
        rows.push(row);
    }

    let result = guacr_terminal::QueryResult {
        columns: column_names,
        rows,
        affected_rows: None,
        execution_time_ms: None,
    };

    let filename = generate_csv_filename(query, "dynamodb");
    let stream_idx = STREAM_INDEX.fetch_add(1, Ordering::SeqCst);
    let mut exporter = CsvExporter::new(stream_idx);

    let row_count_msg = if truncated {
        format!(
            "Beginning CSV download ({} rows, truncated at {} row limit). Press Ctrl+C to cancel.",
            result.rows.len(),
            MAX_PARTIQL_ROWS
        )
    } else {
        format!(
            "Beginning CSV download ({} rows). Press Ctrl+C to cancel.",
            result.rows.len()
        )
    };
    executor
        .terminal
        .write_line(&row_count_msg)
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    let file_instr = exporter.start_download(&filename);
    to_client
        .send(file_instr)
        .await
        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

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

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for DynamoDbHandler {
    fn name(&self) -> &str {
        "dynamodb"
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
    fn test_dynamodb_handler_new() {
        let handler = DynamoDbHandler::with_defaults();
        assert_eq!(
            <DynamoDbHandler as ProtocolHandler>::name(&handler),
            "dynamodb"
        );
    }

    #[test]
    fn test_dynamodb_config() {
        let config = DynamoDbConfig::default();
        assert_eq!(config.default_port, 8000);
        assert!(!config.require_tls);
    }

    #[test]
    fn test_format_attribute_value_string() {
        let av = AttributeValue::S("hello".to_string());
        assert_eq!(format_attribute_value(&av), "hello");
    }

    #[test]
    fn test_format_attribute_value_number() {
        let av = AttributeValue::N("42".to_string());
        assert_eq!(format_attribute_value(&av), "42");
    }

    #[test]
    fn test_format_attribute_value_bool() {
        let av = AttributeValue::Bool(true);
        assert_eq!(format_attribute_value(&av), "true");

        let av = AttributeValue::Bool(false);
        assert_eq!(format_attribute_value(&av), "false");
    }

    #[test]
    fn test_format_attribute_value_null() {
        let av = AttributeValue::Null(true);
        assert_eq!(format_attribute_value(&av), "NULL");
    }

    #[test]
    fn test_format_attribute_value_list() {
        let av = AttributeValue::L(vec![
            AttributeValue::S("a".to_string()),
            AttributeValue::N("1".to_string()),
        ]);
        assert_eq!(format_attribute_value(&av), "[a, 1]");
    }

    #[test]
    fn test_format_attribute_value_map() {
        let mut map = HashMap::new();
        map.insert("key".to_string(), AttributeValue::S("val".to_string()));
        let av = AttributeValue::M(map);
        assert_eq!(format_attribute_value(&av), "{key: val}");
    }

    #[test]
    fn test_format_attribute_value_string_set() {
        let av = AttributeValue::Ss(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(format_attribute_value(&av), "[a, b, c]");
    }

    #[test]
    fn test_format_attribute_value_number_set() {
        let av = AttributeValue::Ns(vec!["1".to_string(), "2".to_string(), "3".to_string()]);
        assert_eq!(format_attribute_value(&av), "[1, 2, 3]");
    }

    #[test]
    fn test_is_dynamodb_modifying_statement() {
        // Modifying statements
        assert!(is_dynamodb_modifying_statement(
            "INSERT INTO \"Users\" VALUE {'id': '1'}"
        ));
        assert!(is_dynamodb_modifying_statement(
            "UPDATE \"Users\" SET name = 'x' WHERE id = '1'"
        ));
        assert!(is_dynamodb_modifying_statement(
            "DELETE FROM \"Users\" WHERE id = '1'"
        ));
        assert!(is_dynamodb_modifying_statement(
            "CREATE TABLE test (id S HASH)"
        ));
        assert!(is_dynamodb_modifying_statement("DROP TABLE test"));

        // Read-only statements
        assert!(!is_dynamodb_modifying_statement("SELECT * FROM \"Users\""));
        assert!(!is_dynamodb_modifying_statement("LIST TABLES"));
        assert!(!is_dynamodb_modifying_statement("\\tables"));
        assert!(!is_dynamodb_modifying_statement("help"));
    }

    #[test]
    fn test_is_dynamodb_modifying_case_insensitive() {
        assert!(is_dynamodb_modifying_statement(
            "insert into \"T\" VALUE {'pk': '1'}"
        ));
        assert!(is_dynamodb_modifying_statement(
            "update \"T\" SET x = 1 WHERE pk = '1'"
        ));
        assert!(is_dynamodb_modifying_statement(
            "delete from \"T\" WHERE pk = '1'"
        ));
    }
}
