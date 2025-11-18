// guacr-rdp: RDP protocol handler for remote desktop access
//
// Provides Windows Remote Desktop Protocol support using ironrdp (pure Rust implementation)
//
// # Example
//
// ```no_run
// use guacr_rdp::RdpHandler;
// use guacr_handlers::ProtocolHandler;
// use std::collections::HashMap;
//
// let handler = RdpHandler::with_defaults();
//
// let mut params = HashMap::new();
// params.insert("hostname".to_string(), "windows-server.example.com".to_string());
// params.insert("username".to_string(), "Administrator".to_string());
// params.insert("password".to_string(), "password123".to_string());
// ```

mod framebuffer;
mod handler;

pub use framebuffer::FrameBuffer;
pub use handler::RdpHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RdpError {
    #[error("RDP connection failed: {0}")]
    ConnectionFailed(String),

    #[error("RDP authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RdpError>;
