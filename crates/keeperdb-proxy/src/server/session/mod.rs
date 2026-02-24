//! Session management and relay
//!
//! This module provides session lifecycle management and traffic relay:
//! - [`relay`]: Bidirectional traffic forwarding between client and server
//! - [`manager`]: Session lifecycle, timers, and limit enforcement

pub mod manager;
pub mod relay;

pub use manager::{DisconnectReason, SessionManager};
pub use relay::{ManagedSession, Session};
