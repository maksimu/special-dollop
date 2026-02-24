//! TLS Acceptor for server-side TLS connections
//!
//! This module provides `TlsAcceptor` which upgrades incoming TCP connections
//! to TLS-encrypted connections. Used for accepting encrypted connections
//! from database clients.

use std::sync::Arc;

use rustls::ServerConfig;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

use crate::tls::{load_certificates, load_private_key, TlsError, TlsServerConfig};

/// TLS Acceptor for upgrading TCP connections to TLS
///
/// Wraps `tokio_rustls::TlsAcceptor` with configuration loading
/// and error handling.
///
/// # Example
///
/// ```ignore
/// let config = TlsServerConfig {
///     enabled: true,
///     cert_path: Some("/path/to/cert.pem".into()),
///     key_path: Some("/path/to/key.pem".into()),
///     key_password: None,
/// };
///
/// let acceptor = TlsAcceptor::new(&config)?;
/// let tls_stream = acceptor.accept(tcp_stream).await?;
/// ```
#[derive(Clone)]
pub struct TlsAcceptor {
    inner: tokio_rustls::TlsAcceptor,
}

impl TlsAcceptor {
    /// Create a new TLS acceptor from configuration
    ///
    /// Loads the server certificate and private key from the paths
    /// specified in the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `cert_path` or `key_path` is not specified
    /// - Certificate file cannot be read or parsed
    /// - Private key file cannot be read or parsed
    /// - TLS configuration is invalid
    pub fn new(config: &TlsServerConfig) -> Result<Self, TlsError> {
        // Validate config
        config.validate().map_err(TlsError::config)?;

        let cert_path = config
            .cert_path
            .as_ref()
            .ok_or_else(|| TlsError::config("cert_path is required"))?;

        let key_path = config
            .key_path
            .as_ref()
            .ok_or_else(|| TlsError::config("key_path is required"))?;

        // Load certificates
        let certs = load_certificates(cert_path)?;
        if certs.is_empty() {
            return Err(TlsError::cert_load(
                cert_path,
                "no certificates found in file",
            ));
        }

        // Load private key
        let key = load_private_key(key_path)?;

        let provider = rustls::crypto::ring::default_provider();

        // Build rustls ServerConfig
        let server_config = ServerConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsError::config(format!("Failed to set protocol versions: {}", e)))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| TlsError::config(format!("Failed to build TLS config: {}", e)))?;

        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        Ok(Self { inner: acceptor })
    }

    /// Upgrade a TCP stream to TLS
    ///
    /// Performs the TLS handshake with the client. This is an async
    /// operation that may take some time depending on network conditions.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The TLS handshake fails
    /// - The client disconnects during handshake
    /// - The client presents invalid TLS data
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream<TcpStream>, TlsError> {
        self.inner
            .accept(stream)
            .await
            .map_err(|e| TlsError::handshake(e.to_string()))
    }

    /// Upgrade any async stream to TLS
    ///
    /// Generic version of `accept` that works with any stream implementing
    /// `AsyncRead + AsyncWrite + Unpin`. Useful for TDS-wrapped TLS where
    /// the underlying stream is not a plain TcpStream.
    pub async fn accept_stream<S>(&self, stream: S) -> Result<TlsStream<S>, TlsError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.inner
            .accept(stream)
            .await
            .map_err(|e| TlsError::handshake(e.to_string()))
    }

    /// Create a TLS 1.2-only acceptor for SQL Server compatibility
    ///
    /// Some SQL Server clients don't properly handle TLS 1.3. This creates
    /// an acceptor that only supports TLS 1.2.
    pub fn new_tls12_only(config: &TlsServerConfig) -> Result<Self, TlsError> {
        // Validate config
        config.validate().map_err(TlsError::config)?;

        let cert_path = config
            .cert_path
            .as_ref()
            .ok_or_else(|| TlsError::config("cert_path is required"))?;

        let key_path = config
            .key_path
            .as_ref()
            .ok_or_else(|| TlsError::config("key_path is required"))?;

        // Load certificates
        let certs = load_certificates(cert_path)?;
        if certs.is_empty() {
            return Err(TlsError::cert_load(
                cert_path,
                "no certificates found in file",
            ));
        }

        // Load private key
        let key = load_private_key(key_path)?;

        let provider = rustls::crypto::ring::default_provider();

        // Build rustls ServerConfig with TLS 1.2 only
        let server_config = ServerConfig::builder_with_provider(Arc::new(provider))
            .with_protocol_versions(&[&rustls::version::TLS12])
            .map_err(|e| TlsError::config(format!("Failed to set protocol versions: {}", e)))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| TlsError::config(format!("Failed to build TLS config: {}", e)))?;

        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        Ok(Self { inner: acceptor })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_acceptor_missing_cert_path() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: None,
            key_path: Some(PathBuf::from("/key.pem")),
            key_password: None,
        };

        let result = TlsAcceptor::new(&config);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("cert_path") || err.contains("TLS enabled"));
    }

    #[test]
    fn test_acceptor_missing_key_path() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: None,
            key_password: None,
        };

        let result = TlsAcceptor::new(&config);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("key_path") || err.contains("TLS enabled"));
    }

    #[test]
    fn test_acceptor_nonexistent_cert_file() {
        let config = TlsServerConfig {
            enabled: true,
            cert_path: Some(PathBuf::from("/nonexistent/cert.pem")),
            key_path: Some(PathBuf::from("/nonexistent/key.pem")),
            key_password: None,
        };

        let result = TlsAcceptor::new(&config);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("certificate") || err.contains("cert"));
    }
}
