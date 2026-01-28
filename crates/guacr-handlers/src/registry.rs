use crate::handler::ProtocolHandler;
use dashmap::DashMap;
use log::{debug, warn};
use std::sync::Arc;

/// Thread-safe registry for protocol handlers
///
/// Uses DashMap for lock-free concurrent access. Handlers can be registered
/// at startup and retrieved during connection handling without locks.
///
/// # Example
///
/// ```no_run
/// use guacr_handlers::ProtocolHandlerRegistry;
///
/// let registry = ProtocolHandlerRegistry::new();
/// // registry.register(SshHandler::new());
/// // registry.register(RdpHandler::new());
///
/// // Later, during connection:
/// if let Some(handler) = registry.get("ssh") {
///     // handler.connect(...).await?;
/// }
/// ```
pub struct ProtocolHandlerRegistry {
    handlers: DashMap<String, Arc<dyn ProtocolHandler>>,
}

impl ProtocolHandlerRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        debug!("Creating new ProtocolHandlerRegistry");
        Self {
            handlers: DashMap::new(),
        }
    }

    /// Register a protocol handler
    ///
    /// # Arguments
    ///
    /// * `handler` - The protocol handler implementation
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use guacr_handlers::ProtocolHandlerRegistry;
    /// let registry = ProtocolHandlerRegistry::new();
    /// // registry.register(SshHandler::new());
    /// ```
    pub fn register<H: ProtocolHandler + 'static>(&self, handler: H) {
        let name = handler.name().to_string();
        debug!("Registering protocol handler: {}", name);
        self.handlers.insert(name, Arc::new(handler));
    }

    /// Get a protocol handler by name
    ///
    /// Returns None if no handler is registered for the given protocol.
    ///
    /// # Arguments
    ///
    /// * `protocol` - Protocol name (e.g., "ssh", "rdp", "vnc")
    ///
    /// # Returns
    ///
    /// An Arc to the handler if found, None otherwise.
    pub fn get(&self, protocol: &str) -> Option<Arc<dyn ProtocolHandler>> {
        self.handlers
            .get(protocol)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// List all registered protocol names
    ///
    /// # Returns
    ///
    /// Vector of protocol names currently registered
    pub fn list(&self) -> Vec<String> {
        self.handlers
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Check if a protocol is registered
    ///
    /// # Arguments
    ///
    /// * `protocol` - Protocol name to check
    ///
    /// # Returns
    ///
    /// true if the protocol has a registered handler
    pub fn has(&self, protocol: &str) -> bool {
        self.handlers.contains_key(protocol)
    }

    /// Remove a protocol handler
    ///
    /// # Arguments
    ///
    /// * `protocol` - Protocol name to remove
    ///
    /// # Returns
    ///
    /// true if handler was removed, false if not found
    pub fn unregister(&self, protocol: &str) -> bool {
        match self.handlers.remove(protocol) {
            Some(_) => {
                debug!("Unregistered protocol handler: {}", protocol);
                true
            }
            None => {
                warn!("Attempted to unregister non-existent handler: {}", protocol);
                false
            }
        }
    }

    /// Get count of registered handlers
    pub fn count(&self) -> usize {
        self.handlers.len()
    }
}

impl Default for ProtocolHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockProtocolHandler;

    #[test]
    fn test_registry_new() {
        let registry = ProtocolHandlerRegistry::new();
        assert_eq!(registry.count(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let registry = ProtocolHandlerRegistry::new();
        let handler = MockProtocolHandler::new("ssh");

        registry.register(handler);

        assert_eq!(registry.count(), 1);
        assert!(registry.has("ssh"));
        assert!(registry.get("ssh").is_some());
        assert!(registry.get("rdp").is_none());
    }

    #[test]
    fn test_list() {
        let registry = ProtocolHandlerRegistry::new();
        registry.register(MockProtocolHandler::new("ssh"));
        registry.register(MockProtocolHandler::new("rdp"));
        registry.register(MockProtocolHandler::new("vnc"));

        let mut protocols = registry.list();
        protocols.sort();

        assert_eq!(protocols, vec!["rdp", "ssh", "vnc"]);
    }

    #[test]
    fn test_unregister() {
        let registry = ProtocolHandlerRegistry::new();
        registry.register(MockProtocolHandler::new("ssh"));

        assert_eq!(registry.count(), 1);
        assert!(registry.unregister("ssh"));
        assert_eq!(registry.count(), 0);
        assert!(!registry.unregister("ssh")); // Already removed
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;

        let registry = Arc::new(ProtocolHandlerRegistry::new());
        registry.register(MockProtocolHandler::new("ssh"));

        // Simulate concurrent access
        let registry_clone = Arc::clone(&registry);
        let handle = std::thread::spawn(move || {
            for _ in 0..1000 {
                assert!(registry_clone.get("ssh").is_some());
            }
        });

        for _ in 0..1000 {
            assert!(registry.get("ssh").is_some());
        }

        handle.join().unwrap();
        assert_eq!(registry.count(), 1);
    }
}
