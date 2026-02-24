//! Static authentication provider.
//!
//! This module provides [`StaticAuthProvider`], an implementation of
//! [`AuthProvider`] that returns the same credentials for all connections.
//! Useful for testing and standalone deployment without Gateway.
//!
//! ## Multi-target support
//!
//! In multi-target mode, the provider stores different credentials for each
//! database protocol (MySQL, PostgreSQL) and returns the appropriate credentials
//! based on the detected protocol type.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use zeroize::Zeroizing;

use crate::config::Config;
use crate::error::Result;
use crate::tls::TlsClientConfig;

use super::{
    AuthCredentials, AuthMethod, AuthProvider, ConnectionContext, DatabaseType, SessionConfig,
    TokenValidation,
};

/// Static authentication provider.
///
/// Returns the same credentials for all connections, loaded from
/// configuration at startup. Useful for:
///
/// - Development and testing
/// - Standalone deployment without Gateway
/// - Single-user scenarios
///
/// ## Multi-target mode
///
/// When created from a multi-target config, returns different credentials
/// based on the detected protocol (MySQL vs PostgreSQL).
///
/// # Example
///
/// ```
/// use keeperdb_proxy::auth::{AuthMethod, StaticAuthProvider};
///
/// let provider = StaticAuthProvider::new(
///     AuthMethod::mysql_native("db_user", "db_password")
/// );
/// ```
pub struct StaticAuthProvider {
    /// Authentication method to use for all connections (single-target mode).
    default_method: AuthMethod,

    /// Per-protocol authentication methods (multi-target mode).
    /// Key is "mysql" or "postgresql"
    protocol_methods: HashMap<String, AuthMethod>,

    /// Per-protocol TLS configurations (multi-target mode).
    protocol_tls: HashMap<String, TlsClientConfig>,

    /// TLS configuration for target connections (single-target mode).
    tls_config: Option<TlsClientConfig>,

    /// Session configuration.
    session_config: SessionConfig,
}

impl StaticAuthProvider {
    /// Create a new StaticAuthProvider with the given authentication method.
    ///
    /// # Arguments
    ///
    /// * `method` - The authentication method to use for all connections
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::{AuthMethod, StaticAuthProvider};
    ///
    /// let provider = StaticAuthProvider::new(
    ///     AuthMethod::mysql_native("root", "secret")
    /// );
    /// ```
    pub fn new(method: AuthMethod) -> Self {
        Self {
            default_method: method,
            protocol_methods: HashMap::new(),
            protocol_tls: HashMap::new(),
            tls_config: None,
            session_config: SessionConfig::default(),
        }
    }

    /// Create a StaticAuthProvider from the existing Config structure.
    ///
    /// Supports both single-target and multi-target configurations:
    /// - Single-target: Uses `target` and `credentials` fields
    /// - Multi-target: Uses `targets` map with per-protocol credentials
    ///
    /// # Arguments
    ///
    /// * `config` - The proxy configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// use keeperdb_proxy::config::Config;
    /// use keeperdb_proxy::auth::StaticAuthProvider;
    ///
    /// let config = Config::load("config.yaml")?;
    /// let provider = StaticAuthProvider::from_config(&config)?;
    /// ```
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut protocol_methods = HashMap::new();
        let mut protocol_tls = HashMap::new();

        // Handle multi-target mode
        if config.is_multi_target() {
            for (protocol, target) in &config.targets {
                // Create appropriate AuthMethod based on protocol
                let method = match protocol.as_str() {
                    "mysql" => AuthMethod::MysqlNativePassword {
                        username: target.credentials.username.clone(),
                        password: Zeroizing::new(target.credentials.password.clone()),
                    },
                    "postgresql" => AuthMethod::PostgresScramSha256 {
                        username: target.credentials.username.clone(),
                        password: Zeroizing::new(target.credentials.password.clone()),
                    },
                    _ => continue, // Skip unknown protocols
                };

                protocol_methods.insert(protocol.clone(), method);

                if target.tls.enabled {
                    protocol_tls.insert(protocol.clone(), target.tls.clone());
                }
            }
        }

        // Create default method from single-target config (backwards compatible)
        let default_method = if let Some(creds) = &config.credentials {
            AuthMethod::MysqlNativePassword {
                username: creds.username.clone(),
                password: Zeroizing::new(creds.password.clone()),
            }
        } else if let Some(mysql_target) = config.get_target("mysql") {
            // Use MySQL from multi-target as default
            AuthMethod::MysqlNativePassword {
                username: mysql_target.credentials.username.clone(),
                password: Zeroizing::new(mysql_target.credentials.password.clone()),
            }
        } else if let Some(pg_target) = config.get_target("postgresql") {
            // Use PostgreSQL from multi-target as default
            AuthMethod::PostgresScramSha256 {
                username: pg_target.credentials.username.clone(),
                password: Zeroizing::new(pg_target.credentials.password.clone()),
            }
        } else {
            // Fallback - shouldn't happen if config validation passed
            AuthMethod::MysqlNativePassword {
                username: String::new(),
                password: Zeroizing::new(String::new()),
            }
        };

        let tls_config = if let Some(target) = &config.target {
            if target.tls.enabled {
                Some(target.tls.clone())
            } else {
                None
            }
        } else {
            None
        };

        let mut session_config = SessionConfig {
            idle_timeout: Some(Duration::from_secs(config.server.idle_timeout_secs)),
            require_token: config.session.require_token,
            connection_grace_period: Some(Duration::from_millis(
                config.session.connection_grace_period_ms,
            )),
            application_lock_timeout: Some(Duration::from_secs(
                config.session.application_lock_timeout_secs,
            )),
            ..SessionConfig::default()
        };

        // Only set limits if non-zero (0 = unlimited)
        if config.session.max_duration_secs > 0 {
            session_config.max_duration =
                Some(Duration::from_secs(config.session.max_duration_secs));
        }
        if config.session.max_queries > 0 {
            session_config.max_queries = Some(config.session.max_queries);
        }
        if config.session.max_connections_per_session > 0 {
            session_config.max_connections_per_session =
                Some(config.session.max_connections_per_session);
        }

        Ok(Self {
            default_method,
            protocol_methods,
            protocol_tls,
            tls_config,
            session_config,
        })
    }

    /// Get the authentication method for a specific database type
    fn get_method_for_type(&self, db_type: DatabaseType) -> &AuthMethod {
        let protocol_key = match db_type {
            DatabaseType::MySQL => "mysql",
            DatabaseType::PostgreSQL => "postgresql",
            DatabaseType::SQLServer => "sqlserver",
            DatabaseType::Oracle => "oracle",
        };

        self.protocol_methods
            .get(protocol_key)
            .unwrap_or(&self.default_method)
    }

    /// Get the TLS configuration for a specific database type
    fn get_tls_for_type(&self, db_type: DatabaseType) -> Option<&TlsClientConfig> {
        let protocol_key = match db_type {
            DatabaseType::MySQL => "mysql",
            DatabaseType::PostgreSQL => "postgresql",
            DatabaseType::SQLServer => "sqlserver",
            DatabaseType::Oracle => "oracle",
        };

        self.protocol_tls
            .get(protocol_key)
            .or(self.tls_config.as_ref())
    }

    /// Set the TLS configuration (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `config` - TLS configuration for target connections
    ///
    /// # Example
    ///
    /// ```ignore
    /// use keeperdb_proxy::auth::{AuthMethod, StaticAuthProvider};
    /// use keeperdb_proxy::tls::TlsClientConfig;
    ///
    /// let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"))
    ///     .with_tls(TlsClientConfig::default());
    /// ```
    pub fn with_tls(mut self, config: TlsClientConfig) -> Self {
        self.tls_config = Some(config);
        self
    }

    /// Set the session configuration (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `config` - Session configuration
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use keeperdb_proxy::auth::{AuthMethod, SessionConfig, StaticAuthProvider};
    ///
    /// let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"))
    ///     .with_session_config(
    ///         SessionConfig::default()
    ///             .with_max_duration(Duration::from_secs(3600))
    ///     );
    /// ```
    pub fn with_session_config(mut self, config: SessionConfig) -> Self {
        self.session_config = config;
        self
    }
}

#[async_trait]
impl AuthProvider for StaticAuthProvider {
    async fn get_auth(&self, context: &ConnectionContext) -> Result<AuthCredentials> {
        let method = self.get_method_for_type(context.database_type);
        Ok(AuthCredentials::new(method.clone()))
    }

    async fn get_tls_config(&self, context: &ConnectionContext) -> Result<Option<TlsClientConfig>> {
        Ok(self.get_tls_for_type(context.database_type).cloned())
    }

    async fn get_session_config(&self, _context: &ConnectionContext) -> Result<SessionConfig> {
        Ok(self.session_config.clone())
    }

    async fn validate_token(&self, _token: &str) -> Result<TokenValidation> {
        // Static provider doesn't use tokens - always valid
        Ok(TokenValidation::valid())
    }
}

// Verify Send + Sync bounds at compile time
#[cfg(test)]
mod send_sync_assertions {
    use super::*;

    ::static_assertions::assert_impl_all!(StaticAuthProvider: Send, Sync);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::DatabaseType;
    use std::net::SocketAddr;

    fn mysql_context() -> ConnectionContext {
        ConnectionContext::new(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            "localhost",
            3306,
            DatabaseType::MySQL,
        )
    }

    fn postgres_context() -> ConnectionContext {
        ConnectionContext::new(
            "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            "localhost",
            5432,
            DatabaseType::PostgreSQL,
        )
    }

    #[tokio::test]
    async fn test_new() {
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("testuser", "testpass"));
        let context = mysql_context();

        let creds = provider.get_auth(&context).await.unwrap();
        assert_eq!(creds.method.username(), "testuser");
        assert_eq!(creds.method.password(), Some("testpass"));
    }

    #[tokio::test]
    async fn test_get_auth_returns_same_credentials_single_target() {
        // In single-target mode (no protocol_methods), same creds for all contexts
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"));

        let creds1 = provider.get_auth(&mysql_context()).await.unwrap();
        let creds2 = provider.get_auth(&postgres_context()).await.unwrap();

        // Same credentials for different contexts (single-target mode)
        assert_eq!(creds1.method.username(), creds2.method.username());
        assert_eq!(creds1.method.password(), creds2.method.password());
    }

    #[tokio::test]
    async fn test_multi_protocol_credentials() {
        // Create a provider with different credentials for each protocol
        let mut protocol_methods = HashMap::new();
        protocol_methods.insert(
            "mysql".to_string(),
            AuthMethod::mysql_native("mysql_user", "mysql_pass"),
        );
        protocol_methods.insert(
            "postgresql".to_string(),
            AuthMethod::postgres_scram_sha256("pg_user", "pg_pass"),
        );

        let provider = StaticAuthProvider {
            default_method: AuthMethod::mysql_native("default", "default"),
            protocol_methods,
            protocol_tls: HashMap::new(),
            tls_config: None,
            session_config: SessionConfig::default(),
        };

        // MySQL should get MySQL credentials
        let mysql_creds = provider.get_auth(&mysql_context()).await.unwrap();
        assert_eq!(mysql_creds.method.username(), "mysql_user");
        assert_eq!(mysql_creds.method.password(), Some("mysql_pass"));

        // PostgreSQL should get PostgreSQL credentials
        let pg_creds = provider.get_auth(&postgres_context()).await.unwrap();
        assert_eq!(pg_creds.method.username(), "pg_user");
        assert_eq!(pg_creds.method.password(), Some("pg_pass"));
    }

    #[tokio::test]
    async fn test_get_tls_config_none() {
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"));
        let context = mysql_context();

        let tls = provider.get_tls_config(&context).await.unwrap();
        assert!(tls.is_none());
    }

    #[tokio::test]
    async fn test_get_session_config() {
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"))
            .with_session_config(
                SessionConfig::default().with_idle_timeout(Duration::from_secs(600)),
            );
        let context = mysql_context();

        let config = provider.get_session_config(&context).await.unwrap();
        assert_eq!(config.idle_timeout, Some(Duration::from_secs(600)));
    }

    #[tokio::test]
    async fn test_validate_token_always_valid() {
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"));

        let result = provider.validate_token("any-token").await.unwrap();
        assert!(result.valid);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_with_session_config() {
        let session = SessionConfig::default().with_max_connections_per_session(1);
        let provider = StaticAuthProvider::new(AuthMethod::mysql_native("user", "pass"))
            .with_session_config(session);

        assert_eq!(provider.session_config.max_connections_per_session, Some(1));
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StaticAuthProvider>();
    }
}
