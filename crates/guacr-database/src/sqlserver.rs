use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// SQL Server protocol handler
pub struct SqlServerHandler {
    #[allow(dead_code)]
    config: SqlServerConfig,
}

#[derive(Debug, Clone)]
pub struct SqlServerConfig {
    pub default_port: u16,
}

impl Default for SqlServerConfig {
    fn default() -> Self {
        Self { default_port: 1433 }
    }
}

impl SqlServerHandler {
    pub fn new(config: SqlServerConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(SqlServerConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for SqlServerHandler {
    fn name(&self) -> &str {
        "sqlserver"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        _to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("SQL Server handler starting");

        let _hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        // TODO: Full SQL Server implementation with tiberius
        let mut terminal = SqlTerminal::new(24, 80, "1> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line("Connected to SQL Server.")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

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
    fn test_sqlserver_handler_new() {
        let handler = SqlServerHandler::with_defaults();
        assert_eq!(handler.name(), "sqlserver");
    }
}
