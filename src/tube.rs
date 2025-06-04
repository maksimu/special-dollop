use crate::channel::Channel;
use crate::models::TunnelTimeouts;
use crate::router_helpers::post_connection_state;
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::{setup_channel_for_data_channel, TubeStatus};
use crate::tube_registry::SignalMessage;
use crate::webrtc_core::{create_data_channel, WebRTCPeerConnection};
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

// A single tube holding a WebRTC peer connection and channels
#[derive(Clone)]
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
    // Active channels registered with this tube
    pub(crate) active_channels: Arc<TokioRwLock<HashMap<String, Arc<TokioMutex<Channel>>>>>,
    // Indicates if this tube was created in a server or client context by its registry
    pub(crate) is_server_mode_context: bool,
    // Current status
    pub(crate) status: Arc<TokioRwLock<TubeStatus>>,
    // Runtime
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
    // Original conversation ID that created this tube (for control channel mapping)
    pub(crate) original_conversation_id: Option<String>,
}

impl Tube {
    // Create a new tube with the optional peer connection
    pub fn new(
        is_server_mode_context: bool,
        original_conversation_id: Option<String>,
    ) -> Result<Arc<Self>> {
        let id = Uuid::new_v4().to_string();
        let runtime = get_runtime();

        let tube = Arc::new(Self {
            id: id.clone(), // Clone ID to use below
            peer_connection: Arc::new(TokioMutex::new(None)),
            data_channels: Arc::new(TokioRwLock::new(HashMap::new())),
            control_channel: Arc::new(TokioRwLock::new(None)),
            channel_shutdown_signals: Arc::new(TokioRwLock::new(HashMap::new())),
            active_channels: Arc::new(TokioRwLock::new(HashMap::new())),
            is_server_mode_context,
            status: Arc::new(TokioRwLock::new(TubeStatus::Initializing)),
            runtime,
            original_conversation_id,
        });

        Ok(tube)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_peer_connection(
        &self,
        config: Option<RTCConfiguration>,
        trickle_ice: bool,
        turn_only: bool,
        ksm_config: String,
        callback_token: String,
        protocol_settings: HashMap<String, serde_json::Value>,
        signal_sender: UnboundedSender<SignalMessage>,
    ) -> Result<()> {
        info!(
            "[TUBE_DEBUG] Tube {}: create_peer_connection called. trickle_ice: {}, turn_only: {}",
            self.id, trickle_ice, turn_only
        );
        trace!(tube_id = %self.id, ?protocol_settings, "Create_peer_connection protocol_settings");

        let connection = WebRTCPeerConnection::new(
            config,
            trickle_ice,
            turn_only,
            Some(signal_sender),
            self.id.clone(), // Pass tube_id
        )
        .await
        .map_err(|e| anyhow!("{}", e))?;

        let connection_arc = Arc::new(connection);

        let status = self.status.clone();

        let tube_arc_for_pc_state = self.clone();

        info!("[TUBE_DEBUG] Tube {}: About to call setup_ice_candidate_handler. Callback token (used as conv_id before): {}", self.id, callback_token);
        connection_arc.setup_ice_candidate_handler();

        connection_arc.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let status_clone = status.clone();
            let tube_clone_for_closure = tube_arc_for_pc_state.clone();

            Box::pin(async move {
                let tube_id_log = tube_clone_for_closure.id.clone();
                match state {
                    RTCPeerConnectionState::Connected => {
                        *status_clone.write().await = TubeStatus::Active;
                        info!(tube_id = %tube_id_log, "Tube connection state changed to Active");
                    },
                    RTCPeerConnectionState::Failed |
                    RTCPeerConnectionState::Closed |
                    RTCPeerConnectionState::Disconnected => {

                        let current_status = *status_clone.read().await;
                        let new_status = match state {
                            RTCPeerConnectionState::Failed => TubeStatus::Failed,
                            RTCPeerConnectionState::Closed => TubeStatus::Closed,
                            RTCPeerConnectionState::Disconnected => TubeStatus::Disconnected,
                            _ => current_status, // Should not happen due to outer match
                        };

                        if current_status != TubeStatus::Closing &&
                           current_status != TubeStatus::Closed &&
                           current_status != TubeStatus::Failed &&
                           current_status != TubeStatus::Disconnected {

                            warn!(tube_id = %tube_id_log, old_status = ?current_status, new_state = ?state, "WebRTC peer connection state indicates closure/failure. Initiating Tube close.");
                            *status_clone.write().await = new_status;

                            // Get the global runtime to spawn the close task
                            let runtime = get_runtime(); // Ensure get_runtime() is accessible
                            runtime.spawn(async move {
                                debug!(tube_id = %tube_id_log, "Spawning task to close tube due to peer connection state change.");
                                let mut registry = crate::tube_registry::REGISTRY.write().await;
                                if let Err(e) = registry.close_tube(&tube_id_log).await {
                                     error!(tube_id = %tube_id_log, "Error trying to close tube via registry after peer connection failed/closed: {}", e);
                                } else {
                                     info!(tube_id = %tube_id_log, "Successfully initiated tube closure via registry after peer connection state change.");
                                }
                            });
                        } else {
                            debug!(tube_id = %tube_id_log, current_status = ?current_status, new_state = ?state, "Peer connection reached terminal state, but tube status indicates it's already closing or closed. No new action taken.");
                            // Ensure the status is updated if it wasn't already the final one
                            if *status_clone.read().await != new_status {
                                *status_clone.write().await = new_status;
                            }
                        }
                    },
                    _ => {
                        debug!(tube_id = %tube_id_log, "Connection state changed to: {:?}", state);
                    }
                }
            })
        }));

        // Set up a handler for incoming data channels
        let tube_clone = self.clone();
        let protocol_settings_clone_for_on_data_channel = protocol_settings.clone(); // Clone for the outer closure
        let callback_token_for_on_data_channel = callback_token.clone(); // Clone for on_data_channel
        let ksm_config_for_on_data_channel = ksm_config.clone(); // Clone for on_data_channel
        connection_arc.peer_connection.on_data_channel(Box::new(move |rtc_data_channel| {
            let tube = tube_clone.clone();
            // Use the protocol_settings cloned for the on_data_channel closure
            let protocol_settings_for_channel_setup = protocol_settings_clone_for_on_data_channel.clone();
            let callback_token_for_channel = callback_token_for_on_data_channel.clone();
            let ksm_config_for_channel = ksm_config_for_on_data_channel.clone();
            let rtc_data_channel_label = rtc_data_channel.label().to_string(); // Get the label once for logging
            let rtc_data_channel_id = rtc_data_channel.id();

            Box::pin(async move {
                info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, rtc_channel_id = ?rtc_data_channel_id, "on_data_channel: Received data channel from remote peer.");
                trace!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, ?protocol_settings_for_channel_setup, "on_data_channel: Protocol settings for this channel.");

                // Create our WebRTCDataChannel wrapper
                let data_channel = WebRTCDataChannel::new(rtc_data_channel);

                // Add it to our data channels map
                if let Err(e) = tube.add_data_channel(data_channel.clone()).await {
                    error!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Failed to add data channel to tube: {}", e);
                    return;
                }
                debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Data channel added to tube's map.");

                // If this is the control channel, store it specially
                if rtc_data_channel_label == "control" {
                    *tube.control_channel.write().await = Some(data_channel.clone());
                    info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Set as control channel.");
                }

                // Determine server_mode for the new channel based on the Tube's context
                let current_server_mode = tube.is_server_mode_context;
                debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, server_mode = current_server_mode, "on_data_channel: Determined server_mode for channel setup.");

                info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: About to call setup_channel_for_data_channel.");
                let channel_result = setup_channel_for_data_channel(
                    &data_channel,
                    rtc_data_channel_label.clone(),
                    None,
                    protocol_settings_for_channel_setup,
                    current_server_mode,
                    Some(callback_token_for_channel), // Use callback_token from tube creation
                    Some(ksm_config_for_channel), // Use ksm_config from tube creation
                    Some(Arc::new(tube.clone())), // Pass tube reference for registration
                ).await;

                let mut owned_channel = match channel_result {
                    Ok(ch_instance) => {
                        info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: setup_channel_for_data_channel successful.");
                        ch_instance
                    }
                    Err(e) => {
                        error!("Tube {}: Failed to setup channel for incoming data channel '{}': {}", tube.id, rtc_data_channel_label, e);
                        return;
                    }
                };
                trace!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, ?owned_channel.active_protocol, ?owned_channel.local_listen_addr, "on_data_channel: Channel details after setup.");

                // Store the shutdown signal for this newly created channel
                let shutdown_signal = Arc::clone(&owned_channel.should_exit);
                tube.channel_shutdown_signals.write().await.insert(rtc_data_channel_label.clone(), shutdown_signal);
                debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Shutdown signal stored for channel.");


                if owned_channel.server_mode {
                    if let Some(listen_addr_str) = owned_channel.local_listen_addr.clone() {
                        if !listen_addr_str.is_empty() &&
                           matches!(owned_channel.active_protocol, crate::channel::types::ActiveProtocol::PortForward | crate::channel::types::ActiveProtocol::Socks5 | crate::channel::types::ActiveProtocol::Guacd) // Assuming Guacamole might be server mode too
                        {
                            info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, protocol = ?owned_channel.active_protocol, listen_addr = %listen_addr_str, "on_data_channel: Channel is server mode, attempting to start server.");
                            match owned_channel.start_server(&listen_addr_str).await {
                                Ok(socket_addr) => {
                                    info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, listen_port = %socket_addr.port(), "on_data_channel: Server started successfully.");
                                }
                                Err(e) => {
                                    error!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, listen_addr = %listen_addr_str, "on_data_channel: Failed to start server: {}. Channel will not run effectively.", e);
                                    tube.channel_shutdown_signals.write().await.remove(&rtc_data_channel_label);
                                    return;
                                }
                            }
                        } else {
                            debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, protocol = ?owned_channel.active_protocol, listen_addr = ?owned_channel.local_listen_addr, "on_data_channel: Server mode channel, but no listen address or not a server-type protocol, skipping start_server.");
                        }
                    } else {
                         debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Server mode channel, but local_listen_addr is None.");
                    }
                } else {
                    debug!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Channel is not server_mode.");
                }

                let label_clone_for_run = rtc_data_channel_label.clone();
                let runtime_for_run = get_runtime();
                let tube_id_for_log = tube.id.clone();
                // Clone references for spawned task - avoid double Arc wrapping
                let tube_arc = Arc::new(tube.clone()); // Single Arc wrapping
                let peer_connection_for_signal = Arc::clone(&tube.peer_connection);

                info!(tube_id = %tube.id, channel_label = %label_clone_for_run, "on_data_channel: Spawning channel.run() task.");
                runtime_for_run.spawn(async move {
                    debug!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "on_data_channel: channel.run() task started.");

                    // Send connection_open callback when a channel starts running
                    if let Err(e) = tube_arc.send_connection_open_callback(&label_clone_for_run).await {
                        warn!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Failed to send connection_open callback: {}", e);
                    }

                    let run_result = owned_channel.run().await;

                    let outcome_details: String = match &run_result {
                        Ok(()) => {
                            info!(target: "lifecycle", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Channel '{}' (from on_data_channel) ran and exited normally. Signaling Python.", label_clone_for_run);
                            "normal_exit".to_string()
                        }
                        Err(crate::error::ChannelError::CriticalUpstreamClosed(closed_channel_id_from_err)) => {
                            warn!(target: "lifecycle", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, channel_id_in_err = %closed_channel_id_from_err, "Channel '{}' (from on_data_channel) exited due to critical upstream closure. Signaling Python.", label_clone_for_run);
                            format!("critical_upstream_closed: {}", closed_channel_id_from_err)
                        }
                        Err(e) => {
                            error!(target: "lifecycle", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Channel '{}' (from on_data_channel) encountered an error in run(): {}. Signaling Python.", label_clone_for_run, e);
                            format!("error: {}", e)
                        }
                    };

                    // Send connection_close callback when channel finishes
                    if let Err(e) = tube_arc.send_connection_close_callback(&label_clone_for_run).await {
                        warn!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Failed to send connection_close callback: {}", e);
                    }

                    // Deregister the channel from the tube
                    tube_arc.deregister_channel(&label_clone_for_run).await;

                    // Remove shutdown signal for this channel
                    tube_arc.remove_channel_shutdown_signal(&label_clone_for_run).await;

                    // Always send a signal when channel.run() finishes, regardless of reason.
                    let pc_guard = peer_connection_for_signal.lock().await;
                    if let Some(pc_instance_arc) = &*pc_guard {
                        if let Some(sender) = &pc_instance_arc.signal_sender {
                            let signal_data = serde_json::json!({
                                "channel_id": label_clone_for_run, // The label of the channel from on_data_channel
                                "outcome": outcome_details
                            }).to_string();

                            let signal_msg = SignalMessage {
                                tube_id: tube_id_for_log.clone(),
                                kind: "channel_closed".to_string(),
                                data: signal_data,
                                conversation_id: label_clone_for_run.clone(),
                            };
                            if let Err(e) = sender.send(signal_msg) {
                                error!(target: "python_bindings", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Failed to send channel_closed signal (from on_data_channel) to Python: {}", e);
                            } else {
                                debug!(target: "python_bindings", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Successfully sent channel_closed signal (from on_data_channel) to Python.");
                            }
                        } else {
                            warn!(target: "python_bindings", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "No signal_sender on peer_connection for channel_closed signal (from on_data_channel).");
                        }
                    } else {
                        warn!(target: "python_bindings", tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "Peer_connection was None, cannot send channel_closed signal (from on_data_channel).");
                    }

                    debug!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "on_data_channel: channel.run() task finished and cleaned up.");
                });

                info!(tube_id = %tube.id, channel_label = %rtc_data_channel_label, "on_data_channel: Successfully set up and spawned channel task.");
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
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

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

        for (_channel_name, channel_arc) in channels_guard.iter() {
            let channel_guard = channel_arc.lock().await;
            if let Some(ref token) = channel_guard.callback_token {
                tokens.push(token.clone());
            }
        }

        debug!(tube_id = %self.id, token_count = tokens.len(), "Collected callback tokens from {} active channels", channels_guard.len());
        tokens
    }

    // Get KSM config from active channels
    pub async fn get_ksm_config_from_channels(&self) -> Option<String> {
        let channels_guard = self.active_channels.read().await;

        // Get ksm_config from the first channel that has one
        for (_channel_name, channel_arc) in channels_guard.iter() {
            let channel_guard = channel_arc.lock().await;
            if let Some(ref config) = channel_guard.ksm_config {
                debug!(tube_id = %self.id, "Found KSM config from active channel");
                return Some(config.clone());
            }
        }

        debug!(tube_id = %self.id, "No KSM config found in any active channel");
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
        &self,
        label: &str,
        ksm_config: String,
        callback_token: String,
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
        &self,
        data_channel: &WebRTCDataChannel,
        label: String,
        ksm_config: String,
        callback_token: String,
    ) {
        // Store references directly where possible
        let dc_ref = &data_channel.data_channel;

        // Set up a state change handler-use string literals to avoid clones
        let label_for_open = label.clone();
        let ksm_config_for_open = ksm_config.clone();
        let callback_token_for_open = callback_token.clone();
        let self_clone_for_open = self.clone();

        dc_ref.on_open(Box::new(move || {
            let label_clone = label_for_open.clone();
            let ksm_config_clone = ksm_config_for_open.clone();
            let callback_token_clone = callback_token_for_open.clone();
            let self_clone = self_clone_for_open.clone();

            Box::pin(async move {
                info!("Data channel '{}' opened", label_clone);
                if let Err(e) = self_clone
                    .report_connection_open(ksm_config_clone, callback_token_clone)
                    .await
                {
                    error!("Failed to report connection open: {}", e);
                }
            })
        }));

        let self_clone_for_close = self.clone();

        dc_ref.on_close(Box::new(move || {
            let label_clone = label.clone();
            let ksm_config_clone = ksm_config.clone();
            let callback_token_clone = callback_token.clone();
            let self_clone = self_clone_for_close.clone();

            Box::pin(async move {
                info!("Data channel '{}' closed", label_clone);
                if let Err(e) = self_clone
                    .report_connection_close(ksm_config_clone, callback_token_clone)
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
    ) -> std::result::Result<(), String> {
        if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
            debug!(
                "TEST MODE: Skipping report_connection_open for ksm_config: {}",
                ksm_config
            );
            return Ok(());
        }
        debug!("Sending connection open callback to router");
        let token_value = serde_json::Value::String(callback_token);

        match post_connection_state(&ksm_config, "connection_open", &token_value, None).await {
            Ok(_) => {
                debug!("Connection open callback sent successfully");
                Ok(())
            }
            Err(e) => {
                error!("Error sending connection open callback: {}", e);
                Err(format!("Failed to send connection open callback: {}", e))
            }
        }
    }

    pub(crate) async fn report_connection_close(
        &self,
        ksm_config: String,
        callback_token: String,
    ) -> std::result::Result<(), String> {
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
            Some(true), // Assuming terminated=true as default for simplicity
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
    pub(crate) async fn create_channel(
        &self,
        name: &str,
        data_channel: &WebRTCDataChannel,
        timeout_seconds: Option<f64>,
        protocol_settings: HashMap<String, serde_json::Value>,
        callback_token: Option<String>,
        ksm_config: Option<String>,
    ) -> Result<Option<u16>> {
        info!(tube_id = %self.id, channel_name = %name, "create_channel: Called.");
        trace!(tube_id = %self.id, channel_name = %name, ?timeout_seconds, ?protocol_settings, "create_channel: Initial parameters.");

        let timeouts = timeout_seconds.map(|timeout| TunnelTimeouts {
            read: std::time::Duration::from_secs_f64(timeout),
            close_connection: std::time::Duration::from_secs_f64(timeout / 2.0),
            guacd_handshake: std::time::Duration::from_secs_f64(timeout / 1.5),
        });
        trace!(tube_id = %self.id, channel_name = %name, ?timeouts, "create_channel: Timeouts configured.");

        info!(tube_id = %self.id, channel_name = %name, "create_channel: About to call setup_channel_for_data_channel.");
        let setup_result = setup_channel_for_data_channel(
            data_channel,
            name.to_string(),
            timeouts,
            protocol_settings.clone(), // protocol_settings is already cloned if needed by the caller or passed as value
            self.is_server_mode_context,
            callback_token,
            ksm_config,
            Some(Arc::new(self.clone())), // Pass tube reference for registration
        )
        .await;

        let mut owned_channel = match setup_result {
            Ok(ch_instance) => {
                info!(tube_id = %self.id, channel_name = %name, "create_channel: setup_channel_for_data_channel successful.");
                ch_instance
            }
            Err(e) => {
                error!(tube_id = %self.id, channel_name = %name, "create_channel: setup_channel_for_data_channel failed: {}", e);
                return Err(e); // Propagate the error from setup_channel_for_data_channel
            }
        };
        trace!(tube_id = %self.id, channel_name = %name, ?owned_channel.active_protocol, ?owned_channel.local_listen_addr, server_mode = owned_channel.server_mode, "create_channel: Channel details after setup.");

        // Store the shutdown signal for this channel
        let shutdown_signal = Arc::clone(&owned_channel.should_exit);
        self.channel_shutdown_signals
            .write()
            .await
            .insert(name.to_string(), shutdown_signal);
        debug!(tube_id = %self.id, channel_name = %name, "create_channel: Shutdown signal stored for channel.");

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
                    info!(tube_id = %self.id, channel_name = %name, protocol = ?owned_channel.active_protocol, listen_addr = %listen_addr_str, "create_channel: Channel is server mode, attempting to start server.");
                    match owned_channel.start_server(&listen_addr_str).await {
                        Ok(socket_addr) => {
                            actual_listening_port = Some(socket_addr.port());
                            info!(tube_id = %self.id, channel_name = %name, listen_port = actual_listening_port.unwrap(), "create_channel: Server started successfully.");
                        }
                        Err(e) => {
                            error!(tube_id = %self.id, channel_name = %name, listen_addr = %listen_addr_str, "create_channel: Failed to start server: {}. Channel will not listen.", e);
                            self.channel_shutdown_signals.write().await.remove(name);
                            return Err(anyhow!(
                                "Failed to start server for channel {}: {}",
                                name,
                                e
                            ));
                        }
                    }
                } else {
                    debug!(tube_id = %self.id, channel_name = %name, protocol = ?owned_channel.active_protocol, listen_addr = ?owned_channel.local_listen_addr, "create_channel: Server mode channel, but no listen address or not a server-type protocol, skipping start_server.");
                }
            } else {
                debug!(tube_id = %self.id, channel_name = %name, "create_channel: Server mode channel, but local_listen_addr is None.");
            }
        } else {
            debug!(tube_id = %self.id, channel_name = %name, "create_channel: Channel is not server_mode.");
        }

        let name_clone = name.to_string();
        let runtime_clone = Arc::clone(&self.runtime);
        let tube_id_for_spawn = self.id.clone(); // Clone self.id here to make it 'static
        let peer_connection_for_spawn = Arc::clone(&self.peer_connection); // Clone peer_connection

        info!(tube_id = %self.id, channel_name = %name_clone, "create_channel: Spawning channel.run() task.");
        let tube_arc = Arc::new(self.clone()); // Single Arc wrapping for callbacks
        runtime_clone.spawn(async move {
            // Use the cloned tube_id_for_spawn which is 'static
            debug!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "create_channel: channel.run() task started.");

            // Send connection_open callback when a channel starts running
            if let Err(e) = tube_arc.send_connection_open_callback(&name_clone).await {
                warn!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Failed to send connection_open callback: {}", e);
            }

            let run_result = owned_channel.run().await;

            let outcome_details: String = match &run_result {
                Ok(()) => {
                    info!(target: "lifecycle", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Channel '{}' ran and exited normally. Signaling Python.", name_clone);
                    "normal_exit".to_string()
                }
                Err(crate::error::ChannelError::CriticalUpstreamClosed(closed_channel_id_from_err)) => {
                    warn!(target: "lifecycle", tube_id = %tube_id_for_spawn, channel_name = %name_clone, channel_id_in_err = %closed_channel_id_from_err, "Channel '{}' exited due to critical upstream closure. Signaling Python.", name_clone);
                    format!("critical_upstream_closed: {}", closed_channel_id_from_err)
                }
                Err(e) => {
                    error!(target: "lifecycle", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Channel '{}' encountered an error in run(): {}. Signaling Python.", name_clone, e);
                    format!("error: {}", e)
                }
            };

            // Send connection_close callback when channel finishes
            if let Err(e) = tube_arc.send_connection_close_callback(&name_clone).await {
                warn!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Failed to send connection_close callback: {}", e);
            }

            // Deregister the channel from the tube
            tube_arc.deregister_channel(&name_clone).await;

            // Remove shutdown signal for this channel
            tube_arc.remove_channel_shutdown_signal(&name_clone).await;

            // Always send a signal when channel.run() finishes, regardless of reason.
            let pc_guard = peer_connection_for_spawn.lock().await;
            if let Some(pc_instance_arc) = &*pc_guard {
                if let Some(sender) = &pc_instance_arc.signal_sender {
                    let signal_data = serde_json::json!({
                        "channel_id": name_clone, // This is the label of the channel that finished
                        "outcome": outcome_details
                    }).to_string();

                    let signal_msg = SignalMessage {
                        tube_id: tube_id_for_spawn.clone(),
                        kind: "channel_closed".to_string(), // Generic kind for any channel closure
                        data: signal_data,
                        conversation_id: name_clone.clone(), // Use the channel's label/name as conversation_id for this signal
                    };
                    if let Err(e) = sender.send(signal_msg) {
                        error!(target: "python_bindings", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Failed to send channel_closed signal to Python: {}", e);
                    } else {
                        debug!(target: "python_bindings", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Successfully sent channel_closed signal to Python.");
                    }
                } else {
                    warn!(target: "python_bindings", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "No signal_sender found on peer_connection to send channel_closed signal.");
                }
            } else {
                warn!(target: "python_bindings", tube_id = %tube_id_for_spawn, channel_name = %name_clone, "Peer_connection was None, cannot send channel_closed signal.");
            }

            debug!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "create_channel: channel.run() task finished and cleaned up.");
        });
        info!(tube_id = %self.id, channel_name = %name, actual_listening_port = ?actual_listening_port, "create_channel: Successfully set up and spawned channel task. Returning listening port.");
        Ok(actual_listening_port)
    }

    // Create the default control channel
    pub(crate) async fn create_control_channel(
        &self,
        ksm_config: String,
        callback_token: String,
    ) -> Result<WebRTCDataChannel> {
        let control_channel = self
            .create_data_channel("control", ksm_config, callback_token)
            .await?;
        *self.control_channel.write().await = Some(control_channel.clone());
        Ok(control_channel)
    }

    // Close a specific channel by signaling its run loop to exit
    pub(crate) async fn close_channel(&self, name: &str) -> Result<()> {
        let mut signals = self.channel_shutdown_signals.write().await;
        if let Some(signal_arc) = signals.remove(name) {
            // Remove from the map once signaled
            info!("Tube {}: Signaling channel '{}' to close.", self.id, name);
            signal_arc.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        } else {
            warn!("Tube {}: No shutdown signal found for channel '{}' during close_channel. It might have already been closed or never run.", self.id, name);
            Err(anyhow!(
                "No shutdown signal for channel not found: {}",
                name
            ))
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
                debug!(target: "webrtc_sdp", tube_id = %pc_arc.tube_id, "Trickle ICE: Setting local description in Tube::create_session_description");
                pc_arc.set_local_description(sdp.clone(), !is_offer).await?;
            } else {
                debug!(target: "webrtc_sdp", tube_id = %pc_arc.tube_id, "Non-trickle ICE: Local description already set and finalized. Skipping redundant set_local_description in Tube.");
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
        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc) = &*pc_guard {
            // Create SessionDescription based on type
            let desc = if is_answer {
                webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(
                    sdp,
                )
            } else {
                webrtc::peer_connection::sdp::session_description::RTCSessionDescription::offer(sdp)
            }
            .map_err(|e| format!("Failed to create session description: {}", e))?;

            // Set the remote description directly on the peer connection
            pc.peer_connection
                .set_remote_description(desc)
                .await
                .map_err(|e| format!("Failed to set remote description: {}", e))
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

    // Get connection state
    pub(crate) async fn connection_state(&self) -> String {
        let pc_guard = self.peer_connection.lock().await;

        if let Some(pc) = &*pc_guard {
            pc.connection_state()
        } else {
            "closed".to_string()
        }
    }

    // Close the entire tube
    pub(crate) async fn close(
        &self,
        registry: &mut crate::tube_registry::TubeRegistry,
    ) -> Result<()> {
        info!("Closing tube with ID: {}", self.id);

        // Set the status to Closed first to prevent Drop from trying to remove
        *self.status.write().await = TubeStatus::Closed;
        info!("Set tube status to Closed");

        // Send connection_close callbacks for all active channels
        let channel_names: Vec<String> = {
            let channels_guard = self.active_channels.read().await;
            channels_guard.keys().cloned().collect()
        };

        for channel_name in &channel_names {
            if let Err(e) = self.send_connection_close_callback(channel_name).await {
                warn!(tube_id = %self.id, channel_name = %channel_name, "Failed to send connection_close callback during tube closure: {}", e);
            }
        }

        // Send "channel_closed" signals to Python for all channels before closing them
        let signal_sender_opt = {
            let pc_guard = self.peer_connection.lock().await;
            if let Some(pc_instance_arc) = &*pc_guard {
                pc_instance_arc.signal_sender.clone()
            } else {
                None
            }
        };

        if let Some(signal_sender) = signal_sender_opt {
            let data_channels_snapshot = self.data_channels.read().await.clone();
            for (_channel_label, data_channel) in data_channels_snapshot.iter() {
                let channel_label_str = data_channel.data_channel.label(); // Use the label which contains the original conversation_id from Python

                // For the control channel, use the original conversation ID if available
                let conversation_id = if channel_label_str == "control" {
                    if let Some(ref original_id) = self.original_conversation_id {
                        original_id.clone()
                    } else {
                        debug!(target: "python_bindings", tube_id = %self.id, "Skipping channel_closed signal for control channel - no original_conversation_id stored");
                        continue;
                    }
                } else {
                    channel_label_str.to_string()
                };

                let signal_data = serde_json::json!({
                    "channel_id": conversation_id,
                    "outcome": "tube_closed"
                })
                .to_string();

                let signal_msg = SignalMessage {
                    tube_id: self.id.clone(),
                    kind: "channel_closed".to_string(),
                    data: signal_data,
                    conversation_id: conversation_id.clone(),
                };

                if let Err(e) = signal_sender.send(signal_msg) {
                    error!(target: "python_bindings", tube_id = %self.id, channel_label = %channel_label_str, conversation_id = %conversation_id, "Failed to send channel_closed signal for tube closure: {}", e);
                } else {
                    debug!(target: "python_bindings", tube_id = %self.id, channel_label = %channel_label_str, conversation_id = %conversation_id, "Sent channel_closed signal due to tube closure");
                }
            }
        } else {
            warn!(target: "python_bindings", tube_id = %self.id, "No signal sender available to notify Python of channel closures during tube close");
        }

        // Clear all channel shutdown signals
        self.channel_shutdown_signals.write().await.clear();

        // Clear all active channels
        self.active_channels.write().await.clear();

        // Close all data channels
        for (_, dc) in self.data_channels.write().await.drain() {
            let _ = dc.close().await;
        }

        // Close peer connection if exists
        let mut pc = self.peer_connection.lock().await;
        if let Some(pc_inner) = pc.take() {
            let _ = pc_inner.close().await;
        }

        // Remove from the global registry using the passed-in mutable reference
        info!("Removing tube {} from registry via Tube::close()", self.id);
        registry.remove_tube(&self.id);

        // Verify removal
        if registry.all_tube_ids_sync().contains(&self.id) {
            warn!("WARNING: Failed to remove tube from registry!");
            // Force removal in case it wasn't properly removed
            registry.tubes_by_id.remove(&self.id);
            registry
                .conversation_mappings
                .retain(|_, tid| tid != &self.id);
        } else {
            info!("Successfully removed tube from registry");
            info!("TUBE CLEANUP COMPLETE: {} - This tube is now fully closed and removed from registry", self.id);
        }

        // Add a delay to ensure registry updates propagate
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        Ok(())
    }

    // Get status
    pub async fn status(&self) -> TubeStatus {
        *self.status.read().await
    }

    // Register a channel with this tube
    pub async fn register_channel(&self, channel_name: String, channel: Channel) -> Result<()> {
        let channel_arc = Arc::new(TokioMutex::new(channel));
        let mut channels_guard = self.active_channels.write().await;
        channels_guard.insert(channel_name.clone(), channel_arc);
        info!(tube_id = %self.id, channel_name = %channel_name, "Registered channel with tube");
        Ok(())
    }

    // Deregister a channel from this tube
    pub async fn deregister_channel(&self, channel_name: &str) {
        let mut channels_guard = self.active_channels.write().await;
        if channels_guard.remove(channel_name).is_some() {
            info!(tube_id = %self.id, channel_name = %channel_name, "Deregistered channel from tube");
        } else {
            warn!(tube_id = %self.id, channel_name = %channel_name, "Attempted to deregister channel that wasn't registered");
        }
    }

    // Remove shutdown signal for a channel
    pub async fn remove_channel_shutdown_signal(&self, channel_name: &str) {
        let mut signals_guard = self.channel_shutdown_signals.write().await;
        if signals_guard.remove(channel_name).is_some() {
            debug!(tube_id = %self.id, channel_name = %channel_name, "Removed shutdown signal for channel");
        } else {
            debug!(tube_id = %self.id, channel_name = %channel_name, "No shutdown signal found to remove for channel");
        }
    }

    // Send connection_open callback for a specific channel
    pub async fn send_connection_open_callback(&self, channel_name: &str) -> Result<()> {
        let channels_guard = self.active_channels.read().await;
        if let Some(channel_arc) = channels_guard.get(channel_name) {
            let channel_guard = channel_arc.lock().await;

            if let (Some(ref ksm_config), Some(ref callback_token)) =
                (&channel_guard.ksm_config, &channel_guard.callback_token)
            {
                // Skip if in test mode
                if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
                    debug!(tube_id = %self.id, channel_name = %channel_name, "TEST MODE: Skipping connection_open callback");
                    return Ok(());
                }

                debug!(tube_id = %self.id, channel_name = %channel_name, "Sending connection_open callback to router");
                let token_value = serde_json::Value::String(callback_token.clone());

                match post_connection_state(ksm_config, "connection_open", &token_value, None).await
                {
                    Ok(_) => {
                        debug!(tube_id = %self.id, channel_name = %channel_name, "Connection open callback sent successfully");
                        Ok(())
                    }
                    Err(e) => {
                        error!(tube_id = %self.id, channel_name = %channel_name, "Error sending connection open callback: {}", e);
                        Err(anyhow!("Failed to send connection open callback: {}", e))
                    }
                }
            } else {
                warn!(tube_id = %self.id, channel_name = %channel_name, "Channel missing ksm_config or callback_token for connection_open callback");
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
        let channels_guard = self.active_channels.read().await;
        if let Some(channel_arc) = channels_guard.get(channel_name) {
            let channel_guard = channel_arc.lock().await;

            if let (Some(ref ksm_config), Some(ref callback_token)) =
                (&channel_guard.ksm_config, &channel_guard.callback_token)
            {
                // Skip if in test mode
                if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
                    debug!(tube_id = %self.id, channel_name = %channel_name, "TEST MODE: Skipping connection_close callback");
                    return Ok(());
                }

                debug!(tube_id = %self.id, channel_name = %channel_name, "Sending connection_close callback to router");
                let token_value = serde_json::Value::String(callback_token.clone());

                match post_connection_state(
                    ksm_config,
                    "connection_close",
                    &token_value,
                    Some(true),
                )
                .await
                {
                    Ok(_) => {
                        debug!(tube_id = %self.id, channel_name = %channel_name, "Connection close callback sent successfully");
                        Ok(())
                    }
                    Err(e) => {
                        error!(tube_id = %self.id, channel_name = %channel_name, "Error sending connection close callback: {}", e);
                        Err(anyhow!("Failed to send connection close callback: {}", e))
                    }
                }
            } else {
                warn!(tube_id = %self.id, channel_name = %channel_name, "Channel missing ksm_config or callback_token for connection_close callback");
                Ok(())
            }
        } else {
            warn!(tube_id = %self.id, channel_name = %channel_name, "Channel not found when trying to send connection_close callback");
            Ok(())
        }
    }
}

impl Drop for Tube {
    fn drop(&mut self) {
        debug!("Drop called for tube with ID: {}", self.id);
        // Note: This is called each time an Arc<Tube> reference is dropped.
        // The actual Tube is only cleaned up when the last Arc reference is dropped.
        // Multiple Drop messages are normal and expected due to Arc cloning in:
        // - PyTubeRegistry::create_tube (Phase 1 and Phase 2)
        // - on_data_channel callback
        // - Various async operations

        // Try to get the strong count if we can access it through one of our Arc fields,
        // This is just for debugging purposes
        if let Ok(pc_guard) = self.peer_connection.try_lock() {
            if let Some(ref _pc) = *pc_guard {
                // We can't directly get Arc strong_count in Drop, but we can note this is happening
                debug!("Tube {} Drop: peer_connection field is still Some", self.id);
            } else {
                debug!("Tube {} Drop: peer_connection field is None", self.id);
            }
        }
    }
}
