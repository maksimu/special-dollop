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
mod clipboard;
mod framebuffer;
mod input_handler;
mod simd;

// Main handler (all logic in one file - SSH pattern)
mod handler;

#[cfg(feature = "sftp")]
mod sftp_integration;

// Public exports
pub use channel_handler::{
    CliprdrData, CliprdrFormat, DispResizeMessage, RdpChannelHandler, RdpgfxUpdate,
};
pub use clipboard::{RdpClipboard, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE};
pub use framebuffer::{FrameBuffer, Rect};
pub use handler::{RdpConfig, RdpHandler};
pub use input_handler::{RdpInputHandler, RdpKeyEvent, RdpPointerEvent};
pub use simd::convert_bgr_to_rgba_simd;

// Re-export hardware encoder from shared terminal module
pub use guacr_terminal::{HardwareEncoder, HardwareEncoderImpl};

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
