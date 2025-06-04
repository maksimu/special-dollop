use crate::runtime::get_runtime;
use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn, trace};


use super::core::{Channel, BUFFER_LOW_THRESHOLD};
use crate::tube_protocol::{ControlMessage, CloseConnectionReason, CONN_NO_LEN};

// Import from the new connect_as module
use super::connect_as::decrypt_connect_as_payload;

// Constants for ConnectAs, similar to those in connections.rs
const CONNECT_AS_DETAILS_LEN_FIELD_BYTES: usize = 4;
const CONNECT_AS_PUBLIC_KEY_BYTES: usize = 65; // As per Python: 65-byte public key
const CONNECT_AS_NONCE_BYTES: usize = 12;    // As per Python: 12 byte nonce

use p256::SecretKey as P256SecretKey;
use p256::pkcs8::DecodePrivateKey; // Trait for from_pkcs8_pem
use hex;

/// Get the current time in milliseconds since epoch
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Channel {
    pub(crate) fn setup_webrtc_state_monitoring(&mut self) {
        let webrtc = self.webrtc.clone();
        let channel_id_base = self.channel_id.clone(); // Base clone for the function scope
        
        let last_state = webrtc.ready_state();
        let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);
        
        let data_channel = webrtc.data_channel.clone(); 
        
        let state_tx_open = state_tx.clone();
        let channel_id_for_open = channel_id_base.clone(); // Clone for on_open
        data_channel.on_open(Box::new(move || {
            let tx = state_tx_open.clone();
            let channel_id_log = channel_id_for_open.clone(); // Clone for async block
            tokio::spawn(async move {
                if let Err(e) = tx.send("Open".to_string()).await {
                    warn!(target: "protocol_event", channel_id=%channel_id_log, error=%e, "Failed to send open state notification");
                }
            });
            Box::pin(async {})
        }));
        
        let state_tx_close = state_tx.clone();
        let channel_id_for_close = channel_id_base.clone(); // Clone for on_close
        data_channel.on_close(Box::new(move || {
            let tx = state_tx_close.clone();
            let channel_id_log = channel_id_for_close.clone(); // Clone for async block
            tokio::spawn(async move {
                if let Err(e) = tx.send("Closed".to_string()).await {
                    warn!(target: "protocol_event", channel_id=%channel_id_log, error=%e, "Failed to send close state notification");
                }
            });
            Box::pin(async {})
        }));
        
        let state_tx_error = state_tx.clone();
        let channel_id_for_error = channel_id_base.clone(); // Clone for on_error
        data_channel.on_error(Box::new(move |err| {
            let tx = state_tx_error.clone();
            let err_str = format!("Error: {}", err);
            let channel_id_log = channel_id_for_error.clone(); // Clone for async block
            tokio::spawn(async move {
                if let Err(e) = tx.send(err_str).await {
                    warn!(target: "protocol_event", channel_id=%channel_id_log, error=%e, "Failed to send error state notification");
                }
            });
            Box::pin(async {})
        }));
        
        let runtime = get_runtime();
        let channel_id_for_runtime_spawn = channel_id_base.clone(); // Clone for runtime spawn
        runtime.spawn(async move {
            let mut last_state_in_task = last_state;
            let channel_id_log = channel_id_for_runtime_spawn.clone(); // Clone for use in loop
            while let Some(current_state) = state_rx.recv().await {
                if current_state != last_state_in_task {
                    info!(target: "protocol_event", channel_id=%channel_id_log, "Endpoint WebRTC state changed: {} -> {}", last_state_in_task, current_state);
                    last_state_in_task = current_state.clone();
                }
                
                let lower_current_state = current_state.to_lowercase();
                if lower_current_state == "closed" || lower_current_state.starts_with("error") {
                    info!(target: "protocol_event", channel_id=%channel_id_log, state = %current_state, "Endpoint WebRTC state, stopping state monitoring task.");
                    break;
                }
            }
        });
        info!(target: "protocol_event", channel_id=%channel_id_base, "Channel WebRTC state change monitoring set up.");
    }

    /// Process a control message received from the data channel
    // **BOLD WARNING: HOT PATH - CALLED FOR EVERY CONTROL MESSAGE**
    // **NO STRING ALLOCATIONS IN DEBUG LOGS UNLESS ENABLED**
    pub(crate) async fn process_control_message(
        &mut self, 
        message_type: ControlMessage, 
        data: &[u8]
    ) -> Result<()> {
        if tracing::enabled!(tracing::Level::DEBUG) {
            let active_connections = self.conns.len();
            let connection_list = self.get_connection_ids();
            let pending_msg_count = self.get_pending_message_count().await;
            
            debug!(target: "protocol_event", channel_id=%self.channel_id, 
                   ?message_type, active_connections, pending_msg_count, ?connection_list,
                   "Processing control message - Channel stats");
        }

        match message_type {
            ControlMessage::CloseConnection => {
                self.handle_close_connection(data).await?;
            },
            ControlMessage::OpenConnection => {
                self.handle_open_connection(data).await?;
            },
            ControlMessage::Ping => {
                self.handle_ping(data).await?;
            },
            ControlMessage::Pong => {
                self.handle_pong(data).await?;
            },
            ControlMessage::SendEOF => {
                self.handle_send_eof(data).await?;
            },
            ControlMessage::ConnectionOpened => {
                // In server mode, this completes protocol handshakes like SOCKS5
                // In client mode, we just log and continue
                self.handle_connection_opened(data).await?;
            },
        }

        Ok(())
    }

    /// Handle a CloseConnection control message
    async fn handle_close_connection(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("CloseConnection message too short"));
        }

        let target_connection_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Extract reason if available
        let reason = if data.len() > CONN_NO_LEN {
            let reason_code = u16::from_be_bytes([data[CONN_NO_LEN], data[CONN_NO_LEN + 1]]);
            CloseConnectionReason::from_code(reason_code)
        } else {
            CloseConnectionReason::Normal
        };

        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Closing connection {} (reason: {:?})", target_connection_no, reason);
        }

        // Special case for connection 0 (control connection)
        if target_connection_no == 0 {
            self.should_exit.store(true, std::sync::atomic::Ordering::Release);
            return Ok(());
        }

        // Close the connection
        self.close_backend(target_connection_no, reason).await
    }

    /// Handle an OpenConnection control message
    async fn handle_open_connection(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("OpenConnection message too short, expected at least {} bytes for conn_no", CONN_NO_LEN));
        }

        let mut cursor = std::io::Cursor::new(data);
        let target_connection_no = cursor.get_u32(); // Consumes first 4 bytes

        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, conn_no = target_connection_no, payload_len = cursor.remaining(), server_mode = self.server_mode, active_protocol = ?self.active_protocol, "Endpoint Received OpenConnection request");
        }
        
        // Initialize effective host/port with channel defaults
        let mut effective_guacd_host = self.guacd_host.clone();
        let mut effective_guacd_port = self.guacd_port;

        // --- ConnectAs Logic (integrated from connections.rs) ---
        if self.active_protocol == super::types::ActiveProtocol::Guacd &&
           (self.connect_as_settings.allow_supply_user || self.connect_as_settings.allow_supply_host) &&
           self.connect_as_settings.gateway_private_key.is_some() {
            
            debug!("Channel({}): Attempting ConnectAs for Guacd target_conn_no {}", self.channel_id, target_connection_no);

            if cursor.remaining() >= CONNECT_AS_DETAILS_LEN_FIELD_BYTES {
                let connect_as_payload_len = cursor.get_u32() as usize; // Consumes 4 bytes
                let required_crypto_block_len = CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES + connect_as_payload_len;

                if cursor.remaining() >= required_crypto_block_len {
                    let mut crypto_buffer = self.buffer_pool.acquire();
                    crypto_buffer.clear();
                    crypto_buffer.resize(required_crypto_block_len, 0);
                    
                    cursor.copy_to_slice(&mut crypto_buffer[..]); // Consumes required_crypto_block_len bytes
                    
                    let client_public_key_bytes = &crypto_buffer[..CONNECT_AS_PUBLIC_KEY_BYTES];
                    let nonce_bytes = &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES..CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES];
                    let encrypted_data_bytes = &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES..];

                    let gateway_private_key_pem = self.connect_as_settings.gateway_private_key.as_ref().unwrap();

                    // 1. Parse PKCS#8 PEM to P256SecretKey
                    let p256_secret_key = P256SecretKey::from_pkcs8_pem(gateway_private_key_pem)
                        .map_err(|e| {
                            error!("Channel({}): Failed to parse gateway private key PKCS#8 PEM: {}. PEM was: '{}'", self.channel_id, e, gateway_private_key_pem);
                            anyhow!("Failed to parse gateway private key PKCS#8 PEM: {}", e)
                        })?;

                    // 2. Get the raw scalar bytes of the P256SecretKey
                    let secret_key_scalar_bytes = p256_secret_key.to_bytes(); // This returns FieldBytes<NistP256>

                    // 3. Convert raw scalar bytes to hex string
                    // FieldBytes<C> (which is GenericArray<u8, C::FieldSize>) implements AsRef<[u8]>,
                    // so it can be passed directly to hex::encode.
                    let gateway_private_key_hex = hex::encode(secret_key_scalar_bytes);

                    match decrypt_connect_as_payload(
                        &gateway_private_key_hex, // Use the hex string of raw scalar bytes
                        client_public_key_bytes,
                        nonce_bytes,
                        encrypted_data_bytes,
                    ) {
                        Ok(decrypted_payload) => {
                            info!("Channel({}): Successfully decrypted ConnectAs payload for target_conn_no {}", self.channel_id, target_connection_no);
                            let mut guacd_params_locked = self.guacd_params.lock().await;

                            // Apply user credentials if EITHER allow_supply_user OR allow_supply_host is true (matches Python logic)
                            if self.connect_as_settings.allow_supply_user || self.connect_as_settings.allow_supply_host {
                                if let Some(user_details) = decrypted_payload.user {
                                    debug!("Channel({}): Applying ConnectAs user details", self.channel_id);
                                    
                                    if let Some(val) = user_details.username { 
                                        guacd_params_locked.insert("username".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.password { 
                                        guacd_params_locked.insert("password".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.private_key { 
                                        guacd_params_locked.insert("privatekey".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.private_key_passphrase { 
                                        guacd_params_locked.insert("privatekeypassphrase".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.passphrase { 
                                        guacd_params_locked.insert("passphrase".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.domain { 
                                        guacd_params_locked.insert("domain".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.connect_database { 
                                        guacd_params_locked.insert("connectdatabase".to_string(), val); 
                                    }
                                    if let Some(val) = user_details.distinguished_name { 
                                        guacd_params_locked.insert("distinguishedname".to_string(), val); 
                                    }
                                }
                            }
                            if self.connect_as_settings.allow_supply_host {
                                if let Some(host) = decrypted_payload.host {
                                    debug!("Channel({}): ConnectAs supplied host: {}", self.channel_id, host);
                                    effective_guacd_host = Some(host);
                                }
                                if let Some(port) = decrypted_payload.port {
                                    debug!("Channel({}): ConnectAs supplied port: {}", self.channel_id, port);
                                    effective_guacd_port = Some(port);
                                }
                            }
                            // Guacd params are updated, effective_guacd_host/port is set.
                        }
                        Err(e) => {
                            error!("Channel({}): Failed to decrypt or parse ConnectAs payload for target_conn_no {}: {}", self.channel_id, target_connection_no, e);
                            self.buffer_pool.release(crypto_buffer);
                            // Unlike original connections.rs, we might not want to immediately return Err here.
                            // Consider if connection should proceed with default Guacd params if ConnectAs fails.
                            // For now, maintaining strict behavior: if ConnectAs is attempted and fails, the connection attempt fails.
                            return Err(anyhow!("ConnectAs decryption/parsing failed: {}", e));
                        }
                    }
                    self.buffer_pool.release(crypto_buffer);
                } else {
                    warn!("Channel({}): ConnectAs payload too short for PK, Nonce, and encrypted data (expected {}, got {}) for target_conn_no {}",
                          self.channel_id, required_crypto_block_len, cursor.remaining(), target_connection_no);
                    return Err(anyhow!("ConnectAs payload too short for PK, Nonce, and encrypted data"));
                }
            } else {
                warn!("Channel({}): ConnectAs payload too short for connect_as_payload_len field (expected {} bytes, got {}) for target_conn_no {}",
                      self.channel_id, CONNECT_AS_DETAILS_LEN_FIELD_BYTES, cursor.remaining(), target_connection_no);
                // If ConnectAs was expected but the payload is too short even for its length, it's an error.
                // If ConnectAs was optional and this path is reached, it implies no ConnectAs data was provided.
                // The original connections.rs returned Err. Here, we might just log and proceed if ConnectAs is not mandatory.
                // For now, assuming if ConnectAs is configured, its presence is expected if data follows conn_no.
                // However, the cursor.remaining() might be 0 if only conn_no was sent.
                // This needs careful consideration of whether ConnectAs data is mandatory or optional if settings allow it.
                // The original logic in connections.rs would bail out. Let's stick to that for now if ConnectAs settings are enabled.
                 if cursor.remaining() > 0 { // If there was some data beyond conn_no but not enough for ConnectAs header
                    return Err(anyhow!("ConnectAs payload present but too short for its own length field"));
                 }
                 // If cursor.remaining() is 0, it means no ConnectAs payload was sent, proceed without it.
                 debug!("Channel({}): No additional payload for ConnectAs provided after target_conn_no {}.", self.channel_id, target_connection_no);
            }
        }
        // --- End of ConnectAs Logic ---


        // Prepare data for the detailed trace log AFTER ConnectAs might have modified params
        let guacd_params_locked = self.guacd_params.lock().await;
        let guacd_params_for_log = format!("{:?}", *guacd_params_locked);
        drop(guacd_params_locked);

        trace!(target: "protocol_event",
            channel_id = %self.channel_id,
            conn_no = target_connection_no,
            effective_guacd_host = ?effective_guacd_host,
            effective_guacd_port = ?effective_guacd_port,
            connect_as_settings = ?self.connect_as_settings,
            guacd_params_map = %guacd_params_for_log,
            "Channel state for OpenConnection (after potential ConnectAs)"
        );

        if self.server_mode && self.active_protocol == super::types::ActiveProtocol::PortForward {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Server-mode PortForward received OpenConnection for conn_no {}. Acknowledging with ConnectionOpened.", target_connection_no);
            self.send_control_message(ControlMessage::ConnectionOpened, &target_connection_no.to_be_bytes()).await?;
            return Ok(());
        }

        if self.conns.contains_key(&target_connection_no) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} already exists or is being processed. Sending ConnectionOpened.", target_connection_no);
            self.send_control_message(ControlMessage::ConnectionOpened, &target_connection_no.to_be_bytes()).await?;
            return Ok(());
        }
        
        // --- Actual Connection Opening Logic ---
        let open_result = match self.active_protocol {
            super::types::ActiveProtocol::Guacd => {
                if let (Some(host), Some(port)) = (effective_guacd_host.as_deref(), effective_guacd_port) {
                    match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
                        Ok(mut addrs) => {
                            if let Some(socket_addr) = addrs.next() {
                                debug!("Channel({}): Guacd OpenConnection for target_conn_no {} to {}:{} (resolved to {}).", 
                                    self.channel_id, target_connection_no, host, port, socket_addr);
                                // Use super::connections to call open_backend
                                super::connections::open_backend(self, target_connection_no, socket_addr, super::types::ActiveProtocol::Guacd).await
                            } else {
                                Err(anyhow!("Could not resolve Guacd host {}:{} to any SocketAddr", host, port))
                            }
                        }
                        Err(e) => Err(anyhow!("DNS lookup failed for Guacd host {}:{}: {}", host, port, e)),
                    }
                } else {
                    Err(anyhow!("Guacd host/port not configured or supplied via ConnectAs for OpenConnection"))
                }
            }
            super::types::ActiveProtocol::PortForward => {
                // This logic is mostly for client mode PortForward. Server mode is handled above.
                if let super::core::ProtocolLogicState::PortForward(pf_state) = &self.protocol_state {
                    if let (Some(host), Some(port)) = (pf_state.target_host.as_deref(), pf_state.target_port) {
                        match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
                            Ok(mut addrs) => {
                                if let Some(socket_addr) = addrs.next() {
                                    debug!("Channel({}): PortForward OpenConnection for target_conn_no {} to {}:{} (resolved to {}).", 
                                        self.channel_id, target_connection_no, host, port, socket_addr);
                                    super::connections::open_backend(self, target_connection_no, socket_addr, super::types::ActiveProtocol::PortForward).await
                                } else {
                                    Err(anyhow!("Could not resolve PortForward host {}:{} to any SocketAddr", host, port))
                                }
                            }
                            Err(e) => Err(anyhow!("DNS lookup failed for PortForward host {}:{}: {}", host, port, e)),
                        }
                    } else {
                        Err(anyhow!("PortForward target host/port not configured in channel protocol state"))
                    }
                } else {
                     Err(anyhow!("Channel is in PortForward mode, but protocol state is not PortForward"))
                }
            }
            super::types::ActiveProtocol::Socks5 => {
                // SOCKS5 connection opening is more complex and typically driven by client requests *after* initial tunnel setup.
                // An OpenConnection here might be an acknowledgment or part of a specific sub-protocol.
                // The original connections.rs deferred SOCKS5 OpenConnection payload parsing.
                // The current protocol.rs has some SOCKS5 host/port parsing if network_checker is present.
                // This section needs to be harmonized based on the exact SOCKS5 flow.
                // For now, let's assume SOCKS5 target details are parsed from `cursor` if present.
                if cursor.has_remaining() && self.network_checker.is_some() && cursor.remaining() >= 4 { // Heuristic: if data remains, it might be SOCKS target
                    // Assuming SOCKS5 target is host_len (u32), then host, then port (u16)
                    // Note: SOCKS addresses can be domain (length prefixed) or IP. This is simplified.
                    // This parsing logic is partially duplicated from below, needs cleanup.
                    let host_len_u32 = cursor.get_u32(); // Read u32 for host_len
                    let host_len = host_len_u32 as usize;


                    if cursor.remaining() >= host_len + 2 { // host_bytes + port_bytes
                        let mut host_buffer = self.buffer_pool.acquire();
                        host_buffer.clear();
                        host_buffer.resize(host_len, 0);
                        cursor.copy_to_slice(&mut host_buffer[..host_len]);
                        let parsed_host = String::from_utf8(host_buffer.to_vec())
                            .map_err(|e| anyhow!("Invalid UTF-8 in SOCKS host: {}", e))?;
                        self.buffer_pool.release(host_buffer);
                        
                        let parsed_port = cursor.get_u16();

                        if let Some(ref checker) = self.network_checker {
                            if !checker.is_host_allowed(&parsed_host) {
                                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 target host {} not allowed.", parsed_host);
                                return Err(anyhow!("SOCKS5 target host {} not allowed", parsed_host));
                            }
                            if !checker.is_port_allowed(parsed_port) {
                                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 target port {} not allowed.", parsed_port);
                                return Err(anyhow!("SOCKS5 target port {} not allowed", parsed_port));
                            }
                        }
                        debug!("Channel({}): SOCKS5 OpenConnection for target_conn_no {} to {}:{}.", 
                                self.channel_id, target_connection_no, parsed_host, parsed_port);
                        
                        // Convert String host to &str for lookup_host
                        match tokio::net::lookup_host(format!("{}:{}", parsed_host, parsed_port)).await {
                            Ok(mut addrs) => {
                                if let Some(socket_addr) = addrs.next() {
                                    super::connections::open_backend(self, target_connection_no, socket_addr, super::types::ActiveProtocol::Socks5).await
                                } else {
                                    Err(anyhow!("Could not resolve SOCKS5 host {}:{} to any SocketAddr", parsed_host, parsed_port))
                                }
                            }
                            Err(e) => Err(anyhow!("DNS lookup failed for SOCKS5 host {}:{}: {}", parsed_host, parsed_port, e)),
                        }
                    } else {
                        warn!(target: "protocol_event", channel_id=%self.channel_id, conn_no=target_connection_no, "SOCKS5 OpenConnection for conn_no {} received, but payload for host/port is incomplete or missing. Remaining: {}", target_connection_no, cursor.remaining());
                        Err(anyhow!("SOCKS5 OpenConnection payload incomplete for host/port"))
                    }
                } else {
                     warn!(target: "protocol_event", channel_id=%self.channel_id, conn_no=target_connection_no, "SOCKS5 OpenConnection for conn_no {} received without host/port payload, or network_checker not configured. Cannot open backend.", target_connection_no);
                     Err(anyhow!("SOCKS5 OpenConnection missing target details or checker"))
                }
            }
        };

        // --- Post OpenConnection Attempt ---
        let mut temp_payload_buffer = self.buffer_pool.acquire(); // Used for ConnectionOpened/CloseConnection
        temp_payload_buffer.clear();

        if let Err(e) = open_result {
            error!("Channel({}): Failed to process OpenConnection for target_conn_no {}: {}. Sending CloseConnection back.", 
                self.channel_id, target_connection_no, e);
            temp_payload_buffer.put_u32(target_connection_no); 
            temp_payload_buffer.put_u16(CloseConnectionReason::ConnectionFailed as u16);
            // Use self.send_control_message for sending
            if let Err(send_err) = self.send_control_message(ControlMessage::CloseConnection, &temp_payload_buffer).await {
                error!("Channel({}): Failed to send CloseConnection for failed OpenConnection {}: {}", self.channel_id, target_connection_no, send_err);
            }
        } else {
            info!("Channel({}): Successfully processed OpenConnection for target_conn_no {}, sending ConnectionOpened.", self.channel_id, target_connection_no);
            temp_payload_buffer.put_u32(target_connection_no);
            if let Err(e) = self.send_control_message(ControlMessage::ConnectionOpened, &temp_payload_buffer).await {
                error!("Channel({}): Error sending ConnectionOpened for conn_no {}: {}", self.channel_id, target_connection_no, e);
            }
        }
        self.buffer_pool.release(temp_payload_buffer);
        Ok(())
    }

    /// Handle a Ping control message
    async fn handle_ping(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            // Basic ping without connection info
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received basic Ping request");
            }
            self.send_control_message(
                ControlMessage::Pong,
                &[0, 0, 0, 0] // connection 0
            ).await?;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Connection 0 is a special control connection that doesn't need to exist in the connection's map
        if conn_no != 0 {
            // Check if the non-control connection exists
            if !self.conns.contains_key(&conn_no) {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} not found for Ping", conn_no);
                return Ok(());
            }
        }
        
        // Build response - include connection number
        // **PERFORMANCE: Use buffer pool instead of Vec allocation**
        let mut response = self.buffer_pool.acquire();
        response.clear();
        response.extend_from_slice(&conn_no.to_be_bytes());
        
        // Handle timing information if present
        if data.len() > CONN_NO_LEN {
            response.extend_from_slice(&data[CONN_NO_LEN..]);
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK request with timing data for {}", conn_no);
            }
        } else {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK request for {}", conn_no);
            }
        }
        
        // Send response - using buffer pool data
        self.send_control_message(ControlMessage::Pong, &response).await?;
        self.buffer_pool.release(response);
        
        Ok(())
    }

    /// Handle a Pong control message
    async fn handle_pong(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received basic pong");
            }
            self.ping_attempt = 0;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Simplified pong handling - no complex stats tracking
        if let Some(_conn_ref) = self.conns.get(&conn_no) {
            if conn_no == 0 {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received pong");
                }
            } else {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK response for {}", conn_no);
                }
            }
        } else {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received pong for unknown connection {}", conn_no);
            }
        }
        
        // Reset ping attempt counter
        self.ping_attempt = 0;
        
        Ok(())
    }

    /// Handle a SendEOF control message
    async fn handle_send_eof(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("SendEOF message too short"));
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Check if the connection exists and handle EOF
        if let Some(conn_ref) = self.conns.get(&conn_no) {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received EOF from remote for connection {}, signaling backend to shutdown write side", conn_no);
            }
            
            // SendEOF means the remote side closed their writing end
            // Send EOF signal to the backend task which will call backend.shutdown() (perfect for RDP!)
            match conn_ref.data_tx.send(crate::models::ConnectionMessage::Eof) {
                Ok(_) => {
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Successfully sent EOF signal to backend for conn {}", conn_no);
                    }
                }
                Err(_) => {
                    // Channel is closed, connection is already dead
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint EOF signal failed, connection {} already closed", conn_no);
                    }
                }
            }
        } else {
            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection for EOF {} not found", conn_no);
        }
        
        Ok(())
    }

    /// Handle a ConnectionOpened control message
    async fn handle_connection_opened(&mut self, data: &[u8]) -> Result<()> {
        // This is called when we receive a ConnectionOpened control message from WebRTC
        // In server_mode, this completes the connection setup
        if !self.server_mode {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ConnectionOpened in client mode (ignoring)");
            }
            return Ok(());
        }

        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("ConnectionOpened message too short"));
        }

        let connection_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Starting reader for connection {}", connection_no);
        }
        
        // Get the connection
        if let Some(conn_ref) = self.conns.get(&connection_no) {
            // If it's a SOCKS5 connection, send a success response to the client
            if self.active_protocol == super::types::ActiveProtocol::Socks5 {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "SOCKS5 Connection opened");
                }
                
                let response = [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
                
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Sending SOCKS5 success response to connection {}", connection_no);
                }
                
                // Send SOCKS5 response via the connection's data channel
                match conn_ref.data_tx.send(crate::models::ConnectionMessage::Data(bytes::Bytes::from(response.to_vec()))) {
                    Ok(_) => {
                        if tracing::enabled!(tracing::Level::DEBUG) {
                            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 success response queued for connection {}", connection_no);
                        }
                    },
                    Err(e) => {
                        error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to queue SOCKS5 success response: {}", e);
                    }
                }
            }
            
            // The conn.to_webrtc and backend tasks are already set up by the connection creation
            // to handle reading from the backend and sending to WebRTC. No need to spawn more tasks here.
            if conn_ref.to_webrtc.is_finished() {
                warn!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "In ConnectionOpened, to_webrtc task was already finished. This is unexpected.");
            }
            
            info!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "Connection fully opened and ready.");
        } else {
            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} not found for ConnectionOpened", connection_no);
            return Ok(());
        }

        // Attempt to send any pending messages for this newly opened connection
        if let Err(e) = self.send_pending_messages(connection_no).await {
            error!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, error = %e, "Error sending pending messages after ConnectionOpened");
            // Depending on desired strictness, you might not want to return an error from handle_connection_opened itself here,
            // as the connection is open, but sending pending messages failed.
        }

        Ok(())
    }

    // Send any pending messages for a given connection
    async fn send_pending_messages(&mut self, conn_no: u32) -> Result<()> {
        // **BOLD WARNING: HOT PATH - SENDING QUEUED MESSAGES**
        // **NO STRING/OBJECT ALLOCATIONS IN THE LOOP**
        // **MESSAGES ARE ALREADY ENCODED AS Bytes**
        
        debug!("Channel({}): Attempting to send pending messages for conn_no {}", self.channel_id, conn_no);
        let mut pending_guard = self.pending_messages.lock().await;
        
        if let Some(queue) = pending_guard.get_mut(&conn_no) {
            if queue.is_empty() {
                debug!("Channel({}): No pending messages for conn_no {}.", self.channel_id, conn_no);
                return Ok(());
            }
            
            let mut messages_sent_count = 0;
            // The WebRTCDataChannel already has set_buffered_amount_low_threshold and on_buffered_amount_low callback.
            // The check here is to proactively avoid over-sending if we are about to send a batch.
            while let Some(message_bytes_ref) = queue.front() { // Peek at the front message
                if message_bytes_ref.is_empty() {
                    queue.pop_front(); // Remove an empty message
                    continue; // Skip empty messages
                }

                let message_bytes_cloned = message_bytes_ref.clone();

                let current_buffered_amount = self.webrtc.buffered_amount().await;
                if current_buffered_amount >= BUFFER_LOW_THRESHOLD { 
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(
                            "Channel({}): Buffer amount {} for conn_no {} is high. Pausing sending of pending messages.",
                            self.channel_id, current_buffered_amount, conn_no
                        );
                    }
                    break; // Stop sending if the buffer is too full
                }

                match self.webrtc.send(message_bytes_cloned.clone()).await { // Send the cloned message
                    Ok(_) => {
                        messages_sent_count += 1;
                        queue.pop_front(); // Successfully sent, now remove from the queue
                    }
                    Err(e) => {
                        error!(
                            "Channel({}): Failed to send pending message for conn_no {}: {}. Message remains at the front of the queue.",
                            self.channel_id, conn_no, e
                        );
                        break; 
                    }
                }
            }
            if messages_sent_count > 0 {
                debug!("Channel({}): Sent {} pending messages for conn_no {}. Remaining in queue: {}", 
                    self.channel_id, messages_sent_count, conn_no, queue.len());
            }
        } else {
            debug!("Channel({}): No pending message queue found for conn_no {}.", self.channel_id, conn_no);
        }
        
        Ok(())
    }
}
