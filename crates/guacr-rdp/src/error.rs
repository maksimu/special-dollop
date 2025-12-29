//! RDP error types

use thiserror::Error;

/// RDP-specific errors
#[derive(Error, Debug)]
pub enum RdpError {
    #[error("RDP connection failed: {0}")]
    ConnectionFailed(String),

    #[error("RDP authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("RDP protocol error: {0}")]
    ProtocolError(String),

    #[error("RDP disconnected: {0}")]
    Disconnected(String),

    #[error("FreeRDP initialization failed: {0}")]
    InitializationFailed(String),

    #[error("FreeRDP not installed or not found")]
    FreeRdpNotFound,

    #[error("Invalid settings: {0}")]
    InvalidSettings(String),

    #[error("Channel error: {0}")]
    ChannelError(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Result type for RDP operations
pub type Result<T> = std::result::Result<T, RdpError>;

impl From<RdpError> for guacr_handlers::HandlerError {
    fn from(e: RdpError) -> Self {
        guacr_handlers::HandlerError::ProtocolError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = RdpError::ConnectionFailed("timeout".to_string());
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("connection failed"));
    }

    #[test]
    fn test_error_conversion_to_handler_error() {
        let rdp_err = RdpError::AuthenticationFailed("bad password".to_string());
        let handler_err: guacr_handlers::HandlerError = rdp_err.into();
        assert!(handler_err.to_string().contains("bad password"));
    }

    #[test]
    fn test_all_error_variants() {
        // Ensure all variants can be created and displayed
        let errors = vec![
            RdpError::ConnectionFailed("test".to_string()),
            RdpError::AuthenticationFailed("test".to_string()),
            RdpError::ProtocolError("test".to_string()),
            RdpError::Disconnected("test".to_string()),
            RdpError::InitializationFailed("test".to_string()),
            RdpError::FreeRdpNotFound,
            RdpError::InvalidSettings("test".to_string()),
            RdpError::ChannelError("test".to_string()),
        ];

        for err in errors {
            // Should not panic
            let _ = err.to_string();
        }
    }
}
