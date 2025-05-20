use std::collections::HashMap;
use std::sync::Arc;
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
pub(crate) async fn setup_channel_for_data_channel(
    data_channel: &WebRTCDataChannel,
    label: String,
    timeouts: Option<TunnelTimeouts>,
    protocol_settings: HashMap<String, serde_json::Value>,
    server_mode: bool,
) -> anyhow::Result<Channel> {
    // Create a channel to receive messages from the data channel
    let (tx, rx) = mpsc::unbounded_channel();

    // Create the channel
    let channel_instance = Channel::new(
        data_channel.clone(),
        rx,
        label.clone(),
        timeouts,
        protocol_settings,
        server_mode,
    ).await?;
    
    // Set up a message handler for the data channel using zero-copy buffers
    let data_channel_ref = &data_channel.data_channel;
    
    let buffer_pool = Arc::new(crate::buffer_pool::BufferPool::default());
    
    // tx is cloned for the on_message closure. The original tx's receiver (rx) is in channel_instance.
    data_channel_ref.on_message(Box::new(move |msg| {
        let tx_clone = tx.clone(); // Clone tx for the async block
        let buffer_pool_clone = buffer_pool.clone();
        let label_clone = label.clone(); // Clone label for the async block
        
        Box::pin(async move {
            let data = &msg.data;
            let message_bytes = buffer_pool_clone.create_bytes(data);
            let message_len = message_bytes.len();
            
            log::debug!("Channel({}): Received {} bytes from WebRTC data channel", 
                     label_clone, message_len);
            
            if let Err(e) = tx_clone.send(message_bytes) {
                log::error!("Channel({}): Failed to send message to MPSC channel for {}: {}", 
                        label_clone, label_clone, e);
            }
        })
    }));

    Ok(channel_instance)
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