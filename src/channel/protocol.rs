use crate::runtime::get_runtime;
use anyhow::{anyhow, Result};
use bytes::Buf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::net::ToSocketAddrs;
use tracing::{debug, error, info, warn, trace};
use tokio::io::{AsyncWriteExt};

use super::core::Channel;
use crate::tube_protocol::{ControlMessage, CloseConnectionReason, CONN_NO_LEN};

// Import from the new connect_as module
use super::connect_as::decrypt_connect_as_payload;

// Placeholder constants - ensure these are defined correctly in your project scope
// or move them to a more appropriate central location (e.g., types.rs or a consts.rs)
pub(crate) const PUBLIC_KEY_LENGTH: usize = 65; 
pub(crate) const NONCE_LENGTH: usize = 12;

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
    pub(crate) async fn process_control_message(
        &mut self, 
        message_type: ControlMessage, 
        data: &[u8]
    ) -> Result<()> {
        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Processing control message: {:?}", message_type);

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

        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Closing connection {} (reason: {:?})", target_connection_no, reason);

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
        let target_connection_no = cursor.get_u32();

        debug!(target: "protocol_event", channel_id=%self.channel_id, conn_no = target_connection_no, payload_len = cursor.remaining(), server_mode = self.server_mode, active_protocol = ?self.active_protocol, "Endpoint Received OpenConnection request");

        // Prepare data for the detailed trace log BEFORE the trace! macro
        let guacd_host_clone = self.guacd_host.clone();
        let guacd_port_clone = self.guacd_port; // Option<u16> is Copy
        let connect_as_settings_clone = self.connect_as_settings.clone();
        
        // Lock and prepare the guacd_params for logging
        let guacd_params_locked = self.guacd_params.lock().await;
        let guacd_params_for_log = format!("{:?}", *guacd_params_locked);
        drop(guacd_params_locked); // Release the lock as soon as possible

        trace!(target: "protocol_event",
            channel_id = %self.channel_id,
            conn_no = target_connection_no,
            guacd_host = ?guacd_host_clone,
            guacd_port = ?guacd_port_clone,
            connect_as_settings = ?connect_as_settings_clone,
            guacd_params_map = %guacd_params_for_log, // Log the prepared string
            "Channel state at OpenConnection for Guacd target determination"
        );

        // If this channel is a server-mode PortForwarder, it originates connections locally.
        // An incoming OpenConnection for a specific conn_no is an ack/part of handshake from the client-side of the tunnel.
        // The server should already have (or be in the process of setting up) this conn_no from a local TCP accept.
        // It should not try to establish a new outbound connection based on this message.
        if self.server_mode && self.active_protocol == super::types::ActiveProtocol::PortForward {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Server-mode PortForward received OpenConnection for conn_no {}. Acknowledging with ConnectionOpened.", target_connection_no);
            // The connection should be in self.conns or will be shortly via the local TCP accept path.
            // Sending ConnectionOpened confirms to the client that this conn_no is active on the server side.
            self.send_control_message(ControlMessage::ConnectionOpened, &target_connection_no.to_be_bytes()).await?;
            return Ok(());
        }

        // Existing check: if connection already processed and in conns map (e.g. client mode, or SOCKS5 server after target resolution)
        if self.conns.lock().await.contains_key(&target_connection_no) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} already exists or is being processed. Sending ConnectionOpened.", target_connection_no);
            self.send_control_message(ControlMessage::ConnectionOpened, &target_connection_no.to_be_bytes()).await?;
            return Ok(());
        }

        let mut host_to_connect: String;
        let mut port_to_connect: u16;

        // Scenario 1: SOCKS5 - Python SOCKS5Server sends conn_no, host_len, host, port
        // Scenario 2: Guacd with connect_as - Python TunnelEntrance (acting on behalf of GuacdExit's request)
        //             might send conn_no, then an encrypted blob. The other Python end (GuacdExit) decrypts this.
        //             The Rust channel's role here is primarily to establish the TCP link to the guacd server.
        //             The *final* guacd host/port might be fixed, come from channel config, or be derivable
        //             if the Rust channel itself were to decrypt a part of the connect_as blob (less likely).
        // Scenario 3: Simple Port Forward / Guacd (no connect_as in payload) - Python TunnelEntrance sends just conn_no.
        //             Rust channel uses its pre-configured target.

        if cursor.has_remaining() { 
            if self.network_checker.is_some() && cursor.remaining() >= 4 { // 4 for host_len u32
                let pos_before_socks_parse = cursor.position();
                let host_len = cursor.get_u32() as usize;
                if cursor.remaining() >= host_len + 2 { 
                    let mut host_bytes = vec![0u8; host_len];
                    cursor.copy_to_slice(&mut host_bytes);
                    let parsed_host = String::from_utf8(host_bytes)
                        .map_err(|e| anyhow!("Invalid UTF-8 in SOCKS host: {}", e))?;
                    let parsed_port = cursor.get_u16();

                    if let Some(ref checker) = self.network_checker {
                        if !checker.is_host_allowed(&parsed_host) {
                            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 target host {} not allowed.", parsed_host);
                            return self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ConnectionFailed, "SOCKS5 host not allowed").await;
                        }
                        if !checker.is_port_allowed(parsed_port) {
                            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 target port {} not allowed.", parsed_port);
                            return self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ConnectionFailed, "SOCKS5 port not allowed").await;
                        }
                    }
                    host_to_connect = parsed_host;
                    port_to_connect = parsed_port;
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Parsed SOCKS5 target from OpenConnection: {}:{} for conn_no {}", host_to_connect, port_to_connect, target_connection_no);
                    return self.establish_backend_connection(target_connection_no, &host_to_connect, port_to_connect).await;
                } else {
                    // Not enough data for SOCKS5 host/port, rewind cursor for other parsing logic.
                    cursor.set_position(pos_before_socks_parse);
                    warn!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint OpenConnection payload looked like SOCKS5 but was too short. Rewinding for other parsing.");
                }
            }

            // If not SOCKS5 or SOCKS5 parsing failed to consume, the payload might be for other protocols (e.g., connect_as for Guacd)
            // For Guacd with 'connect_as'; the other side sends the OpenConnection with this payload.
            // The Rust channel's job is to connect to the guacd server.
            // The Rust channel here might need to know the Guacd server's host/port from its own config, especially if 'connect_as' doesn't change it.
            if self.active_protocol == super::types::ActiveProtocol::Guacd {
                let remaining_at_connect_as_check = cursor.remaining();

                // Using self.connect_as_settings directly
                return if (self.connect_as_settings.allow_supply_user || self.connect_as_settings.allow_supply_host) &&
                    remaining_at_connect_as_check >= (4 /*len field*/ + PUBLIC_KEY_LENGTH + NONCE_LENGTH) {
                    let connect_as_declared_len = cursor.get_u32() as usize;

                    if cursor.remaining() >= (PUBLIC_KEY_LENGTH + NONCE_LENGTH + connect_as_declared_len) {
                        let mut client_public_key = vec![0u8; PUBLIC_KEY_LENGTH];
                        cursor.copy_to_slice(&mut client_public_key);

                        let mut client_nonce = vec![0u8; NONCE_LENGTH];
                        cursor.copy_to_slice(&mut client_nonce);

                        let mut encrypted_connect_as_data = vec![0u8; connect_as_declared_len];
                        cursor.copy_to_slice(&mut encrypted_connect_as_data);

                        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Extracted Guacd 'connect_as' fields for conn_no {}. Encrypted payload length: {}.", target_connection_no, connect_as_declared_len);
                        // Using self.connect_as_settings directly
                        if let Some(private_key_hex) = &self.connect_as_settings.gateway_private_key {
                            match decrypt_connect_as_payload(
                                private_key_hex,
                                &client_public_key,
                                &client_nonce,
                                &encrypted_connect_as_data,
                            ) {
                                Ok(decrypted_payload) => {
                                    info!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Successfully decrypted 'connect_as' payload for conn_no {}: {:?}", target_connection_no, decrypted_payload);

                                    // Use self.guacd_host and self.guacd_port as defaults
                                    host_to_connect = self.guacd_host.clone().unwrap_or_else(|| "localhost".to_string());
                                    port_to_connect = self.guacd_port.unwrap_or(4822);

                                    if self.connect_as_settings.allow_supply_host {
                                        if let Some(supplied_host) = decrypted_payload.host {
                                            host_to_connect = supplied_host;
                                            info!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Using host '{}' from 'connect_as' payload for conn_no {}.", host_to_connect, target_connection_no);
                                        }
                                        if let Some(supplied_port) = decrypted_payload.port {
                                            port_to_connect = supplied_port;
                                            info!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Using port {} from 'connect_as' payload for conn_no {}.", port_to_connect, target_connection_no);
                                        }
                                    }

                                    if self.connect_as_settings.allow_supply_user {
                                        if let Some(user_info) = decrypted_payload.user {
                                            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint User details from 'connect_as' for conn_no {}: {:?}", target_connection_no, user_info);

                                            let mut params_guard = self.guacd_params.lock().await;
                                            // Clear specific connect_as params before adding new ones for this connection? Or augment?
                                            // For now, augmenting. If params are strictly per-connection and not additive to base, clearing might be needed.
                                            if let Some(username) = user_info.username {
                                                params_guard.insert("username".to_string(), username);
                                            }
                                            if let Some(password) = user_info.password {
                                                params_guard.insert("password".to_string(), password);
                                            }
                                            if let Some(domain) = user_info.domain {
                                                params_guard.insert("domain".to_string(), domain);
                                            }
                                            if let Some(private_key) = user_info.private_key {
                                                params_guard.insert("private-key".to_string(), private_key);
                                            }
                                            if let Some(pk_pass) = user_info.private_key_passphrase {
                                                params_guard.insert("private_key_passphrase".to_string(), pk_pass);
                                            }
                                            if let Some(connect_db) = user_info.connect_database {
                                                params_guard.insert("connect_database".to_string(), connect_db);
                                            }
                                            if let Some(d_name) = user_info.distinguished_name {
                                                params_guard.insert("distinguished_name".to_string(), d_name);
                                            }
                                            drop(params_guard);
                                            warn!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %target_connection_no, "Updated self.guacd_params with connect_as user details. TODO: Ensure params are correctly passed to Guacd connection logic.");
                                        }
                                    }
                                    self.establish_backend_connection(target_connection_no, &host_to_connect, port_to_connect).await
                                },
                                Err(e) => {
                                    error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to decrypt/process 'connect_as' payload for conn_no {}: {}. Check key configuration and payload structure.", target_connection_no, e);
                                    self.close_backend_and_notify(target_connection_no, CloseConnectionReason::DecryptionFailed, "Connect As decryption/processing failed").await
                                }
                            }
                        } else {
                            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Gateway private key for 'connect_as' not configured (self.connect_as_settings). Cannot decrypt payload for conn_no {}.", target_connection_no);
                            self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ConfigurationError, "Connect As private key missing").await
                        }
                    } else {
                        error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Invalid Guacd 'connect_as' data for conn_no {}: not enough bytes for key, nonce, and declared payload length {}. Declared: {}, cursor remaining: {}, key+nonce needed: {}.", target_connection_no, connect_as_declared_len, connect_as_declared_len, cursor.remaining(), PUBLIC_KEY_LENGTH + NONCE_LENGTH);
                        self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ProtocolError, "Invalid Connect As data structure (length mismatch)").await
                    }
                } else {
                    error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint OpenConnection for Guacd conn_no {} does not match 'connect_as' structure (min length for header+key+nonce: {}, remaining: {}) or 'connect_as' is not allowed/configured (self.connect_as_settings). Using configured Guacd target.", target_connection_no, 4 + PUBLIC_KEY_LENGTH + NONCE_LENGTH, remaining_at_connect_as_check);
                    self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ProtocolError, "Invalid Connect As data structure (length mismatch)").await
                }
            }
        }
 
        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint OpenConnection for conn_no {} has no (or unparsed) specific target payload. Using pre-configured target.", target_connection_no);
        match self.active_protocol {
            super::types::ActiveProtocol::PortForward => {
                if let super::core::ProtocolLogicState::PortForward(ref pf_state) = self.protocol_state {
                    host_to_connect = pf_state.target_host.clone().ok_or_else(|| anyhow!("PortForward target host not set for conn_no {}", target_connection_no))?;
                    port_to_connect = pf_state.target_port.ok_or_else(|| anyhow!("PortForward target port not set for conn_no {}", target_connection_no))?;
                } else {
                    return Err(anyhow!("Invalid state for PortForward OpenConnection for conn_no {}", target_connection_no));
                }
            }
            super::types::ActiveProtocol::Guacd => {
                // Use self.guacd_host and self.guacd_port
                host_to_connect = self.guacd_host.clone().unwrap_or_else(|| "localhost".to_string());
                port_to_connect = self.guacd_port.unwrap_or(4822);
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Guacd OpenConnection (no specific payload) for conn_no {}. Connecting to configured Guacd server {}:{}.", target_connection_no, host_to_connect, port_to_connect);
            }
            super::types::ActiveProtocol::Socks5 => {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 mode, but OpenConnection for conn_no {} did not contain a valid SOCKS5 target payload.", target_connection_no);
                return self.close_backend_and_notify(target_connection_no, CloseConnectionReason::ConnectionFailed, "SOCKS5 target missing in OpenConnection").await;
            }
        }
        self.establish_backend_connection(target_connection_no, &host_to_connect, port_to_connect).await
    }

    /// Helper to close the backend and potentially send a CloseConnection control message.
    async fn close_backend_and_notify(&mut self, conn_no: u32, reason: CloseConnectionReason, log_message: &str) -> Result<()> {
        error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint {} for conn_no {}. Closing backend.", log_message, conn_no);
        // The close_backend method should handle removing the connection from self.conns
        // and cleaning up its resources. It might also send a CloseConnection control message.
        // If close_backend doesn't always send a notification, we might need to send one here.
        self.close_backend(conn_no, reason).await
        // Optionally, ensure a CloseConnection is sent if not handled by close_backend:
        // let mut payload = Vec::new();
        // payload.extend_from_slice(&conn_no.to_be_bytes());
        // payload.extend_from_slice(&(reason as u16).to_be_bytes());
        // self.send_control_message(ControlMessage::CloseConnection, &payload).await?;
        // Ok(()) returning Ok because the OpenConnection *message itself* was processed, even if the outcome was failure.
    }

    /// Helper function to establish backend connection and send ConnectionOpened
    async fn establish_backend_connection(&mut self, conn_no: u32, host: &str, port: u16) -> Result<()> {
        let target_str = format!("{}:{}", host, port);
        info!(target: "protocol_connect", channel_id = %self.channel_id, conn_no, target_address = %target_str, active_protocol = ?self.active_protocol, "Attempting to establish backend connection to target.");

        // Check network access rules if configured (primarily for client-side initiated from OpenConnection)
        if let Some(ref checker) = self.network_checker {
            if !checker.is_host_allowed(host) {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Target host {} not allowed by network rules for conn_no {}.", host, conn_no);
                self.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                // We might want to send a CloseConnection control message back to origin if not already handled by close_backend.
                return Ok(()); // Return Ok to signify a message handled, even if the connection failed rules.
            }
            if !checker.is_port_allowed(port) {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Target port {} not allowed by network rules for conn_no {}.", port, conn_no);
                self.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                return Ok(());
            }
        }

        let addrs = match target_str.to_socket_addrs() {
            Ok(iter) => iter.collect::<Vec<_>>(),
            Err(e) => {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to resolve address {}: {} for conn_no {}.", target_str, e, conn_no);
                self.close_backend(conn_no, CloseConnectionReason::AddressResolutionFailed).await?;
                return Ok(());
            }
        };

        if addrs.is_empty() {
            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Could not resolve address: {} for conn_no {}.", target_str, conn_no);
            self.close_backend(conn_no, CloseConnectionReason::AddressResolutionFailed).await?;
            return Ok(());
        }

        match tokio::time::timeout(
            self.timeouts.open_connection,
            super::connections::open_backend(self, conn_no, addrs[0], self.active_protocol)
        ).await {
            Ok(Ok(_)) => {
                self.send_control_message(ControlMessage::ConnectionOpened, &conn_no.to_be_bytes()).await?;
                info!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Successfully opened backend connection {} to {}:{}. Sent ConnectionOpened.", conn_no, host, port);
            }
            Ok(Err(e)) => {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to open backend connection {} to {}:{}: {}", conn_no, host, port, e);
                self.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
            }
            Err(_) => {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Backend connection {} to {}:{} timed out.", conn_no, host, port);
                self.close_backend(conn_no, CloseConnectionReason::Timeout).await?;
            }
        }
        Ok(())
    }

    /// Handle a Ping control message
    async fn handle_ping(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            // Basic ping without connection info
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received basic Ping request");
            self.send_control_message(
                ControlMessage::Pong,
                &[0, 0, 0, 0] // connection 0
            ).await?;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Check if the connection exists
        if !self.conns.lock().await.contains_key(&conn_no) {
            error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} not found for Ping", conn_no);
            return Ok(());
        }
        
        // Build response - include connection number
        let mut response = Vec::with_capacity(4);
        response.extend_from_slice(&conn_no.to_be_bytes());
        
        // Handle timing information if present
        if data.len() > CONN_NO_LEN {
            // TODO: transfer_latency tracking implement here. 
            response.extend_from_slice(&data[CONN_NO_LEN..]);
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK request with timing data for {}", conn_no);
        } else {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK request for {}", conn_no);
        }
        
        // Send response
        self.send_control_message(ControlMessage::Pong, &response).await?;
        
        Ok(())
    }

    /// Handle a Pong control message
    async fn handle_pong(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received basic pong");
            self.ping_attempt = 0;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        
        // Update connection state
        let mut conns_guard = self.conns.lock().await;
        if let Some(conn) = conns_guard.get_mut(&conn_no) {
            // Reset message counter
            conn.stats.message_counter = 0;
            
            if conn_no == 0 {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received pong");
            } else {
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ACK response for {}", conn_no);
            }
            
            // Handle round-trip latency calculation if ping time was set
            if let Some(ping_time) = conn.stats.ping_time {
                let latency = now_ms().saturating_sub(ping_time);
                
                // Add to a round-trip latency collection
                {
                    let mut rtl_guard = self.round_trip_latency.lock().await;
                    rtl_guard.push(latency);
                    // Keep the collection size bounded
                    if rtl_guard.len() > 20 {
                        rtl_guard.remove(0);
                    }
                }
                
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Round trip latency: {} ms", latency);
                
                // Reset ping time
                conn.stats.ping_time = None;
                
                // Handle transfer latency calculation
                // In Python, there's additional transfer latency tracking that we'd implement here
            }
        } else {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received pong for unknown connection {}", conn_no);
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
        
        // Check if the connection exists and send EOF
        let mut conns_guard = self.conns.lock().await;
        if let Some(conn) = conns_guard.get_mut(&conn_no) {
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Sending EOF to backend for connection {}", conn_no);
            
            let backend = conn.backend.as_mut(); // Get a mutable reference to the Box<dyn BackendConnection>
            
            match backend.shutdown().await { // Call shutdown on the trait object
                Ok(_) => {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Successfully sent shutdown to backend for EOF on conn {}", conn_no);
                }
                Err(e) => {
                    error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Error shutting down backend for EOF on conn {}: {}", conn_no, e);
                    // Decide if we need to hard close or if shutdown failure is non-fatal for the EOF intent.
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
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Received ConnectionOpened in client mode (ignoring)");
            return Ok(());
        }

        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("ConnectionOpened message too short"));
        }

        let connection_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Starting reader for connection {}", connection_no);
        
        // Get the connection
        let mut conns_guard = self.conns.lock().await;
        let connection = match conns_guard.get_mut(&connection_no) {
            Some(c) => c,
            None => {
                error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Connection {} not found for ConnectionOpened", connection_no);
                return Ok(());
            }
        };
        
        // If it's a SOCKS5 connection, send a success response to the client
        if self.active_protocol == super::types::ActiveProtocol::Socks5 {
            debug!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "SOCKS5 Connection opened");
            
            let response = [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
            
            let backend = connection.backend.as_mut();
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Sending SOCKS5 success response to connection {}", connection_no);
            
            match backend.write_all(&response).await {
                Ok(_) => {
                    debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint SOCKS5 success response sent for connection {}", connection_no);
                    if let Err(e) = backend.flush().await {
                        error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to flush SOCKS5 writer: {}", e);
                    }
                },
                Err(e) => {
                    error!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Failed to send SOCKS5 success response: {}", e);
                }
            }
        }
        
        // The conn.to_webrtc task is already set up by open_backend (via establish_backend_connection)
        // to handle reading from the backend and sending to WebRTC. No need to spawn another one here.
        if connection.to_webrtc.is_finished() {
            warn!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "In ConnectionOpened, to_webrtc task was already finished. This is unexpected.");
        }
        
        info!(target: "protocol_event", channel_id=%self.channel_id, conn_no = %connection_no, "Connection fully opened and ready.");
        
        drop(conns_guard);
        Ok(())
    }
    // 
    // /// Send data to a target connection
    // async fn send_data_to_target(&mut self, connection_no: u32, data: &[u8]) -> Result<()> {
    //     // Get the connection
    //     let mut conns_guard = self.conns.lock().await;
    //     let conn = match conns_guard.get_mut(&connection_no) {
    //         Some(c) => c,
    //         None => {
    //             drop(conns_guard);
    //             return Err(anyhow!("Connection {} not found for send_data_to_target", connection_no));
    //         }
    //     };
    //     
    //     let backend = conn.backend.as_mut();
    //     
    //     // Write to backend
    //     match backend.write_all(data).await {
    //         Ok(_) => {
    //             // Also flush the writer to ensure data is sent
    //             match backend.flush().await {
    //                 Ok(_) => Ok(()),
    //                 Err(e) => {
    //                     drop(conns_guard);
    //                     Err(anyhow!("Failed to flush backend for send_data_to_target: {}", e))
    //                 }
    //             }
    //         },
    //         Err(e) => {
    //             drop(conns_guard);
    //             Err(anyhow!("Failed to write to backend for send_data_to_target: {}", e))
    //         }
    //     }
    // }

    // /// Parse target host and port from the data in an OpenConnection message
    // async fn parse_target_info(&mut self, data: &[u8], connection_no: u32) -> Result<(String, u16)> {
    //     if data.len() < 4 {
    //         return Err(anyhow!("Target info data too short"));
    //     }
    //     
    //     let mut cursor = std::io::Cursor::new(data);
    //     
    //     // Read host length (4 bytes)
    //     let mut host_len_bytes = [0u8; 4];
    //     std::io::Read::read_exact(&mut cursor, &mut host_len_bytes)
    //         .map_err(|e| anyhow!("Failed to read host length: {}", e))?;
    //     let host_len = u32::from_be_bytes(host_len_bytes) as usize;
    //     
    //     if host_len > 256 {
    //         return Err(anyhow!("Host length too large: {}", host_len));
    //     }
    //     
    //     // Read host
    //     let mut host_bytes = vec![0u8; host_len];
    //     std::io::Read::read_exact(&mut cursor, &mut host_bytes)
    //         .map_err(|e| anyhow!("Failed to read host: {}", e))?;
    //     let host = String::from_utf8(host_bytes)
    //         .map_err(|e| anyhow!("Invalid UTF-8 in host: {}", e))?;
    //     
    //     // Read port (2 bytes)
    //     let mut port_bytes = [0u8; 2];
    //     std::io::Read::read_exact(&mut cursor, &mut port_bytes)
    //         .map_err(|e| anyhow!("Failed to read port: {}", e))?;
    //     let port = u16::from_be_bytes(port_bytes);
    //     
    //     debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Parsed target for connection {}: {}:{}", connection_no, host, port);
    //     
    //     Ok((host, port))
    // }

    /// Update connection statistics based on received data
    async fn update_stats(&mut self, connection_no: u32, bytes: usize, timestamp: u64) {
        // Get current time
        let now = now_ms();
        
        // Calculate receive latency (how long it took for the message to arrive)
        let receive_latency = now.saturating_sub(timestamp);
        
        // Update connection stats
        let mut conns_guard = self.conns.lock().await;
        if let Some(conn) = conns_guard.get_mut(&connection_no) {
            // Update bytes received
            conn.stats.receive_size += bytes;
            
            // Update message counter (used for ACK tracking)
            conn.stats.message_counter += 1;
            
            // Update receive latency
            conn.stats.receive_latency_sum += receive_latency;
            conn.stats.receive_latency_count += 1;
            
            // Log stats periodically (every 1000 messages)
            if conn.stats.message_counter % 1000 == 0 {
                let avg_latency = if conn.stats.receive_latency_count > 0 {
                    conn.stats.receive_latency_sum / conn.stats.receive_latency_count
                } else {
                    0
                };
                
                debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint Stats for connection {}: {} bytes received, avg latency {} ms", connection_no, conn.stats.receive_size, avg_latency);
            }
        } else if connection_no != 0 {
            // Only log for non-control connections
            debug!(target: "protocol_event", channel_id=%self.channel_id, "Endpoint No connection found for stats update: {}", connection_no);
        }
    }
}
