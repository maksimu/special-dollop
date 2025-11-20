use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use log::info;
use std::collections::HashMap;
use std::sync::Arc;
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

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
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

// Event-based handler implementation for zero-copy integration
#[async_trait]
impl EventBasedHandler for SqlServerHandler {
    fn name(&self) -> &str {
        "sqlserver"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Wrap the channel-based interface
        let (to_client, mut handler_rx) = mpsc::channel::<Bytes>(128);

        let sender = InstructionSender::new(callback);
        let sender_arc = Arc::new(sender);

        // Spawn task to forward channel messages to event callback (zero-copy)
        let sender_clone = Arc::clone(&sender_arc);
        tokio::spawn(async move {
            while let Some(msg) = handler_rx.recv().await {
                sender_clone.send(msg); // Zero-copy: Bytes is reference-counted
            }
        });

        // Call the existing channel-based connect method
        self.connect(params, to_client, from_client).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlserver_handler_new() {
        let handler = SqlServerHandler::with_defaults();
        assert_eq!(
            <_ as guacr_handlers::ProtocolHandler>::name(&handler),
            "sqlserver"
        );
    }
}
