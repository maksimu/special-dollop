// Core Channel implementation

use anyhow::Result;
use bytes::BytesMut;
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use log::{debug, info};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use crate::error::ChannelError;
use crate::tube_protocol::{Frame, ControlMessage, CloseConnectionReason, try_parse_frame};
use crate::models::{Conn, TunnelTimeouts, NetworkAccessChecker, StreamHalf, ConnectionStats, is_guacd_session, ConversationType};
use crate::runtime::get_runtime;
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::buffer_pool::{BufferPool, BufferPoolConfig};
use crate::tube_and_channel_helpers::parse_network_rules_from_settings;
use super::types::ActiveProtocol;

// Import from sibling modules
use super::frame_handling::handle_incoming_frame;
use super::utils::handle_ping_timeout;

// Define the buffer thresholds
pub(crate) const BUFFER_LOW_THRESHOLD: u64 = 14 * 1024 * 1024; // 14MB for responsive UI

// --- Protocol-specific state definitions ---
#[derive(Debug, Default, Clone)]
pub(crate) struct ChannelSocks5State {
    pub handshake_complete: bool,
    pub target_addr: Option<String>, 
    // Add other SOCKS5 specific fields, e.g., current handshake step
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


#[derive(Debug, Clone)]
pub(crate) enum ProtocolLogicState {
    None, // Should not typically be used once initialized unless as a default before settings apply
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
#[derive(Debug, Clone, Default)]
pub struct ConnectAsSettings {
    pub allow_supply_user: bool,
    pub allow_supply_host: bool,
    pub gateway_private_key: Option<String>, // Hex-encoded private key
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
    // Client-side fields
    pub(crate) cleanup_started: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) connection_tx: Option<mpsc::Sender<(u32, StreamHalf, JoinHandle<()>)>>,
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
    pub(crate) guacd_params: Arc<Mutex<HashMap<String, String>>>, // Base Guacd params can be augmented by connect_as

    // Pending message queues for each connection
    pub(crate) pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>>,
    // Buffer pool for efficient buffer management
    pub(crate) buffer_pool: BufferPool,
    // MPSC channel for frames from receive_message to be processed
    frame_input_tx: mpsc::UnboundedSender<Frame>,
    frame_input_rx: Arc<Mutex<mpsc::UnboundedReceiver<Frame>>>,
    // Trigger for processing pending messages when the buffer is low
    pub(crate) process_pending_trigger_tx: mpsc::UnboundedSender<()>, 
    pub(crate) process_pending_trigger_rx: Arc<Mutex<mpsc::UnboundedReceiver<()>>>,
}

// Implement Clone for Channel
impl Clone for Channel {
    fn clone(&self) -> Self {
        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        let (pending_trigger_tx, pending_trigger_rx) = mpsc::unbounded_channel::<()>();
        
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
            process_pending_trigger_tx: pending_trigger_tx,
            process_pending_trigger_rx: Arc::new(Mutex::new(pending_trigger_rx)),
            cleanup_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            connection_tx: None,
        }
    }
}

impl Channel {
    pub async fn new(
        webrtc: WebRTCDataChannel,
        rx_from_dc: mpsc::UnboundedReceiver<Bytes>, // This receiver is from the WebRTC data channel
        channel_id: String,
        timeouts: Option<TunnelTimeouts>,
        protocol_settings: HashMap<String, serde_json::Value>, // This is the main settings object
        server_mode: bool,
    ) -> Result<Self> {
        info!("Channel::new called for id: '{}', with server_mode param: {}", channel_id, server_mode);

        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        let (f_input_tx, f_input_rx) = mpsc::unbounded_channel::<Frame>();
        let (pending_tx, pending_rx) = mpsc::unbounded_channel::<()>();
        
        webrtc.set_buffered_amount_low_threshold(BUFFER_LOW_THRESHOLD);
        
        let buffer_pool_config = BufferPoolConfig {
            buffer_size: 32 * 1024,
            max_pooled: 64,
            resize_on_return: true,
        };
        let buffer_pool = BufferPool::new(buffer_pool_config);
        
        let pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>> = Arc::new(Mutex::new(HashMap::new()));
        
        let network_checker = parse_network_rules_from_settings(&protocol_settings);

        // --- Determine Active Protocol and State ---
        let mut determined_protocol = ActiveProtocol::PortForward; // Default
        let mut initial_protocol_state = ProtocolLogicState::PortForward(ChannelPortForwardState::default()); // Default state

        // --- Parse Guacd specific settings ---
        let mut guacd_host_setting: Option<String> = None;
        let mut guacd_port_setting: Option<u16> = None;
        let mut parsed_connect_as_settings = ConnectAsSettings::default();
        let mut initial_guacd_params = HashMap::new(); // For base params from settings
        
        let mut local_listen_addr_setting: Option<String> = None; // <<< ADDED FOR PARSING

        // Determine protocol
        if let Some(protocol_name_val) = protocol_settings.get("conversationType") { 
            if let Some(protocol_name_str) = protocol_name_val.as_str() {
                match protocol_name_str.parse::<ConversationType>() {
                    Ok(parsed_conversation_type) => {
                        if is_guacd_session(&parsed_conversation_type) {
                            debug!("Channel({}): Configuring for GuacD protocol based on type: {}", channel_id, protocol_name_str);
                            determined_protocol = ActiveProtocol::Guacd;
                            // Guacd host/port from specific guacd settings take precedence
                            // If not found, it will remain None, and protocol_tests will use defaults or error
                            if let Some(guacd_settings_val) = protocol_settings.get("guacd") {
                                if let Some(guacd_settings_map) = guacd_settings_val.as_object() {
                                    guacd_host_setting = guacd_settings_map.get("guacd_host").and_then(|v| v.as_str()).map(String::from);
                                    guacd_port_setting = guacd_settings_map.get("guacd_port").and_then(|v| v.as_u64()).map(|p| p as u16); // Corrected: guacd_port, not guacd_host
                                }
                            }
                            if let Some(guacd_params_settings_val) = protocol_settings.get("guacd_params") {
                                if let Some(params_map) = guacd_params_settings_val.as_object() {
                                    // Parse initial guacd_params
                                    for (k, v) in params_map {
                                        if let Some(val_str) = v.as_str() {
                                            initial_guacd_params.insert(k.clone(), val_str.to_string());
                                        }
                                    }
                                }
                            }
                            initial_protocol_state = ProtocolLogicState::Guacd(ChannelGuacdState::default());
                        } else {
                            // Handle non-Guacd types like Tunnel or SOCKS5 if network rules are present
                            match parsed_conversation_type {
                                ConversationType::Tunnel => {
                                    if network_checker.is_some() && server_mode {
                                        debug!("Channel({}): Configuring for SOCKS5 protocol (conversationType: tunnel, network rules present)", channel_id);
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(ChannelSocks5State::default());
                                    } else {
                                        debug!("Channel({}): Configuring for PortForward protocol (conversationType: tunnel). Server_mode param: {}", channel_id, server_mode);
                                        determined_protocol = ActiveProtocol::PortForward;
                                        if server_mode {
                                            initial_protocol_state = ProtocolLogicState::PortForward(ChannelPortForwardState::default());
                                        } else {
                                            let dest_host = protocol_settings.get("target_host").and_then(|v| v.as_str()).map(String::from);
                                            let dest_port = protocol_settings.get("target_port")
                                                .and_then(|v| {
                                                    // First, try to get it as a u64 directly
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
                                        debug!("Channel({}): Configuring for SOCKS5 protocol (conversationType: {}, network rules present)", channel_id, protocol_name_str);
                                        determined_protocol = ActiveProtocol::Socks5;
                                        initial_protocol_state = ProtocolLogicState::Socks5(ChannelSocks5State::default());
                                    } else {
                                        debug!("Channel({}): Configuring for PortForward protocol (conversationType: {}, defaulting due to no specific rules)", channel_id, protocol_name_str);
                                        determined_protocol = ActiveProtocol::PortForward;
                                        let dest_host = protocol_settings.get("target_host").and_then(|v| v.as_str()).map(String::from);
                                        let dest_port = protocol_settings.get("target_port").and_then(|v| v.as_u64()).map(|p| p as u16);
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
                         debug!("Channel({}): Invalid conversationType string '{}'. Erroring out.", channel_id, protocol_name_str);
                         return Err(anyhow::anyhow!("Invalid conversationType string: {}", protocol_name_str));
                    }
                }
            } else { // protocol_name_val is not a string
                debug!("Channel({}): conversationType is not a string. Erroring out.", channel_id);
                return Err(anyhow::anyhow!("conversationType is not a string"));
            }
        } else { // "conversationType" not found
            debug!("Channel({}): No specific protocol defined (conversationType missing). Erroring out.", channel_id);
            return Err(anyhow::anyhow!("No specific protocol defined (conversationType missing)"));
        }
        // --- End Protocol Determination ---

        if let Some(connect_as_settings_val) = protocol_settings.get("connect_as") {
            if let Some(connect_as_settings_map) = connect_as_settings_val.as_object() {
                if let Some(cas_val) = connect_as_settings_map.get("connect_as") {
                    if let Some(cas_map) = cas_val.as_object() {
                        parsed_connect_as_settings.allow_supply_user = cas_map.get("allow_supply_user").and_then(|v| v.as_bool()).unwrap_or(false);
                        parsed_connect_as_settings.allow_supply_host = cas_map.get("allow_supply_host").and_then(|v| v.as_bool()).unwrap_or(false);
                        parsed_connect_as_settings.gateway_private_key = cas_map.get("gateway_private_key").and_then(|v| v.as_str()).map(String::from);
                    }
                }
            }
        }

        let channel_id_clone = channel_id.clone();
        let process_pending_trigger_tx_clone = pending_tx.clone();

        webrtc.on_buffered_amount_low(Some(Box::new(move || {
            let ep_name_cb = channel_id_clone.clone();
            let trigger_tx = process_pending_trigger_tx_clone.clone();
            tokio::spawn(async move {
               debug!("Endpoint {}: Buffer amount is now below threshold, signaling to process pending messages", ep_name_cb);
               if let Err(e) = trigger_tx.send(()) {
                    log::error!("Endpoint {}: Failed to send pending process trigger: {}", ep_name_cb, e);
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
            
            // Initialize new fields
            guacd_host: guacd_host_setting,
            guacd_port: guacd_port_setting,
            connect_as_settings: parsed_connect_as_settings,
            guacd_params: Arc::new(Mutex::new(initial_guacd_params)),

            pending_messages,
            buffer_pool,
            frame_input_tx: f_input_tx,
            frame_input_rx: Arc::new(Mutex::new(f_input_rx)),
            process_pending_trigger_tx: pending_tx,
            process_pending_trigger_rx: Arc::new(Mutex::new(pending_rx)),
            cleanup_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            connection_tx: None,
        };
        
        info!("Channel '{}' initialized with self.server_mode: {}", new_channel.channel_id, new_channel.server_mode);
        
        Ok(new_channel)
    }

    pub async fn run(mut self) -> Result<(), ChannelError> {
        self.setup_webrtc_state_monitoring();

        let mut buf = BytesMut::with_capacity(64 * 1024);

        // Take the receiver channel for server connections
        let mut server_conn_rx = self.local_client_server_conn_rx.take();

        // Main processing loop - reads from WebRTC and dispatches frames
        while !self.should_exit.load(std::sync::atomic::Ordering::Relaxed) {
            // Process any complete frames in the buffer
            while let Some(frame) = try_parse_frame(&mut buf) {
                debug!("Channel({}): Received frame from WebRTC, connection {}, payload size: {}", 
                        self.channel_id, frame.connection_no, frame.payload.len());
                
                if let Err(e) = handle_incoming_frame(&mut self, frame).await {
                    log::error!("Channel({}): Error handling frame: {}", self.channel_id, e);
                }
            }

            tokio::select! {
                // Check for any new connections from the server
                // Use biasedly selects to prioritize existing connections over new ones if server_conn_rx is Some
                biased;
                maybe_conn = async { server_conn_rx.as_mut()?.recv().await }, if server_conn_rx.is_some() => {
                    if let Some((conn_no, writer, task)) = maybe_conn {
                        debug!("Endpoint {}: Registering connection {} from server", 
                                self.channel_id, conn_no);
                            
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
                            debug!("Channel({}): Received {} bytes from WebRTC", 
                                    self.channel_id, chunk.len());
                            
                            if chunk.len() > 0 {
                                debug!("Channel({}): First few bytes: {:?}", 
                                        self.channel_id, &chunk[..std::cmp::min(20, chunk.len())]);
                            }
                            
                            buf.extend_from_slice(&chunk);
                            debug!("Channel({}): Buffer size after adding chunk: {} bytes", 
                                    self.channel_id, buf.len());
                            
                            // Process pending messages might be triggered by buffer low, 
                            // but also good to try after receiving new data if not recently triggered.
                            self.process_pending_messages().await?;
                        }
                        Ok(None) => {
                            log::info!("Channel({}): WebRTC data channel closed or sender dropped.", self.channel_id);
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
                        debug!("Channel({}): Trigger received to process pending messages.", self.channel_id);
                        self.process_pending_messages().await?;
                    } else {
                        // Trigger channel closed, maybe log or break.
                        log::info!("Channel({}): Process pending trigger channel closed.", self.channel_id);
                        // Depending on desired behavior, you might want to break the loop.
                    }
                }
            }
        }

        self.cleanup_all_connections().await?;
        Ok(())
    }

    // New method to process pending messages for all connections with better prioritization
    pub(crate) async fn process_pending_messages(&mut self) -> Result<()> {
        let mut pending_guard = self.pending_messages.lock().await;
        if pending_guard.iter().all(|(_, queue)| queue.is_empty()) {
            return Ok(());
        }
        debug!("Endpoint {}: Processing pending messages for {} connections", self.channel_id, pending_guard.len());
        let max_to_process = 100;
        let mut total_processed = 0;
        let total_messages: usize = pending_guard.values().map(|q| q.len()).sum();
        if total_messages > 0 {
           debug!("Endpoint {}: Total of {} pending messages across all connections", self.channel_id, total_messages);
        }
        let conn_nos: Vec<u32> = pending_guard.keys().filter(|&conn_no| !pending_guard[conn_no].is_empty()).copied().collect();
        let mut current_index = 0;
        let encode_buffer = self.buffer_pool.acquire();
        let buffer_threshold = BUFFER_LOW_THRESHOLD;
        
        while total_processed < max_to_process && !conn_nos.is_empty() {
            if conn_nos.is_empty() { break; } // safeguard for empty conn_nos
            let conn_index = current_index % conn_nos.len();
            let conn_no = conn_nos[conn_index];
            
            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                if queue.is_empty() { // Check if the queue became empty
                    current_index += 1;
                    continue;
                }
                let message_to_send = if let Some(message) = queue.front() {
                    message.clone()
                } else {
                    current_index += 1;
                    continue;
                };
                
                if let Err(e) = self.webrtc.send(message_to_send).await {
                    log::error!("Endpoint {}: Failed to send queued message for connection {}: {}", self.channel_id, conn_no, e);
                    break;
                }

                queue.pop_front();
                total_processed += 1;
                
                if total_processed % 10 == 0 {
                    let current_buffer = self.webrtc.buffered_amount().await;
                    if current_buffer >= buffer_threshold {
                        debug!("Endpoint {}: Buffer threshold reached during processing, pausing after {} messages", self.channel_id, total_processed);
                        break;
                    }

                }
            } else {
                 // This case should ideally not happen if conn_nos is derived from pending_guard keys
                 log::warn!("Channel {}: Connection {} not found in pending_messages during processing.", self.channel_id, conn_no);
            }
            current_index += 1;
        }
        
        self.buffer_pool.release(encode_buffer);
        if total_processed > 0 {
           debug!("Endpoint {}: Processed {} pending messages in this batch", self.channel_id, total_processed);
        }
        Ok(())
    }

    pub(crate) async fn cleanup_all_connections(&mut self) -> Result<()> {
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
        let buffered_amount = self.webrtc.buffered_amount().await;
        if buffered_amount >= BUFFER_LOW_THRESHOLD {
           debug!("Endpoint {}: Control message buffer full ({} bytes), but sending control message for {}", self.channel_id, buffered_amount, message);
        }
        self.webrtc.send(encoded).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }
    
    // Method to handle received message bytes from a data channel
    pub async fn receive_message(&mut self, bytes: Bytes) -> Result<()> {
        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&bytes);
        
        while let Some(frame) = try_parse_frame(&mut buffer) {
            debug!("Channel({}): Processing frame from message, connection {}, payload size: {}", self.channel_id, frame.connection_no, frame.payload.len());
            
            if let Err(e) = handle_incoming_frame(self, frame).await {
                log::error!("Channel({}): Error handling frame from message processing: {}", self.channel_id, e);
            }
        }
        
        self.buffer_pool.release(buffer);
        Ok(())
    }

    pub(crate) async fn close_backend(&mut self, conn_no: u32, reason: CloseConnectionReason) -> Result<()> {
        log::info!("Endpoint {}: Closing connection {} (reason: {:?})", self.channel_id, conn_no, reason);

        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&conn_no.to_be_bytes());
        buffer.extend_from_slice(&(reason as u16).to_be_bytes());
        let msg_data = buffer.freeze();

        self.internal_handle_connection_close(conn_no, reason).await?;
        
        self.send_control_message(ControlMessage::CloseConnection, &msg_data).await?;


        if let Some(mut conn) = self.conns.lock().await.remove(&conn_no) {
            let _ = crate::models::AsyncReadWrite::shutdown(&mut conn.backend).await;
            conn.to_webrtc.abort();
            match tokio::time::timeout(self.timeouts.close_connection, conn.to_webrtc).await {
                Ok(_) => debug!("Endpoint {}: Successfully closed backend task for connection {}", self.channel_id, conn_no),
                Err(_) => log::warn!("Endpoint {}: Timeout waiting for backend task to close for connection {}", self.channel_id, conn_no),
            }
        }

        // Correctly remove from pending_messages
        {
            let mut pending_guard = self.pending_messages.lock().await;
            pending_guard.remove(&conn_no);
        }

        if conn_no == 0 {
            self.should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) async fn internal_handle_connection_close(&mut self, conn_no: u32, reason: CloseConnectionReason) -> Result<()> {
        debug!("Channel({}): internal_handle_connection_close for conn_no {}, reason: {:?}", self.channel_id, conn_no, reason);
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
                    log::warn!("Channel({}): Error sending close frame in drop for conn {}: {}", channel_id, conn_no, e);
                }
                buffer_pool_clone.release(close_buffer);
            }
            log::info!("Endpoint {}: Basic resource cleanup initiated in drop for Channel", channel_id);
        });
    }
}
