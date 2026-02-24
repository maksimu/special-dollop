//! Gateway handshake protocol for credential delivery.
//!
//! This module implements a length-prefixed instruction protocol that allows
//! the Gateway to push credentials dynamically when creating database tunnels.
//!
//! # Protocol Format
//!
//! Instructions use length-prefixed encoding:
//! ```text
//! <length>.<opcode>,<arg1_len>.<arg1>,<arg2_len>.<arg2>,...;
//! ```
//!
//! # Handshake Sequence
//!
//! 1. Client sends `select` with database type (mysql/postgresql)
//! 2. Server responds with `args` listing required parameters
//! 3. Client sends `connect` with parameter values
//! 4. Server responds with `ready` and session ID
//! 5. Database protocol traffic flows through the connection

mod handler;
mod instruction;
mod provider;
mod store;

pub use handler::{HandshakeHandler, HandshakeResult};
pub use instruction::Instruction;
pub use provider::HandshakeAuthProvider;
pub use store::{CredentialStore, HandshakeCredentials};
