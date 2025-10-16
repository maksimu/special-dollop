use crate::runtime::get_runtime;
use crate::unlikely;
use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut};
use log::{debug, error, info, warn};
use std::time::{SystemTime, UNIX_EPOCH};

use super::core::Channel;
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, CONN_NO_LEN, PORT_LENGTH};

// Import from the new connect_as module
use super::connect_as::decrypt_connect_as_payload;

// Constants for ConnectAs, similar to those in connections.rs
const CONNECT_AS_DETAILS_LEN_FIELD_BYTES: usize = 4;
const CONNECT_AS_PUBLIC_KEY_BYTES: usize = 65; // As per Python: 65-byte public key
const CONNECT_AS_NONCE_BYTES: usize = 12; // As per Python: 12 byte nonce

// Compile-time SOCKS5 success response constant for zero-allocation response
pub(crate) const SOCKS5_SUCCESS_RESPONSE: [u8; 10] =
    [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
use p256::pkcs8::DecodePrivateKey; // Trait for from_pkcs8_pem
use p256::SecretKey as P256SecretKey;

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
                    warn!(
                        "Failed to send open state notification (channel_id: {}, error: {})",
                        channel_id_log, e
                    );
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
                    warn!(
                        "Failed to send close state notification (channel_id: {}, error: {})",
                        channel_id_log, e
                    );
                }
            });
            Box::pin(async {})
        }));

        let state_tx_error = state_tx.clone();
        let channel_id_for_error = channel_id_base.clone(); // Clone for on_error
        data_channel.on_error(Box::new(move |err| {
            let tx = state_tx_error.clone();
            let err_str = format!("Error: {err}");
            let channel_id_log = channel_id_for_error.clone(); // Clone for async block
            tokio::spawn(async move {
                if let Err(e) = tx.send(err_str).await {
                    warn!(
                        "Failed to send error state notification (channel_id: {}, error: {})",
                        channel_id_log, e
                    );
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
                    debug!("Endpoint WebRTC state changed: {} -> {} (channel_id: {})", last_state_in_task, current_state, channel_id_log);
                    last_state_in_task = current_state.clone();
                }

                let lower_current_state = current_state.to_lowercase();
                if lower_current_state == "closed" || lower_current_state.starts_with("error") {
                    debug!("Endpoint WebRTC state, stopping state monitoring task. (channel_id: {}, state: {})", channel_id_log, current_state);
                    break;
                }
            }
        });
        debug!(
            "Channel WebRTC state change monitoring set up. (channel_id: {})",
            channel_id_base
        );
    }

    /// Process a control message received from the data channel
    // **BOLD WARNING: HOT PATH - CALLED FOR EVERY CONTROL MESSAGE**
    // **NO STRING ALLOCATIONS IN DEBUG LOGS UNLESS ENABLED**
    pub(crate) async fn process_control_message(
        &mut self,
        message_type: ControlMessage,
        data: &[u8],
    ) -> Result<()> {
        if log::log_enabled!(log::Level::Debug) {
            let active_connections = self.conns.len();
            let connection_list = self.get_connection_ids();

            debug!("Processing control message - Channel stats (channel_id: {}, message_type: {:?}, active_connections: {}, connection_list: {:?})",
                   self.channel_id, message_type, active_connections, connection_list);
        }

        match message_type {
            ControlMessage::CloseConnection => {
                self.handle_close_connection(data).await?;
            }
            ControlMessage::OpenConnection => {
                self.handle_open_connection(data).await?;
            }
            ControlMessage::Ping => {
                self.handle_ping(data).await?;
            }
            ControlMessage::Pong => {
                self.handle_pong(data).await?;
            }
            ControlMessage::SendEOF => {
                self.handle_send_eof(data).await?;
            }
            ControlMessage::ConnectionOpened => {
                // In server mode, this completes protocol handshakes like SOCKS5
                // In client mode, we just log and continue
                self.handle_connection_opened(data).await?;
            }
            ControlMessage::UdpAssociate => {
                self.handle_udp_associate(data).await?;
            }
            ControlMessage::UdpAssociateOpened => {
                self.handle_udp_associate_opened(data).await?;
            }
            ControlMessage::UdpPacket => {
                self.handle_udp_packet(data).await?;
            }
            ControlMessage::UdpAssociateClosed => {
                self.handle_udp_associate_closed(data).await?;
            }
            ControlMessage::MetricsRequest => {
                self.handle_metrics_request(data).await?;
            }
            ControlMessage::MetricsResponse => {
                self.handle_metrics_response(data).await?;
            }
            ControlMessage::MetricsConfig => {
                self.handle_metrics_config(data).await?;
            }
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
            let reason_code = data[CONN_NO_LEN] as u16;
            CloseConnectionReason::from_code(reason_code)
        } else {
            CloseConnectionReason::Normal
        };

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Endpoint Closing connection {} (reason: {:?}) (channel_id: {})",
                target_connection_no, reason, self.channel_id
            );
        }

        // Special case for connection 0 (control connection)
        if target_connection_no == 0 {
            self.should_exit
                .store(true, std::sync::atomic::Ordering::Release);
            // Store the close reason for the channel - use try_lock to avoid blocking
            if let Ok(mut guard) = self.channel_close_reason.try_lock() {
                *guard = Some(reason);
            }
            // Even if we can't store the reason, we still need to exit
            return Ok(());
        }

        // Close the connection WITHOUT sending another CloseConnection message
        // This prevents feedback loops where both sides keep sending CloseConnection messages
        self.internal_close_backend_no_message(target_connection_no, reason)
            .await?;

        // Clean up any UDP associations for this connection
        self.cleanup_udp_associations_for_connection(target_connection_no)
            .await?;

        Ok(())
    }

    /// Handle an OpenConnection control message
    async fn handle_open_connection(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            return Err(anyhow!(
                "OpenConnection message too short, expected at least {} bytes for conn_no",
                CONN_NO_LEN
            ));
        }

        let mut cursor = std::io::Cursor::new(data);
        let target_connection_no = cursor.get_u32(); // Consumes first 4 bytes

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!("Endpoint Received OpenConnection request (channel_id: {}, conn_no: {}, payload_len: {}, server_mode: {}, active_protocol: {:?})", self.channel_id, target_connection_no, cursor.remaining(), self.server_mode, self.active_protocol);
        }

        // Initialize effective host/port with channel defaults
        let mut effective_guacd_host = self.guacd_host.clone();
        let mut effective_guacd_port = self.guacd_port;

        // --- ConnectAs Logic (integrated from connections.rs) ---
        if self.active_protocol == super::types::ActiveProtocol::Guacd
            && (self.connect_as_settings.allow_supply_user
                || self.connect_as_settings.allow_supply_host)
            && self.connect_as_settings.gateway_private_key.is_some()
        {
            debug!(
                "Channel({}): Attempting ConnectAs for Guacd target_conn_no {}",
                self.channel_id, target_connection_no
            );

            if cursor.remaining() >= CONNECT_AS_DETAILS_LEN_FIELD_BYTES {
                let connect_as_payload_len = cursor.get_u32() as usize; // Consumes 4 bytes
                let required_crypto_block_len =
                    CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES + connect_as_payload_len;

                if cursor.remaining() >= required_crypto_block_len {
                    let mut crypto_buffer = self.buffer_pool.acquire();
                    crypto_buffer.clear();
                    crypto_buffer.resize(required_crypto_block_len, 0);

                    cursor.copy_to_slice(&mut crypto_buffer[..]); // Consumes required_crypto_block_len bytes

                    let client_public_key_bytes = &crypto_buffer[..CONNECT_AS_PUBLIC_KEY_BYTES];
                    let nonce_bytes = &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES
                        ..CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES];
                    let encrypted_data_bytes =
                        &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES..];

                    let gateway_private_key_pem = self
                        .connect_as_settings
                        .gateway_private_key
                        .as_ref()
                        .unwrap();

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
                            if self.connect_as_settings.allow_supply_user
                                || self.connect_as_settings.allow_supply_host
                            {
                                if let Some(user_details) = decrypted_payload.user {
                                    debug!(
                                        "Channel({}): Applying ConnectAs user details",
                                        self.channel_id
                                    );

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
                                        guacd_params_locked
                                            .insert("privatekeypassphrase".to_string(), val);
                                    }
                                    if let Some(val) = user_details.passphrase {
                                        guacd_params_locked.insert("passphrase".to_string(), val);
                                    }
                                    if let Some(val) = user_details.domain {
                                        guacd_params_locked.insert("domain".to_string(), val);
                                    }
                                    if let Some(val) = user_details.connect_database {
                                        guacd_params_locked
                                            .insert("connectdatabase".to_string(), val);
                                    }
                                    if let Some(val) = user_details.distinguished_name {
                                        guacd_params_locked
                                            .insert("distinguishedname".to_string(), val);
                                    }
                                }
                            }
                            if self.connect_as_settings.allow_supply_host {
                                if let Some(host) = decrypted_payload.host {
                                    debug!(
                                        "Channel({}): ConnectAs supplied host: {}",
                                        self.channel_id, host
                                    );
                                    effective_guacd_host = Some(host);
                                }
                                if let Some(port) = decrypted_payload.port {
                                    debug!(
                                        "Channel({}): ConnectAs supplied port: {}",
                                        self.channel_id, port
                                    );
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
                    return Err(anyhow!(
                        "ConnectAs payload too short for PK, Nonce, and encrypted data"
                    ));
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
                if cursor.remaining() > 0 {
                    // If there was some data beyond conn_no but not enough for ConnectAs header
                    return Err(anyhow!(
                        "ConnectAs payload present but too short for its own length field"
                    ));
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

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!("Channel state for OpenConnection (after potential ConnectAs) (channel_id: {}, conn_no: {}, effective_guacd_host: {:?}, effective_guacd_port: {:?}, connect_as_settings: {:?}, guacd_params_map: {})",
                self.channel_id, target_connection_no, effective_guacd_host, effective_guacd_port, self.connect_as_settings, guacd_params_for_log
            );
        }

        if self.server_mode && self.active_protocol == super::types::ActiveProtocol::PortForward {
            debug!("Server-mode PortForward received OpenConnection for conn_no {}. Acknowledging with ConnectionOpened. (channel_id: {})", target_connection_no, self.channel_id);
            self.send_control_message(
                ControlMessage::ConnectionOpened,
                &target_connection_no.to_be_bytes(),
            )
            .await?;
            return Ok(());
        }

        if self.conns.contains_key(&target_connection_no) {
            debug!("Endpoint Connection {} already exists or is being processed. Sending ConnectionOpened. (channel_id: {})", target_connection_no, self.channel_id);
            self.send_control_message(
                ControlMessage::ConnectionOpened,
                &target_connection_no.to_be_bytes(),
            )
            .await?;
            return Ok(());
        }

        // --- Actual Connection Opening Logic ---
        let open_result = match self.active_protocol {
            super::types::ActiveProtocol::Guacd => {
                if let (Some(host), Some(port)) =
                    (effective_guacd_host.as_deref(), effective_guacd_port)
                {
                    match tokio::net::lookup_host(format!("{host}:{port}")).await {
                        Ok(mut addrs) => {
                            if let Some(socket_addr) = addrs.next() {
                                debug!("Channel({}): Guacd OpenConnection for target_conn_no {} to {}:{} (resolved to {}).",
                                    self.channel_id, target_connection_no, host, port, socket_addr);
                                // Use super::connections to call open_backend
                                super::connections::open_backend(
                                    self,
                                    target_connection_no,
                                    socket_addr,
                                    super::types::ActiveProtocol::Guacd,
                                )
                                .await
                            } else {
                                Err(anyhow!(
                                    "Could not resolve Guacd host {}:{} to any SocketAddr",
                                    host,
                                    port
                                ))
                            }
                        }
                        Err(e) => Err(anyhow!(
                            "DNS lookup failed for Guacd host {}:{}: {}",
                            host,
                            port,
                            e
                        )),
                    }
                } else {
                    Err(anyhow!("Guacd host/port not configured or supplied via ConnectAs for OpenConnection"))
                }
            }
            super::types::ActiveProtocol::PortForward => {
                // This logic is mostly for client mode PortForward. Server mode is handled above.
                if let super::core::ProtocolLogicState::PortForward(pf_state) = &self.protocol_state
                {
                    if let (Some(host), Some(port)) =
                        (pf_state.target_host.as_deref(), pf_state.target_port)
                    {
                        match tokio::net::lookup_host(format!("{host}:{port}")).await {
                            Ok(mut addrs) => {
                                if let Some(socket_addr) = addrs.next() {
                                    debug!("Channel({}): PortForward OpenConnection for target_conn_no {} to {}:{} (resolved to {}).",
                                        self.channel_id, target_connection_no, host, port, socket_addr);
                                    super::connections::open_backend(
                                        self,
                                        target_connection_no,
                                        socket_addr,
                                        super::types::ActiveProtocol::PortForward,
                                    )
                                    .await
                                } else {
                                    Err(anyhow!("Could not resolve PortForward host {}:{} to any SocketAddr", host, port))
                                }
                            }
                            Err(e) => Err(anyhow!(
                                "DNS lookup failed for PortForward host {}:{}: {}",
                                host,
                                port,
                                e
                            )),
                        }
                    } else {
                        Err(anyhow!(
                            "PortForward target host/port not configured in channel protocol state"
                        ))
                    }
                } else {
                    Err(anyhow!(
                        "Channel is in PortForward mode, but protocol state is not PortForward"
                    ))
                }
            }
            super::types::ActiveProtocol::Socks5 => {
                // SOCKS5 parsing
                if !self.server_mode && self.network_checker.is_some() {
                    // Client mode with network checker - parse SOCKS5 target from data

                    if cursor.remaining() >= CONN_NO_LEN {
                        let target_host_length = cursor.get_u32() as usize; // CONNECTION_NO_LENGTH = CONN_NO_LEN

                        if cursor.remaining() >= target_host_length + PORT_LENGTH {
                            let mut host_buffer = self.buffer_pool.acquire();
                            host_buffer.clear();
                            host_buffer.resize(target_host_length, 0);
                            cursor.copy_to_slice(&mut host_buffer[..target_host_length]);
                            let target_host = String::from_utf8(host_buffer.to_vec())
                                .map_err(|e| anyhow!("Invalid UTF-8 in SOCKS host: {}", e))?;
                            self.buffer_pool.release(host_buffer);

                            let target_port = cursor.get_u16(); // PORT_LENGTH = 2 (standard u16)

                            if let Some(ref checker) = self.network_checker {
                                // **PERFORMANCE OPTIMIZED**: Single DNS lookup + permission check
                                match checker.resolve_if_allowed(&target_host).await {
                                    Some(resolved_ips) => {
                                        // Host is allowed and resolved, check port
                                        if !checker.is_port_allowed(target_port) {
                                            error!(
                                                "SOCKS5 port {} not allowed (channel_id: {})",
                                                target_port, self.channel_id
                                            );
                                            return Err(anyhow!(
                                                "SOCKS5 port {} not allowed",
                                                target_port
                                            ));
                                        }

                                        debug!(
                                            "SOCKS5 connection allowed to {}:{} (channel_id: {})",
                                            target_host, target_port, self.channel_id
                                        );

                                        // **ZERO-ALLOCATION**: Use first resolved IP directly (no second DNS lookup)
                                        if let Some(&first_ip) = resolved_ips.first() {
                                            let socket_addr =
                                                std::net::SocketAddr::new(first_ip, target_port);
                                            super::connections::open_backend(
                                                self,
                                                target_connection_no,
                                                socket_addr,
                                                super::types::ActiveProtocol::Socks5,
                                            )
                                            .await
                                        } else {
                                            Err(anyhow!(
                                                "SOCKS5 host {} resolved to empty IP list",
                                                target_host
                                            ))
                                        }
                                    }
                                    None => {
                                        // Host was not allowed or DNS resolution failed
                                        error!("SOCKS5 destination {} not allowed or unresolvable (channel_id: {})", target_host, self.channel_id);
                                        Err(anyhow!(
                                            "SOCKS5 destination {} not allowed or unresolvable",
                                            target_host
                                        ))
                                    }
                                }
                            } else {
                                Err(anyhow!("SOCKS5 network checker not configured"))
                            }
                        } else {
                            error!(
                                "SOCKS5 payload too short for host and port data (channel_id: {})",
                                self.channel_id
                            );
                            Err(anyhow!("SOCKS5 payload too short for host and port data"))
                        }
                    } else {
                        error!(
                            "SOCKS5 payload missing host length (channel_id: {})",
                            self.channel_id
                        );
                        Err(anyhow!("SOCKS5 payload missing host length"))
                    }
                } else {
                    // Server mode or no network checker - not supported in client mode
                    warn!(            "SOCKS5 OpenConnection received but not in client mode with network checker (channel_id: {})", self.channel_id);
                    Err(anyhow!("SOCKS5 OpenConnection not supported in this mode"))
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
            temp_payload_buffer.put_u8(CloseConnectionReason::ConnectionFailed as u8);
            // Use self.send_control_message for sending
            if let Err(send_err) = self
                .send_control_message(ControlMessage::CloseConnection, &temp_payload_buffer)
                .await
            {
                error!(
                    "Channel({}): Failed to send CloseConnection for failed OpenConnection {}: {}",
                    self.channel_id, target_connection_no, send_err
                );
            }
        } else {
            debug!("Channel({}): Successfully processed OpenConnection for target_conn_no {}, sending ConnectionOpened.", self.channel_id, target_connection_no);
            temp_payload_buffer.put_u32(target_connection_no);
            if let Err(e) = self
                .send_control_message(ControlMessage::ConnectionOpened, &temp_payload_buffer)
                .await
            {
                error!(
                    "Channel({}): Error sending ConnectionOpened for conn_no {}: {}",
                    self.channel_id, target_connection_no, e
                );
            }
        }
        self.buffer_pool.release(temp_payload_buffer);
        Ok(())
    }

    /// Handle a Ping control message
    async fn handle_ping(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            // Basic ping without connection info
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Endpoint Received basic Ping request (channel_id: {})",
                    self.channel_id
                );
            }
            self.send_control_message(
                ControlMessage::Pong,
                &[0, 0, 0, 0], // connection 0
            )
            .await?;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

        // Connection 0 is a special control connection that doesn't need to exist in the connection's map
        if conn_no != 0 {
            // Validate non-control connection exists with single lookup
            if self.conns.get(&conn_no).is_none() {
                error!(
                    "Endpoint Connection {} not found for Ping (channel_id: {})",
                    conn_no, self.channel_id
                );
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
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Endpoint Received ACK request with timing data for {} (channel_id: {})",
                    conn_no, self.channel_id
                );
            }
        } else if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Endpoint Received ACK request for {} (channel_id: {})",
                conn_no, self.channel_id
            );
        }

        // Send response - using buffer pool data
        self.send_control_message(ControlMessage::Pong, &response)
            .await?;
        self.buffer_pool.release(response);

        Ok(())
    }

    /// Handle a Pong control message
    async fn handle_pong(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < CONN_NO_LEN {
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Endpoint Received basic pong (channel_id: {})",
                    self.channel_id
                );
            }
            self.ping_attempt = 0;
            return Ok(());
        }

        // Extract connection number
        let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

        // Simplified pong handling - no complex stats tracking
        if let Some(_conn_ref) = self.conns.get(&conn_no) {
            if conn_no == 0 {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Endpoint Received pong (channel_id: {})", self.channel_id);
                }
            } else if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Endpoint Received ACK response for {} (channel_id: {})",
                    conn_no, self.channel_id
                );
            }
        } else if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Endpoint Received pong for unknown connection {} (channel_id: {})",
                conn_no, self.channel_id
            );
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
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!("Endpoint Received EOF from remote for connection {}, signaling backend to shutdown write side (channel_id: {})", conn_no, self.channel_id);
            }

            // SendEOF means the remote side closed their writing end
            // Send EOF signal to the backend task which will call backend.shutdown() (perfect for RDP!)
            match conn_ref.data_tx.send(crate::models::ConnectionMessage::Eof) {
                Ok(_) => {
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Endpoint Successfully sent EOF signal to backend for conn {} (channel_id: {})", conn_no, self.channel_id);
                    }
                }
                Err(_) => {
                    // Channel is closed, connection is already dead
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Endpoint EOF signal failed, connection {} already closed (channel_id: {})", conn_no, self.channel_id);
                    }
                }
            }
        } else {
            error!(
                "Endpoint Connection for EOF {} not found (channel_id: {})",
                conn_no, self.channel_id
            );
        }

        Ok(())
    }

    /// Handle a ConnectionOpened control message
    async fn handle_connection_opened(&mut self, data: &[u8]) -> Result<()> {
        // This is called when we receive a ConnectionOpened control message from WebRTC
        // In server_mode, this completes the connection setup
        if !self.server_mode {
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Endpoint Received ConnectionOpened in client mode (ignoring) (channel_id: {})",
                    self.channel_id
                );
            }
            return Ok(());
        }

        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("ConnectionOpened message too short"));
        }

        let connection_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Endpoint Starting reader for connection {} (channel_id: {})",
                connection_no, self.channel_id
            );
        }

        // Get the connection
        if let Some(conn_ref) = self.conns.get(&connection_no) {
            // If it's a SOCKS5 connection, send a success response to the client
            if self.active_protocol == super::types::ActiveProtocol::Socks5 {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "SOCKS5 Connection opened (channel_id: {}, conn_no: {})",
                        self.channel_id, connection_no
                    );
                }

                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Endpoint Sending SOCKS5 success response to connection {} (channel_id: {})", connection_no, self.channel_id);
                }

                // Send SOCKS5 response via the connection's data channel using zero-allocation constant
                let response_bytes = bytes::Bytes::from_static(&SOCKS5_SUCCESS_RESPONSE);
                match conn_ref
                    .data_tx
                    .send(crate::models::ConnectionMessage::Data(response_bytes))
                {
                    Ok(_) => {
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            debug!("Endpoint SOCKS5 success response queued for connection {} (channel_id: {})", connection_no, self.channel_id);
                        }
                    }
                    Err(e) => {
                        error!(
                            "Endpoint Failed to queue SOCKS5 success response: {} (channel_id: {})",
                            e, self.channel_id
                        );
                    }
                }
            }

            // The conn.to_webrtc and backend tasks are already set up by the connection creation
            // to handle reading from the backend and sending to WebRTC. No need to spawn more tasks here.
            if conn_ref.to_webrtc.is_finished() {
                warn!("In ConnectionOpened, to_webrtc task was already finished. This is unexpected. (channel_id: {}, conn_no: {})", self.channel_id, connection_no);
            }

            debug!(
                "Connection fully opened and ready. (channel_id: {}, conn_no: {})",
                self.channel_id, connection_no
            );
        } else {
            error!(
                "Endpoint Connection {} not found for ConnectionOpened (channel_id: {})",
                connection_no, self.channel_id
            );
            return Ok(());
        }

        Ok(())
    }

    /// Handle a MetricsRequest control message
    async fn handle_metrics_request(&mut self, data: &[u8]) -> Result<()> {
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Processing metrics request (channel_id: {})",
                self.channel_id
            );
        }

        // Extract request type from data (if any)
        let request_type = if !data.is_empty() { data[0] } else { 0 };

        // Generate appropriate metrics response based on request type
        let response_data = match request_type {
            0 => {
                // Request for aggregated metrics
                let metrics = crate::metrics::METRICS_COLLECTOR.get_aggregated_metrics();
                serde_json::to_vec(&metrics).unwrap_or_default()
            }
            1 => {
                // Request for live stats (if connection ID provided)
                if data.len() >= 5 {
                    let conn_id_bytes = &data[1..5];
                    let conn_id = u32::from_be_bytes([
                        conn_id_bytes[0],
                        conn_id_bytes[1],
                        conn_id_bytes[2],
                        conn_id_bytes[3],
                    ]);
                    let conn_id_str = format!("{}-{}", self.channel_id, conn_id);

                    if let Some(stats) =
                        crate::metrics::METRICS_COLLECTOR.get_live_stats(&conn_id_str)
                    {
                        serde_json::to_vec(&stats).unwrap_or_default()
                    } else {
                        b"null".to_vec()
                    }
                } else {
                    b"null".to_vec()
                }
            }
            2 => {
                // Request for system stats
                let uptime = crate::metrics::METRICS_COLLECTOR.get_uptime();
                let connection_count = crate::metrics::METRICS_COLLECTOR.active_connection_count();
                let system_stats = serde_json::json!({
                    "uptime_seconds": uptime.as_secs(),
                    "active_connection_count": connection_count,
                    "channel_id": self.channel_id
                });
                serde_json::to_vec(&system_stats).unwrap_or_default()
            }
            _ => {
                // Unknown request type
                b"error: unknown request type".to_vec()
            }
        };

        // Send metrics response
        self.send_control_message(ControlMessage::MetricsResponse, &response_data)
            .await?;

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Sent metrics response (channel_id: {}, response_bytes: {})",
                self.channel_id,
                response_data.len()
            );
        }

        Ok(())
    }

    /// Handle a MetricsResponse control message
    async fn handle_metrics_response(&mut self, data: &[u8]) -> Result<()> {
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Received metrics response (channel_id: {}, response_bytes: {})",
                self.channel_id,
                data.len()
            );
        }

        // Parse and process metrics response
        if !data.is_empty() && unlikely!(crate::logger::is_verbose_logging()) {
            match serde_json::from_slice::<serde_json::Value>(data) {
                Ok(metrics) => {
                    debug!(
                        "Parsed metrics response (channel_id: {}, metrics: {:?})",
                        self.channel_id, metrics
                    );
                }
                Err(e) => {
                    debug!(
                        "Failed to parse metrics response (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Handle a MetricsConfig control message
    async fn handle_metrics_config(&mut self, data: &[u8]) -> Result<()> {
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Processing metrics config (channel_id: {}, config_bytes: {})",
                self.channel_id,
                data.len()
            );
        }

        // Parse metrics configuration
        if !data.is_empty() {
            match serde_json::from_slice::<serde_json::Value>(data) {
                Ok(config) => {
                    debug!(
                        "Received metrics configuration (channel_id: {}, config: {:?})",
                        self.channel_id, config
                    );

                    // Process configuration settings
                    if let Some(enable_collection) = config.get("enable_collection") {
                        if let Some(enabled) = enable_collection.as_bool() {
                            debug!(
                                "Metrics collection setting (channel_id: {}, enabled: {})",
                                self.channel_id, enabled
                            );
                        }
                    }

                    if let Some(collection_interval) = config.get("collection_interval_ms") {
                        if let Some(interval) = collection_interval.as_u64() {
                            debug!(
                                "Metrics collection interval (channel_id: {}, interval_ms: {})",
                                self.channel_id, interval
                            );
                        }
                    }

                    // Could update metrics collection settings here
                    // For now, just acknowledge we received the config
                }
                Err(e) => {
                    debug!(
                        "Failed to parse metrics config (channel_id: {}, error: {})",
                        self.channel_id, e
                    );
                }
            }
        }

        Ok(())
    }
}
