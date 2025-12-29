//! guacr-rdp: RDP protocol handler for remote desktop access
//!
//! Provides Windows Remote Desktop Protocol support using FreeRDP,
//! the same library used by Apache Guacamole's guacd.
//!
//! # Example
//!
//! ```no_run
//! use guacr_rdp::RdpHandler;
//! use guacr_handlers::ProtocolHandler;
//! use std::collections::HashMap;
//!
//! let handler = RdpHandler::with_defaults();
//!
//! let mut params = HashMap::new();
//! params.insert("hostname".to_string(), "windows-server.example.com".to_string());
//! params.insert("username".to_string(), "Administrator".to_string());
//! params.insert("password".to_string(), "password123".to_string());
//! ```
//!
//! # Backend
//!
//! This crate uses FreeRDP via FFI bindings. FreeRDP is a mature, battle-tested
//! RDP implementation used by guacd and many other projects.
//!
//! ## System Requirements
//!
//! FreeRDP 3.x development libraries must be installed:
//! - Ubuntu/Debian: `apt install freerdp3-dev libwinpr3-dev`
//! - Fedora/RHEL: `dnf install freerdp-devel`
//! - macOS: `brew install freerdp`

// FFI bindings to FreeRDP
mod ffi;

// FreeRDP client wrapper
mod client;

// RDP channel handlers (clipboard, audio, drive)
mod channels;

// Main protocol handler
mod handler;

// Error types
mod error;

// Settings parsing
mod settings;

// Public exports
pub use client::FreeRdpClient;
pub use error::{RdpError, Result};
pub use handler::{RdpConfig, RdpHandler};
pub use settings::RdpSettings;

// Re-export shared types from guacr-terminal
pub use guacr_terminal::{
    FrameBuffer, FrameRect as Rect, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE,
};
