// guacr-database: Database protocol handlers for MySQL, PostgreSQL, and SQL Server
//
// Provides SQL terminal access via WebRTC for database administration

mod mariadb;
mod mongodb;
mod mysql;
mod oracle;
mod postgresql;
mod redis;
mod spreadsheet;
mod sql_terminal;
mod sqlserver;

pub use mariadb::MariaDbHandler;
pub use mongodb::MongoDbHandler;
pub use mysql::MySqlHandler;
pub use oracle::OracleHandler;
pub use postgresql::PostgreSqlHandler;
pub use redis::RedisHandler;
pub use spreadsheet::{QueryResult, SpreadsheetView};
pub use sql_terminal::SqlTerminal;
pub use sqlserver::SqlServerHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("Database connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Terminal error: {0}")]
    TerminalError(#[from] guacr_terminal::TerminalError),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DatabaseError>;
