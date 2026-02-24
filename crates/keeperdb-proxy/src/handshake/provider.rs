//! Handshake-based AuthProvider implementation.
//!
//! This provider retrieves credentials from the CredentialStore
//! based on the session ID in the ConnectionContext.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::auth::{
    AuthCredentials, AuthMethod, AuthProvider, ConnectionContext, SessionConfig, TokenValidation,
};
use crate::error::{ProxyError, Result};
use crate::tls::TlsClientConfig;

use super::store::CredentialStore;

/// AuthProvider that retrieves credentials from handshake store.
///
/// This provider is used in gateway mode where credentials are
/// delivered via handshake and stored in the CredentialStore.
/// The session_id in ConnectionContext is used to look up credentials.
pub struct HandshakeAuthProvider {
    /// Shared credential store
    store: Arc<CredentialStore>,
}

impl HandshakeAuthProvider {
    /// Create a new HandshakeAuthProvider with the given store.
    pub fn new(store: Arc<CredentialStore>) -> Self {
        Self { store }
    }

    /// Parse session_id from context as UUID.
    fn parse_session_id(context: &ConnectionContext) -> Result<Uuid> {
        Uuid::parse_str(&context.session_id).map_err(|_| {
            ProxyError::Auth(format!("Invalid session ID format: {}", context.session_id))
        })
    }
}

#[async_trait]
impl AuthProvider for HandshakeAuthProvider {
    /// Retrieve authentication credentials.
    ///
    /// Looks up credentials by session_id and consumes them (single-use).
    async fn get_auth(&self, context: &ConnectionContext) -> Result<AuthCredentials> {
        let session_id = Self::parse_session_id(context)?;

        let creds = self.store.take(&session_id).ok_or_else(|| {
            ProxyError::Auth(format!("No credentials found for session: {}", session_id))
        })?;

        // Build AuthMethod based on database type
        let method = match creds.database_type {
            crate::auth::DatabaseType::MySQL => {
                AuthMethod::mysql_native(&creds.username, &*creds.password)
            }
            crate::auth::DatabaseType::PostgreSQL => {
                AuthMethod::postgres_scram_sha256(&creds.username, &*creds.password)
            }
            crate::auth::DatabaseType::SQLServer => {
                AuthMethod::sqlserver_auth(&creds.username, &*creds.password)
            }
            crate::auth::DatabaseType::Oracle => {
                // Oracle uses O5LOGON authentication - for now use SQL Server style auth
                // which is also username/password based. Will be replaced with proper
                // Oracle auth method in Story 6.
                AuthMethod::sqlserver_auth(&creds.username, &*creds.password)
            }
        };

        let mut auth_creds = AuthCredentials::new(method);

        if let Some(db) = creds.database {
            auth_creds = auth_creds.with_database(&db);
        }

        Ok(auth_creds)
    }

    /// Retrieve TLS configuration for the target connection.
    ///
    /// Uses peek to avoid consuming credentials.
    async fn get_tls_config(&self, context: &ConnectionContext) -> Result<Option<TlsClientConfig>> {
        let session_id = Self::parse_session_id(context)?;

        let creds_ref = self.store.peek(&session_id).ok_or_else(|| {
            ProxyError::Auth(format!("No credentials found for session: {}", session_id))
        })?;

        if creds_ref.tls_config.enabled {
            Ok(Some(creds_ref.tls_config.clone()))
        } else {
            Ok(None)
        }
    }

    /// Retrieve session configuration.
    ///
    /// Uses peek to avoid consuming credentials.
    async fn get_session_config(&self, context: &ConnectionContext) -> Result<SessionConfig> {
        let session_id = Self::parse_session_id(context)?;

        let creds_ref = self.store.peek(&session_id).ok_or_else(|| {
            ProxyError::Auth(format!("No credentials found for session: {}", session_id))
        })?;

        Ok(creds_ref.session_config.clone())
    }

    /// Validate a connection token.
    ///
    /// In handshake mode, the Gateway has already validated the connection
    /// by providing credentials. Token validation is not needed per-connection.
    async fn validate_token(&self, _token: &str) -> Result<TokenValidation> {
        // Gateway already authorized this connection via handshake
        Ok(TokenValidation::valid())
    }
}

// Ensure HandshakeAuthProvider is Send + Sync
#[cfg(test)]
static_assertions::assert_impl_all!(HandshakeAuthProvider: Send, Sync);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::DatabaseType;
    use std::net::SocketAddr;

    fn test_context(session_id: &str) -> ConnectionContext {
        let mut ctx = ConnectionContext::new(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        );
        // Override session_id
        ctx.session_id = session_id.to_string();
        ctx
    }

    #[tokio::test]
    async fn test_get_auth_success() {
        let store = Arc::new(CredentialStore::default());
        let provider = HandshakeAuthProvider::new(Arc::clone(&store));

        let session_id = Uuid::new_v4();
        let creds = super::super::store::HandshakeCredentials::new(
            DatabaseType::MySQL,
            "localhost".into(),
            3306,
            "testuser".into(),
            "testpass".into(),
        );

        store.store(session_id, creds);

        let context = test_context(&session_id.to_string());
        let auth = provider.get_auth(&context).await.unwrap();

        assert_eq!(auth.method.username(), "testuser");
        assert_eq!(auth.method.password(), Some("testpass"));

        // Credentials should be consumed
        assert!(!store.contains(&session_id));
    }

    #[tokio::test]
    async fn test_get_auth_not_found() {
        let store = Arc::new(CredentialStore::default());
        let provider = HandshakeAuthProvider::new(store);

        let session_id = Uuid::new_v4();
        let context = test_context(&session_id.to_string());

        let result = provider.get_auth(&context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_session_config_does_not_consume() {
        let store = Arc::new(CredentialStore::default());
        let provider = HandshakeAuthProvider::new(Arc::clone(&store));

        let session_id = Uuid::new_v4();
        let mut creds = super::super::store::HandshakeCredentials::new(
            DatabaseType::MySQL,
            "localhost".into(),
            3306,
            "testuser".into(),
            "testpass".into(),
        );
        creds.session_config = SessionConfig::default().with_max_queries(100);

        store.store(session_id, creds);

        let context = test_context(&session_id.to_string());

        // get_session_config should not consume
        let config = provider.get_session_config(&context).await.unwrap();
        assert_eq!(config.max_queries, Some(100));

        // Credentials should still be there
        assert!(store.contains(&session_id));
    }

    #[tokio::test]
    async fn test_validate_token_always_valid() {
        let store = Arc::new(CredentialStore::default());
        let provider = HandshakeAuthProvider::new(store);

        let result = provider.validate_token("any-token").await.unwrap();
        assert!(result.valid);
    }

    #[tokio::test]
    async fn test_invalid_session_id_format() {
        let store = Arc::new(CredentialStore::default());
        let provider = HandshakeAuthProvider::new(store);

        let context = test_context("not-a-uuid");
        let result = provider.get_auth(&context).await;

        assert!(result.is_err());
        assert!(matches!(result, Err(ProxyError::Auth(_))));
    }
}
