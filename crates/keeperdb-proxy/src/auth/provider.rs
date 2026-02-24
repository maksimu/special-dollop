//! AuthProvider trait definition.
//!
//! This module provides the [`AuthProvider`] trait which abstracts
//! credential retrieval for database connections. Implementations
//! can provide credentials from static configuration, Gateway
//! integration, or other sources.

use async_trait::async_trait;

use crate::error::Result;
use crate::tls::TlsClientConfig;

use super::{AuthCredentials, ConnectionContext, SessionConfig, TokenValidation};

/// Trait for pluggable authentication providers.
///
/// Implementations provide credentials and configuration for database
/// connections. The trait is designed to be object-safe so that
/// `Box<dyn AuthProvider>` can be used for runtime polymorphism.
///
/// # Thread Safety
///
/// All implementations must be `Send + Sync` to work with Tokio's
/// multi-threaded runtime.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use keeperdb_proxy::auth::{AuthProvider, StaticAuthProvider, AuthMethod};
///
/// let provider: Arc<dyn AuthProvider> = Arc::new(
///     StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"))
/// );
/// ```
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Retrieve authentication credentials for a connection.
    ///
    /// Called when a new client connection needs to authenticate
    /// to the backend database.
    ///
    /// # Arguments
    ///
    /// * `context` - Information about the incoming connection
    ///
    /// # Returns
    ///
    /// * `Ok(AuthCredentials)` - The credentials to use for authentication
    /// * `Err(_)` - If credentials cannot be retrieved
    ///
    /// # Security
    ///
    /// The returned credentials should be dropped as soon as authentication
    /// completes to minimize the time they exist in memory.
    async fn get_auth(&self, context: &ConnectionContext) -> Result<AuthCredentials>;

    /// Retrieve TLS configuration for the target database connection.
    ///
    /// Called to determine TLS settings for the proxy-to-database connection.
    ///
    /// # Arguments
    ///
    /// * `context` - Information about the incoming connection
    ///
    /// # Returns
    ///
    /// * `Ok(Some(config))` - Use TLS with this configuration
    /// * `Ok(None)` - Do not use TLS for the target connection
    /// * `Err(_)` - If TLS configuration cannot be retrieved
    async fn get_tls_config(&self, context: &ConnectionContext) -> Result<Option<TlsClientConfig>>;

    /// Retrieve session configuration and limits.
    ///
    /// Called to get session limits like timeouts and query limits.
    ///
    /// # Arguments
    ///
    /// * `context` - Information about the incoming connection
    ///
    /// # Returns
    ///
    /// * `Ok(SessionConfig)` - The session configuration to apply
    /// * `Err(_)` - If configuration cannot be retrieved
    async fn get_session_config(&self, context: &ConnectionContext) -> Result<SessionConfig>;

    /// Validate a connection token.
    ///
    /// Called to verify a token provided by the Gateway in the
    /// R3 Pull Model. For providers that don't use tokens
    /// (like [`StaticAuthProvider`](crate::auth::StaticAuthProvider)), this should return
    /// `TokenValidation::valid()`.
    ///
    /// # Arguments
    ///
    /// * `token` - The token to validate
    ///
    /// # Returns
    ///
    /// * `Ok(TokenValidation)` - The validation result
    /// * `Err(_)` - If validation cannot be performed
    async fn validate_token(&self, token: &str) -> Result<TokenValidation>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMethod;
    use std::net::SocketAddr;

    // Mock provider for testing trait bounds
    struct MockProvider;

    #[async_trait]
    impl AuthProvider for MockProvider {
        async fn get_auth(&self, _context: &ConnectionContext) -> Result<AuthCredentials> {
            Ok(AuthCredentials::new(AuthMethod::mysql_native(
                "mock", "mock",
            )))
        }

        async fn get_tls_config(
            &self,
            _context: &ConnectionContext,
        ) -> Result<Option<TlsClientConfig>> {
            Ok(None)
        }

        async fn get_session_config(&self, _context: &ConnectionContext) -> Result<SessionConfig> {
            Ok(SessionConfig::default())
        }

        async fn validate_token(&self, _token: &str) -> Result<TokenValidation> {
            Ok(TokenValidation::valid())
        }
    }

    #[test]
    fn test_trait_is_object_safe() {
        // This test verifies that AuthProvider is object-safe
        // by successfully creating a Box<dyn AuthProvider>
        let _boxed: Box<dyn AuthProvider> = Box::new(MockProvider);
    }

    #[test]
    fn test_trait_is_send_sync() {
        // Verify that the trait bounds include Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockProvider>();
    }

    #[tokio::test]
    async fn test_mock_provider() {
        use crate::auth::DatabaseType;

        let provider = MockProvider;
        let context = ConnectionContext::new(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        );

        let creds = provider.get_auth(&context).await.unwrap();
        assert_eq!(creds.method.username(), "mock");

        let tls = provider.get_tls_config(&context).await.unwrap();
        assert!(tls.is_none());

        let session = provider.get_session_config(&context).await.unwrap();
        assert!(session.max_duration.is_none());

        let token = provider.validate_token("any").await.unwrap();
        assert!(token.valid);
    }
}
