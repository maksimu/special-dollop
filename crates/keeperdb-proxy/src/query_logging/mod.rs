//! Query logging and audit infrastructure.
//!
//! Provides structured SQL query logging across all supported database protocols
//! (MySQL, PostgreSQL, SQL Server, Oracle). Log entries are written as JSON Lines
//! to a named pipe (FIFO), where the Python Gateway reads, encrypts (AES-256-GCM),
//! and uploads to Krouter.
//!
//! ## Architecture
//!
//! Query logging uses a non-blocking async pipeline:
//!
//! 1. Protocol-specific `QueryExtractor` inspects relay traffic
//! 2. `QueryLog` entries are sent via bounded mpsc channel (non-blocking `try_send`)
//! 3. Background `QueryLoggingService` writes JSON Lines to a named pipe
//! 4. Python Gateway handles encryption and upload (same as session recordings)
//!
//! ## Configuration
//!
//! Logging can be enabled globally via YAML config or per-connection via
//! Gateway handshake protocol.

pub mod config;
pub mod extractor;
pub mod log_entry;
pub mod service;

pub use config::{ConnectionLoggingConfig, QueryLoggingConfig};
pub use extractor::{
    MysqlQueryExtractor, OracleQueryExtractor, PostgresQueryExtractor, QueryExtractor, QueryInfo,
    ResponseInfo, TdsQueryExtractor,
};
pub use log_entry::{QueryLog, SessionContext, SqlCommandType};
pub use service::{QueryLogSender, QueryLoggingService};
