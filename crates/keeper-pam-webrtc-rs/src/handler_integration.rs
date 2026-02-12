// Integration layer for guacr protocol handlers
//
// This module provides the glue between keeper-webrtc Channel and guacr protocol handlers,
// allowing direct handler invocation instead of connecting to external guacd server.

use guacr_handlers::{handle_guacd_with_handlers, ProtocolHandlerRegistry};

use crate::models::ConversationType;
use anyhow::Result;
use bytes::Bytes;
use log::info;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Initialize the global protocol handler registry
///
/// Delegates to `guacr::create_default_registry()` which registers all handlers
/// enabled via guacr feature flags.
pub fn create_handler_registry() -> Arc<ProtocolHandlerRegistry> {
    guacr::create_default_registry()
}

/// Invoke a protocol handler for a guacd session
///
/// This function replaces the external guacd TCP connection with a direct
/// handler invocation.
///
/// # Arguments
///
/// * `conversation_type` - The protocol type (SSH, RDP, VNC, etc.)
/// * `params` - Connection parameters (hostname, port, username, etc.)
/// * `registry` - The protocol handler registry
/// * `to_webrtc` - Channel to send messages to WebRTC client
/// * `from_webrtc` - Channel to receive messages from WebRTC client
pub async fn invoke_handler(
    conversation_type: &ConversationType,
    params: HashMap<String, String>,
    registry: Arc<ProtocolHandlerRegistry>,
    to_webrtc: mpsc::Sender<Bytes>,
    from_webrtc: mpsc::Receiver<Bytes>,
) -> Result<()> {
    let protocol_name = conversation_type.to_string();

    info!("Invoking built-in handler for protocol: {}", protocol_name);

    handle_guacd_with_handlers(protocol_name, params, registry, to_webrtc, from_webrtc)
        .await
        .map_err(|e| anyhow::anyhow!("Handler error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_handler_registry() {
        let registry = create_handler_registry();
        assert!(registry.count() >= 2); // At least SSH and Telnet
        assert!(registry.has("ssh"));
        assert!(registry.has("telnet"));
    }
}
