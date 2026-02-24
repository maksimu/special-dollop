//! Server module for keeperdb-proxy
//!
//! This module contains:
//! - TCP listener
//! - Connection handler
//! - Session relay
//! - Protocol handlers
//! - Network stream abstraction (TCP/TLS)
//! - Metrics collection

pub mod connection;
pub mod connection_tracker;
pub mod handlers;
pub mod listener;
pub mod metrics;
pub mod session;
pub mod stream;

pub use connection::Connection;
pub use connection_tracker::{ConnectionGuard, ConnectionInfo, ConnectionTracker, TunnelId};
pub use listener::{Listener, ListenerStats};
pub use metrics::{MetricsSnapshot, ProxyMetrics};
pub use session::{DisconnectReason, ManagedSession, Session, SessionManager};
pub use stream::NetworkStream;
