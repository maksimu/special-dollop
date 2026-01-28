use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::events::EventBasedHandler;

/// Argument descriptor for protocol handler parameters
///
/// Used by handlers to declare what connection parameters they accept,
/// enabling guacd wire protocol compatibility.
#[cfg(feature = "guacd-compat")]
#[derive(Debug, Clone)]
pub struct HandlerArg {
    /// Argument name (e.g., "hostname", "port", "username")
    pub name: &'static str,
    /// Whether this argument is required
    pub required: bool,
    /// Human-readable description
    pub description: &'static str,
}

#[cfg(feature = "guacd-compat")]
impl HandlerArg {
    /// Create a required argument descriptor
    pub const fn required(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            required: true,
            description,
        }
    }

    /// Create an optional argument descriptor
    pub const fn optional(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            required: false,
            description,
        }
    }
}

/// Health status of a protocol handler
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Unhealthy { reason: String },
}

/// Statistics for a protocol handler
#[derive(Debug, Clone, Default)]
pub struct HandlerStats {
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub errors: u64,
}

/// Protocol handler trait
///
/// All remote desktop protocol implementations (SSH, RDP, VNC, Database, RBI)
/// must implement this trait. Handlers receive connection parameters and
/// bidirectional message channels for communicating with the client via WebRTC.
///
/// # Message Format
///
/// Messages are Guacamole protocol instructions in raw bytes format.
/// Use the GuacdParser from keeper-webrtc for parsing and formatting.
///
/// # Example
///
/// ```no_run
/// use guacr_handlers::{ProtocolHandler, Result, HealthStatus};
/// use async_trait::async_trait;
/// use std::collections::HashMap;
/// use tokio::sync::mpsc;
/// use bytes::Bytes;
///
/// struct MyHandler;
///
/// #[async_trait]
/// impl ProtocolHandler for MyHandler {
///     fn name(&self) -> &str {
///         "my-protocol"
///     }
///
///     async fn connect(
///         &self,
///         params: HashMap<String, String>,
///         to_client: mpsc::Sender<Bytes>,
///         mut from_client: mpsc::Receiver<Bytes>,
///     ) -> Result<()> {
///         // Implementation here
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol name (e.g., "ssh", "rdp", "vnc", "mysql", "http")
    ///
    /// This should match the ConversationType string from the WebRTC settings.
    fn name(&self) -> &str;

    /// Connect to the remote host and handle the session
    ///
    /// # Arguments
    ///
    /// * `params` - Connection parameters (hostname, port, username, etc.)
    /// * `to_client` - Channel to send Guacamole protocol messages to client
    /// * `from_client` - Channel to receive Guacamole protocol messages from client
    ///
    /// # Message Format
    ///
    /// Messages are raw bytes containing Guacamole protocol instructions.
    /// Parse with GuacdParser::parse_instruction() from keeper-webrtc crate.
    ///
    /// Common opcodes to handle:
    /// - "key" - Keyboard input
    /// - "mouse" - Mouse movement/clicks
    /// - "clipboard" - Clipboard data
    /// - "size" - Screen size change
    ///
    /// Common opcodes to send:
    /// - "img" - Image data (PNG encoded)
    /// - "sync" - Frame synchronization
    /// - "audio" - Audio data
    /// - "clipboard" - Clipboard data
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails, authentication fails, or the
    /// session encounters an unrecoverable error.
    ///
    /// # Cancellation
    ///
    /// The handler should gracefully exit when from_client channel closes.
    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<()>;

    /// Graceful disconnect (optional, cleanup on drop)
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    /// Health check for this handler
    async fn health_check(&self) -> Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    /// Get handler statistics
    async fn stats(&self) -> Result<HandlerStats> {
        Ok(HandlerStats::default())
    }

    /// Initialize handler with configuration (optional)
    async fn initialize(&self, _config: HashMap<String, String>) -> Result<()> {
        Ok(())
    }

    /// Get as event-based handler if supported
    ///
    /// Returns a reference to the EventBasedHandler trait if this handler
    /// implements it, allowing zero-copy integration with keeper-pam-webrtc-rs.
    /// Returns None if the handler only supports the channel-based interface.
    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        None
    }

    /// Get the list of arguments this handler accepts
    ///
    /// Used for guacd wire protocol compatibility. Returns the argument
    /// descriptors that will be sent in the "args" instruction during
    /// the Guacamole handshake.
    ///
    /// The default implementation returns None, which means the handler
    /// uses the standard argument set from the handshake module.
    /// Handlers can override this to provide custom arguments.
    #[cfg(feature = "guacd-compat")]
    fn required_args(&self) -> Option<&'static [HandlerArg]> {
        None
    }
}
