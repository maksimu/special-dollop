//! TLS/SSL support for keeperdb-proxy
//!
//! This module provides TLS functionality for:
//! - **Server-side TLS**: Accepting encrypted connections from database clients
//! - **Client-side TLS**: Connecting to database servers over encrypted connections
//!
//! # Architecture
//!
//! ```text
//! ┌──────────┐        TLS        ┌───────────┐        TLS        ┌──────────┐
//! │  Client  │ ───────────────── │   Proxy   │ ───────────────── │ Database │
//! │ (MySQL)  │   (server-side)   │           │   (client-side)   │  Server  │
//! └──────────┘                   └───────────┘                   └──────────┘
//! ```
//!
//! # Configuration
//!
//! TLS is configured in the YAML config file:
//!
//! ```yaml
//! server:
//!   listen_port: 3307
//!   tls:
//!     enabled: true
//!     cert_path: "/path/to/server.crt"
//!     key_path: "/path/to/server.key"
//!
//! target:
//!   host: "db.example.com"
//!   port: 3306
//!   tls:
//!     enabled: true
//!     verify_mode: "verify"
//!     ca_path: "/path/to/ca.crt"
//! ```
//!
//! # Usage
//!
//! TLS support integrates with the MySQL protocol handler automatically.
//! When TLS is enabled, the handshake flow is:
//!
//! 1. Client connects to proxy
//! 2. Proxy sends HandshakeV10 with SSL capability flag
//! 3. Client requests SSL upgrade
//! 4. TLS handshake occurs
//! 5. Authentication continues over TLS
//!
//! # Security
//!
//! - Uses rustls (pure Rust TLS implementation) for memory safety
//! - TLS 1.2 minimum, TLS 1.3 preferred
//! - Modern cipher suites only
//! - Certificate verification enabled by default

mod acceptor;
mod config;
mod connector;
mod error;

pub use acceptor::TlsAcceptor;
pub use config::{TlsClientConfig, TlsServerConfig, TlsVerifyMode};
pub use connector::TlsConnector;
pub use error::TlsError;

// Shared utilities for loading certificates and keys
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Load certificates from a PEM file
///
/// Reads all certificates from a PEM-encoded file and returns them as
/// a vector of `CertificateDer`. This supports certificate chains.
pub(crate) fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::cert_load(path, e.to_string()))?;

    let mut reader = BufReader::new(file);

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::cert_load(path, e.to_string()))?;

    Ok(certs)
}

/// Load a private key from a PEM file
///
/// Reads a private key from a PEM-encoded file. Supports RSA, PKCS8, and EC keys.
pub(crate) fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let file = File::open(path).map_err(|e| TlsError::key_load(path, e.to_string()))?;

    let mut reader = BufReader::new(file);

    // Try to read any type of private key (RSA, PKCS8, EC)
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| TlsError::key_load(path, e.to_string()))?
        .ok_or_else(|| TlsError::key_load(path, "no private key found in file"))
}
