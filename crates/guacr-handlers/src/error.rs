use bytes::Bytes;
use guacr_protocol::{
    format_error, STATUS_CLIENT_BAD_REQUEST, STATUS_CLIENT_FORBIDDEN, STATUS_CLIENT_UNAUTHORIZED,
};
use log::{error, warn};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Error, Debug)]
pub enum HandlerError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Missing parameter: {0}")]
    MissingParameter(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Channel error: {0}")]
    ChannelError(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Security violation: {0}")]
    SecurityViolation(String),

    #[error("Disconnected: {0}")]
    Disconnected(String),
}

pub type Result<T> = std::result::Result<T, HandlerError>;

// ============================================================================
// Error Handling Helpers (mimics guacd's guac_client_abort behavior)
// ============================================================================

/// Send error to client and log it, then return HandlerError for propagation
///
/// This mimics guacd's `guac_client_abort()` behavior:
/// 1. Logs the error to server logs (with appropriate level)
/// 2. Sends error instruction to client
/// 3. Returns HandlerError for propagation up the call stack
///
/// Use this for **fatal errors** that should terminate the connection.
///
/// # Arguments
/// * `to_client` - Channel to send error instruction
/// * `message` - Error message for user (shown in client UI)
/// * `status_code` - Guacamole protocol status code (use STATUS_* constants)
/// * `log_message` - Optional detailed message for server logs (defaults to message)
///
/// # Example
/// ```ignore
/// use guacr_handlers::{send_error_and_abort, STATUS_UPSTREAM_ERROR};
///
/// if let Err(e) = database.connect().await {
///     return Err(send_error_and_abort(
///         &to_client,
///         "Database connection failed",
///         STATUS_UPSTREAM_ERROR,
///         Some(&format!("MySQL error: {}", e))
///     ).await);
/// }
/// ```
pub async fn send_error_and_abort(
    to_client: &mpsc::Sender<Bytes>,
    message: &str,
    status_code: u32,
    log_message: Option<&str>,
) -> HandlerError {
    // Log to server (detailed message if provided)
    let log_msg = log_message.unwrap_or(message);

    // Use appropriate log level based on status code
    match status_code {
        STATUS_CLIENT_BAD_REQUEST | STATUS_CLIENT_UNAUTHORIZED | STATUS_CLIENT_FORBIDDEN => {
            warn!("Client error ({}): {}", status_code, log_msg);
        }
        _ => {
            error!("Handler error ({}): {}", status_code, log_msg);
        }
    }

    // Send error instruction to client
    let error_instr = format_error(message, status_code);
    let _ = to_client.send(Bytes::from(error_instr)).await;

    // Return HandlerError for propagation
    HandlerError::ProtocolError(message.to_string())
}

/// Send error to client (best effort, don't propagate failure)
///
/// Use this when you're already in an error/cleanup path and want to notify
/// the client but don't care if the send fails (e.g., client already disconnected).
///
/// This is useful for:
/// - Connection closed by remote server
/// - Mid-session disconnections
/// - Cleanup after other errors
///
/// # Arguments
/// * `to_client` - Channel to send error instruction
/// * `message` - Error message for user (shown in client UI)
/// * `status_code` - Guacamole protocol status code (use STATUS_* constants)
///
/// # Example
/// ```ignore
/// use guacr_handlers::{send_error_best_effort, STATUS_RESOURCE_CLOSED};
///
/// // Connection closed, notify client
/// send_error_best_effort(&to_client, "Connection closed by server", STATUS_RESOURCE_CLOSED).await;
/// break; // Exit event loop
/// ```
pub async fn send_error_best_effort(
    to_client: &mpsc::Sender<Bytes>,
    message: &str,
    status_code: u32,
) {
    let error_instr = format_error(message, status_code);
    let _ = to_client.send(Bytes::from(error_instr)).await;
}
