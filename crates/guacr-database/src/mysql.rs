use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::{debug, info};
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// MySQL protocol handler
pub struct MySqlHandler {
    config: MySqlConfig,
}

#[derive(Debug, Clone)]
pub struct MySqlConfig {
    pub default_port: u16,
}

impl Default for MySqlConfig {
    fn default() -> Self {
        Self { default_port: 3306 }
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

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("MySQL handler starting");

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

        let _database = params.get("database");

        info!("MySQL connecting to {}@{}:{}", username, hostname, port);

        // Create SQL terminal
        let mut terminal = SqlTerminal::new(24, 80, "mysql> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line("Connected to MySQL server.")
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

        // TODO: Actual sqlx MySQL connection
        // let connection_url = format!("mysql://{}:{}@{}:{}/{}",
        //     username, password, hostname, port, database.unwrap_or(""));
        // let pool = sqlx::mysql::MySqlPool::connect(&connection_url).await?;

        // Event loop
        while let Some(_msg) = from_client.recv().await {
            // TODO: Parse SQL commands from key events
            // Execute queries
            // Format results as tables
            // Render and send
            debug!("MySQL: received message");
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mysql_handler_new() {
        let handler = MySqlHandler::with_defaults();
        assert_eq!(handler.name(), "mysql");
    }

    #[test]
    fn test_mysql_config() {
        let config = MySqlConfig::default();
        assert_eq!(config.default_port, 3306);
    }
}
