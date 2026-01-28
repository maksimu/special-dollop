use crate::channel::{Channel, PythonHandlerMessage};
use crate::models::{NetworkAccessChecker, TunnelTimeouts};
use crate::unlikely; // Branch prediction optimization
use crate::webrtc_data_channel::WebRTCDataChannel;
use log::{debug, error};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

// Tube Status
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TubeStatus {
    New,
    Initializing, // Tube is being set up by PyTubeRegistry::create_tube
    Connecting,   // ICE/DTLS negotiation in progress
    Active,       // ICE/DTLS connected, initial channels (like control) are open and ready
    Ready,        // All initial setup is complete, data channels are open and operational
    Failed,
    Closing, // Close has been initiated
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn setup_channel_for_data_channel(
    data_channel: &WebRTCDataChannel,
    peer_connection: &crate::webrtc_core::WebRTCPeerConnection,
    label: String,
    timeouts: Option<TunnelTimeouts>,
    protocol_settings: HashMap<String, serde_json::Value>,
    server_mode: bool,
    callback_token: Option<String>,
    ksm_config: Option<String>,
    client_version: String,
    capabilities: crate::tube_protocol::Capabilities,
    python_handler_tx: Option<mpsc::Sender<PythonHandlerMessage>>,
    handler_registry: Option<Arc<guacr::ProtocolHandlerRegistry>>,
    spawned_task_completion_tx: Arc<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<Channel> {
    // Create a channel to receive messages from the data channel
    let (tx, rx) = mpsc::unbounded_channel();

    // Create shutdown notifier for clean async cancellation
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());

    let channel_instance = Channel::new(crate::channel::core::ChannelParams {
        webrtc: data_channel.clone(),
        rx_from_dc: rx,
        channel_id: label.clone(),
        timeouts,
        protocol_settings,
        server_mode,
        shutdown_notify,
        callback_token,
        ksm_config,
        client_version,
        capabilities,
        python_handler_tx,
        handler_registry,
        spawned_task_completion_tx,
    })
    .await?;

    // Set up a message handler for the data channel using zero-copy buffers
    let data_channel_ref = &data_channel.data_channel;

    let buffer_pool = Arc::new(crate::buffer_pool::BufferPool::new(
        crate::buffer_pool::STANDARD_BUFFER_CONFIG,
    ));

    // CRITICAL: Set the new on_message handler FIRST, before draining the early buffer.
    // This ensures no messages are lost in the transition window.
    // Order matters:
    // 1. Set new handler (replaces early buffer callback)
    // 2. Drain early buffer (returns messages captured before step 1)
    // 3. Forward early messages to channel
    // Any messages arriving after step 1 go directly to the new handler.
    // Messages that arrived before step 1 are in the buffer and get forwarded in step 3.
    let peer_connection_clone = peer_connection.clone();
    let tx_for_handler = tx.clone();
    let label_for_handler = label.clone();
    data_channel_ref.on_message(Box::new(move |msg| {
        let tx_clone = tx_for_handler.clone(); // Clone tx for the async block
        let buffer_pool_clone = buffer_pool.clone();
        let label_clone = label_for_handler.clone(); // Clone label for the async block
        let pc_clone = peer_connection_clone.clone();

        Box::pin(async move {
            // Update activity timestamp when receiving data
            pc_clone.update_activity();
            let data = &msg.data;
            let message_bytes = buffer_pool_clone.create_bytes(data);
            let message_len = message_bytes.len();

            // Record metrics for message received (non-blocking, minimal performance impact)
            crate::metrics::METRICS_COLLECTOR.record_message_received(
                &label_clone, // using label as conversation_id
                message_len as u64,
                None, // latency calculation could be added later if needed
            );

            // HOT PATH: Only log WebRTC receives in verbose mode
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Channel: Received bytes from WebRTC data channel (channel_id: {}, bytes_count: {})",
                    label_clone,
                    message_len
                );
            }

            if let Err(_e) = tx_clone.send(message_bytes) {
                error!(
                    "Channel: Failed to send message to MPSC channel for processing (channel_id: {})",
                    label_clone
                );
            }
        })
    }));

    // NOW drain the early buffer (after new handler is set).
    // The new handler is already active, so any new messages go there.
    // This just returns messages that arrived before we set the new handler.
    let early_messages = data_channel.take_early_messages();
    let early_count = early_messages.len();

    // Forward any early-buffered messages to the channel
    // These messages arrived before the new handler was set.
    if early_count > 0 {
        debug!(
            "Channel: Forwarding {} early-buffered messages to channel (channel_id: {})",
            early_count, label
        );
        for msg in early_messages {
            let message_len = msg.len();
            // Record metrics for early messages too
            crate::metrics::METRICS_COLLECTOR.record_message_received(
                &label,
                message_len as u64,
                None,
            );
            if let Err(_e) = tx.send(msg) {
                error!(
                    "Channel: Failed to forward early message to MPSC channel (channel_id: {})",
                    label
                );
            }
        }
        debug!(
            "Channel: Finished forwarding early messages (channel_id: {})",
            label
        );
    }

    Ok(channel_instance)
}

// Helper method to parse network rules from settings
pub(crate) fn parse_network_rules_from_settings(
    settings: &HashMap<String, serde_json::Value>,
) -> Option<NetworkAccessChecker> {
    // Convert allowed_hosts string to Vec<String>
    let allowed_hosts = settings
        .get("allowed_hosts")
        .and_then(|v| v.as_str())
        .map(|hosts| {
            hosts
                .split(',') // Comma-separated from Python join
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    // Convert allowed_ports string to Vec<u16>
    let allowed_ports = settings
        .get("allowed_ports")
        .and_then(|v| v.as_str())
        .map(|ports| {
            ports
                .split(',') // Comma-separated from Python join
                .filter_map(|s| {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return None;
                    }
                    match trimmed.parse::<u16>() {
                        Ok(port) => {
                            if port > 0 {
                                Some(port)
                            } else {
                                debug!("Warning: Port {} is out of the valid range and will be ignored.", port);
                                None
                            }
                        }
                        Err(_) => {
                            debug!("Warning: '{}' is not a valid port number and will be ignored.", trimmed);
                            None
                        }
                    }
                })
                .collect::<Vec<u16>>()
        })
        .unwrap_or_default();

    // Only create NetworkAccessChecker if there are actual rules to enforce
    if !allowed_hosts.is_empty() || !allowed_ports.is_empty() {
        Some(NetworkAccessChecker::new(allowed_hosts, allowed_ports))
    } else {
        None
    }
}
