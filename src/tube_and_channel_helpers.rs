use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::channel::Channel;
use crate::models::{NetworkAccessChecker, TunnelTimeouts};
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::tube::Tube;
use tracing::{debug, error, trace};

// Tube Status
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TubeStatus {
    New,
    Initializing, // Tube is being set up by PyTubeRegistry::create_tube
    Connecting,   // ICE/DTLS negotiation in progress
    Active,       // ICE/DTLS connected, initial channels (like control) are open and ready
    Ready,        // All initial setup complete, data channels are open and operational
    Failed,
    Closing,      // Close has been initiated
    Closed,
    Disconnected,
}

impl std::fmt::Display for TubeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TubeStatus::New => write!(f, "new"),
            TubeStatus::Initializing => write!(f, "initializing"),
            TubeStatus::Connecting => write!(f, "connecting"),
            TubeStatus::Active => write!(f, "active"),
            TubeStatus::Ready => write!(f, "ready"),
            TubeStatus::Failed => write!(f, "failed"),
            TubeStatus::Closing => write!(f, "closing"),
            TubeStatus::Closed => write!(f, "closed"),
            TubeStatus::Disconnected => write!(f, "disconnected"),       
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
    callback_token: Option<String>,
    ksm_config: Option<String>,
    tube: Option<Arc<Tube>>,
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
        callback_token,
        ksm_config,
    ).await?;
    
    // Register the channel with the tube if provided
    if let Some(tube_arc) = tube {
        tube_arc.register_channel(label.clone(), channel_instance.clone()).await?;
        debug!(target: "channel_setup", channel_name = %label, tube_id = %tube_arc.id, "Channel registered with tube");
    }
    
    // Set up a message handler for the data channel using zero-copy buffers
    let data_channel_ref = &data_channel.data_channel;
    
    let buffer_pool = Arc::new(crate::buffer_pool::BufferPool::default());
    
    // Tx is cloned for the on_message closure. The original tx's receiver (rx) is in channel_instance.
    data_channel_ref.on_message(Box::new(move |msg| {
        trace!( target: "on_message", message_size = msg.data.len(), "Channel got message");
        let tx_clone = tx.clone(); // Clone tx for the async block
        let buffer_pool_clone = buffer_pool.clone();
        let label_clone = label.clone(); // Clone label for the async block
        
        Box::pin(async move {
            let data = &msg.data;
            let message_bytes = buffer_pool_clone.create_bytes(data);
            let message_len = message_bytes.len();
            
            debug!(
                target: "channel_flow", 
                channel_id = %label_clone, 
                bytes_count = message_len, 
                "Channel: Received bytes from WebRTC data channel"
            );
            
            if let Err(_e) = tx_clone.send(message_bytes) {
                error!(
                    target: "channel_flow", 
                    channel_id = %label_clone, 
                    "Channel: Failed to send message to MPSC channel for processing"
                );
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