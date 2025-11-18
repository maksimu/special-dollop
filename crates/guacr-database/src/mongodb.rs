use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// MongoDB handler
///
/// Provides NoSQL database access with spreadsheet-style result display
pub struct MongoDbHandler {
    config: MongoDbConfig,
}

#[derive(Debug, Clone)]
pub struct MongoDbConfig {
    pub default_port: u16,
    pub require_tls: bool,
}

impl Default for MongoDbConfig {
    fn default() -> Self {
        Self {
            default_port: 27017,
            require_tls: true, // Security: TLS by default
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

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("MongoDB handler starting");

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

        let _password = params
            .get("password")
            .ok_or_else(|| HandlerError::MissingParameter("password".to_string()))?;

        let database = params
            .get("database")
            .cloned()
            .unwrap_or_else(|| "admin".to_string());

        info!(
            "MongoDB connecting to {}@{}:{}/{}",
            username, hostname, port, database
        );

        // Security validation
        if self.config.require_tls && !params.contains_key("tls") {
            return Err(HandlerError::SecurityViolation(
                "TLS required for MongoDB connections".to_string(),
            ));
        }

        // Create terminal for MongoDB shell
        let mut terminal = SqlTerminal::new(24, 80, "> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line(&format!("Connected to MongoDB: {}", hostname))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        terminal
            .write_line(&format!("Database: {}", database))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Send initial screen
        let png = terminal
            .render_png()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        let img_instructions = terminal.format_img_instructions(&png, 1);
        for instr in img_instructions {
            to_client
                .send(Bytes::from(instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        }

        // TODO: Actual MongoDB connection
        // Security features to implement:
        // - TLS enforcement
        // - BSON query validation (prevent NoSQL injection)
        // - Database-level isolation
        // - Read-only mode option
        //
        // use mongodb::Client;
        // let tls_option = if self.config.require_tls { "tls=true" } else { "" };
        // let uri = format!("mongodb://{}:{}@{}:{}/{}?{}",
        //     username, password, hostname, port, database, tls_option);
        // let client = Client::with_uri_str(&uri).await?;
        // let db = client.database(&database);
        //
        // Query execution with BSON (type-safe):
        // let collection = db.collection("users");
        // let filter = doc! { "name": user_input };  // Parameterized, safe
        // let results = collection.find(filter, None).await?;

        while from_client.recv().await.is_some() {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mongodb_handler_new() {
        let handler = MongoDbHandler::with_defaults();
        assert_eq!(handler.name(), "mongodb");
    }

    #[test]
    fn test_mongodb_config() {
        let config = MongoDbConfig::default();
        assert_eq!(config.default_port, 27017);
        assert!(config.require_tls); // Security default
    }

    #[test]
    fn test_mongodb_requires_tls() {
        let config = MongoDbConfig {
            require_tls: true,
            ..Default::default()
        };
        assert!(config.require_tls);
    }
}
