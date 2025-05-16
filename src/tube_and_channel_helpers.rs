use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use crate::channel::Channel;
use crate::models::{NetworkAccessChecker, TunnelTimeouts};
use crate::webrtc_data_channel::WebRTCDataChannel;

// Tube Status
#[derive(Debug, Clone, PartialEq)]
pub enum TubeStatus {
    New,
    Connecting,
    Connected,
    Failed,
    Closed,
}

impl std::fmt::Display for TubeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TubeStatus::New => write!(f, "new"),
            TubeStatus::Connecting => write!(f, "connecting"),
            TubeStatus::Connected => write!(f, "connected"),
            TubeStatus::Failed => write!(f, "failed"),
            TubeStatus::Closed => write!(f, "closed"),
        }
    }
}

// Helper method to set up a data channel message handler and create a Channel
pub(crate) fn setup_channel_for_data_channel(
    data_channel: &WebRTCDataChannel,
    label: String,
    timeouts: Option<TunnelTimeouts>,
) -> Arc<Mutex<Channel>> {
    // Create a channel to receive messages from the data channel
    let (tx, rx) = mpsc::unbounded_channel();

    // Create the channel
    let channel = Channel::new(
        data_channel.clone(),
        rx,
        label.clone(),
        timeouts,
        None, // No network checker initially
    );
    
    // Create an Arc wrapped version to share
    let channel_arc = Arc::new(Mutex::new(channel));
    
    // Set up message handler for the data channel using zero-copy buffers
    let data_channel_ref = &data_channel.data_channel;
    
    // Create a buffer pool for efficient buffer management - once, outside the closure
    // Use Arc to avoid cloning the entire buffer pool
    let buffer_pool = Arc::new(crate::buffer_pool::BufferPool::default());
    
    // Need cloneable values for the FnMut closure
    let tx = tx; // Will clone this inside the closure
    let buffer_pool = buffer_pool; // Arc is already clone-efficient
    let label = label; // Will clone this inside the closure

    data_channel_ref.on_message(Box::new(move |msg| {
        // Clone the values we need here - these are cheap clones (Arc and String)
        let tx = tx.clone();
        let buffer_pool = buffer_pool.clone();
        let label = label.clone();
        
        Box::pin(async move {
            // Get data directly as a slice
            let data = &msg.data;
            
            // Create a BytesMut directly from the data
            let message_bytes = buffer_pool.create_bytes(data);
            let message_len = message_bytes.len();
            
            // Log reception
            log::debug!("Channel({}): Received {} bytes from WebRTC data channel", 
                     label, message_len);
            
            // Send the message through the channel to the Channel struct
            // This will move ownership of message_bytes to the receiver without cloning
            if let Err(e) = tx.send(message_bytes) {
                log::error!("Channel({}): Failed to send message to channel: {}", 
                        label, e);
            }
        })
    }));

    channel_arc
}

// Helper method to parse network rules from settings
pub(crate) fn parse_network_rules_from_settings(settings: &HashMap<String, serde_json::Value>) -> Option<NetworkAccessChecker> {
    if settings.get("allowed_hosts").is_some() || settings.get("allowed_ports").is_some() {
        // Convert allowed_hosts string to Vec<String>
        let allowed_hosts = settings.get("allowed_hosts")
            .and_then(|v| v.as_str())
            .map(|hosts| hosts.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>())
            .unwrap_or_default();  // Unwrap to Vec<String> or empty Vec if None

        // Convert allowed_ports string to Vec<u16>
        let allowed_ports = settings.get("allowed_ports")
            .and_then(|v| v.as_str())
            .map(|ports| ports.split(',')
                .filter_map(|s| s.trim().parse::<u16>().ok())
                .collect::<Vec<u16>>())
            .unwrap_or_default();  // Unwrap to Vec<u16> or empty Vec if None

        Some(NetworkAccessChecker::new(
            allowed_hosts,
            allowed_ports,
        ))
    } else {
        None
    }
}