use crate::buffer_pool::BufferPool;
use crate::models::TunnelTimeouts;
use crate::router_helpers::post_connection_state;
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::{setup_channel_for_data_channel, TubeStatus};
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::tube_registry::SignalMessage;
use crate::unlikely;
use crate::webrtc_core::{create_data_channel, WebRTCPeerConnection};
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use uuid::Uuid;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

// Lightweight metadata for tracking channel information without storing the full Channel
#[derive(Clone, Debug)]
pub struct ChannelMetadata {
    pub callback_token: Option<String>,
    pub ksm_config: Option<String>,
    pub client_version: String,
    pub recordings_enabled: bool,
}

// A single tube holding a WebRTC peer connection and channels
// NOTE: Tube should NOT be Clone! It's always used inside Arc<Tube>.
// Cloning Tube creates multiple instances that each call Drop, causing double-frees and premature cleanup.
// Use Arc::clone() on Arc<Tube> instead.
pub struct Tube {
    // Unique ID for this tube
    pub(crate) id: String,
    // WebRTC peer connection
    pub(crate) peer_connection: Arc<TokioMutex<Option<Arc<WebRTCPeerConnection>>>>,
    // Data channels mapped by label
    pub(crate) data_channels: Arc<TokioRwLock<HashMap<String, WebRTCDataChannel>>>,
    // Control channel (special default channel)
    pub(crate) control_channel: Arc<TokioRwLock<Option<WebRTCDataChannel>>>,
    // Map of channel names to their shutdown signals
    pub channel_shutdown_signals: Arc<TokioRwLock<HashMap<String, Arc<AtomicBool>>>>,
    // Active channel metadata for tracking callbacks and config
    pub(crate) active_channels: Arc<TokioRwLock<HashMap<String, ChannelMetadata>>>,
    // Indicates if this tube was created in a server or client context by its registry
    pub(crate) is_server_mode_context: bool,
    // Current status
    pub(crate) status: Arc<TokioRwLock<TubeStatus>>,
    // Runtime
    pub(crate) runtime: crate::runtime::RuntimeHandle,
    // Original conversation ID that created this tube (for control channel mapping)
    pub(crate) original_conversation_id: Option<String>,
    // Client version for authentication
    pub(crate) client_version: Arc<TokioRwLock<Option<String>>>,

    // ============================================================================
    // RAII PATTERN: Lifecycle-bound resources owned by Tube
    // When Tube drops, these automatically cleanup via Drop trait
    // ============================================================================
    /// Signal channel for Python communication - automatically closes when tube drops
    pub(crate) signal_sender: Option<UnboundedSender<SignalMessage>>,

    /// Metrics registration handle - automatically unregisters when tube drops
    pub(crate) metrics_handle: Arc<TokioMutex<Option<crate::metrics::MetricsHandle>>>,

    /// Keepalive task - automatically cancels when tube drops
    pub(crate) keepalive_task: Arc<TokioMutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Tube {
    // Helper method to get the proper conversation_id for a channel
    fn get_conversation_id_for_channel(&self, channel_label: &str) -> Option<String> {
        if channel_label == "control" {
            // For the control channel, use the original conversation ID if available
            self.original_conversation_id.clone()
        } else {
            // For other channels, the label is typically the connection_id
            Some(channel_label.to_string())
        }
    }

    // Create a new tube with RAII resource ownership
    pub fn new(
        is_server_mode_context: bool,
        original_conversation_id: Option<String>,
        signal_sender: Option<UnboundedSender<SignalMessage>>,
        custom_tube_id: Option<String>,
    ) -> Result<Arc<Self>> {
        let id = custom_tube_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let runtime = get_runtime();

        // Create metrics handle (auto-registers with METRICS_COLLECTOR)
        let metrics_handle = original_conversation_id
            .as_ref()
            .map(|conv_id| crate::metrics::MetricsHandle::new(conv_id.clone(), id.clone()));

        let has_signal = signal_sender.is_some();
        let has_metrics = metrics_handle.is_some();

        let tube = Arc::new(Self {
            id: id.clone(),
            peer_connection: Arc::new(TokioMutex::new(None)),
            data_channels: Arc::new(TokioRwLock::new(HashMap::new())),
            control_channel: Arc::new(TokioRwLock::new(None)),
            channel_shutdown_signals: Arc::new(TokioRwLock::new(HashMap::new())),
            active_channels: Arc::new(TokioRwLock::new(HashMap::new())),
            is_server_mode_context,
            status: Arc::new(TokioRwLock::new(TubeStatus::Initializing)),
            runtime,
            original_conversation_id: original_conversation_id.clone(),
            client_version: Arc::new(TokioRwLock::new(None)),

            // RAII resources (owned by Tube):
            signal_sender,
            metrics_handle: Arc::new(TokioMutex::new(metrics_handle)),
            keepalive_task: Arc::new(TokioMutex::new(None)),
        });

        debug!(
            "Tube created with RAII resources (tube_id: {}, has_signal: {}, has_metrics: {})",
            id, has_signal, has_metrics
        );

        Ok(tube)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_peer_connection(
        self: &Arc<Self>,
        config: Option<RTCConfiguration>,
        trickle_ice: bool,
        turn_only: bool,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
        protocol_settings: HashMap<String, serde_json::Value>,
        signal_sender: UnboundedSender<SignalMessage>,
        turn_credentials_created_at: Option<std::time::Instant>, // Track when TURN credentials were fetched
    ) -> Result<()> {
        debug!(
            "[TUBE_DEBUG] Tube {}: create_peer_connection called. trickle_ice: {}, turn_only: {}",
            self.id, trickle_ice, turn_only
        );
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Create_peer_connection protocol_settings (tube_id: {}, protocol_settings: {:?})",
                self.id, protocol_settings
            );
        }

        // Store client_version in the tube
        {
            let mut tube_client_version_guard = self.client_version.write().await;
            if tube_client_version_guard.is_none() {
                *tube_client_version_guard = Some(client_version.to_string());
            }
        }

        let mut connection = WebRTCPeerConnection::new(
            config,
            trickle_ice,
            turn_only,
            Some(signal_sender),
            self.id.clone(),                       // Pass tube_id
            self.original_conversation_id.clone(), // Pass original conversation_id for WebRTC signals
            Some(ksm_config.clone()),              // For TURN credential refresh
            client_version.to_string(),            // For API calls
            turn_credentials_created_at,           // Timestamp when TURN credentials were fetched
        )
        .await
        .map_err(|e| anyhow!("{}", e))?;

        // Set server mode for accurate logging
        connection.set_server_mode(self.is_server_mode_context);

        let connection_arc = Arc::new(connection);

        let status = self.status.clone();

        let tube_arc_for_pc_state = Arc::clone(self);

        debug!("[TUBE_DEBUG] Tube {}: About to call setup_ice_candidate_handler. Callback token (used as conv_id before): {}", self.id, callback_token);
        connection_arc.setup_ice_candidate_handler();

        // Set up comprehensive connection state monitoring with tube lifecycle management
        let is_closing_for_tube = Arc::clone(&connection_arc.is_closing);
        let connection_arc_for_signals = Arc::clone(&connection_arc); // Clone for signal sending
        connection_arc.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let status_clone = status.clone();
            let tube_clone_for_closure = tube_arc_for_pc_state.clone();
            let is_closing_for_handler = Arc::clone(&is_closing_for_tube);
            let connection_for_signals = Arc::clone(&connection_arc_for_signals);

            Box::pin(async move {
                let tube_id_log = tube_clone_for_closure.id.clone();
                let state_str = format!("{state:?}");
                // WebRTC monitoring with tube_id context
                debug!("Connection state changed (tube_id: {}, state: {})", tube_id_log, state_str);

                match state {
                    RTCPeerConnectionState::Connected => {
                        debug!("Connection established (tube_id: {})", tube_id_log);
                        // Tube status update
                        *status_clone.write().await = TubeStatus::Active;
                        debug!("Tube connection state changed to Active (tube_id: {})", tube_id_log);

                        // Start monitoring and quality management
                        if let Err(e) = connection_for_signals.start_monitoring().await {
                            warn!("Failed to start monitoring (tube_id: {}, error: {})", tube_id_log, e);
                        }

                        // Start keepalive mechanism to prevent NAT timeouts
                        if let Err(e) = connection_for_signals.start_keepalive().await {
                            warn!("Failed to start keepalive (tube_id: {}, error: {})", tube_id_log, e);
                        }

                        // Report successful connection establishment
                        if let Err(e) = connection_for_signals.report_success("ConnectionEstablished").await {
                            debug!("Failed to report connection success (tube_id: {}, error: {})", tube_id_log, e);
                        }

                        // Send connection state changed signal to Python
                        debug!("Sending 'connected' signal to Python (tube_id: {})", tube_id_log);
                        connection_for_signals.send_connection_state_changed("connected");
                    },
                    RTCPeerConnectionState::Failed => {
                        // Update the closing flag (from comprehensive monitor)
                        is_closing_for_handler.store(true, std::sync::atomic::Ordering::Release);
                        debug!("Connection failed (tube_id: {}, state: {})", tube_id_log, state_str);

                        // Report connection failure for adaptive learning
                        let connection_error = crate::webrtc_errors::WebRTCError::IceConnectionFailed {
                            tube_id: tube_id_log.clone(),
                            reason: "Peer connection state failed".to_string(),
                        };
                        if let Err(e) = connection_for_signals.trigger_adaptive_recovery(&connection_error).await {
                            warn!("Failed to trigger adaptive recovery (tube_id: {}, error: {})", tube_id_log, e);
                        }

                        // Tube lifecycle management
                        let current_status = *status_clone.read().await;
                        if current_status != TubeStatus::Closing &&
                           current_status != TubeStatus::Closed &&
                           current_status != TubeStatus::Failed &&
                           current_status != TubeStatus::Disconnected {

                            warn!("WebRTC peer connection failed. Initiating Tube close. (tube_id: {}, old_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Failed;

                            let runtime = get_runtime();
                            runtime.spawn(async move {
                                debug!("Spawning task to close tube due to peer connection failure. (tube_id: {})", tube_id_log);

                                // LOCK-FREE: Close via actor (no deadlock possible!)
                                if let Err(e) = crate::tube_registry::REGISTRY
                                    .close_tube(&tube_id_log, Some(CloseConnectionReason::ConnectionFailed))
                                    .await
                                {
                                    error!("Error trying to close tube via registry: {} (tube_id: {})", e, tube_id_log);
                                } else {
                                    debug!("Successfully initiated tube closure via registry. (tube_id: {})", tube_id_log);
                                }
                            });
                        } else {
                            debug!("Peer connection failed, but tube already closing/closed. (tube_id: {}, current_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Failed;
                        }
                    },
                    RTCPeerConnectionState::Closed => {
                        // Update the closing flag (from comprehensive monitor)
                        is_closing_for_handler.store(true, std::sync::atomic::Ordering::Release);
                        info!("Connection closed (tube_id: {}, state: {})", tube_id_log, state_str);

                        // Tube lifecycle management
                        let current_status = *status_clone.read().await;
                        if current_status != TubeStatus::Closing &&
                           current_status != TubeStatus::Closed &&
                           current_status != TubeStatus::Failed &&
                           current_status != TubeStatus::Disconnected {

                            warn!("WebRTC peer connection closed. Initiating Tube close. (tube_id: {}, old_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Closed;

                            let runtime = get_runtime();
                            runtime.spawn(async move {
                                debug!("Spawning task to close tube due to peer connection closure. (tube_id: {})", tube_id_log);
                                // LOCK-FREE: Close via actor (no deadlock possible!)
                                if let Err(e) = crate::tube_registry::REGISTRY
                                    .close_tube(&tube_id_log, Some(CloseConnectionReason::Normal))
                                    .await
                                {
                                    error!("Error trying to close tube via registry: {} (tube_id: {})", e, tube_id_log);
                                } else {
                                    debug!("Successfully initiated tube closure via registry. (tube_id: {})", tube_id_log);
                                }
                            });
                        } else {
                            debug!("Peer connection closed, but tube already closing/closed. (tube_id: {}, current_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Closed;
                        }
                    },
                    RTCPeerConnectionState::Disconnected => {
                        debug!("Connection disconnected (tube_id: {}, state: {})", tube_id_log, state_str);

                        // Tube lifecycle management
                        let current_status = *status_clone.read().await;
                        if current_status != TubeStatus::Closing &&
                           current_status != TubeStatus::Closed &&
                           current_status != TubeStatus::Failed &&
                           current_status != TubeStatus::Disconnected {

                            debug!("WebRTC peer connection disconnected. Initiating Tube close. (tube_id: {}, old_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Disconnected;

                            let runtime = get_runtime();
                            runtime.spawn(async move {
                                debug!("Spawning task to close tube due to peer connection disconnection. (tube_id: {})", tube_id_log);
                                // LOCK-FREE: Close via actor (no deadlock possible!)
                                if let Err(e) = crate::tube_registry::REGISTRY
                                    .close_tube(&tube_id_log, Some(CloseConnectionReason::ConnectionLost))
                                    .await
                                {
                                    error!("Error trying to close tube via registry: {} (tube_id: {})", e, tube_id_log);
                                } else {
                                    debug!("Successfully initiated tube closure via registry. (tube_id: {})", tube_id_log);
                                }
                            });
                        } else {
                            debug!("Peer connection disconnected, but tube already closing/closed. (tube_id: {}, current_status: {:?}, new_state: {:?})", tube_id_log, current_status, state);
                            *status_clone.write().await = TubeStatus::Disconnected;
                        }
                    },
                    _ => {
                        debug!("Connection state changed to: {:?} (tube_id: {})", state, tube_id_log);
                    }
                }
            })
        }));

        // Set up ICE connection state monitoring with TURN detection
        let tube_id_for_ice = self.id.clone();
        let pc_for_analysis = Arc::clone(&connection_arc.peer_connection);
        connection_arc.peer_connection.on_ice_connection_state_change(Box::new(move |state| {
            let tube_id_for_ice_log = tube_id_for_ice.clone();
            let pc_for_candidate_analysis = Arc::clone(&pc_for_analysis);

            Box::pin(async move {
                debug!("ICE connection state changed (tube_id: {}, state: {:?})", tube_id_for_ice_log, state);

                match state {
                    webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected => {
                        debug!("ICE connection established (tube_id: {})", tube_id_for_ice_log);

                        // Enhanced candidate analysis at trace level for TURN detection
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            // Helper function to parse candidate type from SDP
                            fn parse_candidate_type_from_sdp(sdp: &str) -> Option<String> {
                                for line in sdp.lines() {
                                    if line.starts_with("a=candidate:") {
                                        let parts: Vec<&str> = line.split_whitespace().collect();
                                        if parts.len() >= 8 {
                                            let candidate_type = parts[7]; // typ field
                                            return Some(candidate_type.to_string());
                                        }
                                    }
                                }
                                None
                            }

                            // Get local and remote descriptions
                            let local_desc = pc_for_candidate_analysis.local_description().await;
                            let remote_desc = pc_for_candidate_analysis.remote_description().await;

                            if let (Some(local), Some(remote)) = (local_desc, remote_desc) {
                                let local_type = parse_candidate_type_from_sdp(&local.sdp);
                                let remote_type = parse_candidate_type_from_sdp(&remote.sdp);
                                debug!("ICE connection established (tube_id: {})", tube_id_for_ice_log);

                                match (local_type, remote_type) {
                                    (Some(local), Some(remote)) => {
                                        let using_turn = local == "relay" || remote == "relay";

                                        if unlikely!(crate::logger::is_verbose_logging()) {
                                            debug!("ICE candidates: local_type={} remote_type={} {} (tube_id: {}, local_type: {}, remote_type: {}, using_turn: {})",
                                                local, remote,
                                                if using_turn { "(using TURN)" } else { "(no TURN)" },
                                                tube_id_for_ice_log, local, remote, using_turn
                                            );
                                        }

                                        // Always log connection type with TURN indicator
                                        info!("Connection type: local={} remote={}{} (tube_id: {})",
                                            local, remote,
                                            if using_turn { " (TURN relay in use)" } else { "" },
                                            tube_id_for_ice_log
                                        );
                                    },
                                    _ => {
                                        if unlikely!(crate::logger::is_verbose_logging()) {
                                            debug!("ICE connection established but could not parse candidate types (tube_id: {})", tube_id_for_ice_log);
                                        }
                                    }
                                }
                            } else if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("ICE connection established but SDP descriptions not available (tube_id: {})", tube_id_for_ice_log);
                            }
                        }
                    },
                    webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Failed => {
                        warn!("ICE connection failed (tube_id: {})", tube_id_for_ice_log);
                    },
                    webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Disconnected => {
                        info!("ICE connection disconnected (tube_id: {})", tube_id_for_ice_log);
                    },
                    _ => {}
                }
            })
        }));

        // Set up ICE gathering state monitoring with timing metrics
        let tube_id_for_gather = self.id.clone();
        let conversation_id_for_gather = Some(self.id.clone());
        connection_arc
            .peer_connection
            .on_ice_gathering_state_change(Box::new(move |state| {
                let tube_id_for_gather_log = tube_id_for_gather.clone();
                let conversation_id_for_gather_log = conversation_id_for_gather.clone();
                Box::pin(async move {
                    debug!(
                        "ICE gathering state changed (tube_id: {}, state: {:?})",
                        tube_id_for_gather_log, state
                    );

                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as f64;

                    match state {
                    webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Complete => {
                        debug!("ICE gathering complete (tube_id: {})", tube_id_for_gather_log);

                        // Update ICE gathering completion time in metrics
                        if let Some(conversation_id) = &conversation_id_for_gather_log {
                            crate::metrics::METRICS_COLLECTOR.update_ice_gathering_complete(
                                conversation_id,
                                now_ms
                            );
                        }
                    },
                    webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState::Gathering => {
                        debug!("ICE gathering started (tube_id: {})", tube_id_for_gather_log);

                        // Update ICE gathering start time in metrics
                        if let Some(conversation_id) = &conversation_id_for_gather_log {
                            crate::metrics::METRICS_COLLECTOR.update_ice_gathering_start(
                                conversation_id,
                                now_ms
                            );
                        }
                    },
                    _ => {}
                }
                })
            }));

        // Set up a handler for incoming data channels
        debug!("[DATA_CHANNEL_SETUP] Registering on_data_channel callback (tube_id: {}, is_server_mode: {})", self.id, self.is_server_mode_context);
        let tube_clone = Arc::clone(self);
        let protocol_settings_clone_for_on_data_channel = protocol_settings.clone(); // Clone for the outer closure
        let callback_token_for_on_data_channel = callback_token.clone(); // Clone for on_data_channel
        let ksm_config_for_on_data_channel = ksm_config.clone(); // Clone for on_data_channel
        let tube_client_version_for_on_data_channel = self.client_version.clone(); // Clone for on_data_channel
        let peer_connection_for_on_data_channel = connection_arc.clone(); // Clone peer connection for data channel handler
        connection_arc.peer_connection.on_data_channel(Box::new(move |rtc_data_channel| {
            debug!("[DATA_CHANNEL_CALLBACK] on_data_channel FIRED! tube_id: {}, channel_label: {}, rtc_channel_id: {}", tube_clone.id(), rtc_data_channel.label(), rtc_data_channel.id());
            let tube = tube_clone.clone();
            // Use the protocol_settings cloned for the on_data_channel closure
            let protocol_settings_for_channel_setup = protocol_settings_clone_for_on_data_channel.clone();
            let callback_token_for_channel = callback_token_for_on_data_channel.clone();
            let ksm_config_for_channel = ksm_config_for_on_data_channel.clone();
            let client_version_arc_for_channel = tube_client_version_for_on_data_channel.clone();
            let peer_connection_for_channel = peer_connection_for_on_data_channel.clone();
            let rtc_data_channel_label = rtc_data_channel.label().to_string(); // Get the label once for logging
            let rtc_data_channel_id = rtc_data_channel.id();

            Box::pin(async move {
                debug!("[TUBE_CALLBACK] on_data_channel FIRED! tube_id: {}, channel_label: {}, rtc_channel_id: {:?}", tube.id, rtc_data_channel_label, rtc_data_channel_id);
                debug!("on_data_channel: Received data channel from remote peer. (tube_id: {}, channel_label: {}, rtc_channel_id: {:?})", tube.id, rtc_data_channel_label, rtc_data_channel_id);
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("on_data_channel: Protocol settings for this channel. (tube_id: {}, channel_label: {}, protocol_settings_for_channel_setup: {:?})", tube.id, rtc_data_channel_label, protocol_settings_for_channel_setup);
                }

                // Get client_version from the tube
                let client_version = match client_version_arc_for_channel.read().await.clone() {
                    Some(version) => version,
                    None => {
                        error!("client_version not set in tube - cannot create channel. This indicates a bug in tube initialization. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                        return;
                    }
                };

                // Create our WebRTCDataChannel wrapper
                let data_channel = WebRTCDataChannel::new(rtc_data_channel);

                // Add it to our data channels map
                if let Err(e) = tube.add_data_channel(data_channel.clone()).await {
                    error!("on_data_channel: Failed to add data channel to tube: {} (tube_id: {}, channel_label: {})", e, tube.id, rtc_data_channel_label);
                    return;
                }
                debug!("on_data_channel: Data channel added to tube's map. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);

                // If this is the control channel, store it specially
                if rtc_data_channel_label == "control" {
                    *tube.control_channel.write().await = Some(data_channel.clone());
                    debug!("on_data_channel: Set as control channel. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                }

                // Extract recordings_enabled before protocol_settings gets moved
                let recordings_enabled = protocol_settings_for_channel_setup
                    .get("guacd_params")
                    .and_then(|v| v.as_object())
                    .and_then(|params| params.get("recordingenabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Determine server_mode for the new channel based on the Tube's context
                let current_server_mode = tube.is_server_mode_context;
                debug!("on_data_channel: Determined server_mode for channel setup. (tube_id: {}, channel_label: {}, server_mode: {})", tube.id, rtc_data_channel_label, current_server_mode);

                debug!("on_data_channel: About to call setup_channel_for_data_channel. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                let channel_result = setup_channel_for_data_channel(
                    &data_channel,
                    &peer_connection_for_channel,
                    rtc_data_channel_label.clone(),
                    None,
                    protocol_settings_for_channel_setup,
                    current_server_mode,
                    Some(callback_token_for_channel), // Use callback_token from tube creation
                    Some(ksm_config_for_channel), // Use ksm_config from tube creation
                    client_version,
                ).await;

                let mut owned_channel = match channel_result {
                    Ok(ch_instance) => {
                        debug!("on_data_channel: setup_channel_for_data_channel successful. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                        ch_instance
                    }
                    Err(e) => {
                        error!("Tube {}: Failed to setup channel for incoming data channel '{}': {}", tube.id, rtc_data_channel_label, e);
                        return;
                    }
                };

                // Register the channel metadata with the tube for tracking
                let metadata = ChannelMetadata {
                    callback_token: owned_channel.callback_token.clone(),
                    ksm_config: owned_channel.ksm_config.clone(),
                    client_version: owned_channel.client_version.clone(),
                    recordings_enabled,
                };
                if let Err(e) = tube.register_channel_metadata(rtc_data_channel_label.clone(), metadata).await {
                    error!("Tube {}: Failed to register channel metadata '{}': {}", tube.id, rtc_data_channel_label, e);
                    return;
                }
                debug!("on_data_channel: Channel metadata registered with tube (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("on_data_channel: Channel details after setup. (tube_id: {}, channel_label: {}, active_protocol: {:?}, local_listen_addr: {:?})", tube.id, rtc_data_channel_label, owned_channel.active_protocol, owned_channel.local_listen_addr);
                }

                // Store the shutdown signal for this newly created channel
                let shutdown_signal = Arc::clone(&owned_channel.should_exit);
                tube.channel_shutdown_signals.write().await.insert(rtc_data_channel_label.clone(), shutdown_signal);
                debug!("on_data_channel: Shutdown signal stored for channel. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);


                if owned_channel.server_mode {
                    if let Some(listen_addr_str) = owned_channel.local_listen_addr.clone() {
                        if !listen_addr_str.is_empty() &&
                           matches!(owned_channel.active_protocol, crate::channel::types::ActiveProtocol::PortForward | crate::channel::types::ActiveProtocol::Socks5 | crate::channel::types::ActiveProtocol::Guacd) // Assuming Guacamole might be server mode too
                        {
                            debug!("on_data_channel: Channel is server mode, attempting to start server. (tube_id: {}, channel_label: {}, protocol: {:?}, listen_addr: {})", tube.id, rtc_data_channel_label, owned_channel.active_protocol, listen_addr_str);
                            match owned_channel.start_server(&listen_addr_str).await {
                                Ok(socket_addr) => {
                                    debug!("on_data_channel: Server started successfully. (tube_id: {}, channel_label: {}, listen_port: {})", tube.id, rtc_data_channel_label, socket_addr.port());
                                }
                                Err(e) => {
                                    error!("on_data_channel: Failed to start server: {}. Channel will not run effectively. (tube_id: {}, channel_label: {}, listen_addr: {})", e, tube.id, rtc_data_channel_label, listen_addr_str);
                                    tube.channel_shutdown_signals.write().await.remove(&rtc_data_channel_label);
                                    return;
                                }
                            }
                        } else {
                            debug!("on_data_channel: Server mode channel, but no listen address or not a server-type protocol, skipping start_server. (tube_id: {}, channel_label: {}, protocol: {:?}, listen_addr: {:?})", tube.id, rtc_data_channel_label, owned_channel.active_protocol, owned_channel.local_listen_addr);
                        }
                    } else {
                         debug!("on_data_channel: Server mode channel, but local_listen_addr is None. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                    }
                } else {
                    debug!("on_data_channel: Channel is not server_mode. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
                }

                let label_clone_for_run = rtc_data_channel_label.clone();
                let runtime_for_run = get_runtime();
                let tube_id_for_log = tube.id.clone();
                // Clone references for spawned task - avoid double Arc wrapping
                let tube_arc = Arc::clone(&tube); // Clone the Arc, not the Tube
                let peer_connection_for_signal = Arc::clone(&tube.peer_connection);

                debug!("on_data_channel: Spawning channel.run() task. (tube_id: {}, channel_label: {})", tube.id, label_clone_for_run);
                runtime_for_run.spawn(async move {
                    debug!("on_data_channel: channel.run() task started. (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);

                    // Send connection_open callback when a channel starts running
                    if let Err(e) = tube_arc.send_connection_open_callback(&label_clone_for_run).await {
                        warn!("Failed to send connection_open callback: {} (tube_id: {}, channel_label: {})", e, tube_id_for_log, label_clone_for_run);
                    }

                    // Clone the Arc so we can access it after run() consumes the channel
                    let close_reason_arc = owned_channel.channel_close_reason.clone();
                    let run_result = owned_channel.run().await;
                    // Get the close reason after run completes - use try_lock to avoid blocking
                    let close_reason = close_reason_arc.try_lock().ok().and_then(|guard| *guard);

                    let outcome_details: String = match &run_result {
                        Ok(()) => {
                            info!("Channel '{}' (from on_data_channel) ran and exited normally. Signaling Python. (tube_id: {}, channel_label: {})", label_clone_for_run, tube_id_for_log, label_clone_for_run);
                            "normal_exit".to_string()
                        }
                        Err(crate::error::ChannelError::CriticalUpstreamClosed(closed_channel_id_from_err)) => {
                            warn!("Channel '{}' (from on_data_channel) exited due to critical upstream closure. Signaling Python. (tube_id: {}, channel_label: {}, channel_id_in_err: {})", label_clone_for_run, tube_id_for_log, label_clone_for_run, closed_channel_id_from_err);
                            format!("critical_upstream_closed: {closed_channel_id_from_err}")
                        }
                        Err(e) => {
                            error!("Channel '{}' (from on_data_channel) encountered an error in run(): {}. Signaling Python. (tube_id: {}, channel_label: {})", label_clone_for_run, e, tube_id_for_log, label_clone_for_run);
                            format!("error: {e}")
                        }
                    };

                    // Send connection_close callback when channel finishes
                    if let Err(e) = tube_arc.send_connection_close_callback(&label_clone_for_run).await {
                        warn!("Failed to send connection_close callback: {} (tube_id: {}, channel_label: {})", e, tube_id_for_log, label_clone_for_run);
                    }

                    // Deregister the channel from the tube
                    tube_arc.deregister_channel(&label_clone_for_run).await;

                    // Remove shutdown signal for this channel
                    tube_arc.remove_channel_shutdown_signal(&label_clone_for_run).await;

                    // Always send a signal when channel.run() finishes, regardless of reason.
                    let pc_guard = peer_connection_for_signal.lock().await;
                    if let Some(pc_instance_arc) = &*pc_guard {
                        if let Some(sender) = &pc_instance_arc.signal_sender {
                            let mut signal_json = serde_json::json!({
                                "channel_id": label_clone_for_run, // The label of the channel from on_data_channel
                                "outcome": outcome_details
                            });
                            // Add close reason if available
                            if let Some(reason) = close_reason {
                                signal_json["close_reason"] = serde_json::json!({
                                    "code": reason as u16,
                                    "name": format!("{:?}", reason),
                                    "is_critical": reason.is_critical(),
                                    "is_retryable": reason.is_retryable(),
                                });
                            }
                            let signal_data = signal_json.to_string();

                            let signal_msg = SignalMessage {
                                tube_id: tube_id_for_log.clone(),
                                kind: "channel_closed".to_string(),
                                data: signal_data,
                                conversation_id: tube_arc.get_conversation_id_for_channel(&label_clone_for_run).unwrap_or_else(|| {
                                    debug!("No conversation_id mapping found, using channel label (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);
                                    label_clone_for_run.clone()
                                }),
                                progress_flag: Some(0), // COMPLETE - channel closure is complete
                                progress_status: Some("Channel closed".to_string()),
                                is_ok: Some(outcome_details.starts_with("normal")), // true for normal exit, false for errors
                            };
                            if let Err(e) = sender.send(signal_msg) {
                                error!("Failed to send channel_closed signal (from on_data_channel) to Python: {} (tube_id: {}, channel_label: {})", e, tube_id_for_log, label_clone_for_run);
                            } else {
                                debug!("Successfully sent channel_closed signal (from on_data_channel) to Python. (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);
                            }
                        } else {
                            warn!("No signal_sender on peer_connection for channel_closed signal (from on_data_channel). (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);
                        }
                    } else {
                        debug!("Peer_connection was None, cannot send channel_closed signal (from on_data_channel). (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);
                    }

                    debug!("on_data_channel: channel.run() task finished and cleaned up. (tube_id: {}, channel_label: {})", tube_id_for_log, label_clone_for_run);
                });

                debug!("on_data_channel: Successfully set up and spawned channel task. (tube_id: {}, channel_label: {})", tube.id, rtc_data_channel_label);
            })
        }));

        // Now get the lock
        let mut pc = self.peer_connection.lock().await;
        *pc = Some(connection_arc);

        // Update status
        *self.status.write().await = TubeStatus::Connecting;

        // Print debug status
        debug!("Updated tube status to: {:?}", self.status().await);

        // Add a small delay to ensure any pending operations complete
        tokio::time::sleep(Duration::from_millis(10)).await;

        Ok(())
    }

    // Get tube ID
    pub fn id(&self) -> String {
        self.id.clone()
    }

    // Get callback tokens from all active channels (for refresh_connections)
    // Channels report their own tokens, tube just aggregates them
    pub async fn get_callback_tokens(&self) -> Vec<String> {
        let channels_guard = self.active_channels.read().await;
        let mut tokens = Vec::new();

        for (_channel_name, metadata) in channels_guard.iter() {
            if let Some(ref token) = metadata.callback_token {
                tokens.push(token.clone());
            }
        }

        debug!(
            "Collected callback tokens from {} active channels (tube_id: {}, token_count: {})",
            channels_guard.len(),
            self.id,
            tokens.len()
        );
        tokens
    }

    // Get KSM config from active channels
    pub async fn get_ksm_config_from_channels(&self) -> Option<String> {
        let channels_guard = self.active_channels.read().await;

        // Get ksm_config from the first channel that has one
        for (_channel_name, metadata) in channels_guard.iter() {
            if let Some(ref config) = metadata.ksm_config {
                debug!(
                    "Found KSM config from active channel (tube_id: {})",
                    self.id
                );
                return Some(config.clone());
            }
        }

        debug!(
            "No KSM config found in any active channel (tube_id: {})",
            self.id
        );
        None
    }

    // Get reference to peer connection
    #[cfg(test)]
    pub(crate) async fn peer_connection(&self) -> Option<Arc<WebRTCPeerConnection>> {
        let pc = self.peer_connection.lock().await;
        pc.clone()
    }

    // Add a data channel
    pub(crate) async fn add_data_channel(&self, data_channel: WebRTCDataChannel) -> Result<()> {
        let label = data_channel.label();

        // If this is the control channel, set it specially
        if label == "control" {
            *self.control_channel.write().await = Some(data_channel.clone());
        }

        // Add to the channel map
        self.data_channels.write().await.insert(label, data_channel);
        Ok(())
    }

    // Get data channel by label
    #[cfg(test)]
    pub(crate) async fn get_data_channel(&self, label: &str) -> Option<WebRTCDataChannel> {
        self.data_channels.read().await.get(label).cloned()
    }

    // Create a new data channel and add it to the tube
    pub(crate) async fn create_data_channel(
        self: &Arc<Self>,
        label: &str,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
    ) -> Result<WebRTCDataChannel> {
        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc) = &*pc_guard {
            let rtc_data_channel = create_data_channel(&pc.peer_connection, label).await?;
            let data_channel = WebRTCDataChannel::new(rtc_data_channel);

            // Set up a message handler with zero-copy using the buffer pool
            self.setup_data_channel_handlers(
                &data_channel,
                label.to_string(),
                ksm_config,
                callback_token,
                client_version,
            );

            // Clone for release outside the lock
            let data_channel_clone = data_channel.clone();

            // Release lock before adding to avoid potential deadlock
            drop(pc_guard);

            // Add to our mapping
            self.add_data_channel(data_channel.clone()).await?;

            Ok(data_channel_clone)
        } else {
            Err(anyhow!("No peer connection available"))
        }
    }

    // Setup event handlers for a data channel
    fn setup_data_channel_handlers(
        self: &Arc<Self>,
        data_channel: &WebRTCDataChannel,
        label: String,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
    ) {
        // Store references directly where possible
        let dc_ref = &data_channel.data_channel;

        // Set up a state change handler-use string literals to avoid clones
        let label_for_open = label.clone();
        let ksm_config_for_open = ksm_config.clone();
        let callback_token_for_open = callback_token.clone();
        let client_version_for_open = client_version.to_string();
        let self_clone_for_open = Arc::clone(self);

        dc_ref.on_open(Box::new(move || {
            let label_clone = label_for_open.clone();
            let ksm_config_clone = ksm_config_for_open.clone();
            let callback_token_clone = callback_token_for_open.clone();
            let client_version_clone = client_version_for_open.clone();
            let self_clone = self_clone_for_open.clone();

            Box::pin(async move {
                info!("Data channel '{}' opened", label_clone);
                if let Err(e) = self_clone
                    .report_connection_open(
                        ksm_config_clone,
                        callback_token_clone,
                        &client_version_clone,
                    )
                    .await
                {
                    error!("Failed to report connection open: {}", e);
                }
            })
        }));

        let self_clone_for_close = Arc::clone(self);
        let client_version_for_close = client_version.to_string();

        dc_ref.on_close(Box::new(move || {
            let label_clone = label.clone();
            let ksm_config_clone = ksm_config.clone();
            let callback_token_clone = callback_token.clone();
            let client_version_clone = client_version_for_close.clone();
            let self_clone = self_clone_for_close.clone();

            Box::pin(async move {
                info!("Data channel '{}' closed", label_clone);
                if let Err(e) = self_clone
                    .report_connection_close(
                        ksm_config_clone,
                        callback_token_clone,
                        &client_version_clone,
                    )
                    .await
                {
                    error!("Failed to report connection close: {}", e);
                }
            })
        }));
    }

    // Report connection open state to router
    pub(crate) async fn report_connection_open(
        &self,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
    ) -> std::result::Result<(), String> {
        if self.is_server_mode_context {
            return Ok(());
        }
        if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
            debug!(
                "TEST MODE: Skipping report_connection_open for ksm_config: {}",
                ksm_config
            );
            return Ok(());
        }
        debug!("Sending connection open callback to router");
        let token_value = serde_json::Value::String(callback_token);

        match post_connection_state(
            &ksm_config,
            "connection_open",
            &token_value,
            None,
            client_version,
            None, // recording_duration
            None, // closure_reason
            None, // ai_overall_risk_level
            None, // ai_overall_summary
        )
        .await
        {
            Ok(_) => {
                debug!("Connection open callback sent successfully");
                Ok(())
            }
            Err(e) => {
                error!("Error sending connection open callback: {}", e);
                Err(format!("Failed to send connection open callback: {e}"))
            }
        }
    }

    pub(crate) async fn report_connection_close(
        &self,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
    ) -> std::result::Result<(), String> {
        if self.is_server_mode_context {
            return Ok(());
        }
        if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
            debug!(
                "TEST MODE: Skipping report_connection_close for ksm_config: {}",
                ksm_config
            );
            return Ok(());
        }
        // Report connection close to router if configuration exists
        debug!("Sending connection close callback to router");
        let token_value = serde_json::Value::String(callback_token);

        // Fall back to direct API call
        match post_connection_state(
            &ksm_config,
            "connection_close",
            &token_value,
            Some(true),
            client_version,
            None, // recording_duration
            None, // closure_reason
            None, // ai_overall_risk_level
            None, // ai_overall_summary
        )
        .await
        {
            Ok(_) => {
                debug!("Connection close callback sent successfully");
                Ok(())
            }
            Err(e) => {
                error!("Error sending connection close callback: {}", e);
                Err(e.to_string())
            }
        }
    }

    // Create a channel with the given name, using an existing data channel
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_channel(
        self: &Arc<Self>,
        name: &str,
        data_channel: &WebRTCDataChannel,
        timeout_seconds: Option<f64>,
        protocol_settings: HashMap<String, serde_json::Value>,
        callback_token: Option<String>,
        ksm_config: Option<String>,
        client_version: Option<String>,
    ) -> Result<Option<u16>> {
        debug!(
            "create_channel: Called. (tube_id: {}, channel_name: {})",
            self.id, name
        );
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!("create_channel: Initial parameters. (tube_id: {}, channel_name: {}, timeout_seconds: {:?}, protocol_settings: {:?})", self.id, name, timeout_seconds, protocol_settings);
        }

        // Register connection with metrics system
        crate::metrics::METRICS_COLLECTOR.register_connection(name.to_string(), self.id.clone());
        debug!(
            "Registered channel with metrics system (tube_id: {}, channel_name: {})",
            self.id, name
        );

        let timeouts = timeout_seconds.map(|timeout| TunnelTimeouts {
            read: Duration::from_secs_f64(timeout),
            guacd_handshake: Duration::from_secs_f64(timeout / 1.5),
        });
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "create_channel: Timeouts configured. (tube_id: {}, channel_name: {}, timeouts: {:?})",
                self.id,
                name,
                timeouts
            );
        }

        debug!("create_channel: About to call setup_channel_for_data_channel. (tube_id: {}, channel_name: {})", self.id, name);
        let client_version = match client_version {
            Some(version) => version,
            None => {
                error!("client_version is required for channel creation but was not provided (tube_id: {}, channel_name: {})", self.id, name);
                return Err(anyhow!("client_version is required for channel creation"));
            }
        };

        // Get peer connection for activity tracking
        let peer_connection = {
            let pc_guard = self.peer_connection.lock().await;
            if let Some(pc) = &*pc_guard {
                pc.clone()
            } else {
                return Err(anyhow!(
                    "No peer connection available for activity tracking"
                ));
            }
        };

        let setup_result = setup_channel_for_data_channel(
            data_channel,
            &peer_connection,
            name.to_string(),
            timeouts,
            protocol_settings.clone(), // protocol_settings is already cloned if needed by the caller or passed as value
            self.is_server_mode_context,
            callback_token,
            ksm_config,
            client_version,
        )
        .await;

        let mut owned_channel = match setup_result {
            Ok(ch_instance) => {
                debug!("create_channel: setup_channel_for_data_channel successful. (tube_id: {}, channel_name: {})", self.id, name);
                ch_instance
            }
            Err(e) => {
                error!("create_channel: setup_channel_for_data_channel failed: {} (tube_id: {}, channel_name: {})", e, self.id, name);
                return Err(e); // Propagate the error from setup_channel_for_data_channel
            }
        };

        // Register the channel metadata with the tube for tracking
        let metadata = ChannelMetadata {
            callback_token: owned_channel.callback_token.clone(),
            ksm_config: owned_channel.ksm_config.clone(),
            client_version: owned_channel.client_version.clone(),
            recordings_enabled: protocol_settings
                .get("guacd_params")
                .and_then(|v| v.as_object())
                .and_then(|params| params.get("recordingenabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        if let Err(e) = self
            .register_channel_metadata(name.to_string(), metadata)
            .await
        {
            error!("create_channel: Failed to register channel metadata: {} (tube_id: {}, channel_name: {})", e, self.id, name);
            return Err(e);
        }
        debug!(
            "create_channel: Channel metadata registered with tube (tube_id: {}, channel_name: {})",
            self.id, name
        );
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!("create_channel: Channel details after setup. (tube_id: {}, channel_name: {}, active_protocol: {:?}, local_listen_addr: {:?}, server_mode: {})", self.id, name, owned_channel.active_protocol, owned_channel.local_listen_addr, owned_channel.server_mode);
        }

        // Store the shutdown signal for this channel
        let shutdown_signal = Arc::clone(&owned_channel.should_exit);
        self.channel_shutdown_signals
            .write()
            .await
            .insert(name.to_string(), shutdown_signal);
        debug!(
            "create_channel: Shutdown signal stored for channel. (tube_id: {}, channel_name: {})",
            self.id, name
        );

        let mut actual_listening_port: Option<u16> = None;

        if owned_channel.server_mode {
            if let Some(listen_addr_str) = owned_channel.local_listen_addr.clone() {
                if !listen_addr_str.is_empty()
                    && matches!(
                        owned_channel.active_protocol,
                        crate::channel::types::ActiveProtocol::PortForward
                            | crate::channel::types::ActiveProtocol::Socks5
                            | crate::channel::types::ActiveProtocol::Guacd
                    )
                // Assuming Guacamole might be server mode too
                {
                    debug!("create_channel: Channel is server mode, attempting to start server. (tube_id: {}, channel_name: {}, protocol: {:?}, listen_addr: {})", self.id, name, owned_channel.active_protocol, listen_addr_str);
                    match owned_channel.start_server(&listen_addr_str).await {
                        Ok(socket_addr) => {
                            actual_listening_port = Some(socket_addr.port());
                            debug!("create_channel: Server started successfully. (tube_id: {}, channel_name: {}, listen_port: {})", self.id, name, actual_listening_port.unwrap());
                        }
                        Err(e) => {
                            error!("create_channel: Failed to start server: {}. Channel will not listen. (tube_id: {}, channel_name: {}, listen_addr: {})", e, self.id, name, listen_addr_str);
                            self.channel_shutdown_signals.write().await.remove(name);
                            return Err(anyhow!(
                                "Failed to start server for channel {}: {}",
                                name,
                                e
                            ));
                        }
                    }
                } else {
                    debug!("create_channel: Server mode channel, but no listen address or not a server-type protocol, skipping start_server. (tube_id: {}, channel_name: {}, protocol: {:?}, listen_addr: {:?})", self.id, name, owned_channel.active_protocol, owned_channel.local_listen_addr);
                }
            } else {
                debug!("create_channel: Server mode channel, but local_listen_addr is None. (tube_id: {}, channel_name: {})", self.id, name);
            }
        } else {
            debug!(
                "create_channel: Channel is not server_mode. (tube_id: {}, channel_name: {})",
                self.id, name
            );
        }

        let name_clone = name.to_string();
        let runtime_clone = self.runtime.clone();
        let tube_id_for_spawn = self.id.clone(); // Clone self.id here to make it 'static
        let peer_connection_for_spawn = Arc::clone(&self.peer_connection); // Clone peer_connection
        debug!(
            "create_channel: Spawning channel.run() task. (tube_id: {}, channel_name: {})",
            self.id, name_clone
        );
        let tube_arc = Arc::clone(self); // Clone the Arc for the spawned task
        runtime_clone.spawn(async move {
            // Use the cloned tube_id_for_spawn which is 'static
            debug!("create_channel: channel.run() task started. (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);

            // Only send connection_open callback for client mode channels
            if let Err(e) = tube_arc.send_connection_open_callback(&name_clone).await {
                warn!("Failed to send connection_open callback: {} (tube_id: {}, channel_name: {})", e, tube_id_for_spawn, name_clone);
            }

            // Clone the Arc so we can access it after run() consumes the channel
            let close_reason_arc = owned_channel.channel_close_reason.clone();
            let run_result = owned_channel.run().await;
            // Get the close reason after run completes - use try_lock to avoid blocking
            let close_reason = close_reason_arc.try_lock().ok().and_then(|guard| *guard);

            let outcome_details: String = match &run_result {
                Ok(()) => {
                    info!("Channel '{}' ran and exited normally. Signaling Python. (tube_id: {}, channel_name: {})", name_clone, tube_id_for_spawn, name_clone);
                    "normal_exit".to_string()
                }
                Err(crate::error::ChannelError::CriticalUpstreamClosed(closed_channel_id_from_err)) => {
                    warn!("Channel '{}' exited due to critical upstream closure. Signaling Python. (tube_id: {}, channel_name: {}, channel_id_in_err: {})", name_clone, tube_id_for_spawn, name_clone, closed_channel_id_from_err);
                    format!("critical_upstream_closed: {closed_channel_id_from_err}")
                }
                Err(e) => {
                    error!("Channel '{}' encountered an error in run(): {}. Signaling Python. (tube_id: {}, channel_name: {})", name_clone, e, tube_id_for_spawn, name_clone);
                    format!("error: {e}")
                }
            };

            // Send connection_close callback when channel finishes
            if let Err(e) = tube_arc.send_connection_close_callback(&name_clone).await {
                warn!("Failed to send connection_close callback: {} (tube_id: {}, channel_name: {})", e, tube_id_for_spawn, name_clone);
            }

            // Deregister the channel from the tube
            tube_arc.deregister_channel(&name_clone).await;

            // Remove shutdown signal for this channel
            tube_arc.remove_channel_shutdown_signal(&name_clone).await;

            // Always send a signal when channel.run() finishes, regardless of reason.
            let pc_guard = peer_connection_for_spawn.lock().await;
            if let Some(pc_instance_arc) = &*pc_guard {
                if let Some(sender) = &pc_instance_arc.signal_sender {
                    let mut signal_json = serde_json::json!({
                        "channel_id": name_clone, // This is the label of the channel that finished
                        "outcome": outcome_details
                    });
                    // Add close reason if available
                    if let Some(reason) = close_reason {
                        signal_json["close_reason"] = serde_json::json!({
                            "code": reason as u16,
                            "name": format!("{:?}", reason),
                            "is_critical": reason.is_critical(),
                            "is_retryable": reason.is_retryable(),
                        });
                    }
                    let signal_data = signal_json.to_string();

                    let signal_msg = SignalMessage {
                        tube_id: tube_id_for_spawn.clone(),
                        kind: "channel_closed".to_string(), // Generic kind for any channel closure
                        data: signal_data,
                        conversation_id: tube_arc.get_conversation_id_for_channel(&name_clone).unwrap_or_else(|| {
                            debug!("No conversation_id mapping found, using channel name (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);
                            name_clone.clone()
                        }),
                        progress_flag: Some(0), // COMPLETE - channel closure is complete
                        progress_status: Some("Channel closed".to_string()),
                        is_ok: Some(outcome_details.starts_with("normal")), // true for normal exit, false for errors
                    };
                    if let Err(e) = sender.send(signal_msg) {
                        error!("Failed to send channel_closed signal to Python: {} (tube_id: {}, channel_name: {})", e, tube_id_for_spawn, name_clone);
                    } else {
                        debug!("Successfully sent channel_closed signal to Python. (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);
                    }
                } else {
                    warn!("No signal_sender found on peer_connection to send channel_closed signal. (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);
                }
            } else {
                warn!("Peer_connection was None, cannot send channel_closed signal. (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);
            }

            debug!("create_channel: channel.run() task finished and cleaned up. (tube_id: {}, channel_name: {})", tube_id_for_spawn, name_clone);
        });
        debug!("create_channel: Successfully set up and spawned channel task. Returning listening port. (tube_id: {}, channel_name: {}, actual_listening_port: {:?})", self.id, name, actual_listening_port);
        Ok(actual_listening_port)
    }

    // Create the default control channel
    #[cfg(test)] // Only used in tests
    pub(crate) async fn create_control_channel(
        self: &Arc<Self>,
        ksm_config: String,
        callback_token: String,
        client_version: &str,
    ) -> Result<WebRTCDataChannel> {
        let control_channel = self
            .create_data_channel("control", ksm_config, callback_token, client_version)
            .await?;
        *self.control_channel.write().await = Some(control_channel.clone());
        Ok(control_channel)
    }

    // Close a specific channel by signaling its run loop to exit
    pub(crate) async fn close_channel(
        &self,
        name: &str,
        reason: Option<CloseConnectionReason>,
    ) -> Result<()> {
        let reason = reason.unwrap_or(CloseConnectionReason::AdminClosed);
        // First, try to send a CloseConnection message with the specified reason
        // This ensures the remote side knows why it was closed
        let data_channels = self.data_channels.read().await;
        if let Some(channel) = data_channels.get(name) {
            // Send CloseConnection for the control connection with specified reason
            let mut close_data = Vec::with_capacity(5);
            close_data.extend_from_slice(&0u32.to_be_bytes()); // conn_no 0 (control connection)
            close_data.push(reason as u8); // reason - 1 byte

            // We need a buffer pool for frame creation
            let buffer_pool = BufferPool::default();
            let close_frame = Frame::new_control_with_pool(
                ControlMessage::CloseConnection,
                &close_data,
                &buffer_pool,
            );

            let encoded = close_frame.encode_with_pool(&buffer_pool);
            let _ = channel.send(encoded).await; // Ignore errors if channel is already closing

            // Give the close frame time to be transmitted before signaling shutdown
            // This ensures the remote side receives the close reason
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(data_channels);

        // Then signal the channel to exit
        let mut signals = self.channel_shutdown_signals.write().await;
        if let Some(signal_arc) = signals.remove(name) {
            // Remove from the map once signaled
            info!(
                "Tube {}: Signaling channel '{}' to close with {:?} reason.",
                self.id, name, reason
            );
            signal_arc.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        } else {
            // Idempotent: If channel already closed or never existed, that's OK
            warn!("Tube {}: No shutdown signal found for channel '{}' during close_channel. Channel may be already closed or was never created. Treating as idempotent operation.", self.id, name);
            Ok(()) // Don't error - close is idempotent
        }
    }

    // Common helper function for offer/answer creation with ICE gathering
    async fn create_session_description(&self, is_offer: bool) -> Result<String, String> {
        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc_arc) = &*pc_guard {
            // Call the unified (now pub(crate)) method in WebRTCPeerConnection
            let sdp = pc_arc.create_description_with_checks(is_offer).await?;

            // If using trickle ICE, we still need to set the local description here with the initial SDP.
            // For non-trickle ICE, create_description_with_checks (via generate_sdp_and_maybe_gather_ice)
            // already handled setting the local description.
            if pc_arc.trickle_ice {
                // trickle_ice was made pub(crate) by the user
                debug!("Trickle ICE: Setting local description in Tube::create_session_description (tube_id: {})", self.id);
                pc_arc.set_local_description(sdp.clone(), !is_offer).await?;
            } else {
                debug!("Non-trickle ICE: Local description already set and finalized. Skipping redundant set_local_description in Tube. (tube_id: {})", self.id);
            }

            Ok(sdp)
        } else {
            Err("No peer connection available".to_string())
        }
    }

    // Create an offer
    pub(crate) async fn create_offer(&self) -> Result<String, String> {
        self.create_session_description(true).await
    }

    // Create an answer and send via a signal channel if available
    pub(crate) async fn create_answer(&self) -> Result<String, String> {
        self.create_session_description(false).await
    }

    // Set remote description
    pub(crate) async fn set_remote_description(
        &self,
        sdp: String,
        is_answer: bool,
    ) -> Result<(), String> {
        debug!("[SDP_DEBUG] set_remote_description called (tube_id: {}, is_answer: {}, is_server_mode: {}, sdp_length: {})",
            self.id, is_answer, self.is_server_mode_context, sdp.len());

        // Check if SDP contains data channel information
        if sdp.contains("m=application") {
            debug!("[SDP_DEBUG] SDP contains data channel (m=application) - on_data_channel should fire (tube_id: {})", self.id);
        } else {
            warn!("[SDP_DEBUG] SDP does NOT contain data channel (m=application) - on_data_channel will not fire (tube_id: {})", self.id);
        }

        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc) = &*pc_guard {
            // Use the WebRTCPeerConnection wrapper method instead of bypassing to raw library
            // This ensures activity updates, candidate flushing, and ICE restart completion
            pc.set_remote_description(sdp, is_answer)
                .await
                .map_err(|e| format!("Failed to set remote description: {e:?}"))
        } else {
            Err("No peer connection available".to_string())
        }
    }

    // Add an ICE candidate
    pub(crate) async fn add_ice_candidate(&self, candidate: String) -> Result<(), String> {
        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc) = &*pc_guard {
            pc.add_ice_candidate(candidate).await
        } else {
            Err("No peer connection available".to_string())
        }
    }

    // Get status
    pub async fn status(&self) -> TubeStatus {
        *self.status.read().await
    }

    // Register a channel metadata with this tube for tracking purposes
    pub async fn register_channel_metadata(
        &self,
        channel_name: String,
        metadata: ChannelMetadata,
    ) -> Result<()> {
        let mut channels_guard = self.active_channels.write().await;
        channels_guard.insert(channel_name.clone(), metadata);
        debug!(
            "Registered channel metadata with tube (tube_id: {}, channel_name: {})",
            self.id, channel_name
        );
        Ok(())
    }

    // Deregister a channel from this tube
    pub async fn deregister_channel(&self, channel_name: &str) {
        let mut channels_guard = self.active_channels.write().await;
        if channels_guard.remove(channel_name).is_some() {
            // Unregister connection from metrics system
            crate::metrics::METRICS_COLLECTOR.unregister_connection(channel_name);
            debug!(
                "Unregistered channel from metrics system (tube_id: {}, channel_name: {})",
                self.id, channel_name
            );

            info!(
                "Deregistered channel from tube (tube_id: {}, channel_name: {})",
                self.id, channel_name
            );
        } else {
            debug!("Attempted to deregister channel that wasn't registered (tube_id: {}, channel_name: {})", self.id, channel_name);
        }
    }

    // Remove shutdown signal for a channel
    pub async fn remove_channel_shutdown_signal(&self, channel_name: &str) {
        let mut signals_guard = self.channel_shutdown_signals.write().await;
        if signals_guard.remove(channel_name).is_some() {
            debug!(
                "Removed shutdown signal for channel (tube_id: {}, channel_name: {})",
                self.id, channel_name
            );
        } else {
            debug!(
                "No shutdown signal found to remove for channel (tube_id: {}, channel_name: {})",
                self.id, channel_name
            );
        }
    }

    // Send connection_open callback for a specific channel
    pub async fn send_connection_open_callback(&self, channel_name: &str) -> Result<()> {
        if self.is_server_mode_context {
            return Ok(());
        }
        let channels_guard = self.active_channels.read().await;
        if let Some(metadata) = channels_guard.get(channel_name) {
            if let (Some(ref ksm_config), Some(ref callback_token)) =
                (&metadata.ksm_config, &metadata.callback_token)
            {
                let client_version = &metadata.client_version;

                // Skip if in test mode
                if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
                    debug!("TEST MODE: Skipping connection_open callback (tube_id: {}, channel_name: {})", self.id, channel_name);
                    return Ok(());
                }

                debug!(
                    "Sending connection_open callback to router (tube_id: {}, channel_name: {})",
                    self.id, channel_name
                );
                let token_value = serde_json::Value::String(callback_token.clone());

                match post_connection_state(
                    ksm_config,
                    "connection_open",
                    &token_value,
                    None,
                    client_version,
                    None, // recording_duration
                    None, // closure_reason
                    None, // ai_overall_risk_level
                    None, // ai_overall_summary
                )
                .await
                {
                    Ok(_) => {
                        debug!("Connection open callback sent successfully (tube_id: {}, channel_name: {})", self.id, channel_name);
                        Ok(())
                    }
                    Err(e) => {
                        error!("Error sending connection open callback: {} (tube_id: {}, channel_name: {})", e, self.id, channel_name);
                        Err(anyhow!("Failed to send connection open callback: {e}"))
                    }
                }
            } else {
                warn!("Channel missing ksm_config or callback_token for connection_open callback (tube_id: {}, channel_name: {})", self.id, channel_name);
                Ok(())
            }
        } else {
            Err(anyhow!(
                "Channel {} not found in tube {}",
                channel_name,
                self.id
            ))
        }
    }

    // Send connection_close callback for a specific channel
    pub async fn send_connection_close_callback(&self, channel_name: &str) -> Result<()> {
        // Check if recordings are enabled for this channel - skip if recordings are enabled
        let should_skip = {
            let channels_guard = self.active_channels.read().await;
            if let Some(metadata) = channels_guard.get(channel_name) {
                metadata.recordings_enabled
            } else {
                false // If channel not found, don't skip
            }
        };

        if should_skip {
            debug!("Skipping connection_close callback - recordings are enabled for this channel (tube_id: {}, channel_name: {})", self.id, channel_name);
            return Ok(());
        }

        // Check if this tube/connection is already closed/closing to prevent duplicate callbacks
        let current_status = *self.status.read().await;
        if matches!(
            current_status,
            TubeStatus::Closing | TubeStatus::Closed | TubeStatus::Failed
        ) {
            debug!("Skipping connection_close callback - tube already closed/closing/failed (tube_id: {}, channel_name: {}, status: {:?})", self.id, channel_name, current_status);
            return Ok(());
        }

        if self.is_server_mode_context {
            return Ok(());
        }
        let channels_guard = self.active_channels.read().await;
        if let Some(metadata) = channels_guard.get(channel_name) {
            if let (Some(ref ksm_config), Some(ref callback_token)) =
                (&metadata.ksm_config, &metadata.callback_token)
            {
                let client_version = &metadata.client_version;

                // Skip if in test mode
                if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
                    debug!("TEST MODE: Skipping connection_close callback (tube_id: {}, channel_name: {})", self.id, channel_name);
                    return Ok(());
                }

                debug!(
                    "Sending connection_close callback to router (tube_id: {}, channel_name: {})",
                    self.id, channel_name
                );
                let token_value = serde_json::Value::String(callback_token.clone());

                match post_connection_state(
                    ksm_config,
                    "connection_close",
                    &token_value,
                    Some(true),
                    client_version,
                    None, // recording_duration
                    None, // closure_reason
                    None, // ai_overall_risk_level
                    None, // ai_overall_summary
                )
                .await
                {
                    Ok(_) => {
                        debug!("Connection close callback sent successfully (tube_id: {}, channel_name: {})", self.id, channel_name);
                        Ok(())
                    }
                    Err(e) => {
                        error!("Error sending connection close callback: {} (tube_id: {}, channel_name: {})", e, self.id, channel_name);
                        Err(anyhow!("Failed to send connection close callback: {e}"))
                    }
                }
            } else {
                warn!("Channel missing ksm_config or callback_token for connection_close callback (tube_id: {}, channel_name: {})", self.id, channel_name);
                Ok(())
            }
        } else {
            debug!("Channel not found when trying to send connection_close callback (tube_id: {}, channel_name: {})", self.id, channel_name);
            Ok(())
        }
    }

    // ICE restart method for connection recovery
    pub async fn restart_ice(&self) -> Result<String, String> {
        use crate::webrtc_errors::WebRTCError;

        let pc_guard = self.peer_connection.lock().await;
        if let Some(ref pc) = *pc_guard {
            pc.restart_ice().await.map_err(|e| match e {
                WebRTCError::IceRestartFailed { reason, .. } => reason,
                WebRTCError::CircuitBreakerOpen { .. } => {
                    "ICE restart circuit breaker is open".to_string()
                }
                _ => e.to_string(),
            })
        } else {
            Err("No peer connection available for ICE restart".to_string())
        }
    }

    // Get connection statistics
    pub async fn get_connection_stats(&self) -> Result<ConnectionStats, String> {
        let pc_guard = self.peer_connection.lock().await;
        if let Some(ref pc) = *pc_guard {
            // Get WebRTC stats if available
            let reports = pc.peer_connection.get_stats().await;
            let mut stats = ConnectionStats::default();

            // Parse WebRTC stats reports for relevant metrics
            for (_id, report) in reports.reports.iter() {
                match report {
                    webrtc::stats::StatsReportType::InboundRTP(inbound) => {
                        stats.bytes_received += inbound.bytes_received;
                    }
                    webrtc::stats::StatsReportType::OutboundRTP(outbound) => {
                        stats.bytes_sent += outbound.bytes_sent;
                    }
                    webrtc::stats::StatsReportType::RemoteInboundRTP(remote_inbound) => {
                        // Use remote inbound stats for packet loss)
                        stats.packet_loss_rate = remote_inbound.fraction_lost;
                        if let Some(rtt) = remote_inbound.round_trip_time {
                            stats.rtt_ms = Some(rtt * 1000.0); // Convert to milliseconds
                        }
                    }
                    webrtc::stats::StatsReportType::CandidatePair(pair) => {
                        if pair.nominated {
                            stats.rtt_ms = Some(pair.current_round_trip_time * 1000.0);
                            // Convert to milliseconds
                        }
                    }
                    _ => {} // Ignore other stat types
                }
            }

            // Update metrics collector with the latest WebRTC stats for all conversations associated with this tube
            // This ensures that metrics stay up-to-date when stats are requested
            if let Some(conversation_id) = &self.original_conversation_id {
                crate::metrics::METRICS_COLLECTOR
                    .update_webrtc_stats(conversation_id, &reports.reports);
            }

            Ok(stats)
        } else {
            Err("No peer connection available for stats".to_string())
        }
    }

    /// Check if this tube has any active channels
    pub async fn has_active_channels(&self) -> bool {
        let channels_guard = self.active_channels.read().await;
        !channels_guard.is_empty()
    }

    /// Check if this tube appears to be stale/abandoned
    /// A tube is considered stale if it has no active channels AND is in a terminal state
    pub async fn is_stale(&self) -> bool {
        // Tube is stale if:
        // 1. No active channels
        // 2. Status is Failed/Closed/Disconnected

        let has_channels = self.has_active_channels().await;
        if has_channels {
            return false; // Has active channels, not stale
        }

        let status = *self.status.read().await;
        matches!(
            status,
            TubeStatus::Failed | TubeStatus::Closed | TubeStatus::Disconnected
        )
    }

    /// Check if tube has been inactive for specified duration
    /// Used for zombie detection when data channel close events don't fire
    /// Returns true if no WebRTC activity for the given duration
    pub async fn is_inactive_for_duration(&self, duration: std::time::Duration) -> bool {
        if let Ok(pc_guard) = self.peer_connection.try_lock() {
            if let Some(pc) = &*pc_guard {
                return pc.time_since_last_activity() > duration;
            }
        }
        false // Can't check, assume active to be safe
    }

    /// Get current peer connection state (for zombie detection)
    /// Returns None if peer connection is not available or locked
    pub async fn get_connection_state(&self) -> Option<RTCPeerConnectionState> {
        if let Ok(pc_guard) = self.peer_connection.try_lock() {
            pc_guard
                .as_ref()
                .map(|pc| pc.peer_connection.connection_state())
        } else {
            None // Can't get lock
        }
    }

    /// Get comprehensive circuit breaker statistics
    pub async fn get_circuit_breaker_stats(
        &self,
    ) -> Result<crate::webrtc_circuit_breaker::CircuitBreakerStats, String> {
        let pc_guard = self.peer_connection.lock().await;
        if let Some(ref pc) = *pc_guard {
            Ok(pc.get_comprehensive_circuit_breaker_stats())
        } else {
            Err("Peer connection not available".to_string())
        }
    }

    /// Check if circuit breaker is healthy (closed state)
    pub async fn is_circuit_breaker_healthy(&self) -> Result<bool, String> {
        let pc_guard = self.peer_connection.lock().await;
        if let Some(ref pc) = *pc_guard {
            Ok(pc.is_circuit_breaker_healthy())
        } else {
            Err("Peer connection not available".to_string())
        }
    }

    /// Explicit async close - MUST be called before dropping Tube
    ///
    /// This ensures proper cleanup in the correct order:
    /// 1. Stop keepalive task (prevents spurious activity)
    /// 2. Close data channels (sends CloseConnection to remote peers)
    /// 3. Close peer connection (releases TURN allocations, stops refresh timers)
    ///
    /// CRITICAL: Prevents TURN "400 Bad Request" errors by ensuring allocations
    /// are properly deallocated before permission refresh timers fire.
    pub async fn close(&self) -> Result<(), String> {
        let tube_id = self.id.clone();
        info!("Tube {} explicit close starting", tube_id);

        // 1. Stop keepalive task to prevent spurious activity during shutdown
        if let Ok(mut task_guard) = self.keepalive_task.try_lock() {
            if let Some(task) = task_guard.take() {
                task.abort();
                debug!("Keepalive task stopped for tube {}", tube_id);
            }
        }

        // 2. Close all data channels (sends CloseConnection to downstream peers)
        let channels: Vec<_> = {
            let mut guard = self.data_channels.write().await;
            guard.drain().collect()
        };

        let mut closed_count = 0;
        for (label, dc) in channels {
            match tokio::time::timeout(Duration::from_secs(3), dc.close()).await {
                Ok(Ok(_)) => {
                    closed_count += 1;
                    debug!("Data channel {} closed for tube {}", label, tube_id);
                }
                Ok(Err(e)) => {
                    warn!(
                        "Error closing data channel {} for tube {}: {}",
                        label, tube_id, e
                    );
                }
                Err(_) => {
                    warn!(
                        "Timeout closing data channel {} for tube {} after 3s",
                        label, tube_id
                    );
                }
            }
        }

        // 3. Close peer connection (releases TURN allocation, stops refresh timers)
        let mut pc_guard = self.peer_connection.lock().await;
        if let Some(pc) = pc_guard.take() {
            match tokio::time::timeout(Duration::from_secs(5), pc.close()).await {
                Ok(Ok(_)) => {
                    info!(
                        "Tube {} closed successfully (data_channels: {}, TURN allocation released)",
                        tube_id, closed_count
                    );
                }
                Ok(Err(e)) => {
                    return Err(format!(
                        "Failed to close peer connection for tube {}: {}",
                        tube_id, e
                    ));
                }
                Err(_) => {
                    return Err(format!(
                        "Timeout closing peer connection for tube {} after 5s",
                        tube_id
                    ));
                }
            }
        } else {
            debug!("Peer connection already closed for tube {}", tube_id);
        }

        Ok(())
    }
}

// Connection statistics structure
#[derive(Debug, Default, Clone)]
pub struct ConnectionStats {
    pub packet_loss_rate: f64,
    pub rtt_ms: Option<f64>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl Drop for Tube {
    fn drop(&mut self) {
        debug!("===============================================");
        debug!("Tube {} entering RAII Drop (safety net)", self.id);
        debug!("===============================================");

        // ARCHITECTURE NOTE: Tube::close() should be called BEFORE dropping!
        // Drop is now just a safety net that cleans up what it can and warns about leaks.

        // 1. Signal sender drops automatically (RAII)
        if self.signal_sender.is_some() {
            drop(self.signal_sender.take());
            debug!("Signal channel auto-closed (tube_id: {})", self.id);
        }

        // 2. Metrics handle drops automatically (RAII)
        if let Ok(mut guard) = self.metrics_handle.try_lock() {
            if guard.is_some() {
                drop(guard.take());
                debug!("Metrics auto-unregistered (tube_id: {})", self.id);
            }
        }

        // 3. Keepalive task cancelled (best-effort)
        if let Ok(mut guard) = self.keepalive_task.try_lock() {
            if let Some(task) = guard.take() {
                task.abort();
                debug!("Keepalive task cancelled (tube_id: {})", self.id);
            }
        }

        // 4. Clear channel maps
        if let Ok(mut signals) = self.channel_shutdown_signals.try_write() {
            signals.clear();
        }
        if let Ok(mut channels) = self.active_channels.try_write() {
            channels.clear();
        }

        // 5. SAFETY NET: Check if close() was called (should be empty)
        if let Ok(guard) = self.data_channels.try_read() {
            if !guard.is_empty() {
                warn!(
                    "LEAK WARNING: Tube {} dropped without calling close() - {} data channels not properly closed! Downstream peers may not be notified.",
                    self.id, guard.len()
                );
            }
        }

        // 6. SAFETY NET: Check if peer connection was closed
        if let Ok(guard) = self.peer_connection.try_lock() {
            if guard.is_some() {
                warn!(
                    "LEAK WARNING: Tube {} dropped without calling close() - peer connection not properly closed! TURN allocation may leak, causing 400 Bad Request errors.",
                    self.id
                );
            }
        }

        debug!(
            "Tube {} Drop complete (close() should have been called first)",
            self.id
        );
    }
}
