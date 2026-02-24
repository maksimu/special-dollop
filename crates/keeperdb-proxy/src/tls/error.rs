//! TLS-specific error types
//!
//! This module defines errors that can occur during TLS operations,
//! including certificate loading, TLS handshake, and verification failures.

use std::path::PathBuf;
use thiserror::Error;

/// TLS-specific errors
///
/// These errors capture specific failure modes for TLS operations,
/// with detailed context for debugging.
#[derive(Error, Debug)]
pub enum TlsError {
    /// Failed to load certificate from file
    #[error("Failed to load certificate from {path}: {reason}")]
    CertificateLoad {
        /// Path to the certificate file
        path: PathBuf,
        /// Reason for the failure
        reason: String,
    },

    /// Failed to load private key from file
    #[error("Failed to load private key from {path}: {reason}")]
    PrivateKeyLoad {
        /// Path to the key file
        path: PathBuf,
        /// Reason for the failure
        reason: String,
    },

    /// TLS handshake failed
    #[error("TLS handshake failed: {0}")]
    Handshake(String),

    /// Certificate verification failed
    #[error("Certificate verification failed: {0}")]
    Verification(String),

    /// TLS configuration error
    #[error("TLS configuration error: {0}")]
    Config(String),

    /// I/O error during TLS operation
    #[error("TLS I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl TlsError {
    /// Create a certificate load error
    pub fn cert_load(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        TlsError::CertificateLoad {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Create a private key load error
    pub fn key_load(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        TlsError::PrivateKeyLoad {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Create a handshake error
    pub fn handshake(reason: impl Into<String>) -> Self {
        TlsError::Handshake(reason.into())
    }

    /// Create a verification error
    pub fn verification(reason: impl Into<String>) -> Self {
        TlsError::Verification(reason.into())
    }

    /// Create a configuration error
    pub fn config(reason: impl Into<String>) -> Self {
        TlsError::Config(reason.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_certificate_load_error_display() {
        let err = TlsError::cert_load("/path/to/cert.pem", "file not found");
        let msg = err.to_string();
        assert!(msg.contains("/path/to/cert.pem"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn test_private_key_load_error_display() {
        let err = TlsError::key_load("/path/to/key.pem", "invalid format");
        let msg = err.to_string();
        assert!(msg.contains("/path/to/key.pem"));
        assert!(msg.contains("invalid format"));
    }

    #[test]
    fn test_handshake_error_display() {
        let err = TlsError::handshake("client disconnected");
        assert_eq!(err.to_string(), "TLS handshake failed: client disconnected");
    }

    #[test]
    fn test_verification_error_display() {
        let err = TlsError::verification("hostname mismatch");
        assert_eq!(
            err.to_string(),
            "Certificate verification failed: hostname mismatch"
        );
    }

    #[test]
    fn test_config_error_display() {
        let err = TlsError::config("missing cert_path");
        assert_eq!(
            err.to_string(),
            "TLS configuration error: missing cert_path"
        );
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let tls_err: TlsError = io_err.into();
        assert!(tls_err.to_string().contains("file not found"));
    }
}
