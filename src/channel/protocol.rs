// Protocol handler functionality for Channel

use anyhow::{anyhow, Result};
use log::debug;
use std::collections::HashMap;
use crate::runtime::get_runtime;

use super::core::Channel;
use super::protocols::create_protocol_handler;
use crate::tube_and_channel_helpers::parse_network_rules_from_settings;

impl Channel {
    // Register a protocol handler for a specific protocol type
    pub async fn register_protocol_handler(
        &mut self,
        protocol_type: &str,
        settings: HashMap<String, serde_json::Value>,
    ) -> Result<()> {

        // Create a network checker if needed
        self.network_checker = parse_network_rules_from_settings(&settings);
        
        debug!("Channel({}): Creating protocol handler for '{}'", self.channel_id, protocol_type);
        
        let mut handler = create_protocol_handler(
            self.server_mode,
            settings,
        )?;
        
        // Set the WebRTC data channel for the handler to use for sending data
        debug!("Channel({}): Setting WebRTC channel for protocol handler '{}'", 
               self.channel_id, protocol_type);
        handler.set_webrtc_channel(self.webrtc.clone());
        
        // Initialize the protocol handler
        debug!("Channel({}): Initializing protocol handler '{}'", self.channel_id, protocol_type);
        handler.initialize().await?;
        
        // Notify handler of current WebRTC state
        let current_state = self.webrtc.ready_state();
        debug!("Channel({}): Notifying handler '{}' of WebRTC state: {}", 
               self.channel_id, protocol_type, current_state);
        handler.on_webrtc_channel_state_change(&current_state).await?;
        
        // Store the protocol handler
        debug!("Channel({}): Storing protocol handler '{}'", self.channel_id, protocol_type);
        self.protocol_handlers.insert(protocol_type.to_string(), handler);
        
        // Set up WebRTC state monitoring if not already done
        debug!("Channel({}): Setting up WebRTC state monitoring", self.channel_id);
        self.setup_webrtc_state_monitoring();
        
        // Verify the handler was registered successfully
        if !self.protocol_handlers.contains_key(protocol_type) {
            log::error!("Channel({}): Failed to verify protocol handler registration for '{}'", 
                        self.channel_id, protocol_type);
            return Err(anyhow!("Failed to register protocol handler for {}", protocol_type));
        }

        debug!("Channel({}): Testing protocol handler status for '{}'", self.channel_id, protocol_type);
        if let Some(handler) = self.protocol_handlers.get(protocol_type) {
            let status = handler.status();
            debug!("Channel({}): Protocol handler '{}' status: {}", 
                   self.channel_id, protocol_type, status);
        } else {
            log::error!("Channel({}): Protocol handler '{}' not found after registration", 
                       self.channel_id, protocol_type);
            return Err(anyhow!("Protocol handler not found after registration: {}", protocol_type));
        }
        
        log::info!("Endpoint {}: Registered protocol handler for {}", self.channel_id, protocol_type);
        Ok(())
    }
    
    // Setup WebRTC state change monitoring to notify protocol handlers
    fn setup_webrtc_state_monitoring(&mut self) {
        // Clone what we need for the handler callbacks
        let webrtc = self.webrtc.clone();
        let channel_id = self.channel_id.clone();
        
        // Only collect protocol type names, not trying to clone the actual handlers
        let protocol_types: Vec<String> = self.protocol_handlers.keys().cloned().collect();
        
        // We only set this up once per channel
        static STATE_MONITORING_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if STATE_MONITORING_SET.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return; // Already set up
        }
        
        // Create a reference to share with the closure
        let server_mode_clone = self.server_mode.clone();
        
        // Set up the state change handler directly on the WebRTC data channel
        let last_state = webrtc.ready_state();
        
        // Create a shared channel for state change notifications
        let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);
        
        // Set up the WebRTC data channel state change callback
        let data_channel = webrtc.data_channel.clone();
        let state_tx_clone = state_tx.clone();
        
        // Register callback for data channel state changes - properly access the state change functionality
        data_channel.on_open(Box::new(move || {
            let tx = state_tx_clone.clone();
            
            // Spawn task to send state change notification for open
            tokio::spawn(async move {
                if let Err(e) = tx.send("Open".to_string()).await {
                    log::error!("Failed to send open state notification: {}", e);
                }
            });
            
            Box::pin(async {})
        }));
        
        let state_tx_clone2 = state_tx.clone();
        data_channel.on_close(Box::new(move || {
            let tx = state_tx_clone2.clone();
            
            // Spawn task to send state change notification for close
            tokio::spawn(async move {
                if let Err(e) = tx.send("Closed".to_string()).await {
                    log::error!("Failed to send close state notification: {}", e);
                }
            });
            
            Box::pin(async {})
        }));
        
        let state_tx_clone3 = state_tx.clone();
        data_channel.on_error(Box::new(move |err| {
            let tx = state_tx_clone3.clone();
            let err_str = format!("Error: {}", err);
            
            // Spawn task to send state change notification for error
            tokio::spawn(async move {
                if let Err(e) = tx.send(err_str).await {
                    log::error!("Failed to send error state notification: {}", e);
                }
            });
            
            Box::pin(async {})
        }));
        
        // We need to clone these for the task that processes the events
        let webrtc_clone = webrtc.clone();
        let channel_id_clone = channel_id.clone();
        let protocol_types_clone = protocol_types.clone();
        
        // Spawn a task to handle the state change events
        let runtime = get_runtime();
        runtime.spawn(async move {
            let mut last_state_in_task = last_state;
            
            // Process state change events
            while let Some(current_state) = state_rx.recv().await {
                // Only process if the state actually changed
                if current_state != last_state_in_task {
                    log::info!("Endpoint {}: WebRTC state changed: {} -> {}", 
                             channel_id_clone, last_state_in_task, current_state);
                    
                    // Process each protocol type
                    for protocol_type in &protocol_types_clone {
                        // Create a new handler instance for each type
                        match create_protocol_handler(server_mode_clone, HashMap::new()) {
                            Ok(mut handler) => {
                                // Set the WebRTC channel
                                handler.set_webrtc_channel(webrtc_clone.clone());
                                
                                // Notify about state change
                                if let Err(e) = handler.on_webrtc_channel_state_change(&current_state).await {
                                    log::error!("Endpoint {}: Error notifying {} handler of state change: {}", 
                                             channel_id_clone, protocol_type, e);
                                }
                            },
                            Err(e) => {
                                log::error!("Endpoint {}: Failed to create handler for {}: {}", 
                                         channel_id_clone, protocol_type, e);
                            }
                        }
                    }
                    
                    // Update last state
                    last_state_in_task = current_state.clone();
                }
                
                // If closed/closing, break the loop
                if current_state.to_lowercase() == "closed" || current_state.to_lowercase() == "closing" {
                    debug!("Endpoint {}: WebRTC channel closed, stopping state monitoring", channel_id_clone);
                    
                    // Reset the monitoring flag so it can be set up again if needed
                    STATE_MONITORING_SET.store(false, std::sync::atomic::Ordering::Release);
                    break;
                }
            }
        });
    }
    
    // Send a command to a protocol handler
    pub async fn send_handler_command(
        &mut self,
        protocol_type: &str,
        command: &str,
        params: &[u8],
    ) -> Result<()> {
        if let Some(handler) = self.protocol_handlers.get_mut(protocol_type) {
            handler.handle_command(command, params).await
        } else {
            Err(anyhow::anyhow!("Protocol handler not found for {}", protocol_type))
        }
    }
    
    // Get protocol handler status
    pub fn get_handler_status(&self, protocol_type: &str) -> Result<String> {
        if let Some(handler) = self.protocol_handlers.get(protocol_type) {
            Ok(handler.status())
        } else {
            Err(anyhow::anyhow!("Protocol handler not found for {}", protocol_type))
        }
    }
}
