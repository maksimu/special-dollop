// Core Channel implementation

use super::types::ActiveProtocol;
use crate::buffer_pool::{BufferPool, STANDARD_BUFFER_CONFIG};
pub(crate) use crate::error::ChannelError;
use crate::models::{
    is_guacd_session, Conn, ConversationType, NetworkAccessChecker, StreamHalf, TunnelTimeouts,
};
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::parse_network_rules_from_settings;
use crate::tube_protocol::{try_parse_frame, CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely;
use crate::webrtc_data_channel::{WebRTCDataChannel, STANDARD_BUFFER_THRESHOLD};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use bytes::{Buf, BufMut, BytesMut};
use dashmap::DashMap;
use log::{debug, error, info, warn};
use serde::Deserialize;
use serde_json::Value as JsonValue; // For clarity when matching JsonValue types
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
// Add this

// Import from sibling modules
use super::frame_handling::handle_incoming_frame;
use super::guacd_parser::{GuacdInstruction, GuacdParser};
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
    // UDP associations for SOCKS5 UDP ASSOCIATE response handling (server mode)
    pub(crate) udp_associations: super::udp::UdpAssociations,
    // Reverse index: conn_no -> set of destination addresses for efficient cleanup
    pub(crate) udp_conn_index:
        Arc<std::sync::Mutex<HashMap<u32, std::collections::HashSet<std::net::SocketAddr>>>>,
    // UDP receiver tasks for client mode (RAII: ensures proper cleanup)
    pub(crate) udp_receiver_tasks: Arc<Mutex<HashMap<u32, tokio::task::JoinHandle<()>>>>,
    // Timestamp for the last channel-level ping sent (conn_no=0)
    pub(crate) channel_ping_sent_time: Mutex<Option<u64>>,

    // For signaling connection task closures to the main Channel run loop
    pub(crate) conn_closed_tx: mpsc::UnboundedSender<(u32, String)>, // (conn_no, channel_id)
    conn_closed_rx: Option<mpsc::UnboundedReceiver<(u32, String)>>,
    // Stores the conn_no of the primary Guacd data connection
    pub(crate) primary_guacd_conn_no: Arc<Mutex<Option<u32>>>,

    // Store the close reason when control connection closes
    pub(crate) channel_close_reason: Arc<Mutex<Option<CloseConnectionReason>>>,
    // Callback token for router communication
    pub(crate) callback_token: Option<String>,
    // KSM config for router communication
    pub(crate) ksm_config: Option<String>,
    // Client version for router communication
    pub(crate) client_version: String,
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
    pub client_version: String,
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
            client_version,
        } = params;
        debug!("Channel::new called (channel_id: {})", channel_id);
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Initial protocol_settings received by Channel::new (channel_id: {})",
                channel_id
            );
        }

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
                            debug!("Configuring for GuacD protocol (channel_id: {}, protocol_type: {})", channel_id, protocol_name_str);
                            determined_protocol = ActiveProtocol::Guacd;
                            initial_protocol_state =
                                ProtocolLogicState::Guacd(ChannelGuacdState::default());

                            if let Some(guacd_dedicated_settings_val) =
                                protocol_settings.get("guacd")
                            {
                                if unlikely!(crate::logger::is_verbose_logging()) {
                                    debug!("Found 'guacd' block in protocol_settings: {:?} (channel_id: {})", guacd_dedicated_settings_val, channel_id);
                                }
                                if let JsonValue::Object(guacd_map) = guacd_dedicated_settings_val {
                                    guacd_host_setting = guacd_map
                                        .get("guacd_host")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);
                                    guacd_port_setting = guacd_map
                                        .get("guacd_port")
                                        .and_then(|v| v.as_u64())
                                        .map(|p| p as u16);
                                    debug!("Parsed from dedicated 'guacd' settings block. (channel_id: {})", channel_id);
                                } else {
                                    warn!(
                                        "'guacd' block was not a JSON Object. (channel_id: {})",
                                        channel_id
                                    );
                                }
                            } else if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("No dedicated 'guacd' block found in protocol_settings. Guacd server host/port might come from guacd_params or defaults. (channel_id: {})", channel_id);
                            }

                            if let Some(guacd_params_json_val) =
                                protocol_settings.get("guacd_params")
                            {
                                debug!(
                                    "Found 'guacd_params' in protocol_settings. (channel_id: {})",
                                    channel_id
                                );
                                if unlikely!(crate::logger::is_verbose_logging()) {
                                    debug!("Raw guacd_params value for direct processing. (channel_id: {}, guacd_params_value: {:?})", channel_id, guacd_params_json_val);
                                }

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
                                    debug!("Populated guacd_params map directly from JSON Value. (channel_id: {})", channel_id);

                                    // Override protocol name with correct guacd protocol name from ConversationType
                                    let guacd_protocol_name = parsed_conversation_type.to_string();
                                    temp_initial_guacd_params_map.insert(
                                        "protocol".to_string(),
                                        guacd_protocol_name.clone(),
                                    );
                                    debug!("Set guacd protocol name from ConversationType (channel_id: {}, guacd_protocol_name: {})", channel_id, guacd_protocol_name);
                                } else {
                                    error!("guacd_params was not a JSON object. Value: {:?} (channel_id: {})", guacd_params_json_val, channel_id);
                                }
                            } else {
                                debug!("'guacd_params' key not found in protocol_settings. (channel_id: {})", channel_id);
                            }
                        } else {
                            // Handle non-Guacd types like Tunnel or SOCKS5 if network rules are present
                            match parsed_conversation_type {
                                ConversationType::Tunnel => {
                                    // Check if we should use SOCKS5 protocol
                                    let should_use_socks5 = network_checker.is_some()
                                        || protocol_settings
                                            .get("socks_mode")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);

                                    if should_use_socks5 {
                                        debug!("Configuring for SOCKS5 protocol (Tunnel type with network rules or socks_mode) (channel_id: {})", channel_id);
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(
                                            ChannelSocks5State::default(),
                                        );
                                    } else {
                                        debug!("Configuring for PortForward protocol (Tunnel type) (channel_id: {})", channel_id);
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
                                        debug!("Configuring for SOCKS5 protocol (network rules present) (channel_id: {}, protocol_type: {})", channel_id, protocol_name_str);
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(
                                            ChannelSocks5State::default(),
                                        );
                                    } else {
                                        debug!("Configuring for PortForward protocol (defaulting) (channel_id: {}, protocol_type: {})", channel_id, protocol_name_str);
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
                        error!("Invalid conversationType string. Erroring out. (channel_id: {}, protocol_type: {})", channel_id, protocol_name_str);
                        return Err(anyhow::anyhow!(
                            "Invalid conversationType string: {}",
                            protocol_name_str
                        ));
                    }
                }
            } else {
                // protocol_name_val is not a string
                error!(
                    "conversationType is not a string. Erroring out. (channel_id: {})",
                    channel_id
                );
                return Err(anyhow::anyhow!("conversationType is not a string"));
            }
        } else {
            // "conversationType" not found
            error!("No specific protocol defined (conversationType missing). Erroring out. (channel_id: {})", channel_id);
            return Err(anyhow::anyhow!(
                "No specific protocol defined (conversationType missing)"
            ));
        }

        let mut final_connect_as_settings = ConnectAsSettings::default();
        if let Some(connect_as_settings_val) = protocol_settings.get("connect_as_settings") {
            debug!(
                "Found 'connect_as_settings' in protocol_settings. (channel_id: {})",
                channel_id
            );
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Raw connect_as_settings value. (channel_id: {}, cas_value: {:?})",
                    channel_id, connect_as_settings_val
                );
            }
            match serde_json::from_value::<ConnectAsSettings>(connect_as_settings_val.clone()) {
                Ok(parsed_settings) => {
                    final_connect_as_settings = parsed_settings;
                    debug!("Successfully deserialized connect_as_settings into ConnectAsSettings struct. (channel_id: {})", channel_id);
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Final connect_as_settings. (channel_id: {}, final_connect_as_settings: {:?})", channel_id, final_connect_as_settings);
                    }
                }
                Err(e) => {
                    error!("CRITICAL: Failed to deserialize connect_as_settings: {}. Value was: {:?} (channel_id: {})", e, connect_as_settings_val, channel_id);
                    // Returning an error here if connect_as_settings are vital
                    return Err(anyhow!("Failed to deserialize connect_as_settings: {}", e));
                }
            }
        } else {
            debug!("'connect_as_settings' key not found in protocol_settings. Using default. (channel_id: {})", channel_id);
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
            udp_receiver_tasks: Arc::new(Mutex::new(HashMap::new())), // RAII: Track client-side UDP receiver tasks
            channel_ping_sent_time: Mutex::new(None),
            conn_closed_tx,
            conn_closed_rx: Some(conn_closed_rx),
            primary_guacd_conn_no: Arc::new(Mutex::new(None)),
            channel_close_reason: Arc::new(Mutex::new(None)),
            callback_token,
            ksm_config,
            client_version,
        };

        debug!(
            "Channel initialized (channel_id: {}, server_mode: {})",
            new_channel.channel_id, new_channel.server_mode
        );

        Ok(new_channel)
    }

    pub async fn run(mut self) -> Result<(), ChannelError> {
        self.setup_webrtc_state_monitoring();

        let mut buf = BytesMut::with_capacity(64 * 1024);

        // Take the receiver channel for server connections
        let mut server_conn_rx = self.local_client_server_conn_rx.take();

        // Take ownership of conn_closed_rx for the select loop
        let mut local_conn_closed_rx = self.conn_closed_rx.take().ok_or_else(|| {
            error!("conn_closed_rx was already taken or None. Channel cannot monitor connection closures. (channel_id: {})", self.channel_id);
            ChannelError::Internal("conn_closed_rx missing at start of run".to_string())
        })?;

        // Main processing loop - reads from WebRTC and dispatches frames
        while !self.should_exit.load(std::sync::atomic::Ordering::Relaxed) {
            // Process any complete frames in the buffer
            while let Some(frame) = try_parse_frame(&mut buf) {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Received frame from WebRTC (channel_id: {}, connection_no: {}, payload_size: {})", self.channel_id, frame.connection_no, frame.payload.len());
                }

                if let Err(e) = handle_incoming_frame(&mut self, frame).await {
                    error!(
                        "Error handling frame (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                }
            }

            tokio::select! {
                // Check for any new connections from the server
                // Fair scheduling: random polling order prevents keyboard input starvation
                maybe_conn = async { server_conn_rx.as_mut()?.recv().await }, if server_conn_rx.is_some() => {
                    if let Some((conn_no, writer, task)) = maybe_conn {
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            debug!("Registering connection from server (channel_id: {})", self.channel_id);
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
                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("Received data from WebRTC (channel_id: {}, bytes_received: {})", self.channel_id, chunk.len());

                                if !chunk.is_empty() && unlikely!(crate::logger::is_verbose_logging()) {
                                    debug!("First few bytes of received data (channel_id: {}, first_bytes: {:?})", self.channel_id, &chunk[..std::cmp::min(20, chunk.len())]);
                                }
                            }

                            buf.extend_from_slice(&chunk);
                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("Buffer size after adding chunk (channel_id: {}, buffer_size: {})", self.channel_id, buf.len());
                            }

                            // Process pending messages might be triggered by buffer low,
                            // but also good to try after receiving new data if not recently triggered.
                        }
                        Ok(None) => {
                            info!("WebRTC data channel closed or sender dropped. (channel_id: {})", self.channel_id);
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
                        info!("Connection task reported exit to Channel run loop. (channel_id: {}, conn_no: {})", closed_channel_id, closed_conn_no);

                        let mut is_critical_closure = false;
                        if self.active_protocol == ActiveProtocol::Guacd {
                            let primary_opt = self.primary_guacd_conn_no.lock().await;
                            if let Some(primary_conn_no) = *primary_opt {
                                if primary_conn_no == closed_conn_no {
                                    warn!("Critical Guacd data connection has closed. Initiating channel shutdown. (channel_id: {}, conn_no: {})", self.channel_id, closed_conn_no);
                                    is_critical_closure = true;
                                }
                            }
                        }

                        if is_critical_closure {
                            self.should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
                            // Attempt to gracefully close the control connection (conn_no 0) as well, if not already closing.
                            // This sends a CloseConnection message to the client for the channel itself.
                            if closed_conn_no != 0 { // Avoid self-triggering if conn_no 0 was what closed to signal this.
                                info!("Shutting down control connection (0) due to critical upstream closure. (channel_id: {})", self.channel_id);
                                if let Err(e) = self.close_backend(0, CloseConnectionReason::UpstreamClosed).await {
                                    debug!("Error explicitly closing control connection (0) during critical shutdown. (channel_id: {}, error: {})", self.channel_id, e);
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
                        info!("Connection closure signal channel (conn_closed_rx) closed. (channel_id: {})", self.channel_id);
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
                warn!(
                    "Failed to stop server during cleanup (channel_id: {}, error: {})",
                    self.channel_id, e
                );
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
                    if unlikely!(crate::logger::is_verbose_logging()) {
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
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Channel({}): Sent channel PING (conn_no=0, empty payload convention), recorded send time.", self.channel_id);
                }
            }
        }

        let buffered_amount = self.webrtc.buffered_amount().await;
        if buffered_amount >= STANDARD_BUFFER_THRESHOLD
            && unlikely!(crate::logger::is_verbose_logging())
        {
            debug!(
                "Control message buffer full, but sending control message anyway (channel_id: {})",
                self.channel_id
            );
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

        debug!("Closing connection - Connection summary (channel_id: {}, conn_no: {}, reason: {:?}, total_connections: {}, remaining_connections: {:?})",
              self.channel_id, conn_no, reason, total_connections, remaining_connections);

        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&conn_no.to_be_bytes());
        buffer.put_u8(reason as u8);
        let msg_data = buffer.freeze();

        self.internal_handle_connection_close(conn_no, reason)
            .await?;

        self.send_control_message(ControlMessage::CloseConnection, &msg_data)
            .await?;

        // For control connections or explicit cleanup, remove immediately
        let should_delay_removal = conn_no != 0 && reason != CloseConnectionReason::Normal;

        if !should_delay_removal {
            // Send Guacd disconnect message with specific reason before removing connection
            if let Err(e) = self
                .send_guacd_disconnect_message(conn_no, &reason.to_message())
                .await
            {
                warn!("Failed to send Guacd disconnect message during immediate close (channel_id: {}, error: {})", self.channel_id, e);
            }

            // CRITICAL: For Guacd sessions, wait for guacd to process disconnect and complete cleanup
            // Guacd needs time to:
            // 1. Receive the "10.disconnect;" instruction (already transmitted via conn.shutdown)
            // 2. Parse the instruction
            // 3. Execute session cleanup (close RDP/VNC/SSH, free resources, write audit logs)
            // 4. Close the connection gracefully from its side
            // Without this delay, we close TCP socket before guacd finishes cleanup
            if self.active_protocol == ActiveProtocol::Guacd {
                debug!("Waiting 500ms for guacd to process disconnect and complete cleanup (channel_id: {}, conn_no: {})", self.channel_id, conn_no);
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }

            // Immediate removal using DashMap
            if let Some((_, conn)) = self.conns.remove(&conn_no) {
                // Shutdown the connection gracefully (closes channels and waits for tasks)
                if let Err(e) = conn.shutdown().await {
                    warn!(
                        "Error during connection shutdown (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                } else {
                    debug!(
                        "Successfully closed connection and tasks (channel_id: {})",
                        self.channel_id
                    );
                }
            }
        } else {
            // Delayed removal - signal shutdown but keep in map briefly for pending messages
            if let Some(conn_ref) = self.conns.get(&conn_no) {
                // Signal the connection to close its data channel
                // (dropping the sender will cause the backend task to exit)
                if !conn_ref.data_tx.is_closed() {
                    debug!(
                        "Signaling connection to close data channel (channel_id: {})",
                        self.channel_id
                    );
                }
            }

            // Schedule delayed cleanup
            let conns_arc = Arc::clone(&self.conns);
            let channel_id_clone = self.channel_id.clone();

            // Spawn a task to remove the connection after a grace period
            tokio::spawn(async move {
                // Wait a bit to allow in-flight messages to be processed
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                debug!(
                    "Grace period elapsed, removing connection from maps (channel_id: {})",
                    channel_id_clone
                );

                // Now remove from maps
                if let Some((_, conn)) = conns_arc.remove(&conn_no) {
                    // Shutdown the connection gracefully
                    if let Err(e) = conn.shutdown().await {
                        warn!(
                            "Error during delayed connection shutdown (channel_id: {}, error: {})",
                            channel_id_clone, e
                        );
                    } else {
                        debug!(
                            "Successfully closed connection after grace period (channel_id: {})",
                            channel_id_clone
                        );
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

    /// Send Guacd disconnect message to both server and client before closing connection
    async fn send_guacd_disconnect_message(&self, conn_no: u32, reason: &str) -> Result<()> {
        // Only send disconnect for Guacd connections
        if self.active_protocol != ActiveProtocol::Guacd {
            return Ok(());
        }

        // Check if this is the primary Guacd connection
        let is_primary = {
            let primary_opt = self.primary_guacd_conn_no.lock().await;
            *primary_opt == Some(conn_no)
        };

        if !is_primary {
            debug!(
                "Not primary Guacd connection, skipping disconnect message (channel_id: {})",
                self.channel_id
            );
            return Ok(());
        }

        debug!("Sending Guacd log and disconnect message to server and client (channel_id: {}, reason: {})", self.channel_id, reason);

        // Create the log instruction first: log message for debugging
        let log_instruction = GuacdInstruction::new("log".to_string(), vec![reason.to_string()]);
        let log_bytes = GuacdParser::guacd_encode_instruction(&log_instruction);

        // Create the disconnect instruction: "10.disconnect;"
        let disconnect_instruction = GuacdInstruction::new("disconnect".to_string(), vec![]);
        let disconnect_bytes = GuacdParser::guacd_encode_instruction(&disconnect_instruction);

        // Send log message to server (backend) first
        if let Some(conn_ref) = self.conns.get(&conn_no) {
            if !conn_ref.data_tx.is_closed() {
                let log_server_message = crate::models::ConnectionMessage::Data(log_bytes.clone());
                if let Err(e) = conn_ref.data_tx.send(log_server_message) {
                    warn!(
                        "Failed to send log message to server (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                } else {
                    debug!("Successfully sent log message to Guacd server (channel_id: {}, reason: {})", self.channel_id, reason);
                }

                // Then send disconnect message to server
                let disconnect_server_message =
                    crate::models::ConnectionMessage::Data(disconnect_bytes.clone());
                if let Err(e) = conn_ref.data_tx.send(disconnect_server_message) {
                    warn!(
                        "Failed to send disconnect message to server (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                } else {
                    debug!(
                        "Successfully sent disconnect message to Guacd server (channel_id: {})",
                        self.channel_id
                    );
                }
            }
        }

        // Send log message to client (via WebRTC) first
        let log_data_frame = Frame::new_data_with_pool(conn_no, &log_bytes, &self.buffer_pool);
        let log_encoded_frame = log_data_frame.encode_with_pool(&self.buffer_pool);

        if let Err(e) = self.webrtc.send(log_encoded_frame).await {
            if !e.contains("Channel is closing") {
                warn!(
                    "Failed to send log message to client (channel_id: {}, error: {})",
                    self.channel_id, e
                );
            }
        } else {
            debug!(
                "Successfully sent log message to client (channel_id: {}, reason: {})",
                self.channel_id, reason
            );
        }

        // Then send disconnect message to client (via WebRTC)
        let disconnect_data_frame =
            Frame::new_data_with_pool(conn_no, &disconnect_bytes, &self.buffer_pool);
        let disconnect_encoded_frame = disconnect_data_frame.encode_with_pool(&self.buffer_pool);

        let send_start = std::time::Instant::now();
        match self.webrtc.send(disconnect_encoded_frame.clone()).await {
            Ok(_) => {
                let send_latency = send_start.elapsed();
                crate::metrics::METRICS_COLLECTOR.record_message_sent(
                    &self.channel_id,
                    disconnect_encoded_frame.len() as u64,
                    Some(send_latency),
                );
                debug!(
                    "Successfully sent disconnect message to client (channel_id: {})",
                    self.channel_id
                );
            }
            Err(e) => {
                if !e.contains("Channel is closing") {
                    warn!(
                        "Failed to send disconnect message to client (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                    crate::metrics::METRICS_COLLECTOR
                        .record_error(&self.channel_id, "disconnect_message_send_failed");
                }
            }
        }

        Ok(())
    }

    /// Internal method for closing connections without sending a CloseConnection message
    /// This is used when handling received CloseConnection messages to prevent feedback loops
    pub(crate) async fn internal_close_backend_no_message(
        &mut self,
        conn_no: u32,
        reason: CloseConnectionReason,
    ) -> Result<()> {
        let total_connections = self.conns.len();
        let remaining_connections = self.get_connection_ids_except(conn_no);

        debug!("Closing connection (no message) - Connection summary (channel_id: {}, conn_no: {}, reason: {:?}, total_connections: {}, remaining_connections: {:?})",
              self.channel_id, conn_no, reason, total_connections, remaining_connections);

        self.internal_handle_connection_close(conn_no, reason)
            .await?;

        // For control connections or explicit cleanup, remove immediately
        let should_delay_removal = conn_no != 0 && reason != CloseConnectionReason::Normal;

        if !should_delay_removal {
            // Send Guacd disconnect message with specific reason before removing connection
            if let Err(e) = self
                .send_guacd_disconnect_message(conn_no, &reason.to_message())
                .await
            {
                warn!("Failed to send Guacd disconnect message during immediate close (no message) (channel_id: {}, error: {})", self.channel_id, e);
            }

            // CRITICAL: For Guacd sessions, wait for guacd to process disconnect and complete cleanup
            // Guacd needs time to receive, parse, and execute cleanup after receiving "10.disconnect;"
            // This matches the 500ms delay used for data connections (line 993) and prevents
            // interrupting guacd's cleanup by closing TCP socket too early
            if self.active_protocol == ActiveProtocol::Guacd {
                debug!("Waiting 500ms for guacd to process disconnect and complete cleanup (channel_id: {}, conn_no: {})", self.channel_id, conn_no);
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }

            // Immediate removal using DashMap
            if let Some((_, conn)) = self.conns.remove(&conn_no) {
                // Shutdown the connection gracefully (closes channels and waits for tasks)
                if let Err(e) = conn.shutdown().await {
                    warn!(
                        "Error during connection shutdown (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                } else {
                    debug!(
                        "Successfully closed connection and tasks (channel_id: {})",
                        self.channel_id
                    );
                }
            }
        } else {
            // Delayed removal - signal shutdown but keep in map briefly for pending messages
            if let Some(conn_ref) = self.conns.get(&conn_no) {
                // Signal the connection to close its data channel
                // (dropping the sender will cause the backend task to exit)
                if !conn_ref.data_tx.is_closed() {
                    debug!(
                        "Signaling connection to close data channel (channel_id: {})",
                        self.channel_id
                    );
                }
            }

            // Schedule delayed cleanup
            let conns_arc = Arc::clone(&self.conns);
            let channel_id_clone = self.channel_id.clone();

            // Spawn a task to remove the connection after a grace period
            tokio::spawn(async move {
                // Wait a bit to allow in-flight messages to be processed
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                debug!(
                    "Grace period elapsed, removing connection from maps (channel_id: {})",
                    channel_id_clone
                );

                // Now remove from maps
                if let Some((_, conn)) = conns_arc.remove(&conn_no) {
                    // Shutdown the connection gracefully
                    if let Err(e) = conn.shutdown().await {
                        warn!(
                            "Error during delayed connection shutdown (channel_id: {}, error: {})",
                            channel_id_clone, e
                        );
                    } else {
                        debug!(
                            "Successfully closed connection after grace period (channel_id: {})",
                            channel_id_clone
                        );
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
        debug!(
            "internal_handle_connection_close (channel_id: {})",
            self.channel_id
        );

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
            debug!(
                "Stopping server due to critical connection closure (channel_id: {})",
                self.channel_id
            );
            if let Err(e) = self.stop_server().await {
                warn!(
                    "Failed to stop server during connection close (channel_id: {}, error: {})",
                    self.channel_id, e
                );
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
                        debug!("Primary GuacD data connection closed, clearing reference (channel_id: {})", self.channel_id);
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
                debug!(
                    "Extracted target from guacd field ({}): host={:?}, port={:?} (channel_id: {})",
                    context, dest_host, dest_port, channel_id
                );
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

        info!("Channel '{}' closing - Final stats: {} connections: {:?}, {} bytes buffered (channel_id: {}, server_mode: {}, active_protocol: {:?})",
              self.channel_id, total_connections, connection_ids, buffered_amount, self.channel_id, self.server_mode, self.active_protocol);

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
        let active_protocol = self.active_protocol;

        runtime.spawn(async move {
            // Collect connection numbers from DashMap
            let conn_keys = Self::extract_connection_ids(&conns_clone);
            for conn_no in conn_keys {
                if conn_no == 0 { continue; }

                // Send close frame to remote peer
                let mut close_buffer = buffer_pool_clone.acquire();
                close_buffer.clear();
                close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                close_buffer.put_u8(CloseConnectionReason::Normal as u8);

                let close_frame = Frame::new_control_with_buffer(ControlMessage::CloseConnection, &mut close_buffer);
                let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                if let Err(e) = webrtc.send(encoded).await {
                    if !e.contains("Channel is closing") {
                        warn!("Error sending close frame in drop for connection (channel_id: {}, error: {})", channel_id, e);
                    }
                }
                buffer_pool_clone.release(close_buffer);

                // Send graceful shutdown message before aborting tasks
                if let Some(conn_ref) = conns_clone.get(&conn_no) {
                    if active_protocol == ActiveProtocol::Guacd {
                        // For guacd: send disconnect instruction
                        let disconnect_instruction = crate::channel::guacd_parser::GuacdInstruction::new(
                            "disconnect".to_string(),
                            vec![]
                        );
                        let disconnect_bytes = crate::channel::guacd_parser::GuacdParser::guacd_encode_instruction(
                            &disconnect_instruction
                        );
                        let disconnect_message = crate::models::ConnectionMessage::Data(disconnect_bytes);

                        if let Err(e) = conn_ref.data_tx.send(disconnect_message) {
                            debug!("Failed to send disconnect to guacd in drop (channel_id: {}, error: {})", channel_id, e);
                        } else {
                            debug!("Sent disconnect instruction to guacd (channel_id: {}, conn_no: {})", channel_id, conn_no);
                        }
                    } else {
                        // For port forwarding/SOCKS5: send EOF for graceful TCP shutdown
                        if let Err(e) = conn_ref.data_tx.send(crate::models::ConnectionMessage::Eof) {
                            debug!("Failed to send EOF in drop (channel_id: {}, error: {})", channel_id, e);
                        } else {
                            debug!("Sent EOF for graceful shutdown (channel_id: {}, conn_no: {}, protocol: {:?})", channel_id, conn_no, active_protocol);
                        }
                    }

                    // Brief delay to allow shutdown message to be written before aborting tasks
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }

                // Shutdown the connection (now aborts tasks immediately)
                if let Some((_, conn)) = conns_clone.remove(&conn_no) {
                    if let Err(e) = conn.shutdown().await {
                        debug!("Error shutting down connection in drop (channel_id: {}, error: {})", channel_id, e);
                    }
                }
            }
            info!("Channel cleanup completed (channel_id: {})", channel_id);
        });

        // RAII FIX: Clean up ALL UDP receiver tasks in Drop (client mode)
        let udp_receiver_tasks = self.udp_receiver_tasks.clone();
        let channel_id_for_udp = self.channel_id.clone();
        runtime.spawn(async move {
            let mut tasks = udp_receiver_tasks.lock().await;
            let task_count = tasks.len();
            for (conn_no, task) in tasks.drain() {
                task.abort();
                debug!(
                    "Channel({}): Aborted UDP receiver task in Drop (conn_no: {})",
                    channel_id_for_udp, conn_no
                );
            }
            if task_count > 0 {
                info!(
                    "Channel({}): Cleaned up {} UDP receiver tasks in Drop (RAII)",
                    channel_id_for_udp, task_count
                );
            }
        });
    }
}
