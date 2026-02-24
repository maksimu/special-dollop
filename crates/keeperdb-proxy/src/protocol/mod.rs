//! Protocol module for keeperdb-proxy
//!
//! This module contains:
//! - Protocol detection
//! - MySQL protocol implementation
//! - PostgreSQL protocol implementation
//! - SQL Server (TDS) protocol implementation
//! - Oracle (TNS) protocol implementation

pub mod detection;
pub mod mysql;
pub mod oracle;
pub mod postgres;
pub mod sqlserver;

pub use detection::{DetectedProtocol, ProtocolDetector};
