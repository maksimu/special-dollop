use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use anyhow::{anyhow, Result};
use webrtc::peer_connection::configuration::RTCConfiguration;
use tracing::{debug, error, info, warn, trace};
use crate::webrtc_core::{WebRTCPeerConnection, create_data_channel};
use crate::models::TunnelTimeouts;
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::{TubeStatus, setup_channel_for_data_channel};
use crate::webrtc_data_channel::WebRTCDataChannel;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc::UnboundedSender;
use crate::tube_registry::SignalMessage;
use std::sync::atomic::AtomicBool;
use crate::router_helpers::post_connection_state;

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
    // Indicates if this tube was created in a server or client context by its registry
    pub(crate) is_server_mode_context: bool,
    // Current status
    pub(crate) status: Arc<TokioRwLock<TubeStatus>>,
    // Runtime
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
}

impl Tube {
    // Create a new tube with the optional peer connection
    pub fn new(is_server_mode_context: bool) -> Result<Arc<Self>> {
        let id = Uuid::new_v4().to_string();
        let runtime = get_runtime();

        let tube = Arc::new(Self {
            id: id.clone(), // Clone ID to use below
            peer_connection: Arc::new(TokioMutex::new(None)),
            data_channels: Arc::new(TokioRwLock::new(HashMap::new())),
            control_channel: Arc::new(TokioRwLock::new(None)),
            channel_shutdown_signals: Arc::new(TokioRwLock::new(HashMap::new())),
            is_server_mode_context,
            status: Arc::new(TokioRwLock::new(TubeStatus::Initializing)),
            runtime,
        });

        Ok(tube)
    }

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
        info!("[TUBE_DEBUG] Tube {}: create_peer_connection called. trickle_ice: {}, turn_only: {}", self.id, trickle_ice, turn_only);
        trace!(tube_id = %self.id, ?protocol_settings, "Create_peer_connection protocol_settings");

        let connection = WebRTCPeerConnection::new(
            config, 
            trickle_ice, 
            turn_only, 
            ksm_config.clone(), 
            Some(signal_sender),
            self.id.clone(), // Pass tube_id
        ).await 
            .map_err(|e| anyhow!("{}", e))?;

        let connection_arc = Arc::new(connection);
        
        let status = self.status.clone();
        
        info!("[TUBE_DEBUG] Tube {}: About to call setup_ice_candidate_handler. Callback token (used as conv_id before): {}", self.id, callback_token);
        connection_arc.setup_ice_candidate_handler();
        
        connection_arc.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let status_clone = status.clone();
            Box::pin(async move {
                match state {
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                        *status_clone.write().await = TubeStatus::Active;
                        info!("Tube connection state changed to Active");
                    },
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                        *status_clone.write().await = TubeStatus::Failed;
                        info!("Tube connection state changed to Failed");
                    },
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                        *status_clone.write().await = TubeStatus::Closed;
                        info!("Tube connection state changed to Closed");
                    },
                    _ => {
                        debug!("Connection state changed to: {:?}", state);
                    }
                }
            })
        }));
        
        // Set up a handler for incoming data channels
        let tube_clone = self.clone();
        let protocol_settings_clone_for_on_data_channel = protocol_settings.clone(); // Clone for the outer closure
        connection_arc.peer_connection.on_data_channel(Box::new(move |rtc_data_channel| {
            let tube = tube_clone.clone();
            // Use the protocol_settings cloned for the on_data_channel closure
            let protocol_settings_for_channel_setup = protocol_settings_clone_for_on_data_channel.clone();
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
                    current_server_mode
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

                info!(tube_id = %tube.id, channel_label = %label_clone_for_run, "on_data_channel: Spawning channel.run() task.");
                runtime_for_run.spawn(async move {
                    debug!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "on_data_channel: channel.run() task started.");
                    if let Err(e) = owned_channel.run().await {
                        error!("Tube {}: Endpoint (from on_data_channel) {}: Error running channel: {}", tube_id_for_log, label_clone_for_run, e);
                    }
                    debug!(tube_id = %tube_id_for_log, channel_label = %label_clone_for_run, "on_data_channel: channel.run() task finished.");
                    // Optionally, after the run finishes, remove its shutdown signal from the map.
                    // Requires cloning tube.channel_shutdown_signals Arc into the task.
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
    pub(crate) async fn create_data_channel(&self, label: &str, ksm_config: String, callback_token: String) -> Result<WebRTCDataChannel> {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            let rtc_data_channel = create_data_channel(&pc.peer_connection, label).await?;
            let data_channel = WebRTCDataChannel::new(rtc_data_channel);
            
            // Set up a message handler with zero-copy using the buffer pool
            self.setup_data_channel_handlers(&data_channel, label.to_string(), ksm_config, callback_token);
            
            // Clone for release outside the lock
            let data_channel_clone = data_channel.clone();
            
            // Release lock before adding to avoid potential deadlock
            drop(pc_guard);
            
            // Add to our mapping
            self.add_data_channel(data_channel.clone()).await?;
            
            Ok(data_channel_clone)
        } else {
            Err(anyhow!("No peer connection available").into())
        }
    }
    
    // Setup event handlers for a data channel
    fn setup_data_channel_handlers(&self, data_channel: &WebRTCDataChannel, label: String, ksm_config: String, callback_token: String) {
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
                if let Err(e) = self_clone.report_connection_open(ksm_config_clone, callback_token_clone).await {
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
                if let Err(e) = self_clone.report_connection_close(ksm_config_clone, callback_token_clone).await {
                    error!("Failed to report connection close: {}", e);
                }
            })
        }));
    }
    
    // Report connection open state to router
    pub(crate) async fn report_connection_open(&self, ksm_config: String, callback_token: String) -> std::result::Result<(), String> {
        if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
            debug!("TEST MODE: Skipping report_connection_open for ksm_config: {}", ksm_config);
            return Ok(());
        }
        debug!("Sending connection open callback to router");
        let token_value = serde_json::Value::String(callback_token);

        match post_connection_state(
            &*ksm_config,
            "connection_open",
            &token_value,
            None
        ).await {
            Ok(_) => {
                debug!("Connection open callback sent successfully");
                Ok(())
            },
            Err(e) => {
                error!("Error sending connection open callback: {}", e);
                Err(format!("Failed to send connection open callback: {}", e))
            }
        }
    }

    pub(crate) async fn report_connection_close(&self, ksm_config: String, callback_token: String) -> std::result::Result<(), String> {
        if ksm_config.starts_with("TEST_MODE_KSM_CONFIG_") {
            debug!("TEST MODE: Skipping report_connection_close for ksm_config: {}", ksm_config);
            return Ok(());
        }
        // Report connection close to router if configuration exists
        debug!("Sending connection close callback to router");
        let token_value = serde_json::Value::String(callback_token);

        // Fall back to direct API call
        match post_connection_state(
            &*ksm_config,
            "connection_close",
            &token_value,
            Some(true) // Assuming terminated=true as default for simplicity
        ).await {
            Ok(_) => {
                debug!("Connection close callback sent successfully");
                Ok(())
            },
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
    ) -> Result<Option<u16>> {
        info!(tube_id = %self.id, channel_name = %name, "create_channel: Called.");
        trace!(tube_id = %self.id, channel_name = %name, ?timeout_seconds, ?protocol_settings, "create_channel: Initial parameters.");

        let timeouts = if let Some(timeout) = timeout_seconds {
            Some(TunnelTimeouts {
                read: std::time::Duration::from_secs_f64(timeout),
                ping_timeout: std::time::Duration::from_secs_f64(timeout / 3.0),
                open_connection: std::time::Duration::from_secs_f64(timeout),
                close_connection: std::time::Duration::from_secs_f64(timeout / 2.0),
                guacd_handshake: std::time::Duration::from_secs_f64(timeout / 1.5),
            })
        } else {
            None
        };
        trace!(tube_id = %self.id, channel_name = %name, ?timeouts, "create_channel: Timeouts configured.");

        info!(tube_id = %self.id, channel_name = %name, "create_channel: About to call setup_channel_for_data_channel.");
        let setup_result = setup_channel_for_data_channel(
            data_channel, 
            name.to_string(), 
            timeouts,
            protocol_settings.clone(), // protocol_settings is already cloned if needed by the caller or passed as value
            self.is_server_mode_context
        ).await;

        let mut owned_channel = match setup_result {
            Ok(ch_instance) => {
                info!(tube_id = %self.id, channel_name = %name, "create_channel: setup_channel_for_data_channel successful.");
                ch_instance
            }
            Err(e) => {
                error!(tube_id = %self.id, channel_name = %name, "create_channel: setup_channel_for_data_channel failed: {}", e);
                return Err(e.into()); // Propagate the error from setup_channel_for_data_channel
            }
        };
        trace!(tube_id = %self.id, channel_name = %name, ?owned_channel.active_protocol, ?owned_channel.local_listen_addr, server_mode = owned_channel.server_mode, "create_channel: Channel details after setup.");


        // Store the shutdown signal for this channel
        let shutdown_signal = Arc::clone(&owned_channel.should_exit);
        self.channel_shutdown_signals.write().await.insert(name.to_string(), shutdown_signal);
        debug!(tube_id = %self.id, channel_name = %name, "create_channel: Shutdown signal stored for channel.");

        let mut actual_listening_port: Option<u16> = None;

        if owned_channel.server_mode {
            if let Some(listen_addr_str) = owned_channel.local_listen_addr.clone() {
                if !listen_addr_str.is_empty() && 
                   matches!(owned_channel.active_protocol, crate::channel::types::ActiveProtocol::PortForward | crate::channel::types::ActiveProtocol::Socks5 | crate::channel::types::ActiveProtocol::Guacd) // Assuming Guacamole might be server mode too
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
                            return Err(anyhow!("Failed to start server for channel {}: {}", name, e));
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

        info!(tube_id = %self.id, channel_name = %name_clone, "create_channel: Spawning channel.run() task.");
        runtime_clone.spawn(async move {
            // Use the cloned tube_id_for_spawn which is 'static
            debug!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "create_channel: channel.run() task started.");
            if let Err(e) = owned_channel.run().await {
                // Update the error log to also use the 'static tube_id_for_spawn
                error!("Channel '{}' (tube_id: {}) encountered an error in run(): {}", name_clone, tube_id_for_spawn, e);
            }
            // Use the cloned tube_id_for_spawn
            debug!(tube_id = %tube_id_for_spawn, channel_name = %name_clone, "create_channel: channel.run() task finished.");
            // TODO: after run finishes (normally or due to error), remove its shutdown signal from the map
            //  This requires Tube to be passed or its shutdown_signals map Arc to be cloned into the task.
            //  For now, manual removal via close_channel is the main path.
        });
        info!(tube_id = %self.id, channel_name = %name, actual_listening_port = ?actual_listening_port, "create_channel: Successfully set up and spawned channel task. Returning listening port.");
        Ok(actual_listening_port)
    }
    
    // Create the default control channel
    pub(crate) async fn create_control_channel(&self, ksm_config: String, callback_token: String) -> Result<WebRTCDataChannel> {
        let control_channel = self.create_data_channel("control", ksm_config, callback_token).await?;
        *self.control_channel.write().await = Some(control_channel.clone());
        Ok(control_channel)
    }
    
    // Close a specific channel by signaling its run loop to exit
    pub(crate) async fn close_channel(&self, name: &str) -> Result<()> {
        let mut signals = self.channel_shutdown_signals.write().await;
        if let Some(signal_arc) = signals.remove(name) { // Remove from the map once signaled
            info!("Tube {}: Signaling channel '{}' to close.", self.id, name);
            signal_arc.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        } else {
            warn!("Tube {}: No shutdown signal found for channel '{}' during close_channel. It might have already been closed or never run.", self.id, name);
            Err(anyhow!("No shutdown signal for channel not found: {}", name))
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
            if pc_arc.trickle_ice { // trickle_ice was made pub(crate) by the user
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
    pub(crate) async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            // Create SessionDescription based on type
            let desc = if is_answer {
                webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp)
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
    pub(crate) async fn close(&self, registry: &mut crate::tube_registry::TubeRegistry) -> Result<()> {
        info!("Closing tube with ID: {}", self.id);

        // Set the status to Closed first to prevent Drop from trying to remove
        *self.status.write().await = TubeStatus::Closed;
        info!("Set tube status to Closed");

        // Clear all channel shutdown signals
        self.channel_shutdown_signals.write().await.clear();

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
            registry.conversation_mappings.retain(|_, tid| tid != &self.id);
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
        self.status.read().await.clone()
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
