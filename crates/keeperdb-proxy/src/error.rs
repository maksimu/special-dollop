//! Error types for keeperdb-proxy

use thiserror::Error;

use crate::tls::TlsError;

/// Main error type for the proxy
#[derive(Error, Debug)]
pub enum ProxyError {
    /// I/O error (network, file)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Protocol parsing error
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Authentication error
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Connection error
    #[error("Connection error: {0}")]
    Connection(String),

    /// Timeout error
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Unsupported protocol
    #[error("Unsupported protocol: {0}")]
    UnsupportedProtocol(String),

    /// Unsupported authentication method
    #[error("Unsupported authentication method: {0}")]
    UnsupportedAuthMethod(String),

    /// Token validation failure
    #[error("Token validation failed: {0}")]
    TokenValidation(String),

    /// Credential retrieval failure
    #[error("Failed to retrieve credentials: {0}")]
    CredentialRetrieval(String),

    /// TLS/SSL error
    #[error("TLS error: {0}")]
    Tls(#[from] TlsError),

    /// Session limit reached
    #[error("Session limit: {0}")]
    SessionLimit(String),
}

/// Result type alias for ProxyError
pub type Result<T> = std::result::Result<T, ProxyError>;

impl From<serde_yaml::Error> for ProxyError {
    fn from(err: serde_yaml::Error) -> Self {
        ProxyError::Config(err.to_string())
    }
}
