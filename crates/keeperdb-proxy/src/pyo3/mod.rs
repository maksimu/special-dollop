//! PyO3 bindings for Keeper Gateway integration.
//!
//! This module provides Python bindings for the database proxy, allowing
//! the Keeper Gateway to start and manage proxy instances programmatically.
//!
//! Logging is handled by the parent `keeper_pam_connections` module via
//! `initialize_logger()`, which bridges Rust `log` records to Python's
//! logging system. The `tracing` crate's built-in `log` feature automatically
//! emits tracing events as `log` records when no tracing subscriber is set,
//! so all keeperdb-proxy logs flow through to Python without any extra setup.
//!
//! # Modes
//!
//! The proxy supports two modes:
//!
//! ## Settings Mode (credentials provided)
//!
//! Credentials are provided at startup - the proxy replaces client credentials
//! with configured credentials when connecting to the target database.
//!
//! ```python
//! import keeper_pam_connections
//!
//! # Start a proxy with database parameters
//! proxy = keeper_pam_connections.keeperdb_proxy({
//!     "database_type": "mysql",
//!     "target_host": "db.example.com",
//!     "target_port": 3306,
//!     "username": "db_user",
//!     "password": "secret",
//! })
//!
//! print(f"Proxy listening on {proxy.host}:{proxy.port}")
//! proxy.stop()
//! ```
//!
//! ## Gateway Mode (credentials via handshake)
//!
//! No credentials provided - the proxy expects a Gateway handshake
//! to deliver credentials when connections are established.
//!
//! ```python
//! import keeper_pam_connections
//!
//! # Start a long-running proxy in gateway mode
//! proxy = keeper_pam_connections.keeperdb_proxy({
//!     "listen_host": "127.0.0.1",
//!     "listen_port": 13306,
//!     # No username/password = gateway mode
//! })
//!
//! print(f"Gateway proxy listening on {proxy.host}:{proxy.port}")
//! print(f"Gateway mode: {proxy.gateway_mode}")  # True
//!
//! # Gateway then connects with handshake to deliver credentials per-tunnel
//! proxy.stop()
//! ```

use pyo3::prelude::*;

mod config;
mod errors;
mod gateway_provider;
mod proxy_handle;

use proxy_handle::ProxyHandle;

/// Start a database proxy with the given configuration.
///
/// Args:
///     db_params: Dictionary with database connection parameters including:
///         - database_type: "mysql" or "postgresql"
///         - target_host: Target database hostname
///         - target_port: Target database port
///         - username: Database username
///         - password: Database password
///         - database: Optional database name
///         - auth_method: Optional auth method (e.g., "native_password", "caching_sha2")
///         - session_config: Optional dict with session limits
///         - tls_config: Optional dict with TLS settings
///         - listen_host: Optional proxy listen host (default: "127.0.0.1")
///         - listen_port: Optional proxy listen port (default: 0 for auto-assign)
///
/// Returns:
///     ProxyHandle: Handle to control the running proxy
///
/// Raises:
///     ValueError: If configuration is invalid
///     RuntimeError: If proxy fails to start
#[pyfunction]
fn keeperdb_proxy(_py: Python<'_>, db_params: &Bound<'_, PyAny>) -> PyResult<ProxyHandle> {
    ProxyHandle::start(db_params)
}

/// Register keeperdb-proxy Python bindings into a parent module.
///
/// This follows the workspace registration pattern used by other crates.
/// Called from the unified python-bindings crate to expose proxy functionality
/// through the `keeper_pam_connections` module.
///
/// Logging is not registered here - keeperdb-proxy's `tracing` events
/// automatically fall through to the `log` crate (via tracing's built-in
/// log feature), which is captured by the WebRTC logger bridge set up
/// via `initialize_logger()`.
///
/// # Example
///
/// ```ignore
/// // In python-bindings/src/lib.rs:
/// keeperdb_proxy::pyo3::register_keeperdb_proxy_module(py, m)?;
/// ```
pub fn register_keeperdb_proxy_module(
    _py: Python<'_>,
    parent: &Bound<'_, PyModule>,
) -> PyResult<()> {
    parent.add_function(wrap_pyfunction!(keeperdb_proxy, parent)?)?;
    parent.add_class::<ProxyHandle>()?;

    Ok(())
}
