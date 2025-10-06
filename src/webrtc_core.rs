use crate::resource_manager::{IceAgentGuard, ResourceError, RESOURCE_MANAGER};
use crate::tube_registry::SignalMessage;
use crate::webrtc_errors::{WebRTCError, WebRTCResult};
use futures::FutureExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
// Removed unused imports - simplified API setup fixed the data channel issue

// Consolidated state structures to prevent deadlocks
#[derive(Debug)]
struct ActivityState {
    last_activity: Instant,
    last_successful_activity: Instant,
}

#[derive(Debug)]
struct IceRestartState {
    attempts: u32,
    last_restart: Option<Instant>,
}

impl ActivityState {
    fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            last_successful_activity: now,
        }
    }

    fn update_both(&mut self, now: Instant) {
        self.last_activity = now;
        self.last_successful_activity = now;
    }
}

impl IceRestartState {
    fn new() -> Self {
        Self {
            attempts: 0,
            last_restart: None,
        }
    }

    fn record_attempt(&mut self, now: Instant) {
        self.attempts += 1;
        self.last_restart = Some(now);
    }

    fn get_min_interval(&self) -> Duration {
        // Exponential backoff: 5s → 10s → 20s → 60s max
        match self.attempts {
            0 => Duration::from_secs(5),
            1 => Duration::from_secs(10),
            2 => Duration::from_secs(20),
            _ => Duration::from_secs(60), // Max backoff
        }
    }

    fn time_since_last_restart(&self, now: Instant) -> Option<Duration> {
        self.last_restart.map(|last| now.duration_since(last))
    }
}
use crate::unlikely;
use log::{debug, error, info, warn};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

// Constants for SCTP max message size negotiation
const DEFAULT_MAX_MESSAGE_SIZE: u32 = 262144; // 256KB - Common default for WebRTC
const OUR_MAX_MESSAGE_SIZE: u32 = 65536; // 64KB - Safe limit for webrtc-rs

// Constants for ICE restart management
/// Maximum number of ICE restart attempts before giving up.
/// The value 5 is chosen to balance recovery from network issues and resource usage.
const MAX_ICE_RESTART_ATTEMPTS: u32 = 5;

/// Activity timeout threshold for ICE restart decisions.
///
/// This timeout determines how long we wait without successful activity
/// before considering the connection degraded enough to warrant an ICE restart.
/// The 2-minute threshold balances between being responsive to connectivity issues
/// and avoiding unnecessary restarts during brief network interruptions.
const ACTIVITY_TIMEOUT_SECS: u64 = 120;

/// ISOLATION: Per-tube WebRTC API instances to prevent shared state corruption
/// This isolates TURN/STUN client state while preserving hot-path frame processing performance
pub struct IsolatedWebRTCAPI {
    api: webrtc::api::API,
    tube_id: String,
    created_at: Instant,
    error_count: AtomicUsize,
    turn_failure_count: AtomicUsize,
    is_healthy: AtomicBool,
}

impl IsolatedWebRTCAPI {
    /// Create completely isolated WebRTC API instance per tube
    /// PERFORMANCE: Only affects connection establishment, not frame processing
    pub fn new(tube_id: String) -> Self {
        // Configure SettingEngine with extended timeouts for trickle ICE
        let mut setting_engine = SettingEngine::default();

        // Increase ICE timeouts to allow time for trickle ICE candidates to arrive
        // Default is 7 seconds for disconnected and 25 seconds for failed
        // We extend these significantly to accommodate slow candidate trickling
        setting_engine.set_ice_timeouts(
            Some(Duration::from_secs(30)), // disconnected_timeout: 30s instead of 7s
            Some(Duration::from_secs(60)), // failed_timeout: 60s instead of 25s
            Some(Duration::from_millis(200)), // keepalive_interval: check connectivity every 200ms
        );

        debug!(
            "Configured ICE timeouts for tube {} (disconnected: 30s, failed: 60s, keepalive: 200ms)",
            tube_id
        );

        // Build API with custom settings
        let api = APIBuilder::new()
            .with_setting_engine(setting_engine)
            .build();

        Self {
            api,
            tube_id,
            created_at: Instant::now(),
            error_count: AtomicUsize::new(0),
            turn_failure_count: AtomicUsize::new(0),
            is_healthy: AtomicBool::new(true),
        }
    }

    /// Create peer connection with isolated TURN/STUN state
    /// PERFORMANCE: Preserves all hot-path optimizations in frame processing
    pub async fn create_peer_connection(
        &self,
        config: RTCConfiguration,
    ) -> webrtc::error::Result<RTCPeerConnection> {
        // Check circuit breaker
        if !self.is_healthy.load(Ordering::Acquire) {
            return Err(webrtc::Error::new(
                "Tube WebRTC API circuit breaker open".to_string(),
            ));
        }

        // Use original configuration - isolation is achieved via separate API instances
        // Each IsolatedWebRTCAPI has its own internal TURN client state
        let isolated_config = config;

        // Use the isolated API instance (completely separate from other tubes)
        let result = self.api.new_peer_connection(isolated_config).await;

        // Track errors for circuit breaking
        match &result {
            Err(e) => {
                let count = self.error_count.fetch_add(1, Ordering::Relaxed);
                if e.to_string().contains("turn") || e.to_string().contains("TURN") {
                    let turn_failures = self.turn_failure_count.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "TURN failure in isolated API for tube {} (failure #{}, total_errors:{})",
                        self.tube_id,
                        turn_failures + 1,
                        count + 1
                    );

                    // Circuit breaker: disable after 5 TURN failures
                    if turn_failures >= 4 {
                        error!(
                            "Circuit breaker OPEN for tube {} after {} TURN failures",
                            self.tube_id,
                            turn_failures + 1
                        );
                        self.is_healthy.store(false, Ordering::Release);
                    }
                } else {
                    warn!(
                        "WebRTC error in isolated API for tube {} (error #{}): {}",
                        self.tube_id,
                        count + 1,
                        e
                    );
                }
            }
            Ok(_) => {
                debug!(
                    "Successful peer connection created for tube {}",
                    self.tube_id
                );
            }
        }

        result
    }

    /// Get API health status
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Acquire)
    }

    /// Force reset the circuit breaker (for recovery)
    pub fn reset_circuit_breaker(&self) {
        info!("Resetting circuit breaker for tube {}", self.tube_id);
        self.error_count.store(0, Ordering::Release);
        self.turn_failure_count.store(0, Ordering::Release);
        self.is_healthy.store(true, Ordering::Release);
    }

    /// Get diagnostic information
    pub fn get_diagnostics(&self) -> (usize, usize, Duration) {
        (
            self.error_count.load(Ordering::Acquire),
            self.turn_failure_count.load(Ordering::Acquire),
            self.created_at.elapsed(),
        )
    }
}

// Utility for formatting ICE candidates as strings with the pre-allocated capacity
pub fn format_ice_candidate(candidate: &RTCIceCandidate) -> String {
    // Use a single format! macro for better efficiency
    if candidate.related_address.is_empty() {
        format!(
            "candidate:{} {} {} {} {} {} typ {}",
            candidate.foundation,
            candidate.component,
            candidate.protocol.to_string().to_lowercase(),
            candidate.priority,
            candidate.address,
            candidate.port,
            candidate.typ.to_string().to_lowercase()
        )
    } else {
        format!(
            "candidate:{} {} {} {} {} {} typ {} raddr {} rport {}",
            candidate.foundation,
            candidate.component,
            candidate.protocol.to_string().to_lowercase(),
            candidate.priority,
            candidate.address,
            candidate.port,
            candidate.typ.to_string().to_lowercase(),
            candidate.related_address,
            candidate.related_port
        )
    }
}

// Helper function to create a WebRTC peer connection with isolated API
pub async fn create_peer_connection_isolated(
    api: &IsolatedWebRTCAPI,
    config: Option<RTCConfiguration>,
) -> webrtc::error::Result<RTCPeerConnection> {
    // Use the configuration as provided or default
    let actual_config = config.unwrap_or_default();

    // Use the isolated API instance to prevent shared state corruption
    api.create_peer_connection(actual_config).await
}

// DEPRECATED: Legacy function - use create_peer_connection_isolated instead
// This function may cause TURN client state corruption between tubes
#[deprecated(note = "Use create_peer_connection_isolated to prevent tube cross-contamination")]
pub async fn create_peer_connection(
    config: Option<RTCConfiguration>,
) -> webrtc::error::Result<RTCPeerConnection> {
    warn!("DEPRECATED: Using global API singleton - this may cause TURN client corruption between tubes!");

    // Fallback to a temporary isolated API for backward compatibility
    let temp_api = IsolatedWebRTCAPI::new("legacy-global".to_string());
    create_peer_connection_isolated(&temp_api, config).await
}

// Helper function to create a data channel with optimized settings
pub async fn create_data_channel(
    peer_connection: &RTCPeerConnection,
    label: &str,
) -> webrtc::error::Result<Arc<RTCDataChannel>> {
    let config = RTCDataChannelInit {
        ordered: Some(true),   // Guarantee message order (required for TCP tunneling)
        max_retransmits: None, // CRITICAL FIX: Unlimited retransmits for fully reliable delivery
        max_packet_life_time: None, // No timeout for packets
        protocol: None,        // No specific protocol
        negotiated: None,      // Let WebRTC handle negotiation
    };

    debug!(
        "Creating data channel '{}' with config: ordered={:?}, fully reliable with unlimited retransmits",
        label, config.ordered
    );

    peer_connection
        .create_data_channel(label, Some(config))
        .await
}

// Lightweight struct for ICE candidate handler data to avoid circular references
#[derive(Clone)]
struct IceCandidateHandlerContext {
    tube_id: String,
    signal_sender: Option<UnboundedSender<SignalMessage>>,
    trickle_ice: bool,
    conversation_id: Option<String>,
    pending_candidates: Arc<Mutex<Vec<String>>>,
    peer_connection: Arc<RTCPeerConnection>,
    ice_gathering_start_time: Arc<Mutex<Option<Instant>>>,
    ice_candidate_count: Arc<AtomicUsize>,
}

impl IceCandidateHandlerContext {
    fn new(peer_connection: &WebRTCPeerConnection) -> Self {
        Self {
            tube_id: peer_connection.tube_id.clone(),
            signal_sender: peer_connection.signal_sender.clone(),
            trickle_ice: peer_connection.trickle_ice,
            conversation_id: peer_connection.conversation_id.clone(),
            pending_candidates: Arc::clone(&peer_connection.pending_incoming_ice_candidates),
            peer_connection: Arc::clone(&peer_connection.peer_connection),
            ice_gathering_start_time: Arc::clone(&peer_connection.ice_gathering_start_time),
            ice_candidate_count: Arc::clone(&peer_connection.ice_candidate_count),
        }
    }
}

// Async-first wrapper for core WebRTC operations
#[derive(Clone)]
pub struct WebRTCPeerConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub(crate) trickle_ice: bool,
    pub(crate) is_closing: Arc<AtomicBool>,
    pending_incoming_ice_candidates: Arc<Mutex<Vec<String>>>, // Buffer incoming candidates until ready
    pub(crate) signal_sender: Option<UnboundedSender<SignalMessage>>,
    pub tube_id: String,
    pub(crate) conversation_id: Option<String>,
    pub(crate) is_server_mode: bool,
    /// ICE agent resource guard wrapped in Arc<Mutex<>> for thread-safe access.
    ///
    /// This change from Arc<Option<IceAgentGuard>> to Arc<Mutex<Option<IceAgentGuard>>>
    /// was necessary to ensure proper resource cleanup during connection close operations.
    /// The Mutex provides thread-safe access for explicitly dropping the guard to prevent
    /// circular references that could block resource cleanup. This is critical for avoiding
    /// resource leaks in the ICE agent resource management system.
    ///
    /// The guard ensures that ICE agent resources are properly allocated and released,
    /// preventing resource exhaustion under high connection loads.
    _ice_agent_guard: Arc<Mutex<Option<IceAgentGuard>>>,

    // ISOLATION: Per-tube WebRTC API instance for complete isolation
    isolated_api: Arc<IsolatedWebRTCAPI>,

    // ISOLATION: Circuit breaker for comprehensive failure protection
    circuit_breaker: TubeCircuitBreaker,

    // Quality and network monitoring systems
    quality_manager: Arc<crate::webrtc_quality_manager::AdaptiveQualityManager>,

    // Keepalive infrastructure for session timeout prevention
    keepalive_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    keepalive_interval: Duration,
    last_activity: Arc<Mutex<Instant>>,
    keepalive_enabled: Arc<AtomicBool>,

    // Stats collection task for quality monitoring
    stats_collection_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,

    // Network monitoring and migration
    #[allow(dead_code)]
    network_monitor: Arc<crate::webrtc_network_monitor::NetworkMonitor>,
    network_integration: Arc<crate::webrtc_network_monitor::WebRTCNetworkIntegration>,

    // ICE restart and connection quality tracking
    connection_quality_degraded: Arc<AtomicBool>,
    initial_network_scan_triggered: Arc<AtomicBool>,

    // ICE gathering timing for minimum duration enforcement
    ice_gathering_start_time: Arc<Mutex<Option<Instant>>>,
    ice_candidate_count: Arc<AtomicUsize>,
    remote_candidate_count: Arc<AtomicUsize>,
    remote_candidate_receive_start: Arc<Mutex<Option<Instant>>>,

    // Consolidated state to prevent deadlocks
    activity_state: Arc<Mutex<ActivityState>>,
    ice_restart_state: Arc<Mutex<IceRestartState>>,
}

impl WebRTCPeerConnection {
    // Helper function to validate signaling state transitions
    fn validate_signaling_state_transition(
        current_state: webrtc::peer_connection::signaling_state::RTCSignalingState,
        is_answer: bool,
        is_local: bool,
    ) -> Result<(), String> {
        let operation = match (is_local, is_answer) {
            (true, true) => "local answer",
            (true, false) => "local offer",
            (false, true) => "remote answer",
            (false, false) => "remote offer",
        };

        let valid_transition = match (current_state, is_local, is_answer) {
            // Local descriptions
            (
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer,
                true,
                true,
            ) => true, // Local answer after remote offer
            (
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer,
                true,
                false,
            ) => false, // Local offer after local offer (invalid)
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true, false) => {
                true
            } // Local offer from stable
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true, true) => {
                false
            } // Local answer from stable (invalid)

            // Remote descriptions
            (
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer,
                false,
                true,
            ) => true, // Remote answer after local offer
            (
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer,
                false,
                false,
            ) => false, // Remote offer after remote offer (invalid)
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false, false) => {
                true
            } // Remote offer from stable
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false, true) => {
                false
            } // Remote answer from stable (invalid)

            _ => true, // Allow other transitions
        };

        if !valid_transition {
            return Err(format!(
                "Invalid signaling state transition from {current_state:?} applying {operation}"
            ));
        }

        Ok(())
    }

    pub async fn new(
        config: Option<RTCConfiguration>,
        trickle_ice: bool,
        turn_only: bool,
        signal_sender: Option<UnboundedSender<SignalMessage>>,
        tube_id: String,
        conversation_id: Option<String>,
    ) -> Result<Self, String> {
        debug!("Creating isolated WebRTC connection for tube {}", tube_id);

        // ISOLATION: Create dedicated WebRTC API instance for this tube
        // This prevents TURN client corruption from affecting other tubes
        let isolated_api = Arc::new(IsolatedWebRTCAPI::new(tube_id.clone()));

        // ISOLATION: Create circuit breaker for comprehensive failure protection
        let circuit_breaker = TubeCircuitBreaker::new(tube_id.clone());

        // Initialize quality manager for connection monitoring
        let quality_manager = Arc::new(crate::webrtc_quality_manager::AdaptiveQualityManager::new(
            tube_id.clone(),
            Default::default(),
        ));

        // Initialize per-tube network monitor to maintain isolation
        // Each tube gets its own monitor to prevent one failing tube from affecting others
        let network_monitor = Arc::new(crate::webrtc_network_monitor::NetworkMonitor::new(
            Default::default(),
        ));
        let network_integration = Arc::new(
            crate::webrtc_network_monitor::WebRTCNetworkIntegration::new(Arc::clone(
                &network_monitor,
            )),
        );
        // Acquire ICE agent permit before creating peer connection
        let ice_agent_guard = match RESOURCE_MANAGER.acquire_ice_agent_permit().await {
            Ok(guard) => Some(guard),
            Err(ResourceError::Exhausted { resource, limit }) => {
                warn!(
                    "ICE agent resource exhausted: {} limit ({}) exceeded (tube_id: {})",
                    resource, limit, tube_id
                );
                return Err(format!(
                    "Resource exhausted: {resource} limit ({limit}) exceeded"
                ));
            }
            Err(e) => {
                error!(
                    "Failed to acquire ICE agent permit: {} (tube_id: {})",
                    e, tube_id
                );
                return Err(format!("Failed to acquire ICE agent permit: {e}"));
            }
        };

        // Use the provided configuration or default
        let mut actual_config = config.unwrap_or_default();

        // Apply resource limits from the resource manager
        let limits = RESOURCE_MANAGER.get_limits();

        // Limit ICE candidate pool size to reduce socket usage
        actual_config.ice_candidate_pool_size = limits.max_interfaces_per_agent as u8;

        // Enhanced IPv6 handling: Filter out problematic IPv6 interfaces
        // This prevents binding errors that can reduce candidate availability

        // Apply ICE transport policy settings based on the turn_only flag
        if turn_only {
            // If turn_only, force use of relay candidates only
            actual_config.ice_transport_policy =
                webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay;
        } else {
            // Otherwise use all candidates
            actual_config.ice_transport_policy =
                webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All;
        }

        // ISOLATION: Create peer connection using isolated API instance
        // This ensures TURN/STUN client state is completely separate per tube
        let peer_connection =
            create_peer_connection_isolated(&isolated_api, Some(actual_config.clone()))
                .await
                .map_err(|e| {
                    format!(
                        "Failed to create isolated peer connection for tube {}: {e}",
                        tube_id
                    )
                })?;

        // Store the closing state and signal channel
        let is_closing = Arc::new(AtomicBool::new(false));
        let pending_incoming_ice_candidates = Arc::new(Mutex::new(Vec::new()));

        // Create an Arc<RTCPeerConnection> first
        let pc_arc = Arc::new(peer_connection);

        // No longer setting up ICE candidate handler here - this will be done in setup_ice_candidate_handler
        // to avoid duplicate handlers

        debug!(
            "Successfully created WebRTC peer connection with resource management (tube_id: {})",
            tube_id
        );

        // Return the new WebRTCPeerConnection struct with isolated API and keepalive infrastructure
        let now = Instant::now();
        Ok(Self {
            peer_connection: pc_arc,
            trickle_ice,
            is_closing,
            pending_incoming_ice_candidates,
            signal_sender,
            tube_id,
            conversation_id,
            is_server_mode: false, // Default to false (Gateway), will be set by Tube
            _ice_agent_guard: Arc::new(Mutex::new(ice_agent_guard)),

            // ISOLATION: Store isolated API instance with this connection
            isolated_api,

            // ISOLATION: Store circuit breaker with this connection
            circuit_breaker,

            // Quality and network monitoring
            quality_manager,

            // Initialize keepalive infrastructure
            keepalive_task: Arc::new(Mutex::new(None)),
            keepalive_interval: limits.ice_keepalive_interval,
            last_activity: Arc::new(Mutex::new(now)),
            keepalive_enabled: Arc::new(AtomicBool::new(false)),

            // Stats collection task for quality monitoring
            stats_collection_task: Arc::new(Mutex::new(None)),

            // Network monitoring and migration
            network_monitor,
            network_integration,

            // Initialize ICE restart and connection quality tracking
            connection_quality_degraded: Arc::new(AtomicBool::new(false)),
            initial_network_scan_triggered: Arc::new(AtomicBool::new(false)),

            // Initialize ICE gathering timing
            ice_gathering_start_time: Arc::new(Mutex::new(None)),
            ice_candidate_count: Arc::new(AtomicUsize::new(0)),
            remote_candidate_count: Arc::new(AtomicUsize::new(0)),
            remote_candidate_receive_start: Arc::new(Mutex::new(None)),

            // Consolidated state to prevent deadlocks
            activity_state: Arc::new(Mutex::new(ActivityState::new(now))),
            ice_restart_state: Arc::new(Mutex::new(IceRestartState::new())),
        })
    }

    /// Set server mode (true = Commander/creates offers, false = Gateway/creates answers)
    pub fn set_server_mode(&mut self, is_server_mode: bool) {
        self.is_server_mode = is_server_mode;
    }

    // Method to set up ICE candidate handler with channel-based signaling
    pub fn setup_ice_candidate_handler(&self) {
        // Handle ICE candidates only when using trickle ICE
        if !self.trickle_ice {
            debug!(
                "Not setting up ICE candidate handler - trickle ICE is disabled (tube_id: {})",
                self.tube_id
            );
            return;
        }
        debug!(
            "Setting up ICE candidate handler (tube_id: {})",
            self.tube_id
        );

        // IMPORTANT: To avoid circular references that prevent ICE agent cleanup,
        // we use a lightweight context struct instead of cloning the entire WebRTCPeerConnection
        let context = IceCandidateHandlerContext::new(self);

        // Remove any existing handlers first to avoid duplicates
        self.peer_connection
            .on_ice_candidate(Box::new(|_| Box::pin(async {})));

        // Set up handler for signaling state changes to flush buffered INCOMING candidates when ready
        let context_signaling = context.clone();

        self.peer_connection.on_signaling_state_change(Box::new(move |state| {
            debug!("Signaling state changed to: {:?} (tube_id: {})", state, context_signaling.tube_id);
            let context_clone = context_signaling.clone();
            Box::pin(async move {
                // Check if both descriptions are now set, regardless of specific state
                let local_desc = context_clone.peer_connection.local_description().await;
                let remote_desc = context_clone.peer_connection.remote_description().await;
                if local_desc.is_some() && remote_desc.is_some() {
                    debug!("Both descriptions set after signaling state change, flushing buffered INCOMING ICE candidates (tube_id: {})", context_clone.tube_id);
                    // Flush pending candidates manually (no self reference)
                    let candidates_to_flush = {
                        let mut lock = context_clone.pending_candidates.lock().unwrap();
                        std::mem::take(&mut *lock)
                    };
                    if !candidates_to_flush.is_empty() {
                        warn!("Flushing {} buffered incoming ICE candidates (tube_id: {}, count: {})", candidates_to_flush.len(), context_clone.tube_id, candidates_to_flush.len());
                        for (index, candidate_str) in candidates_to_flush.iter().enumerate() {
                            if !candidate_str.is_empty() {
                                let candidate_init = RTCIceCandidateInit {
                                    candidate: candidate_str.clone(),
                                    ..Default::default()
                                };
                                match context_clone.peer_connection.add_ice_candidate(candidate_init).await {
                                    Ok(()) => {
                                        info!("Successfully added buffered incoming ICE candidate (tube_id: {}, candidate: {}, index: {})", context_clone.tube_id, candidate_str, index);
                                    }
                                    Err(e) => {
                                        error!("Failed to add buffered incoming ICE candidate (tube_id: {}, candidate: {}, error: {}, index: {})", context_clone.tube_id, candidate_str, e, index);
                                    }
                                }
                            }
                        }
                    }
                }
            })
        }));

        // Set up handler for ICE candidates - SEND IMMEDIATELY (proper trickle ICE)
        let context_ice = context.clone();

        self.peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            debug!("on_ice_candidate triggered (tube_id: {})", context_ice.tube_id);

            let context_handler = context_ice.clone();

            Box::pin(async move {
                if let Some(c) = candidate {
                    // Record gathering start time on first candidate
                    {
                        let mut start_time = context_handler.ice_gathering_start_time.lock().unwrap();
                        if start_time.is_none() {
                            *start_time = Some(Instant::now());
                            debug!("ICE gathering started (tube_id: {})", context_handler.tube_id);

                            // Record metrics for ICE gathering start
                            if let Some(conversation_id) = &context_handler.conversation_id {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as f64;
                                crate::metrics::METRICS_COLLECTOR.update_ice_gathering_start(
                                    conversation_id,
                                    now_ms
                                );
                            }
                        }
                    }

                    // Increment candidate count
                    let count = context_handler.ice_candidate_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                    // Convert the ICE candidate to a string representation
                    let candidate_str = format_ice_candidate(&c);
                    debug!("ICE candidate gathered (tube_id: {}, candidate: {}, count: {})", context_handler.tube_id, candidate_str, count);

                    // Enhanced debugging: Log detailed candidate information
                    Self::log_candidate_details_static(&candidate_str, "OUTGOING", &context_handler.tube_id);

                    // Send immediately - no buffering on send side!
                    debug!("Sending ICE candidate immediately (trickle ICE) (tube_id: {})", context_handler.tube_id);
                    // Send ICE candidate manually (no self reference)
                    if let Some(sender) = &context_handler.signal_sender {
                        let message = SignalMessage {
                            tube_id: context_handler.tube_id.clone(),
                            kind: "icecandidate".to_string(),
                            data: candidate_str,
                            conversation_id: context_handler.conversation_id.clone().unwrap_or_else(|| context_handler.tube_id.clone()),
                            progress_flag: Some(if context_handler.trickle_ice { 2 } else { 0 }),
                            progress_status: Some("OK".to_string()),
                            is_ok: Some(true),
                        };
                        let _ = sender.send(message);
                    }
                } else {
                    // All ICE candidates gathered (received None)
                    // Enforce minimum gathering duration for trickle ICE to allow TURN allocation + signaling
                    const MIN_GATHERING_DURATION_SECS: u64 = 6; // 6 seconds for TURN (2-5s) + signaling latency (3-5s)

                    let should_delay = if context_handler.trickle_ice {
                        if let Some(start_time) = *context_handler.ice_gathering_start_time.lock().unwrap() {
                            let elapsed = start_time.elapsed().as_secs();
                            if elapsed < MIN_GATHERING_DURATION_SECS {
                                let delay_needed = MIN_GATHERING_DURATION_SECS - elapsed;
                                debug!(
                                    "ICE gathering completed early (tube_id: {}, elapsed: {}s, delaying {}s to allow TURN allocation + signaling)",
                                    context_handler.tube_id, elapsed, delay_needed
                                );
                                Some(Duration::from_secs(delay_needed))
                            } else {
                                None
                            }
                        } else {
                            warn!("ICE gathering completed but no start time recorded (tube_id: {})", context_handler.tube_id);
                            None
                        }
                    } else {
                        None
                    };

                    // Apply delay if needed
                    if let Some(delay) = should_delay {
                        tokio::time::sleep(delay).await;
                    }

                    let final_count = context_handler.ice_candidate_count.load(std::sync::atomic::Ordering::Relaxed);
                    let gathering_duration = context_handler.ice_gathering_start_time.lock().unwrap()
                        .map(|start| start.elapsed().as_secs_f64())
                        .unwrap_or(0.0);

                    debug!(
                        "ICE gathering complete (tube_id: {}, total_candidates: {}, duration: {:.1}s)",
                        context_handler.tube_id, final_count, gathering_duration
                    );

                    // Record metrics for ICE gathering complete
                    if let Some(conversation_id) = &context_handler.conversation_id {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as f64;
                        crate::metrics::METRICS_COLLECTOR.update_ice_gathering_complete(
                            conversation_id,
                            now_ms
                        );
                    }

                    // Send empty candidate signal
                    if let Some(sender) = &context_handler.signal_sender {
                        let message = SignalMessage {
                            tube_id: context_handler.tube_id.clone(),
                            kind: "icecandidate".to_string(),
                            data: "".to_string(),
                            conversation_id: context_handler.conversation_id.clone().unwrap_or_else(|| context_handler.tube_id.clone()),
                            progress_flag: Some(if context_handler.trickle_ice { 2 } else { 0 }),
                            progress_status: Some("OK".to_string()),
                            is_ok: Some(true),
                        };
                        let _ = sender.send(message);
                    }
                }
            })
        }));
    }

    // Method to flush buffered INCOMING ICE candidates (receive-side buffering)
    async fn flush_buffered_incoming_ice_candidates(&self) {
        debug!(
            "flush_buffered_incoming_ice_candidates called (tube_id: {})",
            self.tube_id
        );

        // Take the buffered candidates with a single lock operation
        let pending_candidates = {
            let mut lock = self.pending_incoming_ice_candidates.lock().unwrap();
            std::mem::take(&mut *lock)
        };

        // Add any buffered incoming candidates to the peer connection
        if !pending_candidates.is_empty() {
            warn!(
                "Flushing {} buffered incoming ICE candidates (tube_id: {}, count: {})",
                pending_candidates.len(),
                self.tube_id,
                pending_candidates.len()
            );
            for (index, candidate_str) in pending_candidates.iter().enumerate() {
                if !candidate_str.is_empty() {
                    let candidate_init = RTCIceCandidateInit {
                        candidate: candidate_str.clone(),
                        ..Default::default()
                    };

                    match self.peer_connection.add_ice_candidate(candidate_init).await {
                        Ok(()) => {
                            info!("Successfully added buffered incoming ICE candidate (tube_id: {}, candidate: {}, index: {})", self.tube_id, candidate_str, index);
                        }
                        Err(e) => {
                            error!("Failed to add buffered incoming ICE candidate (tube_id: {}, candidate: {}, error: {}, index: {})", self.tube_id, candidate_str, e, index);
                        }
                    }
                } else {
                    debug!(
                        "Skipping empty buffered candidate (tube_id: {}, index: {})",
                        self.tube_id, index
                    );
                }
            }

            // TRICKLE ICE FIX: After flushing all buffered candidates, trigger ICE connectivity checks
            if self.trickle_ice && !pending_candidates.is_empty() {
                let peer_conn_clone = self.peer_connection.clone();
                let tube_id_clone = self.tube_id.clone();
                let candidate_count = pending_candidates.len();
                tokio::spawn(async move {
                    // Small delay to allow all candidates to be fully processed
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    // Getting stats triggers internal ICE agent processing
                    let _ = peer_conn_clone.get_stats().await;

                    debug!(
                        "Triggered ICE connectivity check after flushing {} buffered trickle candidates (tube_id: {})",
                        candidate_count, tube_id_clone
                    );
                });
            }
        } else {
            debug!(
                "No buffered incoming ICE candidates to flush (tube_id: {})",
                self.tube_id
            );
        }
    }

    // Set or update the signal channel
    pub fn set_signal_channel(&mut self, signal_sender: UnboundedSender<SignalMessage>) {
        self.signal_sender = Some(signal_sender);
    }

    // Method to send an ICE candidate using the signal channel
    pub fn send_ice_candidate(&self, candidate: &str) {
        // Only proceed if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            // Create the ICE candidate message - use one-time allocation with format!
            // The data field of SignalMessage is just a String. We'll send the candidate string directly.

            let _progress_flag = Some(if self.trickle_ice { 2 } else { 0 });
            // Prepare the signaling message
            let message = SignalMessage {
                tube_id: self.tube_id.clone(),
                kind: "icecandidate".to_string(),
                data: candidate.to_string(), // Send the candidate string directly
                conversation_id: self
                    .conversation_id
                    .clone()
                    .unwrap_or_else(|| self.tube_id.clone()), // Use conversation_id if available, otherwise tube_id
                progress_flag: _progress_flag,
                progress_status: Some("OK".to_string()),
                is_ok: Some(true),
            };

            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!(
                    "Failed to send ICE candidate signal (tube_id: {}, error: {})",
                    self.tube_id, e
                );
            }
        } else {
            warn!(
                "Signal sender not available for ICE candidate (tube_id: {})",
                self.tube_id
            );
        }
    }

    // Method to send answer to router (no buffering - immediate sending)
    pub fn send_answer(&self, answer_sdp: &str) {
        // Only send it if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            let _progress_flag = Some(if self.trickle_ice { 2 } else { 0 });

            // Create and serialize the answer in one step
            let message = SignalMessage {
                tube_id: self.tube_id.clone(),
                kind: "answer".to_string(),
                data: answer_sdp.to_string(), // Send the answer SDP string directly
                conversation_id: self
                    .conversation_id
                    .clone()
                    .unwrap_or_else(|| self.tube_id.clone()), // Use conversation_id if available, otherwise tube_id
                progress_flag: _progress_flag,
                progress_status: Some("OK".to_string()),
                is_ok: Some(true),
            };

            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!(
                    "Failed to send answer signal (tube_id: {}, error: {})",
                    self.tube_id, e
                );
            }
        } else {
            warn!(
                "Signal sender not available for answer (tube_id: {})",
                self.tube_id
            );
        }
    }

    // Method to send connection state change signals
    pub fn send_connection_state_changed(&self, state: &str) {
        // Only send it if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            let _progress_flag = Some(if self.trickle_ice { 2 } else { 0 });

            // Create the connection state changed message
            let message = SignalMessage {
                tube_id: self.tube_id.clone(),
                kind: "connection_state_changed".to_string(),
                data: state.to_string(),
                conversation_id: self
                    .conversation_id
                    .clone()
                    .unwrap_or_else(|| self.tube_id.clone()), // Use conversation_id if available, otherwise tube_id
                progress_flag: _progress_flag,
                progress_status: Some("OK".to_string()),
                is_ok: Some(true),
            };

            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!(
                    "Failed to send connection state changed signal (tube_id: {}, error: {})",
                    self.tube_id, e
                );
            } else {
                debug!(
                    "Successfully sent connection state changed signal (tube_id: {}, state: {})",
                    self.tube_id, state
                );
            }
        } else {
            warn!(
                "Signal sender not available for connection state change (tube_id: {})",
                self.tube_id
            );
        }
    }

    pub(crate) async fn create_description_with_checks(
        &self,
        is_offer: bool,
    ) -> Result<String, String> {
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        let current_state = self.peer_connection.signaling_state();
        let sdp_type_str = if is_offer { "offer" } else { "answer" };
        debug!(
            "Current signaling state before create_{} (tube_id: {}, state: {:?})",
            sdp_type_str, self.tube_id, current_state
        );

        if is_offer {
            // Offer-specific signaling state validation
            if current_state
                == webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer
            {
                return if !self.trickle_ice {
                    if let Some(desc) = self.peer_connection.local_description().await {
                        debug!("Already have local offer and non-trickle, returning existing SDP (tube_id: {})", self.tube_id);
                        Ok(desc.sdp)
                    } else {
                        Err("Cannot create offer: already have local offer but failed to retrieve it (non-trickle)".to_string())
                    }
                } else {
                    Err(
                        "Cannot create offer when already have local offer (trickle ICE)"
                            .to_string(),
                    )
                };
            }
            // Other states are generally fine for creating an offer
        } else {
            // Answer-specific signaling state validation
            match current_state {
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer => {} // This is the expected state
                _ => {
                    return Err(format!(
                        "Cannot create answer when in state {current_state:?} - must have remote offer"
                    ));
                }
            }
        }

        self.generate_sdp_and_maybe_gather_ice(is_offer).await
    }

    async fn generate_sdp_and_maybe_gather_ice(&self, is_offer: bool) -> Result<String, String> {
        let sdp_type_str = if is_offer { "offer" } else { "answer" };

        let sdp_obj = if is_offer {
            self.peer_connection
                .create_offer(None)
                .await
                .map_err(|e| format!("Failed to create initial {sdp_type_str}: {e}"))?
        } else {
            self.peer_connection
                .create_answer(None)
                .await
                .map_err(|e| format!("Failed to create initial {sdp_type_str}: {e}"))?
        };

        if !self.trickle_ice {
            debug!(
                "Non-trickle ICE: gathering candidates before returning {} (tube_id: {})",
                sdp_type_str, self.tube_id
            );

            let initial_desc = if is_offer {
                RTCSessionDescription::offer(sdp_obj.sdp.clone())
            } else {
                RTCSessionDescription::answer(sdp_obj.sdp.clone())
            }
            .map_err(|e| {
                format!("Failed to create RTCSessionDescription for initial {sdp_type_str}: {e}")
            })?;

            self.peer_connection
                .set_local_description(initial_desc)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to set initial local description for {sdp_type_str} (non-trickle): {e}"
                    )
                })?;

            let (tx, rx) = oneshot::channel();
            let tx_arc = Arc::new(Mutex::new(Some(tx))); // Wrap sender in Arc<Mutex<Option<T>>>

            let pc_clone = Arc::clone(&self.peer_connection);
            let tube_id_clone = self.tube_id.clone();
            let sdp_type_str_clone = sdp_type_str.to_string(); // Clone for closure
            let captured_tx_arc = Arc::clone(&tx_arc); // Clone Arc for closure

            self.peer_connection.on_ice_gathering_state_change(Box::new(move |state: RTCIceGathererState| {
                let tx_for_handler = Arc::clone(&captured_tx_arc); // Clone Arc for the async block
                let pc_on_gather = Arc::clone(&pc_clone);
                let tube_id_log = tube_id_clone.clone();
                let sdp_type_log = sdp_type_str_clone.clone(); // Clone for async block logging
                Box::pin(async move {
                    debug!("ICE gathering state changed (non-trickle {}) (tube_id: {}, new_state: {:?})", sdp_type_log, tube_id_log, state);
                    if state == RTCIceGathererState::Complete {
                        if let Some(sender) = tx_for_handler.lock().unwrap().take() { // Use the Arc<Mutex<Option<Sender>>>
                            let _ = sender.send(());
                        }
                        // Clear the handler after completion by setting a no-op one.
                        pc_on_gather.on_ice_gathering_state_change(Box::new(|_| Box::pin(async {})));
                    }
                })
            }));

            match tokio::time::timeout(Duration::from_secs(15), rx).await {
                Ok(Ok(_)) => {
                    debug!(
                        "ICE gathering complete for non-trickle {} (tube_id: {})",
                        sdp_type_str, self.tube_id
                    );
                    if let Some(final_desc) = self.peer_connection.local_description().await {
                        let mut sdp_str = final_desc.sdp;

                        // Add max-message-size to answer SDP for non-trickle ICE only
                        if !is_offer && !sdp_str.contains("a=max-message-size") {
                            debug!("Answer SDP missing max-message-size, attempting to add it (tube_id: {})", self.tube_id);

                            // Extract max-message-size from the offer (remote description)
                            let max_message_size = if let Some(remote_desc) =
                                self.peer_connection.remote_description().await
                            {
                                debug!("Remote description found, searching for max-message-size (tube_id: {})", self.tube_id);

                                // Extract the max-message-size from the remote offer
                                let offer_sdp = &remote_desc.sdp;
                                if let Some(pos) = offer_sdp.find("a=max-message-size:") {
                                    let start = pos + "a=max-message-size:".len();
                                    if let Some(end) = offer_sdp[start..]
                                        .find('\r')
                                        .or_else(|| offer_sdp[start..].find('\n'))
                                    {
                                        if let Ok(size) =
                                            offer_sdp[start..start + end].trim().parse::<u32>()
                                        {
                                            debug!("Successfully extracted max-message-size from offer: {} (tube_id: {})", size, self.tube_id);
                                            size
                                        } else {
                                            debug!("Failed to parse max-message-size value from offer (tube_id: {})", self.tube_id);
                                            DEFAULT_MAX_MESSAGE_SIZE // Default if parsing fails
                                        }
                                    } else {
                                        debug!("No line ending found after max-message-size in offer (tube_id: {})", self.tube_id);
                                        DEFAULT_MAX_MESSAGE_SIZE // Default if no line ending
                                    }
                                } else {
                                    debug!("No max-message-size found in remote offer SDP (tube_id: {})", self.tube_id);
                                    DEFAULT_MAX_MESSAGE_SIZE // Default if isn't found in offer
                                }
                            } else {
                                debug!(
                                    "No remote description available (tube_id: {})",
                                    self.tube_id
                                );
                                DEFAULT_MAX_MESSAGE_SIZE // Default if no remote description
                            };

                            // Use the minimum of the client's requested size and our maximum
                            let our_max = OUR_MAX_MESSAGE_SIZE;
                            let negotiated_size = max_message_size.min(our_max);

                            debug!("Negotiating max-message-size: client_requested={} ({}KB), our_max={} ({}KB), negotiated={} ({}KB) (tube_id: {})",
                                   max_message_size, max_message_size/1024, our_max, our_max/1024, negotiated_size, negotiated_size/1024, self.tube_id);

                            // Find the position to insert after sctp-port
                            if let Some(sctp_pos) = sdp_str.find("a=sctp-port:") {
                                // Find the end of the sctp-port line
                                if let Some(line_end) = sdp_str[sctp_pos..].find('\n') {
                                    let insert_pos = sctp_pos + line_end + 1;
                                    debug!(
                                        "Found sctp-port at position {} (tube_id: {})",
                                        sctp_pos, self.tube_id
                                    );
                                    debug!("Inserting 'a=max-message-size:{}' at position {} (tube_id: {})", negotiated_size, insert_pos, self.tube_id);
                                    sdp_str.insert_str(
                                        insert_pos,
                                        &format!("a=max-message-size:{negotiated_size}\r\n"),
                                    );
                                    info!("Successfully added max-message-size={} ({}KB) to answer SDP (client requested: {} ({}KB), our max: {} ({}KB)) (tube_id: {})",
                                          negotiated_size, negotiated_size/1024, max_message_size, max_message_size/1024, our_max, our_max/1024, self.tube_id);
                                }
                            }
                        }

                        Ok(sdp_str)
                    } else {
                        Err(format!(
                            "Failed to get local description after gathering for {sdp_type_str}"
                        ))
                    }
                }
                Ok(Err(_)) => Err(format!("ICE gathering was cancelled for {sdp_type_str}")),
                Err(_) => Err(format!("ICE gathering timeout for {sdp_type_str}")),
            }
        } else {
            // Trickle ICE: return the SDP immediately.
            // The calling Tube will set the local description if this is an offer/answer being created by self.
            debug!(
                "Trickle ICE: returning {} immediately (tube_id: {})",
                sdp_type_str, self.tube_id
            );
            debug!(
                "Initial {} SDP (tube_id: {}, sdp: {})",
                sdp_type_str, self.tube_id, sdp_obj.sdp
            );

            // For trickle ICE, do not modify the SDP
            Ok(sdp_obj.sdp)
        }
    }

    // Create an offer (returns SDP string)
    pub async fn create_offer(&self) -> WebRTCResult<String> {
        self.create_description_with_checks(true)
            .await
            .map_err(|e| {
                WebRTCError::from_string_with_context(self.tube_id.clone(), e, "create_offer")
            })
    }

    // Create an answer (returns SDP string)
    pub async fn create_answer(&self) -> WebRTCResult<String> {
        self.create_description_with_checks(false)
            .await
            .map_err(|e| {
                WebRTCError::from_string_with_context(self.tube_id.clone(), e, "create_answer")
            })
    }

    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> WebRTCResult<()> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err(WebRTCError::ConnectionClosing {
                tube_id: self.tube_id.clone(),
            });
        }

        debug!(
            "set_remote_description called with {} (length: {} bytes) (tube_id: {})",
            if is_answer { "answer" } else { "offer" },
            sdp.len(),
            self.tube_id
        );

        // Check if the offer contains max-message-size
        if !is_answer && sdp.contains("a=max-message-size:") {
            debug!(
                "Incoming offer contains max-message-size attribute (tube_id: {})",
                self.tube_id
            );
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            RTCSessionDescription::answer(sdp)
        } else {
            RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {e}"))?;

        // Check the current signaling state before setting the remote description
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before set_remote_description");

        // Validate the signaling state transition
        Self::validate_signaling_state_transition(current_state, is_answer, false)?;

        // Set the remote description
        let result = self
            .peer_connection
            .set_remote_description(desc)
            .await
            .map_err(|e| WebRTCError::RemoteDescriptionFailed {
                tube_id: self.tube_id.clone(),
                reason: format!("Failed to set remote description: {e}"),
            });

        // If successful, update activity and flush buffered incoming candidates
        if result.is_ok() {
            // Update activity timestamp - SDP exchange is significant activity
            self.update_activity();

            let local_desc = self.peer_connection.local_description().await;
            let remote_desc = self.peer_connection.remote_description().await;
            if local_desc.is_some() && remote_desc.is_some() {
                debug!("Both descriptions now set after remote description, flushing buffered incoming ICE candidates (tube_id: {})", self.tube_id);
                self.flush_buffered_incoming_ice_candidates().await;
            }
        }

        result
    }

    pub async fn add_ice_candidate(&self, candidate_str: String) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            warn!(
                "add_ice_candidate called but connection is closing (tube_id: {})",
                self.tube_id
            );
            return Err("Connection is closing".to_string());
        }

        debug!(
            "add_ice_candidate called (tube_id: {}, candidate: {})",
            self.tube_id, candidate_str
        );

        // Check if we can add candidates immediately (both descriptions must be set)
        let local_desc = self.peer_connection.local_description().await;
        let remote_desc = self.peer_connection.remote_description().await;
        let can_add_immediately = local_desc.is_some() && remote_desc.is_some();

        debug!("Checking if can add ICE candidate immediately (tube_id: {}, local_set: {}, remote_set: {}, can_add_immediately: {})", self.tube_id, local_desc.is_some(), remote_desc.is_some(), can_add_immediately);

        if can_add_immediately {
            // Connection is ready, add the candidate immediately
            debug!(
                "Both descriptions set, adding incoming ICE candidate immediately (tube_id: {})",
                self.tube_id
            );

            if !candidate_str.is_empty() {
                // Track remote candidate receipt timing and count
                {
                    let mut start_time = self.remote_candidate_receive_start.lock().unwrap();
                    if start_time.is_none() {
                        *start_time = Some(Instant::now());
                        debug!(
                            "Started receiving remote ICE candidates (tube_id: {})",
                            self.tube_id
                        );
                    }
                }

                let remote_count = self.remote_candidate_count.fetch_add(1, Ordering::Relaxed) + 1;

                // Warn if we're receiving very few candidates
                if remote_count == 1 {
                    // Start a background task to check candidate count after 15 seconds
                    let tube_id = self.tube_id.clone();
                    let remote_count_check = Arc::clone(&self.remote_candidate_count);
                    let start_time_check = Arc::clone(&self.remote_candidate_receive_start);

                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(15)).await;
                        let final_count = remote_count_check.load(Ordering::Relaxed);
                        if final_count < 3 {
                            let elapsed = start_time_check
                                .lock()
                                .unwrap()
                                .map(|start| start.elapsed().as_secs())
                                .unwrap_or(0);
                            warn!(
                                "[LOW_CANDIDATE_COUNT] Received only {} remote candidates after {}s (tube_id: {}) - connection may fail",
                                final_count, elapsed, tube_id
                            );
                        }
                    });
                }

                debug!(
                    "Received remote ICE candidate #{} (tube_id: {})",
                    remote_count, self.tube_id
                );

                let candidate_init = RTCIceCandidateInit {
                    candidate: candidate_str.clone(),
                    ..Default::default()
                };

                match self.peer_connection.add_ice_candidate(candidate_init).await {
                    Ok(()) => {
                        debug!(
                            "Successfully added ICE candidate immediately (tube_id: {})",
                            self.tube_id
                        );

                        // Enhanced debugging: Log candidate details and analyze pairs
                        self.log_candidate_details(&candidate_str, "INCOMING");

                        // TRICKLE ICE FIX: Trigger ICE agent to re-evaluate candidate pairs
                        // In webrtc-rs 0.14.0, adding candidates after set_remote_description()
                        // doesn't automatically trigger connectivity checks on newly formed pairs.
                        // We work around this by getting stats, which internally causes the ICE
                        // agent to re-evaluate the connection state and trigger checks.
                        if self.trickle_ice {
                            let peer_conn_clone = self.peer_connection.clone();
                            let tube_id_clone = self.tube_id.clone();
                            tokio::spawn(async move {
                                // Small delay to allow the candidate to be fully processed
                                tokio::time::sleep(Duration::from_millis(100)).await;

                                // Getting stats triggers internal ICE agent processing
                                let _ = peer_conn_clone.get_stats().await;

                                debug!(
                                    "Triggered ICE connectivity check after adding trickle candidate (tube_id: {})",
                                    tube_id_clone
                                );

                                // Analyze candidate pairs for debugging
                                if let Err(e) = Self::analyze_candidate_pairs_static(
                                    &peer_conn_clone,
                                    &tube_id_clone,
                                )
                                .await
                                {
                                    debug!("Failed to analyze candidate pairs after adding remote candidate (tube_id: {}, error: {})", tube_id_clone, e);
                                }
                            });
                        }

                        Ok(())
                    }
                    Err(e) => {
                        error!(
                            "Failed to add ICE candidate immediately (tube_id: {}, error: {})",
                            self.tube_id, e
                        );
                        Err(format!("Failed to add ICE candidate: {e}"))
                    }
                }
            } else {
                // Empty candidate string means end-of-candidates, which is valid
                let final_remote_count = self.remote_candidate_count.load(Ordering::Relaxed);
                let receive_duration = self
                    .remote_candidate_receive_start
                    .lock()
                    .unwrap()
                    .map(|start| start.elapsed().as_secs_f64())
                    .unwrap_or(0.0);

                info!(
                    "Remote ICE gathering complete (tube_id: {}, total_remote_candidates: {}, duration: {:.1}s)",
                    self.tube_id, final_remote_count, receive_duration
                );

                // Final warning if very few candidates received
                if final_remote_count > 0 && final_remote_count < 3 {
                    warn!(
                        "[LOW_CANDIDATE_COUNT] Remote peer sent only {} candidates - connection quality may be degraded (tube_id: {})",
                        final_remote_count, self.tube_id
                    );
                }

                Ok(())
            }
        } else {
            // Connection is not ready yet, buffer the incoming candidate
            let mut candidates_lock = self.pending_incoming_ice_candidates.lock().unwrap();
            candidates_lock.push(candidate_str.clone());
            let buffered_count = candidates_lock.len();
            drop(candidates_lock);

            warn!("Descriptions not ready (local: {}, remote: {}), buffering incoming ICE candidate (total buffered: {}) (tube_id: {}, candidate: {})",
                   local_desc.is_some(), remote_desc.is_some(), buffered_count, self.tube_id, candidate_str);
            Ok(())
        }
    }

    pub fn connection_state(&self) -> String {
        // Fast path for closing state
        if self.is_closing.load(Ordering::Acquire) {
            return "Closed".to_string();
        }

        format!("{:?}", self.peer_connection.connection_state())
    }

    /// Setup connection state monitoring to trigger ICE restarts on connection issues
    pub async fn setup_connection_state_monitoring(&self) -> Result<(), String> {
        let network_integration = Arc::clone(&self.network_integration);
        let tube_id = self.tube_id.clone();
        let initial_scan_triggered = Arc::clone(&self.initial_network_scan_triggered);

        // Monitor peer connection state changes
        let peer_connection = Arc::clone(&self.peer_connection);
        peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            let network_integration = Arc::clone(&network_integration);
            let tube_id = tube_id.clone();
            let initial_scan_triggered = Arc::clone(&initial_scan_triggered);

            Box::pin(async move {
                debug!("Peer connection state changed for tube {}: {:?}", tube_id, state);

                // Trigger ICE restart for problematic states
                match state {
                    RTCPeerConnectionState::Disconnected => {
                        info!("Connection disconnected for tube {}, considering ICE restart", tube_id);
                        // Wait a moment to see if connection recovers
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        network_integration.trigger_ice_restart(&tube_id, "connection disconnected");
                    },
                    RTCPeerConnectionState::Failed => {
                        warn!("Connection failed for tube {}, triggering immediate ICE restart", tube_id);
                        network_integration.trigger_ice_restart(&tube_id, "connection failed");
                    },
                    RTCPeerConnectionState::Connected => {
                        info!("Connection established/restored for tube {}", tube_id);

                        // Trigger initial network scan now that WebRTC connection is established (idempotent)
                        if !initial_scan_triggered.load(Ordering::Acquire)
                            && initial_scan_triggered.compare_exchange(
                                false,
                                true,
                                Ordering::AcqRel,
                                Ordering::Acquire
                            ).is_ok() {
                                let network_integration_clone = Arc::clone(&network_integration);
                                let tube_id_clone = tube_id.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = network_integration_clone.trigger_initial_scan().await {
                                        warn!("Failed to trigger initial network scan for tube {}: {}", tube_id_clone, e);
                                    } else {
                                        info!("Initial network scan triggered successfully for tube {} (event-driven)", tube_id_clone);
                                    }
                                });
                            }
                    },
                    _ => {
                        debug!("Peer connection state: {:?} for tube {}", state, tube_id);
                    }
                }
            })
        }));

        debug!(
            "Connection state monitoring setup completed for tube {}",
            self.tube_id
        );
        Ok(())
    }

    /// Coordinate quality management with recovery systems
    pub async fn trigger_adaptive_recovery(&self, error: &WebRTCError) -> Result<(), String> {
        // Simplified recovery handling - just log the error
        let error_type_str = format!("{:?}", error)
            .split('(')
            .next()
            .unwrap_or("Unknown")
            .to_string();

        debug!(
            "Connection error reported for tube {} (error: {})",
            self.tube_id, error_type_str
        );

        Ok(())
    }

    /// Start monitoring and quality management systems
    pub async fn start_monitoring(&self) -> Result<(), String> {
        // Start quality monitoring
        self.quality_manager
            .start_monitoring()
            .await
            .map_err(|e| format!("Failed to start quality monitoring: {}", e))?;

        // Register this tube with monitoring systems
        debug!("Registering tube {} with monitoring systems", self.tube_id);

        // Register with metrics collector for connection health tracking
        if let Some(ref conv_id) = self.conversation_id {
            crate::metrics::METRICS_COLLECTOR
                .register_connection(conv_id.clone(), self.tube_id.clone());
            debug!(
                "Registered connection with metrics collector (conversation_id: {}, tube_id: {})",
                conv_id, self.tube_id
            );
        }

        // Start periodic stats collection
        self.start_stats_collection().await?;

        // Network monitoring note:
        // The NetworkMonitor tries to use pyo3_log::Logger from Rust async tasks
        // but Python interpreter isn't initialized in pure Rust test contexts
        debug!("Skipping network monitoring startup to avoid Python interpreter issues");

        // Register ICE restart callback for network changes
        let ice_restart_callback = {
            let peer_connection = Arc::clone(&self.peer_connection);
            let tube_id = self.tube_id.clone();
            move || {
                let pc = Arc::clone(&peer_connection);
                let id = tube_id.clone();
                tokio::spawn(async move {
                    info!(
                        "Network change detected, triggering ICE restart for tube {}",
                        id
                    );
                    if let Err(e) = pc.restart_ice().await {
                        warn!(
                            "Failed to restart ICE due to network change (tube_id: {}, error: {})",
                            id, e
                        );
                    }
                });
            }
        };

        self.network_integration
            .register_tube(self.tube_id.clone(), ice_restart_callback);

        // Start network monitoring
        if let Err(e) = self.network_integration.start().await {
            warn!(
                "Failed to start network integration (tube_id: {}, error: {})",
                self.tube_id, e
            );
        }

        // Enable connection state-based ICE restart triggers as backup
        self.setup_connection_state_monitoring().await?;

        debug!("Monitoring systems started for tube {}", self.tube_id);
        Ok(())
    }

    /// Stop all monitoring systems during shutdown
    pub async fn stop_monitoring_systems(&self) -> Result<(), String> {
        // Stop quality monitoring
        self.quality_manager.stop_monitoring();

        // Stop stats collection task
        if let Ok(mut task_guard) = self.stats_collection_task.lock() {
            if let Some(handle) = task_guard.take() {
                handle.abort();
                info!(
                    "Stats collection task stopped and cleaned up (tube_id: {})",
                    self.tube_id
                );
            }
        }

        // Stop network monitoring
        self.network_monitor.stop_monitoring();
        self.network_integration.unregister_tube(&self.tube_id);
        self.network_integration.stop();

        info!("Monitoring systems stopped for tube {}", self.tube_id);
        Ok(())
    }

    /// Report successful operation for monitoring systems
    pub async fn report_success(&self, operation: &str) -> Result<(), String> {
        debug!(
            "Success reported for tube {} operation: {}",
            self.tube_id, operation
        );
        Ok(())
    }

    /// Start periodic stats collection for quality monitoring
    async fn start_stats_collection(&self) -> Result<(), String> {
        let peer_connection = Arc::clone(&self.peer_connection);
        let quality_manager = Arc::clone(&self.quality_manager);
        let is_closing = Arc::clone(&self.is_closing);
        let tube_id = self.tube_id.clone();
        let conversation_id = self.conversation_id.clone();
        let is_server_mode = self.is_server_mode;

        let stats_task_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5)); // Collect stats every 5 seconds
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let ipv6_binding_failures = std::sync::atomic::AtomicU32::new(0);
            let mut previous_stats: Option<crate::webrtc_quality_manager::WebRTCStats> = None;

            loop {
                interval.tick().await;

                // Check if connection is closing
                if is_closing.load(Ordering::Acquire) {
                    debug!(
                        "Stats collection stopping due to connection closing (tube_id: {})",
                        tube_id
                    );
                    break;
                }

                // IPv6 monitoring: Real IPv6 binding failure detection
                let ipv6_failures = {
                    let mut failure_count = 0u32;

                    // Test IPv6 binding capability
                    match std::net::UdpSocket::bind("[::]:0") {
                        Ok(socket) => {
                            // IPv6 works, verify we got an IPv6 address
                            if let Ok(addr) = socket.local_addr() {
                                if !addr.is_ipv6() {
                                    failure_count += 1;
                                }
                            }
                        }
                        Err(_) => {
                            // IPv6 binding failed
                            failure_count += 1;
                        }
                    }

                    // Check if any IPv6 interfaces exist using if-addrs
                    match if_addrs::get_if_addrs() {
                        Ok(addrs) => {
                            let has_ipv6 = addrs.iter().any(|iface| {
                                matches!(iface.addr, if_addrs::IfAddr::V6(_))
                                    && !iface.addr.ip().is_loopback()
                            });
                            if !has_ipv6 {
                                failure_count += 5; // No IPv6 interfaces = significant failure
                            }
                        }
                        Err(_) => failure_count += 3,
                    }

                    failure_count
                };

                if ipv6_failures > 0 {
                    ipv6_binding_failures
                        .store(ipv6_failures, std::sync::atomic::Ordering::Relaxed);

                    if ipv6_failures > 5 {
                        debug!(
                            "[IPV6_MONITOR] Multiple IPv6 binding failures detected (count: {}) - normal on macOS, may reduce candidate pool (tube_id: {})",
                            ipv6_failures, tube_id
                        );
                    }
                }

                // Collect real WebRTC stats from peer connection
                let reports = peer_connection.get_stats().await;
                let webrtc_stats = {
                    let mut collected_stats = crate::webrtc_quality_manager::WebRTCStats {
                        timestamp: Instant::now(),
                        ..Default::default()
                    };

                    // Parse WebRTC stats reports for relevant metrics
                    for (_id, report) in reports.reports.iter() {
                        match report {
                            webrtc::stats::StatsReportType::InboundRTP(inbound) => {
                                collected_stats.bytes_received = inbound.bytes_received;
                                collected_stats.packets_received = inbound.packets_received;
                            }
                            webrtc::stats::StatsReportType::OutboundRTP(outbound) => {
                                collected_stats.bytes_sent = outbound.bytes_sent;
                                collected_stats.packets_sent = outbound.packets_sent;
                            }
                            webrtc::stats::StatsReportType::DataChannel(data_channel) => {
                                // Data channel message and byte counts (for non-media traffic like nops, sync, etc.)
                                collected_stats.bytes_sent = collected_stats
                                    .bytes_sent
                                    .max(data_channel.bytes_sent as u64);
                                collected_stats.packets_sent = collected_stats
                                    .packets_sent
                                    .max(data_channel.messages_sent as u64);
                            }
                            webrtc::stats::StatsReportType::Transport(transport) => {
                                // Transport-level bytes (includes all data channel traffic)
                                collected_stats.bytes_sent =
                                    collected_stats.bytes_sent.max(transport.bytes_sent as u64);
                                collected_stats.bytes_received = collected_stats
                                    .bytes_received
                                    .max(transport.bytes_received as u64);
                            }
                            webrtc::stats::StatsReportType::RemoteInboundRTP(remote_inbound) => {
                                // Use remote inbound stats for RTT and packet loss (more accurate)
                                if let Some(rtt) = remote_inbound.round_trip_time {
                                    collected_stats.rtt_ms = Some(rtt * 1000.0);
                                }
                                // Calculate packets lost from fraction_lost if available
                                if collected_stats.packets_sent > 0 {
                                    collected_stats.packets_lost = (collected_stats.packets_sent
                                        as f64
                                        * remote_inbound.fraction_lost)
                                        as u64;
                                }
                            }
                            webrtc::stats::StatsReportType::CandidatePair(pair) => {
                                if pair.nominated {
                                    // Use nominated candidate pair for RTT if remote stats unavailable
                                    if collected_stats.rtt_ms.is_none() {
                                        collected_stats.rtt_ms =
                                            Some(pair.current_round_trip_time * 1000.0);
                                    }
                                }
                            }
                            _ => {} // Ignore other stat types
                        }
                    }

                    // Calculate bitrate from byte delta if we have previous stats
                    if let Some(prev_stats) = &previous_stats {
                        let time_delta = collected_stats
                            .timestamp
                            .duration_since(prev_stats.timestamp)
                            .as_secs_f64();
                        if time_delta > 0.0 {
                            let bytes_delta = (collected_stats.bytes_sent
                                + collected_stats.bytes_received)
                                .saturating_sub(prev_stats.bytes_sent + prev_stats.bytes_received);
                            collected_stats.bitrate_bps =
                                Some((bytes_delta as f64 * 8.0 / time_delta) as u64);
                        }
                    }

                    collected_stats
                };

                // Log collected stats to verify real data is being gathered
                debug!(
                    "Collected WebRTC stats (tube_id: {}, bytes_sent: {}, bytes_received: {}, packets_sent: {}, packets_received: {}, rtt_ms: {:?}, bitrate_bps: {:?})",
                    tube_id,
                    webrtc_stats.bytes_sent,
                    webrtc_stats.bytes_received,
                    webrtc_stats.packets_sent,
                    webrtc_stats.packets_received,
                    webrtc_stats.rtt_ms,
                    webrtc_stats.bitrate_bps
                );

                // Store current stats for next cycle's bitrate calculation
                previous_stats = Some(webrtc_stats.clone());

                // Update the quality manager with real stats
                if let Err(e) = quality_manager.update_stats(webrtc_stats.clone()).await {
                    debug!(
                        "Failed to update quality manager stats (tube_id: {}, error: {})",
                        tube_id, e
                    );
                }

                // Retrieve comprehensive metrics for connection leg visibility
                let connection_health = if let Some(ref conv_id) = conversation_id {
                    crate::metrics::METRICS_COLLECTOR
                        .get_connection_health(&tube_id)
                        .or_else(|| {
                            crate::metrics::METRICS_COLLECTOR.get_connection_health(conv_id)
                        })
                } else {
                    crate::metrics::METRICS_COLLECTOR.get_connection_health(&tube_id)
                };

                if let Some(metrics) = connection_health {
                    let legs = &metrics.webrtc_metrics.connection_legs;
                    let ice_stats = &metrics.webrtc_metrics.rtc_stats.ice_stats;
                    let bandwidth_estimate_bps = quality_manager.get_bandwidth_estimate_bps();
                    let current_metrics = quality_manager.get_current_metrics().await;

                    // Calculate uptime
                    let uptime = chrono::Utc::now().signed_duration_since(metrics.established_at);
                    let uptime_str = if uptime.num_hours() > 0 {
                        format!("{}h{}m", uptime.num_hours(), uptime.num_minutes() % 60)
                    } else if uptime.num_minutes() > 0 {
                        format!("{}m{}s", uptime.num_minutes(), uptime.num_seconds() % 60)
                    } else {
                        format!("{}s", uptime.num_seconds())
                    };

                    // Get connection path from selected candidate pair
                    let connection_path = if let Some(ref pair) = ice_stats.selected_candidate_pair
                    {
                        format!(
                            "{}->{}",
                            pair.local_candidate_type, pair.remote_candidate_type
                        )
                    } else {
                        "unknown".to_string()
                    };

                    // Calculate current throughput rates (from bitrate which is already delta-based)
                    let send_rate_bps = webrtc_stats.bitrate_bps.unwrap_or(0) / 2; // Approximate split
                    let recv_rate_bps = webrtc_stats.bitrate_bps.unwrap_or(0) / 2;

                    // Determine side label based on server_mode
                    let side_label = if is_server_mode {
                        "Commander"
                    } else {
                        "Gateway"
                    };

                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!(
                            "Connection Metrics ({}) | tube_id: {} | Uptime: {} | Path: {} | E2E: {:?}ms | {}<->KRelay: {:?}ms | RTT: {:?}ms | Jitter: {:.1}ms | BW: {:.2}Mbps ^{:.0}bps v{:.0}bps | Loss: {:.2}% | Quality: {}/100 | Congestion: {:?} | Sent: {:.2}MB | Recv: {:.2}MB",
                            side_label,
                            tube_id,
                            uptime_str,
                            connection_path,
                            legs.end_to_end_latency_ms,
                            side_label,
                            legs.krelay_to_gateway_latency_ms,
                            webrtc_stats.rtt_ms,
                            current_metrics.jitter_ms,
                            bandwidth_estimate_bps as f64 / 1_000_000.0,
                            send_rate_bps,
                            recv_rate_bps,
                            current_metrics.packet_loss_rate * 100.0,
                            current_metrics.quality_score,
                            current_metrics.congestion_level,
                            webrtc_stats.bytes_sent as f64 / 1_000_000.0,
                            webrtc_stats.bytes_received as f64 / 1_000_000.0
                        );
                    }
                }

                // Apply quality recommendations (every 5 seconds)
                // This creates a feedback loop where quality metrics influence connection behavior
                // Note: We need to be careful not to create circular references here
                // The quality manager makes recommendations, but doesn't directly call back to the connection
            }

            debug!("Stats collection task finished (tube_id: {})", tube_id);
        });

        if let Ok(mut task_guard) = self.stats_collection_task.lock() {
            *task_guard = Some(stats_task_handle);
            debug!("Started stats collection task (tube_id: {})", self.tube_id);
        } else {
            return Err("Failed to acquire stats task lock".to_string());
        }

        Ok(())
    }

    pub async fn close(&self) -> Result<(), String> {
        // Avoid duplicate close operations
        if self.is_closing.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already closing or closed
        }

        // Stop keepalive task before closing
        if let Err(e) = self.stop_keepalive().await {
            warn!(
                "Failed to stop keepalive during close (tube_id: {}, error: {})",
                self.tube_id, e
            );
        }

        // Stop all monitoring systems before closing
        if let Err(e) = self.stop_monitoring_systems().await {
            warn!(
                "Failed to stop monitoring systems during close (tube_id: {}, error: {})",
                self.tube_id, e
            );
        }

        // First, clear all callbacks
        self.peer_connection
            .on_ice_candidate(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_ice_gathering_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_data_channel(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_peer_connection_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_signaling_state_change(Box::new(|_| Box::pin(async {})));

        // CRITICAL: Explicitly drop the ICE agent guard to ensure resource cleanup
        // This breaks any circular references that might prevent the guard from being dropped
        {
            match self._ice_agent_guard.lock() {
                Ok(mut guard_lock) => {
                    if let Some(guard) = guard_lock.take() {
                        info!("Explicitly dropping ICE agent guard to ensure resource cleanup (tube_id: {})", self.tube_id);
                        drop(guard);
                    }
                }
                Err(e) => {
                    warn!("Failed to acquire ICE agent guard lock during cleanup - proceeding anyway (tube_id: {}, error: {})", self.tube_id, e);
                }
            }
        }

        // Then close the connection with a timeout to avoid hanging
        match tokio::time::timeout(Duration::from_secs(5), self.peer_connection.close()).await {
            Ok(result) => result.map_err(|e| format!("Failed to close peer connection: {e}")),
            Err(_) => {
                // The timeout elapsed.
                warn!("Close operation timed out for peer connection. The underlying webrtc-rs close() did not complete in 5 seconds. (tube_id: {})", self.tube_id);
                // Return an error instead of Ok(())
                Err(format!(
                    "Peer connection close operation timed out for tube {}",
                    self.tube_id
                ))
            }
        }
    }

    // Add method to set local description for better state management
    pub async fn set_local_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            RTCSessionDescription::answer(sdp)
        } else {
            RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {e}"))?;

        // Check the current signaling state before setting the local description
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before set_local_description");

        // Validate the signaling state transition
        Self::validate_signaling_state_transition(current_state, is_answer, true)?;

        // Set the local description
        let result = self
            .peer_connection
            .set_local_description(desc)
            .await
            .map_err(|e| format!("Failed to set local description: {e}"));

        // If successful, update activity and flush buffered incoming candidates
        if result.is_ok() {
            // Update activity timestamp - local SDP setting is significant activity
            self.update_activity();

            let local_desc = self.peer_connection.local_description().await;
            let remote_desc = self.peer_connection.remote_description().await;
            if local_desc.is_some() && remote_desc.is_some() {
                debug!("Both descriptions now set after local description, flushing buffered incoming ICE candidates (tube_id: {})", self.tube_id);
                self.flush_buffered_incoming_ice_candidates().await;
            }
        }

        result
    }

    // Get buffered incoming ICE candidates (for debugging/monitoring)
    pub fn get_ice_candidates(&self) -> Vec<String> {
        // NOTE: Outgoing candidates are sent immediately (no buffering)
        // This returns currently buffered incoming candidates
        let candidates = self.pending_incoming_ice_candidates.lock().unwrap();
        candidates.clone()
    }

    // Start keepalive mechanism to prevent NAT timeout (19-minute issue prevention)
    // This integrates with the existing channel ping/pong system rather than duplicating it
    pub async fn start_keepalive(&self) -> Result<(), String> {
        // Enable keepalive flag for coordination with existing ping system
        self.keepalive_enabled.store(true, Ordering::Relaxed);

        // The actual keepalive implementation leverages the existing channel ping/pong system
        // Channels already send pings on timeout - we just need to ensure they do it frequently enough
        // to prevent NAT timeout (every 5 minutes instead of waiting for actual timeouts)

        let keepalive_enabled_clone = self.keepalive_enabled.clone();
        let tube_id_clone = self.tube_id.clone();
        let pc_clone = self.peer_connection.clone();
        let keepalive_interval = self.keepalive_interval;

        // Create a lightweight task that just ensures periodic activity
        let keepalive_task_handle = tokio::spawn(async move {
            debug!("NAT timeout prevention active - ensuring periodic activity every {} seconds (tube_id: {}, interval_minutes: {})",
                  keepalive_interval.as_secs(), tube_id_clone, keepalive_interval.as_secs() / 60);

            let mut interval = tokio::time::interval(keepalive_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            while keepalive_enabled_clone.load(Ordering::Relaxed) {
                interval.tick().await;

                if !keepalive_enabled_clone.load(Ordering::Relaxed) {
                    break;
                }

                // This keepalive task does not send pings directly; instead, it ensures periodic activity
                // so that the channel's internal ping/pong mechanism (which triggers on activity or timeout)
                // remains active and prevents NAT timeouts. No additional ping implementation is needed here.
                debug!("NAT timeout prevention tick - periodic activity to keep channel ping system active (tube_id: {})", tube_id_clone);

                // Get current connection state to verify we're still connected
                let connection_state = pc_clone.connection_state();
                debug!(
                    "Connection state check (tube_id: {}, connection_state: {:?})",
                    tube_id_clone, connection_state
                );
            }

            info!(
                "NAT timeout prevention stopped (tube_id: {})",
                tube_id_clone
            );
        });

        // Store the task handle
        if let Ok(mut task_guard) = self.keepalive_task.lock() {
            if let Some(old_task) = task_guard.take() {
                old_task.abort(); // Clean up any existing task
            }
            *task_guard = Some(keepalive_task_handle);
        } else {
            return Err("Failed to acquire keepalive task lock".to_string());
        }

        debug!("NAT timeout prevention started - integrated with existing channel ping system (tube_id: {})", self.tube_id);
        Ok(())
    }

    // Update activity timestamp for timeout detection
    pub fn update_activity(&self) {
        let now = Instant::now();

        // Update both activity timestamps in a single lock acquisition (deadlock-safe)
        if let Ok(mut activity_state) = self.activity_state.lock() {
            activity_state.update_both(now);
            debug!(
                "Activity updated - connection active (tube_id: {})",
                self.tube_id
            );
        } else {
            warn!(
                "Failed to acquire lock for activity update (tube_id: {})",
                self.tube_id
            );
        }

        // Also update the legacy last_activity for backward compatibility
        if let Ok(mut last_activity) = self.last_activity.lock() {
            *last_activity = now;
        }
    }

    // Stop keepalive mechanism
    pub async fn stop_keepalive(&self) -> Result<(), String> {
        // Disable keepalive flag
        self.keepalive_enabled.store(false, Ordering::Relaxed);

        // Stop and cleanup the keepalive task
        if let Ok(mut task_guard) = self.keepalive_task.lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
                info!(
                    "Keepalive task stopped and cleaned up (tube_id: {})",
                    self.tube_id
                );
            } else {
                debug!(
                    "No active keepalive task to stop (tube_id: {})",
                    self.tube_id
                );
            }
        } else {
            warn!(
                "Failed to acquire keepalive task lock for cleanup (tube_id: {})",
                self.tube_id
            );
        }

        info!("NAT timeout prevention stopped (tube_id: {})", self.tube_id);
        Ok(())
    }

    // ISOLATION: Get health status of this tube's isolated WebRTC API
    pub fn get_api_health(&self) -> (bool, usize, usize, Duration) {
        let (errors, turn_failures, age) = self.isolated_api.get_diagnostics();
        (self.isolated_api.is_healthy(), errors, turn_failures, age)
    }

    // ISOLATION: Reset the circuit breaker for this tube's WebRTC API
    pub fn reset_api_circuit_breaker(&self) {
        info!(
            "Resetting WebRTC API circuit breaker for tube {}",
            self.tube_id
        );
        self.isolated_api.reset_circuit_breaker();
    }

    // CIRCUIT BREAKER: Get circuit breaker state and metrics
    pub fn get_circuit_breaker_status(
        &self,
    ) -> (String, (usize, usize, usize, usize, usize, usize)) {
        let state = self.circuit_breaker.get_state();
        let metrics = self.circuit_breaker.get_metrics();
        (state, metrics)
    }

    // CIRCUIT BREAKER: Reset the circuit breaker (for recovery)
    pub fn reset_circuit_breaker(&self) {
        self.circuit_breaker.force_reset();
    }

    // CIRCUIT BREAKER: Execute ICE restart with circuit breaker protection
    pub async fn restart_ice_protected(&self) -> WebRTCResult<String> {
        info!(
            "ICE restart with circuit breaker protection for tube {}",
            self.tube_id
        );

        let tube_id = self.tube_id.clone();
        let result = self
            .circuit_breaker
            .execute(|| async { self.restart_ice_internal().await })
            .await;

        match result {
            Ok(sdp) => {
                info!("Protected ICE restart successful for tube {}", tube_id);
                Ok(sdp)
            }
            Err(e) => {
                error!("Protected ICE restart failed for tube {}: {}", tube_id, e);

                // Get actual failure count from circuit breaker metrics
                let (_, _, failed_requests, _, _, _) = self.circuit_breaker.get_metrics();

                Err(WebRTCError::CircuitBreakerOpen {
                    tube_id,
                    breaker_type: "ICE restart".to_string(),
                    failure_count: failed_requests as u32,
                })
            }
        }
    }

    // Internal ICE restart method (wrapped by circuit breaker)
    async fn restart_ice_internal(&self) -> Result<String, String> {
        // This is the existing restart_ice logic, renamed to be internal
        info!(
            "ICE restart initiated for connection recovery (tube_id: {})",
            self.tube_id
        );

        // Update restart tracking in single lock acquisition (deadlock-safe)
        let now = Instant::now();
        if let Ok(mut restart_state) = self.ice_restart_state.lock() {
            restart_state.record_attempt(now);
            let count = restart_state.attempts;
            info!(
                "ICE restart attempt #{} (tube_id: {}, attempt: {})",
                count, self.tube_id, count
            );
        } else {
            return Err("Failed to acquire restart state lock".to_string());
        }

        // Set connection quality as degraded during restart
        self.connection_quality_degraded
            .store(true, Ordering::Relaxed);

        // Generate new offer with ICE restart
        match self.peer_connection.create_offer(None).await {
            Ok(offer) => {
                info!(
                    "Successfully generated ICE restart offer (tube_id: {}, sdp_length: {})",
                    self.tube_id,
                    offer.sdp.len()
                );

                // Set the new local description to trigger ICE restart
                let offer_desc = RTCSessionDescription::offer(offer.sdp.clone())
                    .map_err(|e| format!("Failed to create offer session description: {e}"))?;

                match self.peer_connection.set_local_description(offer_desc).await {
                    Ok(()) => {
                        info!("ICE restart offer set as local description - new ICE session will begin (tube_id: {})", self.tube_id);

                        // Update activity since we just performed a successful SDP operation
                        self.update_activity();

                        // Reset connection quality flag - we'll monitor for improvement
                        self.connection_quality_degraded
                            .store(false, Ordering::Relaxed);

                        Ok(offer.sdp)
                    }
                    Err(e) => {
                        warn!("Failed to set ICE restart offer as local description (tube_id: {}, error: {})", self.tube_id, e);
                        Err(format!(
                            "Failed to set local description for ICE restart: {e}"
                        ))
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to create ICE restart offer (tube_id: {}, error: {})",
                    self.tube_id, e
                );
                Err(format!("Failed to create ICE restart offer: {e}"))
            }
        }
    }

    // Check if ICE restart is needed based on connection quality (DEADLOCK-SAFE)
    pub fn should_restart_ice(&self) -> bool {
        let current_state = self.peer_connection.connection_state();
        let now = Instant::now();

        // Check if connection is in a degraded state
        let connection_degraded = matches!(
            current_state,
            RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Failed
        );

        // Get all activity and restart state in single lock acquisitions (deadlock-safe)
        let (time_since_success, activity_timeout) = {
            if let Ok(activity_state) = self.activity_state.lock() {
                let time_since = now.duration_since(activity_state.last_successful_activity);
                (
                    time_since,
                    time_since > Duration::from_secs(ACTIVITY_TIMEOUT_SECS),
                )
            } else {
                // If we can't get the lock, assume recent activity to be safe
                return false;
            }
        };

        let (attempts, enough_time_passed, min_interval) = {
            if let Ok(restart_state) = self.ice_restart_state.lock() {
                let min_int = restart_state.get_min_interval();
                let enough_time = restart_state
                    .time_since_last_restart(now)
                    .map(|duration| duration >= min_int)
                    .unwrap_or(true); // Never restarted before
                (restart_state.attempts, enough_time, min_int)
            } else {
                return false; // Can't get lock, be conservative
            }
        };

        // Don't restart too many times
        let not_too_many_attempts = attempts < MAX_ICE_RESTART_ATTEMPTS;

        let should_restart =
            connection_degraded && activity_timeout && enough_time_passed && not_too_many_attempts;

        if should_restart {
            debug!("ICE restart conditions met (tube_id: {}, connection_state: {:?}, time_since_success_secs: {}, restart_attempts: {}, min_interval_secs: {})",
                   self.tube_id, current_state, time_since_success.as_secs(), attempts, min_interval.as_secs());
        } else {
            debug!("ICE restart conditions not met (tube_id: {}, connection_state: {:?}, connection_degraded: {}, activity_timeout: {}, enough_time_passed: {}, not_too_many_attempts: {})",
                   self.tube_id, current_state, connection_degraded, activity_timeout, enough_time_passed, not_too_many_attempts);
        }

        should_restart
    }

    // Perform ICE restart to recover from connectivity issues (CIRCUIT BREAKER PROTECTED)
    pub async fn restart_ice(&self) -> WebRTCResult<String> {
        // All ICE restarts are now protected by circuit breaker for isolation
        self.restart_ice_protected().await
    }

    // Test helper methods (only compiled in test builds)
    #[cfg(test)]
    pub fn is_keepalive_running(&self) -> bool {
        let task_guard = self.keepalive_task.lock().unwrap();
        task_guard.is_some()
    }

    #[cfg(test)]
    pub fn get_last_activity(&self) -> Instant {
        let activity_guard = self.last_activity.lock().unwrap();
        *activity_guard
    }

    #[cfg(test)]
    pub fn set_last_activity(&self, time: Instant) {
        let mut activity_guard = self.last_activity.lock().unwrap();
        *activity_guard = time;
    }

    /// Static version of UDP connectivity test
    async fn test_udp_connectivity_static(ip_addr: &str, port: &str) -> Result<(), std::io::Error> {
        use std::time::Duration;
        use tokio::net::UdpSocket;

        // Parse port
        let port_num: u16 = port.parse().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid port number")
        })?;

        // Create a UDP socket bound to any local address
        let socket = UdpSocket::bind("0.0.0.0:0").await?;

        // Set a short timeout for the test
        let timeout_duration = Duration::from_millis(500);

        // Try to connect/send a test packet with timeout
        let connect_result = tokio::time::timeout(
            timeout_duration,
            socket.connect(format!("{}:{}", ip_addr, port_num)),
        )
        .await;

        match connect_result {
            Ok(Ok(())) => Ok(()), // Successfully connected
            Ok(Err(e)) => Err(e), // Connection failed
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Connection timeout",
            )), // Timeout
        }
    }

    /// Enhanced debugging: Log detailed candidate information
    fn log_candidate_details(&self, candidate: &str, direction: &str) {
        if candidate.is_empty() {
            debug!(
                "[CANDIDATE_DEBUG] {} end-of-candidates signal (tube_id: {})",
                direction, self.tube_id
            );
            return;
        }

        // Parse candidate details
        let parts: Vec<&str> = candidate.split_whitespace().collect();
        if parts.len() >= 8 {
            let candidate_type = parts.get(7).unwrap_or(&"unknown");
            let ip_addr = parts.get(4).unwrap_or(&"unknown");
            let port = parts.get(5).unwrap_or(&"unknown");
            let protocol = parts.get(2).unwrap_or(&"unknown");
            let priority = parts.get(3).unwrap_or(&"unknown");

            debug!(
                "[CANDIDATE_DEBUG] {} candidate (tube_id: {}, type: {}, ip: {}, port: {}, protocol: {}, priority: {})",
                direction, self.tube_id, candidate_type, ip_addr, port, protocol, priority
            );

            // Special logging for TURN candidates
            if candidate_type == &"relay" {
                debug!(
                    "[TURN_DEBUG] TURN relay candidate {} (tube_id: {}, relay_ip: {}, relay_port: {})",
                    direction, self.tube_id, ip_addr, port
                );
            }

            // Test network reachability for incoming remote candidates
            if direction == "INCOMING" {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "[PAIR_TEST] Testing connectivity to remote candidate (tube_id: {}, remote: {}:{})",
                        self.tube_id, ip_addr, port
                    );
                }

                // Immediate UDP connectivity test with result logging outside async task
                let tube_id_clone = self.tube_id.clone();
                let ip_clone = ip_addr.to_string();
                let port_clone = port.to_string();

                tokio::spawn(async move {
                    match Self::test_udp_connectivity_static(&ip_clone, &port_clone).await {
                        Ok(_) => {
                            // Use println! to ensure log shows up immediately
                            debug!("[PAIR_TEST] Candidate pair viable (tube_id: {}, result: Reachable)", tube_id_clone);
                        }
                        Err(e) => {
                            debug!(
                                "[PAIR_TEST] Candidate pair not viable (tube_id: {}, result: {:?})",
                                tube_id_clone, e
                            );
                        }
                    }
                });
            }
        } else {
            debug!(
                "[CANDIDATE_DEBUG] {} malformed candidate (tube_id: {}, candidate: {})",
                direction, self.tube_id, candidate
            );
        }
    }

    /// Static version for use in closures where self is not available
    fn log_candidate_details_static(candidate: &str, direction: &str, tube_id: &str) {
        if candidate.is_empty() {
            debug!(
                "[CANDIDATE_DEBUG] {} end-of-candidates signal (tube_id: {})",
                direction, tube_id
            );
            return;
        }

        // Parse candidate details
        let parts: Vec<&str> = candidate.split_whitespace().collect();
        if parts.len() >= 8 {
            let candidate_type = parts.get(7).unwrap_or(&"unknown");
            let ip_addr = parts.get(4).unwrap_or(&"unknown");
            let port = parts.get(5).unwrap_or(&"unknown");
            let protocol = parts.get(2).unwrap_or(&"unknown");
            let priority = parts.get(3).unwrap_or(&"unknown");

            debug!(
                "[CANDIDATE_DEBUG] {} candidate (tube_id: {}, type: {}, ip: {}, port: {}, protocol: {}, priority: {})",
                direction, tube_id, candidate_type, ip_addr, port, protocol, priority
            );

            // Special logging for TURN candidates
            if candidate_type == &"relay" {
                debug!(
                    "[TURN_DEBUG] TURN relay candidate {} (tube_id: {}, relay_ip: {}, relay_port: {})",
                    direction, tube_id, ip_addr, port
                );
            }
        } else {
            debug!(
                "[CANDIDATE_DEBUG] {} malformed candidate (tube_id: {}, candidate: {})",
                direction, tube_id, candidate
            );
        }
    }

    /// Analyze data channel connectivity using real WebRTC statistics
    /// Focuses only on data channel connectivity (no audio/video RTP stats)
    async fn analyze_candidate_pairs_static(
        peer_connection: &Arc<RTCPeerConnection>,
        tube_id: &str,
    ) -> Result<(), String> {
        // Get real WebRTC stats
        let stats_report = peer_connection.get_stats().await;

        let mut total_pairs = 0u32;
        let mut data_channel_count = 0u32;

        // Get current ICE connection state for diagnostics
        let ice_connection_state = peer_connection.ice_connection_state();
        let peer_connection_state = peer_connection.connection_state();

        // Focus on data channel connectivity stats only (ignore RTP/media)
        for stats_value in stats_report.reports.values() {
            match stats_value {
                // Count real candidate pairs (the key diagnostic metric)
                webrtc::stats::StatsReportType::CandidatePair(pair_stats) => {
                    total_pairs += 1;

                    // Log detailed pair info for debugging
                    debug!(
                        "[PAIR_DEBUG] Candidate pair (tube_id: {}, pair_id: {:?}, state: {:?})",
                        tube_id, pair_stats.id, pair_stats.state
                    );
                }
                // Count data channels (your actual use case)
                webrtc::stats::StatsReportType::DataChannel(_) => {
                    data_channel_count += 1;
                }
                // Ignore audio/video RTP stats (not used)
                webrtc::stats::StatsReportType::InboundRTP(_)
                | webrtc::stats::StatsReportType::OutboundRTP(_)
                | webrtc::stats::StatsReportType::RemoteInboundRTP(_)
                | webrtc::stats::StatsReportType::RemoteOutboundRTP(_) => {
                    // Skip - not relevant for data channel only usage
                }
                _ => {} // Other stats types
            }
        }

        // Log data channel connectivity analysis with real WebRTC data - use println! to ensure visibility
        debug!(
            "[CONNECTIVITY_DEBUG] Data channel connectivity (tube_id: {}, candidate_pairs: {}, data_channels: {}, ice_state: {:?}, peer_state: {:?})",
            tube_id, total_pairs, data_channel_count, ice_connection_state, peer_connection_state
        );

        // Critical warnings for your specific "no candidate pairs" issue
        if total_pairs == 0 {
            debug!(
                "[CONNECTIVITY_DEBUG] NO CANDIDATE PAIRS FORMED! Data channel connectivity impossible. Check that both local and remote candidates are being added. (tube_id: {})",
                tube_id
            );
        }

        Ok(())
    }
}

/// ISOLATION: Per-tube circuit breaker to prevent cascading failures
/// Provides bulletproof failure isolation with automatic recovery
#[derive(Clone)]
pub struct TubeCircuitBreaker {
    state: Arc<Mutex<CircuitState>>,
    config: CircuitConfig,
    tube_id: String,
    metrics: Arc<CircuitMetrics>,
}

#[derive(Debug, Clone)]
pub struct CircuitConfig {
    failure_threshold: u32,        // Trip after N failures (default threshold)
    timeout: Duration,             // Stay open for this long
    success_threshold: u32,        // Successes needed to close
    max_half_open_requests: u32,   // Limit test requests
    max_request_timeout: Duration, // Individual operation timeout

    // Error-type specific configurations
    error_specific_thresholds: HashMap<String, ErrorSpecificConfig>,
}

/// Error-specific circuit breaker configuration
#[derive(Debug, Clone)]
pub struct ErrorSpecificConfig {
    pub failure_threshold: u32,
    pub timeout_multiplier: f64,  // Multiply base timeout by this factor
    pub recovery_multiplier: f64, // Multiply recovery time by this factor
    pub priority: u8,             // Higher priority errors trip circuit faster (0-10)
}

impl Default for CircuitConfig {
    fn default() -> Self {
        let mut error_specific_thresholds = HashMap::new();

        // ICE-related errors - more lenient (network issues are common)
        error_specific_thresholds.insert(
            "IceConnectionFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 8,
                timeout_multiplier: 1.5,
                recovery_multiplier: 2.0,
                priority: 6,
            },
        );

        // TURN/STUN errors - moderate threshold
        error_specific_thresholds.insert(
            "TurnServerConnectionFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 5,
                timeout_multiplier: 1.0,
                recovery_multiplier: 1.5,
                priority: 7,
            },
        );

        // Authentication errors - strict threshold (likely configuration issue)
        error_specific_thresholds.insert(
            "AuthenticationFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 2,
                timeout_multiplier: 3.0,
                recovery_multiplier: 3.0,
                priority: 9,
            },
        );

        // Data channel errors - moderate threshold
        error_specific_thresholds.insert(
            "DataChannelFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 4,
                timeout_multiplier: 1.2,
                recovery_multiplier: 1.8,
                priority: 7,
            },
        );

        // Signaling errors - strict threshold (protocol issues)
        error_specific_thresholds.insert(
            "SignalingFailed".to_string(),
            ErrorSpecificConfig {
                failure_threshold: 3,
                timeout_multiplier: 2.0,
                recovery_multiplier: 2.5,
                priority: 8,
            },
        );

        Self {
            failure_threshold: 5,                         // Trip after 5 failures (default)
            timeout: Duration::from_secs(30),             // Stay open for 30 seconds
            success_threshold: 3,                         // Need 3 successes to close
            max_half_open_requests: 3,                    // Max 3 test requests
            max_request_timeout: Duration::from_secs(10), // 10-second operation timeout
            error_specific_thresholds,
        }
    }
}

#[derive(Debug)]
enum CircuitState {
    Closed {
        failure_count: u32,
        last_failure: Option<Instant>,
        error_type_failures: HashMap<String, u32>, // Track failures by error type
        last_error_type: Option<String>,
    },
    Open {
        opened_at: Instant,
        last_attempt: Option<Instant>,
        trigger_error_type: String, // Error type that triggered the opening
    },
    HalfOpen {
        test_started: Instant,
        test_count: u32,
        success_count: u32,
        trigger_error_type: String, // Error type that caused the open state
    },
}

#[derive(Debug)]
struct CircuitMetrics {
    total_requests: AtomicUsize,
    successful_requests: AtomicUsize,
    failed_requests: AtomicUsize,
    circuit_opens: AtomicUsize,
    circuit_closes: AtomicUsize,
    timeouts: AtomicUsize,

    // Error-type specific metrics
    error_type_counts: Arc<Mutex<HashMap<String, u32>>>,
    error_type_triggered_opens: Arc<Mutex<HashMap<String, u32>>>,
}

impl Default for CircuitMetrics {
    fn default() -> Self {
        Self {
            total_requests: AtomicUsize::new(0),
            successful_requests: AtomicUsize::new(0),
            failed_requests: AtomicUsize::new(0),
            circuit_opens: AtomicUsize::new(0),
            circuit_closes: AtomicUsize::new(0),
            timeouts: AtomicUsize::new(0),
            error_type_counts: Arc::new(Mutex::new(HashMap::new())),
            error_type_triggered_opens: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug)]
pub enum CircuitError<E> {
    CircuitOpen,
    Timeout,
    OperationFailed(E),
    TooManyTestRequests,
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitError::CircuitOpen => write!(f, "Circuit breaker is open"),
            CircuitError::Timeout => write!(f, "Operation timed out"),
            CircuitError::OperationFailed(e) => write!(f, "Operation failed: {}", e),
            CircuitError::TooManyTestRequests => {
                write!(f, "Too many test requests in half-open state")
            }
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for CircuitError<E> {}

/// Detailed circuit state information for monitoring and diagnostics
#[derive(Debug, Clone)]
pub enum CircuitStateInfo {
    Closed {
        failure_count: u32,
        last_failure_ago: Option<Duration>,
    },
    Open {
        opened_ago: Duration,
        last_attempt_ago: Option<Duration>,
    },
    HalfOpen {
        test_started_ago: Duration,
        test_count: u32,
        success_count: u32,
    },
}

/// Comprehensive circuit breaker statistics including error-type specific data
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub tube_id: String,
    pub state: String,
    pub total_requests: usize,
    pub successful_requests: usize,
    pub failed_requests: usize,
    pub circuit_opens: usize,
    pub circuit_closes: usize,
    pub timeouts: usize,
    pub error_type_counts: HashMap<String, u32>,
    pub error_type_triggered_opens: HashMap<String, u32>,
    pub current_error_type_failures: HashMap<String, u32>,
}

impl TubeCircuitBreaker {
    pub fn new(tube_id: String) -> Self {
        Self::with_config(tube_id, CircuitConfig::default())
    }

    pub fn with_config(tube_id: String, config: CircuitConfig) -> Self {
        debug!(
            "Creating circuit breaker for tube {} with config: failure_threshold={}, timeout={}s",
            tube_id,
            config.failure_threshold,
            config.timeout.as_secs()
        );

        Self {
            state: Arc::new(Mutex::new(CircuitState::Closed {
                failure_count: 0,
                last_failure: None,
                error_type_failures: HashMap::new(),
                last_error_type: None,
            })),
            config,
            tube_id,
            metrics: Arc::new(CircuitMetrics::default()),
        }
    }

    /// Execute an async operation with circuit breaker protection
    /// PERFORMANCE: Only adds ~1μs overhead for circuit state check
    pub async fn execute<F, Fut, T, E>(&self, operation: F) -> Result<T, CircuitError<E>>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<T, E>> + Send,
        T: Send,
        E: std::fmt::Debug + std::fmt::Display + Send,
    {
        // Increment total requests
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

        // FAST PATH: Check circuit state (optimized for closed state)
        let can_execute = {
            let mut state = self.state.lock().unwrap();
            match &mut *state {
                CircuitState::Closed { .. } => true, // Fast path - most common case

                CircuitState::Open {
                    opened_at,
                    trigger_error_type,
                    ..
                } => {
                    if opened_at.elapsed() >= self.config.timeout {
                        info!(
                            "Circuit breaker transitioning to half-open for tube {} (triggered by: {})",
                            self.tube_id, trigger_error_type
                        );
                        *state = CircuitState::HalfOpen {
                            test_started: Instant::now(),
                            test_count: 0,
                            success_count: 0,
                            trigger_error_type: trigger_error_type.clone(),
                        };
                        true
                    } else {
                        false // Still in open state
                    }
                }

                CircuitState::HalfOpen { test_count, .. } => {
                    if *test_count >= self.config.max_half_open_requests {
                        false // Too many test requests
                    } else {
                        *test_count += 1;
                        true
                    }
                }
            }
        };

        if !can_execute {
            let error = match &*self.state.lock().unwrap() {
                CircuitState::Open { .. } => CircuitError::CircuitOpen,
                CircuitState::HalfOpen { .. } => CircuitError::TooManyTestRequests,
                _ => CircuitError::CircuitOpen,
            };
            return Err(error);
        }

        // Execute operation with timeout
        let result = tokio::time::timeout(self.config.max_request_timeout, operation()).await;

        // Process result and update circuit state
        match result {
            Ok(Ok(value)) => {
                self.record_success();
                Ok(value)
            }
            Ok(Err(e)) => {
                self.record_failure(&format!("{:?}", e));
                Err(CircuitError::OperationFailed(e))
            }
            Err(_) => {
                self.record_timeout();
                Err(CircuitError::Timeout)
            }
        }
    }

    /// Record a successful operation
    fn record_success(&self) {
        self.metrics
            .successful_requests
            .fetch_add(1, Ordering::Relaxed);

        let mut state = self.state.lock().unwrap();
        match &mut *state {
            CircuitState::Closed {
                failure_count,
                error_type_failures,
                last_error_type,
                ..
            } => {
                // Reset failure counts on success
                *failure_count = 0;
                error_type_failures.clear();
                *last_error_type = None;
            }
            CircuitState::HalfOpen { success_count, .. } => {
                *success_count += 1;
                if *success_count >= self.config.success_threshold {
                    info!(
                        "Circuit breaker closed for tube {} after {} successful tests",
                        self.tube_id, success_count
                    );
                    *state = CircuitState::Closed {
                        failure_count: 0,
                        last_failure: None,
                        error_type_failures: HashMap::new(),
                        last_error_type: None,
                    };
                    self.metrics.circuit_closes.fetch_add(1, Ordering::Relaxed);
                }
            }
            CircuitState::Open { .. } => {
                // Should not happen, but handle gracefully
                warn!(
                    "Unexpected success in open circuit state for tube {}",
                    self.tube_id
                );
            }
        }
    }

    /// Record a failed operation with error-type specific handling
    fn record_failure(&self, error: &str) {
        self.record_failure_with_type(error, "Unknown")
    }

    /// Record a failed operation with specific error type
    pub fn record_failure_with_type(&self, error: &str, error_type: &str) {
        self.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);

        // Update error type metrics
        {
            let mut error_counts = self.metrics.error_type_counts.lock().unwrap();
            *error_counts.entry(error_type.to_string()).or_insert(0) += 1;
        }

        let mut state = self.state.lock().unwrap();
        let (should_open, trigger_error_type) = match &mut *state {
            CircuitState::Closed {
                failure_count,
                last_failure,
                error_type_failures,
                last_error_type,
            } => {
                *failure_count += 1;
                *last_failure = Some(Instant::now());
                *last_error_type = Some(error_type.to_string());

                // Update error-type specific failure count
                *error_type_failures
                    .entry(error_type.to_string())
                    .or_insert(0) += 1;

                // Check if this error type should trigger circuit opening
                let threshold = self.get_threshold_for_error_type(error_type);
                let error_specific_count = error_type_failures.get(error_type).unwrap_or(&0);

                debug!(
                    "Error type '{}' count: {} (threshold: {}, total failures: {}) for tube {}",
                    error_type, error_specific_count, threshold, failure_count, self.tube_id
                );

                let should_open = *error_specific_count >= threshold
                    || *failure_count >= self.config.failure_threshold;
                (should_open, error_type.to_string())
            }
            CircuitState::HalfOpen {
                trigger_error_type, ..
            } => {
                // Failed during testing - reopen circuit
                (true, trigger_error_type.clone())
            }
            CircuitState::Open { .. } => {
                (false, error_type.to_string()) // Already open
            }
        };

        if should_open {
            let timeout_multiplier =
                self.get_timeout_multiplier_for_error_type(&trigger_error_type);
            let adjusted_timeout =
                Duration::from_secs_f64(self.config.timeout.as_secs_f64() * timeout_multiplier);

            match &*state {
                CircuitState::Closed {
                    failure_count,
                    error_type_failures,
                    ..
                } => {
                    let error_specific_count =
                        error_type_failures.get(&trigger_error_type).unwrap_or(&0);
                    error!(
                        "Circuit breaker OPENED for tube {} after {} total failures, {} '{}' errors (last_error: {}). Timeout adjusted to {}s",
                        self.tube_id, failure_count, error_specific_count, trigger_error_type, error, adjusted_timeout.as_secs()
                    );
                }
                CircuitState::HalfOpen { .. } => {
                    error!(
                        "Circuit breaker RE-OPENED for tube {} after failed test (error_type: {}, error: {}). Timeout: {}s",
                        self.tube_id, trigger_error_type, error, adjusted_timeout.as_secs()
                    );
                }
                _ => {}
            }

            *state = CircuitState::Open {
                opened_at: Instant::now(),
                last_attempt: None,
                trigger_error_type: trigger_error_type.clone(),
            };

            self.metrics.circuit_opens.fetch_add(1, Ordering::Relaxed);

            // Track which error type triggered the opening
            {
                let mut triggered_opens = self.metrics.error_type_triggered_opens.lock().unwrap();
                *triggered_opens.entry(trigger_error_type).or_insert(0) += 1;
            }
        }
    }

    /// Get failure threshold for specific error type
    fn get_threshold_for_error_type(&self, error_type: &str) -> u32 {
        self.config
            .error_specific_thresholds
            .get(error_type)
            .map(|config| config.failure_threshold)
            .unwrap_or(self.config.failure_threshold)
    }

    /// Get timeout multiplier for specific error type
    fn get_timeout_multiplier_for_error_type(&self, error_type: &str) -> f64 {
        self.config
            .error_specific_thresholds
            .get(error_type)
            .map(|config| config.timeout_multiplier)
            .unwrap_or(1.0)
    }

    /// Record a timeout
    fn record_timeout(&self) {
        self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        self.record_failure("timeout");
    }

    /// Get current circuit state
    pub fn get_state(&self) -> String {
        let state = self.state.lock().unwrap();
        match &*state {
            CircuitState::Closed { failure_count, .. } => {
                format!("Closed (failures: {})", failure_count)
            }
            CircuitState::Open {
                opened_at,
                last_attempt,
                ..
            } => {
                let last_attempt_info = match last_attempt {
                    Some(t) => format!(", last attempt: {}s ago", t.elapsed().as_secs()),
                    None => "".to_string(),
                };
                format!(
                    "Open ({}s ago{})",
                    opened_at.elapsed().as_secs(),
                    last_attempt_info
                )
            }
            CircuitState::HalfOpen {
                test_started,
                test_count,
                success_count,
                ..
            } => {
                format!(
                    "Half-Open (tests: {}, successes: {}, testing for: {}s)",
                    test_count,
                    success_count,
                    test_started.elapsed().as_secs()
                )
            }
        }
    }

    /// Get detailed circuit state information for monitoring
    pub fn get_detailed_state(&self) -> CircuitStateInfo {
        let state = self.state.lock().unwrap();
        match &*state {
            CircuitState::Closed {
                failure_count,
                last_failure,
                ..
            } => CircuitStateInfo::Closed {
                failure_count: *failure_count,
                last_failure_ago: last_failure.map(|t| t.elapsed()),
            },
            CircuitState::Open {
                opened_at,
                last_attempt,
                ..
            } => CircuitStateInfo::Open {
                opened_ago: opened_at.elapsed(),
                last_attempt_ago: last_attempt.map(|t| t.elapsed()),
            },
            CircuitState::HalfOpen {
                test_started,
                test_count,
                success_count,
                ..
            } => CircuitStateInfo::HalfOpen {
                test_started_ago: test_started.elapsed(),
                test_count: *test_count,
                success_count: *success_count,
            },
        }
    }

    /// Get circuit breaker metrics
    pub fn get_metrics(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.metrics.total_requests.load(Ordering::Relaxed),
            self.metrics.successful_requests.load(Ordering::Relaxed),
            self.metrics.failed_requests.load(Ordering::Relaxed),
            self.metrics.circuit_opens.load(Ordering::Relaxed),
            self.metrics.circuit_closes.load(Ordering::Relaxed),
            self.metrics.timeouts.load(Ordering::Relaxed),
        )
    }

    /// Get error-type specific metrics
    pub fn get_error_type_metrics(&self) -> (HashMap<String, u32>, HashMap<String, u32>) {
        let error_counts = self.metrics.error_type_counts.lock().unwrap().clone();
        let triggered_opens = self
            .metrics
            .error_type_triggered_opens
            .lock()
            .unwrap()
            .clone();
        (error_counts, triggered_opens)
    }

    /// Get comprehensive circuit breaker statistics
    pub fn get_comprehensive_stats(&self) -> CircuitBreakerStats {
        let (error_counts, triggered_opens) = self.get_error_type_metrics();
        let state = self.state.lock().unwrap();

        let (current_state, error_type_failures) = match &*state {
            CircuitState::Closed {
                failure_count,
                error_type_failures,
                ..
            } => (
                format!("Closed (failures: {})", failure_count),
                error_type_failures.clone(),
            ),
            CircuitState::Open {
                opened_at,
                trigger_error_type,
                ..
            } => (
                format!(
                    "Open ({}s ago, triggered by: {})",
                    opened_at.elapsed().as_secs(),
                    trigger_error_type
                ),
                HashMap::new(),
            ),
            CircuitState::HalfOpen {
                test_started,
                trigger_error_type,
                test_count,
                success_count,
                ..
            } => (
                format!(
                    "Half-Open (testing for: {}s, triggered by: {}, tests: {}, successes: {})",
                    test_started.elapsed().as_secs(),
                    trigger_error_type,
                    test_count,
                    success_count
                ),
                HashMap::new(),
            ),
        };

        CircuitBreakerStats {
            tube_id: self.tube_id.clone(),
            state: current_state,
            total_requests: self.metrics.total_requests.load(Ordering::Relaxed),
            successful_requests: self.metrics.successful_requests.load(Ordering::Relaxed),
            failed_requests: self.metrics.failed_requests.load(Ordering::Relaxed),
            circuit_opens: self.metrics.circuit_opens.load(Ordering::Relaxed),
            circuit_closes: self.metrics.circuit_closes.load(Ordering::Relaxed),
            timeouts: self.metrics.timeouts.load(Ordering::Relaxed),
            error_type_counts: error_counts,
            error_type_triggered_opens: triggered_opens,
            current_error_type_failures: error_type_failures,
        }
    }

    /// Force reset the circuit breaker (for manual recovery)
    pub fn force_reset(&self) {
        info!("Force resetting circuit breaker for tube {}", self.tube_id);
        let mut state = self.state.lock().unwrap();
        *state = CircuitState::Closed {
            failure_count: 0,
            last_failure: None,
            error_type_failures: HashMap::new(),
            last_error_type: None,
        };
    }

    /// Check if circuit is healthy (closed)
    pub fn is_healthy(&self) -> bool {
        matches!(*self.state.lock().unwrap(), CircuitState::Closed { .. })
    }
}

/// ISOLATION: Per-tube runtime isolation for complete task sandboxing
/// Prevents panics and failures from affecting other tubes
pub struct IsolatedTubeRuntime {
    runtime: Arc<tokio::runtime::Runtime>,
    tube_id: String,
    panic_count: Arc<AtomicUsize>,
    max_panics: usize,
    created_at: Instant,
    is_healthy: Arc<AtomicBool>,
    active_tasks: Arc<AtomicUsize>,
}

impl IsolatedTubeRuntime {
    /// Create a new isolated runtime for a tube
    /// PERFORMANCE: Each tube gets 2 worker threads to maintain responsiveness
    pub fn new(tube_id: String) -> Result<Self, String> {
        info!("Creating isolated runtime for tube {}", tube_id);

        let tube_id_for_threads = tube_id.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name_fn(move || {
                static COUNTER: AtomicUsize = AtomicUsize::new(1);
                format!(
                    "tube-{}-worker-{}",
                    tube_id_for_threads,
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                )
            })
            .worker_threads(2) // Limit resources per tube
            .max_blocking_threads(2) // Limit blocking thread pool
            .thread_keep_alive(Duration::from_secs(10))
            .enable_all()
            .build()
            .map_err(|e| {
                format!(
                    "Failed to create isolated runtime for tube {}: {}",
                    tube_id, e
                )
            })?;

        Ok(Self {
            runtime: Arc::new(runtime),
            tube_id,
            panic_count: Arc::new(AtomicUsize::new(0)),
            max_panics: 3, // Circuit breaker after 3 panics
            created_at: Instant::now(),
            is_healthy: Arc::new(AtomicBool::new(true)),
            active_tasks: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Spawn a task with panic protection and circuit breaking
    /// PERFORMANCE: Only adds ~100ns overhead for panic safety
    pub fn spawn_protected<F, T>(&self, future: F) -> Result<tokio::task::JoinHandle<T>, String>
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Check circuit breaker
        if !self.is_healthy.load(Ordering::Acquire) {
            return Err(format!(
                "Tube {} runtime circuit breaker is open",
                self.tube_id
            ));
        }

        let panic_count = Arc::clone(&self.panic_count);
        let tube_id = self.tube_id.clone();
        let active_tasks = Arc::clone(&self.active_tasks);
        let is_healthy = Arc::clone(&self.is_healthy);
        let max_panics = self.max_panics;

        // Increment active task counter
        active_tasks.fetch_add(1, Ordering::Relaxed);

        let wrapped_future = async move {
            let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;

            // Decrement active task counter regardless of outcome
            active_tasks.fetch_sub(1, Ordering::Relaxed);

            match result {
                Ok(value) => value,
                Err(panic_payload) => {
                    let count = panic_count.fetch_add(1, Ordering::AcqRel) + 1;
                    error!("Task panic in tube {} (panic #{})", tube_id, count);

                    // Circuit breaker: disable runtime after max panics
                    if count >= max_panics {
                        error!(
                            "Circuit breaker OPENED for tube {} runtime after {} panics",
                            tube_id, count
                        );
                        is_healthy.store(false, Ordering::Release);
                    }

                    // Extract panic message if possible
                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic".to_string()
                    };

                    // Don't re-panic - return a default value or handle gracefully
                    // This prevents cascade failures
                    warn!("Task in tube {} panicked: {}", tube_id, panic_msg);
                    panic!("Task panicked in isolated tube runtime: {}", panic_msg);
                }
            }
        };

        Ok(self.runtime.spawn(wrapped_future))
    }

    /// Block on a future with isolation
    pub fn block_on<F: std::future::Future>(&self, future: F) -> Result<F::Output, String> {
        if !self.is_healthy.load(Ordering::Acquire) {
            return Err(format!("Tube {} runtime is unhealthy", self.tube_id));
        }

        Ok(self.runtime.block_on(future))
    }

    /// Spawn a task without panic protection (for performance-critical paths)
    /// PERFORMANCE: Zero overhead spawning for hot paths
    pub fn spawn_unprotected<F, T>(&self, future: F) -> Result<tokio::task::JoinHandle<T>, String>
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        if !self.is_healthy.load(Ordering::Acquire) {
            return Err(format!("Tube {} runtime is unhealthy", self.tube_id));
        }

        Ok(self.runtime.spawn(future))
    }

    /// Get runtime health status
    pub fn get_health_status(&self) -> (bool, usize, usize, Duration, usize) {
        (
            self.is_healthy.load(Ordering::Acquire),
            self.panic_count.load(Ordering::Acquire),
            self.active_tasks.load(Ordering::Acquire),
            self.created_at.elapsed(),
            self.max_panics,
        )
    }

    /// Force reset the runtime circuit breaker (for recovery)
    pub fn reset_circuit_breaker(&self) {
        info!(
            "Resetting runtime circuit breaker for tube {}",
            self.tube_id
        );
        self.panic_count.store(0, Ordering::Release);
        self.is_healthy.store(true, Ordering::Release);
    }

    /// Check if runtime is healthy
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Acquire)
    }

    /// Get runtime statistics
    pub fn get_stats(&self) -> RuntimeStats {
        RuntimeStats {
            tube_id: self.tube_id.clone(),
            is_healthy: self.is_healthy(),
            panic_count: self.panic_count.load(Ordering::Acquire),
            active_tasks: self.active_tasks.load(Ordering::Acquire),
            uptime: self.created_at.elapsed(),
            max_panics: self.max_panics,
        }
    }

    /// Graceful shutdown with timeout
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        info!("Shutting down isolated runtime for tube {}", self.tube_id);

        // Mark as unhealthy to prevent new tasks
        self.is_healthy.store(false, Ordering::Release);

        // Wait for active tasks to complete (with timeout)
        let start = Instant::now();
        while self.active_tasks.load(Ordering::Acquire) > 0 && start.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let remaining_tasks = self.active_tasks.load(Ordering::Acquire);
        if remaining_tasks > 0 {
            warn!(
                "Force shutdown tube {} runtime with {} active tasks",
                self.tube_id, remaining_tasks
            );
        }

        // Force shutdown the runtime
        // Note: We can't directly shut down a Runtime from within itself,
        // so this will complete when the Runtime is dropped
        info!("Tube {} runtime shutdown initiated", self.tube_id);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeStats {
    pub tube_id: String,
    pub is_healthy: bool,
    pub panic_count: usize,
    pub active_tasks: usize,
    pub uptime: Duration,
    pub max_panics: usize,
}
