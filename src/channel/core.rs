// Core Channel implementation

use anyhow::{anyhow, Result};
use bytes::{Buf, BytesMut};
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
pub(crate) use crate::error::ChannelError;
use crate::tube_protocol::{Frame, ControlMessage, CloseConnectionReason, try_parse_frame};
use crate::models::{Conn, TunnelTimeouts, NetworkAccessChecker, StreamHalf, ConnectionStats, is_guacd_session, ConversationType};
use crate::runtime::get_runtime;
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::buffer_pool::{BufferPool, BufferPoolConfig};
use crate::tube_and_channel_helpers::parse_network_rules_from_settings;
use super::types::ActiveProtocol;
use tracing::{debug, error, info, trace, warn};
use serde_json::Value as JsonValue; // For clarity when matching JsonValue types
use serde::Deserialize; // Add this

// Import from sibling modules
use super::frame_handling::handle_incoming_frame;
use super::utils::handle_ping_timeout;

// Define the buffer thresholds
pub(crate) const BUFFER_LOW_THRESHOLD: u64 = 14 * 1024 * 1024; // 14MB for responsive UI

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
    pub(crate) conns: Arc<Mutex<HashMap<u32, Conn>>>,
    pub(crate) next_conn_no: u32,
    pub(crate) rx_from_dc: mpsc::UnboundedReceiver<Bytes>,
    pub(crate) channel_id: String,
    pub(crate) timeouts: TunnelTimeouts,
    pub(crate) network_checker: Option<NetworkAccessChecker>,
    pub(crate) ping_attempt: u32,
    pub(crate) round_trip_latency: Arc<Mutex<Vec<u64>>>,
    pub(crate) is_connected: bool,
    pub(crate) should_exit: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) server_mode: bool,
    // Server-related fields
    pub(crate) local_listen_addr: Option<String>,
    pub(crate) actual_listen_addr: Option<std::net::SocketAddr>,
    pub(crate) local_client_server: Option<Arc<TcpListener>>,
    pub(crate) local_client_server_task: Option<JoinHandle<()>>,
    pub(crate) local_client_server_conn_tx: Option<mpsc::Sender<(u32, OwnedWriteHalf, JoinHandle<()>)>>,
    pub(crate) local_client_server_conn_rx: Option<mpsc::Receiver<(u32, OwnedWriteHalf, JoinHandle<()>)>>,
    
    // Protocol handling integrated into Channel
    pub(crate) active_protocol: ActiveProtocol,
    pub(crate) protocol_state: ProtocolLogicState,

    // New fields for Guacd and ConnectAs specific settings
    pub(crate) guacd_host: Option<String>,
    pub(crate) guacd_port: Option<u16>,
    pub(crate) connect_as_settings: ConnectAsSettings,
    pub(crate) guacd_params: Arc<Mutex<HashMap<String, String>>>, // Kept for now for minimal diff

    // Pending message queues for each connection
    pub(crate) pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>>,
    // Buffer pool for efficient buffer management
    pub(crate) buffer_pool: BufferPool,
    // MPSC channel for frames from receive_message to be processed
    frame_input_tx: mpsc::UnboundedSender<Frame>,
    frame_input_rx: Arc<Mutex<mpsc::UnboundedReceiver<Frame>>>,
    // Trigger for processing pending messages when the buffer is low
    pub(crate) process_pending_trigger_rx: Arc<Mutex<mpsc::UnboundedReceiver<()>>>,
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

// Implement Clone for Channel
impl Clone for Channel {
    fn clone(&self) -> Self {
        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        let (_, pending_trigger_rx) = mpsc::unbounded_channel::<()>();
        // For cloned Channel instances, conn_closed_tx can be a new sender,
        // but conn_closed_rx should be None as run() consumes the original.
        let cloned_conn_closed_tx = mpsc::unbounded_channel::<(u32, String)>().0;
        
        Self {
            webrtc: self.webrtc.clone(),
            conns: Arc::clone(&self.conns),
            next_conn_no: self.next_conn_no,
            // Each clone gets a new rx_from_dc; this is problematic if the original is consumed.
            // Channel instances are typically Arc<Mutex<Channel>>, and cloning that Arc is preferred.
            // If direct Channel cloning is essential, rx_from_dc handling needs careful thought.
            rx_from_dc: mpsc::unbounded_channel().1, 
            channel_id: format!("{}_clone", self.channel_id),
            timeouts: self.timeouts.clone(),
            network_checker: self.network_checker.clone(),
            ping_attempt: self.ping_attempt,
            round_trip_latency: Arc::clone(&self.round_trip_latency),
            is_connected: self.is_connected,
            should_exit: Arc::clone(&self.should_exit),
            server_mode: self.server_mode,
            local_listen_addr: None,
            actual_listen_addr: None,
            local_client_server: None, // Cloned server instances usually don't re-listen on the same port.
            local_client_server_task: None,
            local_client_server_conn_tx: Some(server_conn_tx),
            local_client_server_conn_rx: Some(server_conn_rx),
            active_protocol: self.active_protocol, // Clone
            protocol_state: self.protocol_state.clone(), // Clone
            
            // Clone new fields
            guacd_host: self.guacd_host.clone(),
            guacd_port: self.guacd_port,
            connect_as_settings: self.connect_as_settings.clone(),
            guacd_params: Arc::clone(&self.guacd_params),

            pending_messages: Arc::clone(&self.pending_messages),
            buffer_pool: self.buffer_pool.clone(),
            frame_input_tx: self.frame_input_tx.clone(),
            frame_input_rx: Arc::clone(&self.frame_input_rx),
            process_pending_trigger_rx: Arc::new(Mutex::new(pending_trigger_rx)),
            channel_ping_sent_time: Mutex::new(None), // Initialize for clone
            conn_closed_tx: cloned_conn_closed_tx, // Cloned instance gets a new sender (though likely unused if the clone isn't run)
            conn_closed_rx: None, // Cloned instance does not get the original receiver
            primary_guacd_conn_no: Arc::new(Mutex::new(None)), // Cloned channels start fresh for primary Guacd conn
            callback_token: self.callback_token.clone(),
            ksm_config: self.ksm_config.clone(),
        }
    }
}

impl Channel {
    pub async fn new(
        webrtc: WebRTCDataChannel,
        rx_from_dc: mpsc::UnboundedReceiver<Bytes>, // This receiver is from the WebRTC data channel
        channel_id: String,
        timeouts: Option<TunnelTimeouts>,
        protocol_settings: HashMap<String, JsonValue>, // Using JsonValue alias
        server_mode: bool,
        callback_token: Option<String>,
        ksm_config: Option<String>,
    ) -> Result<Self> {
        info!(target: "channel_lifecycle", channel_id = %channel_id, server_mode, "Channel::new called");
        trace!(target: "channel_setup", channel_id = %channel_id, ?protocol_settings, "Initial protocol_settings received by Channel::new");

        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        let (f_input_tx, f_input_rx) = mpsc::unbounded_channel::<Frame>();
        let (pending_tx, pending_rx) = mpsc::unbounded_channel::<()>();
        let (conn_closed_tx, conn_closed_rx) = mpsc::unbounded_channel::<(u32, String)>();
        
        webrtc.set_buffered_amount_low_threshold(BUFFER_LOW_THRESHOLD);
        
        let buffer_pool_config = BufferPoolConfig {
            buffer_size: 32 * 1024,
            max_pooled: 64,
            resize_on_return: true,
        };
        let buffer_pool = BufferPool::new(buffer_pool_config);
        
        let pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>> = Arc::new(Mutex::new(HashMap::new()));
        
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
                            initial_protocol_state = ProtocolLogicState::Guacd(ChannelGuacdState::default());

                            if let Some(guacd_dedicated_settings_val) = protocol_settings.get("guacd") {
                                trace!(target: "channel_setup", channel_id = %channel_id, "Found 'guacd' block in protocol_settings: {:?}", guacd_dedicated_settings_val);
                                if let JsonValue::Object(guacd_map) = guacd_dedicated_settings_val {
                                    guacd_host_setting = guacd_map.get("guacd_host").and_then(|v| v.as_str()).map(String::from);
                                    guacd_port_setting = guacd_map.get("guacd_port").and_then(|v| v.as_u64()).map(|p| p as u16);
                                    info!(target: "channel_setup", channel_id = %channel_id, ?guacd_host_setting, ?guacd_port_setting, "Parsed from dedicated 'guacd' settings block.");
                                } else {
                                    warn!(target: "channel_setup", channel_id = %channel_id, "'guacd' block was not a JSON Object.");
                                }
                            } else {
                                 trace!(target: "channel_setup", channel_id = %channel_id, "No dedicated 'guacd' block found in protocol_settings. Guacd server host/port might come from guacd_params or defaults.");
                            }

                            if let Some(guacd_params_json_val) = protocol_settings.get("guacd_params") {
                                info!(target: "channel_setup", channel_id = %channel_id, "Found 'guacd_params' in protocol_settings.");
                                trace!(target: "channel_setup", channel_id = %channel_id, guacd_params_value = ?guacd_params_json_val, "Raw guacd_params value for direct processing.");

                                if let JsonValue::Object(map) = guacd_params_json_val {
                                    temp_initial_guacd_params_map = map.iter().filter_map(|(k, v)| {
                                        match v {
                                            JsonValue::String(s) => Some((k.clone(), s.clone())),
                                            JsonValue::Bool(b) => Some((k.clone(), b.to_string())),
                                            JsonValue::Number(n) => Some((k.clone(), n.to_string())),
                                            JsonValue::Array(arr) => {
                                                let str_arr: Vec<String> = arr.iter()
                                                    .filter_map(|val| val.as_str().map(String::from))
                                                    .collect();
                                                if !str_arr.is_empty() {
                                                    Some((k.clone(), str_arr.join(",")))
                                                } else {
                                                    // For arrays not of strings, or empty string arrays, produce empty string or skip.
                                                    // Guacamole usually expects comma-separated for multiple values like image/audio mimetypes.
                                                    // If it's an array of other things, stringifying the whole array might be an option.
                                                    Some((k.clone(), "".to_string())) // Or None to skip
                                                }
                                            }
                                            JsonValue::Null => None, // Omit null values by not adding them
                                            // For JsonValue::Object, stringify the nested object.
                                            // This matches the behavior if a struct field was Option<JsonValue> and then stringified.
                                            JsonValue::Object(obj_map) => serde_json::to_string(obj_map).ok().map(|s_val| (k.clone(), s_val)),
                                        }
                                    }).collect();
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
                                        initial_protocol_state = ProtocolLogicState::Socks5(ChannelSocks5State::default());
                                    } else {
                                        debug!(target:"channel_setup", channel_id = %channel_id, server_mode, "Configuring for PortForward protocol (Tunnel type)");
                                        determined_protocol = ActiveProtocol::PortForward;
                                        if server_mode {
                                            initial_protocol_state = ProtocolLogicState::PortForward(ChannelPortForwardState::default());
                                        } else {
                                            // Try to get the target host / port from either target_host/target_port or guacd field
                                            let mut dest_host = protocol_settings.get("target_host").and_then(|v| v.as_str()).map(String::from);
                                            let mut dest_port = protocol_settings.get("target_port")
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
                                            if dest_host.is_none() || dest_port.is_none() {
                                                if let Some(guacd_obj) = protocol_settings.get("guacd").and_then(|v| v.as_object()) {
                                                    if dest_host.is_none() {
                                                        dest_host = guacd_obj.get("guacd_host")
                                                            .and_then(|v| v.as_str())
                                                            .map(|s| s.trim().to_string()); // Trim whitespace
                                                    }
                                                    if dest_port.is_none() {
                                                        dest_port = guacd_obj.get("guacd_port")
                                                            .and_then(|v| v.as_u64())
                                                            .map(|p| p as u16);
                                                    }
                                                    debug!(target:"channel_setup", channel_id = %channel_id, 
                                                           "Extracted target from guacd field: host={:?}, port={:?}", 
                                                           dest_host, dest_port);
                                                }
                                            }
                                            
                                            initial_protocol_state = ProtocolLogicState::PortForward(ChannelPortForwardState {
                                                target_host: dest_host,
                                                target_port: dest_port,
                                            });
                                        }
                                    }
                                    if server_mode { // For PortForward server, we need a listen address
                                        local_listen_addr_setting = protocol_settings.get("local_listen_addr").and_then(|v| v.as_str()).map(String::from);
                                    }
                                }
                                _ => { // Other non-Guacd types
                                    if network_checker.is_some() {
                                        debug!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Configuring for SOCKS5 protocol (network rules present)");
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(ChannelSocks5State::default());
                                    } else {
                                        debug!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Configuring for PortForward protocol (defaulting)");
                                        determined_protocol = ActiveProtocol::PortForward;
                                        let mut dest_host = protocol_settings.get("target_host").and_then(|v| v.as_str()).map(String::from);
                                        let mut dest_port = protocol_settings.get("target_port").and_then(|v| v.as_u64()).map(|p| p as u16);
                                        
                                        // If not found, check the guacd field
                                        if dest_host.is_none() || dest_port.is_none() {
                                            if let Some(guacd_obj) = protocol_settings.get("guacd").and_then(|v| v.as_object()) {
                                                if dest_host.is_none() {
                                                    dest_host = guacd_obj.get("guacd_host")
                                                        .and_then(|v| v.as_str())
                                                        .map(|s| s.trim().to_string()); // Trim whitespace
                                                }
                                                if dest_port.is_none() {
                                                    dest_port = guacd_obj.get("guacd_port")
                                                        .and_then(|v| v.as_u64())
                                                        .map(|p| p as u16);
                                                }
                                                debug!(target:"channel_setup", channel_id = %channel_id, 
                                                       "Extracted target from guacd field (default case): host={:?}, port={:?}", 
                                                       dest_host, dest_port);
                                            }
                                        }
                                        
                                        initial_protocol_state = ProtocolLogicState::PortForward(ChannelPortForwardState {
                                            target_host: dest_host,
                                            target_port: dest_port,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                         error!(target:"channel_setup", channel_id = %channel_id, protocol_type = %protocol_name_str, "Invalid conversationType string. Erroring out.");
                         return Err(anyhow::anyhow!("Invalid conversationType string: {}", protocol_name_str));
                    }
                }
            } else { // protocol_name_val is not a string
                error!(target:"channel_setup", channel_id = %channel_id, "conversationType is not a string. Erroring out.");
                return Err(anyhow::anyhow!("conversationType is not a string"));
            }
        } else { // "conversationType" not found
            error!(target:"channel_setup", channel_id = %channel_id, "No specific protocol defined (conversationType missing). Erroring out.");
            return Err(anyhow::anyhow!("No specific protocol defined (conversationType missing)"));
        }

        let mut final_connect_as_settings = ConnectAsSettings::default();
        if let Some(connect_as_settings_val) = protocol_settings.get("connect_as_settings") {
            info!(target: "channel_setup", channel_id = %channel_id, "Found 'connect_as_settings' in protocol_settings.");
            trace!(target: "channel_setup", channel_id = %channel_id, cas_value = ?connect_as_settings_val, "Raw connect_as_settings value.");
            match serde_json::from_value::<ConnectAsSettings>(connect_as_settings_val.clone()) {
                Ok(parsed_settings) => {
                    final_connect_as_settings = parsed_settings;
                    info!(target: "channel_setup", channel_id = %channel_id, "Successfully deserialized connect_as_settings into ConnectAsSettings struct.");
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
        
        let channel_id_clone = channel_id.clone();
        let process_pending_trigger_tx_clone = pending_tx.clone();

        webrtc.on_buffered_amount_low(Some(Box::new(move || {
            let ep_name_cb = channel_id_clone.clone();
            let trigger_tx = process_pending_trigger_tx_clone.clone();
            tokio::spawn(async move {
               if tracing::enabled!(tracing::Level::DEBUG) {
                   debug!(target: "channel_flow", channel_id = %ep_name_cb, "Buffer amount is now below threshold, signaling to process pending messages");
               }
               if let Err(e) = trigger_tx.send(()) {
                    error!(target: "channel_flow", channel_id = %ep_name_cb, error = %e, "Failed to send pending process trigger");
               }
            });
        })));
        
        let new_channel = Self {
            webrtc,
            conns: Arc::new(Mutex::new(HashMap::new())),
            next_conn_no: 1,
            rx_from_dc,
            channel_id,
            timeouts: timeouts.unwrap_or_default(),
            network_checker,
            ping_attempt: 0,
            round_trip_latency: Arc::new(Mutex::new(Vec::new())),
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

            pending_messages,
            buffer_pool,
            frame_input_tx: f_input_tx,
            frame_input_rx: Arc::new(Mutex::new(f_input_rx)),
            process_pending_trigger_rx: Arc::new(Mutex::new(pending_rx)),
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
                            
                        // Create a connection
                        let conn = Conn {
                            backend: Box::new(stream_half),
                            to_webrtc: task,
                            stats: ConnectionStats::default(),
                        };
                            
                        // Store in our registry
                        self.conns.lock().await.insert(conn_no, conn);
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
                                
                                if chunk.len() > 0 {
                                    debug!(target: "channel_flow", channel_id = %self.channel_id, first_bytes = ?&chunk[..std::cmp::min(20, chunk.len())], "First few bytes of received data");
                                }
                            }
                            
                            buf.extend_from_slice(&chunk);
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!(target: "channel_flow", channel_id = %self.channel_id, buffer_size = buf.len(), "Buffer size after adding chunk");
                            }
                            
                            // Process pending messages might be triggered by buffer low, 
                            // but also good to try after receiving new data if not recently triggered.
                            self.process_pending_messages().await?;
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

                // Listen for trigger to process pending messages
                trigger = async {
                    let mut guard = self.process_pending_trigger_rx.lock().await;
                    guard.recv().await
                } => {
                    if trigger.is_some() {
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            debug!(target: "channel_flow", channel_id = %self.channel_id, "Trigger received to process pending messages.");
                        }
                        self.process_pending_messages().await?;
                    } else {
                        // Trigger channel closed, maybe log or break.
                        info!(target: "channel_lifecycle", channel_id = %self.channel_id, "Process pending trigger channel closed.");
                        // Depending on desired behavior, you might want to break the loop.
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
                                    error!(target: "channel_lifecycle", channel_id = %self.channel_id, error = %e, "Error explicitly closing control connection (0) during critical shutdown.");
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

        self.cleanup_all_connections().await?;
        Ok(())
    }

    // New method to process pending messages for all connections with better prioritization
    // **BOLD WARNING: HOT PATH - PROCESSES QUEUED MESSAGES**
    // **AVOID UNNECESSARY CLONES AND ALLOCATIONS**
    pub(crate) async fn process_pending_messages(&mut self) -> Result<()> {
        let mut pending_guard = self.pending_messages.lock().await;
        if pending_guard.iter().all(|(_, queue)| queue.is_empty()) {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "channel_flow", channel_id = %self.channel_id, "process_pending_messages: No messages in any queue.");
            }
            return Ok(());
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!(target: "channel_flow", channel_id = %self.channel_id, num_connections_with_pending = pending_guard.len(), "process_pending_messages: Starting.");
            
            for (conn_no, queue) in pending_guard.iter() {
                if !queue.is_empty() {
                    debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, queue_size = queue.len(), "process_pending_messages: Connection has pending messages.");
                }
            }
        }

        let max_to_process = 100;
        let mut total_processed = 0;
        let total_messages: usize = pending_guard.values().map(|q| q.len()).sum();
        if total_messages > 0 && tracing::enabled!(tracing::Level::DEBUG) {
           debug!(target: "channel_flow", channel_id = %self.channel_id, total_pending_messages = total_messages, "process_pending_messages: Total pending messages across all connections.");
        }
        let conn_nos: Vec<u32> = pending_guard.keys().filter(|&conn_no| !pending_guard[conn_no].is_empty()).copied().collect();
        let mut current_index = 0;
        let buffer_threshold = BUFFER_LOW_THRESHOLD;
        
        while total_processed < max_to_process && !conn_nos.is_empty() {
            if conn_nos.is_empty() { 
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "channel_flow", channel_id = %self.channel_id, "process_pending_messages: conn_nos became empty, breaking.");
                }
                break; 
            }
            let conn_index = current_index % conn_nos.len();
            let conn_no = conn_nos[conn_index];
            
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, "process_pending_messages: Attempting to process queue for connection.");
            }

            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                if queue.is_empty() { 
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, "process_pending_messages: Queue is now empty for connection, skipping.");
                    }
                    current_index += 1;
                    continue;
                }
                
                // **PERFORMANCE: Pop the message directly instead of cloning**
                let message_to_send = match queue.pop_front() {
                    Some(msg) => msg,
                    None => {
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, "process_pending_messages: Queue was empty after checking, skipping.");
                        }
                        current_index += 1;
                        continue;
                    }
                };
                
                let current_buffered_amount_before_send = self.webrtc.buffered_amount().await;
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, message_size = message_to_send.len(), buffered_amount_before_send = current_buffered_amount_before_send, "process_pending_messages: Sending message from queue.");
                }

                if let Err(e) = self.webrtc.send(message_to_send).await {
                    error!(target: "channel_flow", channel_id = %self.channel_id, conn_no, error = %e, "process_pending_messages: Failed to send queued message for connection. Breaking loop.");
                    break;
                }

                total_processed += 1;
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "channel_flow", channel_id = %self.channel_id, conn_no, total_processed_in_loop = total_processed, remaining_in_queue = queue.len(), "process_pending_messages: Successfully sent message, already popped from queue.");
                }
                
                if total_processed % 10 == 0 {
                    let current_buffer_after_batch = self.webrtc.buffered_amount().await;
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(target: "channel_flow", channel_id = %self.channel_id, total_processed_in_batch = total_processed, current_buffered_amount = current_buffer_after_batch, "process_pending_messages: Checking buffer threshold mid-batch.");
                    }
                    if current_buffer_after_batch >= buffer_threshold {
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            debug!(target: "channel_flow", channel_id = %self.channel_id, total_processed_in_batch = total_processed, "process_pending_messages: Buffer threshold reached during processing, pausing.");
                        }
                        break;
                    }

                }
            } else {
                 warn!(target: "channel_flow", channel_id = %self.channel_id, conn_no, "process_pending_messages: Connection not found in pending_messages during processing. This should not happen.");
            }
            current_index += 1;
        }
        
        if total_processed > 0 {
           if tracing::enabled!(tracing::Level::DEBUG) {
               debug!(target: "channel_flow", channel_id = %self.channel_id, processed_in_batch = total_processed, "process_pending_messages: Finished batch.");
           }
        } else {
           if tracing::enabled!(tracing::Level::DEBUG) {
               debug!(target: "channel_flow", channel_id = %self.channel_id, "process_pending_messages: No messages processed in this batch.");
           }
        }
        Ok(())
    }

    pub(crate) async fn cleanup_all_connections(&mut self) -> Result<()> {
        // Stop the server if it's running
        if self.server_mode && self.local_client_server_task.is_some() {
            if let Err(e) = self.stop_server().await {
                warn!(target: "channel_lifecycle", channel_id = %self.channel_id, error = %e, "Failed to stop server during cleanup");
            }
        }

        let conn_keys: Vec<u32> = self.conns.lock().await.keys().copied().collect();
        for conn_no in conn_keys {
            if conn_no != 0 {
                self.close_backend(conn_no, CloseConnectionReason::Normal).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn send_control_message(&mut self, message: ControlMessage, data: &[u8]) -> Result<()> {
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
                        debug!("Channel({}): Sent channel PING (conn_no=0), recorded send time.", self.channel_id);
                    }
                }
            } else if data.is_empty() { // Convention: empty data for Ping implies channel ping
                 let mut sent_time = self.channel_ping_sent_time.lock().await;
                 *sent_time = Some(crate::tube_protocol::now_ms());
                 if tracing::enabled!(tracing::Level::DEBUG) {
                     debug!("Channel({}): Sent channel PING (conn_no=0, empty payload convention), recorded send time.", self.channel_id);
                 }
            }
        }

        let buffered_amount = self.webrtc.buffered_amount().await;
        if buffered_amount >= BUFFER_LOW_THRESHOLD && tracing::enabled!(tracing::Level::DEBUG) {
           debug!(target: "channel_flow", channel_id = %self.channel_id, buffered_amount, ?message, "Control message buffer full, but sending control message anyway");
        }
        self.webrtc.send(encoded).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }
    // 
    // // Method to handle received message bytes from a data channel
    // pub async fn receive_message(&mut self, bytes: Bytes) -> Result<()> {
    //     let mut buffer = self.buffer_pool.acquire();
    //     buffer.clear();
    //     buffer.extend_from_slice(&bytes);
    //     
    //     while let Some(frame) = try_parse_frame(&mut buffer) {
    //         debug!(target: "channel_flow", channel_id = %self.channel_id, connection_no = frame.connection_no, payload_size = frame.payload.len(), "Processing frame from message");
    //         
    //         if let Err(e) = handle_incoming_frame(self, frame).await {
    //             error!(target: "channel_flow", channel_id = %self.channel_id, error = %e, "Error handling frame from message processing");
    //         }
    //     }
    //     
    //     self.buffer_pool.release(buffer);
    //     Ok(())
    // }

    pub(crate) async fn close_backend(&mut self, conn_no: u32, reason: CloseConnectionReason) -> Result<()> {
        info!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, ?reason, "Closing connection");

        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&conn_no.to_be_bytes());
        buffer.extend_from_slice(&(reason as u16).to_be_bytes());
        let msg_data = buffer.freeze();

        self.internal_handle_connection_close(conn_no, reason).await?;
        
        self.send_control_message(ControlMessage::CloseConnection, &msg_data).await?;

        // For control connections or explicit cleanup, remove immediately
        let should_delay_removal = conn_no != 0 && reason != CloseConnectionReason::Normal;

        if !should_delay_removal {
            // Immediate removal
            if let Some(mut conn) = self.conns.lock().await.remove(&conn_no) {
                let _ = crate::models::AsyncReadWrite::shutdown(&mut conn.backend).await;
                conn.to_webrtc.abort();
                match tokio::time::timeout(self.timeouts.close_connection, conn.to_webrtc).await {
                    Ok(_) => debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Successfully closed backend task for connection"),
                    Err(_) => warn!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, "Timeout waiting for backend task to close for connection"),
                }
            }

            // Correctly remove from pending_messages
            {
                let mut pending_guard = self.pending_messages.lock().await;
                pending_guard.remove(&conn_no);
            }
        } else {
            // Delayed removal - shutdown the connection but keep it in the map briefly
            {
                let mut conns_guard = self.conns.lock().await;
                if let Some(conn) = conns_guard.get_mut(&conn_no) {
                    // Shutdown the backend connection
                    let _ = crate::models::AsyncReadWrite::shutdown(&mut conn.backend).await;
                    // Abort the to_webrtc task
                    conn.to_webrtc.abort();
                }
            }

            // Schedule delayed cleanup
            let conns_arc = Arc::clone(&self.conns);
            let pending_messages_arc = Arc::clone(&self.pending_messages);
            let channel_id_clone = self.channel_id.clone();
            let close_timeout = self.timeouts.close_connection;
            
            // Spawn a task to remove the connection after a grace period
            tokio::spawn(async move {
                // Wait a bit to allow in-flight messages to be processed
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                
                debug!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, "Grace period elapsed, removing connection from maps");
                
                // Now remove from both maps
                if let Some(conn) = conns_arc.lock().await.remove(&conn_no) {
                    // Wait for the task to finish
                    match tokio::time::timeout(close_timeout, conn.to_webrtc).await {
                        Ok(_) => debug!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, "Successfully closed backend task after grace period"),
                        Err(_) => warn!(target: "connection_lifecycle", channel_id = %channel_id_clone, conn_no, "Timeout waiting for backend task after grace period"),
                    }
                }
                pending_messages_arc.lock().await.remove(&conn_no);
            });
        }

        if conn_no == 0 {
            self.should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) async fn internal_handle_connection_close(&mut self, conn_no: u32, reason: CloseConnectionReason) -> Result<()> {
        debug!(target: "connection_lifecycle", channel_id = %self.channel_id, conn_no, ?reason, "internal_handle_connection_close");
        match self.active_protocol {
            ActiveProtocol::Socks5 => {
                // TODO: SOCKS5 specific close logic
            }
            ActiveProtocol::Guacd => {
                // TODO: GuacD specific close logic
            }
            ActiveProtocol::PortForward => {
                // TODO: PortForward specific close logic (if any beyond TCP stream closure)
            }
        }
        // Common logic for all protocols after specific handling
        Ok(())
    }
}

// Ensure all resources are properly cleaned up
impl Drop for Channel {
    fn drop(&mut self) {
        self.should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(task) = &self.local_client_server_task {
            task.abort();
        }

        let runtime = get_runtime();
        let webrtc = self.webrtc.clone();
        let channel_id = self.channel_id.clone();
        let conns_clone = Arc::clone(&self.conns); // Clone Arc for use in the spawned task
        let buffer_pool_clone = self.buffer_pool.clone();

        runtime.spawn(async move {
            let conn_keys: Vec<u32> = conns_clone.lock().await.keys().copied().collect();
            for conn_no in conn_keys {
                if conn_no == 0 { continue; }
                let mut close_buffer = buffer_pool_clone.acquire();
                close_buffer.clear();
                close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                close_buffer.extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());
                
                let close_frame = Frame::new_control_with_buffer(ControlMessage::CloseConnection, &mut close_buffer);
                let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                if let Err(e) = webrtc.send(encoded).await {
                    warn!(target: "channel_cleanup", channel_id = %channel_id, conn_no, error = %e, "Error sending close frame in drop for connection");
                }
                buffer_pool_clone.release(close_buffer);
            }
            info!(target: "channel_lifecycle", channel_id = %channel_id, "Basic resource cleanup initiated in drop for Channel");
        });
    }
}
