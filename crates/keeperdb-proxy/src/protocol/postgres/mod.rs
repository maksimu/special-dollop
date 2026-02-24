//! PostgreSQL protocol implementation
//!
//! This module contains:
//! - Protocol constants and message types
//! - Message structures for the wire protocol
//! - Message codec (read/write functions)
//! - Authentication (MD5 and SCRAM-SHA-256)
//!
//! Reference: <https://www.postgresql.org/docs/current/protocol.html>

pub mod auth;
pub mod codec;
pub mod constants;
pub mod messages;

// Re-export commonly used items
pub use auth::*;
pub use codec::*;
pub use constants::*;
pub use messages::*;
