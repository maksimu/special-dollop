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

use crate::handler_helpers::{
    handle_quit, parse_display_size, render_connection_error, render_connection_success,
    render_help, send_render, HelpSection,
};
use crate::query_executor::QueryExecutor;
use crate::recording::{
    finalize_recording, init_recording, record_error_output, record_query_input,
};
use crate::security::DatabaseSecuritySettings;

/// Elasticsearch handler
///
/// Provides interactive Elasticsearch access via REST API.
/// Supports both SQL queries (via the ES SQL API) and raw JSON search queries.
pub struct ElasticsearchHandler {
    config: ElasticsearchConfig,
}

#[derive(Debug, Clone)]
pub struct ElasticsearchConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub require_auth: bool,
    pub connection_timeout_secs: u64,
}

impl Default for ElasticsearchConfig {
    fn default() -> Self {
        Self {
            default_port: 9200,
            require_tls: false,
            require_auth: false, // ES can run without auth
            connection_timeout_secs: guacr_handlers::DEFAULT_CONNECTION_TIMEOUT_SECS,
        }
    }
}

impl ElasticsearchHandler {
    pub fn new(config: ElasticsearchConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(ElasticsearchConfig::default())
    }
}

/// Elasticsearch client wrapper holding the HTTP client and base URL
struct ElasticsearchClient {
    http: reqwest::Client,
    base_url: String,
}

impl ElasticsearchClient {
    fn new(
        base_url: String,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // Allow self-signed certs for dev
            .timeout(std::time::Duration::from_secs(30));

        // If both username and password are provided, use basic auth via default headers
        if let (Some(user), Some(pass)) = (username, password) {
            let credentials = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!("{}:{}", user, pass),
            );
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Basic {}", credentials))
                    .map_err(|e| format!("Invalid auth header: {}", e))?,
            );
            builder = builder.default_headers(headers);
        }

        let http = builder
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        Ok(Self { http, base_url })
    }

    /// Execute a SQL query via the ES SQL API
    async fn execute_sql(&self, sql: &str) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/_sql?format=json", self.base_url);
        let body = serde_json::json!({ "query": sql });

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SQL request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_sql_response(&json)
    }

    /// Execute a raw JSON search query against an index
    async fn execute_search(
        &self,
        index: &str,
        body: &serde_json::Value,
    ) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/{}/_search", self.base_url, index);

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Search request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_search_response(&json)
    }

    /// Execute a generic REST API call (GET/PUT/POST/DELETE)
    async fn execute_rest(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<String, String> {
        let url = format!("{}{}", self.base_url, path);

        let request = match method.to_uppercase().as_str() {
            "GET" => self.http.get(&url),
            "PUT" => self.http.put(&url),
            "POST" => self.http.post(&url),
            "DELETE" => self.http.delete(&url),
            "HEAD" => self.http.head(&url),
            other => return Err(format!("Unsupported HTTP method: {}", other)),
        };

        let request = request.header("Content-Type", "application/json");
        let request = if let Some(b) = body {
            request.json(b)
        } else {
            request
        };

        let resp = request
            .send()
            .await
            .map_err(|e| format!("REST request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        Ok(text)
    }

    /// Check cluster health (used for connection test)
    async fn cluster_health(&self) -> Result<String, String> {
        self.execute_rest("GET", "/_cluster/health", None).await
    }

    /// Get cat/indices output
    async fn cat_indices(&self) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/_cat/indices?format=json", self.base_url);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_cat_response(&json)
    }

    /// Get cat/health output
    async fn cat_health(&self) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/_cat/health?format=json", self.base_url);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_cat_response(&json)
    }

    /// Get cat/nodes output
    async fn cat_nodes(&self) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/_cat/nodes?format=json", self.base_url);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_cat_response(&json)
    }

    /// Get index mapping (DESCRIBE equivalent)
    async fn describe_index(&self, index: &str) -> Result<guacr_terminal::QueryResult, String> {
        let url = format!("{}/{}/_mapping", self.base_url, index);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        if !status.is_success() {
            return Err(parse_es_error(&text, status.as_u16()));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid JSON response: {}", e))?;

        parse_mapping_response(&json, index)
    }
}

/// Parse an Elasticsearch error response into a human-readable message
fn parse_es_error(body: &str, status_code: u16) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = json.get("error") {
            if let Some(reason) = error.get("reason").and_then(|r| r.as_str()) {
                let error_type = error
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");
                return format!("[{}] {}: {}", status_code, error_type, reason);
            }
            if let Some(msg) = error.as_str() {
                return format!("[{}] {}", status_code, msg);
            }
        }
    }
    format!(
        "[{}] {}",
        status_code,
        body.chars().take(200).collect::<String>()
    )
}

/// Parse SQL API response into QueryResult
fn parse_sql_response(json: &serde_json::Value) -> Result<guacr_terminal::QueryResult, String> {
    let columns: Vec<String> = json
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|cols| {
            cols.iter()
                .map(|c| {
                    c.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("?")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default();

    let rows: Vec<Vec<String>> = json
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    if let Some(arr) = row.as_array() {
                        arr.iter().map(format_json_value).collect()
                    } else {
                        vec![format_json_value(row)]
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut result = guacr_terminal::QueryResult::new(columns);
    result.rows = rows;
    Ok(result)
}

/// Parse search API response into QueryResult
fn parse_search_response(json: &serde_json::Value) -> Result<guacr_terminal::QueryResult, String> {
    let hits = json
        .get("hits")
        .and_then(|h| h.get("hits"))
        .and_then(|h| h.as_array())
        .ok_or_else(|| "No hits found in response".to_string())?;

    if hits.is_empty() {
        let mut result = guacr_terminal::QueryResult::new(vec!["(no results)".to_string()]);
        result.rows = vec![vec!["0 hits".to_string()]];
        return Ok(result);
    }

    // Collect all unique field names from _source across all hits.
    // Use a Vec to maintain insertion order without needing indexmap.
    let mut columns: Vec<String> = vec!["_id".to_string()];

    for hit in hits {
        if let Some(source) = hit.get("_source").and_then(|s| s.as_object()) {
            for key in source.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }

    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|hit| {
            let id = hit.get("_id").map(format_json_value).unwrap_or_default();
            let source = hit.get("_source");

            columns
                .iter()
                .map(|col| {
                    if col == "_id" {
                        id.clone()
                    } else {
                        source
                            .and_then(|s| s.get(col.as_str()))
                            .map(format_json_value)
                            .unwrap_or_else(|| "NULL".to_string())
                    }
                })
                .collect()
        })
        .collect();

    let total = json
        .get("hits")
        .and_then(|h| h.get("total"))
        .and_then(|t| t.get("value"))
        .and_then(|v| v.as_u64());

    let mut result = guacr_terminal::QueryResult::new(columns);
    result.rows = rows;
    if let Some(total) = total {
        result.affected_rows = Some(total);
    }
    Ok(result)
}

/// Parse _cat API JSON response into QueryResult
fn parse_cat_response(json: &serde_json::Value) -> Result<guacr_terminal::QueryResult, String> {
    let arr = json
        .as_array()
        .ok_or_else(|| "Expected JSON array from _cat API".to_string())?;

    if arr.is_empty() {
        let mut result = guacr_terminal::QueryResult::new(vec!["(empty)".to_string()]);
        result.rows = vec![vec!["No data".to_string()]];
        return Ok(result);
    }

    // Collect column names from first object
    let columns: Vec<String> = arr[0]
        .as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    let rows: Vec<Vec<String>> = arr
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|col| {
                    item.get(col.as_str())
                        .map(format_json_value)
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();

    let mut result = guacr_terminal::QueryResult::new(columns);
    result.rows = rows;
    Ok(result)
}

/// Parse _mapping response into QueryResult showing field name and type
fn parse_mapping_response(
    json: &serde_json::Value,
    index: &str,
) -> Result<guacr_terminal::QueryResult, String> {
    // Mapping response: { "index_name": { "mappings": { "properties": { ... } } } }
    let properties = json
        .get(index)
        .or_else(|| {
            // Try the first key if index name doesn't match exactly
            json.as_object().and_then(|obj| obj.values().next())
        })
        .and_then(|idx| idx.get("mappings"))
        .and_then(|m| m.get("properties"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| format!("No mapping found for index '{}'", index))?;

    let mut result =
        guacr_terminal::QueryResult::new(vec!["field".to_string(), "type".to_string()]);

    let mut rows = Vec::new();
    flatten_mapping_properties(properties, "", &mut rows);
    result.rows = rows;
    Ok(result)
}

/// Recursively flatten nested mapping properties into field/type rows
fn flatten_mapping_properties(
    properties: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
    rows: &mut Vec<Vec<String>>,
) {
    for (name, value) in properties {
        let full_name = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}.{}", prefix, name)
        };

        let field_type = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("object")
            .to_string();

        rows.push(vec![full_name.clone(), field_type]);

        // Recurse into nested properties
        if let Some(nested) = value.get("properties").and_then(|p| p.as_object()) {
            flatten_mapping_properties(nested, &full_name, rows);
        }
    }
}

/// Format a JSON value for display in result tables
fn format_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_json_value).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(_) => {
            // Compact JSON for nested objects
            serde_json::to_string(value).unwrap_or_else(|_| "{...}".to_string())
        }
    }
}

/// Classify the user input to determine how to handle it
enum InputType {
    /// SQL query to be sent to the ES SQL API
    Sql(String),
    /// JSON search body with target index: POST /<index>/_search
    JsonSearch {
        index: String,
        body: serde_json::Value,
    },
    /// REST API call: METHOD /path [body]
    RestCall {
        method: String,
        path: String,
        body: Option<serde_json::Value>,
    },
    /// Shortcut command (\indices, \health, \nodes, \d <index>)
    Shortcut(ShortcutCommand),
    /// Built-in command (help, quit, exit)
    Builtin(String),
    /// Empty or whitespace-only input
    Empty,
}

#[derive(Debug)]
enum ShortcutCommand {
    Indices,
    Health,
    Nodes,
    Describe(String),
}

/// Classify the input into one of the recognized categories
fn classify_input(input: &str) -> InputType {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return InputType::Empty;
    }

    let lower = trimmed.to_lowercase();

    // Built-in commands
    if matches!(lower.as_str(), "help" | "?" | "quit" | "exit") {
        return InputType::Builtin(lower);
    }

    // Shortcut commands
    if lower == "\\indices" || lower == "\\i" {
        return InputType::Shortcut(ShortcutCommand::Indices);
    }
    if lower == "\\health" || lower == "\\h" {
        return InputType::Shortcut(ShortcutCommand::Health);
    }
    if lower == "\\nodes" || lower == "\\n" {
        return InputType::Shortcut(ShortcutCommand::Nodes);
    }
    if lower.starts_with("\\d ") || lower.starts_with("\\describe ") {
        let index = if lower.starts_with("\\d ") {
            trimmed[3..].trim().to_string()
        } else {
            trimmed[10..].trim().to_string()
        };
        if !index.is_empty() {
            return InputType::Shortcut(ShortcutCommand::Describe(index));
        }
    }

    // REST API calls: METHOD /path [body]
    if is_rest_call(&lower) {
        return parse_rest_call(trimmed);
    }

    // DESCRIBE <index> as SQL-like shorthand
    if lower.starts_with("describe ") || lower.starts_with("desc ") {
        let index = if lower.starts_with("describe ") {
            trimmed[9..].trim().to_string()
        } else {
            trimmed[5..].trim().to_string()
        };
        if !index.is_empty() {
            return InputType::Shortcut(ShortcutCommand::Describe(index));
        }
    }

    // JSON input starting with { - treat as search body against _all
    if trimmed.starts_with('{') {
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(body) => {
                return InputType::JsonSearch {
                    index: "_all".to_string(),
                    body,
                };
            }
            Err(e) => {
                // Invalid JSON, treat as SQL and let ES return the error
                debug!("Input looks like JSON but failed to parse: {}", e);
            }
        }
    }

    // Check for "index_name { ... }" pattern
    if let Some(brace_pos) = trimmed.find('{') {
        let potential_index = trimmed[..brace_pos].trim();
        if !potential_index.is_empty()
            && !potential_index.contains(' ')
            && potential_index
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '*')
        {
            let json_str = &trimmed[brace_pos..];
            if let Ok(body) = serde_json::from_str::<serde_json::Value>(json_str) {
                return InputType::JsonSearch {
                    index: potential_index.to_string(),
                    body,
                };
            }
        }
    }

    // Default: SQL query
    InputType::Sql(trimmed.to_string())
}

/// Check if input starts with an HTTP method followed by a path
fn is_rest_call(lower: &str) -> bool {
    lower.starts_with("get /")
        || lower.starts_with("put /")
        || lower.starts_with("post /")
        || lower.starts_with("delete /")
        || lower.starts_with("head /")
}

/// Parse a REST API call input into method, path, and optional body
fn parse_rest_call(input: &str) -> InputType {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    if parts.len() < 2 {
        return InputType::Sql(input.to_string());
    }

    let method = parts[0].to_uppercase();
    let rest = parts[1].trim();

    // Split path and body at first '{'
    let (path, body) = if let Some(brace_pos) = rest.find('{') {
        let p = rest[..brace_pos].trim();
        let json_str = &rest[brace_pos..];
        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(b) => (p.to_string(), Some(b)),
            Err(_) => (rest.to_string(), None),
        }
    } else {
        (rest.to_string(), None)
    };

    // Ensure path starts with /
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    InputType::RestCall { method, path, body }
}

/// Check if an Elasticsearch operation is modifying (not read-only)
fn is_es_modifying_operation(input: &str) -> bool {
    let lower = input.trim().to_lowercase();

    // PUT and DELETE are always modifying
    if lower.starts_with("put ") || lower.starts_with("delete ") {
        return true;
    }
    // POST is modifying except for search and SQL queries
    if let Some(stripped) = lower.strip_prefix("post ") {
        let path = stripped.trim();
        // Allow POST /_sql and POST /<index>/_search as read-only
        if path.starts_with("/_sql") || path.contains("/_search") || path.contains("/_msearch") {
            return false;
        }
        return true;
    }

    false
}

/// Format a raw JSON REST API response for terminal display
fn format_rest_response(text: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        serde_json::to_string_pretty(&json).unwrap_or_else(|_| text.to_string())
    } else {
        text.to_string()
    }
}

#[async_trait]
impl ProtocolHandler for ElasticsearchHandler {
    fn name(&self) -> &str {
        "elasticsearch"
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
        info!("Elasticsearch handler starting");

        // Parse security settings
        let security = DatabaseSecuritySettings::from_params(&params);
        if security.base.read_only {
            info!("Elasticsearch: Read-only mode enabled");
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
        let username = params.get("username").map(|s| s.as_str());
        let password = params.get("password").map(|s| s.as_str());
        let use_tls = params
            .get("use-tls")
            .or_else(|| params.get("use_tls"))
            .map(|v| v == "true" || v == "1")
            .unwrap_or(self.config.require_tls);

        // Security check
        if self.config.require_auth && (username.is_none() || password.is_none()) {
            return Err(HandlerError::MissingParameter(
                "username and password (required for security)".to_string(),
            ));
        }

        info!("Elasticsearch: Connecting to {}:{}", hostname, port);

        // Parse display size from parameters
        let (width, height, cols, rows) = parse_display_size(&params);

        info!(
            "Elasticsearch: Display size {}x{} px -> {}x{} chars",
            width, height, cols, rows
        );

        // Create query executor with Elasticsearch prompt and correct dimensions
        let prompt = if security.base.read_only {
            "es [RO]> "
        } else {
            "es> "
        };
        let mut executor = QueryExecutor::new_with_size(prompt, "elasticsearch", rows, cols)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Initialize recording if enabled
        let mut recorder = init_recording(&recording_config, &params, "Elasticsearch", cols, rows);

        // Send display initialization instructions (ready, name, cursor, size)
        QueryExecutor::send_display_init(&to_client, width, height).await?;
        debug!("Elasticsearch: Sent display init instructions");

        // Build base URL
        let scheme = if use_tls { "https" } else { "http" };
        let base_url = format!("{}://{}:{}", scheme, hostname, port);

        debug!("Elasticsearch: Base URL: {}", base_url);

        // Create Elasticsearch client
        let client = match ElasticsearchClient::new(base_url.clone(), username, password) {
            Ok(c) => c,
            Err(e) => {
                let error_msg = format!("Failed to create Elasticsearch client: {}", e);
                warn!("Elasticsearch: {}", error_msg);

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

        // Test connection by checking cluster health
        match client.cluster_health().await {
            Ok(health_text) => {
                info!("Elasticsearch: Connected successfully");

                // Extract cluster name and status from health response
                let cluster_info =
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&health_text) {
                        let name = json
                            .get("cluster_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let status = json
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let nodes = json
                            .get("number_of_nodes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        format!("Cluster: {} (status: {}, nodes: {})", name, status, nodes)
                    } else {
                        "Connected".to_string()
                    };

                let conn_line = format!("Connected to Elasticsearch at {}:{}", hostname, port);
                render_connection_success(
                    &mut executor,
                    &to_client,
                    &[
                        &conn_line,
                        &cluster_info,
                        "Type 'help' for available commands.",
                    ],
                    &mut recorder,
                )
                .await?;
                debug!("Elasticsearch: Initial screen sent successfully");
            }
            Err(e) => {
                let error_msg = format!("Elasticsearch connection failed: {}", e);
                warn!("Elasticsearch: {}", error_msg);

                render_connection_error(
                    &mut executor,
                    &to_client,
                    &mut from_client,
                    &error_msg,
                    &[
                        "Check hostname and port",
                        "Verify Elasticsearch is running",
                        "Check credentials if authentication is enabled",
                        "Check TLS settings (use-tls parameter)",
                        "Check firewall rules",
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
                        debug!("Elasticsearch: Client disconnected, stopping debounce timer");
                        break;
                    }

                    if executor.is_dirty() {
                        let (_, instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in instructions {
                            if crate::recording::send_and_record(&to_client, &mut recorder, instr).await.is_err() {
                                debug!("Elasticsearch: Client channel closed during debounce, stopping");
                                break 'outer;
                            }
                        }
                    }
                }

                // Process input from client
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Elasticsearch: Client disconnected");
                        break 'outer;
                    };
                    match executor.process_input(&msg).await {
                        Ok((needs_render, instructions, pending_query)) => {
                    if let Some(command) = pending_query {
                        info!("Elasticsearch: Executing command: {}", command);

                        // Record query input
                        record_query_input(&mut recorder, &recording_config, &command);

                        // Classify and handle the input
                        let input_type = classify_input(&command);

                        match input_type {
                            InputType::Empty => {
                                executor
                                    .terminal
                                    .write_prompt()
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }

                            InputType::Builtin(cmd) => {
                                if handle_builtin_command(
                                    &cmd,
                                    &mut executor,
                                    &to_client,
                                    &security,
                                    &mut recorder,
                                )
                                .await?
                                {
                                    continue;
                                }
                            }

                            InputType::Shortcut(shortcut) => {
                                handle_shortcut_command(
                                    shortcut,
                                    &client,
                                    &mut executor,
                                    &to_client,
                                    &mut recorder,
                                )
                                .await?;
                                continue;
                            }

                            InputType::Sql(sql) => {
                                let start = std::time::Instant::now();
                                match client.execute_sql(&sql).await {
                                    Ok(mut result) => {
                                        result.execution_time_ms =
                                            Some(start.elapsed().as_millis() as u64);
                                        executor
                                            .terminal
                                            .write_result(&result)
                                            .map_err(|e| {
                                                HandlerError::ProtocolError(e.to_string())
                                            })?;
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
                            }

                            InputType::JsonSearch { index, body } => {
                                let start = std::time::Instant::now();
                                match client.execute_search(&index, &body).await {
                                    Ok(mut result) => {
                                        result.execution_time_ms =
                                            Some(start.elapsed().as_millis() as u64);
                                        executor
                                            .terminal
                                            .write_result(&result)
                                            .map_err(|e| {
                                                HandlerError::ProtocolError(e.to_string())
                                            })?;
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
                            }

                            InputType::RestCall { method, path, body } => {
                                // Check read-only mode for modifying operations
                                if security.base.read_only
                                    && is_es_modifying_operation(&format!(
                                        "{} {}",
                                        method, path
                                    ))
                                {
                                    executor
                                        .terminal
                                        .write_error(
                                            "Operation blocked: read-only mode is enabled.",
                                        )
                                        .map_err(|e| {
                                            HandlerError::ProtocolError(e.to_string())
                                        })?;
                                } else {
                                    let start = std::time::Instant::now();
                                    match client
                                        .execute_rest(&method, &path, body.as_ref())
                                        .await
                                    {
                                        Ok(response_text) => {
                                            let elapsed = start.elapsed();
                                            let formatted =
                                                format_rest_response(&response_text);
                                            for line in formatted.lines() {
                                                executor
                                                    .terminal
                                                    .write_line(line)
                                                    .map_err(|e| {
                                                        HandlerError::ProtocolError(
                                                            e.to_string(),
                                                        )
                                                    })?;
                                            }
                                            executor
                                                .terminal
                                                .write_line(&format!(
                                                    "({} ms)",
                                                    elapsed.as_millis()
                                                ))
                                                .map_err(|e| {
                                                    HandlerError::ProtocolError(
                                                        e.to_string(),
                                                    )
                                                })?;
                                        }
                                        Err(e) => {
                                            record_error_output(&mut recorder, &e);
                                            executor
                                                .terminal
                                                .write_error(&e)
                                                .map_err(|e| {
                                                    HandlerError::ProtocolError(
                                                        e.to_string(),
                                                    )
                                                })?;
                                        }
                                    }
                                }
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
                            warn!("Elasticsearch: Input processing error: {}", e);
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
        finalize_recording(recorder, "Elasticsearch");

        send_disconnect(&to_client).await;
        info!("Elasticsearch handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
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
            let mut rest_cmds: Vec<(&'static str, &'static str)> = vec![
                ("GET /_cat/indices", ""),
                ("GET /_cluster/health", ""),
                ("GET /_cat/nodes", ""),
            ];
            if !security.base.read_only {
                rest_cmds.push(("PUT /my-index {...}", ""));
                rest_cmds.push(("POST /my-index/_doc {...}", ""));
                rest_cmds.push(("DELETE /my-index", ""));
            }

            let sections = vec![
                HelpSection {
                    title: "SQL queries (via ES SQL API)",
                    commands: vec![
                        ("SELECT * FROM my_index LIMIT 10", ""),
                        ("SHOW TABLES", ""),
                        ("DESCRIBE my_index", ""),
                    ],
                },
                HelpSection {
                    title: "JSON search queries",
                    commands: vec![
                        ("my_index {\"query\": {\"match_all\": {}}}", ""),
                        ("{\"query\": {\"match\": {\"field\": \"value\"}}}", ""),
                    ],
                },
                HelpSection {
                    title: "REST API calls",
                    commands: rest_cmds,
                },
                HelpSection {
                    title: "Shortcuts",
                    commands: vec![
                        ("\\indices (\\i)", "List all indices"),
                        ("\\health  (\\h)", "Cluster health"),
                        ("\\nodes   (\\n)", "Node information"),
                        ("\\d <index>", "Show index mapping"),
                    ],
                },
            ];

            render_help(
                executor,
                to_client,
                "Elasticsearch CLI",
                &sections,
                recorder,
            )
            .await?;
            return Ok(true);
        }
        "quit" | "exit" => {
            return Err(handle_quit(executor, to_client, recorder).await);
        }
        _ => {}
    }

    Ok(false)
}

/// Handle shortcut commands (\indices, \health, \nodes, \d)
async fn handle_shortcut_command(
    shortcut: ShortcutCommand,
    client: &ElasticsearchClient,
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    recorder: &mut Option<guacr_handlers::MultiFormatRecorder>,
) -> guacr_handlers::Result<()> {
    let start = std::time::Instant::now();

    let result = match shortcut {
        ShortcutCommand::Indices => client.cat_indices().await,
        ShortcutCommand::Health => client.cat_health().await,
        ShortcutCommand::Nodes => client.cat_nodes().await,
        ShortcutCommand::Describe(ref index) => client.describe_index(index).await,
    };

    match result {
        Ok(mut query_result) => {
            query_result.execution_time_ms = Some(start.elapsed().as_millis() as u64);
            executor
                .terminal
                .write_result(&query_result)
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
        Err(e) => {
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

    send_render(executor, to_client, recorder).await?;

    Ok(())
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for ElasticsearchHandler {
    fn name(&self) -> &str {
        "elasticsearch"
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
    fn test_elasticsearch_handler_new() {
        let handler = ElasticsearchHandler::with_defaults();
        assert_eq!(
            <ElasticsearchHandler as ProtocolHandler>::name(&handler),
            "elasticsearch"
        );
    }

    #[test]
    fn test_elasticsearch_config() {
        let config = ElasticsearchConfig::default();
        assert_eq!(config.default_port, 9200);
        assert!(!config.require_auth);
        assert!(!config.require_tls);
    }

    #[test]
    fn test_classify_input_empty() {
        assert!(matches!(classify_input(""), InputType::Empty));
        assert!(matches!(classify_input("   "), InputType::Empty));
    }

    #[test]
    fn test_classify_input_builtin() {
        assert!(matches!(classify_input("help"), InputType::Builtin(_)));
        assert!(matches!(classify_input("?"), InputType::Builtin(_)));
        assert!(matches!(classify_input("quit"), InputType::Builtin(_)));
        assert!(matches!(classify_input("exit"), InputType::Builtin(_)));
    }

    #[test]
    fn test_classify_input_shortcuts() {
        assert!(matches!(
            classify_input("\\indices"),
            InputType::Shortcut(ShortcutCommand::Indices)
        ));
        assert!(matches!(
            classify_input("\\i"),
            InputType::Shortcut(ShortcutCommand::Indices)
        ));
        assert!(matches!(
            classify_input("\\health"),
            InputType::Shortcut(ShortcutCommand::Health)
        ));
        assert!(matches!(
            classify_input("\\h"),
            InputType::Shortcut(ShortcutCommand::Health)
        ));
        assert!(matches!(
            classify_input("\\nodes"),
            InputType::Shortcut(ShortcutCommand::Nodes)
        ));
        assert!(matches!(
            classify_input("\\n"),
            InputType::Shortcut(ShortcutCommand::Nodes)
        ));
        assert!(matches!(
            classify_input("\\d my_index"),
            InputType::Shortcut(ShortcutCommand::Describe(_))
        ));
        if let InputType::Shortcut(ShortcutCommand::Describe(idx)) = classify_input("\\d my_index")
        {
            assert_eq!(idx, "my_index");
        }
    }

    #[test]
    fn test_classify_input_describe() {
        assert!(matches!(
            classify_input("DESCRIBE my_index"),
            InputType::Shortcut(ShortcutCommand::Describe(_))
        ));
        assert!(matches!(
            classify_input("desc my_index"),
            InputType::Shortcut(ShortcutCommand::Describe(_))
        ));
    }

    #[test]
    fn test_classify_input_sql() {
        assert!(matches!(
            classify_input("SELECT * FROM my_index"),
            InputType::Sql(_)
        ));
        assert!(matches!(classify_input("SHOW TABLES"), InputType::Sql(_)));
    }

    #[test]
    fn test_classify_input_rest_call() {
        assert!(matches!(
            classify_input("GET /_cat/indices"),
            InputType::RestCall { .. }
        ));
        assert!(matches!(
            classify_input("PUT /my-index"),
            InputType::RestCall { .. }
        ));
        assert!(matches!(
            classify_input("DELETE /my-index"),
            InputType::RestCall { .. }
        ));
    }

    #[test]
    fn test_classify_input_rest_call_with_body() {
        let input = "POST /my-index/_doc {\"name\": \"test\"}";
        if let InputType::RestCall { method, path, body } = classify_input(input) {
            assert_eq!(method, "POST");
            assert_eq!(path, "/my-index/_doc");
            assert!(body.is_some());
            assert_eq!(body.unwrap()["name"], "test");
        } else {
            panic!("Expected RestCall");
        }
    }

    #[test]
    fn test_classify_input_json_search() {
        let input = "my_index {\"query\": {\"match_all\": {}}}";
        if let InputType::JsonSearch { index, body } = classify_input(input) {
            assert_eq!(index, "my_index");
            assert!(body.get("query").is_some());
        } else {
            panic!("Expected JsonSearch");
        }
    }

    #[test]
    fn test_classify_input_json_search_no_index() {
        let input = "{\"query\": {\"match_all\": {}}}";
        if let InputType::JsonSearch { index, body } = classify_input(input) {
            assert_eq!(index, "_all");
            assert!(body.get("query").is_some());
        } else {
            panic!("Expected JsonSearch");
        }
    }

    #[test]
    fn test_is_es_modifying_operation() {
        assert!(is_es_modifying_operation("PUT /my-index"));
        assert!(is_es_modifying_operation("DELETE /my-index"));
        assert!(is_es_modifying_operation("POST /my-index/_doc"));
        assert!(!is_es_modifying_operation("GET /_cat/indices"));
        assert!(!is_es_modifying_operation("POST /_sql"));
        assert!(!is_es_modifying_operation("POST /my-index/_search"));
        assert!(!is_es_modifying_operation("POST /my-index/_msearch"));
    }

    #[test]
    fn test_format_json_value() {
        assert_eq!(format_json_value(&serde_json::Value::Null), "NULL");
        assert_eq!(format_json_value(&serde_json::Value::Bool(true)), "true");
        assert_eq!(format_json_value(&serde_json::json!(42)), "42");
        assert_eq!(format_json_value(&serde_json::json!("hello")), "hello");
        assert_eq!(
            format_json_value(&serde_json::json!([1, 2, 3])),
            "[1, 2, 3]"
        );
    }

    #[test]
    fn test_parse_sql_response() {
        let json = serde_json::json!({
            "columns": [
                {"name": "id", "type": "integer"},
                {"name": "name", "type": "text"}
            ],
            "rows": [
                [1, "Alice"],
                [2, "Bob"]
            ]
        });

        let result = parse_sql_response(&json).unwrap();
        assert_eq!(result.columns, vec!["id", "name"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0], vec!["1", "Alice"]);
        assert_eq!(result.rows[1], vec!["2", "Bob"]);
    }

    #[test]
    fn test_parse_sql_response_empty() {
        let json = serde_json::json!({
            "columns": [
                {"name": "id", "type": "integer"}
            ],
            "rows": []
        });

        let result = parse_sql_response(&json).unwrap();
        assert_eq!(result.columns, vec!["id"]);
        assert_eq!(result.rows.len(), 0);
    }

    #[test]
    fn test_parse_search_response() {
        let json = serde_json::json!({
            "hits": {
                "total": {"value": 2},
                "hits": [
                    {
                        "_id": "1",
                        "_source": {"name": "Alice", "age": 30}
                    },
                    {
                        "_id": "2",
                        "_source": {"name": "Bob", "age": 25}
                    }
                ]
            }
        });

        let result = parse_search_response(&json).unwrap();
        assert!(result.columns.contains(&"_id".to_string()));
        assert!(result.columns.contains(&"name".to_string()));
        assert!(result.columns.contains(&"age".to_string()));
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.affected_rows, Some(2));
    }

    #[test]
    fn test_parse_search_response_empty() {
        let json = serde_json::json!({
            "hits": {
                "total": {"value": 0},
                "hits": []
            }
        });

        let result = parse_search_response(&json).unwrap();
        assert_eq!(result.columns, vec!["(no results)"]);
        assert_eq!(result.rows.len(), 1);
    }

    #[test]
    fn test_parse_search_response_mixed_fields() {
        let json = serde_json::json!({
            "hits": {
                "total": {"value": 2},
                "hits": [
                    {
                        "_id": "1",
                        "_source": {"name": "Alice"}
                    },
                    {
                        "_id": "2",
                        "_source": {"name": "Bob", "email": "bob@test.com"}
                    }
                ]
            }
        });

        let result = parse_search_response(&json).unwrap();
        // Both name and email should be columns
        assert!(result.columns.contains(&"name".to_string()));
        assert!(result.columns.contains(&"email".to_string()));
        // First row should have NULL for email
        let email_idx = result.columns.iter().position(|c| c == "email").unwrap();
        assert_eq!(result.rows[0][email_idx], "NULL");
        assert_eq!(result.rows[1][email_idx], "bob@test.com");
    }

    #[test]
    fn test_parse_cat_response() {
        let json = serde_json::json!([
            {"health": "green", "index": "test-index", "docs.count": "100"},
            {"health": "yellow", "index": "other-index", "docs.count": "50"}
        ]);

        let result = parse_cat_response(&json).unwrap();
        assert!(result.columns.contains(&"health".to_string()));
        assert!(result.columns.contains(&"index".to_string()));
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn test_parse_cat_response_empty() {
        let json = serde_json::json!([]);
        let result = parse_cat_response(&json).unwrap();
        assert_eq!(result.columns, vec!["(empty)"]);
    }

    #[test]
    fn test_parse_mapping_response() {
        let json = serde_json::json!({
            "test-index": {
                "mappings": {
                    "properties": {
                        "name": {"type": "text"},
                        "age": {"type": "integer"},
                        "address": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "keyword"},
                                "zip": {"type": "keyword"}
                            }
                        }
                    }
                }
            }
        });

        let result = parse_mapping_response(&json, "test-index").unwrap();
        assert_eq!(result.columns, vec!["field", "type"]);
        let field_names: Vec<&String> = result.rows.iter().map(|r| &r[0]).collect();
        assert!(field_names.contains(&&"name".to_string()));
        assert!(field_names.contains(&&"age".to_string()));
        assert!(field_names.contains(&&"address".to_string()));
        assert!(field_names.contains(&&"address.city".to_string()));
        assert!(field_names.contains(&&"address.zip".to_string()));
    }

    #[test]
    fn test_parse_mapping_response_fallback() {
        // Test with different index name in response (alias scenario)
        let json = serde_json::json!({
            "actual-index-name": {
                "mappings": {
                    "properties": {
                        "field1": {"type": "text"}
                    }
                }
            }
        });

        let result = parse_mapping_response(&json, "alias-name").unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "field1");
    }

    #[test]
    fn test_parse_es_error() {
        let body =
            r#"{"error":{"type":"parsing_exception","reason":"Unknown query"},"status":400}"#;
        let msg = parse_es_error(body, 400);
        assert!(msg.contains("parsing_exception"));
        assert!(msg.contains("Unknown query"));
    }

    #[test]
    fn test_parse_es_error_string() {
        let body = r#"{"error":"something went wrong"}"#;
        let msg = parse_es_error(body, 500);
        assert!(msg.contains("something went wrong"));
    }

    #[test]
    fn test_parse_es_error_plain_text() {
        let body = "Not Found";
        let msg = parse_es_error(body, 404);
        assert!(msg.contains("Not Found"));
        assert!(msg.contains("404"));
    }

    #[test]
    fn test_format_rest_response_json() {
        let text = r#"{"status":"green","cluster_name":"test"}"#;
        let formatted = format_rest_response(text);
        assert!(formatted.contains('\n'));
        assert!(formatted.contains("green"));
    }

    #[test]
    fn test_format_rest_response_plain() {
        let text = "Not JSON content";
        let formatted = format_rest_response(text);
        assert_eq!(formatted, text);
    }

    #[test]
    fn test_flatten_mapping_properties() {
        let properties: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "name": {"type": "text"},
                "nested": {
                    "type": "object",
                    "properties": {
                        "inner": {"type": "keyword"}
                    }
                }
            }"#,
        )
        .unwrap();

        let mut rows = Vec::new();
        flatten_mapping_properties(&properties, "", &mut rows);

        assert_eq!(rows.len(), 3);
        let field_names: Vec<&String> = rows.iter().map(|r| &r[0]).collect();
        assert!(field_names.contains(&&"name".to_string()));
        assert!(field_names.contains(&&"nested".to_string()));
        assert!(field_names.contains(&&"nested.inner".to_string()));
    }

    #[test]
    fn test_parse_rest_call_method_path() {
        if let InputType::RestCall {
            method, path, body, ..
        } = parse_rest_call("GET /_cluster/health")
        {
            assert_eq!(method, "GET");
            assert_eq!(path, "/_cluster/health");
            assert!(body.is_none());
        } else {
            panic!("Expected RestCall");
        }
    }

    #[test]
    fn test_parse_rest_call_with_json_body() {
        if let InputType::RestCall {
            method, path, body, ..
        } = parse_rest_call("PUT /my-index {\"settings\": {\"number_of_shards\": 1}}")
        {
            assert_eq!(method, "PUT");
            assert_eq!(path, "/my-index");
            assert!(body.is_some());
            let b = body.unwrap();
            assert_eq!(b["settings"]["number_of_shards"], 1);
        } else {
            panic!("Expected RestCall");
        }
    }
}
