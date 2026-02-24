//! Session configuration and limits.
//!
//! This module provides the [`SessionConfig`] struct which contains
//! session limits and configuration returned by [`AuthProvider`].

use std::time::Duration;

/// Session configuration and limits.
///
/// Defines optional limits and settings for a proxy session.
/// All fields are optional, with `None` meaning no limit or use defaults.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use keeperdb_proxy::auth::SessionConfig;
///
/// // Create unlimited session
/// let config = SessionConfig::unlimited();
///
/// // Create session with limits
/// let limited = SessionConfig::default()
///     .with_max_duration(Duration::from_secs(3600))
///     .with_idle_timeout(Duration::from_secs(300))
///     .with_max_connections_per_session(1);
/// ```
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    /// Maximum session duration.
    ///
    /// If set, the session will be terminated after this duration
    /// regardless of activity. `None` means no time limit.
    pub max_duration: Option<Duration>,

    /// Idle timeout override.
    ///
    /// If set, overrides the server's default idle timeout.
    /// `None` means use the server's configured timeout.
    pub idle_timeout: Option<Duration>,

    /// Maximum number of queries allowed.
    ///
    /// If set, the session will be terminated after this many queries.
    /// `None` means unlimited queries.
    pub max_queries: Option<u64>,

    /// Maximum simultaneous connections per session.
    ///
    /// If set, limits the number of concurrent connections allowed
    /// for a single session/tunnel. `None` means unlimited.
    pub max_connections_per_session: Option<u32>,

    /// Grace period for establishing connections.
    ///
    /// During this window after the first connection, additional connections
    /// are allowed even if limits would otherwise block them. After the grace
    /// period expires, connection limits are strictly enforced.
    /// `None` means use the default (1 second).
    pub connection_grace_period: Option<Duration>,

    /// Require connection token validation.
    ///
    /// When `true`, the proxy requires a valid connection token from
    /// the Gateway before allowing the connection. The token is extracted
    /// from the password field and validated via `AuthProvider::validate_token()`.
    pub require_token: bool,

    /// Application lock timeout.
    ///
    /// After all connections from a session disconnect, this is how long
    /// the application fingerprint lock is retained before allowing a
    /// different tool to connect. `None` means use default (5 seconds).
    pub application_lock_timeout: Option<Duration>,
}

impl SessionConfig {
    /// Create a session configuration with no limits.
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::unlimited();
    /// assert!(config.max_duration.is_none());
    /// assert!(config.max_queries.is_none());
    /// assert!(config.max_connections_per_session.is_none());
    /// ```
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Set the maximum session duration (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `duration` - Maximum duration for the session
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_max_duration(Duration::from_secs(3600));
    ///
    /// assert_eq!(config.max_duration, Some(Duration::from_secs(3600)));
    /// ```
    pub fn with_max_duration(mut self, duration: Duration) -> Self {
        self.max_duration = Some(duration);
        self
    }

    /// Set the idle timeout (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `duration` - Idle timeout duration
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_idle_timeout(Duration::from_secs(300));
    ///
    /// assert_eq!(config.idle_timeout, Some(Duration::from_secs(300)));
    /// ```
    pub fn with_idle_timeout(mut self, duration: Duration) -> Self {
        self.idle_timeout = Some(duration);
        self
    }

    /// Set the maximum query count (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `count` - Maximum number of queries allowed
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_max_queries(1000);
    ///
    /// assert_eq!(config.max_queries, Some(1000));
    /// ```
    pub fn with_max_queries(mut self, count: u64) -> Self {
        self.max_queries = Some(count);
        self
    }

    /// Set whether token validation is required (builder pattern).
    ///
    /// When enabled, the proxy will extract a token from the client's
    /// password field and validate it via the AuthProvider before
    /// allowing the connection.
    ///
    /// # Arguments
    ///
    /// * `require` - Whether to require token validation
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_require_token(true);
    ///
    /// assert!(config.require_token);
    /// ```
    pub fn with_require_token(mut self, require: bool) -> Self {
        self.require_token = require;
        self
    }

    /// Set maximum connections per session (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `max` - Maximum number of simultaneous connections allowed
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_max_connections_per_session(2);
    ///
    /// assert_eq!(config.max_connections_per_session, Some(2));
    /// ```
    pub fn with_max_connections_per_session(mut self, max: u32) -> Self {
        self.max_connections_per_session = Some(max);
        self
    }

    /// Set connection grace period (builder pattern).
    ///
    /// During this window after the first connection, additional connections
    /// are allowed even if limits would otherwise block them.
    ///
    /// # Arguments
    ///
    /// * `duration` - Grace period duration
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_connection_grace_period(Duration::from_millis(500));
    ///
    /// assert_eq!(config.connection_grace_period, Some(Duration::from_millis(500)));
    /// ```
    pub fn with_connection_grace_period(mut self, duration: Duration) -> Self {
        self.connection_grace_period = Some(duration);
        self
    }

    /// Set application lock timeout (builder pattern).
    ///
    /// After all connections disconnect, this is how long the application
    /// fingerprint lock is retained before allowing a different tool to connect.
    ///
    /// # Arguments
    ///
    /// * `duration` - Lock timeout duration
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use keeperdb_proxy::auth::SessionConfig;
    ///
    /// let config = SessionConfig::default()
    ///     .with_application_lock_timeout(Duration::from_secs(10));
    ///
    /// assert_eq!(config.application_lock_timeout, Some(Duration::from_secs(10)));
    /// ```
    pub fn with_application_lock_timeout(mut self, duration: Duration) -> Self {
        self.application_lock_timeout = Some(duration);
        self
    }

    /// Get the effective maximum connections per session.
    ///
    /// Returns the configured max connections, or `None` if unlimited.
    pub fn effective_max_connections(&self) -> Option<u32> {
        self.max_connections_per_session.filter(|&max| max > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_unlimited() {
        let config = SessionConfig::default();
        assert!(config.max_duration.is_none());
        assert!(config.idle_timeout.is_none());
        assert!(config.max_queries.is_none());
        assert!(!config.require_token);
        assert!(config.max_connections_per_session.is_none());
        assert!(config.connection_grace_period.is_none());
        assert!(config.application_lock_timeout.is_none());
    }

    #[test]
    fn test_unlimited() {
        let config = SessionConfig::unlimited();
        assert!(config.max_duration.is_none());
        assert!(config.max_queries.is_none());
        assert!(config.max_connections_per_session.is_none());
    }

    #[test]
    fn test_with_max_duration() {
        let config = SessionConfig::default().with_max_duration(Duration::from_secs(3600));
        assert_eq!(config.max_duration, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_with_idle_timeout() {
        let config = SessionConfig::default().with_idle_timeout(Duration::from_secs(300));
        assert_eq!(config.idle_timeout, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_with_max_queries() {
        let config = SessionConfig::default().with_max_queries(1000);
        assert_eq!(config.max_queries, Some(1000));
    }

    #[test]
    fn test_builder_chain() {
        let config = SessionConfig::default()
            .with_max_duration(Duration::from_secs(3600))
            .with_idle_timeout(Duration::from_secs(300))
            .with_max_queries(500);

        assert_eq!(config.max_duration, Some(Duration::from_secs(3600)));
        assert_eq!(config.idle_timeout, Some(Duration::from_secs(300)));
        assert_eq!(config.max_queries, Some(500));
    }

    #[test]
    fn test_clone() {
        let config1 = SessionConfig::default()
            .with_max_duration(Duration::from_secs(100))
            .with_max_queries(50);
        let config2 = config1.clone();

        assert_eq!(config1.max_duration, config2.max_duration);
        assert_eq!(config1.max_queries, config2.max_queries);
    }

    #[test]
    fn test_require_token_default_false() {
        let config = SessionConfig::default();
        assert!(!config.require_token);
    }

    #[test]
    fn test_with_require_token() {
        let config = SessionConfig::default().with_require_token(true);
        assert!(config.require_token);
    }

    #[test]
    fn test_with_require_token_false() {
        let config = SessionConfig::default()
            .with_require_token(true)
            .with_require_token(false);
        assert!(!config.require_token);
    }
}
