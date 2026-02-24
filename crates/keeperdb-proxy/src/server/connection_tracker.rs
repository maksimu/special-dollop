//! Connection tracker for connection limit enforcement.
//!
//! This module provides the [`ConnectionTracker`] struct which tracks active
//! connections per tunnel to enforce connection limits with grace period support.
//!
//! # Application Fingerprint Security Model
//!
//! The connection tracker implements an "application lock" feature that validates
//! client application identity across connections to the same database target.
//! This provides defense-in-depth against session hijacking by different tools.
//!
//! ## How It Works
//!
//! 1. The first connection to a target "claims" it with a client fingerprint
//! 2. Subsequent connections must provide a matching fingerprint
//! 3. After all connections disconnect, the lock is retained for a configurable
//!    timeout (default 5 seconds) to allow reconnection
//! 4. After the timeout, a different tool can connect
//!
//! ## Protocol Differences
//!
//! ### MySQL (Strong Fingerprinting)
//!
//! MySQL clients send rich `connect_attrs` containing:
//! - `program_name` / `_client_name`: Application name
//! - `_client_version`: Version number
//! - `_platform` / `_os`: Operating system
//! - `_runtime_vendor`: Java vendor for JDBC clients
//!
//! These attributes create a robust fingerprint that can distinguish between
//! different tools and even different versions of the same tool.
//!
//! ### PostgreSQL (Weak Fingerprinting)
//!
//! PostgreSQL's startup message contains only session parameters, not client
//! identification. The fingerprint relies on:
//! - `application_name`: Primary identifier (optional, many clients don't set it)
//! - Fallback parameters: `client_encoding`, `DateStyle`, `TimeZone`, etc.
//!
//! **Known Limitations:**
//! - Many PostgreSQL clients (e.g., JetBrains DataGrip) don't set `application_name`
//! - Fallback parameters may produce identical fingerprints for different tools
//! - The fingerprint can be easily spoofed
//!
//! ## Security Recommendations
//!
//! The application fingerprint should be treated as **defense-in-depth**, not
//! primary security. For stronger protection, combine with:
//! - Session UID validation (ensures same PAM session)
//! - Network controls (IP allowlisting, VPN)
//! - Short session timeouts
//!
//! The fingerprint is most effective at detecting accidental tool switching
//! (e.g., user opens DBeaver while pgAdmin session is active) rather than
//! intentional attacks.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{ProxyError, Result};

/// Identifier for a tunnel/session grouping.
///
/// Used to track connections per PAM session or client connection.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TunnelId(String);

impl TunnelId {
    /// Create from record UID (token-based).
    ///
    /// This is the preferred method when using Gateway token validation,
    /// as the record_uid uniquely identifies the PAM session.
    ///
    /// # Arguments
    ///
    /// * `uid` - The record UID from token validation
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::server::TunnelId;
    ///
    /// let id = TunnelId::from_record_uid("abc123");
    /// ```
    pub fn from_record_uid(uid: &str) -> Self {
        Self(format!("record:{}", uid))
    }

    /// Create from client address and target (fallback).
    ///
    /// Used when token validation is not enabled and we need to
    /// identify connections by their network endpoints.
    ///
    /// # Arguments
    ///
    /// * `client_addr` - Client IP address or socket address
    /// * `target_host` - Target database hostname
    /// * `target_port` - Target database port
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::server::TunnelId;
    ///
    /// let id = TunnelId::from_connection("192.168.1.1:12345", "db.example.com", 3306);
    /// assert!(id.to_string().contains("192.168.1.1"));
    /// // Note: source port is stripped for connection limiting per client IP
    /// ```
    pub fn from_connection(client_addr: &str, target_host: &str, target_port: u16) -> Self {
        // Strip the port from client_addr to get just the IP
        // This allows connection limiting to work per client IP, not per TCP connection
        let client_ip = client_addr
            .rsplit_once(':')
            .map(|(ip, _port)| ip)
            .unwrap_or(client_addr);
        Self(format!(
            "conn:{}->{}:{}",
            client_ip, target_host, target_port
        ))
    }
}

impl std::fmt::Display for TunnelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifier for a database target (host:port).
///
/// Used to enforce single-session-per-target security policy.
/// Only one session_uid can be active for a given target at a time.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TargetId(String);

impl TargetId {
    /// Create a target ID from host and port.
    pub fn new(host: &str, port: u16) -> Self {
        Self(format!("{}:{}", host, port))
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Normalize an application name to extract the base tool identifier.
///
/// Many database tools include dynamic information in the application name:
/// - pgAdmin: "pgAdmin 4 - DB:postgres" -> "pgAdmin 4"
/// - DBeaver: "DBeaver 23.0.0 - PostgreSQL" -> "DBeaver"
/// - DataGrip: "DataGrip 2023.1" -> "DataGrip"
///
/// This function extracts just the tool name for consistent comparison.
fn normalize_application_name(app_name: &str) -> String {
    let name = app_name.trim();

    // Handle pgAdmin's "pgAdmin 4 - DB:xxx" format
    if let Some(idx) = name.find(" - DB:") {
        return name[..idx].trim().to_string();
    }

    // Handle pgAdmin's "pgAdmin 4 - CONN:xxx" format (connection ID)
    if let Some(idx) = name.find(" - CONN:") {
        return name[..idx].trim().to_string();
    }

    // Handle general " - " separator (e.g., "DBeaver 23.0.0 - PostgreSQL")
    if let Some(idx) = name.find(" - ") {
        return name[..idx].trim().to_string();
    }

    // Handle version numbers at the end (e.g., "DataGrip 2023.1")
    // Keep the first word(s) before any version-like pattern
    let words: Vec<&str> = name.split_whitespace().collect();
    if words.len() > 1 {
        // Check if last word looks like a version (starts with digit)
        if let Some(last) = words.last() {
            if last
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
            {
                // Return everything except the version
                return words[..words.len() - 1].join(" ");
            }
        }
    }

    name.to_string()
}

/// State for a tunnel with potentially multiple connections.
#[derive(Debug, Clone)]
pub struct TunnelState {
    /// When the first connection was established (starts grace period).
    pub first_connection_time: Instant,
    /// All active session IDs for this tunnel.
    pub active_sessions: HashSet<Uuid>,
    /// Normalized application name from the first connection (for security validation).
    /// In PostgreSQL this comes from the `application_name` parameter.
    /// In MySQL this comes from the `program_name` connect attribute.
    /// Subsequent connections must match this normalized value if set.
    ///
    /// Note: Application names are normalized to extract the base tool name,
    /// ignoring database-specific suffixes like "pgAdmin 4 - DB:postgres".
    pub application_name: Option<String>,
}

impl TunnelState {
    /// Create a new tunnel state with the first connection.
    pub fn new(session_id: Uuid) -> Self {
        Self::with_application_name(session_id, None)
    }

    /// Create a new tunnel state with the first connection and application name.
    /// The application name is normalized to extract just the base tool name.
    pub fn with_application_name(session_id: Uuid, application_name: Option<String>) -> Self {
        let mut sessions = HashSet::new();
        sessions.insert(session_id);
        Self {
            first_connection_time: Instant::now(),
            active_sessions: sessions,
            // Normalize the application name to handle tool-specific suffixes
            application_name: application_name.map(|s| normalize_application_name(&s)),
        }
    }

    /// Add a session to this tunnel.
    pub fn add_session(&mut self, session_id: Uuid) {
        self.active_sessions.insert(session_id);
    }

    /// Remove a session from this tunnel.
    /// Returns true if the tunnel is now empty.
    pub fn remove_session(&mut self, session_id: &Uuid) -> bool {
        self.active_sessions.remove(session_id);
        self.active_sessions.is_empty()
    }

    /// Get the number of active connections.
    pub fn connection_count(&self) -> usize {
        self.active_sessions.len()
    }

    /// Check if the grace period has expired.
    pub fn grace_period_expired(&self, grace_period: Duration) -> bool {
        self.first_connection_time.elapsed() > grace_period
    }
}

/// Legacy alias for backwards compatibility.
pub type ConnectionInfo = TunnelState;

/// Information about an active session for a target.
#[derive(Debug, Clone)]
pub struct TargetSession {
    /// The session UID that owns this target
    pub session_uid: String,
    /// The normalized application name that first connected (for validation)
    pub application_name: Option<String>,
    /// When the last connection closed (None if connections are active)
    /// Used to implement a timeout for the application_name lock
    pub last_disconnect: Option<Instant>,
}

/// Default duration to retain the application_name lock after all connections close.
/// After this timeout, a different tool can connect to the same target.
/// This can be overridden via configuration.
const DEFAULT_APPLICATION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a disconnected session entry should be kept before being cleaned up.
/// This should be significantly longer than APPLICATION_LOCK_TIMEOUT to avoid
/// cleaning up entries that might still be needed for reconnection.
/// Default: 5 minutes
const STALE_SESSION_THRESHOLD: Duration = Duration::from_secs(300);

/// How often the background cleanup task runs.
/// Default: 60 seconds
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// Thread-safe tracker for active connections.
///
/// Tracks connections per tunnel and enforces connection limits with grace period support.
/// During the grace period after the first connection, additional connections are allowed
/// up to the max limit. After the grace period expires, no new connections are accepted.
///
/// # Example
///
/// ```
/// use keeperdb_proxy::server::{ConnectionTracker, TunnelId};
/// use uuid::Uuid;
/// use std::time::Duration;
///
/// # #[tokio::main]
/// # async fn main() {
/// let tracker = ConnectionTracker::new();
/// let tunnel_id = TunnelId::from_record_uid("test-uid");
/// let session_id = Uuid::new_v4();
///
/// // First registration succeeds
/// let guard = tracker.try_register_with_limits(
///     tunnel_id.clone(),
///     session_id,
///     Some(2), // max 2 connections
///     Some(Duration::from_secs(1)), // 1 second grace period
/// ).await.unwrap();
///
/// // Second registration succeeds within grace period
/// let guard2 = tracker.try_register_with_limits(
///     tunnel_id.clone(),
///     Uuid::new_v4(),
///     Some(2),
///     Some(Duration::from_secs(1)),
/// ).await.unwrap();
///
/// // Clean up
/// guard.release_async(&tracker).await;
/// guard2.release_async(&tracker).await;
/// # }
/// ```
pub struct ConnectionTracker {
    /// Tunnels (session_uid -> connection state)
    tunnels: RwLock<HashMap<TunnelId, TunnelState>>,
    /// Active session per target (target -> session info including application_name)
    /// Enforces single-session-per-target security policy
    /// The application_name is persisted here so it survives tunnel removal
    active_sessions: RwLock<HashMap<TargetId, TargetSession>>,
}

impl ConnectionTracker {
    /// Create a new connection tracker.
    pub fn new() -> Self {
        Self {
            tunnels: RwLock::new(HashMap::new()),
            active_sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Create a shared instance wrapped in Arc.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Try to register a new connection with limits.
    ///
    /// # Arguments
    ///
    /// * `tunnel_id` - Unique identifier for the tunnel
    /// * `session_id` - UUID of the session
    /// * `max_connections` - Maximum connections allowed (None = unlimited)
    /// * `grace_period` - Time window after first connection where new connections are allowed
    ///
    /// # Logic
    ///
    /// 1. If no existing connections for this tunnel → allow, start grace period timer
    /// 2. If within grace period AND under max_connections → allow
    /// 3. If grace period expired OR at max_connections → reject
    ///
    /// # Thread Safety
    ///
    /// Uses RwLock, safe to call concurrently.
    pub async fn try_register_with_limits(
        &self,
        tunnel_id: TunnelId,
        session_id: Uuid,
        max_connections: Option<u32>,
        grace_period: Option<Duration>,
    ) -> Result<ConnectionGuard> {
        self.try_register_with_limits_and_app(
            tunnel_id,
            session_id,
            max_connections,
            grace_period,
            None,
        )
        .await
    }

    /// Try to register a new connection with limits and application name validation.
    ///
    /// # Arguments
    ///
    /// * `tunnel_id` - Unique identifier for the tunnel
    /// * `session_id` - UUID of the session
    /// * `max_connections` - Maximum connections allowed (None = unlimited)
    /// * `grace_period` - Time window after first connection where new connections are allowed
    /// * `application_name` - Client application identifier (e.g., "pgAdmin 4", "DBeaver")
    ///
    /// # Security
    ///
    /// If the first connection provides an application_name, subsequent connections
    /// must provide the same application_name or they will be rejected. This provides
    /// an additional layer of security against attackers using different tools to
    /// hijack a session, though it can be spoofed by determined attackers.
    pub async fn try_register_with_limits_and_app(
        &self,
        tunnel_id: TunnelId,
        session_id: Uuid,
        max_connections: Option<u32>,
        grace_period: Option<Duration>,
        application_name: Option<String>,
    ) -> Result<ConnectionGuard> {
        let mut tunnels = self.tunnels.write().await;
        let grace = grace_period.unwrap_or(Duration::from_secs(1));

        if let Some(state) = tunnels.get_mut(&tunnel_id) {
            // Tunnel exists - check limits
            let current_count = state.connection_count();
            let grace_expired = state.grace_period_expired(grace);

            // Check application_name matches if the tunnel has one set
            // Both stored and incoming names are normalized for comparison
            // Note: Error messages are intentionally generic to avoid revealing the security mechanism
            if let Some(ref expected_app) = state.application_name {
                let normalized_provided = application_name
                    .as_ref()
                    .map(|s| normalize_application_name(s));
                match &normalized_provided {
                    Some(provided_app) if provided_app != expected_app => {
                        warn!(
                            tunnel_id = %tunnel_id,
                            session_id = %session_id,
                            expected_app = %expected_app,
                            provided_app = %provided_app,
                            raw_provided = ?application_name,
                            "Connection rejected: application_name mismatch (reported as connection limit)"
                        );
                        // Generic error to avoid revealing security mechanism
                        return Err(ProxyError::SessionLimit(
                            "Too many connections for this session".to_string(),
                        ));
                    }
                    None => {
                        warn!(
                            tunnel_id = %tunnel_id,
                            session_id = %session_id,
                            expected_app = %expected_app,
                            "Connection rejected: application_name required but not provided (reported as connection limit)"
                        );
                        // Generic error to avoid revealing security mechanism
                        return Err(ProxyError::SessionLimit(
                            "Too many connections for this session".to_string(),
                        ));
                    }
                    _ => {
                        // application_name matches - proceed
                        debug!(
                            tunnel_id = %tunnel_id,
                            normalized_app = %expected_app,
                            "Application name validated"
                        );
                    }
                }
            }

            // Check max connections limit
            if let Some(max) = max_connections {
                if current_count >= max as usize {
                    warn!(
                        tunnel_id = %tunnel_id,
                        session_id = %session_id,
                        current_connections = current_count,
                        max_connections = max,
                        "Connection rejected: max connections reached"
                    );
                    return Err(ProxyError::SessionLimit(format!(
                        "Connection limit exceeded: max {} connections",
                        max
                    )));
                }
            }

            // Under the max limit - allow the connection
            // Grace period is now informational only (logged but not enforced)
            state.add_session(session_id);
            if grace_expired {
                debug!(
                    tunnel_id = %tunnel_id,
                    session_id = %session_id,
                    connection_count = state.connection_count(),
                    max_connections = ?max_connections,
                    application_name = ?application_name,
                    "Additional connection registered (grace period expired, but under max)"
                );
            } else {
                debug!(
                    tunnel_id = %tunnel_id,
                    session_id = %session_id,
                    connection_count = state.connection_count(),
                    grace_remaining_ms = (grace.saturating_sub(state.first_connection_time.elapsed())).as_millis(),
                    application_name = ?application_name,
                    "Additional connection registered within grace period"
                );
            }
        } else {
            // First connection for this tunnel
            let state = TunnelState::with_application_name(session_id, application_name.clone());
            tunnels.insert(tunnel_id.clone(), state);
            debug!(
                tunnel_id = %tunnel_id,
                session_id = %session_id,
                grace_period_ms = grace.as_millis(),
                max_connections = ?max_connections,
                application_name = ?application_name,
                "First connection registered, grace period started"
            );
        }

        Ok(ConnectionGuard {
            tunnel_id,
            session_id,
            released: false,
            target_id: None,
            session_uid: None,
        })
    }

    /// Try to register a connection with single-session-per-target enforcement.
    ///
    /// This method ensures that only one session_uid can be active for a given target
    /// at a time. This prevents session hijacking where an attacker could piggyback
    /// on extra connection slots allocated for legitimate multi-connection tools.
    ///
    /// # Arguments
    ///
    /// * `target_id` - The database target (host:port)
    /// * `session_uid` - The Gateway-provided session identifier (e.g., PAM session ID)
    /// * `tunnel_id` - Unique identifier for the tunnel (based on session_uid)
    /// * `session_id` - UUID of this specific TCP connection
    /// * `max_connections` - Maximum connections allowed for this session (None = unlimited)
    /// * `grace_period` - Time window after first connection where new connections are allowed
    ///
    /// # Security Model
    ///
    /// 1. First connection to a target "claims" it with the session_uid
    /// 2. Subsequent connections must have matching session_uid or be rejected
    /// 3. Target is released when ALL connections from that session disconnect
    pub async fn try_register_for_target(
        &self,
        target_id: TargetId,
        session_uid: String,
        tunnel_id: TunnelId,
        session_id: Uuid,
        max_connections: Option<u32>,
        grace_period: Option<Duration>,
    ) -> Result<ConnectionGuard> {
        self.try_register_for_target_with_app(
            target_id,
            session_uid,
            tunnel_id,
            session_id,
            max_connections,
            grace_period,
            None,
            None, // Use default application lock timeout
        )
        .await
    }

    /// Try to register a connection with single-session-per-target enforcement
    /// and application name validation.
    ///
    /// This method ensures that only one session_uid can be active for a given target
    /// at a time, AND validates that all connections use the same client application.
    ///
    /// # Arguments
    ///
    /// * `target_id` - The database target (host:port)
    /// * `session_uid` - The Gateway-provided session identifier (e.g., PAM session ID)
    /// * `tunnel_id` - Unique identifier for the tunnel (based on session_uid)
    /// * `session_id` - UUID of this specific TCP connection
    /// * `max_connections` - Maximum connections allowed for this session (None = unlimited)
    /// * `grace_period` - Time window after first connection where new connections are allowed
    /// * `application_name` - Client application identifier for validation
    /// * `application_lock_timeout` - How long to retain app lock after disconnect (None = default 5s)
    ///
    /// # Security Model
    ///
    /// 1. First connection to a target "claims" it with the session_uid and application_name
    /// 2. Subsequent connections must have matching session_uid AND application_name
    /// 3. Target is released when ALL connections from that session disconnect
    /// 4. Application name is stored with target claim and persists across reconnections
    #[allow(clippy::too_many_arguments)]
    pub async fn try_register_for_target_with_app(
        &self,
        target_id: TargetId,
        session_uid: String,
        tunnel_id: TunnelId,
        session_id: Uuid,
        max_connections: Option<u32>,
        grace_period: Option<Duration>,
        application_name: Option<String>,
        application_lock_timeout: Option<Duration>,
    ) -> Result<ConnectionGuard> {
        // Normalize the application name for consistent comparison
        let normalized_app = application_name
            .as_ref()
            .map(|s| normalize_application_name(s));

        // First, check/claim the target for this session_uid
        {
            let mut active_sessions = self.active_sessions.write().await;

            if let Some(target_session) = active_sessions.get_mut(&target_id) {
                // Target has an existing session entry - verify session_uid matches
                if target_session.session_uid != session_uid {
                    // Different session_uid - check if previous session is fully disconnected
                    // A session is considered "gone" if:
                    // 1. last_disconnect is set (normal cleanup path), OR
                    // 2. The tunnel no longer exists or has no connections (abrupt disconnection,
                    //    e.g., WebRTC tunnel died but TCP connections weren't cleaned up yet)
                    let previous_session_gone = if target_session.last_disconnect.is_some() {
                        true
                    } else {
                        // Check if the tunnel for the previous session still exists with connections
                        let old_tunnel_id = TunnelId::from_record_uid(&target_session.session_uid);
                        let tunnels = self.tunnels.read().await;
                        let tunnel_has_connections = tunnels
                            .get(&old_tunnel_id)
                            .map(|state| !state.active_sessions.is_empty())
                            .unwrap_or(false);
                        !tunnel_has_connections
                    };

                    if previous_session_gone {
                        // Previous session fully disconnected or tunnel gone - allow takeover
                        info!(
                            target = %target_id,
                            old_session = %target_session.session_uid,
                            new_session = %session_uid,
                            "Previous session disconnected, allowing new session takeover"
                        );
                        // Update to new session
                        target_session.session_uid = session_uid.clone();
                        target_session.application_name = normalized_app.clone();
                        target_session.last_disconnect = None;
                        // Continue with tunnel registration below
                    } else {
                        // Previous session still has active connections - reject
                        warn!(
                            target = %target_id,
                            active_session = %target_session.session_uid,
                            rejected_session = %session_uid,
                            "Connection rejected: another session is active for this target"
                        );
                        return Err(ProxyError::SessionLimit(format!(
                            "Another session is already active for target {}",
                            target_id
                        )));
                    }
                }

                // Same session_uid - check if application_name lock has timed out
                let lock_timeout =
                    application_lock_timeout.unwrap_or(DEFAULT_APPLICATION_LOCK_TIMEOUT);
                let lock_expired = target_session
                    .last_disconnect
                    .map(|t| t.elapsed() > lock_timeout)
                    .unwrap_or(false);

                // Check application_name (unless lock has expired)
                // The application_name is stored at the target level so it persists
                // even after all connections disconnect and tunnel is removed
                if !lock_expired {
                    if let Some(ref expected_app) = target_session.application_name {
                        match &normalized_app {
                            Some(provided_app) if provided_app != expected_app => {
                                warn!(
                                    target = %target_id,
                                    session_uid = %session_uid,
                                    expected_app = %expected_app,
                                    provided_app = %provided_app,
                                    raw_provided = ?application_name,
                                    "Connection rejected: application_name mismatch at target level"
                                );
                                // Generic error to avoid revealing security mechanism
                                return Err(ProxyError::SessionLimit(
                                    "Too many connections for this session".to_string(),
                                ));
                            }
                            None => {
                                warn!(
                                    target = %target_id,
                                    session_uid = %session_uid,
                                    expected_app = %expected_app,
                                    "Connection rejected: application_name required but not provided"
                                );
                                // Generic error to avoid revealing security mechanism
                                return Err(ProxyError::SessionLimit(
                                    "Too many connections for this session".to_string(),
                                ));
                            }
                            _ => {
                                // application_name matches - proceed
                                debug!(
                                    target = %target_id,
                                    normalized_app = %expected_app,
                                    "Application name validated at target level"
                                );
                            }
                        }
                    }
                } else {
                    // Lock expired - allow the new tool and update the application_name
                    debug!(
                        target = %target_id,
                        session_uid = %session_uid,
                        old_app = ?target_session.application_name,
                        new_app = ?normalized_app,
                        "Application lock expired, allowing new tool"
                    );
                    target_session.application_name = normalized_app.clone();
                    target_session.last_disconnect = None;
                }

                // Only clear last_disconnect if we have an app name (handled above for lock_expired case)
                if !lock_expired && normalized_app.is_some() {
                    target_session.last_disconnect = None;
                }

                debug!(
                    target = %target_id,
                    session_uid = %session_uid,
                    "Session matches active session for target"
                );
            } else {
                // No active session - claim the target with session_uid AND application_name
                let target_session = TargetSession {
                    session_uid: session_uid.clone(),
                    application_name: normalized_app.clone(),
                    last_disconnect: None,
                };
                active_sessions.insert(target_id.clone(), target_session);
                debug!(
                    target = %target_id,
                    session_uid = %session_uid,
                    application_name = ?normalized_app,
                    "Target claimed by session"
                );
            }
        }

        // Now proceed with normal connection limit registration
        // Note: We skip application_name validation at tunnel level since we already
        // validated at target level (which persists across reconnections)
        let result = self
            .try_register_with_limits_and_app(
                tunnel_id.clone(),
                session_id,
                max_connections,
                grace_period,
                application_name, // Still pass for logging/tunnel state
            )
            .await;

        // If registration failed, we might need to release the target claim
        // (only if this was the first connection attempt for this session)
        if result.is_err() {
            let tunnels = self.tunnels.read().await;
            if !tunnels.contains_key(&tunnel_id) {
                // No connections exist for this tunnel - release the target claim
                let mut active_sessions = self.active_sessions.write().await;
                if let Some(target_session) = active_sessions.get(&target_id) {
                    if target_session.session_uid == session_uid {
                        active_sessions.remove(&target_id);
                        debug!(
                            target = %target_id,
                            session_uid = %session_uid,
                            "Target claim released (registration failed, no connections)"
                        );
                    }
                }
            }
        }

        // Wrap the guard with target info for proper cleanup
        result.map(|mut guard| {
            // Mark the original guard as released since we're creating a new one
            guard.mark_released();
            ConnectionGuard {
                tunnel_id: guard.tunnel_id.clone(),
                session_id: guard.session_id,
                released: false, // The new guard is not released
                target_id: Some(target_id),
                session_uid: Some(session_uid),
            }
        })
    }

    /// Legacy method - try to register with strict single-connection mode.
    ///
    /// This is equivalent to `try_register_with_limits(tunnel_id, session_id, Some(1), None)`.
    pub async fn try_register(
        &self,
        tunnel_id: TunnelId,
        session_id: Uuid,
    ) -> Result<ConnectionGuard> {
        self.try_register_with_limits(tunnel_id, session_id, Some(1), Some(Duration::from_secs(0)))
            .await
    }

    /// Release a specific session from a tunnel.
    ///
    /// If the tunnel becomes empty, it is removed entirely.
    pub async fn release_session(&self, tunnel_id: &TunnelId, session_id: &Uuid) {
        let mut tunnels = self.tunnels.write().await;
        if let Some(state) = tunnels.get_mut(tunnel_id) {
            let is_empty = state.remove_session(session_id);
            debug!(
                tunnel_id = %tunnel_id,
                session_id = %session_id,
                remaining_connections = state.connection_count(),
                "Connection released from tunnel"
            );
            if is_empty {
                tunnels.remove(tunnel_id);
                debug!(tunnel_id = %tunnel_id, "Tunnel removed (no active connections)");
            }
        } else {
            debug!(
                tunnel_id = %tunnel_id,
                session_id = %session_id,
                "Attempted to release session from non-existent tunnel"
            );
        }
    }

    /// Release a session from a tunnel.
    ///
    /// This method should be used when single-session-per-target enforcement is enabled.
    /// When the tunnel empties, sets `last_disconnect` on the target claim to start
    /// the application_name lock timeout. After the timeout expires, a different tool
    /// can connect.
    pub async fn release_session_for_target(
        &self,
        tunnel_id: &TunnelId,
        session_id: &Uuid,
        target_id: &TargetId,
        session_uid: &str,
    ) {
        let tunnel_empty = {
            let mut tunnels = self.tunnels.write().await;
            if let Some(state) = tunnels.get_mut(tunnel_id) {
                let is_empty = state.remove_session(session_id);
                debug!(
                    tunnel_id = %tunnel_id,
                    session_id = %session_id,
                    remaining_connections = state.connection_count(),
                    "Connection released from tunnel"
                );
                if is_empty {
                    tunnels.remove(tunnel_id);
                    debug!(tunnel_id = %tunnel_id, "Tunnel removed (no active connections)");
                    true
                } else {
                    false
                }
            } else {
                debug!(
                    tunnel_id = %tunnel_id,
                    session_id = %session_id,
                    "Attempted to release session from non-existent tunnel"
                );
                false
            }
        };

        // If tunnel is now empty, set last_disconnect to start the timeout
        if tunnel_empty {
            let mut active_sessions = self.active_sessions.write().await;
            if let Some(target_session) = active_sessions.get_mut(target_id) {
                if target_session.session_uid == session_uid {
                    target_session.last_disconnect = Some(Instant::now());
                    // Note: actual timeout may differ if configured; this logs the default
                    debug!(
                        target = %target_id,
                        session_uid = %session_uid,
                        timeout_secs = DEFAULT_APPLICATION_LOCK_TIMEOUT.as_secs(),
                        "Application lock timeout started"
                    );
                }
            }
        }
    }

    /// Legacy release method - releases entire tunnel.
    pub async fn release(&self, tunnel_id: &TunnelId) {
        let mut tunnels = self.tunnels.write().await;
        if let Some(state) = tunnels.remove(tunnel_id) {
            debug!(
                tunnel_id = %tunnel_id,
                sessions = state.connection_count(),
                duration = ?state.first_connection_time.elapsed(),
                "Tunnel released"
            );
        }
    }

    /// Force-release a tunnel and mark associated target sessions as disconnected.
    ///
    /// This method should be called when the underlying transport (e.g., WebRTC tunnel)
    /// dies but the TCP connections haven't been properly closed yet. It:
    /// 1. Removes the tunnel from tracking
    /// 2. Sets `last_disconnect` on any target sessions that use this tunnel's session_uid
    ///
    /// This allows new sessions to take over the target immediately instead of waiting
    /// for the stale cleanup to run.
    ///
    /// # Arguments
    ///
    /// * `session_uid` - The session UID (record UID) associated with the tunnel
    pub async fn force_release_tunnel(&self, session_uid: &str) {
        let tunnel_id = TunnelId::from_record_uid(session_uid);

        // Remove from tunnels
        {
            let mut tunnels = self.tunnels.write().await;
            if let Some(state) = tunnels.remove(&tunnel_id) {
                info!(
                    tunnel_id = %tunnel_id,
                    session_uid = %session_uid,
                    sessions = state.connection_count(),
                    "Tunnel force-released (transport disconnected)"
                );
            }
        }

        // Set last_disconnect on any active_sessions that use this session_uid
        // This allows new sessions to take over immediately
        {
            let mut active_sessions = self.active_sessions.write().await;
            for (target_id, target_session) in active_sessions.iter_mut() {
                if target_session.session_uid == session_uid
                    && target_session.last_disconnect.is_none()
                {
                    target_session.last_disconnect = Some(Instant::now());
                    info!(
                        target = %target_id,
                        session_uid = %session_uid,
                        "Target session marked as disconnected (tunnel force-released)"
                    );
                }
            }
        }
    }

    /// Check if any connections exist for a tunnel.
    pub async fn is_connected(&self, tunnel_id: &TunnelId) -> bool {
        let tunnels = self.tunnels.read().await;
        tunnels.contains_key(tunnel_id)
    }

    /// Get the number of active tunnels.
    pub async fn active_count(&self) -> usize {
        let tunnels = self.tunnels.read().await;
        tunnels.len()
    }

    /// Get the total number of active connections across all tunnels.
    pub async fn total_connections(&self) -> usize {
        let tunnels = self.tunnels.read().await;
        tunnels.values().map(|s| s.connection_count()).sum()
    }

    /// Get connection count for a specific tunnel.
    pub async fn tunnel_connection_count(&self, tunnel_id: &TunnelId) -> usize {
        let tunnels = self.tunnels.read().await;
        tunnels
            .get(tunnel_id)
            .map(|s| s.connection_count())
            .unwrap_or(0)
    }

    /// Get the number of tracked target sessions.
    ///
    /// This includes both active sessions and disconnected sessions that
    /// haven't been cleaned up yet.
    pub async fn target_session_count(&self) -> usize {
        let sessions = self.active_sessions.read().await;
        sessions.len()
    }

    /// Clean up stale session entries that have been disconnected for too long.
    ///
    /// Removes entries from `active_sessions` where:
    /// - `last_disconnect` is set (session is disconnected)
    /// - The disconnect time exceeds the threshold
    ///
    /// # Arguments
    ///
    /// * `threshold` - Duration after which disconnected sessions are considered stale.
    ///   If None, uses the default STALE_SESSION_THRESHOLD (5 minutes).
    ///
    /// # Returns
    ///
    /// The number of stale sessions that were removed.
    pub async fn cleanup_stale_sessions(&self, threshold: Option<Duration>) -> usize {
        let threshold = threshold.unwrap_or(STALE_SESSION_THRESHOLD);
        let mut sessions = self.active_sessions.write().await;

        let before_count = sessions.len();

        sessions.retain(|target_id, session| {
            // Keep if still connected (no last_disconnect)
            let Some(disconnect_time) = session.last_disconnect else {
                return true;
            };

            // Keep if not yet stale
            if disconnect_time.elapsed() <= threshold {
                return true;
            }

            // Remove stale entry
            debug!(
                target = %target_id,
                session_uid = %session.session_uid,
                application_name = ?session.application_name,
                disconnected_secs = disconnect_time.elapsed().as_secs(),
                "Removing stale session entry"
            );
            false
        });

        let removed = before_count - sessions.len();
        if removed > 0 {
            info!(
                removed_count = removed,
                remaining_count = sessions.len(),
                threshold_secs = threshold.as_secs(),
                "Cleaned up stale session entries"
            );
        }

        removed
    }

    /// Start a background task that periodically cleans up stale sessions.
    ///
    /// The task runs every CLEANUP_INTERVAL (60 seconds by default) and removes
    /// sessions that have been disconnected for longer than STALE_SESSION_THRESHOLD
    /// (5 minutes by default).
    ///
    /// # Arguments
    ///
    /// * `tracker` - Arc-wrapped tracker to clean up
    /// * `shutdown_rx` - Broadcast receiver to signal shutdown
    ///
    /// # Returns
    ///
    /// A JoinHandle for the spawned task.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use tokio::sync::broadcast;
    /// use std::sync::Arc;
    ///
    /// let tracker = ConnectionTracker::shared();
    /// let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    ///
    /// let cleanup_handle = ConnectionTracker::start_cleanup_task(
    ///     Arc::clone(&tracker),
    ///     shutdown_rx,
    /// );
    ///
    /// // Later, to stop the task:
    /// let _ = shutdown_tx.send(());
    /// ```
    pub fn start_cleanup_task(
        tracker: Arc<Self>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let removed = tracker.cleanup_stale_sessions(None).await;
                        if removed > 0 {
                            debug!(
                                removed = removed,
                                "Background cleanup completed"
                            );
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Session cleanup task shutting down");
                        break;
                    }
                }
            }
        })
    }
}

impl Default for ConnectionTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that tracks a registered connection.
///
/// When the guard is dropped, call `release()` on the tracker to
/// remove the connection registration. Note that since drop cannot
/// be async, you must call `release_async()` before dropping if
/// you want to ensure the connection is released immediately.
///
/// Alternatively, you can use `release_spawn()` which spawns a task
/// to release the connection asynchronously.
pub struct ConnectionGuard {
    tunnel_id: TunnelId,
    session_id: Uuid,
    released: bool,
    /// Target ID for single-session-per-target enforcement
    target_id: Option<TargetId>,
    /// Session UID for single-session-per-target enforcement
    session_uid: Option<String>,
}

impl ConnectionGuard {
    /// Get the tunnel ID.
    pub fn tunnel_id(&self) -> &TunnelId {
        &self.tunnel_id
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &Uuid {
        &self.session_id
    }

    /// Release the connection asynchronously.
    ///
    /// This should be called when you have access to the tracker
    /// and can await the release operation.
    pub async fn release_async(mut self, tracker: &ConnectionTracker) {
        if let (Some(target_id), Some(session_uid)) = (&self.target_id, &self.session_uid) {
            // Use target-aware release to clear target claim when tunnel empties
            tracker
                .release_session_for_target(
                    &self.tunnel_id,
                    &self.session_id,
                    target_id,
                    session_uid,
                )
                .await;
        } else {
            // Legacy release without target tracking
            tracker
                .release_session(&self.tunnel_id, &self.session_id)
                .await;
        }
        self.released = true;
    }

    /// Release the connection by spawning an async task.
    ///
    /// This is useful when you need to release the connection but
    /// cannot await (e.g., in a drop handler or sync context).
    ///
    /// # Arguments
    ///
    /// * `tracker` - Arc-wrapped tracker to release from
    pub fn release_spawn(mut self, tracker: Arc<ConnectionTracker>) {
        let tunnel_id = self.tunnel_id.clone();
        let session_id = self.session_id;
        let target_id = self.target_id.clone();
        let session_uid = self.session_uid.clone();
        self.released = true;
        tokio::spawn(async move {
            if let (Some(target_id), Some(session_uid)) = (target_id, session_uid) {
                // Use target-aware release to clear target claim when tunnel empties
                tracker
                    .release_session_for_target(&tunnel_id, &session_id, &target_id, &session_uid)
                    .await;
            } else {
                // Legacy release without target tracking
                tracker.release_session(&tunnel_id, &session_id).await;
            }
        });
    }

    /// Mark as released without actually releasing.
    ///
    /// Use this if the connection was released through other means.
    pub fn mark_released(&mut self) {
        self.released = true;
    }

    /// Check if this guard has been released.
    pub fn is_released(&self) -> bool {
        self.released
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if !self.released {
            // Log warning - connection wasn't properly released
            warn!(
                tunnel_id = %self.tunnel_id,
                "ConnectionGuard dropped without release - connection may not be cleaned up"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Application Name Normalization Tests
    // ========================================================================

    #[test]
    fn test_normalize_pgadmin_db_suffix() {
        // pgAdmin includes database name in application_name
        assert_eq!(
            normalize_application_name("pgAdmin 4 - DB:postgres"),
            "pgAdmin 4"
        );
        assert_eq!(
            normalize_application_name("pgAdmin 4 - DB:testdb"),
            "pgAdmin 4"
        );
        assert_eq!(
            normalize_application_name("pgAdmin 4 - DB:my_database_name"),
            "pgAdmin 4"
        );
    }

    #[test]
    fn test_normalize_pgadmin_conn_suffix() {
        // pgAdmin may include connection ID
        assert_eq!(
            normalize_application_name("pgAdmin 4 - CONN:123"),
            "pgAdmin 4"
        );
    }

    #[test]
    fn test_normalize_dbeaver() {
        // DBeaver includes version and database type
        assert_eq!(
            normalize_application_name("DBeaver 23.0.0 - PostgreSQL"),
            "DBeaver 23.0.0"
        );
        assert_eq!(
            normalize_application_name("DBeaver 23.1.0 - MySQL"),
            "DBeaver 23.1.0"
        );
    }

    #[test]
    fn test_normalize_version_suffix() {
        // Tools with version numbers at the end
        assert_eq!(normalize_application_name("DataGrip 2023.1"), "DataGrip");
        assert_eq!(normalize_application_name("HeidiSQL 12.0"), "HeidiSQL");
    }

    #[test]
    fn test_normalize_simple_names() {
        // Simple names without suffixes should remain unchanged
        assert_eq!(normalize_application_name("psql"), "psql");
        assert_eq!(
            normalize_application_name("MySQL Workbench"),
            "MySQL Workbench"
        );
        assert_eq!(normalize_application_name("TablePlus"), "TablePlus");
    }

    #[test]
    fn test_normalize_whitespace() {
        // Should trim whitespace
        assert_eq!(normalize_application_name("  psql  "), "psql");
        assert_eq!(
            normalize_application_name("  pgAdmin 4 - DB:test  "),
            "pgAdmin 4"
        );
    }

    // ========================================================================
    // Tunnel ID Tests
    // ========================================================================

    #[test]
    fn test_tunnel_id_from_record_uid() {
        let id = TunnelId::from_record_uid("abc123");
        assert_eq!(id.0, "record:abc123");
    }

    #[test]
    fn test_tunnel_id_from_connection() {
        // Source port is stripped for connection limiting per client IP
        let id = TunnelId::from_connection("192.168.1.1:12345", "db.example.com", 3306);
        assert_eq!(id.0, "conn:192.168.1.1->db.example.com:3306");

        // Different source ports should produce the same TunnelId
        let id2 = TunnelId::from_connection("192.168.1.1:54321", "db.example.com", 3306);
        assert_eq!(id, id2);
    }

    #[test]
    fn test_tunnel_id_equality() {
        let id1 = TunnelId::from_record_uid("same");
        let id2 = TunnelId::from_record_uid("same");
        let id3 = TunnelId::from_record_uid("different");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_tunnel_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(TunnelId::from_record_uid("a"));
        set.insert(TunnelId::from_record_uid("b"));
        set.insert(TunnelId::from_record_uid("a")); // duplicate

        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_tunnel_id_display() {
        let id = TunnelId::from_record_uid("test");
        assert_eq!(format!("{}", id), "record:test");
    }

    #[tokio::test]
    async fn test_tracker_new() {
        let tracker = ConnectionTracker::new();
        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_shared() {
        let tracker = ConnectionTracker::shared();
        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_register_success() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("test");
        let session_id = Uuid::new_v4();

        let result = tracker.try_register(tunnel_id.clone(), session_id).await;
        assert!(result.is_ok());
        assert_eq!(tracker.active_count().await, 1);
        assert!(tracker.is_connected(&tunnel_id).await);

        // Clean up
        let mut guard = result.unwrap();
        guard.mark_released();
    }

    #[tokio::test]
    async fn test_tracker_register_duplicate_fails() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("test");

        // First registration succeeds
        let result1 = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await;
        assert!(result1.is_ok());

        // Second registration fails
        let result2 = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await;
        assert!(result2.is_err());

        // Clean up
        let mut guard = result1.unwrap();
        guard.mark_released();
    }

    #[tokio::test]
    async fn test_tracker_different_tunnels() {
        let tracker = ConnectionTracker::new();
        let tunnel_id1 = TunnelId::from_record_uid("one");
        let tunnel_id2 = TunnelId::from_record_uid("two");

        // Both should succeed
        let result1 = tracker
            .try_register(tunnel_id1.clone(), Uuid::new_v4())
            .await;
        let result2 = tracker
            .try_register(tunnel_id2.clone(), Uuid::new_v4())
            .await;

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert_eq!(tracker.active_count().await, 2);

        // Clean up
        let mut guard1 = result1.unwrap();
        let mut guard2 = result2.unwrap();
        guard1.mark_released();
        guard2.mark_released();
    }

    #[tokio::test]
    async fn test_tracker_release() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("test");
        let session_id = Uuid::new_v4();

        let guard = tracker
            .try_register(tunnel_id.clone(), session_id)
            .await
            .unwrap();
        assert!(tracker.is_connected(&tunnel_id).await);

        guard.release_async(&tracker).await;
        assert!(!tracker.is_connected(&tunnel_id).await);
        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_release_allows_new_registration() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("test");

        // Register and release
        let guard1 = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await
            .unwrap();
        guard1.release_async(&tracker).await;

        // Should be able to register again
        let result = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await;
        assert!(result.is_ok());

        // Clean up
        let mut guard = result.unwrap();
        guard.mark_released();
    }

    #[tokio::test]
    async fn test_tracker_release_spawn() {
        let tracker = ConnectionTracker::shared();
        let tunnel_id = TunnelId::from_record_uid("test");

        let guard = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await
            .unwrap();
        assert!(tracker.is_connected(&tunnel_id).await);

        guard.release_spawn(tracker.clone());

        // Give the spawned task time to complete
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(!tracker.is_connected(&tunnel_id).await);
    }

    #[test]
    fn test_connection_guard_tunnel_id() {
        let guard = ConnectionGuard {
            tunnel_id: TunnelId::from_record_uid("test"),
            session_id: Uuid::new_v4(),
            released: false,
            target_id: None,
            session_uid: None,
        };

        assert_eq!(guard.tunnel_id().0, "record:test");
        assert!(!guard.is_released());

        // Mark released to avoid drop warning
        let mut guard = guard;
        guard.mark_released();
        assert!(guard.is_released());
    }

    #[tokio::test]
    async fn test_tunnel_state() {
        let session_id = Uuid::new_v4();
        let state = TunnelState::new(session_id);

        assert_eq!(state.connection_count(), 1);
        assert!(state.active_sessions.contains(&session_id));
        // Duration should be very small
        assert!(state.first_connection_time.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_grace_period_within() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("grace-test");
        let grace = Duration::from_secs(5);

        // First connection
        let guard1 = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(5), Some(grace))
            .await
            .unwrap();

        // Second connection within grace period should succeed
        let guard2 = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(5), Some(grace))
            .await
            .unwrap();

        assert_eq!(tracker.tunnel_connection_count(&tunnel_id).await, 2);

        // Clean up
        guard1.release_async(&tracker).await;
        guard2.release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_grace_period_expired_allows_under_max() {
        // Grace period is now informational only - connections are allowed
        // as long as they're under max_connections, even after grace expires
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("grace-expired-test");
        let grace = Duration::from_millis(10); // Very short grace period

        // First connection
        let guard1 = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(5), Some(grace))
            .await
            .unwrap();

        // Wait for grace period to expire
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Second connection should SUCCEED - under max_connections (5)
        let result = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(5), Some(grace))
            .await;

        assert!(result.is_ok());
        assert_eq!(tracker.tunnel_connection_count(&tunnel_id).await, 2);

        // Clean up
        guard1.release_async(&tracker).await;
        result.unwrap().release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_max_connections_limit() {
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("max-conn-test");
        let grace = Duration::from_secs(60); // Long grace period

        // Register max connections (2)
        let guard1 = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(2), Some(grace))
            .await
            .unwrap();
        let guard2 = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(2), Some(grace))
            .await
            .unwrap();

        // Third connection should fail - max reached
        let result = tracker
            .try_register_with_limits(tunnel_id.clone(), Uuid::new_v4(), Some(2), Some(grace))
            .await;

        assert!(result.is_err());
        assert_eq!(tracker.tunnel_connection_count(&tunnel_id).await, 2);

        // Clean up
        guard1.release_async(&tracker).await;
        guard2.release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_concurrent_registration() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::task::JoinSet;

        let tracker = ConnectionTracker::shared();
        let tunnel_id = TunnelId::from_record_uid("contested");
        let success_count = Arc::new(AtomicUsize::new(0));

        let mut tasks = JoinSet::new();

        // Spawn 10 concurrent registration attempts
        for _ in 0..10 {
            let t = tracker.clone();
            let id = tunnel_id.clone();
            let count = success_count.clone();

            tasks.spawn(async move {
                if let Ok(guard) = t.try_register(id, Uuid::new_v4()).await {
                    count.fetch_add(1, Ordering::Relaxed);
                    // Hold the connection briefly
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    guard.release_async(&t).await;
                }
            });
        }

        // Wait for all tasks
        while tasks.join_next().await.is_some() {}

        // Only one should have succeeded at a time, but total could be more
        // if they were sequential enough
        assert!(success_count.load(Ordering::Relaxed) >= 1);
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_tunnel_id_empty_string() {
        // Edge case: empty strings should still work as valid IDs
        let id1 = TunnelId::from_record_uid("");
        let id2 = TunnelId::from_record_uid("");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_tunnel_id_special_characters() {
        // Edge case: special characters in IDs
        let id = TunnelId::from_record_uid("user@domain.com/resource#fragment?query=value");
        assert!(id.0.contains("user@domain.com"));
    }

    #[tokio::test]
    async fn test_tracker_many_tunnels() {
        // Test with many different tunnel IDs
        let tracker = ConnectionTracker::new();
        let mut guards = Vec::new();

        for i in 0..100 {
            let id = TunnelId::from_record_uid(&format!("tunnel-{}", i));
            let guard = tracker.try_register(id, Uuid::new_v4()).await.unwrap();
            guards.push(guard);
        }

        assert_eq!(tracker.active_count().await, 100);

        // Release all
        for guard in guards {
            guard.release_async(&tracker).await;
        }

        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_release_nonexistent() {
        // Releasing a non-existent tunnel should be a no-op
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("never-registered");

        // Should not panic
        tracker.release(&tunnel_id).await;
        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_double_release() {
        // Double release should be a no-op (idempotent)
        let tracker = ConnectionTracker::new();
        let tunnel_id = TunnelId::from_record_uid("double-release");

        let guard = tracker
            .try_register(tunnel_id.clone(), Uuid::new_v4())
            .await
            .unwrap();
        guard.release_async(&tracker).await;

        // Second release should not panic
        tracker.release(&tunnel_id).await;
        assert_eq!(tracker.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_tracker_rapid_register_release() {
        // Stress test: rapid registration and release
        let tracker = ConnectionTracker::shared();
        let tunnel_id = TunnelId::from_record_uid("rapid-test");

        for _ in 0..50 {
            let guard = tracker
                .try_register(tunnel_id.clone(), Uuid::new_v4())
                .await
                .unwrap();
            guard.release_async(&tracker).await;
        }

        assert_eq!(tracker.active_count().await, 0);
    }

    #[test]
    fn test_connection_guard_is_released_flag() {
        let mut guard = ConnectionGuard {
            tunnel_id: TunnelId::from_record_uid("flag-test"),
            session_id: Uuid::new_v4(),
            released: false,
            target_id: None,
            session_uid: None,
        };

        assert!(!guard.is_released());
        guard.mark_released();
        assert!(guard.is_released());

        // Can mark multiple times (idempotent)
        guard.mark_released();
        assert!(guard.is_released());
    }

    // ========================================================================
    // Stale Session Cleanup Tests
    // ========================================================================

    #[tokio::test]
    async fn test_cleanup_stale_sessions_removes_old_entries() {
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("test-host", 5432);
        let session_uid = "test-session".to_string();

        // Manually insert a stale entry
        {
            let mut sessions = tracker.active_sessions.write().await;
            sessions.insert(
                target_id.clone(),
                TargetSession {
                    session_uid: session_uid.clone(),
                    application_name: Some("TestApp".to_string()),
                    last_disconnect: Some(Instant::now() - Duration::from_secs(600)), // 10 minutes ago
                },
            );
        }

        assert_eq!(tracker.target_session_count().await, 1);

        // Cleanup with 5 minute threshold should remove it
        let removed = tracker
            .cleanup_stale_sessions(Some(Duration::from_secs(300)))
            .await;
        assert_eq!(removed, 1);
        assert_eq!(tracker.target_session_count().await, 0);
    }

    #[tokio::test]
    async fn test_cleanup_stale_sessions_keeps_active_entries() {
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("test-host", 5432);
        let session_uid = "test-session".to_string();

        // Insert an active entry (no last_disconnect)
        {
            let mut sessions = tracker.active_sessions.write().await;
            sessions.insert(
                target_id.clone(),
                TargetSession {
                    session_uid: session_uid.clone(),
                    application_name: Some("TestApp".to_string()),
                    last_disconnect: None, // Still connected
                },
            );
        }

        assert_eq!(tracker.target_session_count().await, 1);

        // Cleanup should not remove active entries
        let removed = tracker
            .cleanup_stale_sessions(Some(Duration::from_secs(0)))
            .await;
        assert_eq!(removed, 0);
        assert_eq!(tracker.target_session_count().await, 1);
    }

    #[tokio::test]
    async fn test_cleanup_stale_sessions_keeps_recent_disconnects() {
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("test-host", 5432);
        let session_uid = "test-session".to_string();

        // Insert a recently disconnected entry
        {
            let mut sessions = tracker.active_sessions.write().await;
            sessions.insert(
                target_id.clone(),
                TargetSession {
                    session_uid: session_uid.clone(),
                    application_name: Some("TestApp".to_string()),
                    last_disconnect: Some(Instant::now()), // Just disconnected
                },
            );
        }

        assert_eq!(tracker.target_session_count().await, 1);

        // Cleanup with 5 minute threshold should keep it
        let removed = tracker
            .cleanup_stale_sessions(Some(Duration::from_secs(300)))
            .await;
        assert_eq!(removed, 0);
        assert_eq!(tracker.target_session_count().await, 1);
    }

    #[tokio::test]
    async fn test_target_session_count() {
        let tracker = ConnectionTracker::new();

        // Initially empty
        assert_eq!(tracker.target_session_count().await, 0);

        // Add some entries
        {
            let mut sessions = tracker.active_sessions.write().await;
            sessions.insert(
                TargetId::new("host1", 5432),
                TargetSession {
                    session_uid: "session1".to_string(),
                    application_name: None,
                    last_disconnect: None,
                },
            );
            sessions.insert(
                TargetId::new("host2", 3306),
                TargetSession {
                    session_uid: "session2".to_string(),
                    application_name: None,
                    last_disconnect: Some(Instant::now()),
                },
            );
        }

        assert_eq!(tracker.target_session_count().await, 2);
    }

    // ========================================================================
    // Issue #49: Session Takeover Tests
    // ========================================================================

    #[tokio::test]
    async fn test_session_takeover_after_disconnect() {
        // Issue #49: New session should be able to take over after previous session disconnects
        // In Gateway mode, tunnel_id is created from session_uid
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("db.example.com", 3306);
        let session_uid_a = "session-A".to_string();
        let session_uid_b = "session-B".to_string();

        // Register session A (tunnel_id = session_uid in Gateway mode)
        let guard_a = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_a.clone(),
                TunnelId::from_record_uid(&session_uid_a),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("mysql".to_string()),
                None,
            )
            .await
            .unwrap();

        // Session B should be rejected while A is active (A's tunnel still has connections)
        let result_b = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("mysql".to_string()),
                None,
            )
            .await;
        assert!(
            result_b.is_err(),
            "Session B should be rejected while A is active"
        );

        // Release session A (simulates TCP close)
        guard_a.release_async(&tracker).await;

        // Now session B should be able to take over
        let result_b = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("mysql".to_string()),
                None,
            )
            .await;
        assert!(
            result_b.is_ok(),
            "Session B should succeed after A disconnects"
        );

        // Clean up
        result_b.unwrap().release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_session_rejection_while_active() {
        // Different session should be rejected while current session has active connections
        // In Gateway mode, tunnel_id is created from session_uid
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("db.example.com", 5432);
        let session_uid_a = "active-session".to_string();
        let session_uid_b = "intruder-session".to_string();

        // Register session A - keep connection active (tunnel_id = session_uid)
        let _guard_a = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_a.clone(),
                TunnelId::from_record_uid(&session_uid_a),
                Uuid::new_v4(),
                Some(2),
                Some(Duration::from_secs(60)),
                Some("psql".to_string()),
                None,
            )
            .await
            .unwrap();

        // Session B should be rejected (A still has active connection in its tunnel)
        let result_b = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("psql".to_string()),
                None,
            )
            .await;

        assert!(result_b.is_err());
        if let Err(e) = result_b {
            assert!(e.to_string().contains("active"));
        }

        // Clean up - mark released to avoid drop warning
        // Note: _guard_a will be dropped, which is fine
    }

    #[tokio::test]
    async fn test_same_session_reconnect_preserves_app_lock() {
        // Same session reconnecting should still respect application lock timeout
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("db.example.com", 3306);
        let tunnel_id = TunnelId::from_record_uid("app-lock-tunnel");
        let session_uid = "same-session".to_string();

        // Register with app "mysql"
        let guard = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid.clone(),
                tunnel_id.clone(),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("mysql".to_string()),
                Some(Duration::from_secs(5)), // 5 second app lock
            )
            .await
            .unwrap();

        // Release connection
        guard.release_async(&tracker).await;

        // Immediately try to reconnect with different app - should fail
        let result = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid.clone(),
                TunnelId::from_record_uid("same-session-new-tunnel"),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("workbench".to_string()), // Different app
                Some(Duration::from_secs(5)),
            )
            .await;

        // Should be rejected due to app mismatch within lock timeout
        assert!(
            result.is_err(),
            "Different app should be rejected within lock timeout"
        );

        // Same app should work
        let result = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid.clone(),
                TunnelId::from_record_uid("same-session-same-app"),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("mysql".to_string()), // Same app
                Some(Duration::from_secs(5)),
            )
            .await;

        assert!(result.is_ok(), "Same app should succeed");
        result.unwrap().release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_session_takeover_allows_different_app() {
        // Issue #49: New session taking over should be able to use a different application
        // since the application lock only applies within the same session
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("db.example.com", 5432);
        let session_uid_a = "session-A".to_string();
        let session_uid_b = "session-B".to_string();

        // Register session A with "pgAdmin"
        let guard_a = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_a.clone(),
                TunnelId::from_record_uid("tunnel-a"),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("pgAdmin 4".to_string()),
                None,
            )
            .await
            .unwrap();

        // Release session A
        guard_a.release_async(&tracker).await;

        // Session B should be able to take over with a DIFFERENT app name
        // The application lock is per-session, so a new session gets a fresh slate
        let result_b = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid("tunnel-b"),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("DBeaver".to_string()), // Different app from session A
                None,
            )
            .await;

        assert!(
            result_b.is_ok(),
            "New session should use different app after takeover"
        );

        // Verify the target now has session B's app name
        {
            let sessions = tracker.active_sessions.read().await;
            let target_session = sessions.get(&target_id).unwrap();
            assert_eq!(target_session.session_uid, session_uid_b);
            assert_eq!(target_session.application_name, Some("DBeaver".to_string()));
        }

        result_b.unwrap().release_async(&tracker).await;
    }

    #[tokio::test]
    async fn test_session_takeover_when_tunnel_gone() {
        // Issue #49: When WebRTC tunnel dies abruptly, the tunnel entry is removed
        // but active_sessions entry remains (last_disconnect not set).
        // A new session should be able to take over in this case.
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("db.example.com", 5432);
        let session_uid_a = "session-A".to_string();
        let session_uid_b = "session-B".to_string();

        // Register session A
        let mut guard_a = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_a.clone(),
                TunnelId::from_record_uid(&session_uid_a),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("pgAdmin".to_string()),
                None,
            )
            .await
            .unwrap();

        // Simulate WebRTC tunnel dying abruptly:
        // - Remove the tunnel entry directly (simulating it was cleaned up by WebRTC layer)
        // - But keep the active_sessions entry (last_disconnect NOT set)
        // - This simulates the race condition where tunnel is gone but proxy hasn't cleaned up
        guard_a.mark_released(); // Prevent drop from calling release
        {
            let mut tunnels = tracker.tunnels.write().await;
            tunnels.remove(&TunnelId::from_record_uid(&session_uid_a));
        }

        // Verify active_sessions still has Session A with last_disconnect = None
        {
            let sessions = tracker.active_sessions.read().await;
            let target_session = sessions.get(&target_id).unwrap();
            assert_eq!(target_session.session_uid, session_uid_a);
            assert!(
                target_session.last_disconnect.is_none(),
                "last_disconnect should be None (simulating abrupt tunnel death)"
            );
        }

        // Session B should be able to take over because the tunnel is gone
        let result_b = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(1),
                Some(Duration::from_secs(60)),
                Some("DBeaver".to_string()),
                None,
            )
            .await;

        assert!(
            result_b.is_ok(),
            "Session B should succeed when Session A's tunnel is gone (even without last_disconnect)"
        );

        // Verify the target now has session B
        {
            let sessions = tracker.active_sessions.read().await;
            let target_session = sessions.get(&target_id).unwrap();
            assert_eq!(target_session.session_uid, session_uid_b);
        }

        result_b.unwrap().release_async(&tracker).await;
    }

    /// Tests that force_release_tunnel marks target sessions as disconnected,
    /// allowing immediate takeover by new sessions.
    #[tokio::test]
    async fn test_force_release_tunnel_enables_takeover() {
        let tracker = ConnectionTracker::new();
        let target_id = TargetId::new("127.0.0.1", 5432);

        // Session A - the one whose tunnel will die
        let session_uid_a = "session_a_uid".to_string();
        let tunnel_id_a = TunnelId::from_record_uid(&session_uid_a);

        // Session B - the one trying to take over
        let session_uid_b = "session_b_uid".to_string();

        // Register Session A with an active connection
        let mut guard_a = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_a.clone(),
                tunnel_id_a.clone(),
                Uuid::new_v4(),
                Some(2),
                Some(Duration::from_secs(60)),
                Some("pgAdmin 4".to_string()),
                None,
            )
            .await
            .expect("Session A registration should succeed");

        // Session B should be REJECTED because Session A is active
        let result_b1 = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(2),
                Some(Duration::from_secs(60)),
                Some("DBeaver".to_string()),
                None,
            )
            .await;

        assert!(
            result_b1.is_err(),
            "Session B should be rejected while Session A is active"
        );

        // Simulate WebRTC tunnel death by calling force_release_tunnel
        // This is what the Gateway should call when it detects the tunnel is dead
        tracker.force_release_tunnel(&session_uid_a).await;

        // Verify Session A's target session is now marked as disconnected
        {
            let sessions = tracker.active_sessions.read().await;
            let target_session = sessions.get(&target_id).unwrap();
            assert!(
                target_session.last_disconnect.is_some(),
                "last_disconnect should be set after force_release_tunnel"
            );
        }

        // Session B should now succeed because Session A's tunnel was force-released
        let result_b2 = tracker
            .try_register_for_target_with_app(
                target_id.clone(),
                session_uid_b.clone(),
                TunnelId::from_record_uid(&session_uid_b),
                Uuid::new_v4(),
                Some(2),
                Some(Duration::from_secs(60)),
                Some("DBeaver".to_string()),
                None,
            )
            .await;

        assert!(
            result_b2.is_ok(),
            "Session B should succeed after force_release_tunnel"
        );

        // Verify the target now has Session B
        {
            let sessions = tracker.active_sessions.read().await;
            let target_session = sessions.get(&target_id).unwrap();
            assert_eq!(target_session.session_uid, session_uid_b);
            assert_eq!(target_session.application_name, Some("DBeaver".to_string()));
        }

        // Clean up - drop guard_a without releasing since tunnel was force-released
        // (in reality the ConnectionGuard would have been abandoned)
        guard_a.mark_released();
        result_b2.unwrap().release_async(&tracker).await;
    }
}
