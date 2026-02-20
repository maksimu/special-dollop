// guacr-database: Database protocol handlers for MySQL, PostgreSQL, SQL Server, Oracle, MongoDB, Redis, Elasticsearch, and Cassandra
//
// Provides SQL/NoSQL terminal access via WebRTC for database administration.
// All handlers use the shared DatabaseTerminal from guacr-terminal for consistent UI.

mod cassandra;
mod csv_export;
mod csv_import;
mod dynamodb;
mod elasticsearch;
pub mod handler_helpers;
mod mariadb;
mod mongodb;
mod mysql;
mod odbc;
mod oracle;
mod postgresql;
mod query_executor;
mod recording;
mod redis;
mod security;
mod sqlserver;

pub use cassandra::CassandraHandler;
pub use csv_export::{generate_csv_filename, CsvExporter};
pub use csv_import::{CsvData, CsvImporter, ImportState};
pub use dynamodb::DynamoDbHandler;
pub use elasticsearch::ElasticsearchHandler;
pub use mariadb::MariaDbHandler;
pub use mongodb::MongoDbHandler;
pub use mysql::MySqlHandler;
pub use odbc::OdbcHandler;
pub use oracle::OracleHandler;
pub use postgresql::PostgreSqlHandler;
pub use query_executor::{QueryExecutor, QueryResultData};
pub use redis::RedisHandler;
pub use security::{
    check_csv_export_allowed, check_csv_import_allowed, check_query_allowed, classify_query,
    DatabaseSecuritySettings, QueryType,
};
pub use sqlserver::SqlServerHandler;

// Re-export shared types from guacr-terminal
pub use guacr_terminal::{DatabaseTerminal, QueryResult, SpreadsheetRenderer};

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
