//! GatewayAuthProvider implementation.
//!
//! This module provides an AuthProvider implementation for Gateway integration
//! that stores credentials provided at initialization time.

use async_trait::async_trait;

use crate::auth::{
    AuthCredentials, AuthMethod, AuthProvider, ConnectionContext, SessionConfig, TokenValidation,
};
use crate::error::Result;
use crate::tls::TlsClientConfig;

use super::config::PyDbParams;

/// AuthProvider implementation for Keeper Gateway integration.
///
/// This provider stores credentials and configuration passed from Python
/// at initialization time. Unlike callback-based providers, all auth data
/// is available immediately without needing to call back into Python.
///
/// # Thread Safety
///
/// The provider stores all configuration in immutable fields, making it
/// safe to share across threads. Credentials are stored with secure memory
/// handling via [`AuthMethod`]'s use of [`Zeroizing<String>`].
///
/// # Example
///
/// ```ignore
/// use keeperdb_proxy::pyo3::GatewayAuthProvider;
///
/// // Created from PyDbParams parsed from Python dict
/// let provider = GatewayAuthProvider::from_params(db_params);
///
/// // Use as AuthProvider
/// let creds = provider.get_auth(&context).await?;
/// ```
pub struct GatewayAuthProvider {
    /// Authentication method with credentials
    auth_method: AuthMethod,

    /// Optional database name override
    database: Option<String>,

    /// Session configuration (limits, timeouts)
    session_config: SessionConfig,

    /// TLS configuration for backend connection
    tls_config: TlsClientConfig,
}

impl GatewayAuthProvider {
    /// Create a new GatewayAuthProvider from parsed Python parameters.
    ///
    /// # Arguments
    ///
    /// * `params` - Parsed database parameters from Python
    ///
    /// # Example
    ///
    /// ```ignore
    /// let params = PyDbParams::from_py_dict(dict)?;
    /// let provider = GatewayAuthProvider::from_params(&params);
    /// ```
    pub fn from_params(params: &PyDbParams) -> std::result::Result<Self, &'static str> {
        Ok(Self {
            auth_method: params.to_auth_method()?,
            database: params.database.clone(),
            session_config: params.session_config.clone(),
            tls_config: params.tls_config.clone(),
        })
    }

    /// Create a new GatewayAuthProvider with explicit parameters.
    ///
    /// This constructor is useful for testing or when not using PyDbParams.
    ///
    /// # Arguments
    ///
    /// * `auth_method` - The authentication method with credentials
    /// * `database` - Optional database name override
    /// * `session_config` - Session limits and configuration
    /// * `tls_config` - TLS configuration for backend connection
    #[allow(dead_code)]
    pub fn new(
        auth_method: AuthMethod,
        database: Option<String>,
        session_config: SessionConfig,
        tls_config: TlsClientConfig,
    ) -> Self {
        Self {
            auth_method,
            database,
            session_config,
            tls_config,
        }
    }
}

#[async_trait]
impl AuthProvider for GatewayAuthProvider {
    /// Retrieve authentication credentials.
    ///
    /// Returns the credentials stored at initialization time.
    /// The connection context is logged but not used to vary credentials,
    /// since Gateway provides credentials per-proxy-instance.
    async fn get_auth(&self, _context: &ConnectionContext) -> Result<AuthCredentials> {
        let mut creds = AuthCredentials::new(self.auth_method.clone());

        if let Some(ref db) = self.database {
            creds = creds.with_database(db);
        }

        Ok(creds)
    }

    /// Retrieve TLS configuration for the target database connection.
    ///
    /// Returns the TLS configuration stored at initialization time.
    /// Returns `None` if TLS is not enabled.
    async fn get_tls_config(
        &self,
        _context: &ConnectionContext,
    ) -> Result<Option<TlsClientConfig>> {
        if self.tls_config.enabled {
            Ok(Some(self.tls_config.clone()))
        } else {
            Ok(None)
        }
    }

    /// Retrieve session configuration and limits.
    ///
    /// Returns the session configuration stored at initialization time.
    async fn get_session_config(&self, _context: &ConnectionContext) -> Result<SessionConfig> {
        Ok(self.session_config.clone())
    }

    /// Validate a connection token.
    ///
    /// In the Gateway settings-based model, tokens are not used for
    /// per-connection validation. This always returns valid.
    ///
    /// If token validation is needed in the future, this can be extended
    /// to validate against a token store or callback to Gateway.
    async fn validate_token(&self, _token: &str) -> Result<TokenValidation> {
        // In settings-based mode, we don't validate tokens per-connection.
        // The Gateway has already authorized this proxy instance.
        Ok(TokenValidation::valid())
    }
}

#[cfg(test)]
mod tests {
    // Ensure GatewayAuthProvider is Send + Sync as required by AuthProvider
    static_assertions::assert_impl_all!(super::GatewayAuthProvider: Send, Sync);
    use super::*;
    use crate::auth::DatabaseType;
    use std::net::SocketAddr;

    fn test_context() -> ConnectionContext {
        ConnectionContext::new(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        )
    }

    #[tokio::test]
    async fn test_get_auth_basic() {
        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("testuser", "testpass"),
            None,
            SessionConfig::default(),
            TlsClientConfig::default(),
        );

        let creds = provider.get_auth(&test_context()).await.unwrap();
        assert_eq!(creds.method.username(), "testuser");
        assert_eq!(creds.method.password(), Some("testpass"));
        assert!(creds.database.is_none());
    }

    #[tokio::test]
    async fn test_get_auth_with_database() {
        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("user", "pass"),
            Some("mydb".to_string()),
            SessionConfig::default(),
            TlsClientConfig::default(),
        );

        let creds = provider.get_auth(&test_context()).await.unwrap();
        assert_eq!(creds.database, Some("mydb".to_string()));
    }

    #[tokio::test]
    async fn test_get_tls_config_disabled() {
        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("user", "pass"),
            None,
            SessionConfig::default(),
            TlsClientConfig::default(), // enabled = false by default
        );

        let tls = provider.get_tls_config(&test_context()).await.unwrap();
        assert!(tls.is_none());
    }

    #[tokio::test]
    async fn test_get_tls_config_enabled() {
        let tls_config = TlsClientConfig {
            enabled: true,
            ..TlsClientConfig::default()
        };

        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("user", "pass"),
            None,
            SessionConfig::default(),
            tls_config,
        );

        let tls = provider.get_tls_config(&test_context()).await.unwrap();
        assert!(tls.is_some());
        assert!(tls.unwrap().enabled);
    }

    #[tokio::test]
    async fn test_get_session_config() {
        use std::time::Duration;

        let session = SessionConfig::default()
            .with_max_duration(Duration::from_secs(3600))
            .with_max_queries(1000);

        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("user", "pass"),
            None,
            session,
            TlsClientConfig::default(),
        );

        let config = provider.get_session_config(&test_context()).await.unwrap();
        assert_eq!(config.max_duration, Some(Duration::from_secs(3600)));
        assert_eq!(config.max_queries, Some(1000));
    }

    #[tokio::test]
    async fn test_validate_token_always_valid() {
        let provider = GatewayAuthProvider::new(
            AuthMethod::mysql_native("user", "pass"),
            None,
            SessionConfig::default(),
            TlsClientConfig::default(),
        );

        let result = provider.validate_token("any_token").await.unwrap();
        assert!(result.valid);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_postgres_auth_method() {
        let provider = GatewayAuthProvider::new(
            AuthMethod::postgres_scram_sha256("pguser", "pgpass"),
            Some("postgres".to_string()),
            SessionConfig::default(),
            TlsClientConfig::default(),
        );

        let creds = provider.get_auth(&test_context()).await.unwrap();
        assert_eq!(creds.method.username(), "pguser");
        assert_eq!(creds.database, Some("postgres".to_string()));
    }
}
