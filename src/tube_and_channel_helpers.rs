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
    ksm_config: String,
    callback_token: String,
    timeouts: Option<TunnelTimeouts>,
) -> Arc<Mutex<Channel>> {
    // Create a channel to receive messages from the data channel
    let (tx, rx) = mpsc::unbounded_channel();

    // Set up message handler for the data channel
    let tx_clone = tx.clone();
    let data_channel_clone = data_channel.data_channel.clone();

    data_channel_clone.on_message(Box::new(move |msg| {
        let tx = tx_clone.clone();
        Box::pin(async move {
            let _ = tx.send(msg.data.clone());
        })
    }));

    // Create the channel
    let channel = Channel::new(
        data_channel.clone(),
        rx,
        label,
        timeouts,
        None, // No network checker initially
        ksm_config,
        callback_token,
    );

    Arc::new(Mutex::new(channel))
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