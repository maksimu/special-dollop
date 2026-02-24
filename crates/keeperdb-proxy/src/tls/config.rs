//! TLS configuration types
//!
//! This module defines configuration structures for TLS/SSL connections:
//! - `TlsServerConfig` for accepting TLS connections from clients
//! - `TlsClientConfig` for connecting to database servers over TLS

use serde::Deserialize;
use std::path::PathBuf;

/// Server-side TLS configuration (proxy accepting client connections)
///
/// This configuration controls how the proxy presents itself to database clients.
/// When enabled, clients can connect using TLS encryption.
///
/// # Example YAML
/// ```yaml
/// server:
///   listen_port: 3307
///   tls:
///     enabled: true
///     cert_path: "/path/to/server.crt"
///     key_path: "/path/to/server.key"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TlsServerConfig {
    /// Enable TLS for client connections
    ///
    /// When `false` (default), clients connect over plain TCP.
    /// When `true`, clients must connect over TLS.
    #[serde(default)]
    pub enabled: bool,

    /// Path to server certificate in PEM format
    ///
    /// This certificate is presented to clients during TLS handshake.
    /// Should be signed by a CA trusted by clients, or be self-signed
    /// for development.
    pub cert_path: Option<PathBuf>,

    /// Path to server private key in PEM format
    ///
    /// The private key corresponding to the certificate.
    /// Can be encrypted (password-protected).
    pub key_path: Option<PathBuf>,

    /// Password for encrypted private key (optional)
    ///
    /// If the private key is encrypted, provide the password here.
    /// Supports environment variable substitution: `${TLS_KEY_PASSWORD}`
    #[serde(default)]
    pub key_password: Option<String>,
}

/// Client-side TLS configuration (proxy connecting to database)
///
/// This configuration controls how the proxy connects to backend databases.
/// When enabled, connections to the database server use TLS encryption.
///
/// # Example YAML
/// ```yaml
/// target:
///   host: "db.example.com"
///   port: 3306
///   tls:
///     enabled: true
///     verify_mode: "verify"
///     ca_path: "/path/to/ca-bundle.crt"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TlsClientConfig {
    /// Enable TLS for database connections
    ///
    /// When `false` (default), connects to database over plain TCP.
    /// When `true`, connects to database over TLS.
    #[serde(default)]
    pub enabled: bool,

    /// Certificate verification mode
    ///
    /// Controls how the proxy verifies the database server's certificate.
    /// Default is `Verify` (full verification).
    #[serde(default)]
    pub verify_mode: TlsVerifyMode,

    /// Path to CA certificate bundle in PEM format
    ///
    /// Used to verify the database server's certificate.
    /// If not specified, uses system CA certificates.
    pub ca_path: Option<PathBuf>,

    /// Path to client certificate in PEM format (optional)
    ///
    /// For certificate-based authentication to the database.
    /// If specified, `client_key_path` must also be specified.
    pub client_cert_path: Option<PathBuf>,

    /// Path to client private key in PEM format (optional)
    ///
    /// The private key for client certificate authentication.
    pub client_key_path: Option<PathBuf>,
}

/// Certificate verification mode for client-side TLS
///
/// Controls how strictly the proxy verifies the database server's certificate.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsVerifyMode {
    /// Full verification: CA chain + hostname match
    ///
    /// This is the default and most secure mode. The server certificate
    /// must be signed by a trusted CA and the hostname must match.
    #[default]
    Verify,

    /// Verify CA chain only, skip hostname check
    ///
    /// The server certificate must be signed by a trusted CA, but the
    /// hostname in the certificate doesn't need to match the target host.
    /// Useful for connecting via IP address when cert has DNS name.
    #[serde(rename = "verify_ca")]
    VerifyCa,

    /// No verification (INSECURE - development only!)
    ///
    /// WARNING: This completely disables certificate verification.
    /// Only use for development and testing, never in production.
    /// Any certificate will be accepted, including self-signed and expired.
    None,
}

impl TlsServerConfig {
    /// Validate the server TLS configuration
    ///
    /// Returns an error if TLS is enabled but required paths are missing.
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled {
            if self.cert_path.is_none() {
                return Err("TLS enabled but cert_path not specified".to_string());
            }
            if self.key_path.is_none() {
                return Err("TLS enabled but key_path not specified".to_string());
            }
        }
        Ok(())
    }
}

impl TlsClientConfig {
    /// Validate the client TLS configuration
    ///
    /// Returns an error if client cert is specified but key is missing, or vice versa.
    pub fn validate(&self) -> Result<(), String> {
        if self.client_cert_path.is_some() != self.client_key_path.is_some() {
            return Err(
                "client_cert_path and client_key_path must both be specified or both omitted"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_server_config_defaults() {
        let config: TlsServerConfig = serde_yaml::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert!(config.cert_path.is_none());
        assert!(config.key_path.is_none());
        assert!(config.key_password.is_none());
    }

    #[test]
    fn test_tls_server_config_full() {
        let yaml = r#"
            enabled: true
            cert_path: /path/to/cert.pem
            key_path: /path/to/key.pem
            key_password: secret123
        "#;
        let config: TlsServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(
            config.cert_path.unwrap(),
            PathBuf::from("/path/to/cert.pem")
        );
        assert_eq!(config.key_path.unwrap(), PathBuf::from("/path/to/key.pem"));
        assert_eq!(config.key_password.unwrap(), "secret123");
    }

    #[test]
    fn test_tls_client_config_defaults() {
        let config: TlsClientConfig = serde_yaml::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert_eq!(config.verify_mode, TlsVerifyMode::Verify);
        assert!(config.ca_path.is_none());
        assert!(config.client_cert_path.is_none());
        assert!(config.client_key_path.is_none());
    }

    #[test]
    fn test_tls_client_config_full() {
        let yaml = r#"
            enabled: true
            verify_mode: verify_ca
            ca_path: /path/to/ca.pem
            client_cert_path: /path/to/client.crt
            client_key_path: /path/to/client.key
        "#;
        let config: TlsClientConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.verify_mode, TlsVerifyMode::VerifyCa);
        assert_eq!(config.ca_path.unwrap(), PathBuf::from("/path/to/ca.pem"));
        assert_eq!(
            config.client_cert_path.unwrap(),
            PathBuf::from("/path/to/client.crt")
        );
        assert_eq!(
            config.client_key_path.unwrap(),
            PathBuf::from("/path/to/client.key")
        );
    }

    #[test]
    fn test_tls_verify_mode_parsing() {
        let verify: TlsVerifyMode = serde_yaml::from_str("verify").unwrap();
        assert_eq!(verify, TlsVerifyMode::Verify);

        let verify_ca: TlsVerifyMode = serde_yaml::from_str("verify_ca").unwrap();
        assert_eq!(verify_ca, TlsVerifyMode::VerifyCa);

        let none: TlsVerifyMode = serde_yaml::from_str("none").unwrap();
        assert_eq!(none, TlsVerifyMode::None);
    }

    #[test]
    fn test_tls_server_validation_enabled_missing_cert() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: None,
            key_path: Some(PathBuf::from("/key.pem")),
            key_password: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_server_validation_enabled_missing_key() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: None,
            key_password: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_server_validation_enabled_valid() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: Some(PathBuf::from("/key.pem")),
            key_password: None,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_tls_server_validation_disabled() {
        let config = TlsServerConfig {
            enabled: false,
            cert_path: None,
            key_path: None,
            key_password: None,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_tls_client_validation_partial_client_cert() {
        let config = TlsClientConfig {
            enabled: true,
            verify_mode: TlsVerifyMode::Verify,
            ca_path: None,
            client_cert_path: Some(PathBuf::from("/client.crt")),
            client_key_path: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_client_validation_valid_with_client_cert() {
        let config = TlsClientConfig {
            enabled: true,
            verify_mode: TlsVerifyMode::Verify,
            ca_path: None,
            client_cert_path: Some(PathBuf::from("/client.crt")),
            client_key_path: Some(PathBuf::from("/client.key")),
        };
        assert!(config.validate().is_ok());
    }
}
