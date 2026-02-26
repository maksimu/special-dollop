//! Configuration types

use serde::Deserialize;
use std::collections::HashMap;

use crate::query_logging::QueryLoggingConfig;
use crate::tls::{TlsClientConfig, TlsServerConfig};

/// Root configuration structure
///
/// Supports two configuration modes:
/// 1. **Single-target mode** (backwards compatible): Uses `target` and `credentials` fields
/// 2. **Multi-target mode**: Uses `targets` map with per-protocol configuration
///
/// # Multi-target Example
///
/// ```yaml
/// server:
///   listen_port: 3307
///
/// targets:
///   mysql:
///     host: "mysql.example.com"
///     port: 3306
///     credentials:
///       username: "root"
///       password: "secret"
///   postgresql:
///     host: "postgres.example.com"
///     port: 5432
///     credentials:
///       username: "postgres"
///       password: "secret"
/// ```
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Server configuration
    pub server: ServerConfig,

    /// Multi-target configuration (preferred)
    /// Key is protocol name: "mysql" or "postgresql"
    #[serde(default)]
    pub targets: HashMap<String, TargetWithCredentials>,

    /// Target database configuration (single-target mode, backwards compatible)
    #[serde(default)]
    pub target: Option<TargetConfig>,

    /// Credentials to inject (single-target mode, backwards compatible)
    #[serde(default)]
    pub credentials: Option<CredentialsConfig>,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Session security configuration
    #[serde(default)]
    pub session: SessionSecurityConfig,

    /// Query logging and session recording configuration
    #[serde(default)]
    pub query_logging: QueryLoggingConfig,
}

impl Config {
    /// Get the target configuration for a specific protocol
    pub fn get_target(&self, protocol: &str) -> Option<&TargetWithCredentials> {
        // First check multi-target mode
        if let Some(target) = self.targets.get(protocol) {
            return Some(target);
        }
        None
    }

    /// Get the single-target configuration (for backwards compatibility)
    /// Returns a TargetWithCredentials if both target and credentials are set
    pub fn get_single_target(&self) -> Option<TargetWithCredentials> {
        match (&self.target, &self.credentials) {
            (Some(target), Some(creds)) => Some(TargetWithCredentials {
                host: target.host.clone(),
                port: target.port,
                database: target.database.clone(),
                tls: target.tls.clone(),
                credentials: creds.clone(),
            }),
            _ => None,
        }
    }

    /// Check if this config is in multi-target mode
    pub fn is_multi_target(&self) -> bool {
        !self.targets.is_empty()
    }

    /// Check if credentials are configured (settings mode vs gateway mode)
    pub fn has_credentials(&self) -> bool {
        // Check multi-target mode
        if !self.targets.is_empty() {
            return true;
        }
        // Check single-target mode
        self.credentials.is_some()
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        let has_targets = !self.targets.is_empty();
        let has_single_target = self.target.is_some() && self.credentials.is_some();

        // Gateway mode: no credentials required (they come from handshake)
        // Only need server config
        if !has_targets && !has_single_target {
            // This is gateway mode - valid as long as server is configured
            return Ok(());
        }

        // Validate target names in multi-target mode
        for key in self.targets.keys() {
            match key.as_str() {
                "mysql" | "postgresql" | "sqlserver" => {}
                _ => {
                    return Err(format!(
                    "Invalid target protocol '{}'. Must be 'mysql', 'postgresql', or 'sqlserver'",
                    key
                ))
                }
            }
        }

        Ok(())
    }

    /// Get the effective target for a given protocol.
    ///
    /// This method handles both single-target and multi-target modes:
    /// - In multi-target mode: Returns the target for the specified protocol
    /// - In single-target mode: Returns the single target for any protocol
    ///
    /// Returns None if no target is configured for the protocol.
    pub fn get_effective_target(&self, protocol: &str) -> Option<TargetWithCredentials> {
        // First check multi-target mode
        if let Some(target) = self.targets.get(protocol) {
            return Some(target.clone());
        }

        // Fall back to single-target mode
        self.get_single_target()
    }
}

/// Target configuration with embedded credentials (for multi-target mode)
#[derive(Debug, Clone, Deserialize)]
pub struct TargetWithCredentials {
    /// Target database host
    pub host: String,
    /// Target database port
    pub port: u16,
    /// Database name (optional)
    #[serde(default)]
    pub database: Option<String>,
    /// TLS configuration for connecting to database server
    #[serde(default)]
    pub tls: TlsClientConfig,
    /// Credentials for this target
    pub credentials: CredentialsConfig,
}

/// Server listener configuration
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// Address to listen on
    #[serde(default = "default_listen_address")]
    pub listen_address: String,
    /// Port to listen on
    pub listen_port: u16,
    /// Connection timeout in seconds
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    /// Idle timeout in seconds
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// Maximum number of concurrent connections (0 = unlimited)
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// TLS configuration for accepting client connections
    #[serde(default)]
    pub tls: TlsServerConfig,
}

/// Target database configuration (for single-target backwards compatibility)
#[derive(Debug, Clone, Deserialize)]
pub struct TargetConfig {
    /// Target database host
    pub host: String,
    /// Target database port
    pub port: u16,
    /// Database name (optional)
    pub database: Option<String>,
    /// TLS configuration for connecting to database server
    #[serde(default)]
    pub tls: TlsClientConfig,
}

/// Credentials to inject during authentication
#[derive(Debug, Clone, Deserialize)]
pub struct CredentialsConfig {
    /// Username to use when connecting to target
    pub username: String,
    /// Password to use when connecting to target
    pub password: String,
}

/// Logging configuration
#[derive(Debug, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Whether to log protocol details
    #[serde(default)]
    pub protocol_debug: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            protocol_debug: false,
        }
    }
}

fn default_listen_address() -> String {
    "127.0.0.1".to_string()
}

fn default_connect_timeout() -> u64 {
    30
}

fn default_idle_timeout() -> u64 {
    300
}

fn default_max_connections() -> usize {
    1000 // Reasonable default for production
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Session security configuration
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionSecurityConfig {
    /// Maximum session duration in seconds (0 = unlimited)
    #[serde(default)]
    pub max_duration_secs: u64,

    /// Idle timeout in seconds (0 = unlimited)
    /// Default: 300 seconds (5 minutes)
    #[serde(default = "default_session_idle_timeout")]
    pub idle_timeout_secs: u64,

    /// Maximum number of queries per session (0 = unlimited)
    #[serde(default)]
    pub max_queries: u64,

    /// Maximum connections per session (0 = unlimited)
    #[serde(default)]
    pub max_connections_per_session: u32,

    /// Grace period in milliseconds for establishing connections
    /// During this window after the first connection, additional connections are allowed
    /// After the grace period, no new connections are accepted (if limits are set)
    /// Default: 1000ms (1 second)
    #[serde(default = "default_connection_grace_period")]
    pub connection_grace_period_ms: u64,

    /// Application lock timeout in seconds.
    /// After all connections disconnect, this is how long the application fingerprint
    /// lock is retained before allowing a different tool to connect.
    /// Default: 5 seconds
    #[serde(default = "default_application_lock_timeout")]
    pub application_lock_timeout_secs: u64,

    /// Require token validation for connections
    #[serde(default)]
    pub require_token: bool,
}

fn default_session_idle_timeout() -> u64 {
    300 // 5 minutes default idle timeout
}

fn default_connection_grace_period() -> u64 {
    1000 // 1 second default grace period
}

fn default_application_lock_timeout() -> u64 {
    5 // 5 seconds default - time to retain app lock after disconnect
}
