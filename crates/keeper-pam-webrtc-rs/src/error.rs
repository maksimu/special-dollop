use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("Connection timed out: {0}")]
    Timeout(String),

    #[error("WebRTC error: {0}")]
    WebRTCError(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Other error: {0}")]
    Other(String),

    #[error("Channel closed due to critical upstream connection loss for channel_id: {0}")]
    CriticalUpstreamClosed(String),
}

impl From<anyhow::Error> for ChannelError {
    fn from(err: anyhow::Error) -> Self {
        ChannelError::Other(err.to_string())
    }
}
