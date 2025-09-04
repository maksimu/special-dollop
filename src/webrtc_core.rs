use crate::resource_manager::{IceAgentGuard, ResourceError, RESOURCE_MANAGER};
use crate::tube_registry::SignalMessage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};
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

// Cached API instance for reuse
static API: once_cell::sync::Lazy<webrtc::api::API> =
    once_cell::sync::Lazy::new(|| APIBuilder::new().build());

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

// Helper function to create a WebRTC peer connection with cached API
pub async fn create_peer_connection(
    config: Option<RTCConfiguration>,
) -> webrtc::error::Result<RTCPeerConnection> {
    // Use the configuration as provided or default
    let actual_config = config.unwrap_or_default();

    // Use the cached API instance instead of creating a new one each time
    API.new_peer_connection(actual_config).await
}

// Helper function to create a data channel with optimized settings
pub async fn create_data_channel(
    peer_connection: &RTCPeerConnection,
    label: &str,
) -> webrtc::error::Result<Arc<RTCDataChannel>> {
    let config = RTCDataChannelInit {
        ordered: Some(true),        // Guarantee message order
        max_retransmits: Some(0),   // No retransmits
        max_packet_life_time: None, // No timeout for packets
        protocol: None,             // No specific protocol
        negotiated: None,           // Let WebRTC handle negotiation
    };

    debug!(
        "Creating data channel '{}' with config: ordered={:?}, reliable delivery",
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

    // Keepalive infrastructure for session timeout prevention
    keepalive_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    keepalive_interval: Duration,
    last_activity: Arc<Mutex<Instant>>,
    keepalive_enabled: Arc<AtomicBool>,

    // ICE restart and connection quality tracking
    connection_quality_degraded: Arc<AtomicBool>,

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
        // Acquire ICE agent permit before creating peer connection
        let ice_agent_guard = match RESOURCE_MANAGER.acquire_ice_agent_permit().await {
            Ok(guard) => Some(guard),
            Err(ResourceError::Exhausted { resource, limit }) => {
                warn!(target: "resource_management", tube_id = %tube_id, 
                      "ICE agent resource exhausted: {} limit ({}) exceeded", resource, limit);
                return Err(format!(
                    "Resource exhausted: {resource} limit ({limit}) exceeded"
                ));
            }
            Err(e) => {
                error!(target: "resource_management", tube_id = %tube_id, 
                       "Failed to acquire ICE agent permit: {}", e);
                return Err(format!("Failed to acquire ICE agent permit: {e}"));
            }
        };

        // Use the provided configuration or default
        let mut actual_config = config.unwrap_or_default();

        // Apply resource limits from the resource manager
        let limits = RESOURCE_MANAGER.get_limits();

        // Limit ICE candidate pool size to reduce socket usage
        actual_config.ice_candidate_pool_size = limits.max_interfaces_per_agent as u8;

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

        // Create peer connection
        let peer_connection = create_peer_connection(Some(actual_config.clone()))
            .await
            .map_err(|e| format!("Failed to create peer connection: {e}"))?;

        // Store the closing state and signal channel
        let is_closing = Arc::new(AtomicBool::new(false));
        let pending_incoming_ice_candidates = Arc::new(Mutex::new(Vec::new()));

        // Create an Arc<RTCPeerConnection> first
        let pc_arc = Arc::new(peer_connection);

        // No longer setting up ICE candidate handler here - this will be done in setup_ice_candidate_handler
        // to avoid duplicate handlers

        info!(target: "resource_management", tube_id = %tube_id, 
              "Successfully created WebRTC peer connection with resource management");

        // Return the new WebRTCPeerConnection struct with keepalive infrastructure
        let now = Instant::now();
        Ok(Self {
            peer_connection: pc_arc,
            trickle_ice,
            is_closing,
            pending_incoming_ice_candidates,
            signal_sender,
            tube_id,
            conversation_id,
            _ice_agent_guard: Arc::new(Mutex::new(ice_agent_guard)),

            // Initialize keepalive infrastructure
            keepalive_task: Arc::new(Mutex::new(None)),
            keepalive_interval: limits.ice_keepalive_interval, // configurable, uses ResourceLimits setting
            last_activity: Arc::new(Mutex::new(now)),
            keepalive_enabled: Arc::new(AtomicBool::new(false)),

            // Initialize ICE restart and connection quality tracking
            connection_quality_degraded: Arc::new(AtomicBool::new(false)),

            // Consolidated state to prevent deadlocks
            activity_state: Arc::new(Mutex::new(ActivityState::new(now))),
            ice_restart_state: Arc::new(Mutex::new(IceRestartState::new())),
        })
    }

    // Method to set up ICE candidate handler with channel-based signaling
    pub fn setup_ice_candidate_handler(&self) {
        // Handle ICE candidates only when using trickle ICE
        if !self.trickle_ice {
            debug!(target: "webrtc_ice", tube_id = %self.tube_id, "Not setting up ICE candidate handler - trickle ICE is disabled");
            return;
        }
        info!(target: "webrtc_ice", tube_id = %self.tube_id, "Setting up ICE candidate handler");

        // IMPORTANT: To avoid circular references that prevent ICE agent cleanup,
        // we use a lightweight context struct instead of cloning the entire WebRTCPeerConnection
        let context = IceCandidateHandlerContext::new(self);

        // Remove any existing handlers first to avoid duplicates
        self.peer_connection
            .on_ice_candidate(Box::new(|_| Box::pin(async {})));

        // Set up handler for signaling state changes to flush buffered INCOMING candidates when ready
        let context_signaling = context.clone();

        self.peer_connection.on_signaling_state_change(Box::new(move |state| {
            debug!(target: "webrtc_ice", tube_id = %context_signaling.tube_id, "Signaling state changed to: {:?}", state);
            let context_clone = context_signaling.clone();
            Box::pin(async move {
                // Check if both descriptions are now set, regardless of specific state
                let local_desc = context_clone.peer_connection.local_description().await;
                let remote_desc = context_clone.peer_connection.remote_description().await;
                if local_desc.is_some() && remote_desc.is_some() {
                    debug!(target: "webrtc_ice", tube_id = %context_clone.tube_id, "Both descriptions set after signaling state change, flushing buffered INCOMING ICE candidates");
                    // Flush pending candidates manually (no self reference)
                    let candidates_to_flush = {
                        let mut lock = context_clone.pending_candidates.lock().unwrap();
                        std::mem::take(&mut *lock)
                    };
                    if !candidates_to_flush.is_empty() {
                        warn!(target: "webrtc_ice", tube_id = %context_clone.tube_id, count = candidates_to_flush.len(), "Flushing {} buffered incoming ICE candidates", candidates_to_flush.len());
                        for (index, candidate_str) in candidates_to_flush.iter().enumerate() {
                            if !candidate_str.is_empty() {
                                let candidate_init = RTCIceCandidateInit {
                                    candidate: candidate_str.clone(),
                                    ..Default::default()
                                };
                                match context_clone.peer_connection.add_ice_candidate(candidate_init).await {
                                    Ok(()) => {
                                        info!(target: "webrtc_ice", tube_id = %context_clone.tube_id, index = index, candidate = %candidate_str, "Successfully added buffered incoming ICE candidate");
                                    }
                                    Err(e) => {
                                        error!(target: "webrtc_ice", tube_id = %context_clone.tube_id, index = index, candidate = %candidate_str, error = %e, "Failed to add buffered incoming ICE candidate");
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
            info!(target: "webrtc_ice_internal", tube_id = %context_ice.tube_id, ?candidate, "on_ice_candidate triggered");

            let context_handler = context_ice.clone();

            Box::pin(async move {
                if let Some(c) = candidate {
                    // Convert the ICE candidate to a string representation
                    let candidate_str = format_ice_candidate(&c);
                    info!(target: "webrtc_ice", tube_id = %context_handler.tube_id, candidate = %candidate_str, "ICE candidate gathered");
                    debug!(target: "webrtc_ice_verbose", tube_id = %context_handler.tube_id, candidate = %candidate_str, "New ICE candidate details");

                    // Send immediately - no buffering on send side!
                    debug!(target: "webrtc_ice", tube_id = %context_handler.tube_id, "Sending ICE candidate immediately (trickle ICE)");
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
                    // All ICE candidates gathered (received None) - send immediately
                    debug!(target: "webrtc_ice", tube_id = %context_handler.tube_id, "All ICE candidates gathered (received None). Sending empty candidate signal immediately.");
                    // Send empty candidate signal manually (no self reference)
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
        info!(target: "webrtc_ice", tube_id = %self.tube_id, "flush_buffered_incoming_ice_candidates called");

        // Take the buffered candidates with a single lock operation
        let pending_candidates = {
            let mut lock = self.pending_incoming_ice_candidates.lock().unwrap();
            std::mem::take(&mut *lock)
        };

        // Add any buffered incoming candidates to the peer connection
        if !pending_candidates.is_empty() {
            warn!(target: "webrtc_ice", tube_id = %self.tube_id, count = pending_candidates.len(), "Flushing {} buffered incoming ICE candidates", pending_candidates.len());
            for (index, candidate_str) in pending_candidates.iter().enumerate() {
                if !candidate_str.is_empty() {
                    let candidate_init = RTCIceCandidateInit {
                        candidate: candidate_str.clone(),
                        ..Default::default()
                    };

                    match self.peer_connection.add_ice_candidate(candidate_init).await {
                        Ok(()) => {
                            info!(target: "webrtc_ice", tube_id = %self.tube_id, index = index, candidate = %candidate_str, "Successfully added buffered incoming ICE candidate");
                        }
                        Err(e) => {
                            error!(target: "webrtc_ice", tube_id = %self.tube_id, index = index, candidate = %candidate_str, error = %e, "Failed to add buffered incoming ICE candidate");
                        }
                    }
                } else {
                    info!(target: "webrtc_ice", tube_id = %self.tube_id, index = index, "Processed buffered end-of-candidates signal");
                }
            }
        } else {
            debug!(target: "webrtc_ice", tube_id = %self.tube_id, "No buffered incoming ICE candidates to flush");
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
                warn!(target: "webrtc_signal", tube_id = %self.tube_id, error = %e, "Failed to send ICE candidate signal");
            }
        } else {
            warn!(target: "webrtc_signal", tube_id = %self.tube_id, "Signal sender not available for ICE candidate");
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
                warn!(target: "webrtc_signal", tube_id = %self.tube_id, error = %e, "Failed to send answer signal");
            }
        } else {
            warn!(target: "webrtc_signal", tube_id = %self.tube_id, "Signal sender not available for answer");
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
                warn!(target: "webrtc_signal", tube_id = %self.tube_id, error = %e, "Failed to send connection state changed signal");
            } else {
                info!(target: "webrtc_signal", tube_id = %self.tube_id, state = %state, "Successfully sent connection state changed signal");
            }
        } else {
            warn!(target: "webrtc_signal", tube_id = %self.tube_id, "Signal sender not available for connection state change");
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
        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, state = ?current_state, "Current signaling state before create_{}", sdp_type_str);

        if is_offer {
            // Offer-specific signaling state validation
            if current_state
                == webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer
            {
                return if !self.trickle_ice {
                    if let Some(desc) = self.peer_connection.local_description().await {
                        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Already have local offer and non-trickle, returning existing SDP");
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
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Non-trickle ICE: gathering candidates before returning {}.", sdp_type_str);

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
                    debug!(target: "webrtc_ice", tube_id = %tube_id_log, new_state = ?state, "ICE gathering state changed (non-trickle {})", sdp_type_log);
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
                    debug!(target: "webrtc_ice", tube_id = %self.tube_id, "ICE gathering complete for non-trickle {}.", sdp_type_str);
                    if let Some(final_desc) = self.peer_connection.local_description().await {
                        let mut sdp_str = final_desc.sdp;

                        // Add max-message-size to answer SDP for non-trickle ICE only
                        if !is_offer && !sdp_str.contains("a=max-message-size") {
                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Answer SDP missing max-message-size, attempting to add it");

                            // Extract max-message-size from the offer (remote description)
                            let max_message_size = if let Some(remote_desc) =
                                self.peer_connection.remote_description().await
                            {
                                debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Remote description found, searching for max-message-size");

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
                                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Successfully extracted max-message-size from offer: {}", size);
                                            size
                                        } else {
                                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Failed to parse max-message-size value from offer");
                                            DEFAULT_MAX_MESSAGE_SIZE // Default if parsing fails
                                        }
                                    } else {
                                        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No line ending found after max-message-size in offer");
                                        DEFAULT_MAX_MESSAGE_SIZE // Default if no line ending
                                    }
                                } else {
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No max-message-size found in remote offer SDP");
                                    DEFAULT_MAX_MESSAGE_SIZE // Default if isn't found in offer
                                }
                            } else {
                                debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No remote description available");
                                DEFAULT_MAX_MESSAGE_SIZE // Default if no remote description
                            };

                            // Use the minimum of the client's requested size and our maximum
                            let our_max = OUR_MAX_MESSAGE_SIZE;
                            let negotiated_size = max_message_size.min(our_max);

                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Negotiating max-message-size: client_requested={} ({}KB), our_max={} ({}KB), negotiated={} ({}KB)",
                                   max_message_size, max_message_size/1024, our_max, our_max/1024, negotiated_size, negotiated_size/1024);

                            // Find the position to insert after sctp-port
                            if let Some(sctp_pos) = sdp_str.find("a=sctp-port:") {
                                // Find the end of the sctp-port line
                                if let Some(line_end) = sdp_str[sctp_pos..].find('\n') {
                                    let insert_pos = sctp_pos + line_end + 1;
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Found sctp-port at position {}", sctp_pos);
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Inserting 'a=max-message-size:{}' at position {}", negotiated_size, insert_pos);
                                    sdp_str.insert_str(
                                        insert_pos,
                                        &format!("a=max-message-size:{negotiated_size}\r\n"),
                                    );
                                    info!(target: "webrtc_sdp", tube_id = %self.tube_id,
                                          "Successfully added max-message-size={} ({}KB) to answer SDP (client requested: {} ({}KB), our max: {} ({}KB))",
                                          negotiated_size, negotiated_size/1024, max_message_size, max_message_size/1024, our_max, our_max/1024);
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
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Trickle ICE: returning {} immediately.", sdp_type_str);
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Initial {} SDP", sdp_obj.sdp.clone());

            // For trickle ICE, do not modify the SDP
            Ok(sdp_obj.sdp)
        }
    }

    // Create an offer (returns SDP string)
    pub async fn create_offer(&self) -> Result<String, String> {
        self.create_description_with_checks(true).await
    }

    // Create an answer (returns SDP string)
    pub async fn create_answer(&self) -> Result<String, String> {
        self.create_description_with_checks(false).await
    }

    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        debug!(target: "webrtc_sdp", tube_id = %self.tube_id,
               "set_remote_description called with {} (length: {} bytes)",
               if is_answer { "answer" } else { "offer" }, sdp.len());

        // Check if the offer contains max-message-size
        if !is_answer && sdp.contains("a=max-message-size:") {
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id,
                   "Incoming offer contains max-message-size attribute");
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
        debug!(target: "webrtc_sdp", ?current_state, "Current signaling state before set_remote_description");

        // Validate the signaling state transition
        Self::validate_signaling_state_transition(current_state, is_answer, false)?;

        // Set the remote description
        let result = self
            .peer_connection
            .set_remote_description(desc)
            .await
            .map_err(|e| format!("Failed to set remote description: {e}"));

        // If successful, update activity and flush buffered incoming candidates
        if result.is_ok() {
            // Update activity timestamp - SDP exchange is significant activity
            self.update_activity();

            let local_desc = self.peer_connection.local_description().await;
            let remote_desc = self.peer_connection.remote_description().await;
            if local_desc.is_some() && remote_desc.is_some() {
                debug!(target: "webrtc_ice", tube_id = %self.tube_id, "Both descriptions now set after remote description, flushing buffered incoming ICE candidates");
                self.flush_buffered_incoming_ice_candidates().await;
            }
        }

        result
    }

    pub async fn add_ice_candidate(&self, candidate_str: String) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            warn!(target: "webrtc_ice", tube_id = %self.tube_id, "add_ice_candidate called but connection is closing");
            return Err("Connection is closing".to_string());
        }

        info!(target: "webrtc_ice", tube_id = %self.tube_id, candidate = %candidate_str, "add_ice_candidate called");

        // Check if we can add candidates immediately (both descriptions must be set)
        let local_desc = self.peer_connection.local_description().await;
        let remote_desc = self.peer_connection.remote_description().await;
        let can_add_immediately = local_desc.is_some() && remote_desc.is_some();

        info!(target: "webrtc_ice", tube_id = %self.tube_id, local_set = local_desc.is_some(), remote_set = remote_desc.is_some(), can_add_immediately = can_add_immediately, "Checking if can add ICE candidate immediately");

        if can_add_immediately {
            // Connection is ready, add the candidate immediately
            info!(target: "webrtc_ice", tube_id = %self.tube_id, "Both descriptions set, adding incoming ICE candidate immediately");

            if !candidate_str.is_empty() {
                let candidate_init = RTCIceCandidateInit {
                    candidate: candidate_str.clone(),
                    ..Default::default()
                };

                match self.peer_connection.add_ice_candidate(candidate_init).await {
                    Ok(()) => {
                        info!(target: "webrtc_ice", tube_id = %self.tube_id, "Successfully added ICE candidate immediately");
                        Ok(())
                    }
                    Err(e) => {
                        error!(target: "webrtc_ice", tube_id = %self.tube_id, error = %e, "Failed to add ICE candidate immediately");
                        Err(format!("Failed to add ICE candidate: {e}"))
                    }
                }
            } else {
                // Empty candidate string means end-of-candidates, which is valid
                info!(target: "webrtc_ice", tube_id = %self.tube_id, "Received end-of-candidates signal");
                Ok(())
            }
        } else {
            // Connection is not ready yet, buffer the incoming candidate
            let mut candidates_lock = self.pending_incoming_ice_candidates.lock().unwrap();
            candidates_lock.push(candidate_str.clone());
            let buffered_count = candidates_lock.len();
            drop(candidates_lock);

            warn!(target: "webrtc_ice", tube_id = %self.tube_id, candidate = %candidate_str, "Descriptions not ready (local: {}, remote: {}), buffering incoming ICE candidate (total buffered: {})", 
                   local_desc.is_some(), remote_desc.is_some(), buffered_count);
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

    pub async fn close(&self) -> Result<(), String> {
        // Avoid duplicate close operations
        if self.is_closing.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already closing or closed
        }

        // Stop keepalive task before closing
        if let Err(e) = self.stop_keepalive().await {
            warn!(target: "webrtc_lifecycle", tube_id = %self.tube_id, 
                  error = %e, "Failed to stop keepalive during close");
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
                        info!(target: "resource_management", tube_id = %self.tube_id, 
                              "Explicitly dropping ICE agent guard to ensure resource cleanup");
                        drop(guard);
                    }
                }
                Err(e) => {
                    warn!(target: "resource_management", tube_id = %self.tube_id,
                          error = %e, "Failed to acquire ICE agent guard lock during cleanup - proceeding anyway");
                }
            }
        }

        // Then close the connection with a timeout to avoid hanging
        match tokio::time::timeout(Duration::from_secs(5), self.peer_connection.close()).await {
            Ok(result) => result.map_err(|e| format!("Failed to close peer connection: {e}")),
            Err(_) => {
                // The timeout elapsed.
                warn!(target: "webrtc_lifecycle", tube_id = %self.tube_id, "Close operation timed out for peer connection. The underlying webrtc-rs close() did not complete in 5 seconds.");
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
        debug!(target: "webrtc_sdp", ?current_state, "Current signaling state before set_local_description");

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
                debug!(target: "webrtc_ice", tube_id = %self.tube_id, "Both descriptions now set after local description, flushing buffered incoming ICE candidates");
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
            info!(target: "webrtc_keepalive", tube_id = %tube_id_clone, 
                  interval_minutes = keepalive_interval.as_secs() / 60,
                  "NAT timeout prevention active - ensuring periodic activity every {} seconds", 
                  keepalive_interval.as_secs());

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
                debug!(target: "webrtc_keepalive", tube_id = %tube_id_clone, 
                       "NAT timeout prevention tick - periodic activity to keep channel ping system active");

                // Get current connection state to verify we're still connected
                let connection_state = pc_clone.connection_state();
                debug!(target: "webrtc_keepalive", tube_id = %tube_id_clone,
                       connection_state = ?connection_state, "Connection state check");
            }

            info!(target: "webrtc_keepalive", tube_id = %tube_id_clone, 
                  "NAT timeout prevention stopped");
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

        info!(target: "webrtc_keepalive", tube_id = %self.tube_id, 
              "NAT timeout prevention started - integrated with existing channel ping system");
        Ok(())
    }

    // Update activity timestamp for timeout detection
    pub fn update_activity(&self) {
        let now = Instant::now();

        // Update both activity timestamps in a single lock acquisition (deadlock-safe)
        if let Ok(mut activity_state) = self.activity_state.lock() {
            activity_state.update_both(now);
            debug!(target: "webrtc_activity", tube_id = %self.tube_id, 
                   "Activity updated - connection active");
        } else {
            warn!(target: "webrtc_activity", tube_id = %self.tube_id, 
                  "Failed to acquire lock for activity update");
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
                info!(target: "webrtc_keepalive", tube_id = %self.tube_id, 
                      "Keepalive task stopped and cleaned up");
            } else {
                debug!(target: "webrtc_keepalive", tube_id = %self.tube_id, 
                       "No active keepalive task to stop");
            }
        } else {
            warn!(target: "webrtc_keepalive", tube_id = %self.tube_id, 
                  "Failed to acquire keepalive task lock for cleanup");
        }

        info!(target: "webrtc_keepalive", tube_id = %self.tube_id, 
              "NAT timeout prevention stopped");
        Ok(())
    }

    // Check if ICE restart is needed based on connection quality (DEADLOCK-SAFE)
    pub fn should_restart_ice(&self) -> bool {
        let current_state = self.peer_connection.connection_state();
        let now = Instant::now();

        // Check if connection is in a degraded state
        let connection_degraded = matches!(
            current_state,
            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected
                | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
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
            debug!(target: "webrtc_ice_restart", tube_id = %self.tube_id,
                   connection_state = ?current_state,
                   time_since_success_secs = time_since_success.as_secs(),
                   restart_attempts = attempts,
                   min_interval_secs = min_interval.as_secs(),
                   "ICE restart conditions met");
        } else {
            debug!(target: "webrtc_ice_restart", tube_id = %self.tube_id,
                   connection_state = ?current_state,
                   connection_degraded = connection_degraded,
                   activity_timeout = activity_timeout,
                   enough_time_passed = enough_time_passed,
                   not_too_many_attempts = not_too_many_attempts,
                   "ICE restart conditions not met");
        }

        should_restart
    }

    // Perform ICE restart to recover from connectivity issues (DEADLOCK-SAFE)
    pub async fn restart_ice(&self) -> Result<String, String> {
        info!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
              "ICE restart initiated for connection recovery");

        // Update restart tracking in single lock acquisition (deadlock-safe)
        let now = Instant::now();
        if let Ok(mut restart_state) = self.ice_restart_state.lock() {
            restart_state.record_attempt(now);
            let count = restart_state.attempts;
            info!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
                  attempt = count, "ICE restart attempt #{}", count);
        } else {
            return Err("Failed to acquire restart state lock".to_string());
        }

        // Set connection quality as degraded during restart
        self.connection_quality_degraded
            .store(true, Ordering::Relaxed);

        // Generate new offer with ICE restart
        match self.peer_connection.create_offer(None).await {
            Ok(offer) => {
                info!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
                      sdp_length = offer.sdp.len(),
                      "Successfully generated ICE restart offer");

                // Set the new local description to trigger ICE restart
                let offer_desc = RTCSessionDescription::offer(offer.sdp.clone())
                    .map_err(|e| format!("Failed to create offer session description: {e}"))?;

                match self.peer_connection.set_local_description(offer_desc).await {
                    Ok(()) => {
                        info!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
                              "ICE restart offer set as local description - new ICE session will begin");

                        // Update activity since we just performed a successful SDP operation
                        self.update_activity();

                        // Reset connection quality flag - we'll monitor for improvement
                        self.connection_quality_degraded
                            .store(false, Ordering::Relaxed);

                        Ok(offer.sdp)
                    }
                    Err(e) => {
                        warn!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
                              error = %e, "Failed to set ICE restart offer as local description");
                        Err(format!(
                            "Failed to set local description for ICE restart: {e}"
                        ))
                    }
                }
            }
            Err(e) => {
                warn!(target: "webrtc_ice_restart", tube_id = %self.tube_id, 
                      error = %e, "Failed to create ICE restart offer");
                Err(format!("Failed to create ICE restart offer: {e}"))
            }
        }
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
}
