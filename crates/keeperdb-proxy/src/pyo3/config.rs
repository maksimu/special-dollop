//! Configuration parsing for PyO3 bindings.
//!
//! This module handles converting Python dictionaries into Rust configuration types.

use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::auth::{AuthMethod, DatabaseType, SessionConfig};
use crate::tls::{TlsClientConfig, TlsVerifyMode};

/// Parsed database parameters from Python.
///
/// This struct is populated from a Python dictionary and contains
/// all the configuration needed to start a proxy instance.
///
/// Supports two modes:
/// - **Settings mode**: All fields including credentials are provided upfront
/// - **Gateway mode**: No credentials (username/password) - they come via handshake
pub struct PyDbParams {
    /// Database type (mysql or postgresql) - required in settings mode, optional in gateway mode
    pub database_type: Option<DatabaseType>,

    /// Target database host - required in settings mode, optional in gateway mode
    pub target_host: Option<String>,

    /// Target database port - required in settings mode, optional in gateway mode
    pub target_port: Option<u16>,

    /// Database username (None = gateway mode)
    pub username: Option<String>,

    /// Database password (None = gateway mode)
    pub password: Option<String>,

    /// Optional database name
    pub database: Option<String>,

    /// Authentication method name (e.g., "native_password", "caching_sha2")
    pub auth_method: Option<String>,

    /// Session configuration
    pub session_config: SessionConfig,

    /// TLS configuration for backend connection
    pub tls_config: TlsClientConfig,

    /// Proxy listen host (default: "127.0.0.1")
    pub listen_host: String,

    /// Proxy listen port (default: 0 for auto-assign)
    pub listen_port: u16,
}

impl PyDbParams {
    /// Parse a Python dictionary into PyDbParams.
    ///
    /// # Arguments
    ///
    /// * `dict` - Python dictionary with database parameters
    ///
    /// # Settings Mode (credentials provided)
    ///
    /// Required keys:
    /// * `database_type` - "mysql" or "postgresql"
    /// * `target_host` - Target database hostname
    /// * `target_port` - Target database port
    /// * `username` - Database username
    /// * `password` - Database password
    ///
    /// # Gateway Mode (no credentials)
    ///
    /// Required keys:
    /// * `listen_host` and/or `listen_port` - Where to listen
    ///
    /// # Optional keys (both modes)
    ///
    /// * `database` - Database name
    /// * `auth_method` - Authentication method name
    /// * `session_config` - Dict with session limits
    /// * `tls_config` - Dict with TLS settings
    /// * `listen_host` - Proxy listen host (default: "127.0.0.1")
    /// * `listen_port` - Proxy listen port (default: 0)
    pub fn from_py_dict(dict: &Bound<'_, PyDict>) -> PyResult<Self> {
        // Database type (required for settings mode, optional for gateway mode)
        let database_type: Option<DatabaseType> = dict
            .get_item("database_type")?
            .map(|v| {
                let s: String = v.extract()?;
                parse_database_type(&s)
            })
            .transpose()?;

        // Target host/port (required for settings mode, optional for gateway mode)
        let target_host: Option<String> = dict
            .get_item("target_host")?
            .map(|v| v.extract())
            .transpose()?;

        let target_port: Option<u16> = dict
            .get_item("target_port")?
            .map(|v| v.extract())
            .transpose()?;

        // Credentials (None = gateway mode)
        let username: Option<String> = dict
            .get_item("username")?
            .map(|v| v.extract())
            .transpose()?;

        let password: Option<String> = dict
            .get_item("password")?
            .map(|v| v.extract())
            .transpose()?;

        // Optional fields
        let database: Option<String> = dict
            .get_item("database")?
            .map(|v| v.extract())
            .transpose()?;

        let auth_method: Option<String> = dict
            .get_item("auth_method")?
            .map(|v| v.extract())
            .transpose()?;

        let listen_host: String = dict
            .get_item("listen_host")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or_else(|| "127.0.0.1".to_string());

        let listen_port: u16 = dict
            .get_item("listen_port")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(0);

        // Parse nested session_config
        let session_config = if let Some(session_dict) = dict.get_item("session_config")? {
            let session_dict: &Bound<'_, PyDict> = session_dict.cast()?;
            parse_session_config(session_dict)?
        } else {
            SessionConfig::default()
        };

        // Parse nested tls_config
        let tls_config = if let Some(tls_dict) = dict.get_item("tls_config")? {
            let tls_dict: &Bound<'_, PyDict> = tls_dict.cast()?;
            parse_tls_config(tls_dict)?
        } else {
            TlsClientConfig::default()
        };

        let params = Self {
            database_type,
            target_host,
            target_port,
            username,
            password,
            database,
            auth_method,
            session_config,
            tls_config,
            listen_host,
            listen_port,
        };

        // Validate the configuration
        params.validate()?;

        Ok(params)
    }

    /// Check if this is gateway mode (no credentials provided).
    pub fn is_gateway_mode(&self) -> bool {
        self.username.is_none() || self.password.is_none()
    }

    /// Validate the configuration.
    ///
    /// - Settings mode: requires database_type, target_host, target_port, username, password
    /// - Gateway mode: requires listen config
    fn validate(&self) -> PyResult<()> {
        if self.is_gateway_mode() {
            // Gateway mode: just need listen config (has defaults, so always valid)
            // But warn if credentials are partially provided
            if self.username.is_some() != self.password.is_some() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "Both username and password must be provided together, or neither for gateway mode"
                ));
            }
            Ok(())
        } else {
            // Settings mode: need full target config
            if self.database_type.is_none() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "missing required field for settings mode: database_type",
                ));
            }
            if self.target_host.is_none() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "missing required field for settings mode: target_host",
                ));
            }
            if self.target_port.is_none() {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "missing required field for settings mode: target_port",
                ));
            }
            Ok(())
        }
    }

    /// Create an AuthMethod from the parsed parameters.
    ///
    /// Uses the `auth_method` field to determine which variant to create,
    /// defaulting to the standard method for each database type.
    ///
    /// Returns an error if called in gateway mode (credentials not available).
    pub fn to_auth_method(&self) -> std::result::Result<AuthMethod, &'static str> {
        let db_type = self
            .database_type
            .ok_or("to_auth_method called in gateway mode")?;
        let username = self
            .username
            .as_ref()
            .ok_or("to_auth_method called without username")?;
        let password = self
            .password
            .as_ref()
            .ok_or("to_auth_method called without password")?;

        Ok(match db_type {
            DatabaseType::MySQL => {
                match self.auth_method.as_deref() {
                    Some("caching_sha2") | Some("caching_sha2_password") => {
                        AuthMethod::mysql_caching_sha2(username, password)
                    }
                    // Default to native_password for MySQL
                    _ => AuthMethod::mysql_native(username, password),
                }
            }
            DatabaseType::PostgreSQL => {
                match self.auth_method.as_deref() {
                    Some("md5") => AuthMethod::postgres_md5(username, password),
                    // Default to SCRAM-SHA-256 for PostgreSQL
                    _ => AuthMethod::postgres_scram_sha256(username, password),
                }
            }
            DatabaseType::SQLServer => {
                // SQL Server support is in development (Issue #3)
                // For now, just use MySQL native password as a placeholder
                // This will need to be updated when SQL Server auth is implemented
                AuthMethod::mysql_native(username, password)
            }
            DatabaseType::Oracle => {
                // Oracle uses O5LOGON authentication
                // For settings mode, we use native password format as placeholder
                AuthMethod::mysql_native(username, password)
            }
        })
    }
}

/// Parse database type from string.
fn parse_database_type(s: &str) -> PyResult<DatabaseType> {
    match s.to_lowercase().as_str() {
        "mysql" => Ok(DatabaseType::MySQL),
        "postgresql" | "postgres" => Ok(DatabaseType::PostgreSQL),
        "sqlserver" | "mssql" => Ok(DatabaseType::SQLServer),
        "oracle" => Ok(DatabaseType::Oracle),
        _ => Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
            "invalid database_type '{}': must be 'mysql', 'postgresql', 'sqlserver', or 'oracle'",
            s
        ))),
    }
}

/// Parse session configuration from Python dict.
fn parse_session_config(dict: &Bound<'_, PyDict>) -> PyResult<SessionConfig> {
    let mut config = SessionConfig::default();

    // max_duration_secs
    if let Some(val) = dict.get_item("max_duration_secs")? {
        let secs: u64 = val.extract()?;
        if secs > 0 {
            config.max_duration = Some(Duration::from_secs(secs));
        }
    }

    // idle_timeout_secs
    if let Some(val) = dict.get_item("idle_timeout_secs")? {
        let secs: u64 = val.extract()?;
        if secs > 0 {
            config.idle_timeout = Some(Duration::from_secs(secs));
        }
    }

    // max_queries
    if let Some(val) = dict.get_item("max_queries")? {
        let count: u64 = val.extract()?;
        if count > 0 {
            config.max_queries = Some(count);
        }
    }

    // max_connections
    if let Some(val) = dict.get_item("max_connections")? {
        let max: u32 = val.extract()?;
        if max > 0 {
            config.max_connections_per_session = Some(max);
        }
    }

    // grace_period_ms
    if let Some(val) = dict.get_item("grace_period_ms")? {
        let ms: u64 = val.extract()?;
        config.connection_grace_period = Some(Duration::from_millis(ms));
    }

    // require_token
    if let Some(val) = dict.get_item("require_token")? {
        config.require_token = val.extract()?;
    }

    // application_lock_timeout_secs
    if let Some(val) = dict.get_item("application_lock_timeout_secs")? {
        let secs: u64 = val.extract()?;
        if secs > 0 {
            config.application_lock_timeout = Some(Duration::from_secs(secs));
        }
    }

    Ok(config)
}

/// Parse TLS configuration from Python dict.
fn parse_tls_config(dict: &Bound<'_, PyDict>) -> PyResult<TlsClientConfig> {
    let mut config = TlsClientConfig::default();

    // enabled
    if let Some(val) = dict.get_item("enabled")? {
        config.enabled = val.extract()?;
    }

    // verify_mode
    if let Some(val) = dict.get_item("verify_mode")? {
        let mode_str: String = val.extract()?;
        config.verify_mode = parse_verify_mode(&mode_str)?;
    }

    // ca_path
    if let Some(val) = dict.get_item("ca_path")? {
        let path: String = val.extract()?;
        config.ca_path = Some(path.into());
    }

    // client_cert_path
    if let Some(val) = dict.get_item("client_cert_path")? {
        let path: String = val.extract()?;
        config.client_cert_path = Some(path.into());
    }

    // client_key_path
    if let Some(val) = dict.get_item("client_key_path")? {
        let path: String = val.extract()?;
        config.client_key_path = Some(path.into());
    }

    // Validate
    config.validate().map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("invalid tls_config: {}", e))
    })?;

    Ok(config)
}

/// Parse TLS verify mode from string.
fn parse_verify_mode(s: &str) -> PyResult<TlsVerifyMode> {
    match s.to_lowercase().as_str() {
        "verify" | "full" => Ok(TlsVerifyMode::Verify),
        "verify_ca" | "ca" => Ok(TlsVerifyMode::VerifyCa),
        "none" | "disabled" => Ok(TlsVerifyMode::None),
        _ => Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
            "invalid verify_mode '{}': must be 'verify', 'verify_ca', or 'none'",
            s
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_database_type() {
        assert!(matches!(
            parse_database_type("mysql").unwrap(),
            DatabaseType::MySQL
        ));
        assert!(matches!(
            parse_database_type("MySQL").unwrap(),
            DatabaseType::MySQL
        ));
        assert!(matches!(
            parse_database_type("postgresql").unwrap(),
            DatabaseType::PostgreSQL
        ));
        assert!(matches!(
            parse_database_type("postgres").unwrap(),
            DatabaseType::PostgreSQL
        ));
        assert!(parse_database_type("invalid").is_err());
    }

    #[test]
    fn test_parse_database_type_sqlserver() {
        assert!(matches!(
            parse_database_type("sqlserver").unwrap(),
            DatabaseType::SQLServer
        ));
        assert!(matches!(
            parse_database_type("mssql").unwrap(),
            DatabaseType::SQLServer
        ));
        assert!(matches!(
            parse_database_type("SQLSERVER").unwrap(),
            DatabaseType::SQLServer
        ));
        assert!(matches!(
            parse_database_type("MSSQL").unwrap(),
            DatabaseType::SQLServer
        ));
    }

    #[test]
    fn test_parse_database_type_case_insensitive() {
        // MySQL variants
        assert!(parse_database_type("MYSQL").is_ok());
        assert!(parse_database_type("MySql").is_ok());
        assert!(parse_database_type("mySQL").is_ok());

        // PostgreSQL variants
        assert!(parse_database_type("POSTGRESQL").is_ok());
        assert!(parse_database_type("PostgreSQL").is_ok());
        assert!(parse_database_type("POSTGRES").is_ok());
        assert!(parse_database_type("Postgres").is_ok());
    }

    #[test]
    fn test_parse_database_type_invalid_error_message() {
        let err = parse_database_type("oracle").unwrap_err();
        let err_msg = err.to_string();
        // Error should mention the invalid value
        assert!(err_msg.contains("oracle"));
        // Error should suggest valid options
        assert!(err_msg.contains("mysql") || err_msg.contains("postgresql"));
    }

    #[test]
    fn test_parse_database_type_empty_string() {
        let err = parse_database_type("").unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("invalid"));
    }

    #[test]
    fn test_parse_verify_mode() {
        assert_eq!(parse_verify_mode("verify").unwrap(), TlsVerifyMode::Verify);
        assert_eq!(parse_verify_mode("full").unwrap(), TlsVerifyMode::Verify);
        assert_eq!(
            parse_verify_mode("verify_ca").unwrap(),
            TlsVerifyMode::VerifyCa
        );
        assert_eq!(parse_verify_mode("ca").unwrap(), TlsVerifyMode::VerifyCa);
        assert_eq!(parse_verify_mode("none").unwrap(), TlsVerifyMode::None);
        assert_eq!(parse_verify_mode("disabled").unwrap(), TlsVerifyMode::None);
        assert!(parse_verify_mode("invalid").is_err());
    }

    #[test]
    fn test_parse_verify_mode_case_insensitive() {
        assert_eq!(parse_verify_mode("VERIFY").unwrap(), TlsVerifyMode::Verify);
        assert_eq!(parse_verify_mode("Verify").unwrap(), TlsVerifyMode::Verify);
        assert_eq!(parse_verify_mode("NONE").unwrap(), TlsVerifyMode::None);
        assert_eq!(parse_verify_mode("None").unwrap(), TlsVerifyMode::None);
        assert_eq!(
            parse_verify_mode("VERIFY_CA").unwrap(),
            TlsVerifyMode::VerifyCa
        );
    }

    #[test]
    fn test_parse_verify_mode_invalid_error_message() {
        let err = parse_verify_mode("invalid_mode").unwrap_err();
        let err_msg = err.to_string();
        // Error should mention the invalid value
        assert!(err_msg.contains("invalid_mode"));
        // Error should suggest valid options
        assert!(err_msg.contains("verify") || err_msg.contains("none"));
    }
}
