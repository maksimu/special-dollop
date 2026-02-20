//! Runtime configuration via KEEPER_GATEWAY_* environment variables
//!
//! All timing and scaling values can be overridden for deployment-specific tuning.
//! Defaults are optimized for typical Gateway deployments.
//!
//! ## Design Philosophy
//! - All timeouts are configurable to support diverse network conditions
//! - Defaults balance responsiveness (fast networks) with reliability (slow networks)
//! - Magic numbers are eliminated - all values documented here
//!
//! ## Usage
//! ```rust,ignore
//! // Internal module - use via crate public API
//! let timeout = backend_flush_timeout();
//! assert!(timeout.as_millis() > 0);
//! ```

use std::time::Duration;

// ============================================================================
// PROTOCOL CONSTANTS (Hardcoded - WebRTC Spec / Internal Timing)
// ============================================================================

/// ICE disconnected timeout (WebRTC SettingEngine).
///
/// **Value**: 30 seconds
///
/// **Rationale**: Extended from WebRTC default (7s) to accommodate slow candidate
/// trickling in trickle ICE mode. WebRTC spec-related, not network-dependent.
pub const ICE_DISCONNECTED_TIMEOUT_SECS: u64 = 30;

/// ICE failed timeout (WebRTC SettingEngine).
///
/// **Value**: 60 seconds
///
/// **Rationale**: Extended from WebRTC default (25s) to allow full ICE restart cycle.
/// WebRTC spec-related, not network-dependent.
pub const ICE_FAILED_TIMEOUT_SECS: u64 = 60;

/// ICE keepalive interval (WebRTC SettingEngine).
///
/// **Value**: 200 milliseconds
///
/// **Rationale**: WebRTC connectivity checks. 200ms balances responsiveness with overhead.
/// WebRTC protocol timing, not configurable.
pub const ICE_KEEPALIVE_INTERVAL_MS: u64 = 200;

/// Stats collection interval for quality monitoring.
///
/// **Value**: 5 seconds
///
/// **Rationale**: Internal polling frequency. Too frequent = CPU overhead, too slow = stale data.
/// Internal timing, not network-dependent.
pub const STATS_COLLECTION_INTERVAL_SECS: u64 = 5;
/// Internal protocol message delivery delay.
///
/// **Value**: 100 milliseconds
///
/// **Rationale**: Small delay to allow protocol messages to reach buffers before
/// connection close. Internal timing, not network-dependent.
pub const PROTOCOL_MESSAGE_DELAY_MS: u64 = 100;

// ============================================================================
// BACKEND I/O CONFIGURATION (Network-Dependent)
// ============================================================================

/// Maximum time to wait for backend flush() to complete.
///
/// **Default**: 50ms
///
/// **Rationale**: Interactive typing should feel instant (<100ms human perception).
/// 50ms allows for some network delay while still feeling responsive.
/// On slow networks, timeout is acceptable lag rather than connection death.
///
/// **Tuning**:
/// - Fast/reliable networks: 20-30ms for quicker failure detection
/// - Slow/mobile networks: 100-200ms for more tolerance
///
/// **Env**: `KEEPER_GATEWAY_BACKEND_FLUSH_TIMEOUT_MS`
pub fn backend_flush_timeout() -> Duration {
    Duration::from_millis(
        std::env::var("KEEPER_GATEWAY_BACKEND_FLUSH_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50),
    )
}

/// Number of consecutive flush failures before closing connection.
///
/// **Default**: 5
///
/// **Rationale**: Transient network issues are common (WiFi handoff, mobile tower switch).
/// 5 failures (~250ms total @ 50ms each) indicates persistent problem, not transient.
/// Timeouts don't count as failures - only actual errors.
///
/// **Tuning**:
/// - Aggressive: 3 (faster detection of dead connections)
/// - Conservative: 10 (more tolerance for flaky networks)
///
/// **Env**: `KEEPER_GATEWAY_MAX_FLUSH_FAILURES`
pub fn max_flush_failures() -> usize {
    std::env::var("KEEPER_GATEWAY_MAX_FLUSH_FAILURES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

/// Grace period before signaling channels to exit during tube close.
///
/// **Default**: 100ms
///
/// **Rationale**: Allows in-flight operations (DNS, TCP handshake, TLS) to complete.
/// 100ms is enough for most operations without adding noticeable delay to shutdown.
/// Prevents race condition where channels are signaled before fully initialized.
///
/// **Tuning**:
/// - Fast shutdown: 50ms (minimal delay)
/// - Slow networks: 200-500ms (more completion time)
///
/// **Env**: `KEEPER_GATEWAY_CHANNEL_SHUTDOWN_GRACE_MS`
pub fn channel_shutdown_grace_period() -> Duration {
    Duration::from_millis(
        std::env::var("KEEPER_GATEWAY_CHANNEL_SHUTDOWN_GRACE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100),
    )
}

// ============================================================================
// CHANNEL CLEANUP TIMEOUTS
// ============================================================================

/// Timeout for data channel close operation.
///
/// **Default**: 3 seconds
///
/// **Rationale**: WebRTC spec suggests 3-5s for graceful close with SCTP retransmission.
/// Allows pending messages to be delivered before forceful close.
///
/// **Tuning**: Usually don't change - WebRTC protocol timing
///
/// **Env**: `KEEPER_GATEWAY_DATA_CHANNEL_CLOSE_TIMEOUT_SECS`
pub fn data_channel_close_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_DATA_CHANNEL_CLOSE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3),
    )
}

/// Timeout for peer connection close operation.
///
/// **Default**: 5 seconds
///
/// **Rationale**: Needs time for DTLS close_notify + TURN deallocation (typically 1-2s).
/// 5s provides buffer for slow TURN servers or network delays.
///
/// **Tuning**: Usually don't change - WebRTC protocol timing
///
/// **Env**: `KEEPER_GATEWAY_PEER_CONNECTION_CLOSE_TIMEOUT_SECS`
pub fn peer_connection_close_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_PEER_CONNECTION_CLOSE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    )
}

/// Timeout for waiting for channel.run() tasks to complete during tube close.
///
/// **Default**: 2 seconds
///
/// **Rationale**: After signaling channels to shut down, we must wait for channel.run() tasks
/// to exit gracefully and release their buffers. Channel tasks check shutdown notification
/// every 500ms (READ_CANCELLATION_CHECK_INTERVAL), so 2s provides 4 check cycles.
/// This prevents memory leaks when connections are created/closed rapidly.
///
/// **Tuning**:
/// - Fast networks: 1-1.5s (faster cleanup)
/// - Slow networks or high load: 3-4s (more buffer release time)
///
/// **Env**: `KEEPER_GATEWAY_CHANNEL_TASK_COMPLETION_TIMEOUT_SECS`
pub fn channel_task_completion_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_CHANNEL_TASK_COMPLETION_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
    )
}

/// Timeout for waiting for spawned tasks (e.g., channel handler monitors) to complete during tube close.
///
/// **Default**: 2 seconds
///
/// **Rationale**: After closing channels, we wait for spawned monitor tasks to complete and signal
/// via completion channel. These tasks should complete quickly (<500ms) once channels are closed.
/// However, in production with slow networks or high load, they may take longer. The early exit
/// logic (200ms after last message) handles fast cases, while this timeout handles edge cases.
///
/// **Impact if timeout expires**:
/// - Non-critical: Tasks continue in background, no resources leaked
/// - Warning logged for monitoring/debugging
/// - Tube close continues normally
///
/// **Tuning**:
/// - Normal operation: 2s (balanced - fast enough for tests, safe for production)
/// - Fast cleanup needed: 1s (may see more warnings in production)
/// - Debugging slow tasks: 3-5s (more time for investigation)
///
/// **Env**: `KEEPER_GATEWAY_SPAWNED_TASK_COMPLETION_TIMEOUT_SECS`
pub fn spawned_task_completion_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_SPAWNED_TASK_COMPLETION_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
    )
}

/// Delay between disconnect message and EOF in Drop cleanup.
///
/// **Default**: 100ms
///
/// **Rationale**: Gives protocol-level disconnect time to send before TCP FIN.
/// 100ms allows message to reach buffers without blocking shutdown.
/// Ensures Guacd sees disconnect instruction before connection closes.
///
/// **Tuning**:
/// - Fast shutdown: 50ms
/// - Ensure protocol delivery: 200ms
///
/// **Env**: `KEEPER_GATEWAY_DISCONNECT_TO_EOF_DELAY_MS`
pub fn disconnect_to_eof_delay() -> Duration {
    Duration::from_millis(
        std::env::var("KEEPER_GATEWAY_DISCONNECT_TO_EOF_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100),
    )
}

// ============================================================================
// ICE / CONNECTION ESTABLISHMENT
// ============================================================================

/// ICE gathering timeout for initial connection.
///
/// **Default**: 30 seconds
///
/// **Rationale**: Mobile/slow networks need more time. 30s covers worst-case STUN retries
/// and multiple network interfaces. Trickle ICE means we can start connecting before
/// gathering completes.
///
/// **Tuning**:
/// - Fast networks: 10-15s
/// - Mobile/slow: 45-60s
///
/// **Env**: `KEEPER_GATEWAY_ICE_GATHER_TIMEOUT_SECS`
pub fn ice_gather_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ICE_GATHER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    )
}

/// Timeout waiting for ICE restart answer from remote peer.
///
/// **Default**: 20 seconds
///
/// **Rationale**: ICE restart involves only WebRTC signaling operations (no user creation).
/// Client waits 15s for restart answers (PrivateTunnel.ts:1106). Gateway must wait slightly
/// longer to avoid racing the client. Industry standard for ICE restart: 10-20 seconds.
/// 20s = 15s (client timeout) + 5s (processing/network buffer).
///
/// **Note**: Ephemeral user creation only happens during initial connection, not ICE restart.
///
/// **Tuning**:
/// - Fast signaling: 15s
/// - Slow/satellite: 30s
///
/// **Env**: `KEEPER_GATEWAY_ICE_RESTART_ANSWER_TIMEOUT_SECS`
pub fn ice_restart_answer_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ICE_RESTART_ANSWER_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20),
    )
}

/// Wait time after ICE disconnected before triggering restart.
///
/// **Default**: 2 seconds
///
/// **Rationale**: WebRTC connections rarely self-recover after disconnect.
/// 2s is enough to filter transient blips while being responsive.
///
/// **Tuning**:
/// - Aggressive recovery: 1s
/// - Conservative: 5s
///
/// **Env**: `KEEPER_GATEWAY_ICE_DISCONNECTED_WAIT_SECS`
pub fn ice_disconnected_wait() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ICE_DISCONNECTED_WAIT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
    )
}

// ============================================================================
// ACTIVITY MONITORING
// ============================================================================

/// Inactivity duration before considering ICE restart.
///
/// **Default**: 120 seconds (2 minutes)
///
/// **Rationale**: 2 minutes without data suggests network path changed or failed.
/// Long enough to avoid false positives during legitimate pauses in activity.
///
/// **Tuning**: Usually don't change - balances responsiveness with stability
///
/// **Env**: `KEEPER_GATEWAY_ACTIVITY_TIMEOUT_SECS`
pub fn activity_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ACTIVITY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    )
}

/// How often stale tube sweeper runs.
///
/// **Default**: 300 seconds (5 minutes)
///
/// **Rationale**: 5 minutes balances resource cleanup vs sweep overhead.
/// Stale tube sweeper is a safety net for edge cases, not primary cleanup.
/// Finds tubes in Failed/Disconnected state with prolonged inactivity.
///
/// **Tuning**:
/// - High churn: 60-120s (more frequent)
/// - Stable: 600s (less overhead)
///
/// **Env**: `KEEPER_GATEWAY_STALE_TUBE_SWEEP_INTERVAL_SECS`
pub fn stale_tube_sweep_interval() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_STALE_TUBE_SWEEP_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
    )
}

// ============================================================================
// CONCURRENCY / SCALE
// ============================================================================

/// Maximum concurrent tube creations allowed.
///
/// **Default**: 100
///
/// **Rationale**: Backpressure prevents resource exhaustion during creation storms.
/// 100 concurrent creates supports high throughput while maintaining system stability.
///
/// **Tuning**:
/// - High-spec servers: 200-500
/// - Resource-constrained: 50
///
/// **Env**: `KEEPER_GATEWAY_MAX_CONCURRENT_CREATES`
pub fn max_concurrent_creates() -> usize {
    std::env::var("KEEPER_GATEWAY_MAX_CONCURRENT_CREATES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
}

// ============================================================================
// ROUTER / HTTP TIMEOUTS
// ============================================================================

/// HTTP timeout for router API calls (TURN credentials, relay access, etc).
///
/// **Default**: 5 seconds (reduced from 30s)
///
/// **Rationale**: Fast failure detection prevents actor from freezing. Router should respond
/// quickly or fail fast. 30s timeout caused entire registry to appear down during router issues.
///
/// **Tuning**:
/// - Fast/reliable router: 3s
/// - Slow/remote router: 10s
///
/// **Env**: `KEEPER_GATEWAY_ROUTER_HTTP_TIMEOUT_SECS`
pub fn router_http_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ROUTER_HTTP_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    )
}

/// Total timeout for tube creation (prevents actor freeze on slow I/O).
///
/// **Default**: 15 seconds
///
/// **Rationale**: Prevents single-threaded actor from blocking entire registry when
/// router is slow or connection establishment hangs. Must be >= router_http_timeout.
///
/// **Tuning**:
/// - Fast environments: 10s
/// - Slow/complex: 30s
///
/// **Env**: `KEEPER_GATEWAY_TUBE_CREATION_TIMEOUT_SECS`
pub fn tube_creation_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_TUBE_CREATION_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15),
    )
}

/// Circuit breaker cooldown after router failures.
///
/// **Default**: 60 seconds
///
/// **Rationale**: Stop hammering dead router. After threshold failures, fast-fail
/// all requests for cooldown period. Allows router to recover without load.
///
/// **Tuning**:
/// - Aggressive retry: 30s
/// - Conservative: 120s (2 minutes)
///
/// **Env**: `KEEPER_GATEWAY_ROUTER_CIRCUIT_BREAKER_COOLDOWN_SECS`
pub fn router_circuit_breaker_cooldown() -> Duration {
    Duration::from_secs(
        std::env::var("KEEPER_GATEWAY_ROUTER_CIRCUIT_BREAKER_COOLDOWN_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60),
    )
}

/// Number of consecutive router failures before opening circuit breaker.
///
/// **Default**: 3
///
/// **Rationale**: 3 consecutive failures indicates router is down, not transient issue.
/// Opens circuit breaker to prevent cascading failures and actor blocking.
///
/// **Tuning**:
/// - Aggressive: 2 (faster circuit breaker)
/// - Tolerant: 5 (more retries)
///
/// **Env**: `KEEPER_GATEWAY_ROUTER_CIRCUIT_BREAKER_THRESHOLD`
pub fn router_circuit_breaker_threshold() -> usize {
    std::env::var("KEEPER_GATEWAY_ROUTER_CIRCUIT_BREAKER_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

// ============================================================================
// LOGGING
// ============================================================================

/// Include WebRTC library logs (very verbose).
///
/// **Default**: false
///
/// **Rationale**: WebRTC library is extremely verbose. Only enable for deep protocol debugging.
///
/// **Env**: `KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS` (set to "1" or "true")
pub fn include_webrtc_logs() -> bool {
    std::env::var("KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}
