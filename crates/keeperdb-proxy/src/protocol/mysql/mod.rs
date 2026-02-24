//! MySQL protocol implementation
//!
//! This module contains:
//! - Packet structures
//! - Packet parser (read/write)
//! - Authentication (mysql_native_password)
//! - MySQL connection handler

pub mod auth;
pub mod packets;
pub mod parser;

pub use auth::*;
pub use packets::*;
pub use parser::*;
