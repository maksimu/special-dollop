use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Connection timed out: {0}")]
    Timeout(String),

    #[error("Invalid protocol format: {0}")]
    InvalidFormat(String),

    #[error("Network access denied: {0}")]
    NetworkAccessDenied(String),

    #[error("Remote disconnected")]
    RemoteDisconnected,

    #[error("WebRTC error: {0}")]
    WebRTCError(String),

    #[error("Data channel error: {0}")]
    DataChannelError(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Other error: {0}")]
    Other(String),
}

impl From<anyhow::Error> for ChannelError {
    fn from(err: anyhow::Error) -> Self {
        ChannelError::Other(err.to_string())
    }
}

