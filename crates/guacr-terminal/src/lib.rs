// guacr-terminal: Terminal emulation library for SSH and Telnet handlers
//
// Provides VT100/ANSI terminal emulation and rendering to JPEG for
// transmission over the Guacamole protocol.
//
// # Example
//
// ```
// use guacr_terminal::{TerminalEmulator, TerminalRenderer};
//
// // Create terminal
// let mut term = TerminalEmulator::new(24, 80);
//
// // Process output
// term.process(b"Hello, World!\n").unwrap();
//
// // Render to JPEG (5-10x faster than PNG)
// let renderer = TerminalRenderer::new().unwrap();
// let jpeg = renderer.render_screen(term.screen(), 24, 80).unwrap();
// assert!(!jpeg.is_empty());
// ```

mod buffer_pool;
mod dirty_tracker;
mod emulator;
mod hardware_encoder;
mod hardware_encoding_helper;
mod keysym;
mod recorder;
mod renderer;

// Hardware encoder platform-specific implementations
#[cfg(feature = "videotoolbox")]
mod hardware_encoder_videotoolbox;

pub use buffer_pool::{BufferPool, BufferPoolStats};
pub use dirty_tracker::{DirtyRect, DirtyTracker};
pub use emulator::{Rect, ScrollbackLine, TerminalEmulator};
pub use hardware_encoder::{HardwareEncoder, HardwareEncoderImpl};
pub use hardware_encoding_helper::try_hardware_encode_then_fallback;
pub use keysym::{mouse_event_to_x11_sequence, x11_keysym_to_bytes, ModifierState};
pub use recorder::{
    create_recording_transports, AsciicastHeader, AsciicastRecorder, AsyncDualFormatRecorder,
    ChannelRecordingTransport, DualFormatRecorder, EventType, FileRecordingTransport,
    GuacamoleSessionRecorder, MultiTransportRecorder, RecordingTransport,
};

// S3 recording transport available when s3 feature is enabled
// #[cfg(feature = "s3")]
// pub use recorder::S3RecordingTransport;
pub use guacr_protocol::*;
pub use renderer::TerminalRenderer;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TerminalError {
    #[error("Rendering error: {0}")]
    RenderError(String),

    #[error("Font error: {0}")]
    FontError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),
}

pub type Result<T> = std::result::Result<T, TerminalError>;
