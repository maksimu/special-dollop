use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use anyhow::Result;
use parking_lot::RwLock;
use webrtc::peer_connection::configuration::RTCConfiguration;
use log;

use crate::channel::Channel;
use crate::webrtc_core::{WebRTCPeerConnection, create_data_channel};
use crate::models::TunnelTimeouts;
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::{TubeStatus, setup_channel_for_data_channel};
use crate::tube_registry::REGISTRY;
use crate::webrtc_data_channel::WebRTCDataChannel;

// A single tube holding a WebRTC peer connection and channels
#[derive(Clone)]
pub struct Tube {
    // Unique ID for this tube
    pub(crate) id: String,
    // WebRTC peer connection
    peer_connection: Arc<tokio::sync::Mutex<Option<Arc<WebRTCPeerConnection>>>>,
    // Data channels mapped by label
    pub(crate) data_channels: Arc<RwLock<HashMap<String, WebRTCDataChannel>>>,
    // Control channel (special default channel)
    control_channel: Arc<RwLock<Option<WebRTCDataChannel>>>,
    // Channels (from old_channel) mapped by name
    pub channels: Arc<RwLock<HashMap<String, Arc<Mutex<Channel>>>>>,
    // Current status
    status: Arc<RwLock<TubeStatus>>,
    // Runtime
    runtime: Arc<tokio::runtime::Runtime>,
}

impl Tube {
    // Create a new tube with the optional peer connection
    pub fn new() -> Result<Arc<Self>> {
        let id = Uuid::new_v4().to_string();
        let runtime = get_runtime();

        // Print debug info about the current registry state
        {
            let registry = REGISTRY.read();
            log::debug!("Before adding new tube - registry has {} tubes", registry.all_tube_ids().len());
            log::debug!("Registry tubes: {:?}", registry.all_tube_ids());
        }

        let tube = Arc::new(Self {
            id: id.clone(), // Clone ID to use below
            peer_connection: Arc::new(tokio::sync::Mutex::new(None)),
            data_channels: Arc::new(RwLock::new(HashMap::new())),
            control_channel: Arc::new(RwLock::new(None)),
            channels: Arc::new(RwLock::new(HashMap::new())),
            status: Arc::new(RwLock::new(TubeStatus::New)),
            runtime,
        });

        // Register in the global registry with explicit lock scope
        {
            log::info!("Adding tube with ID: {} to registry", id);
            let mut registry = REGISTRY.write();
            registry.add_tube(Arc::clone(&tube));
        }

        // Verify the tube was added correctly
        {
            let registry = REGISTRY.read();
            let tube_ids = registry.all_tube_ids();
            log::debug!("After adding tube - registry contains: {:?}", tube_ids);
            if !tube_ids.contains(&id) {
                log::error!("ERROR: Added tube not found in registry!");
            }
        }

        Ok(tube)
    }

    pub async fn create_peer_connection(
        &self,
        config: Option<RTCConfiguration>,
        trickle_ice: bool,
        turn_only: bool,
        ksm_config: String,
        callback_token: String
    ) -> Result<()> {
        // Store a strong reference to self to prevent dropping during async operation
        let self_ref = Arc::new(self.id.clone());

        // Get the signal channel from the registry
        let signal_sender = {
            let registry = REGISTRY.read();
            registry.get_signal_channel(&self.id)
        };

        let connection = WebRTCPeerConnection::new(config, trickle_ice, turn_only, ksm_config.clone(), signal_sender).await
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Create Arc BEFORE getting the lock to minimize lock time
        let connection_arc = Arc::new(connection);
        
        // Set up connection state monitoring
        let status = self.status.clone();
        let ksm_config_clone = ksm_config.clone();
        let callback_token_clone = callback_token.clone();
        let tube_id = self.id.clone();
        
        // Set up ICE candidate handler with a signal channel
        connection_arc.setup_ice_candidate_handler(tube_id.clone(), callback_token_clone.clone());
        
        connection_arc.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let status_clone = status.clone();
            Box::pin(async move {
                match state {
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected => {
                        *status_clone.write() = TubeStatus::Connected;
                        log::info!("Tube connection state changed to Connected");
                    },
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed => {
                        *status_clone.write() = TubeStatus::Failed;
                        log::info!("Tube connection state changed to Failed");
                    },
                    webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed => {
                        *status_clone.write() = TubeStatus::Closed;
                        log::info!("Tube connection state changed to Closed");
                    },
                    _ => {
                        log::debug!("Connection state changed to: {:?}", state);
                    }
                }
            })
        }));
        
        // Set up handler for incoming data channels
        let tube_clone = self.clone();
        let ksm_config_for_handler = ksm_config_clone.clone();
        let callback_token_for_handler = callback_token_clone.clone();
        connection_arc.peer_connection.on_data_channel(Box::new(move |rtc_data_channel| {
            let tube = tube_clone.clone();
            let ksm_config_inner = ksm_config_for_handler.clone();
            let callback_token_inner = callback_token_for_handler.clone();
            
            Box::pin(async move {
                let label = rtc_data_channel.label().to_string();
                log::info!("Received data channel from remote peer with label: {}", label);

                // Create our WebRTCDataChannel wrapper
                let data_channel = WebRTCDataChannel::new(rtc_data_channel);

                // Add it to our data channels map
                if let Err(e) = tube.add_data_channel(data_channel.clone()) {
                    log::error!("Failed to add data channel to tube: {}", e);
                    return;
                }

                // If this is the control channel, store it specially
                if label == "control" {
                    *tube.control_channel.write() = Some(data_channel.clone());
                    log::info!("Set control channel");
                    return;
                }

                // Set up the data channel and create a Channel object
                let channel = setup_channel_for_data_channel(&data_channel, label.clone(), ksm_config_inner, callback_token_inner, None);

                // Store in the channel map
                tube.channels.write().insert(label.clone(), Arc::clone(&channel));

                // Spawn the channel runner
                let channel_clone = Arc::clone(&channel);
                let label_clone = label.clone();
                let runtime = get_runtime();

                runtime.spawn(async move {
                    let channel = match channel_clone.lock() {
                        Ok(guard) => (*guard).clone(),
                        Err(e) => {
                            log::error!("Failed to lock channel '{}': {}", label_clone, e);
                            return;
                        }
                    };

                    // For client mode, use wait_for_events instead of run
                    // This handles incoming messages without trying to connect to backends
                    let is_server_mode = {
                        let registry = REGISTRY.read();
                        registry.is_server_mode()
                    }; // Drop the read lock before to await

                    if is_server_mode {
                        if let Err(e) = channel.run().await {
                            log::error!("Channel '{}' encountered an error: {}", label_clone, e);
                        }
                    } else {
                        if let Err(e) = channel.wait_for_events().await {
                            log::error!("Channel '{}' encountered an error in wait_for_events: {}", label_clone, e);
                        }
                    }
                });
                
                log::info!("Successfully set up channel for incoming data channel: {}", label);
            })
        }));

        // Now get the lock
        let mut pc = self.peer_connection.lock().await;
        *pc = Some(connection_arc);

        // Update status
        *self.status.write() = TubeStatus::Connecting;

        // Print debug status
        log::debug!("Updated tube status to: {:?}", self.status());

        // Add a small delay to ensure any pending operations complete
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Verify we're still in the registry (keep _self_ref alive until here)
        {
            let registry = REGISTRY.read();
            let tube_ids = registry.all_tube_ids();
            log::debug!("After creating peer connection - registry contains: {:?}", tube_ids);
            if !tube_ids.contains(&self.id) {
                log::warn!("WARNING: Tube not found in registry after creating peer connection!");

                // Re-add it to the registry as a safeguard using get_tube
                drop(registry); // Release the read lock

                // Cannot re-add directly since we don't have an Arc<Tube> to self
                // Instead, we'll create a flag to indicate that this tube needs to be re-registered
                *self.status.write() = TubeStatus::Connecting;
                log::info!("Marked tube for re-registration");
            }
        }

        // Keep self_ref alive until the end of the function
        let _ = self_ref;

        Ok(())
    }
    
    // Get tube ID
    pub fn id(&self) -> String {
        self.id.clone()
    }
    
    // Set peer connection
    pub async fn set_peer_connection(&self, peer_connection: Arc<WebRTCPeerConnection>) {
        let mut pc = self.peer_connection.lock().await;
        *pc = Some(peer_connection);
    }
    
    // Get reference to peer connection
    pub async fn peer_connection(&self) -> Option<Arc<WebRTCPeerConnection>> {
        let pc = self.peer_connection.lock().await;
        pc.clone()
    }
    
    // Add a data channel
    pub fn add_data_channel(&self, data_channel: WebRTCDataChannel) -> Result<()> {
        let label = data_channel.label();
        
        // If this is the control channel, set it specially
        if label == "control" {
            *self.control_channel.write() = Some(data_channel.clone());
        }
        
        // Add to the channel map
        self.data_channels.write().insert(label, data_channel);
        Ok(())
    }
    
    // Get data channel by label
    pub fn get_data_channel(&self, label: &str) -> Option<WebRTCDataChannel> {
        self.data_channels.read().get(label).cloned()
    }
    
    // Get all data channels
    pub fn get_all_data_channels(&self) -> Vec<WebRTCDataChannel> {
        self.data_channels.read().values().cloned().collect()
    }
    
    // Create a new data channel and add it to the tube
    pub async fn create_data_channel(&self, label: &str) -> Result<WebRTCDataChannel> {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            let rtc_data_channel = create_data_channel(&pc.peer_connection, label).await?;
            let data_channel = WebRTCDataChannel::new(rtc_data_channel);
            
            // Clone for release outside the lock
            let data_channel_clone = data_channel.clone();
            
            // Release lock before adding to avoid potential deadlock
            drop(pc_guard);
            
            // Add to our mapping
            self.add_data_channel(data_channel)?;
            
            Ok(data_channel_clone)
        } else {
            Err(anyhow::anyhow!("No peer connection available").into())
        }
    }
    
    // Create a channel with the given name, using an existing data channel
    pub fn create_channel(
        &self,
        name: &str,
        data_channel: &WebRTCDataChannel,
        ksm_config: String,
        callback_token: String,
        timeout_seconds: Option<f64>,
    ) -> Result<Arc<Mutex<Channel>>> {
        // Create timeouts
        let timeouts = if let Some(timeout) = timeout_seconds {
            Some(TunnelTimeouts {
                read: std::time::Duration::from_secs_f64(timeout),
                ping_timeout: std::time::Duration::from_secs_f64(timeout / 3.0),
                open_connection: std::time::Duration::from_secs_f64(timeout),
                close_connection: std::time::Duration::from_secs_f64(timeout / 2.0),
            })
        } else {
            None
        };

        // Set up the data channel and create a Channel object
        let channel = setup_channel_for_data_channel(data_channel, name.to_string(), ksm_config, callback_token, timeouts);

        // Store in the channel map
        self.channels.write().insert(name.to_string(), Arc::clone(&channel));

        // Spawn the channel runner
        let channel_clone = Arc::clone(&channel);
        let name_clone = name.to_string();
        self.runtime.spawn(async move {
            let channel = match channel_clone.lock() {
                Ok(guard) => (*guard).clone(),
                Err(e) => {
                    log::error!("Failed to lock channel '{}': {}", name_clone, e);
                    return;
                }
            };

            if let Err(e) = channel.run().await {
                log::error!("Channel '{}' encountered an error: {}", name_clone, e);
            }
        });

        Ok(channel)
    }
    
    // Create the default control channel
    pub async fn create_control_channel(&self) -> Result<WebRTCDataChannel> {
        let control_channel = self.create_data_channel("control").await?;
        *self.control_channel.write() = Some(control_channel.clone());
        Ok(control_channel)
    }
    
    // Close a specific channel
    pub fn close_channel(&self, name: &str) -> Result<()> {
        if let Some(channel) = self.channels.write().remove(name) {
            // The channel will be properly closed when dropped
            drop(channel);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Channel not found: {}", name))
        }
    }
    
    // Common helper function for offer/answer creation with ICE gathering
    async fn create_session_description(&self, is_offer: bool) -> Result<String, String> {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            // First create the offer/answer
            let sdp = if is_offer {
                pc.create_offer().await?
            } else {
                pc.create_answer().await?
            };
            
            // Now set it as the local description properly
            // Pass is_answer as true when we created an answer (is_offer is false)
            pc.set_local_description(sdp.clone(), !is_offer).await?;
            
            // Return the SDP
            Ok(sdp)
        } else {
            Err("No peer connection available".to_string())
        }
    }
    
    // Create an offer
    pub async fn create_offer(&self) -> Result<String, String> {
        self.create_session_description(true).await
    }
    
    // Create an answer and send via a signal channel if available
    pub async fn create_answer(&self) -> Result<String, String> {
        self.create_session_description(false).await
    }
    
    // Set remote description
    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
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
    pub async fn add_ice_candidate(&self, candidate: String) -> Result<(), String> {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            pc.add_ice_candidate(candidate).await
        } else {
            Err("No peer connection available".to_string())
        }
    }
    
    // Get connection state
    pub async fn connection_state(&self) -> String {
        let pc_guard = self.peer_connection.lock().await;
        
        if let Some(pc) = &*pc_guard {
            pc.connection_state()
        } else {
            "closed".to_string()
        }
    }
    
    // Close the entire tube
    pub async fn close(&self) -> Result<()> {
        log::info!("Closing tube with ID: {}", self.id);

        // Set the status to Closed first to prevent Drop from trying to remove
        *self.status.write() = TubeStatus::Closed;
        log::info!("Set tube status to Closed");

        // Close all channels
        self.channels.write().clear();

        // Close all data channels
        for (_, dc) in self.data_channels.write().drain() {
            let _ = dc.close().await;
        }

        // Close peer connection if exists
        let mut pc = self.peer_connection.lock().await;
        if let Some(pc) = pc.take() {
            let _ = pc.close().await;
        }

        // Remove from the global registry with explicit error handling
        {
            log::info!("Removing tube {} from registry via close()", self.id);
            let mut registry = REGISTRY.write();
            registry.remove_tube(&self.id);

            // Verify removal
            if registry.all_tube_ids().contains(&self.id) {
                log::warn!("WARNING: Failed to remove tube from registry!");
                // Force removal in case it wasn't properly removed
                registry.tubes_by_id.remove(&self.id);
                registry.conversation_mappings.retain(|_, tid| tid != &self.id);
            } else {
                log::info!("Successfully removed tube from registry");
            }
        }

        // Add a delay to ensure registry updates propagate
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        Ok(())
    }
    
    // Get status
    pub fn status(&self) -> TubeStatus {
        self.status.read().clone()
    }
    
    // Set status
    pub fn set_status(&self, status: TubeStatus) {
        *self.status.write() = status;
    }
}

impl Drop for Tube {
    fn drop(&mut self) {
        log::debug!("Drop called for tube with ID: {}", self.id);

        // Only attempt to remove if not already in Closed state
        // This prevents redundant removal attempts
        if *self.status.read() != TubeStatus::Closed {
            log::debug!("Drop: removing tube {} from registry", self.id);
            
            // Attempt to acquire the lock - if we can't, log an error
            match REGISTRY.try_write() {
                Some(mut registry) => {
                    registry.remove_tube(&self.id);
                    log::debug!("Drop: tube removed from registry");
                    
                    // Force additional cleanup in case references remain
                    registry.tubes_by_id.remove(&self.id);
                    registry.conversation_mappings.retain(|_, tid| tid != &self.id);
                }
                None => {
                    log::error!("Drop: couldn't acquire registry lock for tube {}", self.id);
                    // Spawn a cleanup task on the runtime
                    let id = self.id.clone();
                    if let Ok(runtime) = std::panic::catch_unwind(|| get_runtime()) {
                        runtime.spawn(async move {
                            log::info!("Async cleanup for tube {}", id);
                            // Use a loop with retry logic
                            for attempt in 1..=3 {
                                if let Some(mut registry) = REGISTRY.try_write() {
                                    registry.remove_tube(&id);
                                    registry.tubes_by_id.remove(&id);
                                    registry.conversation_mappings.retain(|_, tid| tid != &id);
                                    log::info!("Async cleanup successful for tube {} on attempt {}", id, attempt);
                                    break;
                                }
                                tokio::time::sleep(tokio::time::Duration::from_millis(10 * attempt)).await;
                            }
                        });
                    }
                }
            }
        } else {
            log::debug!("Drop: tube {} already in Closed state, skipping registry cleanup", self.id);
        }
    }
}
