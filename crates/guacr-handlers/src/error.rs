use thiserror::Error;

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
}

pub type Result<T> = std::result::Result<T, HandlerError>;
