// Core Channel implementation

use super::types::ActiveProtocol;
use crate::buffer_pool::{BufferPool, STANDARD_BUFFER_CONFIG};
pub(crate) use crate::error::ChannelError;
use crate::models::{
    is_guacd_session, Conn, ConversationType, NetworkAccessChecker, StreamHalf, TunnelTimeouts,
};
use crate::runtime::get_runtime;
use crate::trace_ultra_hot_path;
use crate::tube_and_channel_helpers::parse_network_rules_from_settings;
use crate::tube_protocol::{try_parse_frame, CloseConnectionReason, ControlMessage, Frame};
use crate::webrtc_data_channel::{WebRTCDataChannel, STANDARD_BUFFER_THRESHOLD};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use bytes::{Buf, BytesMut};
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::Value as JsonValue; // For clarity when matching JsonValue types
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace, warn};
// Add this

// Import from sibling modules
use super::frame_handling::handle_incoming_frame;
use super::utils::handle_ping_timeout;

// --- Protocol-specific state definitions ---
#[derive(Default, Clone, Debug)]
pub(crate) struct ChannelSocks5State {
    // SOCKS5 handshake and target address are handled directly in server.rs
    // without persistent state storage
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ChannelGuacdState {
    // Add GuacD specific fields, e.g., Guacamole client state, connected things
}

// Potentially, PortForward might also have a state if we need to store target addresses resolved from settings
#[derive(Debug, Default, Clone)]
pub(crate) struct ChannelPortForwardState {
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
}

#[derive(Clone, Debug)]
pub(crate) enum ProtocolLogicState {
    Socks5(ChannelSocks5State),
    Guacd(ChannelGuacdState),
    PortForward(ChannelPortForwardState),
}

impl Default for ProtocolLogicState {
    fn default() -> Self {
        ProtocolLogicState::PortForward(ChannelPortForwardState::default()) // Default to PortForward
    }
}
// --- End Protocol-specific state definitions ---

// --- ConnectAs Settings Definition ---
#[derive(Deserialize, Debug, Clone, Default)] // Added Deserialize
pub struct ConnectAsSettings {
    #[serde(alias = "allow_supply_user", default)]
    pub allow_supply_user: bool,
    #[serde(alias = "allow_supply_host", default)]
    pub allow_supply_host: bool,
    #[serde(alias = "gateway_private_key")]
    pub gateway_private_key: Option<String>,
}
// --- End ConnectAs Settings Definition ---

/// Channel instance. Owns the data‑channel and a map of active back‑end TCP streams.
pub struct Channel {
    pub(crate) webrtc: WebRTCDataChannel,
    pub(crate) conns: Arc<DashMap<u32, Conn>>,
    pub(crate) rx_from_dc: mpsc::UnboundedReceiver<Bytes>,
    pub(crate) channel_id: String,
    pub(crate) timeouts: TunnelTimeouts,
    pub(crate) network_checker: Option<NetworkAccessChecker>,
    pub(crate) ping_attempt: u32,
    pub(crate) is_connected: bool,
    pub(crate) should_exit: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) server_mode: bool,
    // Server-related fields
    pub(crate) local_listen_addr: Option<String>,
    pub(crate) actual_listen_addr: Option<std::net::SocketAddr>,
    pub(crate) local_client_server: Option<Arc<TcpListener>>,
    pub(crate) local_client_server_task: Option<JoinHandle<()>>,
    pub(crate) local_client_server_conn_tx:
        Option<mpsc::Sender<(u32, OwnedWriteHalf, JoinHandle<()>)>>,
    pub(crate) local_client_server_conn_rx:
        Option<mpsc::Receiver<(u32, OwnedWriteHalf, JoinHandle<()>)>>,

    // Protocol handling integrated into Channel
    pub(crate) active_protocol: ActiveProtocol,
    pub(crate) protocol_state: ProtocolLogicState,

    // New fields for Guacd and ConnectAs specific settings
    pub(crate) guacd_host: Option<String>,
    pub(crate) guacd_port: Option<u16>,
    pub(crate) connect_as_settings: ConnectAsSettings,
    pub(crate) guacd_params: Arc<Mutex<HashMap<String, String>>>, // Kept for now for minimal diff

    // Buffer pool for efficient buffer management
    pub(crate) buffer_pool: BufferPool,
    // UDP associations for SOCKS5 UDP ASSOCIATE response handling
    pub(crate) udp_associations: super::udp::UdpAssociations,
    // Reverse index: conn_no -> set of destination addresses for efficient cleanup
    pub(crate) udp_conn_index:
        Arc<std::sync::Mutex<HashMap<u32, std::collections::HashSet<std::net::SocketAddr>>>>,
    // Timestamp for the last channel-level ping sent (conn_no=0)
    pub(crate) channel_ping_sent_time: Mutex<Option<u64>>,

    // For signaling connection task closures to the main Channel run loop
    pub(crate) conn_closed_tx: mpsc::UnboundedSender<(u32, String)>, // (conn_no, channel_id)
    conn_closed_rx: Option<mpsc::UnboundedReceiver<(u32, String)>>,
    // Stores the conn_no of the primary Guacd data connection
    pub(crate) primary_guacd_conn_no: Arc<Mutex<Option<u32>>>,
    // Callback token for router communication
    pub(crate) callback_token: Option<String>,
    // KSM config for router communication
    pub(crate) ksm_config: Option<String>,
}

// NOTE: Channel is intentionally NOT Clone because it contains a single-consumer receiver
// (rx_from_dc) that can only be owned by one instance. Cloning would create a broken
// receiver that never receives messages. Use Arc<Channel> for sharing instead.

pub struct ChannelParams {
    pub webrtc: WebRTCDataChannel,
    pub rx_from_dc: mpsc::UnboundedReceiver<Bytes>,
    pub channel_id: String,
    pub timeouts: Option<TunnelTimeouts>,
    pub protocol_settings: HashMap<String, JsonValue>,
    pub server_mode: bool,
    pub callback_token: Option<String>,
    pub ksm_config: Option<String>,
}

impl Channel {
    pub async fn new(params: ChannelParams) -> Result<Self> {
        let ChannelParams {
            webrtc,
            rx_from_dc,
            channel_id,
            timeouts,
            protocol_settings,
            server_mode,
            callback_token,
            ksm_config,
        } = params;
        debug!(target: "channel_lifecycle", channel_id = %channel_id, server_mode, "Channel::new called");
        trace!(target: "channel_setup", channel_id = %channel_id, ?protocol_settings, "Initial protocol_settings received by Channel::new");

        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        let (conn_closed_tx, conn_closed_rx) = mpsc::unbounded_channel::<(u32, String)>();

        // Use standard buffer pool configuration for consistent performance
        let buffer_pool = BufferPool::new(STANDARD_BUFFER_CONFIG);

        let network_checker = parse_network_rules_from_settings(&protocol_settings);

        let determined_protocol; // Declare without initial assignment
        let initial_protocol_state; // Declare without initial assignment

        let mut guacd_host_setting: Option<String> = None;
        let mut guacd_port_setting: Option<u16> = None;
        let mut temp_initial_guacd_params_map = HashMap::new();

        let mut local_listen_addr_setting: Option<String> = None;

        if let Some(protocol_name_val) = protocol_settings.get("conversationType") {
            if let Some(protocol_name_str) = protocol_name_val.as_str() {
                match protocol_name_str.parse::<ConversationType>() {
                    Ok(parsed_conversation_type) => {
                        if is_guacd_session(&parsed_conversation_type) {
                            debug!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Configuring for GuacD protocol");
                            determined_protocol = ActiveProtocol::Guacd;
                            initial_protocol_state =
                                ProtocolLogicState::Guacd(ChannelGuacdState::default());

                            if let Some(guacd_dedicated_settings_val) =
                                protocol_settings.get("guacd")
                            {
                                trace!(target: "channel_setup", channel_id = %channel_id, "Found 'guacd' block in protocol_settings: {:?}", guacd_dedicated_settings_val);
                                if let JsonValue::Object(guacd_map) = guacd_dedicated_settings_val {
                                    guacd_host_setting = guacd_map
                                        .get("guacd_host")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);
                                    guacd_port_setting = guacd_map
                                        .get("guacd_port")
                                        .and_then(|v| v.as_u64())
                                        .map(|p| p as u16);
                                    debug!(target: "channel_setup", channel_id = %channel_id, ?guacd_host_setting, ?guacd_port_setting, "Parsed from dedicated 'guacd' settings block.");
                                } else {
                                    warn!(target: "channel_setup", channel_id = %channel_id, "'guacd' block was not a JSON Object.");
                                }
                            } else {
                                trace!(target: "channel_setup", channel_id = %channel_id, "No dedicated 'guacd' block found in protocol_settings. Guacd server host/port might come from guacd_params or defaults.");
                            }

                            if let Some(guacd_params_json_val) =
                                protocol_settings.get("guacd_params")
                            {
                                debug!(target: "channel_setup", channel_id = %channel_id, "Found 'guacd_params' in protocol_settings.");
                                trace!(target: "channel_setup", channel_id = %channel_id, guacd_params_value = ?guacd_params_json_val, "Raw guacd_params value for direct processing.");

                                if let JsonValue::Object(map) = guacd_params_json_val {
                                    temp_initial_guacd_params_map = map
                                        .iter()
                                        .filter_map(|(k, v)| {
                                            match v {
                                                JsonValue::String(s) => {
                                                    Some((k.clone(), s.clone()))
                                                }
                                                JsonValue::Bool(b) => {
                                                    Some((k.clone(), b.to_string()))
                                                }
                                                JsonValue::Number(n) => {
                                                    Some((k.clone(), n.to_string()))
                                                }
                                                JsonValue::Array(arr) => {
                                                    let str_arr: Vec<String> = arr
                                                        .iter()
                                                        .filter_map(|val| {
                                                            val.as_str().map(String::from)
                                                        })
                                                        .collect();
                                                    if !str_arr.is_empty() {
                                                        Some((k.clone(), str_arr.join(",")))
                                                    } else {
                                                        // For arrays not of strings, or empty string arrays, produce empty string or skip.
                                                        // Guacamole usually expects comma-separated for multiple values like image/audio mimetypes.
                                                        // If it's an array of other things, stringifying the whole array might be an option.
                                                        Some((k.clone(), "".to_string()))
                                                        // Or None to skip
                                                    }
                                                }
                                                JsonValue::Null => None, // Omit null values by not adding them
                                                // For JsonValue::Object, stringify the nested object.
                                                // This matches the behavior if a struct field was Option<JsonValue> and then stringified.
                                                JsonValue::Object(obj_map) => {
                                                    serde_json::to_string(obj_map)
                                                        .ok()
                                                        .map(|s_val| (k.clone(), s_val))
                                                }
                                            }
                                        })
                                        .collect();
                                    debug!(target: "channel_setup", channel_id = %channel_id, ?temp_initial_guacd_params_map, "Populated guacd_params map directly from JSON Value.");
                                } else {
                                    error!(target: "channel_setup", channel_id = %channel_id, "guacd_params was not a JSON object. Value: {:?}", guacd_params_json_val);
                                }
                            } else {
                                warn!(target: "channel_setup", channel_id = %channel_id, "'guacd_params' key not found in protocol_settings.");
                            }
                        } else {
                            // Handle non-Guacd types like Tunnel or SOCKS5 if network rules are present
                            match parsed_conversation_type {
                                ConversationType::Tunnel => {
                                    if network_checker.is_some() && server_mode {
                                        debug!(target:"channel_setup", channel_id = %channel_id, "Configuring for SOCKS5 protocol (Tunnel type with network rules)");
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(
                                            ChannelSocks5State::default(),
                                        );
                                    } else {
                                        debug!(target:"channel_setup", channel_id = %channel_id, server_mode, "Configuring for PortForward protocol (Tunnel type)");
                                        determined_protocol = ActiveProtocol::PortForward;
                                        if server_mode {
                                            initial_protocol_state =
                                                ProtocolLogicState::PortForward(
                                                    ChannelPortForwardState::default(),
                                                );
                                        } else {
                                            // Try to get the target host / port from either target_host/target_port or guacd field
                                            let mut dest_host = protocol_settings
                                                .get("target_host")
                                                .and_then(|v| v.as_str())
                                                .map(String::from);
                                            let mut dest_port = protocol_settings
                                                .get("target_port")
                                                .and_then(|v| {
                                                    // First, try to get it as an u64 directly
                                                    if let Some(num) = v.as_u64() {
                                                        Some(num as u16)
                                                    }
                                                    // If that fails, try to get it as a string and parse
                                                    else if let Some(s) = v.as_str() {
                                                        s.parse::<u16>().ok()
                                                    }
                                                    // If both approaches fail, return None
                                                    else {
                                                        None
                                                    }
                                                });

                                            // If not found, check the guacd field for tunnel connections
                                            (dest_host, dest_port) =
                                                Self::extract_host_port_from_guacd(
                                                    &protocol_settings,
                                                    dest_host,
                                                    dest_port,
                                                    &channel_id,
                                                    "tunnel connections",
                                                );

                                            initial_protocol_state =
                                                ProtocolLogicState::PortForward(
                                                    ChannelPortForwardState {
                                                        target_host: dest_host,
                                                        target_port: dest_port,
                                                    },
                                                );
                                        }
                                    }
                                    if server_mode {
                                        // For PortForward server, we need a listen address
                                        local_listen_addr_setting = protocol_settings
                                            .get("local_listen_addr")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);
                                    }
                                }
                                _ => {
                                    // Other non-Guacd types
                                    if network_checker.is_some() {
                                        debug!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Configuring for SOCKS5 protocol (network rules present)");
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(
                                            ChannelSocks5State::default(),
                                        );
                                    } else {
                                        debug!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Configuring for PortForward protocol (defaulting)");
                                        determined_protocol = ActiveProtocol::PortForward;
                                        let mut dest_host = protocol_settings
                                            .get("target_host")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);
                                        let mut dest_port = protocol_settings
                                            .get("target_port")
                                            .and_then(|v| v.as_u64())
                                            .map(|p| p as u16);

                                        // If not found, check the guacd field
                                        (dest_host, dest_port) = Self::extract_host_port_from_guacd(
                                            &protocol_settings,
                                            dest_host,
                                            dest_port,
                                            &channel_id,
                                            "default case",
                                        );

                                        initial_protocol_state = ProtocolLogicState::PortForward(
                                            ChannelPortForwardState {
                                                target_host: dest_host,
                                                target_port: dest_port,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        error!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Invalid conversationType string. Erroring out.");
                        return Err(anyhow::anyhow!(
                            "Invalid conversationType string: {}",
                            protocol_name_str
                        ));
                    }
                }
            } else {
                // protocol_name_val is not a string
                error!(target:"channel_setup", channel_id = %channel_id, "conversationType is not a string. Erroring out.");
                return Err(anyhow::anyhow!("conversationType is not a string"));
            }
        } else {
            // "conversationType" not found
            error!(target:"channel_setup", channel_id = %channel_id, "No specific protocol defined (conversationType missing). Erroring out.");
            return Err(anyhow::anyhow!(
                "No specific protocol defined (conversationType missing)"
            ));
        }

        let mut final_connect_as_settings = ConnectAsSettings::default();
        if let Some(connect_as_settings_val) = protocol_settings.get("connect_as_settings") {
            debug!(target: "channel_setup", channel_id = %channel_id, "Found 'connect_as_settings' in protocol_settings.");
            trace!(target: "channel_setup", channel_id = %channel_id, cas_value = ?connect_as_settings_val, "Raw connect_as_settings value.");
            match serde_json::from_value::<ConnectAsSettings>(connect_as_settings_val.clone()) {
                Ok(parsed_settings) => {
                    final_connect_as_settings = parsed_settings;
                    debug!(target: "channel_setup", channel_id = %channel_id, "Successfully deserialized connect_as_settings into ConnectAsSettings struct.");
                    trace!(target: "channel_setup", channel_id = %channel_id, ?final_connect_as_settings);
                }
                Err(e) => {
                    error!(target: "channel_setup", channel_id = %channel_id, "CRITICAL: Failed to deserialize connect_as_settings: {}. Value was: {:?}", e, connect_as_settings_val);
                    // Returning an error here if connect_as_settings are vital
                    return Err(anyhow!("Failed to deserialize connect_as_settings: {}", e));
                }
            }
        } else {
            warn!(target: "channel_setup", channel_id = %channel_id, "'connect_as_settings' key not found in protocol_settings. Using default.");
        }

        let new_channel = Self {
            webrtc,
            conns: Arc::new(DashMap::new()),
            rx_from_dc,
            channel_id,
            timeouts: timeouts.unwrap_or_default(),
            network_checker,
            ping_attempt: 0,
            is_connected: true,
            should_exit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            server_mode,
            local_listen_addr: local_listen_addr_setting,
            actual_listen_addr: None,
            local_client_server: None,
            local_client_server_task: None,
            local_client_server_conn_tx: Some(server_conn_tx),
            local_client_server_conn_rx: Some(server_conn_rx),
            active_protocol: determined_protocol,
            protocol_state: initial_protocol_state,

            guacd_host: guacd_host_setting,
            guacd_port: guacd_port_setting,
            connect_as_settings: final_connect_as_settings,
            guacd_params: Arc::new(Mutex::new(temp_initial_guacd_params_map)),

            buffer_pool,
            udp_associations: Arc::new(Mutex::new(HashMap::new())),
            udp_conn_index: Arc::new(std::sync::Mutex::new(HashMap::new())),
            channel_ping_sent_time: Mutex::new(None),
            conn_closed_tx,
            conn_closed_rx: Some(conn_closed_rx),
            primary_guacd_conn_no: Arc::new(Mutex::new(None)),
            callback_token,
            ksm_config,
        };

        info!(target: "channel_lifecycle", channel_id = %new_channel.channel_id, server_mode = new_channel.server_mode, "Channel initialized");

        Ok(new_channel)
    }

    pub async fn run(mut self) -> Result<(), ChannelError> {
        self.setup_webrtc_state_monitoring();

        let mut buf = BytesMut::with_capacity(64 * 1024);

        // Take the receiver channel for server connections
        let mut server_conn_rx = self.local_client_server_conn_rx.take();

        // Take ownership of conn_closed_rx for the select loop
        let mut local_conn_closed_rx = self.conn_closed_rx.take().ok_or_else(|| {
            error!(target: "channel_lifecycle", channel_id = %self.channel_id, "conn_closed_rx was already taken or None. Channel cannot monitor connection closures.");
            ChannelError::Internal("conn_closed_rx missing at start of run".to_string())
        })?;

        // Main processing loop - reads from WebRTC and dispatches frames
        while !self.should_exit.load(std::sync::atomic::Ordering::Relaxed) {
            // Process any complete frames in the buffer
            while let Some(frame) = try_parse_frame(&mut buf) {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "channel_flow", channel_id = %self.channel_id, connection_no = frame.connection_no, payload_size = frame.payload.len(), "Received frame from WebRTC");
                }

                if let Err(e) = handle_incoming_frame(&mut self, frame).await {
                    error!(target: "channel_flow", channel_id = %self.channel_id, error = %e, "Error handling frame");
                }
            }

            tokio::select! {
                // Check for any new connections from the server
                // Use biasedly selects to prioritize existing connections over new ones if server_conn_rx is Some
                biased;
                maybe_conn = async { server_conn_rx.as_mut()?.recv().await }, if server_conn_rx.is_some() => {
                    if let Some((conn_no, writer, task)) = maybe_conn {
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Registering connection from server");
                        }

                        // Create a stream half
                        let stream_half = StreamHalf {
                            reader: None,
                            writer,
                        };

                        // Create a lock-free connection with a dedicated backend task
                        let conn = Conn::new_with_backend(
                            Box::new(stream_half),
                            task,
                            conn_no,
                            self.channel_id.clone(),
                        ).await;

                        // Store in our lock-free registry
                        self.conns.insert(conn_no, conn);
                    } else {
                        // server_conn_rx was dropped or closed
                        server_conn_rx = None; // Prevent further polling of this arm
                    }
                }

                // Wait for more data from WebRTC
                maybe_chunk = self.rx_from_dc.recv() => {
                    match tokio::time::timeout(self.timeouts.read, async { maybe_chunk }).await { // Wrap future for timeout
                        Ok(Some(chunk)) => {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!(target: "channel_flow", channel_id = %self.channel_id, bytes_received = chunk.len(), "Received data from WebRTC");

                                if !chunk.is_empty() {
                                    trace_ultra_hot_path!(target: "channel_flow", channel_id = %self.channel_id, first_bytes = ?&chunk[..std::cmp::min(20, chunk.len())], "First few bytes of received data");
                                }
                            }

                            buf.extend_from_slice(&chunk);
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!(target: "channel_flow", channel_id = %self.channel_id, buffer_size = buf.len(), "Buffer size after adding chunk");
                            }

                            // Process pending messages might be triggered by buffer low,
                            // but also good to try after receiving new data if not recently triggered.
                        }
                        Ok(None) => {
                            info!(target: "channel_lifecycle", channel_id = %self.channel_id, "WebRTC data channel closed or sender dropped.");
                            break;
                        }
                        Err(_) => { // Timeout on rx_from_dc.recv()
                            handle_ping_timeout(&mut self).await?;
                        }
                    }
                }

                // Listen for connection closure signals
                maybe_closed_conn_info = local_conn_closed_rx.recv() => {
                    if let Some((closed_conn_no, closed_channel_id)) = maybe_closed_conn_info {
                        info!(target: "connection_lifecycle", channel_id = %closed_channel_id, conn_no = closed_conn_no, "Connection task reported exit to Channel run loop.");

                        let mut is_critical_closure = false;
                        if self.active_protocol == ActiveProtocol::Guacd {
                            let primary_opt = self.primary_guacd_conn_no.lock().await;
                            if let Some(primary_conn_no) = *primary_opt {
                                if primary_conn_no == closed_conn_no {
                                    warn!(target: "channel_lifecycle", channel_id = %self.channel_id, conn_no = closed_conn_no, "Critical Guacd data connection has closed. Initiating channel shutdown.");
                                    is_critical_closure = true;
                                }
                            }
                        }

                        if is_critical_closure {
                            self.should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
                            // Attempt to gracefully close the control connection (conn_no 0) as well, if not already closing.
                            // This sends a CloseConnection message to the client for the channel itself.
                            if closed_conn_no != 0 { // Avoid self-triggering if conn_no 0 was what closed to signal this.
                                info!(target: "channel_lifecycle", channel_id = %self.channel_id, "Shutting down control connection (0) due to critical upstream closure.");
                                if let Err(e) = self.close_backend(0, CloseConnectionReason::UpstreamClosed).await {
                                    debug!(target: "channel_lifecycle", channel_id = %self.channel_id, error = %e, "Error explicitly closing control connection (0) during critical shutdown.");
                                }
                            }
                            // Instead of just breaking, return the specific error to indicate why the channel is stopping.
                            // The main loop will break due to should_exit, but this provides the reason to the caller of run().
                            // However, the run loop continues until should_exit is polled again.
                            // For immediate exit and propagation: directly return.
                            return Err(ChannelError::CriticalUpstreamClosed(self.channel_id.clone()));
                        }
                        // Optional: Remove from self.conns and self.pending_messages if desired immediately.
                        // However, cleanup_all_connections will handle it upon loop exit.

                    } else {
                        // Conn_closed_tx was dropped, meaning all senders are gone.
                        // This might happen if the channel is already shutting down and tasks are aborting.
                        info!(target: "channel_lifecycle", channel_id = %self.channel_id, "Connection closure signal channel (conn_closed_rx) closed.");
                        // If this is unexpected, it might warrant setting should_exit to true.
                    }
                }
            }
        }

        // Log final stats before cleanup
        self.log_final_stats().await;

        self.cleanup_all_connections().await?;
        Ok(())
    }

    pub(crate) async fn cleanup_all_connections(&mut self) -> Result<()> {
        // Stop the server if it's running
        if self.server_mode && self.local_client_server_task.is_some() {
            if let Err(e) = self.stop_server().await {
                warn!(target: "channel_lifecycle", channel_id = %self.channel_id, error = %e, "Failed to stop server during cleanup");
            }
        }

        // Collect connection numbers from DashMap
        let conn_keys = self.get_connection_ids();
        for conn_no in conn_keys {
            if conn_no != 0 {
                self.close_backend(conn_no, CloseConnectionReason::Normal)
                    .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn send_control_message(
        &mut self,
        message: ControlMessage,
        data: &[u8],
    ) -> Result<()> {
        let frame = Frame::new_control_with_pool(message, data, &self.buffer_pool);
        let encoded = frame.encode_with_pool(&self.buffer_pool);

        if message == ControlMessage::Ping {
            // Check if this ping is for conn_no 0 (channel ping)
            // The `data` for a Ping should contain the conn_no it's for.
            // Assuming the first 4 bytes of Ping data payload is the conn_no.
            if data.len() >= 4 {
                let ping_conn_no = (&data[0..4]).get_u32();
                if ping_conn_no == 0 {
                    let mut sent_time = self.channel_ping_sent_time.lock().await;
                    *sent_time = Some(crate::tube_protocol::now_ms());
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(
                            "Channel({}): Sent channel PING (conn_no=0), recorded send time.",
                            self.channel_id
                        );
                    }
                }
            } else if data.is_empty() {
                // Convention: empty data for Ping implies channel ping
                let mut sent_time = self.channel_ping_sent_time.lock().await;
                *sent_time = Some(crate::tube_protocol::now_ms());
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!("Channel({}): Sent channel PING (conn_no=0, empty payload convention), recorded send time.", self.channel_id);
                }
            }
        }

        let buffered_amount = self.webrtc.buffered_amount().await;
        if buffered_amount >= STANDARD_BUFFER_THRESHOLD && tracing::enabled!(tracing::Level::DEBUG)
        {
            debug!(target: "channel_flow", channel_id = %self.channel_id, buffered_amount, ?message, "Control message buffer full, but sending control message anyway");
        }
        self.webrtc
            .send(encoded)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }

    pub(crate) async fn close_backend(
        &mut self,
        conn_no: u32,
        reason: CloseConnectionReason,
    ) -> Result<()> {
        let total_connections = self.conns.len();
        let remaining_connections = self.get_connection_ids_except(conn_no);

        debug!(target: "connection_lifecycle",
              channel_id = %self.channel_id, conn_no, ?reason,
              total_connections, ?remaining_connections,
              "Closing connection - Connection summary");

        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&conn_no.to_be_bytes());
        buffer.extend_from_slice(&(reason as u16).to_be_bytes());
        let msg_data = buffer.freeze();

        self.internal_handle_connection_close(conn_no, reason)
            .await?;

        self.send_control_message(ControlMessage::CloseConnection, &msg_data)
            .await?;

        // For control connections or explicit cleanup, remove immediately
        let should_delay_removal = conn_no != 0 && reason != CloseConnectionReason::Normal;

        if !should_delay_removal {
            // Immediate removal using DashMap
            if let Some((_, conn)) = self.conns.remove(&conn_no) {
                // Shutdown the connection gracefully (closes channels and waits for tasks)
                if let Err(e) = conn.shutdown().await {
                    warn!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, error = %e, "Error during connection shutdown");
                } else {
                    debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Successfully closed connection and tasks");
                }
            }
        } else {
            // Delayed removal - signal shutdown but keep in map briefly for pending messages
            if let Some(conn_ref) = self.conns.get(&conn_no) {
                // Signal the connection to close its data channel
                // (dropping the sender will cause the backend task to exit)
                if !conn_ref.data_tx.is_closed() {
                    debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Signaling connection to close data channel");
                }
            }

            // Schedule delayed cleanup
            let conns_arc = Arc::clone(&self.conns);
            let channel_id_clone = self.channel_id.clone();

            // Spawn a task to remove the connection after a grace period
            tokio::spawn(async move {
                // Wait a bit to allow in-flight messages to be processed
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                debug!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, "Grace period elapsed, removing connection from maps");

                // Now remove from maps
                if let Some((_, conn)) = conns_arc.remove(&conn_no) {
                    // Shutdown the connection gracefully
                    if let Err(e) = conn.shutdown().await {
                        warn!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, error = %e, "Error during delayed connection shutdown");
                    } else {
                        debug!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, "Successfully closed connection after grace period");
                    }
                }
            });
        }

        if conn_no == 0 {
            self.should_exit
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) async fn internal_handle_connection_close(
        &mut self,
        conn_no: u32,
        reason: CloseConnectionReason,
    ) -> Result<()> {
        debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, ?reason, "internal_handle_connection_close");

        // If this is the control connection (conn_no 0) or we're shutting down due to an error,
        // and we're in server mode, stop the server to prevent new connections
        if self.server_mode
            && (conn_no == 0
                || matches!(
                    reason,
                    CloseConnectionReason::UpstreamClosed | CloseConnectionReason::Error
                ))
            && self.local_client_server_task.is_some()
        {
            debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Stopping server due to critical connection closure");
            if let Err(e) = self.stop_server().await {
                warn!(target: "connection_lifecycle", channel_id = %self.channel_id, error = %e, "Failed to stop server during connection close");
            }
        }

        match self.active_protocol {
            ActiveProtocol::Socks5 => {
                // SOCKS5 connections are stateless after handshake, no special cleanup needed
            }
            ActiveProtocol::Guacd => {
                // Check if this was the primary data connection
                if let Some(primary_conn_no) = *self.primary_guacd_conn_no.lock().await {
                    if primary_conn_no == conn_no {
                        debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Primary GuacD data connection closed, clearing reference");
                        *self.primary_guacd_conn_no.lock().await = None;
                    }
                }
            }
            ActiveProtocol::PortForward => {
                // Port forwarding connections are just TCP streams, no special cleanup needed
            }
        }

        Ok(())
    }

    /// Get a list of all active connection IDs
    pub(crate) fn get_connection_ids(&self) -> Vec<u32> {
        Self::extract_connection_ids(&self.conns)
    }

    /// Get a list of all active connection IDs except the specified one
    pub(crate) fn get_connection_ids_except(&self, exclude_conn_no: u32) -> Vec<u32> {
        self.conns
            .iter()
            .map(|entry| *entry.key())
            .filter(|&id| id != exclude_conn_no)
            .collect()
    }

    /// Static helper to extract connection IDs from any DashMap reference
    fn extract_connection_ids(conns: &DashMap<u32, Conn>) -> Vec<u32> {
        conns.iter().map(|entry| *entry.key()).collect()
    }

    /// Helper to extract host/port from guacd settings if not already set
    fn extract_host_port_from_guacd(
        protocol_settings: &HashMap<String, JsonValue>,
        mut dest_host: Option<String>,
        mut dest_port: Option<u16>,
        channel_id: &str,
        context: &str,
    ) -> (Option<String>, Option<u16>) {
        if dest_host.is_none() || dest_port.is_none() {
            if let Some(guacd_obj) = protocol_settings.get("guacd").and_then(|v| v.as_object()) {
                if dest_host.is_none() {
                    dest_host = guacd_obj
                        .get("guacd_host")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()); // Trim whitespace
                }
                if dest_port.is_none() {
                    dest_port = guacd_obj
                        .get("guacd_port")
                        .and_then(|v| v.as_u64())
                        .map(|p| p as u16);
                }
                debug!(target:"channel_setup", channel_id = %channel_id,
                       "Extracted target from guacd field ({}): host={:?}, port={:?}",
                       context, dest_host, dest_port);
            }
        }
        (dest_host, dest_port)
    }

    /// Log comprehensive WebRTC statistics when a channel closes
    pub async fn log_final_stats(&mut self) {
        // Log comprehensive connection summary on channel close
        let total_connections = self.conns.len();
        let connection_ids = self.get_connection_ids();
        let buffered_amount = self.webrtc.buffered_amount().await;

        info!(target: "channel_summary",
              channel_id=%self.channel_id, total_connections, ?connection_ids,
              server_mode=self.server_mode, buffered_amount,
              ?self.active_protocol,
              "Channel '{}' closing - Final stats: {} connections: {:?}, {} bytes buffered",
              self.channel_id, total_connections, connection_ids, buffered_amount);

        // Note: Full WebRTC native stats (bytes sent/received, round-trip time,
        // packet loss, bandwidth usage, connection quality, etc.) are available
        // via peer_connection.get_stats() API in browser context.
        // These provide much more detailed metrics than our previous custom tracking.
    }
}

// Ensure all resources are properly cleaned up
impl Drop for Channel {
    fn drop(&mut self) {
        self.should_exit
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(task) = &self.local_client_server_task {
            task.abort();
        }

        let runtime = get_runtime();
        let webrtc = self.webrtc.clone();
        let channel_id = self.channel_id.clone();
        let conns_clone = Arc::clone(&self.conns); // Clone Arc for use in the spawned task
        let buffer_pool_clone = self.buffer_pool.clone();

        runtime.spawn(async move {
            // Collect connection numbers from DashMap
            let conn_keys = Self::extract_connection_ids(&conns_clone);
            for conn_no in conn_keys {
                if conn_no == 0 { continue; }

                // Send close frame to remote peer
                let mut close_buffer = buffer_pool_clone.acquire();
                close_buffer.clear();
                close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                close_buffer.extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());

                let close_frame = Frame::new_control_with_buffer(ControlMessage::CloseConnection, &mut close_buffer);
                let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                if let Err(e) = webrtc.send(encoded).await {
                    if !e.contains("Channel is closing") {
                        warn!(target: "channel_cleanup", channel_id = %channel_id, conn_no, error = %e, "Error sending close frame in drop for connection");
                    }
                }
                buffer_pool_clone.release(close_buffer);

                // Shutdown the connection gracefully
                if let Some((_, conn)) = conns_clone.remove(&conn_no) {
                    if let Err(e) = conn.shutdown().await {
                        debug!(target: "channel_cleanup", channel_id = %channel_id, conn_no, error = %e, "Error shutting down connection in drop");
                    }
                }
            }
            info!(target: "channel_lifecycle", channel_id = %channel_id, "Channel cleanup completed");
        });
    }
}
