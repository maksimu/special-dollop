// guacr-vnc: VNC protocol handler for remote desktop access
//
// # Example
//
// ```no_run
// use guacr_vnc::VncHandler;
// use guacr_handlers::ProtocolHandler;
//
// let handler = VncHandler::with_defaults();
// ```

// Supporting module (VNC protocol implementation)
mod vnc_protocol;

// Main handler (all logic in one file - SSH pattern)
mod handler;

#[cfg(feature = "sftp")]
mod sftp_integration;

// Public exports
pub use handler::{VncConfig, VncHandler};
pub use vnc_protocol::{VncPixelFormat, VncProtocol, VncRectangle};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum VncError {
    #[error("VNC connection failed: {0}")]
    ConnectionFailed(String),

    #[error("VNC authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("VNC protocol error: {0}")]
    ProtocolError(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, VncError>;
