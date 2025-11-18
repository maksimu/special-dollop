use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// PostgreSQL protocol handler
pub struct PostgreSqlHandler {
    #[allow(dead_code)]
    config: PostgreSqlConfig,
}

#[derive(Debug, Clone)]
pub struct PostgreSqlConfig {
    pub default_port: u16,
}

impl Default for PostgreSqlConfig {
    fn default() -> Self {
        Self { default_port: 5432 }
    }
}

impl PostgreSqlHandler {
    pub fn new(config: PostgreSqlConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(PostgreSqlConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for PostgreSqlHandler {
    fn name(&self) -> &str {
        "postgresql"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        _to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("PostgreSQL handler starting");

        let _hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        // TODO: Full PostgreSQL implementation with sqlx
        // Similar to MySQL but with PostgreSQL-specific features

        let mut terminal = SqlTerminal::new(24, 80, "postgres=# ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line("Connected to PostgreSQL server.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Event loop
        while from_client.recv().await.is_some() {}

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
    fn test_postgresql_handler_new() {
        let handler = PostgreSqlHandler::with_defaults();
        assert_eq!(handler.name(), "postgresql");
    }
}
