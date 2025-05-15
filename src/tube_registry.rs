use std::collections::HashMap;
use std::sync::Arc;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use crate::Tube;
use log;
use anyhow::{Result, anyhow};
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use bytes::Bytes;
use crate::router_helpers::{get_relay_access_creds, router_url_from_ksm_config};
use std::sync::mpsc::{channel, Sender, Receiver};

// Define a message structure for signaling
#[derive(Debug, Clone)]
pub struct SignalMessage {
    pub tube_id: String,
    pub kind: String,  // "icecandidate", "answer", etc.
    pub data: String,
    pub conversation_id: String,
}

// Global registry for all tubes - using Lazy with explicit thread safety
pub(crate) static REGISTRY: Lazy<RwLock<TubeRegistry>> = Lazy::new(|| {
    log::info!("Initializing global tube registry");
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
    pub(crate) signal_channels: HashMap<String, Sender<SignalMessage>>,
}

impl TubeRegistry {
    pub(crate) fn new() -> Self {
        Self {
            tubes_by_id: HashMap::new(),
            conversation_mappings: HashMap::new(),
            server_mode: false, // Default to client mode
            signal_channels: HashMap::new(),
        }
    }

    // Register a signal channel for a tube
    pub(crate) fn register_signal_channel(&mut self, tube_id: &str) -> Receiver<SignalMessage> {
        let (sender, receiver) = channel::<SignalMessage>();
        self.signal_channels.insert(tube_id.to_string(), sender);
        receiver
    }

    // Remove a signal channel
    pub(crate) fn remove_signal_channel(&mut self, tube_id: &str) {
        self.signal_channels.remove(tube_id);
    }

    // Get a signal channel sender
    pub(crate) fn get_signal_channel(&self, tube_id: &str) -> Option<Sender<SignalMessage>> {
        self.signal_channels.get(tube_id).cloned()
    }

    // Send a message to the signal channel for a tube
    pub(crate) fn send_signal(&self, message: SignalMessage) -> Result<()> {
        if let Some(sender) = self.signal_channels.get(&message.tube_id) {
            sender.send(message).map_err(|e| anyhow!("Failed to send signal: {}", e))?;
            Ok(())
        } else {
            Err(anyhow!("No signal channel found for tube: {}", message.tube_id))
        }
    }

    // Add a tube to the registry
    pub(crate) fn add_tube(&mut self, tube: Arc<Tube>) {
        let id = tube.id();
        log::debug!("TubeRegistry::add_tube - Adding tube with ID: {}", id);
        self.tubes_by_id.insert(id, Arc::clone(&tube));

        // Verify it was added
        if !self.tubes_by_id.contains_key(&tube.id()) {
            log::error!("ERROR: Failed to add tube to registry!");
        }
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
        log::debug!("TubeRegistry::get_by_tube_id - Looking for tube: {}", tube_id);
        log::debug!("Available tubes: {:?}", self.tubes_by_id.keys().collect::<Vec<_>>());

        if let Some(tube) = self.tubes_by_id.get(tube_id) {
            log::debug!("Found tube with ID: {}", tube_id);
            Some(Arc::clone(tube))
        } else {
            log::debug!("Tube with ID {} not found in registry", tube_id);
            None
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
    pub(crate) fn associate_conversation(&mut self, tube_id: &str, conversation_id: &str) -> Result<()> {
        if !self.tubes_by_id.contains_key(tube_id) {
            return Err(anyhow!("Tube not found: {}", tube_id));
        }

        self.conversation_mappings.insert(conversation_id.to_string(), tube_id.to_string());
        Ok(())
    }

    // Get all tube IDs
    pub(crate) fn all_tube_ids(&self) -> Vec<String> {
        self.tubes_by_id.keys().cloned().collect()
    }

    // Find tubes by partial match of tube ID or conversation ID
    pub(crate) fn find_tubes(&self, search_term: &str) -> Vec<Arc<Tube>> {
        let mut results = Vec::new();

        // Search in tube IDs
        for (id, tube) in &self.tubes_by_id {
            if id.contains(search_term) {
                results.push(Arc::clone(tube));
            }
        }

        // Search in conversation IDs
        for (conv_id, tube_id) in &self.conversation_mappings {
            if conv_id.contains(search_term) {
                if let Some(tube) = self.tubes_by_id.get(tube_id) {
                    // Only add if not already in results
                    if !results.iter().any(|t| t.id() == tube.id()) {
                        results.push(Arc::clone(tube));
                    }
                }
            }
        }

        results
    }
    
    /// get all conversations from a tube id
    pub(crate) fn tube_id_from_conversation_id(&self, conversation_id: &str) -> Option<&String> {
        self.conversation_mappings.get(conversation_id)
    }
    
    /// get all conversation ids by tube id 
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
    pub(crate) async fn create_tube(
        &mut self,
        conversation_id: &str,
        protocol_type: &str,
        settings: HashMap<String, serde_json::Value>,
        is_server_mode: bool,
        trickle_ice: bool,
        callback_token: &str,
        ksm_config: &str,
    ) -> Result<(Arc<Tube>, Option<String>)> {
        // Create a new tube
        let tube = Tube::new()?;
        
        // Add it to the registry
        self.add_tube(Arc::clone(&tube));
        
        // Associate with conversation ID
        self.associate_conversation(&tube.id(), conversation_id)?;
        
        // Set the server mode
        self.set_server_mode(is_server_mode);
        
        let turn_only = settings.get("turn_only").map_or(false, |v| v.as_bool().unwrap_or(false));
        let use_turn = settings.get("use_turn").map_or(true, |v| v.as_bool().unwrap_or(true));
        let relay_server = router_url_from_ksm_config(ksm_config)?;

        // Configure ICE servers
        let mut ice_servers = Vec::new();

        // Add STUN server first (unless turn_only mode)
        if !turn_only {
            let stun_url = format!("stun:{}:3478", relay_server);
            let stun_server = RTCIceServer {
                urls: vec![stun_url],
                username: String::new(),
                credential: String::new(),
            };
            ice_servers.push(stun_server);
        }

        // Only get and use TURN credentials if the use_turn setting is set to true
        if use_turn {
            // Get TURN credentials using the router_helpers
            let creds = get_relay_access_creds(ksm_config, None)
                .await
                .map_err(|e| anyhow!("Failed to get relay access credentials: {}", e))?;

            // Extract username and password from the JSON response
            let username = creds.get("username")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();

            let credential = creds.get("password")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();

            log::debug!("Got TURN credentials: username={}, password=***", username);

            if !username.is_empty() && !credential.is_empty() {
                let turn_url = format!("turn:{}:3478", relay_server);
                let turn_server = RTCIceServer {
                    urls: vec![turn_url],
                    username,
                    credential,
                };
                ice_servers.push(turn_server);
            }
        } else {
            log::debug!("TURN is disabled, using STUN-only configuration");
        }


        // Configure peer connection
        let config = {
            // Create a custom config with the provided ICE servers
            let mut config = RTCConfiguration {
                ice_servers,
                ..Default::default()
            };
            
            // Set ICE transport policy if turn_only is true
            if turn_only {
                config.ice_transport_policy = RTCIceTransportPolicy::Relay;
            } else {
                config.ice_transport_policy = RTCIceTransportPolicy::All;
            }
            
            Some(config)
        };
        
        // Create the peer connection
        tube.create_peer_connection(
            config,
            trickle_ice,
            turn_only,
            ksm_config.to_string(),
            callback_token.to_string(),
        ).await?;
        
        // Create the control channel
        match tube.create_control_channel().await {
            Ok(_) => log::debug!("Control channel created for tube {}", tube.id()),
            Err(e) => log::warn!("Failed to create control channel: {}", e),
        }
        
        // Create a data channel for the conversation
        let data_channel = tube.create_data_channel(conversation_id).await?;
        
        // Create the channel
        let channel = tube.create_channel(
            conversation_id,
            &data_channel,
            ksm_config.to_string(),
            callback_token.to_string(),
            None,  // timeout_seconds
        )?;
        
        // If settings are provided, register protocol handler
        let mut channel_guard = channel.lock().map_err(|_| anyhow!("Failed to lock channel"))?;
        
        if let Err(e) = channel_guard.register_protocol_handler(protocol_type, settings).await {
            log::warn!("Failed to register protocol handler: {}", e);
        }
        
        // Start the connection
        if is_server_mode {
            if let Err(e) = channel_guard.send_handler_command(protocol_type, "connect", &Bytes::new()).await {
                log::warn!("Failed to connect protocol handler: {}", e);
            }
        }
        
        // Generate offer or answer based on server mode
        let sdp = if is_server_mode {
            // Server mode - create an offer
            Some(tube.create_offer().await.map_err(|e| anyhow!("Failed to create offer: {}", e))?)
        } else {
            // Client mode - no SDP to return yet
            None
        };
        
        Ok((tube, sdp))
    }
    
    /// Set remote description and create answer if needed
    pub(crate) async fn set_remote_description(
        &self,
        tube_id: &str,
        sdp: &str,
        is_answer: bool,
    ) -> Result<Option<String>> {
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        // Set the remote description
        tube.set_remote_description(sdp.to_string(), is_answer).await
            .map_err(|e| anyhow!("Failed to set remote description: {}", e))?;
        
        // If this is an offer, create an answer
        if !is_answer {
            let answer = tube.create_answer().await
                .map_err(|e| anyhow!("Failed to create answer: {}", e))?;
            
            return Ok(Some(answer));
        }
        
        Ok(None)
    }
    
    /// Add an ICE candidate
    pub(crate) async fn add_ice_candidate(
        &self,
        tube_id: &str,
        candidate: &str,
    ) -> Result<()> {
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        tube.add_ice_candidate(candidate.to_string()).await
            .map_err(|e| anyhow!("Failed to add ICE candidate: {}", e))?;
        
        Ok(())
    }
    
    /// Get connection state
    pub(crate) async fn get_connection_state(&self, tube_id: &str) -> Result<String> {
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        Ok(tube.connection_state().await)
    }
    
    /// Close a tube
    pub(crate) async fn close_tube(&mut self, tube_id: &str) -> Result<()> {
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        // Close the tube
        tube.close().await?;
        
        // Remove from the registry
        self.remove_tube(tube_id);
        
        Ok(())
    }
    
    /// Register a channel and set up a data channel
    pub(crate) async fn register_channel(
        &mut self,
        channel_id: &str,
        tube_id: &str,
        settings: &HashMap<String, serde_json::Value>,
    ) -> Result<()> {

        // Associate conversation with tube
        self.associate_conversation(&tube_id, channel_id)?;

        let tube = match self.get_by_tube_id(tube_id) {
            Some(existing_tube) => existing_tube,
            None => return Err(anyhow!("Tube not found: {}", tube_id)),
        };
        
        // Create a new tube
        let trickle_ice = settings.contains_key("trickle_ice");
        let turn_only = settings.contains_key("turn_only");

        // Check for required configuration keys
        if !settings.contains_key("ksm_config") {
            return Err(anyhow::anyhow!("Missing required setting: ksm_config"));
        }

        if !settings.contains_key("callback_token") {
            return Err(anyhow::anyhow!("Missing required setting: callback_token"));
        }

        // Retrieve the values now that we know they exist
        let ksm_config = settings.get("ksm_config").unwrap();
        let callback_token = settings.get("callback_token").unwrap();

        // Create WebRTC peer connection with the default configuration
        tube.create_peer_connection(None, trickle_ice, turn_only, ksm_config.to_string(), callback_token.to_string()).await?;
        
        // Create a data channel for this channel
        let data_channel = tube.create_data_channel(channel_id).await?;
        
        // Create the channel
        tube.create_channel(channel_id, &data_channel, ksm_config.to_string(), callback_token.to_string(), None)?;
        
        // Create the control channel
        if let Err(e) = tube.create_control_channel().await {
            log::warn!("Failed to create control channel: {}", e);
        }
        
        // Get the channel
        let channels = tube.channels.read();
        if let Some(channel) = channels.get(channel_id) {
            // Use the provided protocol type if available, otherwise determine from settings
            let protocol_type =
                // Determine protocol type from settings
                if settings.contains_key("destination") {
                    if settings.contains_key("client_id") {
                        "guacd"
                    } else {
                        "socks5"
                    }
                } else {
                    "port_forward"
                };
            
            // Register the protocol handler and apply settings
            let channel_clone = channel.clone();
            drop(channels); // Release lock before async operation

            let mut channel_guard = match channel_clone.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("Failed to lock channel: {}", e);
                    return Err(anyhow!("Failed to lock channel"));
                }
            };
            if let Err(e) = channel_guard.register_protocol_handler(protocol_type, settings.clone()).await {
                log::warn!("Failed to register protocol handler: {}", e);
            }
        }
        
        // Add it to the registry - mutate the registry
        {
            let mut registry = REGISTRY.write();
            registry.add_tube(Arc::clone(&tube));
        }
        
        Ok(())
    }

    /// Register a protocol handler for a channel
    pub(crate) async fn register_protocol_handler(
        &self,
        tube_id: &str,
        channel_name: &str,
        protocol_type: &str,
        settings: HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        // Get the tube from registry
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        // Get the channel
        let channels = tube.channels.read();
        let channel = channels.get(channel_name)
            .ok_or_else(|| anyhow!("Channel not found: {}", channel_name))?;
        
        // Get a lock on the channel
        let mut channel_guard = channel.lock()
            .map_err(|_| anyhow!("Failed to lock channel"))?;
        
        // Register the protocol handler
        channel_guard.register_protocol_handler(protocol_type, settings).await
            .map_err(|e| anyhow!("Failed to register protocol handler: {}", e))
    }

    /// Send a command to a protocol handler
    pub(crate) async fn send_handler_command(
        &self,
        tube_id: &str,
        channel_name: &str,
        protocol_type: &str,
        command: &str,
        params: Bytes,
    ) -> Result<()> {
        // Get the tube from registry
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        // Get the channel
        let channels = tube.channels.read();
        let channel = channels.get(channel_name)
            .ok_or_else(|| anyhow!("Channel not found: {}", channel_name))?;
        
        // Get a lock on the channel
        let mut channel_guard = channel.lock()
            .map_err(|_| anyhow!("Failed to lock channel"))?;
        
        // Send the command
        channel_guard.send_handler_command(protocol_type, command, &params).await
            .map_err(|e| anyhow!("Failed to send command: {}", e))
    }

    /// Get the status of a protocol handler
    pub(crate) fn get_handler_status(
        &self,
        tube_id: &str,
        channel_id: &str,
        protocol_type: &str,
    ) -> Result<String> {
        // Get the tube from registry
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        // Get the channel
        let channels = tube.channels.read();
        let channel = channels.get(channel_id)
            .ok_or_else(|| anyhow!("Channel not found: {}", channel_id))?;
        
        // Get a lock on the channel
        let channel_guard = channel.lock()
            .map_err(|_| anyhow!("Failed to lock channel"))?;
        
        // Get the status
        channel_guard.get_handler_status(protocol_type)
            .map_err(|e| anyhow!("Failed to get protocol handler status: {}", e))
    }

    /// Create a new connection, either using an existing tube or creating a new one
    pub(crate) async fn new_connection(
        &mut self,
        tube_id: Option<&str>,
        channel_id: &str,
        settings: HashMap<String, serde_json::Value>,
        server_mode: bool,
    ) -> Result<String> {
        // Handle tube_id logic
        let the_tube_id = match tube_id {
            Some(id) => {
                // Check if the tube exists
                if self.tubes_by_id.contains_key(id) {
                    id.to_string()
                } else {
                    return Err(anyhow!("Tube not found: {}", id));
                }
            },
            None => {
                // Create a new tube
                let new_tube = Tube::new()?;
                let tube_id = new_tube.id();
                
                // Add the tube to the registry
                self.add_tube(new_tube);
                log::info!("Created new tube with ID: {}", tube_id);
                tube_id
            }
        };
        
        // Set server mode
        self.set_server_mode(server_mode);
        
        // Register the channel with the specified protocol type and settings
        self.register_channel(channel_id, &the_tube_id, &settings).await?;
        
        Ok(the_tube_id)
    }

    /// Add an ICE candidate received from the external source
    pub(crate) async fn add_external_ice_candidate(&self, tube_id: &str, candidate: &str) -> Result<()> {
        let tube = self.get_by_tube_id(tube_id)
            .ok_or_else(|| anyhow!("Tube not found: {}", tube_id))?;
        
        tube.add_ice_candidate(candidate.to_string()).await
            .map_err(|e| anyhow!("Failed to add ICE candidate: {}", e))
    }
}
