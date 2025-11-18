// Integration helpers for connecting protocol handlers with keeper-webrtc Channel

use bytes::Bytes;
use log::{debug, error, info};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{ProtocolHandlerRegistry, Result};

/// Handle a guacd conversation using protocol handlers instead of external TCP connection
///
/// This function is called when a Channel receives a guacd session type
/// (SSH, RDP, VNC, Telnet, Http, Database). It:
/// 1. Extracts the conversation type and parameters
/// 2. Looks up the appropriate protocol handler from the registry
/// 3. Creates bidirectional channels for Guacamole protocol messages
/// 4. Spawns the protocol handler task
/// 5. Forwards messages between WebRTC channel and protocol handler
///
/// # Arguments
///
/// * `conversation_type` - The specific protocol (ssh, rdp, vnc, etc.)
/// * `params` - Connection parameters (hostname, port, username, password, etc.)
/// * `registry` - Protocol handler registry
/// * `to_webrtc` - Channel to send messages to WebRTC client
/// * `from_webrtc` - Channel to receive messages from WebRTC client
///
/// # Returns
///
/// Ok(()) when the session completes gracefully, Err if connection fails
pub async fn handle_guacd_with_handlers(
    conversation_type: String,
    params: HashMap<String, String>,
    registry: Arc<ProtocolHandlerRegistry>,
    to_webrtc: mpsc::Sender<Bytes>,
    mut from_webrtc: mpsc::Receiver<Bytes>,
) -> Result<()> {
    info!(
        "Starting guacd session with handlers: protocol={}, params_count={}",
        conversation_type,
        params.len()
    );

    // Look up protocol handler
    let handler = registry.get(&conversation_type).ok_or_else(|| {
        error!("No handler registered for protocol: {}", conversation_type);
        crate::error::HandlerError::Unsupported(format!(
            "Protocol '{}' not supported - no handler registered",
            conversation_type
        ))
    })?;

    debug!("Found handler for protocol: {}", conversation_type);

    // Create channels for handler communication
    let (handler_to_client, mut handler_rx) = mpsc::channel::<Bytes>(128);
    let (webrtc_tx, handler_from_client) = mpsc::channel::<Bytes>(128);

    // Spawn protocol handler task
    let handler_clone = Arc::clone(&handler);
    let params_clone = params.clone();
    let protocol_name = conversation_type.clone();

    let handler_task = tokio::spawn(async move {
        debug!("Protocol handler task started: {}", protocol_name);
        match handler_clone
            .connect(params_clone, handler_to_client, handler_from_client)
            .await
        {
            Ok(()) => {
                info!("Protocol handler completed successfully: {}", protocol_name);
                Ok(())
            }
            Err(e) => {
                error!("Protocol handler failed: {} - {}", protocol_name, e);
                Err(e)
            }
        }
    });

    // Bidirectional forwarding
    let forward_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Handler -> WebRTC
                Some(msg) = handler_rx.recv() => {
                    if let Err(e) = to_webrtc.send(msg).await {
                        error!("Failed to send to WebRTC: {}", e);
                        break;
                    }
                }

                // WebRTC -> Handler
                Some(msg) = from_webrtc.recv() => {
                    if let Err(e) = webrtc_tx.send(msg).await {
                        error!("Failed to send to handler: {}", e);
                        break;
                    }
                }

                // Both channels closed
                else => {
                    debug!("Both channels closed, ending forwarding");
                    break;
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    // Wait for tasks to complete
    let (handler_result, forward_result) = tokio::join!(handler_task, forward_task);

    match handler_result {
        Ok(Ok(())) => debug!("Handler task completed successfully"),
        Ok(Err(e)) => error!("Handler task failed: {}", e),
        Err(e) => error!("Handler task panicked: {}", e),
    }

    match forward_result {
        Ok(Ok(())) => debug!("Forwarding task completed successfully"),
        Ok(Err(e)) => error!("Forwarding task failed: {}", e),
        Err(e) => error!("Forwarding task panicked: {}", e),
    }

    info!("Guacd session ended: {}", conversation_type);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProtocolHandler;

    #[tokio::test]
    async fn test_handle_guacd_unsupported_protocol() {
        let registry = Arc::new(ProtocolHandlerRegistry::new());
        // Don't register any handlers

        let (to_webrtc, _webrtc_rx) = mpsc::channel(10);
        let (_webrtc_tx, from_webrtc) = mpsc::channel(10);

        let result = handle_guacd_with_handlers(
            "ssh".to_string(),
            HashMap::new(),
            registry,
            to_webrtc,
            from_webrtc,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not supported"));
    }

    #[test]
    fn test_registry_lookup() {
        let registry = Arc::new(ProtocolHandlerRegistry::new());
        registry.register(MockProtocolHandler::new("ssh"));
        registry.register(MockProtocolHandler::new("rdp"));

        assert!(registry.has("ssh"));
        assert!(registry.has("rdp"));
        assert!(!registry.has("vnc"));

        let ssh_handler = registry.get("ssh");
        assert!(ssh_handler.is_some());
        assert_eq!(ssh_handler.unwrap().name(), "ssh");
    }
}
