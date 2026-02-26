//! keeperdb-proxy - Database protocol proxy with credential injection
//!
//! This library provides the core functionality for a database proxy that:
//! - Detects database protocols (MySQL, PostgreSQL, SQL Server)
//! - Injects configured credentials during authentication
//! - Relays all traffic transparently after authentication
//! - Supports TLS/SSL encryption on both client and server sides
//! - Provides pluggable authentication via the [`auth`] module

#[macro_use]
mod logging;

pub mod auth;
pub mod config;
pub mod error;
pub mod handshake;
pub mod protocol;
pub mod query_logging;
pub mod server;
pub mod tls;

// PyO3 bindings for Gateway integration (optional)
#[cfg(feature = "python")]
pub mod pyo3;

pub use auth::{
    AuthCredentials, AuthMethod, AuthProvider, ConnectionContext, DatabaseType, SessionConfig,
    StaticAuthProvider, TokenValidation,
};
pub use config::Config;
pub use error::{ProxyError, Result};
pub use handshake::{CredentialStore, HandshakeAuthProvider};
pub use server::{Listener, MetricsSnapshot, NetworkStream, ProxyMetrics};
pub use tls::{
    TlsAcceptor, TlsClientConfig, TlsConnector, TlsError, TlsServerConfig, TlsVerifyMode,
};
