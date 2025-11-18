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

mod handler;

pub use handler::VncHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum VncError {
    #[error("VNC connection failed: {0}")]
    ConnectionFailed(String),

    #[error("VNC authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, VncError>;
