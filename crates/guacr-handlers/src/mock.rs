use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::handler::{HandlerStats, HealthStatus, ProtocolHandler};

/// Mock protocol handler for testing
///
/// Useful for testing the handler registry and integration points without
/// needing actual protocol implementations.
pub struct MockProtocolHandler {
    name: String,
    connect_count: Arc<AtomicU64>,
    disconnect_count: Arc<AtomicU64>,
    health_status: Arc<parking_lot::RwLock<HealthStatus>>,
}

impl MockProtocolHandler {
    /// Create a new mock handler with the given protocol name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            connect_count: Arc::new(AtomicU64::new(0)),
            disconnect_count: Arc::new(AtomicU64::new(0)),
            health_status: Arc::new(parking_lot::RwLock::new(HealthStatus::Healthy)),
        }
    }

    /// Get the number of times connect was called
    pub fn connect_count(&self) -> u64 {
        self.connect_count.load(Ordering::SeqCst)
    }

    /// Get the number of times disconnect was called
    pub fn disconnect_count(&self) -> u64 {
        self.disconnect_count.load(Ordering::SeqCst)
    }

    /// Set the health status this mock will report
    pub fn set_health(&self, status: HealthStatus) {
        *self.health_status.write() = status;
    }
}

#[async_trait]
impl ProtocolHandler for MockProtocolHandler {
    fn name(&self) -> &str {
        &self.name
    }

    async fn connect(
        &self,
        _params: HashMap<String, String>,
        _to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<()> {
        self.connect_count.fetch_add(1, Ordering::SeqCst);

        // Simply consume messages until channel closes
        while from_client.recv().await.is_some() {
            // Echo or ignore messages
        }

        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        self.disconnect_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn health_check(&self) -> Result<HealthStatus> {
        Ok(self.health_status.read().clone())
    }

    async fn stats(&self) -> Result<HandlerStats> {
        Ok(HandlerStats {
            active_connections: 0,
            total_connections: self.connect_count.load(Ordering::SeqCst),
            bytes_sent: 0,
            bytes_received: 0,
            errors: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_handler_name() {
        let handler = MockProtocolHandler::new("test-protocol");
        assert_eq!(handler.name(), "test-protocol");
    }

    #[tokio::test]
    async fn test_mock_handler_connect() {
        let handler = MockProtocolHandler::new("ssh");
        let (to_client, _rx) = mpsc::channel(10);
        let (tx, from_client) = mpsc::channel(10);

        assert_eq!(handler.connect_count(), 0);

        // Spawn connect task
        let handler_clone = Arc::new(handler);
        let handler_ref = Arc::clone(&handler_clone);
        let connect_task = tokio::spawn(async move {
            handler_ref
                .connect(HashMap::new(), to_client, from_client)
                .await
        });

        // Wait a bit
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Close channel
        drop(tx);

        // Wait for connect to finish
        connect_task.await.unwrap().unwrap();

        assert_eq!(handler_clone.connect_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_handler_health() {
        let handler = MockProtocolHandler::new("test");

        let status = handler.health_check().await.unwrap();
        assert_eq!(status, HealthStatus::Healthy);

        handler.set_health(HealthStatus::Degraded {
            reason: "test degradation".to_string(),
        });

        let status = handler.health_check().await.unwrap();
        assert!(matches!(status, HealthStatus::Degraded { .. }));
    }

    #[tokio::test]
    async fn test_mock_handler_stats() {
        let handler = Arc::new(MockProtocolHandler::new("test"));
        let (to_client, _rx) = mpsc::channel(10);
        let (tx, from_client) = mpsc::channel(10);

        let handler_ref = Arc::clone(&handler);
        let connect_task = tokio::spawn(async move {
            handler_ref
                .connect(HashMap::new(), to_client, from_client)
                .await
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        drop(tx);
        connect_task.await.unwrap().unwrap();

        let stats = handler.stats().await.unwrap();
        assert_eq!(stats.total_connections, 1);
    }
}
