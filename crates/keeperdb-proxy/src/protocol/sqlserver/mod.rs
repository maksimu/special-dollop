//! SQL Server TDS protocol implementation
//!
//! This module implements the TDS (Tabular Data Stream) protocol used by
//! Microsoft SQL Server. It provides packet parsing, serialization, and
//! authentication utilities for the proxy.
//!
//! ## Protocol Overview
//!
//! TDS is a binary protocol with the following connection flow:
//! 1. Client sends PRELOGIN packet (version negotiation, encryption)
//! 2. Server responds with PRELOGIN
//! 3. Optional TLS handshake (if encryption negotiated)
//! 4. Client sends LOGIN7 packet (authentication)
//! 5. Server responds with LOGINACK + ENVCHANGE tokens
//! 6. SQL queries sent as SQL_BATCH packets
//! 7. Results returned as TABULAR_RESULT packets
//!
//! ## References
//!
//! - [MS-TDS]: Tabular Data Stream Protocol
//!   <https://docs.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/>

pub mod auth;
pub mod constants;
pub mod packets;
pub mod parser;
pub mod tds_tls_stream;

// Re-export commonly used types
pub use constants::EncryptionMode;
pub use packets::{EnvChange, Login7, LoginAck, Message, Prelogin, TdsHeader};
pub use tds_tls_stream::{TdsClientTlsStream, TdsServerTlsStream};
