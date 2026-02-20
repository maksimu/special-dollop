//! Metrics Handle for RAII-based automatic cleanup
//!
//! MetricsHandle provides automatic registration/unregistration from METRICS_COLLECTOR.
//! When dropped, it automatically unregisters the connection, preventing leaks.

use super::METRICS_COLLECTOR;
use log::debug;

/// RAII handle for metrics registration
/// Automatically unregisters connection when dropped
#[derive(Debug)]
pub struct MetricsHandle {
    conversation_id: String,
    tube_id: String,
}

impl MetricsHandle {
    /// Create new metrics handle and register with collector
    pub fn new(conversation_id: String, tube_id: String) -> Self {
        // Register on creation
        METRICS_COLLECTOR.register_connection(conversation_id.clone(), tube_id.clone());
        debug!(
            "Metrics auto-registered (conversation_id: {}, tube_id: {})",
            conversation_id, tube_id
        );

        Self {
            conversation_id,
            tube_id,
        }
    }

    // NOTE: Getters deleted - conversation_id and tube_id only used internally by Drop
}

// Why no getters?
// - These fields are only needed for Drop to unregister metrics
// - External code doesn't need to query them
// - Tube already has original_conversation_id if needed
// - Keep it simple!

impl Drop for MetricsHandle {
    fn drop(&mut self) {
        // Auto-unregister on drop
        METRICS_COLLECTOR.unregister_connection(&self.conversation_id);
        debug!(
            "Metrics auto-unregistered via Drop (conversation_id: {}, tube_id: {})",
            self.conversation_id, self.tube_id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_handle_drop() {
        // When handle drops, should auto-unregister
        {
            let _handle = MetricsHandle::new("test_conv".to_string(), "test_tube".to_string());
            // Handle created and registered
        } // ← Handle drops here, auto-unregisters

        // Verify no panic occurred
    }
}
