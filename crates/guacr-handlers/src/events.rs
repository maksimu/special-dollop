// Event-based protocol handler interface
// Zero-copy integration with keeper-pam-webrtc-rs

use bytes::Bytes;
use std::sync::Arc;

/// Protocol handler events (zero-copy)
///
/// Events are sent from protocol handlers to the WebRTC layer.
/// Uses Bytes for zero-copy data transfer.
#[derive(Debug, Clone)]
pub enum HandlerEvent {
    /// Send instruction to client (zero-copy Bytes)
    Instruction(Bytes),

    /// Error occurred - terminate connection
    Error { message: String, code: Option<i32> },

    /// Disconnect request
    Disconnect { reason: String },

    /// Threat detected - terminate session
    ThreatDetected { level: String, description: String },

    /// Connection ready
    Ready { connection_id: String },

    /// Size change (backwards compatibility, not needed with threat detection)
    Size { width: u32, height: u32 },
}

/// Event callback trait for WebRTC integration
///
/// This allows keeper-pam-webrtc-rs to register callbacks for events
/// instead of parsing all instructions. Only critical events (error,
/// disconnect, threat) need to be handled by WebRTC layer.
pub trait EventCallback: Send + Sync {
    /// Handle a protocol handler event
    fn on_event(&self, event: HandlerEvent);

    /// Send instruction to client (zero-copy)
    ///
    /// This is the hot path - Bytes is reference-counted, so no copying happens.
    /// The WebRTC layer should send the Bytes directly to the client.
    fn send_instruction(&self, instruction: Bytes);
}

/// Protocol handler with event-based interface
///
/// This is an alternative to the channel-based interface that allows
/// zero-copy integration with keeper-pam-webrtc-rs.
///
/// For bidirectional communication, the handler receives a channel receiver
/// for client messages (keyboard, mouse, etc.) as a parameter.
#[async_trait::async_trait]
pub trait EventBasedHandler: Send + Sync {
    /// Protocol name
    fn name(&self) -> &str;

    /// Connect with event callback and client message receiver
    ///
    /// # Arguments
    ///
    /// * `params` - Connection parameters (hostname, port, username, etc.)
    /// * `callback` - Callback for sending instructions to client (handler → WebRTC)
    /// * `from_client` - Channel receiver for client messages (WebRTC → handler)
    ///
    /// The handler calls the callback to send instructions to the client (zero-copy).
    /// The handler receives client messages (keyboard, mouse, etc.) from the channel.
    async fn connect_with_events(
        &self,
        params: std::collections::HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: tokio::sync::mpsc::Receiver<Bytes>,
    ) -> Result<(), crate::error::HandlerError>;
}

/// Zero-copy instruction sender
///
/// Allows handlers to send instructions directly without copying.
pub struct InstructionSender {
    callback: Arc<dyn EventCallback>,
}

impl InstructionSender {
    pub fn new(callback: Arc<dyn EventCallback>) -> Self {
        Self { callback }
    }

    /// Send instruction (zero-copy)
    ///
    /// Bytes is reference-counted, so this is zero-copy.
    pub fn send(&self, instruction: Bytes) {
        self.callback.send_instruction(instruction);
    }

    /// Send error event
    pub fn send_error(&self, message: String, code: Option<i32>) {
        self.callback
            .on_event(HandlerEvent::Error { message, code });
    }

    /// Send disconnect event
    pub fn send_disconnect(&self, reason: String) {
        self.callback.on_event(HandlerEvent::Disconnect { reason });
    }

    /// Send threat detected event
    pub fn send_threat(&self, level: String, description: String) {
        self.callback
            .on_event(HandlerEvent::ThreatDetected { level, description });
    }

    /// Send ready event
    pub fn send_ready(&self, connection_id: String) {
        self.callback
            .on_event(HandlerEvent::Ready { connection_id });
    }

    /// Send size event (backwards compatibility)
    pub fn send_size(&self, width: u32, height: u32) {
        self.callback.on_event(HandlerEvent::Size { width, height });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestCallback {
        events: Arc<Mutex<Vec<HandlerEvent>>>,
        instructions: Arc<Mutex<Vec<Bytes>>>,
    }

    impl EventCallback for TestCallback {
        fn on_event(&self, event: HandlerEvent) {
            self.events.lock().unwrap().push(event);
        }

        fn send_instruction(&self, instruction: Bytes) {
            self.instructions.lock().unwrap().push(instruction);
        }
    }

    #[test]
    fn test_event_callback() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let instructions = Arc::new(Mutex::new(Vec::new()));

        let callback = Arc::new(TestCallback {
            events: events.clone(),
            instructions: instructions.clone(),
        });

        let sender = InstructionSender::new(callback.clone());

        // Send instruction
        sender.send(Bytes::from("test"));
        assert_eq!(instructions.lock().unwrap().len(), 1);

        // Send error
        sender.send_error("test error".to_string(), Some(1));
        assert_eq!(events.lock().unwrap().len(), 1);

        // Send threat
        sender.send_threat("critical".to_string(), "threat detected".to_string());
        assert_eq!(events.lock().unwrap().len(), 2);
    }
}
