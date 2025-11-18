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

mod dirty_tracker;
mod emulator;
mod keysym;
mod recorder;
mod renderer;

pub use dirty_tracker::{DirtyRect, DirtyTracker};
pub use emulator::{Rect, TerminalEmulator};
pub use keysym::x11_keysym_to_bytes;
pub use recorder::{AsciicastHeader, AsciicastRecorder, EventType};
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
