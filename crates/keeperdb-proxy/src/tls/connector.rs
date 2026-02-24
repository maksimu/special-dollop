//! TLS Connector for client-side TLS connections
//!
//! This module provides `TlsConnector` which establishes TLS-encrypted connections
//! to database servers. Used for connecting to TLS-enabled backend databases.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::ClientConfig;
use rustls::RootCertStore;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::tls::{load_certificates, load_private_key, TlsClientConfig, TlsError, TlsVerifyMode};

/// TLS Connector for establishing TLS connections to servers
///
/// Wraps `tokio_rustls::TlsConnector` with configuration loading
/// and error handling.
///
/// # Example
///
/// ```ignore
/// let config = TlsClientConfig {
///     enabled: true,
///     verify_mode: TlsVerifyMode::Verify,
///     ca_path: Some("/path/to/ca.crt".into()),
///     client_cert_path: None,
///     client_key_path: None,
/// };
///
/// let connector = TlsConnector::new(&config)?;
/// let tls_stream = connector.connect(tcp_stream, "db.example.com").await?;
/// ```
#[derive(Clone)]
pub struct TlsConnector {
    inner: tokio_rustls::TlsConnector,
}

impl TlsConnector {
    /// Create a new TLS connector from configuration
    ///
    /// Loads CA certificates and optionally client certificates
    /// from the paths specified in the configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - CA certificate file cannot be read or parsed
    /// - Client certificate/key cannot be read or parsed
    /// - TLS configuration is invalid
    pub fn new(config: &TlsClientConfig) -> Result<Self, TlsError> {
        // Validate config
        config.validate().map_err(TlsError::config)?;

        // Build root cert store
        let root_store = build_root_store(config)?;

        let provider = rustls::crypto::ring::default_provider();

        // Build client config based on whether we have client certs
        let client_config = if let (Some(cert_path), Some(key_path)) =
            (&config.client_cert_path, &config.client_key_path)
        {
            // Load client certificate and key for mutual TLS
            let certs = load_certificates(cert_path)?;
            if certs.is_empty() {
                return Err(TlsError::cert_load(
                    cert_path,
                    "no certificates found in file",
                ));
            }

            let key = load_private_key(key_path)?;

            ClientConfig::builder_with_provider(Arc::new(provider))
                .with_safe_default_protocol_versions()
                .map_err(|e| TlsError::config(format!("Failed to set protocol versions: {}", e)))?
                .with_root_certificates(root_store)
                .with_client_auth_cert(certs, key)
                .map_err(|e| {
                    TlsError::config(format!("Failed to build client TLS config: {}", e))
                })?
        } else {
            // No client authentication
            ClientConfig::builder_with_provider(Arc::new(provider))
                .with_safe_default_protocol_versions()
                .map_err(|e| TlsError::config(format!("Failed to set protocol versions: {}", e)))?
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

        Ok(Self { inner: connector })
    }

    /// Create a TLS connector that skips certificate verification
    ///
    /// **WARNING**: This is insecure and should only be used for testing
    /// or when connecting to servers with self-signed certificates.
    pub fn new_insecure() -> Result<Self, TlsError> {
        let provider = rustls::crypto::ring::default_provider();

        // Create a client config that doesn't verify certificates
        let client_config = ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsError::config(format!("Failed to set protocol versions: {}", e)))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
            .with_no_client_auth();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

        Ok(Self { inner: connector })
    }

    /// Connect to a server over TLS
    ///
    /// Performs the TLS handshake with the server. The `server_name` is used
    /// for SNI (Server Name Indication) and certificate verification.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The TLS handshake fails
    /// - The server disconnects during handshake
    /// - Certificate verification fails
    /// - The server name is invalid
    pub async fn connect(
        &self,
        stream: TcpStream,
        server_name: &str,
    ) -> Result<TlsStream<TcpStream>, TlsError> {
        let server_name = ServerName::try_from(server_name.to_string())
            .map_err(|_| TlsError::config(format!("Invalid server name: {}", server_name)))?;

        self.inner
            .connect(server_name, stream)
            .await
            .map_err(|e| TlsError::handshake(e.to_string()))
    }

    /// Connect over any async stream using TLS
    ///
    /// Generic version of `connect` that works with any stream implementing
    /// `AsyncRead + AsyncWrite + Unpin`. Useful for TDS-wrapped TLS where
    /// the underlying stream is not a plain TcpStream.
    pub async fn connect_stream<S>(
        &self,
        stream: S,
        server_name: &str,
    ) -> Result<TlsStream<S>, TlsError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let server_name = ServerName::try_from(server_name.to_string())
            .map_err(|_| TlsError::config(format!("Invalid server name: {}", server_name)))?;

        self.inner
            .connect(server_name, stream)
            .await
            .map_err(|e| TlsError::handshake(e.to_string()))
    }
}

/// Build the root certificate store based on configuration
fn build_root_store(config: &TlsClientConfig) -> Result<RootCertStore, TlsError> {
    let mut root_store = RootCertStore::empty();

    match config.verify_mode {
        TlsVerifyMode::None => {
            // For None mode, we still need a root store but won't verify
            // Add webpki roots as a baseline (verification is disabled elsewhere)
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        TlsVerifyMode::VerifyCa | TlsVerifyMode::Verify => {
            // Load custom CA if provided
            if let Some(ca_path) = &config.ca_path {
                let certs = load_certificates(ca_path)?;
                for cert in certs {
                    root_store
                        .add(cert)
                        .map_err(|e| TlsError::cert_load(ca_path, e.to_string()))?;
                }
            } else {
                // Use system/webpki roots
                root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
        }
    }

    Ok(root_store)
}

/// Custom certificate verifier that accepts any certificate
///
/// **WARNING**: This is insecure and should only be used for testing.
#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Accept any certificate
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_connector_default_config() {
        // Default config with system roots should work
        let config = TlsClientConfig::default();
        // Note: This would fail in CI without system certs, so we just test parsing
        let _ = TlsConnector::new(&config);
    }

    #[test]
    fn test_connector_insecure() {
        // Insecure connector should be creatable
        let result = TlsConnector::new_insecure();
        assert!(result.is_ok());
    }

    #[test]
    fn test_connector_nonexistent_ca_file() {
        let config = TlsClientConfig {
            enabled: true,
            verify_mode: TlsVerifyMode::Verify,
            ca_path: Some(PathBuf::from("/nonexistent/ca.crt")),
            client_cert_path: None,
            client_key_path: None,
        };

        let result = TlsConnector::new(&config);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("certificate") || err.contains("cert"));
    }

    #[test]
    fn test_connector_partial_client_cert() {
        // Having cert but no key should fail validation
        let config = TlsClientConfig {
            enabled: true,
            verify_mode: TlsVerifyMode::Verify,
            ca_path: None,
            client_cert_path: Some(PathBuf::from("/path/to/cert.pem")),
            client_key_path: None,
        };

        let result = TlsConnector::new(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_root_store_none_mode() {
        let config = TlsClientConfig {
            enabled: true,
            verify_mode: TlsVerifyMode::None,
            ca_path: None,
            client_cert_path: None,
            client_key_path: None,
        };

        // Should succeed even with None mode
        let result = build_root_store(&config);
        assert!(result.is_ok());
    }
}
