//! RDP channel handlers
//!
//! Implements handlers for RDP virtual channels:
//! - Clipboard (cliprdr)
//! - Audio output (rdpsnd)
//! - Audio input
//! - Drive redirection (rdpdr)
//! - Display resize (disp)
//! - Graphics pipeline (rdpgfx)

mod clipboard;

pub use clipboard::ClipboardHandler;

// TODO: Implement additional channel handlers
// mod audio;
// mod drive;
// mod display;
// mod graphics;
