// Example: Integration with keeper-pam-webrtc-rs
// Shows how to implement EventCallback for zero-copy integration

use guacr_handlers::{
    EventCallback, EventBasedHandler, HandlerEvent, InstructionSender,
    ProtocolHandlerRegistry,
};
use std::collections::HashMap;
use std::sync::Arc;
use bytes::Bytes;
use tokio::sync::mpsc;

// Example WebRTC data channel trait (adapt to your actual type)
pub trait DataChannelTrait: Send + Sync {
    fn send(&self, data: Bytes) -> Result<(), String>;
    fn close(&self);
}

// Implement EventCallback for WebRTC
pub struct WebRtcEventCallback {
    data_channel: Arc<dyn DataChannelTrait>,
}

impl WebRtcEventCallback {
    pub fn new(data_channel: Arc<dyn DataChannelTrait>) -> Self {
        Self { data_channel }
    }
}

impl EventCallback for WebRtcEventCallback {
    fn on_event(&self, event: HandlerEvent) {
        match event {
            HandlerEvent::Error { message, code } => {
                log::error!("Protocol error: {} (code: {:?})", message, code);
                // Close WebRTC connection
                self.data_channel.close();
            }
            HandlerEvent::Disconnect { reason } => {
                log::info!("Protocol disconnect: {}", reason);
                // Close WebRTC connection gracefully
                self.data_channel.close();
            }
            HandlerEvent::ThreatDetected { level, description } => {
                log::error!("Threat detected [{}]: {}", level, description);
                // Immediately close connection
                self.data_channel.close();
            }
            HandlerEvent::Ready { connection_id } => {
                log::info!("Connection ready: {}", connection_id);
                // Connection established, can start sending data
            }
            HandlerEvent::Size { width, height } => {
                log::debug!("Size change: {}x{}", width, height);
                // Optional: Handle size change (not needed with threat detection)
            }
            HandlerEvent::Instruction(bytes) => {
                // This shouldn't happen - use send_instruction instead
                self.send_instruction(bytes);
            }
        }
    }
    
    fn send_instruction(&self, instruction: Bytes) {
        // Zero-copy send - Bytes is reference-counted
        // This is the hot path - no copying happens
        if let Err(e) = self.data_channel.send(instruction) {
            log::error!("Failed to send instruction: {}", e);
        }
    }
}

// Example: Connect protocol handler with event-based interface
pub async fn connect_with_events(
    conversation_type: &str,
    params: HashMap<String, String>,
    registry: Arc<ProtocolHandlerRegistry>,
    data_channel: Arc<dyn DataChannelTrait>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Get handler
    let handler = registry.get(conversation_type)
        .ok_or_else(|| format!("No handler for {}", conversation_type))?;
    
    // Create event callback
    let callback = Arc::new(WebRtcEventCallback::new(data_channel));
    
    // Check if handler supports event-based interface
    // Note: This requires handlers to implement EventBasedHandler
    // For now, use channel-based interface as fallback
    
    // Fallback to channel-based interface (backwards compatible)
    let (to_webrtc, mut from_handler) = mpsc::channel::<Bytes>(128);
    let (to_handler, mut from_webrtc) = mpsc::channel::<Bytes>(128);
    
    // Spawn forwarding task
    let channel_clone = data_channel.clone();
    tokio::spawn(async move {
        while let Some(msg) = from_handler.recv().await {
            if let Err(e) = channel_clone.send(msg) {
                log::error!("Failed to forward message: {}", e);
                break;
            }
        }
    });
    
    // Use existing channel-based interface
    guacr_handlers::handle_guacd_with_handlers(
        conversation_type.to_string(),
        params,
        registry,
        to_webrtc,
        from_webrtc,
    ).await?;
    
    Ok(())
}

// Example: Channel-based wrapper that uses EventCallback
pub struct ChannelEventCallback {
    sender: mpsc::Sender<Bytes>,
}

impl ChannelEventCallback {
    pub fn new(sender: mpsc::Sender<Bytes>) -> Self {
        Self { sender }
    }
}

impl EventCallback for ChannelEventCallback {
    fn on_event(&self, event: HandlerEvent) {
        match event {
            HandlerEvent::Error { message, code } => {
                log::error!("Protocol error: {} (code: {:?})", message, code);
                // Could send error instruction to client
            }
            HandlerEvent::Disconnect { reason } => {
                log::info!("Protocol disconnect: {}", reason);
            }
            HandlerEvent::ThreatDetected { level, description } => {
                log::error!("Threat detected [{}]: {}", level, description);
            }
            HandlerEvent::Ready { connection_id } => {
                log::info!("Connection ready: {}", connection_id);
            }
            HandlerEvent::Size { width, height } => {
                log::debug!("Size change: {}x{}", width, height);
            }
            HandlerEvent::Instruction(bytes) => {
                self.send_instruction(bytes);
            }
        }
    }
    
    fn send_instruction(&self, instruction: Bytes) {
        // Send through channel (Bytes is still zero-copy, refcounted)
        if let Err(e) = self.sender.try_send(instruction) {
            log::error!("Failed to send instruction: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // Mock data channel for testing
    struct MockDataChannel {
        sent: Arc<std::sync::Mutex<Vec<Bytes>>>,
    }
    
    impl DataChannelTrait for MockDataChannel {
        fn send(&self, data: Bytes) -> Result<(), String> {
            self.sent.lock().unwrap().push(data);
            Ok(())
        }
        
        fn close(&self) {
            // Mock close
        }
    }
    
    #[test]
    fn test_webrtc_event_callback() {
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let channel = Arc::new(MockDataChannel {
            sent: sent.clone(),
        });
        
        let callback = WebRtcEventCallback::new(channel);
        
        // Send instruction
        callback.send_instruction(Bytes::from("test"));
        assert_eq!(sent.lock().unwrap().len(), 1);
        
        // Send error
        callback.on_event(HandlerEvent::Error {
            message: "test error".to_string(),
            code: Some(1),
        });
        // Channel should be closed (tested via close() call)
    }
}
