//! Per-connection credential store.
//!
//! Stores credentials received via handshake, keyed by connection/session ID.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::auth::{DatabaseType, SessionConfig};
use crate::query_logging::config::ConnectionLoggingConfig;
use crate::tls::TlsClientConfig;

/// Credentials received via handshake.
#[derive(Debug, Clone)]
pub struct HandshakeCredentials {
    /// Database type (MySQL or PostgreSQL)
    pub database_type: DatabaseType,
    /// Target database host
    pub target_host: String,
    /// Target database port
    pub target_port: u16,
    /// Database username
    pub username: String,
    /// Database password (zeroized on drop)
    pub password: Zeroizing<String>,
    /// Default database name
    pub database: Option<String>,
    /// TLS configuration for target connection
    pub tls_config: TlsClientConfig,
    /// Session limits and configuration
    pub session_config: SessionConfig,
    /// Gateway-provided session UID for correlation
    pub session_uid: Option<String>,
    /// Per-connection logging configuration (from handshake)
    pub logging_config: Option<ConnectionLoggingConfig>,
    /// When credentials were stored (for TTL cleanup)
    pub stored_at: Instant,
}

impl HandshakeCredentials {
    /// Create new handshake credentials.
    pub fn new(
        database_type: DatabaseType,
        target_host: String,
        target_port: u16,
        username: String,
        password: String,
    ) -> Self {
        Self {
            database_type,
            target_host,
            target_port,
            username,
            password: Zeroizing::new(password),
            database: None,
            tls_config: TlsClientConfig::default(),
            session_config: SessionConfig::default(),
            session_uid: None,
            logging_config: None,
            stored_at: Instant::now(),
        }
    }

    /// Set the database name.
    pub fn with_database(mut self, database: Option<String>) -> Self {
        self.database = database;
        self
    }

    /// Set TLS configuration.
    pub fn with_tls_config(mut self, tls_config: TlsClientConfig) -> Self {
        self.tls_config = tls_config;
        self
    }

    /// Set session configuration.
    pub fn with_session_config(mut self, session_config: SessionConfig) -> Self {
        self.session_config = session_config;
        self
    }

    /// Set Gateway session UID.
    pub fn with_session_uid(mut self, uid: Option<String>) -> Self {
        self.session_uid = uid;
        self
    }

    /// Set per-connection logging configuration.
    pub fn with_logging_config(mut self, config: Option<ConnectionLoggingConfig>) -> Self {
        self.logging_config = config;
        self
    }
}

/// Thread-safe credential store for handshake credentials.
///
/// Credentials are stored after successful handshake and retrieved
/// when the database handler needs to authenticate with the target.
pub struct CredentialStore {
    /// Map of session_id -> credentials
    credentials: DashMap<Uuid, HandshakeCredentials>,
    /// TTL for abandoned credentials (cleanup threshold)
    ttl: Duration,
}

impl CredentialStore {
    /// Create a new credential store with default TTL (5 minutes).
    pub fn new() -> Self {
        Self {
            credentials: DashMap::new(),
            ttl: Duration::from_secs(300),
        }
    }

    /// Create a credential store with custom TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            credentials: DashMap::new(),
            ttl,
        }
    }

    /// Store credentials for a session.
    pub fn store(&self, session_id: Uuid, credentials: HandshakeCredentials) {
        self.credentials.insert(session_id, credentials);
    }

    /// Peek at credentials without consuming them.
    ///
    /// Returns a reference guard that holds a read lock.
    pub fn peek(
        &self,
        session_id: &Uuid,
    ) -> Option<dashmap::mapref::one::Ref<'_, Uuid, HandshakeCredentials>> {
        self.credentials.get(session_id)
    }

    /// Take and remove credentials (single-use).
    ///
    /// The credentials are removed from the store after this call.
    pub fn take(&self, session_id: &Uuid) -> Option<HandshakeCredentials> {
        self.credentials.remove(session_id).map(|(_, v)| v)
    }

    /// Check if credentials exist for a session.
    pub fn contains(&self, session_id: &Uuid) -> bool {
        self.credentials.contains_key(session_id)
    }

    /// Get the number of stored credentials.
    pub fn len(&self) -> usize {
        self.credentials.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }

    /// Clean up expired credentials.
    ///
    /// Removes credentials that have been stored longer than the TTL.
    /// Returns the number of credentials removed.
    pub fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let mut removed = 0;

        self.credentials.retain(|_, creds| {
            let keep = now.duration_since(creds.stored_at) < self.ttl;
            if !keep {
                removed += 1;
            }
            keep
        });

        removed
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_take() {
        let store = CredentialStore::new();
        let session_id = Uuid::new_v4();

        let creds = HandshakeCredentials::new(
            DatabaseType::MySQL,
            "localhost".into(),
            3306,
            "user".into(),
            "pass".into(),
        );

        store.store(session_id, creds);
        assert!(store.contains(&session_id));
        assert_eq!(store.len(), 1);

        let taken = store.take(&session_id);
        assert!(taken.is_some());
        assert!(!store.contains(&session_id));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_peek_does_not_remove() {
        let store = CredentialStore::new();
        let session_id = Uuid::new_v4();

        let creds = HandshakeCredentials::new(
            DatabaseType::MySQL,
            "localhost".into(),
            3306,
            "user".into(),
            "pass".into(),
        );

        store.store(session_id, creds);

        // Peek should not remove
        {
            let peeked = store.peek(&session_id);
            assert!(peeked.is_some());
        }

        // Should still be there
        assert!(store.contains(&session_id));
    }

    #[test]
    fn test_take_nonexistent() {
        let store = CredentialStore::new();
        let session_id = Uuid::new_v4();

        let taken = store.take(&session_id);
        assert!(taken.is_none());
    }

    #[test]
    fn test_credentials_builder() {
        let creds = HandshakeCredentials::new(
            DatabaseType::PostgreSQL,
            "db.example.com".into(),
            5432,
            "pguser".into(),
            "pgpass".into(),
        )
        .with_database(Some("mydb".into()))
        .with_session_uid(Some("gateway-123".into()));

        assert_eq!(creds.database_type, DatabaseType::PostgreSQL);
        assert_eq!(creds.database, Some("mydb".into()));
        assert_eq!(creds.session_uid, Some("gateway-123".into()));
        assert!(creds.logging_config.is_none());
    }

    #[test]
    fn test_credentials_with_logging_config() {
        let logging_config = ConnectionLoggingConfig {
            query_logging_enabled: true,
            include_query_text: true,
            max_query_length: 5000,
            pipe_path: Some("/tmp/query.pipe".into()),
        };

        let creds = HandshakeCredentials::new(
            DatabaseType::MySQL,
            "localhost".into(),
            3306,
            "user".into(),
            "pass".into(),
        )
        .with_logging_config(Some(logging_config));

        let config = creds.logging_config.unwrap();
        assert!(config.query_logging_enabled);
        assert_eq!(config.pipe_path.as_deref(), Some("/tmp/query.pipe"));
    }
}
