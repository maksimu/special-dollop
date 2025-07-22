use crate::router_helpers::get_relay_access_creds;
use crate::Tube;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
#[cfg(test)]
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};
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
        debug!(target: "registry", tube_id = %id, "TubeRegistry::add_tube - Adding tube");
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
        if !self.tubes_by_id.contains_key(tube_id) {
            return Err(anyhow!("Tube not found: {}", tube_id));
        }

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
                info!(target: "tube_lifecycle", tube_id = %provided_tube_id, conversation_id = %conversation_id, "Using existing tube for conversation");

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
                                info!(target: "tube_lifecycle", tube_id = %provided_tube_id, conversation_id = %conversation_id, "Successfully created new channel on existing tube");
                            }
                            Err(e) => {
                                warn!(target: "tube_lifecycle", tube_id = %provided_tube_id, conversation_id = %conversation_id, "Failed to create logical channel on existing tube: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(target: "tube_lifecycle", tube_id = %provided_tube_id, conversation_id = %conversation_id, "Failed to create data channel on existing tube: {}", e);
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
            trace!(target: "ice_config", tube_id = %tube_id, krelay_server = %krelay_server, "Server mode: Received krelay_server for ICE server setup");
        } else if let Some(ksm_cfg) = ksm_config {
            trace!(target: "ice_config", tube_id = %tube_id, krelay_server = %krelay_server, ksm_config = %ksm_cfg, "Client mode: Received krelay_server and ksm_config for ICE server setup");
        } else {
            trace!(target: "ice_config", tube_id = %tube_id, krelay_server = %krelay_server, "Client mode: Received krelay_server for ICE server setup, no ksm_config provided");
        }

        let mut ice_servers = Vec::new();
        let mut turn_only_for_config = settings
            .get("turn_only")
            .is_some_and(|v| v.as_bool().unwrap_or(false));
        debug!(target: "ice_config", tube_id = %tube_id, turn_only_setting = turn_only_for_config, "Initial 'turn_only' setting from input");

        // Check for test mode - ksm_config might be None in server mode, so check carefully
        let is_test_mode = ksm_config.is_some_and(|cfg| cfg.starts_with("TEST_MODE_KSM_CONFIG"));

        if is_test_mode {
            info!(target: "ice_config", tube_id = %tube_id, "TEST_MODE_KSM_CONFIG active: Using Google STUN server and disabling TURN for this test configuration.");
            turn_only_for_config = false;
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            info!(target: "ice_config", tube_id = %tube_id, stun_url = "stun:stun.l.google.com:19302?transport=udp&family=ipv4", "Added Google STUN server");
            ice_servers.push(RTCIceServer {
                urls: vec!["stun:stun1.l.google.com:19302?transport=udp&family=ipv4".to_string()],
                username: String::new(),
                credential: String::new(),
            });
            info!(target: "ice_config", tube_id = %tube_id, stun_url = "stun:stun1.l.google.com:19302?transport=udp&family=ipv4", "Added Google STUN server");
        } else if !krelay_server.is_empty() {
            debug!(target: "ice_config", tube_id = %tube_id, relay_server_host = %krelay_server, "Using provided krelay_server for ICE configuration");

            if !turn_only_for_config {
                let stun_url_udp = format!("stun:{krelay_server}:3478");
                ice_servers.push(RTCIceServer {
                    urls: vec![stun_url_udp.clone()],
                    username: String::new(),
                    credential: String::new(),
                });
                debug!(target: "ice_config", tube_id = %tube_id, stun_url = %stun_url_udp, "Added STUN server (UDP)");
            }

            let use_turn_for_config_from_settings = settings
                .get("use_turn")
                .is_none_or(|v| v.as_bool().unwrap_or(true));
            debug!(target: "ice_config", tube_id = %tube_id, use_turn_setting = use_turn_for_config_from_settings, "'use_turn' setting");

            if use_turn_for_config_from_settings {
                // Get TURN credentials if needed - only in client mode when ksm_config is available
                if let Some(ksm_cfg) = ksm_config {
                    if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                        debug!(target: "ice_config", tube_id = %tube_id, "Fetching TURN credentials from router");
                        match get_relay_access_creds(ksm_cfg, None, client_version).await {
                            Ok(creds) => {
                                debug!(target: "ice_config", tube_id = %tube_id, "Successfully fetched TURN credentials");
                                trace!(target: "ice_config", tube_id = %tube_id, credentials = %creds, "Received TURN credentials");

                                // Extract username and password from credentials
                                if let (Some(username), Some(password)) = (
                                    creds.get("username").and_then(|v| v.as_str()),
                                    creds.get("password").and_then(|v| v.as_str()),
                                ) {
                                    debug!(target: "ice_config", tube_id = %tube_id, relay_url = %krelay_server, "Using TURN server");
                                    ice_servers.push(RTCIceServer {
                                        urls: vec![format!("turn:{}", krelay_server)],
                                        username: username.to_string(),
                                        credential: password.to_string(),
                                    });
                                } else {
                                    warn!(target: "ice_config", tube_id = %tube_id, "Invalid TURN credentials format");
                                }
                            }
                            Err(e) => {
                                error!(target: "ice_config", tube_id = %tube_id, "Failed to get TURN credentials: {}", e);
                                // Don't fail the entire operation, just log the error
                            }
                        }
                    }
                } else {
                    debug!(target: "ice_config", tube_id = %tube_id, "Server mode: Skipping TURN credential fetch (no ksm_config available)");
                }
            }
        } else {
            warn!(target: "ice_config", tube_id = %tube_id, "No krelay_server provided. STUN/TURN servers will not be configured.");
        }

        let all_configured_urls: Vec<String> =
            ice_servers.iter().flat_map(|s| s.urls.clone()).collect();
        debug!(target: "ice_config", tube_id = %tube_id, configured_ice_urls = ?all_configured_urls, "Final list of ICE server URLs to be used");

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
            Some(rtc_config)
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
                client_version,
                settings.clone(),
                signal_sender.clone(),
            )
            .await
            .context("Failed to create peer connection")?;

        let mut listening_port_option: Option<u16> = None; // Initialize outside if

        // Conditionally create channels only if in server mode (no initial offer from the client)
        if is_server_mode {
            info!(target: "tube_lifecycle", tube_id = %tube_id, "Server mode: Proactively creating control and main data channels.");
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
                            warn!(target: "tube_lifecycle", tube_id = %tube_id, channel_id = %conversation_id, "Server mode: Failed to create logical channel for main data channel: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!(target: "tube_lifecycle", tube_id = %tube_id, channel_id = %conversation_id, "Server mode: Failed to create main data channel: {}", e);
                }
            }
        } else {
            debug!(target: "tube_lifecycle", tube_id = %tube_id, "Client mode: Expecting client to create data channels via its offer.");
        }

        let mut result_map = HashMap::new();
        result_map.insert("tube_id".to_string(), tube_id.clone());
        if is_server_mode {
            if let Some(port) = listening_port_option {
                result_map.insert(
                    "actual_local_listen_addr".to_string(),
                    format!("127.0.0.1:{port}"),
                );
                debug!(target: "tube_lifecycle", tube_id = %tube_id, listen_addr = format!("127.0.0.1:{port}"), "Server mode: Reporting listening address.");
            } else {
                warn!(target: "tube_lifecycle", tube_id = %tube_id, "Server mode: No listening port obtained for main data channel, not adding actual_local_listen_addr to result.");
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

        info!(
            target: "tube_lifecycle",
            tube_id = %tube_id,
            conversation_id = %conversation_id,
            mode = if is_server_mode {"Server"} else {"Client"},
            result_keys = ?result_map.keys(),
            settings_keys = ?settings.keys(),
            "Tube processing complete."
        );

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
    pub(crate) async fn close_tube(&mut self, tube_id: &str) -> Result<()> {
        let tube_arc = self.get_by_tube_id(tube_id)
            .ok_or_else(|| {
                warn!(target: "registry", tube_id = %tube_id, "close_tube: Tube not found in registry.");
                anyhow!("Tube not found: {}", tube_id)
            })?;

        let current_status = *tube_arc.status.read().await;
        debug!(target: "registry", tube_id = %tube_id, status = %current_status, "close_tube: Attempting to close tube.");

        match current_status {
            crate::tube_and_channel_helpers::TubeStatus::Initializing => {
                error!(target: "registry", tube_id = %tube_id, "close_tube: Attempted to close tube while it is still initializing. Operation aborted.");
                Err(anyhow!(
                    "Cannot close tube {}: still initializing.",
                    tube_id
                ))
            }
            crate::tube_and_channel_helpers::TubeStatus::Closing
            | crate::tube_and_channel_helpers::TubeStatus::Closed => {
                info!(target: "registry", tube_id = %tube_id, status = %current_status, "close_tube: Tube is already closing or closed. No action needed.");
                Ok(())
            }
            _ => {
                // New, Connecting, Active, Ready, Failed
                // Transition to Closing state first
                *tube_arc.status.write().await =
                    crate::tube_and_channel_helpers::TubeStatus::Closing;
                info!(target: "registry", tube_id = %tube_id, "close_tube: Transitioned tube status to Closing. Proceeding with close.");

                tube_arc.close(self).await.map_err(|e| {
                    error!(target: "registry", tube_id = %tube_id, "close_tube: tube.close() failed: {}", e);
                    anyhow!("Failed during tube.close() for {}: {}", tube_id, e)
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
}
