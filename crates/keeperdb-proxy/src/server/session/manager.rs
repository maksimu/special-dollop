//! Session manager for lifecycle and limit enforcement.
//!
//! This module provides the [`SessionManager`] struct which manages
//! session state including query counting, timeouts, and disconnect signals.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::auth::SessionConfig;

/// Grace period before disconnect for warnings (10 seconds).
const GRACE_PERIOD: Duration = Duration::from_secs(10);

/// Warning threshold for query count (90%).
const QUERY_WARNING_THRESHOLD: f64 = 0.9;

/// Reason for session disconnect.
#[derive(Debug, Clone)]
pub enum DisconnectReason {
    /// Session max_duration reached.
    MaxDuration {
        /// The configured max duration.
        duration: Duration,
    },

    /// Session idle_timeout reached.
    IdleTimeout {
        /// The configured idle timeout.
        duration: Duration,
    },

    /// Session max_queries reached.
    MaxQueries {
        /// The number of queries executed.
        count: u64,
    },

    /// Client closed connection normally.
    ClientDisconnect,

    /// Server closed connection.
    ServerDisconnect,

    /// I/O error during relay.
    IoError(String),

    /// Connection rejected (connection limit exceeded).
    ConnectionLimitExceeded,

    /// Token validation failed.
    TokenInvalid(String),
}

impl DisconnectReason {
    /// Get the MySQL error code for this disconnect reason.
    ///
    /// Returns `None` for normal disconnects that don't need an error packet.
    pub fn mysql_error_code(&self) -> Option<u16> {
        match self {
            Self::MaxDuration { .. } => Some(1317), // ER_QUERY_INTERRUPTED
            Self::IdleTimeout { .. } => Some(1317),
            Self::MaxQueries { .. } => Some(1317),
            Self::ConnectionLimitExceeded => Some(1040), // ER_CON_COUNT_ERROR
            Self::TokenInvalid(_) => Some(1045),         // ER_ACCESS_DENIED_ERROR
            Self::ClientDisconnect | Self::ServerDisconnect | Self::IoError(_) => None,
        }
    }

    /// Get a human-readable message for this disconnect reason.
    pub fn message(&self) -> String {
        match self {
            Self::MaxDuration { duration } => {
                format!("Session max duration ({:?}) reached", duration)
            }
            Self::IdleTimeout { duration } => {
                format!("Session idle timeout ({:?}) reached", duration)
            }
            Self::MaxQueries { count } => {
                format!("Session query limit ({}) reached", count)
            }
            Self::ClientDisconnect => "Client disconnected".to_string(),
            Self::ServerDisconnect => "Database server disconnected".to_string(),
            Self::IoError(e) => format!("I/O error: {}", e),
            Self::ConnectionLimitExceeded => {
                "Connection limit exceeded for this session".to_string()
            }
            Self::TokenInvalid(e) => format!("Invalid connection token: {}", e),
        }
    }
}

/// Manages session lifecycle, timeouts, and query limits.
///
/// The `SessionManager` tracks session state and enforces limits configured
/// via [`SessionConfig`]. It uses atomic operations for query counting and
/// channels for disconnect signaling.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use keeperdb_proxy::auth::SessionConfig;
/// use keeperdb_proxy::server::SessionManager;
///
/// let config = SessionConfig::default()
///     .with_max_queries(100)
///     .with_max_duration(Duration::from_secs(3600));
///
/// let mut mgr = SessionManager::new(config);
///
/// // Count queries (timers started separately in async context)
/// for _ in 0..50 {
///     mgr.increment_query_count().unwrap();
/// }
///
/// assert_eq!(mgr.query_count(), 50);
/// ```
pub struct SessionManager {
    /// Session configuration.
    config: SessionConfig,

    /// Unique session identifier.
    session_id: Uuid,

    /// When the session manager was created.
    created_at: Instant,

    /// Query counter (atomic for thread-safety).
    query_count: Arc<AtomicU64>,

    /// Channel to send disconnect signal.
    disconnect_tx: Option<oneshot::Sender<DisconnectReason>>,

    /// Channel to receive disconnect signal.
    disconnect_rx: Option<oneshot::Receiver<DisconnectReason>>,

    /// Handle to max_duration background task.
    max_duration_task: Option<JoinHandle<()>>,

    /// Whether warning for max_queries has been logged.
    query_limit_warned: bool,
}

impl SessionManager {
    /// Create a new session manager with the given configuration.
    ///
    /// The session starts immediately but timers are not started until
    /// [`start_timers()`](Self::start_timers) is called.
    ///
    /// # Arguments
    ///
    /// * `config` - Session configuration from the auth provider
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::SessionConfig;
    /// use keeperdb_proxy::server::SessionManager;
    ///
    /// let config = SessionConfig::default();
    /// let mgr = SessionManager::new(config);
    ///
    /// assert_eq!(mgr.query_count(), 0);
    /// ```
    pub fn new(config: SessionConfig) -> Self {
        let (tx, rx) = oneshot::channel();

        Self {
            config,
            session_id: Uuid::new_v4(),
            created_at: Instant::now(),
            query_count: Arc::new(AtomicU64::new(0)),
            disconnect_tx: Some(tx),
            disconnect_rx: Some(rx),
            max_duration_task: None,
            query_limit_warned: false,
        }
    }

    /// Create a new session manager with a pre-generated session ID.
    ///
    /// This is useful when the session ID needs to be determined before
    /// the session manager is created (e.g., for connection tracking).
    ///
    /// # Arguments
    ///
    /// * `config` - Session configuration from the auth provider
    /// * `session_id` - Pre-generated session ID string
    pub fn with_session_id(config: SessionConfig, session_id: String) -> Self {
        let (tx, rx) = oneshot::channel();
        let uuid = Uuid::parse_str(&session_id).unwrap_or_else(|e| {
            warn!(
                session_id = %session_id,
                error = %e,
                "Invalid session_id format, generating new UUID"
            );
            Uuid::new_v4()
        });

        Self {
            config,
            session_id: uuid,
            created_at: Instant::now(),
            query_count: Arc::new(AtomicU64::new(0)),
            disconnect_tx: Some(tx),
            disconnect_rx: Some(rx),
            max_duration_task: None,
            query_limit_warned: false,
        }
    }

    /// Get the session ID.
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Get the session configuration.
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Start background timers for session limits.
    ///
    /// This starts the max_duration timer if configured. The timer will
    /// send a disconnect signal when the duration expires.
    ///
    /// # Note
    ///
    /// This should be called after authentication completes. The max_duration
    /// timer begins counting from when this method is called, not from when
    /// the SessionManager was created.
    pub fn start_timers(&mut self) {
        if let Some(max_duration) = self.config.max_duration {
            self.start_max_duration_timer(max_duration);
        }

        debug!(
            session_id = %self.session_id,
            max_duration = ?self.config.max_duration,
            idle_timeout = ?self.config.idle_timeout,
            max_queries = ?self.config.max_queries,
            max_connections = ?self.config.max_connections_per_session,
            require_token = self.config.require_token,
            "Session started with config"
        );
    }

    /// Start the max_duration background timer.
    fn start_max_duration_timer(&mut self, duration: Duration) {
        let session_id = self.session_id;
        let disconnect_tx = self.disconnect_tx.take();

        let task = tokio::spawn(async move {
            // Phase 1: Wait until grace period before max_duration
            if duration > GRACE_PERIOD {
                let warning_time = duration - GRACE_PERIOD;
                tokio::time::sleep(warning_time).await;
                warn!(
                    session_id = %session_id,
                    "Session will disconnect in {:?} (max_duration limit)",
                    GRACE_PERIOD
                );
            }

            // Phase 2: Wait for remaining time
            let remaining = if duration > GRACE_PERIOD {
                GRACE_PERIOD
            } else {
                duration
            };
            tokio::time::sleep(remaining).await;

            // Send disconnect signal
            if let Some(tx) = disconnect_tx {
                info!(
                    session_id = %session_id,
                    duration = ?duration,
                    "Session max_duration reached, disconnecting"
                );
                let _ = tx.send(DisconnectReason::MaxDuration { duration });
            }
        });

        self.max_duration_task = Some(task);
        debug!(
            session_id = %self.session_id,
            duration = ?duration,
            "max_duration timer started"
        );
    }

    /// Get the idle timeout for this session.
    ///
    /// Returns the session-specific idle timeout if configured, or `None`
    /// to use the server default.
    pub fn get_idle_timeout(&self) -> Option<Duration> {
        self.config.idle_timeout
    }

    /// Increment the query count and check limits.
    ///
    /// This should be called for each query command (COM_QUERY, COM_STMT_EXECUTE, etc.)
    /// received from the client.
    ///
    /// # Returns
    ///
    /// * `Ok(count)` - Current query count after increment
    /// * `Err(DisconnectReason::MaxQueries)` - If the limit is exceeded
    ///
    /// # Thread Safety
    ///
    /// Uses atomic operations, safe to call from multiple tasks.
    pub fn increment_query_count(&mut self) -> Result<u64, DisconnectReason> {
        let count = self.query_count.fetch_add(1, Ordering::Relaxed) + 1;

        if let Some(max_queries) = self.config.max_queries {
            // Check warning threshold (90%)
            let warning_threshold = (max_queries as f64 * QUERY_WARNING_THRESHOLD) as u64;
            if count >= warning_threshold && !self.query_limit_warned {
                warn!(
                    session_id = %self.session_id,
                    count = count,
                    max = max_queries,
                    "Session approaching query limit ({}/{})",
                    count,
                    max_queries
                );
                self.query_limit_warned = true;
            }

            // Check limit
            if count >= max_queries {
                info!(
                    session_id = %self.session_id,
                    count = count,
                    "Session max_queries reached, disconnecting"
                );
                return Err(DisconnectReason::MaxQueries { count });
            }
        }

        debug!(
            session_id = %self.session_id,
            count = count,
            "Query count incremented"
        );

        Ok(count)
    }

    /// Get the current query count.
    pub fn query_count(&self) -> u64 {
        self.query_count.load(Ordering::Relaxed)
    }

    /// Take the disconnect receiver for relay monitoring.
    ///
    /// This transfers ownership of the receiver to the caller, typically
    /// the relay loop which will monitor for disconnect signals.
    ///
    /// # Returns
    ///
    /// The disconnect receiver, or `None` if already taken.
    pub fn take_disconnect_rx(&mut self) -> Option<oneshot::Receiver<DisconnectReason>> {
        self.disconnect_rx.take()
    }

    /// Get the elapsed time since the SessionManager was created.
    ///
    /// This is useful for logging and diagnostics. Note that the max_duration
    /// timer is independent and starts when `start_timers()` is called.
    pub fn elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Trigger a disconnect with the given reason.
    ///
    /// This sends a disconnect signal to the relay loop.
    pub fn trigger_disconnect(&mut self, reason: DisconnectReason) {
        if let Some(tx) = self.disconnect_tx.take() {
            info!(
                session_id = %self.session_id,
                reason = ?reason,
                "Disconnect triggered"
            );
            let _ = tx.send(reason);
        }
    }

    /// Shutdown the session manager (cancel timers).
    pub fn shutdown(&mut self) {
        // Cancel max_duration task if running
        if let Some(task) = self.max_duration_task.take() {
            task.abort();
            debug!(
                session_id = %self.session_id,
                "max_duration timer cancelled"
            );
        }

        // Drop disconnect_tx to signal shutdown
        self.disconnect_tx.take();
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_manager_new() {
        let config = SessionConfig::default();
        let mgr = SessionManager::new(config);

        assert_eq!(mgr.query_count(), 0);
        assert!(mgr.disconnect_rx.is_some());
    }

    #[test]
    fn test_session_id_unique() {
        let mgr1 = SessionManager::new(SessionConfig::default());
        let mgr2 = SessionManager::new(SessionConfig::default());

        assert_ne!(mgr1.session_id(), mgr2.session_id());
    }

    #[test]
    fn test_increment_query_count_no_limit() {
        let config = SessionConfig::default(); // max_queries = None
        let mut mgr = SessionManager::new(config);

        for i in 1..=1000 {
            assert_eq!(mgr.increment_query_count().unwrap(), i);
        }

        assert_eq!(mgr.query_count(), 1000);
    }

    #[test]
    fn test_increment_query_count_with_limit() {
        let config = SessionConfig::default().with_max_queries(10);
        let mut mgr = SessionManager::new(config);

        // Should succeed up to limit - 1
        for i in 1..10 {
            assert!(mgr.increment_query_count().is_ok());
            assert_eq!(mgr.query_count(), i);
        }

        // Should fail at limit
        let result = mgr.increment_query_count();
        assert!(matches!(
            result,
            Err(DisconnectReason::MaxQueries { count: 10 })
        ));
    }

    #[test]
    fn test_get_idle_timeout() {
        let config = SessionConfig::default().with_idle_timeout(Duration::from_secs(300));
        let mgr = SessionManager::new(config);

        assert_eq!(mgr.get_idle_timeout(), Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_get_idle_timeout_none() {
        let config = SessionConfig::default();
        let mgr = SessionManager::new(config);

        assert_eq!(mgr.get_idle_timeout(), None);
    }

    #[test]
    fn test_disconnect_reason_mysql_codes() {
        assert_eq!(
            DisconnectReason::MaxDuration {
                duration: Duration::from_secs(1)
            }
            .mysql_error_code(),
            Some(1317)
        );
        assert_eq!(
            DisconnectReason::IdleTimeout {
                duration: Duration::from_secs(1)
            }
            .mysql_error_code(),
            Some(1317)
        );
        assert_eq!(
            DisconnectReason::MaxQueries { count: 100 }.mysql_error_code(),
            Some(1317)
        );
        assert_eq!(
            DisconnectReason::ConnectionLimitExceeded.mysql_error_code(),
            Some(1040)
        );
        assert_eq!(
            DisconnectReason::TokenInvalid("test".into()).mysql_error_code(),
            Some(1045)
        );
        assert_eq!(DisconnectReason::ClientDisconnect.mysql_error_code(), None);
        assert_eq!(DisconnectReason::ServerDisconnect.mysql_error_code(), None);
        assert_eq!(
            DisconnectReason::IoError("test".into()).mysql_error_code(),
            None
        );
    }

    #[test]
    fn test_disconnect_reason_messages() {
        let msg = DisconnectReason::MaxDuration {
            duration: Duration::from_secs(3600),
        }
        .message();
        assert!(msg.contains("max duration"));

        let msg = DisconnectReason::MaxQueries { count: 100 }.message();
        assert!(msg.contains("query limit"));
        assert!(msg.contains("100"));

        let msg = DisconnectReason::TokenInvalid("expired".into()).message();
        assert!(msg.contains("token"));
        assert!(msg.contains("expired"));
    }

    #[test]
    fn test_take_disconnect_rx() {
        let mut mgr = SessionManager::new(SessionConfig::default());

        // First take should succeed
        assert!(mgr.take_disconnect_rx().is_some());

        // Second take should return None
        assert!(mgr.take_disconnect_rx().is_none());
    }

    #[test]
    fn test_trigger_disconnect() {
        let mut mgr = SessionManager::new(SessionConfig::default());
        let mut rx = mgr.take_disconnect_rx().unwrap();

        mgr.trigger_disconnect(DisconnectReason::ClientDisconnect);

        // Channel should have received the message
        let result = rx.try_recv();
        assert!(result.is_ok());
        assert!(matches!(
            result.unwrap(),
            DisconnectReason::ClientDisconnect
        ));
    }

    #[test]
    fn test_session_elapsed() {
        let mgr = SessionManager::new(SessionConfig::default());
        let elapsed = mgr.elapsed();

        // Elapsed time should be very small (just created)
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn test_config_access() {
        let config = SessionConfig::default()
            .with_max_queries(500)
            .with_require_token(true);
        let mgr = SessionManager::new(config);

        assert_eq!(mgr.config().max_queries, Some(500));
        assert!(mgr.config().require_token);
    }

    #[tokio::test]
    async fn test_max_duration_timer_fires() {
        let config = SessionConfig::default().with_max_duration(Duration::from_millis(100));
        let mut mgr = SessionManager::new(config);
        let rx = mgr.take_disconnect_rx().unwrap();

        mgr.start_timers();

        // Wait for timer
        let result = tokio::time::timeout(Duration::from_secs(1), rx).await;
        assert!(result.is_ok());

        let reason = result.unwrap().unwrap();
        assert!(matches!(reason, DisconnectReason::MaxDuration { .. }));
    }

    #[tokio::test]
    async fn test_timer_cancelled_on_shutdown() {
        let config = SessionConfig::default().with_max_duration(Duration::from_secs(10));
        let mut mgr = SessionManager::new(config);
        let mut rx = mgr.take_disconnect_rx().unwrap();

        mgr.start_timers();

        // Shutdown should cancel the timer
        mgr.shutdown();

        // Give it a moment
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Channel should be closed (not receive a message)
        let result = rx.try_recv();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_timer_cancelled_on_drop() {
        let mut rx = {
            let config = SessionConfig::default().with_max_duration(Duration::from_secs(10));
            let mut mgr = SessionManager::new(config);
            let rx = mgr.take_disconnect_rx().unwrap();
            mgr.start_timers();
            rx
            // mgr dropped here
        };

        // Give it a moment
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Channel should be closed
        let result = rx.try_recv();
        assert!(result.is_err());
    }

    #[test]
    fn test_query_warning_threshold() {
        let config = SessionConfig::default().with_max_queries(100);
        let mut mgr = SessionManager::new(config);

        // Execute 89 queries - no warning yet
        for _ in 0..89 {
            mgr.increment_query_count().unwrap();
        }
        assert!(!mgr.query_limit_warned);

        // 90th query should trigger warning
        mgr.increment_query_count().unwrap();
        assert!(mgr.query_limit_warned);

        // Warning flag should stay true
        mgr.increment_query_count().unwrap();
        assert!(mgr.query_limit_warned);
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_trigger_disconnect_idempotent() {
        let mut mgr = SessionManager::new(SessionConfig::default());
        let mut rx = mgr.take_disconnect_rx().unwrap();

        // First trigger should work
        mgr.trigger_disconnect(DisconnectReason::ClientDisconnect);
        let result = rx.try_recv();
        assert!(result.is_ok());

        // Second trigger should be no-op (tx already consumed)
        mgr.trigger_disconnect(DisconnectReason::MaxDuration {
            duration: Duration::from_secs(1),
        });
        // Should not panic, just be silently ignored
    }

    #[tokio::test]
    async fn test_shutdown_idempotent() {
        let config = SessionConfig::default().with_max_duration(Duration::from_secs(100));
        let mut mgr = SessionManager::new(config);
        mgr.start_timers();

        // Multiple shutdowns should not panic
        mgr.shutdown();
        mgr.shutdown();
        mgr.shutdown();
    }

    #[test]
    fn test_query_count_boundary_exact_limit() {
        let config = SessionConfig::default().with_max_queries(5);
        let mut mgr = SessionManager::new(config);

        // Queries 1-4 succeed
        for _ in 0..4 {
            assert!(mgr.increment_query_count().is_ok());
        }

        // Query 5 (at limit) fails
        let result = mgr.increment_query_count();
        assert!(matches!(
            result,
            Err(DisconnectReason::MaxQueries { count: 5 })
        ));

        // Verify count is exactly at limit
        assert_eq!(mgr.query_count(), 5);
    }

    #[test]
    fn test_query_count_single_query_limit() {
        // Edge case: limit of 1 query
        let config = SessionConfig::default().with_max_queries(1);
        let mut mgr = SessionManager::new(config);

        // First query hits the limit
        let result = mgr.increment_query_count();
        assert!(matches!(
            result,
            Err(DisconnectReason::MaxQueries { count: 1 })
        ));
    }

    #[test]
    fn test_start_timers_no_limits() {
        // Session with no time limits - start_timers should be no-op
        let config = SessionConfig::default();
        let mut mgr = SessionManager::new(config);

        // Should not panic
        mgr.start_timers();

        // No task should be spawned
        assert!(mgr.max_duration_task.is_none());
    }

    // ========================================================================
    // Security Tests
    // ========================================================================

    #[test]
    fn test_disconnect_reason_messages_no_sensitive_data() {
        // Verify error messages don't leak tokens or secrets
        let reasons = vec![
            DisconnectReason::MaxDuration {
                duration: Duration::from_secs(3600),
            },
            DisconnectReason::MaxQueries { count: 100 },
            DisconnectReason::IdleTimeout {
                duration: Duration::from_secs(300),
            },
            DisconnectReason::TokenInvalid("some-error-reason".into()),
            DisconnectReason::TokenInvalid("expired".into()),
            DisconnectReason::ConnectionLimitExceeded,
            DisconnectReason::ClientDisconnect,
            DisconnectReason::ServerDisconnect,
            DisconnectReason::IoError("connection reset".into()),
        ];

        for reason in reasons {
            let message = reason.message();
            // Messages should not contain potential token patterns
            assert!(
                !message.contains("Bearer"),
                "Message should not contain token patterns"
            );
            assert!(
                !message.contains("eyJ"),
                "Message should not contain JWT patterns"
            );
            // Messages should be informative but not expose internals
            assert!(!message.is_empty(), "Message should not be empty");
        }
    }

    #[test]
    fn test_mysql_error_codes_standard() {
        // Verify MySQL error codes are standard codes
        // 1045 = ER_ACCESS_DENIED_ERROR
        // 1040 = ER_CON_COUNT_ERROR
        // 1317 = ER_QUERY_INTERRUPTED

        assert_eq!(
            DisconnectReason::TokenInvalid("".into()).mysql_error_code(),
            Some(1045) // Standard access denied
        );
        assert_eq!(
            DisconnectReason::TokenInvalid("expired".into()).mysql_error_code(),
            Some(1045) // Standard access denied
        );
        assert_eq!(
            DisconnectReason::ConnectionLimitExceeded.mysql_error_code(),
            Some(1040) // Standard too many connections
        );
        assert_eq!(
            DisconnectReason::MaxDuration {
                duration: Duration::from_secs(1)
            }
            .mysql_error_code(),
            Some(1317) // Query interrupted
        );
        assert_eq!(
            DisconnectReason::MaxQueries { count: 1 }.mysql_error_code(),
            Some(1317) // Query interrupted
        );
        assert_eq!(
            DisconnectReason::IdleTimeout {
                duration: Duration::from_secs(1)
            }
            .mysql_error_code(),
            Some(1317) // Query interrupted
        );
    }

    #[test]
    fn test_session_id_is_valid_uuid() {
        let mgr = SessionManager::new(SessionConfig::default());
        let session_id = mgr.session_id();

        // Verify it's a valid v4 UUID (random)
        assert_eq!(session_id.get_version_num(), 4);
    }
}
