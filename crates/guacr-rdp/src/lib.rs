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

// Supporting modules (kept minimal - infrastructure only)
mod channel_handler;

// Main handler (all logic in one file - SSH pattern)
mod handler;

#[cfg(feature = "sftp")]
mod sftp_integration;

// Public exports
pub use channel_handler::{
    CliprdrData, CliprdrFormat, DispResizeMessage, RdpChannelHandler, RdpgfxUpdate,
};
pub use handler::{RdpConfig, RdpHandler};

// Re-export shared types from guacr-terminal
// All framebuffer, clipboard, input, and SIMD functionality comes from guacr-terminal
pub use guacr_terminal::{
    convert_bgr_to_rgba_simd, FrameBuffer, FrameRect as Rect, RdpClipboard, RdpInputHandler,
    RdpKeyEvent, RdpPointerEvent, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE,
};

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
