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
mod clipboard;
mod config;
mod database_renderer;
mod dirty_tracker;
mod emulator;
mod framebuffer;
mod guacamole_input;
mod input_handler;
mod keysym;
mod recorder;
mod recording_pipeline;
mod renderer;
mod scroll_detector;
mod selection_point;
mod simd;
mod terminal_input_handler;

pub use buffer_pool::{BufferPool, BufferPoolStats};
pub use clipboard::{RdpClipboard, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE};
pub use config::{ColorScheme, TerminalConfig};
pub use database_renderer::{DatabaseTerminal, QueryResult, SpreadsheetRenderer};
pub use dirty_tracker::{DirtyRect, DirtyTracker};
pub use emulator::{Rect, ScrollbackLine, TerminalEmulator};
pub use framebuffer::{FrameBuffer, FrameRect};
pub use guacamole_input::{
    extract_selection_text, format_clear_selection_instructions, format_clipboard_instructions,
    format_selection_overlay_instructions, handle_mouse_selection, parse_clipboard_blob,
    parse_key_instruction, parse_mouse_instruction, KeyEvent, MouseEvent, MouseSelection,
    SelectionResult,
};
pub use input_handler::{RdpInputHandler, RdpKeyEvent, RdpPointerEvent};
pub use keysym::{
    mouse_event_to_x11_sequence, x11_keysym_to_bytes, x11_keysym_to_bytes_with_backspace,
    x11_keysym_to_bytes_with_modes, x11_keysym_to_kitty_sequence, ModifierState,
};
pub use recorder::{
    create_recording_transports, AsciicastHeader, AsciicastRecorder, AsyncDualFormatRecorder,
    ChannelRecordingTransport, DualFormatRecorder, EventType, FileRecordingTransport,
    GuacamoleSessionRecorder, MultiTransportRecorder, RecordingTransport,
};
pub use recording_pipeline::{RecordingPipeline, RecordingTaskManager};
pub use scroll_detector::{ScrollDetector, ScrollDirection, ScrollOperation};
pub use selection_point::{points_enclose_text, ColumnSide, SelectionPoint};
pub use simd::convert_bgr_to_rgba_simd;
pub use terminal_input_handler::TerminalInputHandler;

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
