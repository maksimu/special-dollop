use crate::resource_manager::RESOURCE_MANAGER;
use crate::router_helpers::get_relay_access_creds;
use crate::tube_protocol::CloseConnectionReason;
use crate::Tube;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use log::{debug, error, info, trace, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
#[cfg(test)]
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::RwLock;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

// Define a message structure for signaling
#[derive(Debug, Clone)]
pub struct SignalMessage {
    pub tube_id: String,
    pub kind: String, // "icecandidate", "answer", etc.
    pub data: String,
    pub conversation_id: String,
    pub progress_flag: Option<i32>, // Progress flag for gateway responses (0=COMPLETE, 1=FAIL, 2=PROGRESS, 3=SKIP, 4=ABSENT)
    pub progress_status: Option<String>, // Progress status message
    pub is_ok: Option<bool>,        // Success/failure indicator
}

// Global registry for all tubes - using Lazy with explicit thread safety
pub(crate) static REGISTRY: Lazy<RwLock<TubeRegistry>> = Lazy::new(|| {
    debug!("Initializing global tube registry");
    RwLock::new(TubeRegistry::new())
});

// Unified registry for managing tubes with different lookup methods
pub(crate) struct TubeRegistry {
    // Primary storage of tubes by their ID
    pub(crate) tubes_by_id: HashMap<String, Arc<Tube>>,
    // Mapping of conversation IDs to tube IDs for lookup
    pub(crate) conversation_mappings: HashMap<String, String>,
    // Track whether we're in server mode (creates a server) or client mode (connects to servers)
    pub(crate) server_mode: bool,
    // Mapping of tube IDs to signaling channels
    pub(crate) signal_channels: HashMap<String, UnboundedSender<SignalMessage>>,
}

impl TubeRegistry {
    pub(crate) fn new() -> Self {
        debug!("TubeRegistry::new() called");
        Self {
            tubes_by_id: HashMap::new(),
            conversation_mappings: HashMap::new(),
            server_mode: false, // Default to client mode
            signal_channels: HashMap::new(),
        }
    }

    // Register a signal channel for a tube
    #[cfg(test)]
    pub(crate) fn register_signal_channel(
        &mut self,
        tube_id: &str,
    ) -> UnboundedReceiver<SignalMessage> {
        let (sender, receiver) = unbounded_channel::<SignalMessage>();
        self.signal_channels.insert(tube_id.to_string(), sender);
        receiver
    }

    // Remove a signal channel
    pub(crate) fn remove_signal_channel(&mut self, tube_id: &str) {
        self.signal_channels.remove(tube_id);
    }

    // Get a signal channel sender
    #[cfg(test)]
    pub(crate) fn get_signal_channel(
        &self,
        tube_id: &str,
    ) -> Option<UnboundedSender<SignalMessage>> {
        self.signal_channels.get(tube_id).cloned()
    }

    // Send a message to the signal channel for a tube
    #[cfg(test)]
    pub(crate) fn send_signal(&self, message: SignalMessage) -> Result<()> {
        if let Some(sender) = self.signal_channels.get(&message.tube_id) {
            sender
                .send(message)
                .map_err(|e| anyhow!("Failed to send signal, message was: {:?}", e.0))?;
            Ok(())
        } else {
            Err(anyhow!(
                "No signal channel found for tube: {}",
                message.tube_id
            ))
        }
    }

    // Add a tube to the registry
    pub(crate) fn add_tube(&mut self, tube: Arc<Tube>) {
        let id = tube.id();
        debug!("TubeRegistry::add_tube - Adding tube (tube_id: {})", id);
        self.tubes_by_id.insert(id.clone(), tube);
    }

    // Set server mode
    pub(crate) fn set_server_mode(&mut self, server_mode: bool) {
        self.server_mode = server_mode;
    }

    // Get server mode
    pub(crate) fn is_server_mode(&self) -> bool {
        self.server_mode
    }

    // Remove a tube from the registry
    pub(crate) fn remove_tube(&mut self, tube_id: &str) {
        self.tubes_by_id.remove(tube_id);

        // Remove the signal channel
        self.remove_signal_channel(tube_id);

        // Also remove any conversation mappings pointing to this tube
        self.conversation_mappings.retain(|_, tid| tid != tube_id);
    }

    pub(crate) fn get_by_tube_id(&self, tube_id: &str) -> Option<Arc<Tube>> {
        debug!(
            "TubeRegistry::get_by_tube_id - Looking for tube: {}",
            tube_id
        );
        match self.tubes_by_id.get(tube_id) {
            Some(tube) => {
                debug!("Found tube with ID: {}", tube_id);
                Some(tube.clone())
            }
            None => {
                debug!("Tube with ID {} not found in registry", tube_id);
                None
            }
        }
    }

    // Get a tube by a conversation ID
    pub(crate) fn get_by_conversation_id(&self, conversation_id: &str) -> Option<Arc<Tube>> {
        if let Some(tube_id) = self.conversation_mappings.get(conversation_id) {
            self.tubes_by_id.get(tube_id).cloned()
        } else {
            None
        }
    }

    // Associate a conversation ID with a tube
    pub(crate) fn associate_conversation(
        &mut self,
        tube_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        // Validate tube exists with single lookup instead of contains_key + potential second lookup
        self.tubes_by_id
            .get(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        self.conversation_mappings
            .insert(conversation_id.to_string(), tube_id.to_string());
        Ok(())
    }

    // Get all tube IDs
    pub(crate) fn all_tube_ids_sync(&self) -> Vec<String> {
        self.tubes_by_id.keys().cloned().collect()
    }

    // Find tubes by partial match of tube ID or conversation ID
    pub(crate) fn find_tubes(&self, search_term: &str) -> Vec<String> {
        let mut results = Vec::new();

        // Search in tube IDs
        for id in self.tubes_by_id.keys() {
            if id.contains(search_term) {
                results.push(id.clone());
            }
        }

        // Search in conversation IDs
        for (conv_id, tube_id) in &self.conversation_mappings {
            if conv_id.contains(search_term) {
                if let Some(tube) = self.tubes_by_id.get(tube_id) {
                    // Only add if not already in results
                    if !results.iter().any(|t| t == &tube.id()) {
                        results.push(tube_id.clone());
                    }
                }
            }
        }

        results
    }

    /// get all conversations from a tube id
    #[allow(dead_code)]
    pub(crate) fn tube_id_from_conversation_id(&self, conversation_id: &str) -> Option<&String> {
        self.conversation_mappings.get(conversation_id)
    }

    /// get all conversation ids by tube id
    #[allow(dead_code)]
    pub(crate) fn conversation_ids_by_tube_id(&self, tube_id: &str) -> Vec<&String> {
        let mut results = Vec::new();
        // Search in conversation IDs
        for (conv_id, con_tube_id) in &self.conversation_mappings {
            if tube_id == con_tube_id {
                // Only add if not already in results
                if !results.contains(&conv_id) {
                    results.push(conv_id);
                }
            }
        }
        results
    }

    /// Create a tube with WebRTC connection and ICE configuration
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_tube(
        &mut self,
        conversation_id: &str,
        settings: HashMap<String, serde_json::Value>,
        initial_offer_sdp: Option<String>,
        trickle_ice: bool,
        callback_token: &str,
        krelay_server: &str,
        ksm_config: Option<&str>,
        client_version: &str,
        signal_sender: UnboundedSender<SignalMessage>,
        tube_id: Option<String>,
    ) -> Result<HashMap<String, String>> {
        // Check if tube_id is provided and already exists
        if let Some(ref provided_tube_id) = tube_id {
            if let Some(existing_tube) = self.get_by_tube_id(provided_tube_id) {
                // Tube already exists, use it
                info!(
                    "Using existing tube for conversation (tube_id: {}, conversation_id: {})",
                    provided_tube_id, conversation_id
                );

                // Associate this conversation_id with the existing tube
                self.associate_conversation(provided_tube_id, conversation_id)?;

                // Register the signal channel for this tube
                self.signal_channels
                    .insert(provided_tube_id.clone(), signal_sender);

                // Create a new data channel for this conversation on the existing tube
                let ksm_config_for_channel = ksm_config.unwrap_or("").to_string();
                match existing_tube
                    .create_data_channel(
                        conversation_id,
                        ksm_config_for_channel.clone(),
                        callback_token.to_string(),
                        client_version,
                    )
                    .await
                {
                    Ok(data_channel) => {
                        // Create the logical channel handler
                        match existing_tube
                            .create_channel(
                                conversation_id,
                                &data_channel,
                                None,
                                settings.clone(),
                                Some(callback_token.to_string()),
                                Some(ksm_config_for_channel),
                                Some(client_version.to_string()),
                            )
                            .await
                        {
                            Ok(_) => {
                                info!("Successfully created new channel on existing tube (tube_id: {}, conversation_id: {})", provided_tube_id, conversation_id);
                            }
                            Err(e) => {
                                warn!("Failed to create logical channel on existing tube: {} (tube_id: {}, conversation_id: {})", e, provided_tube_id, conversation_id);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create data channel on existing tube: {} (tube_id: {}, conversation_id: {})", e, provided_tube_id, conversation_id);
                    }
                }

                // Return basic information about the existing tube
                let mut result_map = HashMap::new();
                result_map.insert("tube_id".to_string(), provided_tube_id.clone());
                // Note: We don't generate new offer/answer for existing tubes
                // The caller should handle WebRTC signaling separately if needed

                return Ok(result_map);
            }
        }

        let initial_offer_sdp_decoded = if let Some(ref b64_offer) = initial_offer_sdp {
            let bytes = BASE64_STANDARD
                .decode(b64_offer)
                .context("Failed to decode initial_offer_sdp from base64")?;
            Some(
                String::from_utf8(bytes)
                    .context("Failed to convert decoded initial_offer_sdp to String")?,
            )
        } else {
            None
        };

        let is_server_mode = initial_offer_sdp_decoded.is_none();

        let tube_arc = Tube::new(is_server_mode, Some(conversation_id.to_string()), tube_id)?;
        let tube_id = tube_arc.id();

        self.add_tube(Arc::clone(&tube_arc));
        self.associate_conversation(&tube_id, conversation_id)?;
        self.set_server_mode(is_server_mode);
        self.signal_channels
            .insert(tube_id.clone(), signal_sender.clone());

        // Log the received parameters differently based on mode
        if is_server_mode {
            trace!("Server mode: Received krelay_server for ICE server setup (tube_id: {}, krelay_server: {})", tube_id, krelay_server);
        } else if let Some(ksm_cfg) = ksm_config {
            trace!("Client mode: Received krelay_server and ksm_config for ICE server setup (tube_id: {}, krelay_server: {}, ksm_config: {})", tube_id, krelay_server, ksm_cfg);
        } else {
            trace!("Client mode: Received krelay_server for ICE server setup, no ksm_config provided (tube_id: {}, krelay_server: {})", tube_id, krelay_server);
        }

        let mut ice_servers = Vec::new();
        let mut turn_only_for_config = settings
            .get("turn_only")
            .is_some_and(|v| v.as_bool().unwrap_or(false));
        debug!(
            "Initial 'turn_only' setting from input (tube_id: {}, turn_only_setting: {})",
            tube_id, turn_only_for_config
        );

        // Check for test mode - ksm_config might be None in server mode, so check carefully
        let is_test_mode = ksm_config.is_some_and(|cfg| cfg.starts_with("TEST_MODE_KSM_CONFIG"));

        if is_test_mode {
            info!("TEST_MODE_KSM_CONFIG active: Using Google STUN server and disabling TURN for this test configuration. (tube_id: {})", tube_id);
            turn_only_for_config = false;
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            info!("Added Google STUN server (tube_id: {}, stun_url: stun:stun.l.google.com:19302?transport=udp&family=ipv4)", tube_id);
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun1.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            info!("Added Google STUN server (tube_id: {}, stun_url: stun:stun1.l.google.com:19302?transport=udp&family=ipv4)", tube_id);
        } else if !krelay_server.is_empty() {
            debug!("Using provided krelay_server for ICE configuration (tube_id: {}, relay_server_host: {})", tube_id, krelay_server);

            if !turn_only_for_config {
                let stun_url_udp = format!("stun:{krelay_server}:3478");
                ice_servers.push(RTCIceServer {
                    urls: vec![stun_url_udp.clone()],
                    username: String::new(),
                    credential: String::new(),
                });
                debug!(
                    "Added STUN server (UDP) (tube_id: {}, stun_url: {})",
                    tube_id, stun_url_udp
                );
            }

            let use_turn_for_config_from_settings = settings
                .get("use_turn")
                .is_none_or(|v| v.as_bool().unwrap_or(true));
            debug!(
                "'use_turn' setting (tube_id: {}, use_turn_setting: {})",
                tube_id, use_turn_for_config_from_settings
            );

            if use_turn_for_config_from_settings {
                // Priority 1: Check for explicit TURN credentials in settings FIRST
                if let (Some(turn_url), Some(turn_username), Some(turn_password)) = (
                    settings.get("turn_url").and_then(|v| v.as_str()),
                    settings.get("turn_username").and_then(|v| v.as_str()),
                    settings.get("turn_password").and_then(|v| v.as_str()),
                ) {
                    debug!(
                        "Using explicit TURN credentials from settings (tube_id: {}, turn_url: {})",
                        tube_id, turn_url
                    );
                    ice_servers.push(RTCIceServer {
                        urls: vec![turn_url.to_string()],
                        username: turn_username.to_string(),
                        credential: turn_password.to_string(),
                    });
                }
                // Priority 2: Fallback to ksm_config if no explicit credentials
                else if let Some(ksm_cfg) = ksm_config {
                    if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                        // First, check if we can reuse an existing TURN connection
                        if let Some(existing_conn) =
                            RESOURCE_MANAGER.get_turn_connection(krelay_server)
                        {
                            debug!("Reusing existing TURN connection from pool (tube_id: {}, relay_url: {}, username: {})", tube_id, krelay_server, existing_conn.username);
                            ice_servers.push(RTCIceServer {
                                urls: vec![format!("turn:{}", krelay_server)],
                                username: existing_conn.username,
                                credential: existing_conn.password, // Use pooled credential
                            });
                        } else {
                            // Create new TURN connection
                            debug!(
                                "Fetching new TURN credentials from router (tube_id: {})",
                                tube_id
                            );

                            let turn_start_time = std::time::Instant::now();
                            // Request 1-hour TTL for TURN credentials (will be refreshed every 30min)
                            match get_relay_access_creds(ksm_cfg, Some(3600), client_version).await
                            {
                                Ok(creds) => {
                                    let turn_duration_ms =
                                        turn_start_time.elapsed().as_millis() as f64;
                                    debug!(
                                        "Successfully fetched TURN credentials (tube_id: {}, duration: {:.1}ms)",
                                        tube_id, turn_duration_ms
                                    );
                                    trace!(
                                        "Received TURN credentials (tube_id: {}, credentials: {})",
                                        tube_id,
                                        creds
                                    );

                                    // Extract and log TTL to understand router behavior
                                    let ttl_seconds =
                                        creds.get("ttl").and_then(|v| v.as_u64()).unwrap_or(0);

                                    if ttl_seconds > 0 {
                                        info!(
                                            "TURN credentials TTL: {}s ({:.1}h) (tube_id: {}, requested: 1h, will refresh every 30min)",
                                            ttl_seconds,
                                            ttl_seconds as f64 / 3600.0,
                                            tube_id
                                        );
                                    } else {
                                        warn!(
                                            "No TTL in TURN credentials response - assuming 1h default, will refresh every 30min (tube_id: {})",
                                            tube_id
                                        );
                                    }

                                    // Record TURN allocation success metrics
                                    crate::metrics::METRICS_COLLECTOR.record_turn_allocation(
                                        &tube_id,
                                        turn_duration_ms,
                                        true,
                                    );

                                    // Extract username and password from credentials
                                    if let (Some(username), Some(password)) = (
                                        creds.get("username").and_then(|v| v.as_str()),
                                        creds.get("password").and_then(|v| v.as_str()),
                                    ) {
                                        // Add to connection pool
                                        let _ref_count = RESOURCE_MANAGER.add_turn_connection(
                                            krelay_server.to_string(),
                                            username.to_string(),
                                            password.to_string(),
                                        );

                                        debug!("Created new TURN connection and added to pool (tube_id: {}, relay_url: {}, username: {})", tube_id, krelay_server, username);
                                        ice_servers.push(RTCIceServer {
                                            urls: vec![format!("turn:{}", krelay_server)],
                                            username: username.to_string(),
                                            credential: password.to_string(),
                                        });
                                    } else {
                                        warn!(
                                            "Invalid TURN credentials format (tube_id: {})",
                                            tube_id
                                        );
                                    }
                                }
                                Err(e) => {
                                    let turn_duration_ms =
                                        turn_start_time.elapsed().as_millis() as f64;
                                    error!(
                                        "Failed to get TURN credentials: {} (tube_id: {}, duration: {:.1}ms)",
                                        e, tube_id, turn_duration_ms
                                    );

                                    // Record TURN allocation failure metrics
                                    crate::metrics::METRICS_COLLECTOR.record_turn_allocation(
                                        &tube_id,
                                        turn_duration_ms,
                                        false,
                                    );
                                    // Don't fail the entire operation, just log the error
                                }
                            }
                        }
                    }
                } else {
                    debug!(
                        "No TURN credentials available in settings or ksm_config (tube_id: {})",
                        tube_id
                    );
                }
            }
        } else {
            warn!("No krelay_server provided. STUN/TURN servers will not be configured. (tube_id: {})", tube_id);
        }

        let all_configured_urls: Vec<String> =
            ice_servers.iter().flat_map(|s| s.urls.clone()).collect();
        debug!(
            "Final list of ICE server URLs to be used (tube_id: {}, configured_ice_urls: {:?})",
            tube_id, all_configured_urls
        );

        let rtc_config_obj = {
            let mut rtc_config = RTCConfiguration {
                ice_servers,
                ..Default::default()
            };
            if turn_only_for_config {
                rtc_config.ice_transport_policy = RTCIceTransportPolicy::Relay;
            } else {
                rtc_config.ice_transport_policy = RTCIceTransportPolicy::All;
            }

            // Apply resource management RTCConfiguration tuning
            let tuned_config = RESOURCE_MANAGER.apply_rtc_config_tuning(rtc_config, &tube_id);
            Some(tuned_config)
        };

        // For server mode, use empty string for ksm_config when creating peer connection since it's not needed
        let ksm_config_for_peer_connection = ksm_config.unwrap_or("");

        tube_arc
            .create_peer_connection(
                rtc_config_obj,
                trickle_ice,
                turn_only_for_config,
                ksm_config_for_peer_connection.to_string(),
                callback_token.to_string(),
                krelay_server.to_string(), // For TURN credential refresh
                client_version,
                settings.clone(),
                signal_sender.clone(),
            )
            .await
            .context("Failed to create peer connection")?;

        let mut listening_port_option: Option<u16> = None; // Initialize outside if

        // Conditionally create channels only if in server mode (no initial offer from the client)
        if is_server_mode {
            debug!(
                "Server mode: Proactively creating control and main data channels. (tube_id: {})",
                tube_id
            );
            if let Err(e) = tube_arc
                .create_control_channel(
                    ksm_config_for_peer_connection.to_string(),
                    callback_token.to_string(),
                    client_version,
                )
                .await
            {
                warn!(
                    "Failed to create control channel for tube {}: {}",
                    tube_id, e
                );
                // Decide if this is a fatal error for server mode. For now, just a warning.
            }

            // Create the main data channel, using conversation_id as its label.
            // The settings for this channel are also passed to tube_arc.create_channel
            match tube_arc
                .create_data_channel(
                    conversation_id,
                    ksm_config_for_peer_connection.to_string(),
                    callback_token.to_string(),
                    client_version,
                )
                .await
            {
                Ok(data_channel_arc) => {
                    // Assign to listening_port_option here
                    match tube_arc
                        .create_channel(
                            conversation_id,
                            &data_channel_arc,
                            None,
                            settings.clone(),
                            Some(callback_token.to_string()),
                            Some(ksm_config_for_peer_connection.to_string()),
                            Some(client_version.to_string()),
                        )
                        .await
                    {
                        Ok(port_opt) => listening_port_option = port_opt,
                        Err(e) => {
                            warn!("Server mode: Failed to create logical channel for main data channel: {} (tube_id: {}, channel_id: {})", e, tube_id, conversation_id);
                        }
                    }
                }
                Err(e) => {
                    warn!("Server mode: Failed to create main data channel: {} (tube_id: {}, channel_id: {})", e, tube_id, conversation_id);
                }
            }
        } else {
            debug!("Client mode: Expecting client to create data channels via its offer. (tube_id: {})", tube_id);
        }

        let mut result_map = HashMap::new();
        result_map.insert("tube_id".to_string(), tube_id.clone());
        if is_server_mode {
            if let Some(port) = listening_port_option {
                result_map.insert(
                    "actual_local_listen_addr".to_string(),
                    format!("127.0.0.1:{port}"),
                );
                debug!("Server mode: Reporting listening address. (tube_id: {}, listen_addr: 127.0.0.1:{})", tube_id, port);
            } else {
                warn!("Server mode: No listening port obtained for main data channel, not adding actual_local_listen_addr to result. (tube_id: {})", tube_id);
            }
        }

        if is_server_mode {
            let offer_sdp = tube_arc
                .create_offer()
                .await
                .map_err(|e| anyhow!("Failed to create offer: {}", e))?;
            result_map.insert("offer".to_string(), BASE64_STANDARD.encode(offer_sdp));
        } else {
            let offer_sdp_str = initial_offer_sdp_decoded.ok_or_else(|| anyhow!("Initial offer SDP is required for client mode (after potential base64 decoding)"))?;
            tube_arc
                .set_remote_description(offer_sdp_str, false)
                .await
                .map_err(|e| anyhow!("Client: Failed to set remote description (offer): {}", e))?;
            let answer_sdp = tube_arc
                .create_answer()
                .await
                .map_err(|e| anyhow!("Client: Failed to create answer: {}", e))?;
            result_map.insert("answer".to_string(), BASE64_STANDARD.encode(answer_sdp));
        }

        debug!("Tube processing complete. (tube_id: {}, conversation_id: {}, mode: {}, result_keys: {:?}, settings_keys: {:?})",
              tube_id, conversation_id, if is_server_mode {"Server"} else {"Client"}, result_map.keys().collect::<Vec<_>>(), settings.keys().collect::<Vec<_>>());

        Ok(result_map)
    }

    /// Set remote description and create answer if needed
    pub(crate) async fn set_remote_description(
        &self,
        tube_id: &str,
        sdp: &str,
        is_answer: bool,
    ) -> Result<Option<String>> {
        let tube = self
            .get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        let sdp_bytes = BASE64_STANDARD.decode(sdp).context(format!(
            "Failed to decode SDP from base64 for tube_id: {tube_id}"
        ))?;
        let sdp_decoded = String::from_utf8(sdp_bytes).context(format!(
            "Failed to convert decoded SDP to String for tube_id: {tube_id}"
        ))?;

        // Set the remote description
        tube.set_remote_description(sdp_decoded, is_answer)
            .await
            .map_err(|e| anyhow!("Failed to set remote description: {}", e))?;

        // If this is an offer, create an answer
        if !is_answer {
            let answer = tube
                .create_answer()
                .await
                .map_err(|e| anyhow!("Failed to create answer: {}", e))?;

            return Ok(Some(BASE64_STANDARD.encode(answer))); // Encode the answer to base64
        }

        Ok(None)
    }

    /// Get connection state
    #[allow(dead_code)]
    pub(crate) async fn get_connection_state(&self, tube_id: &str) -> Result<String> {
        let tube = self
            .get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        Ok(tube.connection_state().await)
    }

    /// Close a tube
    pub(crate) async fn close_tube(
        &mut self,
        tube_id: &str,
        reason: Option<CloseConnectionReason>,
    ) -> Result<()> {
        let tube_arc = self.get_by_tube_id(tube_id).ok_or_else(|| {
            warn!(
                "close_tube: Tube not found in registry. (tube_id: {})",
                tube_id
            );
            anyhow!("Tube not found: {}", tube_id)
        })?;

        let current_status = *tube_arc.status.read().await;
        debug!(
            "close_tube: Attempting to close tube. (tube_id: {}, status: {})",
            tube_id, current_status
        );

        match current_status {
            crate::tube_and_channel_helpers::TubeStatus::Initializing => {
                error!("close_tube: Attempted to close tube while it is still initializing. Operation aborted. (tube_id: {})", tube_id);
                Err(anyhow!(
                    "Cannot close tube {}: still initializing.",
                    tube_id
                ))
            }
            crate::tube_and_channel_helpers::TubeStatus::Closing
            | crate::tube_and_channel_helpers::TubeStatus::Closed => {
                info!("close_tube: Tube is already closing or closed. No action needed. (tube_id: {}, status: {})", tube_id, current_status);
                Ok(())
            }
            _ => {
                // New, Connecting, Active, Ready, Failed
                // Transition to Closing state first
                *tube_arc.status.write().await =
                    crate::tube_and_channel_helpers::TubeStatus::Closing;
                info!("close_tube: Transitioned tube status to Closing. Proceeding with close. (tube_id: {})", tube_id);

                // Use the provided reason or default to AdminClosed
                let close_reason = reason.unwrap_or(CloseConnectionReason::AdminClosed);
                tube_arc
                    .close_with_reason(self, close_reason)
                    .await
                    .map_err(|e| {
                        error!(
                            "close_tube: tube.close_with_reason() failed: {} (tube_id: {})",
                            e, tube_id
                        );
                        anyhow!(
                            "Failed during tube.close_with_reason() for {}: {}",
                            tube_id,
                            e
                        )
                    })
            }
        }
    }

    /// Add an ICE candidate received from the external source
    #[allow(dead_code)]
    pub(crate) async fn add_external_ice_candidate(
        &self,
        tube_id: &str,
        candidate: &str,
    ) -> Result<()> {
        let tube = self
            .get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;

        tube.add_ice_candidate(candidate.to_string())
            .await
            .map_err(|e| anyhow!("Failed to add ICE candidate: {}", e))?;

        Ok(())
    }

    /// Clean up stale tubes that have no active channels and are in terminal states
    /// Returns the list of tube IDs that were successfully cleaned up
    pub(crate) async fn cleanup_stale_tubes(&mut self) -> Vec<String> {
        let mut stale_tube_ids = Vec::new();

        // Collect stale tube IDs
        for (tube_id, tube) in &self.tubes_by_id {
            if tube.is_stale().await {
                debug!("Detected stale tube (tube_id: {})", tube_id);
                stale_tube_ids.push(tube_id.clone());
            }
        }

        // Close stale tubes
        let mut closed_tubes = Vec::new();
        for tube_id in stale_tube_ids {
            debug!("Auto-closing stale tube (tube_id: {})", tube_id);
            match self
                .close_tube(&tube_id, Some(CloseConnectionReason::Timeout))
                .await
            {
                Ok(_) => {
                    info!("Successfully auto-closed stale tube (tube_id: {})", tube_id);
                    closed_tubes.push(tube_id);
                }
                Err(e) => {
                    warn!(
                        "Failed to auto-close stale tube: {} (tube_id: {})",
                        e, tube_id
                    );
                }
            }
        }

        closed_tubes
    }
}
