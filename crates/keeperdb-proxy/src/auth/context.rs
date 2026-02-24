//! Connection context for authentication decisions.
//!
//! This module provides the [`ConnectionContext`] struct which contains
//! metadata about incoming connections used by [`AuthProvider`](crate::auth::AuthProvider) implementations
//! to make authentication decisions.

use std::collections::HashMap;
use std::net::SocketAddr;

/// Supported database types.
///
/// This enum identifies the type of database being connected to,
/// allowing [`AuthProvider`](crate::auth::AuthProvider) implementations to return appropriate
/// credentials and configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatabaseType {
    /// MySQL / MariaDB
    MySQL,
    /// PostgreSQL
    PostgreSQL,
    /// Microsoft SQL Server
    SQLServer,
    /// Oracle Database
    Oracle,
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MySQL => write!(f, "mysql"),
            Self::PostgreSQL => write!(f, "postgresql"),
            Self::SQLServer => write!(f, "sqlserver"),
            Self::Oracle => write!(f, "oracle"),
        }
    }
}

/// Connection context passed to AuthProvider methods.
///
/// Contains metadata about the incoming connection that providers
/// can use to make authentication and configuration decisions.
///
/// # Example
///
/// ```
/// use std::net::SocketAddr;
/// use keeperdb_proxy::auth::{ConnectionContext, DatabaseType};
///
/// let context = ConnectionContext::new(
///     "127.0.0.1:12345".parse().unwrap(),
///     "db.example.com",
///     3306,
///     DatabaseType::MySQL,
/// );
///
/// assert!(!context.session_id.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct ConnectionContext {
    /// Client's socket address (IP and port).
    pub client_address: SocketAddr,

    /// Target database hostname.
    pub target_host: String,

    /// Target database port.
    pub target_port: u16,

    /// Type of database being connected to.
    pub database_type: DatabaseType,

    /// Unique session identifier (UUID v4).
    ///
    /// Generated automatically if not provided.
    pub session_id: String,

    /// Connection token from Gateway (R3 Pull Model).
    ///
    /// When using Gateway integration, this contains the token
    /// that identifies the authorized connection request.
    pub connection_token: Option<String>,

    /// Additional metadata from Gateway.
    ///
    /// Key-value pairs for extensibility.
    pub metadata: HashMap<String, String>,
}

impl ConnectionContext {
    /// Create a new ConnectionContext with a generated session ID.
    ///
    /// # Arguments
    ///
    /// * `client_address` - The client's socket address
    /// * `target_host` - The target database hostname
    /// * `target_port` - The target database port
    /// * `database_type` - The type of database
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::{ConnectionContext, DatabaseType};
    ///
    /// let context = ConnectionContext::new(
    ///     "192.168.1.100:54321".parse().unwrap(),
    ///     "mysql.internal",
    ///     3306,
    ///     DatabaseType::MySQL,
    /// );
    /// ```
    pub fn new(
        client_address: SocketAddr,
        target_host: impl Into<String>,
        target_port: u16,
        database_type: DatabaseType,
    ) -> Self {
        Self {
            client_address,
            target_host: target_host.into(),
            target_port,
            database_type,
            session_id: uuid::Uuid::new_v4().to_string(),
            connection_token: None,
            metadata: HashMap::new(),
        }
    }

    /// Set the connection token (builder pattern).
    ///
    /// # Example
    ///
    /// ```
    /// # use keeperdb_proxy::auth::{ConnectionContext, DatabaseType};
    /// let context = ConnectionContext::new(
    ///     "127.0.0.1:12345".parse().unwrap(),
    ///     "localhost",
    ///     3306,
    ///     DatabaseType::MySQL,
    /// ).with_token("abc123");
    ///
    /// assert_eq!(context.connection_token, Some("abc123".to_string()));
    /// ```
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.connection_token = Some(token.into());
        self
    }

    /// Add a metadata key-value pair (builder pattern).
    ///
    /// # Example
    ///
    /// ```
    /// # use keeperdb_proxy::auth::{ConnectionContext, DatabaseType};
    /// let context = ConnectionContext::new(
    ///     "127.0.0.1:12345".parse().unwrap(),
    ///     "localhost",
    ///     3306,
    ///     DatabaseType::MySQL,
    /// ).with_metadata("user_agent", "my-app/1.0");
    ///
    /// assert_eq!(context.metadata.get("user_agent"), Some(&"my-app/1.0".to_string()));
    /// ```
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Set the session ID (builder pattern).
    ///
    /// Use this when the session ID is provided externally (e.g., from a handshake)
    /// rather than auto-generated.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_generates_session_id() {
        let context = ConnectionContext::new(
            "127.0.0.1:12345".parse().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        );

        assert!(!context.session_id.is_empty());
        // Should be a valid UUID (36 chars with hyphens)
        assert_eq!(context.session_id.len(), 36);
    }

    #[test]
    fn test_with_token() {
        let context = ConnectionContext::new(
            "127.0.0.1:12345".parse().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        )
        .with_token("test-token");

        assert_eq!(context.connection_token, Some("test-token".to_string()));
    }

    #[test]
    fn test_with_metadata() {
        let context = ConnectionContext::new(
            "127.0.0.1:12345".parse().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        )
        .with_metadata("key1", "value1")
        .with_metadata("key2", "value2");

        assert_eq!(context.metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(context.metadata.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_database_type_display() {
        assert_eq!(format!("{}", DatabaseType::MySQL), "mysql");
        assert_eq!(format!("{}", DatabaseType::PostgreSQL), "postgresql");
        assert_eq!(format!("{}", DatabaseType::SQLServer), "sqlserver");
    }

    #[test]
    fn test_context_fields() {
        let addr: SocketAddr = "192.168.1.100:54321".parse().unwrap();
        let context =
            ConnectionContext::new(addr, "db.example.com", 5432, DatabaseType::PostgreSQL);

        assert_eq!(context.client_address, addr);
        assert_eq!(context.target_host, "db.example.com");
        assert_eq!(context.target_port, 5432);
        assert_eq!(context.database_type, DatabaseType::PostgreSQL);
        assert!(context.connection_token.is_none());
        assert!(context.metadata.is_empty());
    }
}
