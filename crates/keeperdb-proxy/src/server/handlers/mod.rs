//! Database protocol handlers
//!
//! This module contains protocol-specific handlers for different database types.
//! Each handler implements the authentication flow with credential injection
//! and traffic relay for its respective database protocol.

pub mod mysql;
pub mod oracle;
pub mod postgres;
pub mod sqlserver;

pub use mysql::MySQLHandler;
pub use oracle::OracleHandler;
pub use postgres::PostgresHandler;
pub use sqlserver::SqlServerHandler;
