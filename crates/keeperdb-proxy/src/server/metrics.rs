//! Proxy metrics for observability.
//!
//! This module provides a centralized metrics collection system for monitoring
//! the proxy in production. Metrics are collected using atomic counters for
//! thread-safe, low-overhead tracking.
//!
//! # Usage
//!
//! ```ignore
//! use keeperdb_proxy::server::ProxyMetrics;
//!
//! let metrics = ProxyMetrics::new();
//!
//! // Record events
//! metrics.connection_accepted();
//! metrics.mysql_connection();
//! metrics.auth_success();
//!
//! // Get current values
//! let snapshot = metrics.snapshot();
//! println!("Active connections: {}", snapshot.connections_active);
//! ```
//!
//! # Metric Categories
//!
//! ## Connection Metrics
//! - `connections_accepted`: Total connections accepted
//! - `connections_active`: Currently active connections
//! - `connections_rejected_limit`: Rejected due to max connections limit
//!
//! ## Protocol Metrics
//! - `mysql_connections`: Total MySQL protocol connections
//! - `postgres_connections`: Total PostgreSQL protocol connections
//!
//! ## Authentication Metrics
//! - `auth_successes`: Successful authentications
//! - `auth_failures`: Failed authentications
//! - `token_validations`: Successful token validations
//! - `token_validation_failures`: Failed token validations
//!
//! ## Security Metrics
//! - `application_lock_rejections`: Connections rejected due to application fingerprint mismatch
//! - `session_lock_rejections`: Connections rejected due to session/target lock
//!
//! ## Session Metrics
//! - `sessions_ended_normal`: Sessions ended normally (client disconnect)
//! - `sessions_ended_max_duration`: Sessions terminated due to max duration
//! - `sessions_ended_max_queries`: Sessions terminated due to query limit
//! - `sessions_ended_idle_timeout`: Sessions terminated due to idle timeout
//!
//! ## Cleanup Metrics
//! - `stale_sessions_cleaned`: Stale session entries removed by cleanup task

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Atomic ordering used for metrics (Relaxed is sufficient for counters).
const METRIC_ORDERING: Ordering = Ordering::Relaxed;

/// Centralized metrics collection for the proxy.
///
/// All metrics use atomic counters for thread-safe access without locks.
/// The overhead is minimal, making it safe to record metrics on every operation.
#[derive(Debug, Default)]
pub struct ProxyMetrics {
    // ========================================================================
    // Connection Metrics
    // ========================================================================
    /// Total connections accepted since startup
    pub connections_accepted: AtomicU64,
    /// Currently active connections
    pub connections_active: AtomicU64,
    /// Connections rejected due to max connections limit
    pub connections_rejected_limit: AtomicU64,

    // ========================================================================
    // Protocol Metrics
    // ========================================================================
    /// Total MySQL protocol connections
    pub mysql_connections: AtomicU64,
    /// Total PostgreSQL protocol connections
    pub postgres_connections: AtomicU64,
    /// Connections with unknown/unsupported protocol
    pub unknown_protocol_connections: AtomicU64,

    // ========================================================================
    // Authentication Metrics
    // ========================================================================
    /// Successful authentications
    pub auth_successes: AtomicU64,
    /// Failed authentications
    pub auth_failures: AtomicU64,
    /// Successful token validations
    pub token_validations: AtomicU64,
    /// Failed token validations (invalid or expired)
    pub token_validation_failures: AtomicU64,

    // ========================================================================
    // Security Metrics (Application Lock)
    // ========================================================================
    /// Connections rejected due to application fingerprint mismatch
    pub application_lock_rejections: AtomicU64,
    /// Connections rejected due to session/target already in use
    pub session_lock_rejections: AtomicU64,
    /// Times the application lock expired and allowed a new tool
    pub application_lock_expirations: AtomicU64,

    // ========================================================================
    // Session Lifecycle Metrics
    // ========================================================================
    /// Sessions ended normally (client disconnect)
    pub sessions_ended_normal: AtomicU64,
    /// Sessions terminated due to max duration limit
    pub sessions_ended_max_duration: AtomicU64,
    /// Sessions terminated due to query limit
    pub sessions_ended_max_queries: AtomicU64,
    /// Sessions terminated due to idle timeout
    pub sessions_ended_idle_timeout: AtomicU64,

    // ========================================================================
    // Cleanup Metrics
    // ========================================================================
    /// Stale session entries removed by background cleanup
    pub stale_sessions_cleaned: AtomicU64,
}

impl ProxyMetrics {
    /// Create a new metrics instance with all counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a shared metrics instance.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    // ========================================================================
    // Connection Recording Methods
    // ========================================================================

    /// Record a new connection accepted.
    pub fn connection_accepted(&self) {
        self.connections_accepted.fetch_add(1, METRIC_ORDERING);
        self.connections_active.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a connection closed.
    pub fn connection_closed(&self) {
        self.connections_active.fetch_sub(1, METRIC_ORDERING);
    }

    /// Record a connection rejected due to limit.
    pub fn connection_rejected_limit(&self) {
        self.connections_rejected_limit
            .fetch_add(1, METRIC_ORDERING);
    }

    // ========================================================================
    // Protocol Recording Methods
    // ========================================================================

    /// Record a MySQL protocol connection.
    pub fn mysql_connection(&self) {
        self.mysql_connections.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a PostgreSQL protocol connection.
    pub fn postgres_connection(&self) {
        self.postgres_connections.fetch_add(1, METRIC_ORDERING);
    }

    /// Record an unknown protocol connection.
    pub fn unknown_protocol(&self) {
        self.unknown_protocol_connections
            .fetch_add(1, METRIC_ORDERING);
    }

    // ========================================================================
    // Authentication Recording Methods
    // ========================================================================

    /// Record a successful authentication.
    pub fn auth_success(&self) {
        self.auth_successes.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a failed authentication.
    pub fn auth_failure(&self) {
        self.auth_failures.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a successful token validation.
    pub fn token_validated(&self) {
        self.token_validations.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a failed token validation.
    pub fn token_validation_failed(&self) {
        self.token_validation_failures.fetch_add(1, METRIC_ORDERING);
    }

    // ========================================================================
    // Security Recording Methods
    // ========================================================================

    /// Record a connection rejected due to application fingerprint mismatch.
    pub fn application_lock_rejected(&self) {
        self.application_lock_rejections
            .fetch_add(1, METRIC_ORDERING);
    }

    /// Record a connection rejected due to session/target lock.
    pub fn session_lock_rejected(&self) {
        self.session_lock_rejections.fetch_add(1, METRIC_ORDERING);
    }

    /// Record an application lock expiration (new tool allowed).
    pub fn application_lock_expired(&self) {
        self.application_lock_expirations
            .fetch_add(1, METRIC_ORDERING);
    }

    // ========================================================================
    // Session Lifecycle Recording Methods
    // ========================================================================

    /// Record a session ended normally.
    pub fn session_ended_normal(&self) {
        self.sessions_ended_normal.fetch_add(1, METRIC_ORDERING);
    }

    /// Record a session terminated due to max duration.
    pub fn session_ended_max_duration(&self) {
        self.sessions_ended_max_duration
            .fetch_add(1, METRIC_ORDERING);
    }

    /// Record a session terminated due to query limit.
    pub fn session_ended_max_queries(&self) {
        self.sessions_ended_max_queries
            .fetch_add(1, METRIC_ORDERING);
    }

    /// Record a session terminated due to idle timeout.
    pub fn session_ended_idle_timeout(&self) {
        self.sessions_ended_idle_timeout
            .fetch_add(1, METRIC_ORDERING);
    }

    // ========================================================================
    // Cleanup Recording Methods
    // ========================================================================

    /// Record stale sessions cleaned up.
    pub fn stale_sessions_cleaned(&self, count: u64) {
        self.stale_sessions_cleaned
            .fetch_add(count, METRIC_ORDERING);
    }

    // ========================================================================
    // Snapshot Methods
    // ========================================================================

    /// Get a snapshot of all current metric values.
    ///
    /// This creates a point-in-time copy of all metrics that can be
    /// serialized or displayed without holding locks.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            // Connection metrics
            connections_accepted: self.connections_accepted.load(METRIC_ORDERING),
            connections_active: self.connections_active.load(METRIC_ORDERING),
            connections_rejected_limit: self.connections_rejected_limit.load(METRIC_ORDERING),

            // Protocol metrics
            mysql_connections: self.mysql_connections.load(METRIC_ORDERING),
            postgres_connections: self.postgres_connections.load(METRIC_ORDERING),
            unknown_protocol_connections: self.unknown_protocol_connections.load(METRIC_ORDERING),

            // Authentication metrics
            auth_successes: self.auth_successes.load(METRIC_ORDERING),
            auth_failures: self.auth_failures.load(METRIC_ORDERING),
            token_validations: self.token_validations.load(METRIC_ORDERING),
            token_validation_failures: self.token_validation_failures.load(METRIC_ORDERING),

            // Security metrics
            application_lock_rejections: self.application_lock_rejections.load(METRIC_ORDERING),
            session_lock_rejections: self.session_lock_rejections.load(METRIC_ORDERING),
            application_lock_expirations: self.application_lock_expirations.load(METRIC_ORDERING),

            // Session lifecycle metrics
            sessions_ended_normal: self.sessions_ended_normal.load(METRIC_ORDERING),
            sessions_ended_max_duration: self.sessions_ended_max_duration.load(METRIC_ORDERING),
            sessions_ended_max_queries: self.sessions_ended_max_queries.load(METRIC_ORDERING),
            sessions_ended_idle_timeout: self.sessions_ended_idle_timeout.load(METRIC_ORDERING),

            // Cleanup metrics
            stale_sessions_cleaned: self.stale_sessions_cleaned.load(METRIC_ORDERING),
        }
    }

    /// Format metrics in Prometheus exposition format.
    ///
    /// This can be served via an HTTP endpoint for Prometheus scraping.
    ///
    /// # Example Output
    ///
    /// ```text
    /// # HELP proxy_connections_accepted_total Total connections accepted
    /// # TYPE proxy_connections_accepted_total counter
    /// proxy_connections_accepted_total 1234
    /// ```
    pub fn to_prometheus(&self) -> String {
        let s = self.snapshot();
        let mut output = String::with_capacity(2048);

        // Connection metrics
        output.push_str("# HELP proxy_connections_accepted_total Total connections accepted\n");
        output.push_str("# TYPE proxy_connections_accepted_total counter\n");
        output.push_str(&format!(
            "proxy_connections_accepted_total {}\n",
            s.connections_accepted
        ));

        output.push_str("# HELP proxy_connections_active Current active connections\n");
        output.push_str("# TYPE proxy_connections_active gauge\n");
        output.push_str(&format!(
            "proxy_connections_active {}\n",
            s.connections_active
        ));

        output.push_str(
            "# HELP proxy_connections_rejected_total Connections rejected due to limit\n",
        );
        output.push_str("# TYPE proxy_connections_rejected_total counter\n");
        output.push_str(&format!(
            "proxy_connections_rejected_total {}\n",
            s.connections_rejected_limit
        ));

        // Protocol metrics
        output.push_str("# HELP proxy_protocol_connections_total Connections by protocol\n");
        output.push_str("# TYPE proxy_protocol_connections_total counter\n");
        output.push_str(&format!(
            "proxy_protocol_connections_total{{protocol=\"mysql\"}} {}\n",
            s.mysql_connections
        ));
        output.push_str(&format!(
            "proxy_protocol_connections_total{{protocol=\"postgresql\"}} {}\n",
            s.postgres_connections
        ));
        output.push_str(&format!(
            "proxy_protocol_connections_total{{protocol=\"unknown\"}} {}\n",
            s.unknown_protocol_connections
        ));

        // Authentication metrics
        output.push_str("# HELP proxy_auth_total Authentication attempts\n");
        output.push_str("# TYPE proxy_auth_total counter\n");
        output.push_str(&format!(
            "proxy_auth_total{{result=\"success\"}} {}\n",
            s.auth_successes
        ));
        output.push_str(&format!(
            "proxy_auth_total{{result=\"failure\"}} {}\n",
            s.auth_failures
        ));

        output.push_str("# HELP proxy_token_validations_total Token validation attempts\n");
        output.push_str("# TYPE proxy_token_validations_total counter\n");
        output.push_str(&format!(
            "proxy_token_validations_total{{result=\"success\"}} {}\n",
            s.token_validations
        ));
        output.push_str(&format!(
            "proxy_token_validations_total{{result=\"failure\"}} {}\n",
            s.token_validation_failures
        ));

        // Security metrics
        output.push_str("# HELP proxy_lock_rejections_total Connections rejected due to locks\n");
        output.push_str("# TYPE proxy_lock_rejections_total counter\n");
        output.push_str(&format!(
            "proxy_lock_rejections_total{{type=\"application\"}} {}\n",
            s.application_lock_rejections
        ));
        output.push_str(&format!(
            "proxy_lock_rejections_total{{type=\"session\"}} {}\n",
            s.session_lock_rejections
        ));

        output.push_str(
            "# HELP proxy_application_lock_expirations_total Application locks that expired\n",
        );
        output.push_str("# TYPE proxy_application_lock_expirations_total counter\n");
        output.push_str(&format!(
            "proxy_application_lock_expirations_total {}\n",
            s.application_lock_expirations
        ));

        // Session lifecycle metrics
        output.push_str("# HELP proxy_sessions_ended_total Sessions ended by reason\n");
        output.push_str("# TYPE proxy_sessions_ended_total counter\n");
        output.push_str(&format!(
            "proxy_sessions_ended_total{{reason=\"normal\"}} {}\n",
            s.sessions_ended_normal
        ));
        output.push_str(&format!(
            "proxy_sessions_ended_total{{reason=\"max_duration\"}} {}\n",
            s.sessions_ended_max_duration
        ));
        output.push_str(&format!(
            "proxy_sessions_ended_total{{reason=\"max_queries\"}} {}\n",
            s.sessions_ended_max_queries
        ));
        output.push_str(&format!(
            "proxy_sessions_ended_total{{reason=\"idle_timeout\"}} {}\n",
            s.sessions_ended_idle_timeout
        ));

        // Cleanup metrics
        output.push_str("# HELP proxy_stale_sessions_cleaned_total Stale sessions cleaned up\n");
        output.push_str("# TYPE proxy_stale_sessions_cleaned_total counter\n");
        output.push_str(&format!(
            "proxy_stale_sessions_cleaned_total {}\n",
            s.stale_sessions_cleaned
        ));

        output
    }
}

/// Point-in-time snapshot of all metrics.
///
/// This struct contains plain values (not atomics) for easy serialization
/// and display. Created by [`ProxyMetrics::snapshot()`].
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    // Connection metrics
    pub connections_accepted: u64,
    pub connections_active: u64,
    pub connections_rejected_limit: u64,

    // Protocol metrics
    pub mysql_connections: u64,
    pub postgres_connections: u64,
    pub unknown_protocol_connections: u64,

    // Authentication metrics
    pub auth_successes: u64,
    pub auth_failures: u64,
    pub token_validations: u64,
    pub token_validation_failures: u64,

    // Security metrics
    pub application_lock_rejections: u64,
    pub session_lock_rejections: u64,
    pub application_lock_expirations: u64,

    // Session lifecycle metrics
    pub sessions_ended_normal: u64,
    pub sessions_ended_max_duration: u64,
    pub sessions_ended_max_queries: u64,
    pub sessions_ended_idle_timeout: u64,

    // Cleanup metrics
    pub stale_sessions_cleaned: u64,
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Proxy Metrics ===")?;
        writeln!(f)?;
        writeln!(f, "Connections:")?;
        writeln!(f, "  Accepted:  {}", self.connections_accepted)?;
        writeln!(f, "  Active:    {}", self.connections_active)?;
        writeln!(f, "  Rejected:  {}", self.connections_rejected_limit)?;
        writeln!(f)?;
        writeln!(f, "Protocols:")?;
        writeln!(f, "  MySQL:      {}", self.mysql_connections)?;
        writeln!(f, "  PostgreSQL: {}", self.postgres_connections)?;
        writeln!(f, "  Unknown:    {}", self.unknown_protocol_connections)?;
        writeln!(f)?;
        writeln!(f, "Authentication:")?;
        writeln!(f, "  Successes: {}", self.auth_successes)?;
        writeln!(f, "  Failures:  {}", self.auth_failures)?;
        writeln!(f, "  Tokens OK: {}", self.token_validations)?;
        writeln!(f, "  Tokens Bad:{}", self.token_validation_failures)?;
        writeln!(f)?;
        writeln!(f, "Security:")?;
        writeln!(
            f,
            "  App Lock Rejections:     {}",
            self.application_lock_rejections
        )?;
        writeln!(
            f,
            "  Session Lock Rejections: {}",
            self.session_lock_rejections
        )?;
        writeln!(
            f,
            "  App Lock Expirations:    {}",
            self.application_lock_expirations
        )?;
        writeln!(f)?;
        writeln!(f, "Sessions Ended:")?;
        writeln!(f, "  Normal:       {}", self.sessions_ended_normal)?;
        writeln!(f, "  Max Duration: {}", self.sessions_ended_max_duration)?;
        writeln!(f, "  Max Queries:  {}", self.sessions_ended_max_queries)?;
        writeln!(f, "  Idle Timeout: {}", self.sessions_ended_idle_timeout)?;
        writeln!(f)?;
        writeln!(f, "Cleanup:")?;
        writeln!(
            f,
            "  Stale Sessions Cleaned: {}",
            self.stale_sessions_cleaned
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_default() {
        let metrics = ProxyMetrics::new();
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.connections_accepted, 0);
        assert_eq!(snapshot.connections_active, 0);
    }

    #[test]
    fn test_connection_lifecycle() {
        let metrics = ProxyMetrics::new();

        metrics.connection_accepted();
        assert_eq!(metrics.connections_accepted.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.connections_active.load(Ordering::Relaxed), 1);

        metrics.connection_accepted();
        assert_eq!(metrics.connections_accepted.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.connections_active.load(Ordering::Relaxed), 2);

        metrics.connection_closed();
        assert_eq!(metrics.connections_accepted.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.connections_active.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_protocol_metrics() {
        let metrics = ProxyMetrics::new();

        metrics.mysql_connection();
        metrics.mysql_connection();
        metrics.postgres_connection();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.mysql_connections, 2);
        assert_eq!(snapshot.postgres_connections, 1);
    }

    #[test]
    fn test_auth_metrics() {
        let metrics = ProxyMetrics::new();

        metrics.auth_success();
        metrics.auth_success();
        metrics.auth_failure();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.auth_successes, 2);
        assert_eq!(snapshot.auth_failures, 1);
    }

    #[test]
    fn test_prometheus_format() {
        let metrics = ProxyMetrics::new();
        metrics.connection_accepted();
        metrics.mysql_connection();
        metrics.auth_success();

        let prom = metrics.to_prometheus();
        assert!(prom.contains("proxy_connections_accepted_total 1"));
        assert!(prom.contains("proxy_protocol_connections_total{protocol=\"mysql\"} 1"));
        assert!(prom.contains("proxy_auth_total{result=\"success\"} 1"));
    }

    #[test]
    fn test_snapshot_display() {
        let metrics = ProxyMetrics::new();
        metrics.connection_accepted();

        let snapshot = metrics.snapshot();
        let display = format!("{}", snapshot);
        assert!(display.contains("Accepted:  1"));
    }

    #[test]
    fn test_shared_metrics() {
        let metrics = ProxyMetrics::shared();
        metrics.connection_accepted();

        let metrics2 = Arc::clone(&metrics);
        metrics2.connection_accepted();

        assert_eq!(metrics.connections_accepted.load(Ordering::Relaxed), 2);
    }
}
