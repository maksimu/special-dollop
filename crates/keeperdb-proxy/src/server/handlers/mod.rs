//! Database protocol handlers
//!
//! This module contains protocol-specific handlers for different database types.
//! Each handler implements the authentication flow with credential injection
//! and traffic relay for its respective database protocol.

pub mod mysql;
pub mod oracle;
pub mod postgres;
pub mod sqlserver;

pub use mysql::MySQLHandler;
pub use oracle::OracleHandler;
pub use postgres::PostgresHandler;
pub use sqlserver::SqlServerHandler;

use crate::auth::{AuthProvider, ConnectionContext};
use crate::query_logging::config::{ConnectionLoggingConfig, QueryLoggingConfig};

/// Resolve the logging config for a connection.
///
/// Tries the per-connection config from the auth provider first (e.g., from
/// Gateway handshake), then falls back to the global config. Returns `None`
/// if logging is disabled in both sources.
///
/// Must be called BEFORE `get_auth()` on providers that consume credentials
/// on first lookup (e.g., handshake provider).
pub async fn resolve_logging_config(
    auth_provider: &dyn AuthProvider,
    context: &ConnectionContext,
    global_config: &QueryLoggingConfig,
) -> Option<ConnectionLoggingConfig> {
    auth_provider
        .get_logging_config(context)
        .await
        .ok()
        .flatten()
        .or_else(|| ConnectionLoggingConfig::from_global(global_config).into_enabled())
}
