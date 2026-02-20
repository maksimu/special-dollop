use std::time::Duration;
use thiserror::Error;

/// Comprehensive WebRTC error types for enhanced error classification and recovery
#[derive(Error, Debug, Clone)]
pub enum WebRTCError {
    // Connection-level errors
    #[error("ICE connection failed: {reason} (tube_id: {tube_id})")]
    IceConnectionFailed { tube_id: String, reason: String },

    #[error("Peer connection creation failed: {reason} (tube_id: {tube_id})")]
    PeerConnectionCreationFailed { tube_id: String, reason: String },

    #[error("Connection closed unexpectedly: {reason} (tube_id: {tube_id})")]
    ConnectionClosed { tube_id: String, reason: String },

    #[error("Connection timeout after {timeout:?} (tube_id: {tube_id})")]
    ConnectionTimeout { tube_id: String, timeout: Duration },

    // TURN/STUN server errors
    #[error("TURN server failure: {server} - {reason} (tube_id: {tube_id})")]
    TurnServerFailure {
        tube_id: String,
        server: String,
        reason: String,
    },

    #[error("STUN server failure: {server} - {reason} (tube_id: {tube_id})")]
    StunServerFailure {
        tube_id: String,
        server: String,
        reason: String,
    },

    #[error("TURN authentication failed: {server} (tube_id: {tube_id})")]
    TurnAuthenticationFailed { tube_id: String, server: String },

    #[error("No viable TURN/STUN servers available (tube_id: {tube_id})")]
    NoViableServers { tube_id: String },

    // SDP-related errors
    #[error("SDP offer creation failed: {reason} (tube_id: {tube_id})")]
    SdpOfferCreationFailed { tube_id: String, reason: String },

    #[error("SDP answer creation failed: {reason} (tube_id: {tube_id})")]
    SdpAnswerCreationFailed { tube_id: String, reason: String },

    #[error("Local description setting failed: {reason} (tube_id: {tube_id})")]
    LocalDescriptionFailed { tube_id: String, reason: String },

    #[error("Remote description setting failed: {reason} (tube_id: {tube_id})")]
    RemoteDescriptionFailed { tube_id: String, reason: String },

    #[error("Invalid SDP format: {reason} (tube_id: {tube_id})")]
    InvalidSdpFormat { tube_id: String, reason: String },

    // ICE-specific errors
    #[error("ICE candidate addition failed: {reason} (tube_id: {tube_id})")]
    IceCandidateAdditionFailed { tube_id: String, reason: String },

    #[error("ICE gathering failed: {reason} (tube_id: {tube_id})")]
    IceGatheringFailed { tube_id: String, reason: String },

    #[error("ICE gathering timeout after {timeout:?} (tube_id: {tube_id})")]
    IceGatheringTimeout { tube_id: String, timeout: Duration },

    #[error("ICE restart failed: {reason} (tube_id: {tube_id}, attempts: {attempts})")]
    IceRestartFailed {
        tube_id: String,
        reason: String,
        attempts: u32,
    },

    #[error("ICE candidate invalid: {candidate} (tube_id: {tube_id})")]
    InvalidIceCandidate { tube_id: String, candidate: String },

    // State management errors
    #[error("Invalid signaling state transition from {from:?} to {to:?} (tube_id: {tube_id})")]
    InvalidStateTransition {
        tube_id: String,
        from: String,
        to: String,
    },

    #[error("Operation not allowed in current state {current_state:?} (tube_id: {tube_id})")]
    InvalidOperationForState {
        tube_id: String,
        current_state: String,
    },

    #[error("Connection is closing or closed (tube_id: {tube_id})")]
    ConnectionClosing { tube_id: String },

    // Network-related errors
    #[error("Network change detected, connection unstable (tube_id: {tube_id}, change_type: {change_type})")]
    NetworkChangeDetected {
        tube_id: String,
        change_type: String,
    },

    #[error("Network interface unavailable: {interface} (tube_id: {tube_id})")]
    NetworkInterfaceUnavailable { tube_id: String, interface: String },

    #[error("Bandwidth insufficient: required {required_bps}bps, available {available_bps}bps (tube_id: {tube_id})")]
    InsufficientBandwidth {
        tube_id: String,
        required_bps: u64,
        available_bps: u64,
    },

    // Quality/Performance errors
    #[error("Connection quality degraded: RTT {rtt_ms}ms, packet_loss {packet_loss}% (tube_id: {tube_id})")]
    QualityDegraded {
        tube_id: String,
        rtt_ms: u64,
        packet_loss: f32,
    },

    #[error("Data channel error: {reason} (tube_id: {tube_id})")]
    DataChannelError { tube_id: String, reason: String },

    // Circuit breaker and isolation errors
    #[error(
        "Circuit breaker open: {breaker_type} (tube_id: {tube_id}, failure_count: {failure_count})"
    )]
    CircuitBreakerOpen {
        tube_id: String,
        breaker_type: String,
        failure_count: u32,
    },

    #[error("Operation failed due to isolation: {reason} (tube_id: {tube_id})")]
    IsolationFailure { tube_id: String, reason: String },

    // Resource exhaustion (WebRTC-specific)
    #[error("WebRTC resource exhausted: {resource} (tube_id: {tube_id})")]
    ResourceExhausted { tube_id: String, resource: String },

    #[error("Too many concurrent connections: {current}/{max} (tube_id: {tube_id})")]
    TooManyConnections {
        tube_id: String,
        current: u32,
        max: u32,
    },

    // Generic fallback for unknown errors
    #[error("Unknown WebRTC error: {reason} (tube_id: {tube_id})")]
    Unknown { tube_id: String, reason: String },
}

impl WebRTCError {
    /// Determine if this error is retryable
    pub fn is_retryable(&self) -> bool {
        match self {
            // Temporary network issues - retryable
            WebRTCError::IceConnectionFailed { .. } => true,
            WebRTCError::ConnectionTimeout { .. } => true,
            WebRTCError::TurnServerFailure { .. } => true,
            WebRTCError::StunServerFailure { .. } => true,
            WebRTCError::IceGatheringTimeout { .. } => true,
            WebRTCError::NetworkChangeDetected { .. } => true,
            WebRTCError::NetworkInterfaceUnavailable { .. } => true,
            WebRTCError::InsufficientBandwidth { .. } => true,
            WebRTCError::QualityDegraded { .. } => true,
            WebRTCError::ResourceExhausted { .. } => true,
            WebRTCError::TooManyConnections { .. } => true,

            // Authentication/authorization - not retryable without changes
            WebRTCError::TurnAuthenticationFailed { .. } => false,

            // State errors - not retryable in current context
            WebRTCError::InvalidStateTransition { .. } => false,
            WebRTCError::InvalidOperationForState { .. } => false,
            WebRTCError::ConnectionClosing { .. } => false,

            // Format errors - not retryable
            WebRTCError::InvalidSdpFormat { .. } => false,
            WebRTCError::InvalidIceCandidate { .. } => false,

            // Circuit breaker - depends on state
            WebRTCError::CircuitBreakerOpen { .. } => false, // Wait for circuit to close

            // Other errors - case by case
            _ => true, // Default to retryable for unknown cases
        }
    }

    /// Get the appropriate retry delay for this error type
    pub fn get_retry_delay(&self, attempt: u32) -> Duration {
        let base_delay = match self {
            // Fast retry for network issues
            WebRTCError::NetworkChangeDetected { .. } => Duration::from_millis(100),
            WebRTCError::NetworkInterfaceUnavailable { .. } => Duration::from_millis(500),

            // Medium delay for server issues
            WebRTCError::TurnServerFailure { .. } => Duration::from_secs(1),
            WebRTCError::StunServerFailure { .. } => Duration::from_secs(1),
            WebRTCError::IceGatheringTimeout { .. } => Duration::from_secs(2),

            // Longer delay for connection issues
            WebRTCError::IceConnectionFailed { .. } => Duration::from_secs(5),
            WebRTCError::ConnectionTimeout { .. } => Duration::from_secs(5),

            // Very long delay for quality/resource issues
            WebRTCError::QualityDegraded { .. } => Duration::from_secs(10),
            WebRTCError::InsufficientBandwidth { .. } => Duration::from_secs(15),
            WebRTCError::ResourceExhausted { .. } => Duration::from_secs(10),
            WebRTCError::TooManyConnections { .. } => Duration::from_secs(20),

            // Default delay
            _ => Duration::from_secs(5),
        };

        // Apply exponential backoff with jitter
        let exponential_delay = base_delay * 2_u32.pow(attempt.min(5)); // Cap at 2^5 = 32x
        let max_delay = Duration::from_secs(60); // Never wait more than 1 minute

        std::cmp::min(exponential_delay, max_delay)
    }

    /// Get error category for metrics and monitoring
    pub fn get_category(&self) -> &'static str {
        match self {
            WebRTCError::IceConnectionFailed { .. }
            | WebRTCError::PeerConnectionCreationFailed { .. }
            | WebRTCError::ConnectionClosed { .. }
            | WebRTCError::ConnectionTimeout { .. } => "connection",

            WebRTCError::TurnServerFailure { .. }
            | WebRTCError::StunServerFailure { .. }
            | WebRTCError::TurnAuthenticationFailed { .. }
            | WebRTCError::NoViableServers { .. } => "turn_stun",

            WebRTCError::SdpOfferCreationFailed { .. }
            | WebRTCError::SdpAnswerCreationFailed { .. }
            | WebRTCError::LocalDescriptionFailed { .. }
            | WebRTCError::RemoteDescriptionFailed { .. }
            | WebRTCError::InvalidSdpFormat { .. } => "sdp",

            WebRTCError::IceCandidateAdditionFailed { .. }
            | WebRTCError::IceGatheringFailed { .. }
            | WebRTCError::IceGatheringTimeout { .. }
            | WebRTCError::IceRestartFailed { .. }
            | WebRTCError::InvalidIceCandidate { .. } => "ice",

            WebRTCError::InvalidStateTransition { .. }
            | WebRTCError::InvalidOperationForState { .. }
            | WebRTCError::ConnectionClosing { .. } => "state",

            WebRTCError::NetworkChangeDetected { .. }
            | WebRTCError::NetworkInterfaceUnavailable { .. }
            | WebRTCError::InsufficientBandwidth { .. } => "network",

            WebRTCError::QualityDegraded { .. } | WebRTCError::DataChannelError { .. } => "quality",

            WebRTCError::CircuitBreakerOpen { .. } | WebRTCError::IsolationFailure { .. } => {
                "isolation"
            }

            WebRTCError::ResourceExhausted { .. } | WebRTCError::TooManyConnections { .. } => {
                "resource"
            }

            WebRTCError::Unknown { .. } => "unknown",
        }
    }

    /// Determine recovery strategy for this error
    pub fn get_recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            WebRTCError::IceConnectionFailed { .. } => RecoveryStrategy::IceRestart,
            WebRTCError::ConnectionTimeout { .. } => RecoveryStrategy::IceRestart,
            WebRTCError::TurnServerFailure { .. } => RecoveryStrategy::TryNextServer,
            WebRTCError::StunServerFailure { .. } => RecoveryStrategy::TryNextServer,
            WebRTCError::TurnAuthenticationFailed { .. } => RecoveryStrategy::RefreshCredentials,
            WebRTCError::NoViableServers { .. } => RecoveryStrategy::RefreshServerList,
            WebRTCError::IceGatheringTimeout { .. } => RecoveryStrategy::RetryWithBackoff,
            WebRTCError::NetworkChangeDetected { .. } => RecoveryStrategy::IceRestart,
            WebRTCError::QualityDegraded { .. } => RecoveryStrategy::ReduceQuality,
            WebRTCError::InsufficientBandwidth { .. } => RecoveryStrategy::ReduceQuality,
            WebRTCError::ResourceExhausted { .. } => RecoveryStrategy::WaitAndRetry,
            WebRTCError::TooManyConnections { .. } => RecoveryStrategy::WaitAndRetry,
            WebRTCError::CircuitBreakerOpen { .. } => RecoveryStrategy::WaitForCircuitClose,
            _ => RecoveryStrategy::RetryWithBackoff,
        }
    }
}

/// Recovery strategies for different error types
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryStrategy {
    /// Retry with exponential backoff
    RetryWithBackoff,
    /// Perform ICE restart
    IceRestart,
    /// Try next available server
    TryNextServer,
    /// Refresh authentication credentials
    RefreshCredentials,
    /// Refresh server list from configuration
    RefreshServerList,
    /// Reduce quality/bitrate to adapt to conditions
    ReduceQuality,
    /// Wait for resources to become available
    WaitAndRetry,
    /// Wait for circuit breaker to close
    WaitForCircuitClose,
    /// Create new connection
    CreateNewConnection,
    /// No recovery possible
    NoRecovery,
}

/// Result type alias for WebRTC operations
pub type WebRTCResult<T> = Result<T, WebRTCError>;

/// Conversion helpers from generic errors to WebRTC errors
impl From<String> for WebRTCError {
    fn from(error: String) -> Self {
        WebRTCError::Unknown {
            tube_id: "unknown".to_string(),
            reason: error,
        }
    }
}

impl WebRTCError {
    /// Convert a generic string error to a WebRTC error with context
    pub fn from_string_with_context(tube_id: String, error: String, operation: &str) -> Self {
        // Pattern match common error strings to specific error types
        let error_lower = error.to_lowercase();

        if error_lower.contains("turn") {
            if error_lower.contains("auth") || error_lower.contains("credential") {
                WebRTCError::TurnAuthenticationFailed {
                    tube_id,
                    server: "unknown".to_string(),
                }
            } else {
                WebRTCError::TurnServerFailure {
                    tube_id,
                    server: "unknown".to_string(),
                    reason: error,
                }
            }
        } else if error_lower.contains("stun") {
            WebRTCError::StunServerFailure {
                tube_id,
                server: "unknown".to_string(),
                reason: error,
            }
        } else if error_lower.contains("ice") {
            if operation.contains("restart") {
                WebRTCError::IceRestartFailed {
                    tube_id,
                    reason: error,
                    attempts: 0,
                }
            } else if operation.contains("candidate") {
                WebRTCError::IceCandidateAdditionFailed {
                    tube_id,
                    reason: error,
                }
            } else if operation.contains("gather") {
                WebRTCError::IceGatheringFailed {
                    tube_id,
                    reason: error,
                }
            } else {
                WebRTCError::IceConnectionFailed {
                    tube_id,
                    reason: error,
                }
            }
        } else if error_lower.contains("sdp") || operation.contains("description") {
            if operation.contains("offer") {
                WebRTCError::SdpOfferCreationFailed {
                    tube_id,
                    reason: error,
                }
            } else if operation.contains("answer") {
                WebRTCError::SdpAnswerCreationFailed {
                    tube_id,
                    reason: error,
                }
            } else if operation.contains("local") {
                WebRTCError::LocalDescriptionFailed {
                    tube_id,
                    reason: error,
                }
            } else if operation.contains("remote") {
                WebRTCError::RemoteDescriptionFailed {
                    tube_id,
                    reason: error,
                }
            } else {
                WebRTCError::InvalidSdpFormat {
                    tube_id,
                    reason: error,
                }
            }
        } else if error_lower.contains("timeout") {
            WebRTCError::ConnectionTimeout {
                tube_id,
                timeout: Duration::from_secs(30), // Default timeout
            }
        } else if error_lower.contains("closing") || error_lower.contains("closed") {
            WebRTCError::ConnectionClosing { tube_id }
        } else {
            WebRTCError::Unknown {
                tube_id,
                reason: error,
            }
        }
    }
}
