//! Oracle TNS protocol handler
//!
//! This handler implements the TNS connection flow with credential injection:
//! 1. Receive CONNECT from client
//! 2. Connect to Oracle and forward CONNECT (potentially modified)
//! 3. Receive ACCEPT/REFUSE/REDIRECT from server
//! 4. Forward response to client
//! 5. Handle authentication DATA packets
//! 6. Relay traffic after authentication
//!
//! # Client Configuration Requirements
//!
//! **IMPORTANT**: When using Oracle through this proxy, clients (DBeaver, SQL Developer, etc.)
//! should NOT provide both username AND password together. The proxy handles credential injection.
//!
//! ## Working Configurations:
//! - **No credentials**: Leave username and password empty. Proxy injects everything.
//! - **Username only**: Enter username, leave password empty. Proxy injects password.
//! - **Password only**: Enter password, leave username empty. (Unusual but works)
//!
//! ## Failing Configuration:
//! - **Both username AND password**: Results in ORA-17452 "OAUTH marshaling failure"
//!
//! ## Technical Explanation:
//! When a JDBC driver has both username AND password configured, it internally computes
//! O5LOGON session keys (password_hash → client_sesskey → combo_key) using those credentials.
//! The driver does NOT send these in the packets - they're computed internally for post-auth
//! encryption. The proxy cannot access or modify this internal state.
//!
//! After successful authentication:
//! - Oracle encrypts responses using combo_key derived from REAL credentials
//! - JDBC driver tries to decrypt using its internally-computed combo_key (from DUMMY credentials)
//! - Mismatch causes ORA-17452 "OAUTH marshaling failure"
//!
//! When credentials are not fully provided, the JDBC driver doesn't initialize its internal
//! crypto state, so it accepts whatever the server sends.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::timeout;

use super::super::connection_tracker::ConnectionTracker;
use super::super::session::{ManagedSession, Session, SessionManager};
use super::super::stream::NetworkStream;
use crate::auth::{AuthProvider, ConnectionContext, DatabaseType};
use crate::config::{Config, TargetWithCredentials};
use crate::error::{ProxyError, Result};
use crate::protocol::oracle::{
    build_proxy_credential_packet,
    extract_client_auth_password,
    // Client sesskey preservation (when client provides credentials)
    extract_client_auth_sesskey,
    extract_full_challenge_data,
    is_auth_failure,
    is_auth_packet,
    is_auth_response_packet,
    is_auth_success,
    packet_type,
    parse_accept,
    parse_connect,
    parse_connect_string,
    parse_header,
    parse_refuse,
    replace_auth_username,
    serialize_refuse,
    // O5LOGON crypto
    O5LogonAuth,
    RefusePacket,
    TNS_HEADER_SIZE,
};
use crate::tls::TlsConnector;

// ============================================================================
// Oracle Handler State
// ============================================================================

/// Oracle handler state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum HandlerState {
    /// Initial state - waiting for client CONNECT
    Init,
    /// Received CONNECT from client
    ReceivedClientConnect,
    /// Connected to Oracle server
    ConnectedToServer,
    /// Sent CONNECT to server
    SentConnectToServer,
    /// Received ACCEPT/REFUSE from server
    ReceivedServerResponse,
    /// In authentication phase (DATA packets)
    Authenticating,
    /// Authentication complete - relaying traffic
    Relaying,
    /// Error state
    Error,
}

// ============================================================================
// Oracle Handler
// ============================================================================

/// Oracle TNS protocol handler
pub struct OracleHandler {
    /// Client stream (Option for safe taking)
    client: Option<NetworkStream>,
    /// Client address
    client_addr: SocketAddr,
    /// Configuration
    config: Arc<Config>,
    /// Target database configuration
    target: TargetWithCredentials,
    /// Authentication provider
    auth_provider: Arc<dyn AuthProvider>,
    /// Handler state
    state: HandlerState,
    /// Shutdown signal receiver
    #[allow(dead_code)]
    shutdown_rx: broadcast::Receiver<()>,
    /// Connection tracker for session enforcement
    #[allow(dead_code)]
    connection_tracker: Option<Arc<ConnectionTracker>>,
    /// TLS connector for connecting to backend servers (always available for auto-detect)
    tls_connector: TlsConnector,
    /// Session ID for gateway mode
    session_id: Option<uuid::Uuid>,
    /// Gateway-provided session UID for tunnel grouping
    #[allow(dead_code)]
    session_uid: Option<String>,
    /// Negotiated SDU size
    negotiated_sdu: u16,
}

impl OracleHandler {
    /// Create a new Oracle handler
    pub fn new(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Self> {
        Self::with_tracker(
            client,
            client_addr,
            config,
            auth_provider,
            shutdown_rx,
            None,
        )
    }

    /// Create a new Oracle handler with connection tracker
    pub fn with_tracker(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
    ) -> Result<Self> {
        // Get the target configuration for Oracle
        let target = config
            .get_effective_target("oracle")
            .ok_or_else(|| ProxyError::Config("No Oracle target configured".to_string()))?;

        Self::with_target(
            client,
            client_addr,
            config,
            auth_provider,
            shutdown_rx,
            connection_tracker,
            target,
            None,
            None,
        )
    }

    /// Create handler with session ID and target for gateway mode
    #[allow(clippy::too_many_arguments)]
    pub fn with_session_id_and_target(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
        session_id: uuid::Uuid,
        target_host: String,
        target_port: u16,
        session_uid: Option<String>,
        tls_config: crate::tls::TlsClientConfig,
        database: Option<String>,
    ) -> Result<Self> {
        let target = TargetWithCredentials {
            host: target_host,
            port: target_port,
            database,
            tls: tls_config,
            credentials: crate::config::CredentialsConfig {
                username: String::new(),
                password: String::new(),
            },
        };

        Self::with_target(
            client,
            client_addr,
            config,
            auth_provider,
            shutdown_rx,
            connection_tracker,
            target,
            Some(session_id),
            session_uid,
        )
    }

    /// Internal constructor with explicit target
    #[allow(clippy::too_many_arguments)]
    fn with_target(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
        target: TargetWithCredentials,
        session_id: Option<uuid::Uuid>,
        session_uid: Option<String>,
    ) -> Result<Self> {
        // Always create a TLS connector for auto-detection.
        // Oracle TCPS (ADB) requires TLS, while XE/Free/EE use plain TCP.
        // The proxy auto-detects by trying TLS first and falling back to TCP.
        let tls_connector = TlsConnector::new(&Default::default()).map_err(|e| {
            error!("Failed to create TLS connector for Oracle: {}", e);
            ProxyError::from(e)
        })?;

        Ok(Self {
            client: Some(NetworkStream::tcp(client)),
            client_addr,
            config,
            target,
            auth_provider,
            state: HandlerState::Init,
            shutdown_rx,
            connection_tracker,
            tls_connector,
            session_id,
            session_uid,
            negotiated_sdu: 8192, // Default SDU
        })
    }

    /// Get mutable reference to client stream
    fn client_mut(&mut self) -> Result<&mut NetworkStream> {
        self.client
            .as_mut()
            .ok_or_else(|| ProxyError::Connection("Client stream not available".into()))
    }

    /// Take client stream for ownership transfer
    fn take_client(&mut self) -> Result<NetworkStream> {
        self.client
            .take()
            .ok_or_else(|| ProxyError::Connection("Client stream not available".into()))
    }

    /// Main handler entry point
    pub async fn handle(mut self) -> Result<()> {
        info!("Oracle handler started, waiting for client CONNECT");

        // Step 1: Receive CONNECT from client
        debug!("Waiting for client CONNECT");
        let client_connect_data = match self.receive_tns_packet().await {
            Ok(data) => {
                debug!("Received TNS packet from client: {} bytes", data.len());
                data
            }
            Err(e) => {
                error!("Failed to receive TNS packet from client: {}", e);
                return Err(e);
            }
        };

        let client_header = parse_header(&client_connect_data)?;
        if client_header.packet_type != packet_type::CONNECT {
            return Err(ProxyError::Protocol(format!(
                "Expected CONNECT (0x01), got 0x{:02X}",
                client_header.packet_type
            )));
        }

        // Some JDBC drivers send CONNECT packets where the TNS header length covers
        // only the fixed fields, with the connect string data following after.
        // Detect this and read the additional bytes if needed.
        let client_connect_data = if client_connect_data.len() >= TNS_HEADER_SIZE + 20 {
            let payload = &client_connect_data[TNS_HEADER_SIZE..];
            let connect_data_length = u16::from_be_bytes([payload[16], payload[17]]) as usize;
            let connect_data_offset = u16::from_be_bytes([payload[18], payload[19]]) as usize;
            let needed = connect_data_offset + connect_data_length;

            if needed > client_connect_data.len() && connect_data_length > 0 {
                let extra = needed - client_connect_data.len();
                debug!(
                    "CONNECT packet split: header={}, need={}, reading {} extra bytes",
                    client_connect_data.len(),
                    needed,
                    extra
                );
                let mut full_packet = client_connect_data;
                let mut extra_buf = vec![0u8; extra];
                let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);
                timeout(
                    connect_timeout,
                    self.client_mut()?.read_exact(&mut extra_buf),
                )
                .await
                .map_err(|_| ProxyError::Timeout("Reading CONNECT data extension".into()))?
                .map_err(|e| {
                    ProxyError::Connection(format!("Failed to read CONNECT data: {}", e))
                })?;
                full_packet.extend_from_slice(&extra_buf);

                // Some JDBC drivers (e.g. DBeaver) set cd_len smaller than the actual
                // connect data they send. The leftover bytes would corrupt the next read.
                // Detect this by checking if the connect string's parens are balanced.
                // If not, keep reading until they are.
                let cd_start = connect_data_offset;
                let connect_data = &full_packet[cd_start..];
                // Find start of paren structure (skip NSD preamble if present)
                let paren_start = connect_data.iter().position(|&b| b == b'(');
                if let Some(ps) = paren_start {
                    let mut depth: i32 = 0;
                    for &b in &connect_data[ps..] {
                        if b == b'(' {
                            depth += 1;
                        } else if b == b')' {
                            depth -= 1;
                        }
                    }
                    if depth > 0 {
                        // Read one byte at a time until parens balance (with timeout)
                        let mut remaining_depth = depth;
                        let mut extra_bytes = Vec::new();
                        let mut one_byte = [0u8; 1];
                        while remaining_depth > 0 {
                            timeout(
                                Duration::from_secs(5),
                                self.client_mut()?.read_exact(&mut one_byte),
                            )
                            .await
                            .map_err(|_| {
                                ProxyError::Timeout("Reading remaining CONNECT data".into())
                            })?
                            .map_err(|e| {
                                ProxyError::Connection(format!(
                                    "Failed to read remaining CONNECT data: {}",
                                    e
                                ))
                            })?;
                            extra_bytes.push(one_byte[0]);
                            if one_byte[0] == b'(' {
                                remaining_depth += 1;
                            } else if one_byte[0] == b')' {
                                remaining_depth -= 1;
                            }
                        }
                        debug!(
                            "CONNECT packet paren-balancing: read {} extra bytes",
                            extra_bytes.len()
                        );
                        full_packet.extend_from_slice(&extra_bytes);
                    }
                }

                // Update the TNS header length to reflect the full packet size
                let full_len = (full_packet.len() as u16).to_be_bytes();
                full_packet[0] = full_len[0];
                full_packet[1] = full_len[1];

                // Also update cd_len to match actual connect data
                let actual_cd_len = (full_packet.len() - connect_data_offset) as u16;
                let cd_len_offset = TNS_HEADER_SIZE + 16;
                let cd_len_bytes = actual_cd_len.to_be_bytes();
                full_packet[cd_len_offset] = cd_len_bytes[0];
                full_packet[cd_len_offset + 1] = cd_len_bytes[1];

                full_packet
            } else {
                client_connect_data
            }
        } else {
            client_connect_data
        };

        let client_connect = parse_connect(&client_connect_data)
            .map_err(|e| ProxyError::Protocol(format!("Failed to parse client CONNECT: {}", e)))?;

        debug!(
            "Client CONNECT: version={}, SDU={}, connect_data_len={}",
            client_connect.version,
            client_connect.session_data_unit_size,
            client_connect.connect_data_length
        );

        // Parse the connect string
        let connect_parts = parse_connect_string(&client_connect.connect_data)
            .map_err(|e| ProxyError::Protocol(format!("Failed to parse connect string: {}", e)))?;

        info!(
            "Oracle client {} connecting to service {:?} (host: {:?}, port: {:?})",
            self.client_addr,
            connect_parts.service_identifier(),
            connect_parts.host,
            connect_parts.port
        );

        self.state = HandlerState::ReceivedClientConnect;

        // Step 2: Connect to Oracle server
        let mut server = match self.connect_to_server().await {
            Ok(s) => s,
            Err(e) => {
                return Err(e);
            }
        };
        self.state = HandlerState::ConnectedToServer;

        // Step 3: Forward CONNECT to server (potentially modified)
        debug!("Forwarding CONNECT to server");

        // Get session config FIRST (before get_auth, which consumes credentials)
        let context = self.build_connection_context();
        let session_config = self
            .auth_provider
            .get_session_config(&context)
            .await
            .unwrap_or_else(|_| crate::auth::SessionConfig::default());

        // Get credentials for user injection (this consumes credentials from store)
        let auth_result = self.auth_provider.get_auth(&context).await;

        // Modify CONNECT packet to inject the real username
        let connect_to_send = if let Ok(ref creds) = auth_result {
            let real_username = creds.method.username();
            debug!("Injecting username '{}' into CONNECT packet", real_username);

            // Modify the raw packet bytes directly to avoid UTF-8 round-trip corruption.
            // Connect data may contain binary bytes (e.g. CONNECTION_ID) that would be
            // corrupted by String::from_utf8_lossy -> modify -> as_bytes().
            let offset = client_connect.connect_data_offset as usize;
            let length = client_connect.connect_data_length as usize;

            if let Some(modified_packet) = crate::protocol::oracle::modify_connect_packet_user(
                &client_connect_data,
                offset,
                length,
                real_username,
            ) {
                modified_packet
            } else {
                // Fallback: use String-based modification (for Easy Connect or missing CID)
                debug!("Raw byte modification failed, using string-based fallback");
                let modified_connect_string = crate::protocol::oracle::modify_connect_string_user(
                    &client_connect.connect_data,
                    real_username,
                );

                let raw_connect_data_len = client_connect.connect_data_length as usize;
                let modified_connect_bytes = modified_connect_string.as_bytes();
                let original_end = offset + raw_connect_data_len;

                let len_diff =
                    modified_connect_bytes.len() as isize - raw_connect_data_len as isize;
                let new_total_len = (client_connect_data.len() as isize + len_diff) as usize;

                let mut packet = Vec::with_capacity(new_total_len);
                packet.extend_from_slice(&client_connect_data[..offset]);
                packet.extend_from_slice(modified_connect_bytes);
                if original_end < client_connect_data.len() {
                    packet.extend_from_slice(&client_connect_data[original_end..]);
                }

                let new_len_bytes = (new_total_len as u16).to_be_bytes();
                packet[0] = new_len_bytes[0];
                packet[1] = new_len_bytes[1];

                let connect_data_len_offset = 8 + 16;
                let connect_data_len_bytes = (modified_connect_bytes.len() as u16).to_be_bytes();
                packet[connect_data_len_offset] = connect_data_len_bytes[0];
                packet[connect_data_len_offset + 1] = connect_data_len_bytes[1];

                packet
            }
        } else {
            debug!("No credentials available, forwarding original CONNECT");
            client_connect_data.clone()
        };

        // Modify ADDRESS section to match actual target (needed when proxying to a
        // different host than what the client connected to, e.g. localhost -> ADB)
        let connect_to_send = {
            let target_protocol = if server.is_encrypted() { "TCPS" } else { "TCP" };

            if let Some(modified) = crate::protocol::oracle::modify_connect_packet_address(
                &connect_to_send,
                &self.target.host,
                self.target.port,
                target_protocol,
                false, // don't add SECURITY section; ssl_server_dn_match is TLS-layer
            ) {
                modified
            } else {
                debug!("ADDRESS modification skipped (no ADDRESS section found)");
                connect_to_send
            }
        };

        // Rewrite SERVICE_NAME/SID to match the target database (needed when
        // ephemeral user is created in a PDB but client connects to CDB).
        // Skip rewrite if the client's service name already contains the target
        // database name (e.g., ATP service names like
        // "dbname_high.adb.oraclecloud.com" already route correctly).
        let connect_to_send = if let Some(ref db) = self.target.database {
            let existing_service = connect_parts.service_identifier().unwrap_or_default();
            if existing_service.to_uppercase().contains(&db.to_uppercase()) {
                debug!(
                    "SERVICE_NAME '{}' already contains target database '{}', skipping rewrite",
                    existing_service, db
                );
                connect_to_send
            } else if let Some(modified) =
                crate::protocol::oracle::modify_connect_packet_service_name(&connect_to_send, db)
            {
                debug!("Rewrote SERVICE_NAME/SID to '{}'", db);
                modified
            } else {
                debug!("SERVICE_NAME/SID rewrite skipped (not found in CONNECT_DATA)");
                connect_to_send
            }
        } else {
            connect_to_send
        };

        // Strip CID and CONNECTION_ID sections for TLS connections.
        // Oracle ADB over TCPS has a ~304 byte CONNECT packet size limit.
        // CID/CONNECTION_ID are informational only; auth happens in O5LOGON.
        let is_tcps = server.is_encrypted();
        let connect_to_send = if is_tcps {
            if let Some(stripped) =
                crate::protocol::oracle::strip_connect_packet_cid_and_connid(&connect_to_send)
            {
                stripped
            } else {
                connect_to_send
            }
        } else {
            connect_to_send
        };

        // Strip NSD (Native Service Data) preamble for TCPS connections.
        // Oracle ADB's TCPS listener treats NSD bytes as part of the connect descriptor,
        // causing ERR=1153. TLS handles encryption/auth so NSD is not needed.
        let connect_to_send = if is_tcps {
            if let Some(stripped) =
                crate::protocol::oracle::strip_connect_packet_nsd_preamble(&connect_to_send)
            {
                stripped
            } else {
                connect_to_send
            }
        } else {
            connect_to_send
        };

        // Send CONNECT packet to server
        server.write_all(&connect_to_send).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to send CONNECT to server: {}", e))
        })?;
        server.flush().await.map_err(|e| {
            ProxyError::Connection(format!("Failed to flush CONNECT to server: {}", e))
        })?;
        self.state = HandlerState::SentConnectToServer;

        // Step 4: Receive server response (ACCEPT, REFUSE, or REDIRECT)
        debug!("Waiting for server response");
        let server_response = match Self::receive_tns_packet_from(&mut server).await {
            Ok(data) => data,
            Err(e) => {
                return Err(e);
            }
        };

        let server_header = parse_header(&server_response)?;
        debug!(
            "Server response: {} (type=0x{:02X})",
            server_header.type_name(),
            server_header.packet_type
        );

        self.state = HandlerState::ReceivedServerResponse;

        match server_header.packet_type {
            packet_type::ACCEPT => {
                let accept = parse_accept(&server_response)
                    .map_err(|e| ProxyError::Protocol(format!("Failed to parse ACCEPT: {}", e)))?;

                self.negotiated_sdu = if accept.session_data_unit_size > 0 {
                    accept.session_data_unit_size
                } else {
                    8192
                };
                info!(
                    "Oracle server accepted connection (version={}, SDU={})",
                    accept.version, self.negotiated_sdu
                );

                // Forward ACCEPT to client
                self.client_mut()?
                    .write_all(&server_response)
                    .await
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to send ACCEPT to client: {}", e))
                    })?;
                self.client_mut()?.flush().await?;

                // Handle authentication phase with credential injection
                // Pass auth_result so we don't look up credentials twice (they're consumed on first lookup)
                self.handle_post_accept(server, auth_result, session_config.clone())
                    .await
            }

            packet_type::REFUSE => {
                let refuse = parse_refuse(&server_response)
                    .map_err(|e| ProxyError::Protocol(format!("Failed to parse REFUSE: {}", e)))?;

                warn!(
                    "Oracle server refused connection: user_reason={}, system_reason={}, msg={}",
                    refuse.user_reason, refuse.system_reason, refuse.refuse_data
                );

                // Forward REFUSE to client
                self.client_mut()?
                    .write_all(&server_response)
                    .await
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to send REFUSE to client: {}", e))
                    })?;

                Err(ProxyError::Auth(format!(
                    "Oracle connection refused: {}",
                    refuse.refuse_data
                )))
            }

            packet_type::REDIRECT => {
                // TODO: Handle REDIRECT in future story
                warn!("Oracle server sent REDIRECT - not yet implemented");

                // Forward REDIRECT to client
                self.client_mut()?
                    .write_all(&server_response)
                    .await
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to send REDIRECT to client: {}", e))
                    })?;

                Err(ProxyError::Protocol(
                    "Oracle REDIRECT not yet supported".into(),
                ))
            }

            packet_type::RESEND => {
                // Server is asking us to resend the CONNECT packet.
                // For Oracle ADB over TCPS, the load balancer/SCAN listener:
                //   1. Accepts the initial TLS + CONNECT to route the connection
                //   2. Sends RESEND with TLS_RENEG flag (byte 5 = 0x08)
                //   3. Expects the client to perform a NEW TLS handshake on the SAME TCP socket
                //   4. Then resend the CONNECT on the fresh TLS session
                // This is how oracledb's thin driver handles it (transport.renegotiate_tls).
                debug!("Performing TLS renegotiation after RESEND");

                // Extract the raw TCP stream from the TLS wrapper
                let mut server = if server.is_encrypted() {
                    let tcp_stream = server.into_client_tls_tcp().map_err(|_| {
                        ProxyError::Protocol("RESEND: failed to extract TCP from TLS stream".into())
                    })?;

                    // Perform a fresh TLS handshake on the same TCP connection
                    let tls_stream = self
                        .tls_connector
                        .connect(tcp_stream, &self.target.host)
                        .await
                        .map_err(|e| {
                            ProxyError::Connection(format!(
                                "TLS renegotiation after RESEND failed: {}",
                                e
                            ))
                        })?;
                    NetworkStream::ClientTls(Box::new(tls_stream))
                } else {
                    // Plain TCP — just resend on the same connection
                    server
                };

                server.write_all(&connect_to_send).await.map_err(|e| {
                    ProxyError::Connection(format!("Failed to resend CONNECT to server: {}", e))
                })?;
                server.flush().await.map_err(|e| {
                    ProxyError::Connection(format!("Failed to flush resent CONNECT: {}", e))
                })?;

                // Wait for response on the renegotiated TLS session
                let server_response2 = Self::receive_tns_packet_from(&mut server).await?;
                let server_header2 = parse_header(&server_response2)?;
                debug!(
                    "Server response after RESEND: {} (type=0x{:02X})",
                    server_header2.type_name(),
                    server_header2.packet_type
                );

                match server_header2.packet_type {
                    packet_type::ACCEPT => {
                        let accept = parse_accept(&server_response2)?;
                        self.negotiated_sdu = if accept.session_data_unit_size > 0 {
                            accept.session_data_unit_size
                        } else {
                            8192 // Default if server doesn't specify
                        };
                        info!(
                            "Oracle server accepted connection after RESEND (version={}, SDU={}, TDU={})",
                            accept.version,
                            accept.session_data_unit_size,
                            accept.maximum_transmission_data_unit_size
                        );
                        self.client_mut()?.write_all(&server_response2).await?;
                        self.client_mut()?.flush().await?;

                        // Handle authentication phase with credential injection
                        // Pass auth_result so we don't look up credentials twice (they're consumed on first lookup)
                        self.handle_post_accept(server, auth_result, session_config.clone())
                            .await
                    }
                    packet_type::REFUSE => {
                        let refuse = parse_refuse(&server_response2)?;
                        warn!("Oracle server refused after RESEND: {}", refuse.refuse_data);
                        self.client_mut()?.write_all(&server_response2).await?;
                        Err(ProxyError::Auth(format!(
                            "Oracle refused: {}",
                            refuse.refuse_data
                        )))
                    }
                    _ => Err(ProxyError::Protocol(format!(
                        "Unexpected response after RESEND: 0x{:02X}",
                        server_header2.packet_type
                    ))),
                }
            }

            _ => {
                warn!(
                    "Unexpected server response type: 0x{:02X}",
                    server_header.packet_type
                );
                Err(ProxyError::Protocol(format!(
                    "Unexpected server response: 0x{:02X}",
                    server_header.packet_type
                )))
            }
        }
    }

    /// Handle connection after ACCEPT received
    ///
    /// Takes the auth_result from the initial lookup in handle() since credentials
    /// are consumed on first lookup from the HandshakeAuthProvider.
    ///
    /// This method implements O5LOGON credential injection:
    /// 1. Intercepts client's initial auth request
    /// 2. Captures server's challenge (AUTH_SESSKEY, AUTH_VFR_DATA)
    /// 3. When client sends auth response, computes correct response using real password
    /// 4. Forwards computed response to server
    async fn handle_post_accept(
        mut self,
        mut server: NetworkStream,
        auth_result: Result<crate::auth::AuthCredentials>,
        session_config: crate::auth::SessionConfig,
    ) -> Result<()> {
        self.state = HandlerState::Authenticating;

        // Extract username and password from the auth_result passed from handle()
        let (real_username, real_password) = match auth_result.as_ref() {
            Ok(creds) => {
                let username = creds.method.username().to_string();
                let password = creds.method.password().map(|p| p.to_string());
                (Some(username), password)
            }
            Err(_) => (None, None),
        };

        debug!(
            "Starting Oracle authentication phase (has_username: {}, has_password: {})",
            real_username.is_some(),
            real_password.is_some()
        );

        // Create O5LOGON auth context if we have credentials
        let mut o5logon_auth = match (&real_username, &real_password) {
            (Some(user), Some(pass)) => {
                debug!("O5LOGON: Created auth context for credential injection");
                Some(O5LogonAuth::new(user, pass))
            }
            _ => {
                debug!("O5LOGON: No credentials available, will relay auth packets as-is");
                None
            }
        };

        // Authentication state tracking
        let mut auth_complete = false;
        let max_auth_rounds = 20;
        let mut round = 0;
        let mut challenge_received = false;
        // Track if we've rebuilt a no-username packet - if so, we handle auth entirely ourselves
        let mut proxy_auth_mode = false;
        // Store the original no-username packet's AUTH fields for building credential packet
        let mut original_no_username_payload: Option<Vec<u8>> = None;

        // Handle authentication packets (native format after ACCEPT)
        while !auth_complete && round < max_auth_rounds {
            round += 1;

            // Receive packet from client (handles both standard and native format)
            let client_packet = self.receive_tns_packet().await?;

            // Extract payload based on packet format
            let (packet_type, payload) = Self::extract_packet_payload(&client_packet);

            // Only process DATA packets (type 0x06)
            if packet_type != packet_type::DATA {
                server.write_all(&client_packet).await.map_err(|e| {
                    ProxyError::Connection(format!("Failed to forward packet to server: {}", e))
                })?;

                if packet_type == packet_type::ABORT {
                    return Err(ProxyError::Connection("Client aborted connection".into()));
                }
                continue;
            }

            // Check if this is an auth packet and if we need to modify it
            let is_auth = is_auth_packet(&payload);
            let is_response = is_auth_response_packet(&payload);

            let packet_to_send = if is_auth {
                // Check if this is Round 4 (has challenge, client sending session data without password)
                // We need to build a complete credential packet with AUTH_SESSKEY and AUTH_PASSWORD
                debug!(
                    "Auth round {}: is_auth={}, is_response={}, challenge_received={}, proxy_auth_mode={}",
                    round, is_auth, is_response, challenge_received, proxy_auth_mode
                );
                if challenge_received && !is_response {
                    // Client sent auth packet but NO AUTH_PASSWORD - build credential packet
                    if let Some(ref mut auth_ctx) = o5logon_auth {
                        match auth_ctx.process_challenge() {
                            Ok(response_data) => {
                                // Use build_proxy_credential_packet to create a properly formatted
                                // credential packet (same approach as proxy auth mode)
                                let modified_payload = build_proxy_credential_packet(
                                    &payload,
                                    &response_data.username,
                                    &response_data,
                                );
                                Self::rebuild_native_packet(&client_packet, &modified_payload)
                            }
                            Err(e) => {
                                warn!("O5LOGON: Failed to compute auth response: {}", e);
                                client_packet.clone()
                            }
                        }
                    } else {
                        client_packet.clone()
                    }
                } else if is_response && challenge_received {
                    // Client sent credentials (AUTH_PASSWORD, AUTH_SESSKEY).
                    //
                    // To work with JDBC's internal verification, we must use the CLIENT's
                    // session key, not generate our own. Otherwise JDBC can't verify the
                    // server's response (ORA-17452).
                    //
                    // Strategy:
                    // 1. Extract client's AUTH_SESSKEY from their packet
                    // 2. Decrypt it using our password_hash to get their raw sesskey
                    // 3. Use their sesskey for our computations
                    // 4. Keep their original AUTH_SESSKEY, replace only AUTH_PASSWORD and SPEEDY_KEY
                    if let Some(ref mut auth_ctx) = o5logon_auth {
                        // CRITICAL: Compute password_hash BEFORE trying to decrypt client's sesskey
                        // Otherwise we'll use the wrong key (legacy SHA-1 instead of PBKDF2)
                        auth_ctx.ensure_password_hash_computed();

                        // Extract client's AUTH_SESSKEY
                        if let Some(encrypted_client_sesskey) =
                            extract_client_auth_sesskey(&payload)
                        {
                            // Decrypt it to get their raw sesskey
                            match auth_ctx
                                .decrypt_external_client_sesskey(&encrypted_client_sesskey)
                            {
                                Ok(client_raw_sesskey) => {
                                    // Use client's sesskey
                                    auth_ctx.set_client_sesskey(client_raw_sesskey);

                                    // Process challenge to compute combo_key (needed for password verification)
                                    match auth_ctx.process_challenge() {
                                        Ok(_response_data) => {
                                            // Now check if client's AUTH_PASSWORD matches the real password
                                            // If so, forward the packet UNCHANGED to avoid JDBC verification issues
                                            if let Some(encrypted_auth_password) =
                                                extract_client_auth_password(&payload)
                                            {
                                                if auth_ctx.verify_client_auth_password(
                                                    &encrypted_auth_password,
                                                ) {
                                                    client_packet.clone()
                                                } else {
                                                    // Wrong password provided - replace with real credentials
                                                    // This results in ORA-17452 (JDBC verification failure) but at least
                                                    // it's a clear error. Check proxy logs for the real reason.
                                                    warn!("Oracle proxy: Wrong password provided - will result in ORA-17452. Use correct password or leave blank for proxy injection.");

                                                    // Build fresh credentials packet with real password
                                                    match auth_ctx.process_challenge() {
                                                        Ok(response_data) => {
                                                            let cred_payload =
                                                                build_proxy_credential_packet(
                                                                    &payload,
                                                                    &auth_ctx.username,
                                                                    &response_data,
                                                                );
                                                            Self::rebuild_native_packet(
                                                                &client_packet,
                                                                &cred_payload,
                                                            )
                                                        }
                                                        Err(_e) => client_packet.clone(),
                                                    }
                                                }
                                            } else {
                                                client_packet.clone()
                                            }
                                        }
                                        Err(_e) => client_packet.clone(),
                                    }
                                }
                                Err(_e) => {
                                    // Fall back to fresh packet (will fail JDBC verification but auth might succeed)
                                    match auth_ctx.process_challenge() {
                                        Ok(response_data) => {
                                            let cred_payload = build_proxy_credential_packet(
                                                &payload,
                                                &auth_ctx.username,
                                                &response_data,
                                            );
                                            Self::rebuild_native_packet(
                                                &client_packet,
                                                &cred_payload,
                                            )
                                        }
                                        Err(_e2) => client_packet.clone(),
                                    }
                                }
                            }
                        } else {
                            match auth_ctx.process_challenge() {
                                Ok(response_data) => {
                                    let cred_payload = build_proxy_credential_packet(
                                        &payload,
                                        &auth_ctx.username,
                                        &response_data,
                                    );
                                    Self::rebuild_native_packet(&client_packet, &cred_payload)
                                }
                                Err(_e) => client_packet.clone(),
                            }
                        }
                    } else {
                        client_packet.clone()
                    }
                } else {
                    // Initial auth request (before challenge) - replace username but don't inject credentials
                    // This is Auth round 3: client sends username, we need to replace it with real username
                    if let Some(ref auth_ctx) = o5logon_auth {
                        // Check if this is a no-username packet (0x73 with 0x80 marker)
                        // If so, we need to handle the entire auth flow ourselves
                        // The 0x80 marker position varies by Oracle version:
                        //   18c XE: byte 8 (same as 21c)
                        //   21c XE: byte 8
                        //   23ai:   byte 9
                        let is_no_username_packet = payload.len() > 9
                            && payload[3] == 0x73
                            && (payload[8] == 0x80 || payload[9] == 0x80);

                        if payload.len() > 14 {
                            debug!(
                                "Auth packet analysis: func=0x{:02X}, bytes[4..10]=[{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}], \
                                 is_no_username={}, payload_len={}",
                                payload[3], payload[4], payload[5], payload[6], payload[7], payload[8], payload[9],
                                is_no_username_packet, payload.len()
                            );
                        }

                        if is_no_username_packet {
                            debug!("Entering proxy_auth_mode: no-username packet detected");
                            proxy_auth_mode = true;
                            // Save the original payload for building the credential packet later
                            original_no_username_payload = Some(payload.clone());
                        }

                        let modified_payload = replace_auth_username(&payload, &auth_ctx.username);
                        if modified_payload.len() != payload.len() {
                            // Payload size changed, need to rebuild packet
                            Self::rebuild_native_packet(&client_packet, &modified_payload)
                        } else {
                            // Same size, can use modified payload in original packet
                            let mut modified_packet = client_packet.clone();
                            modified_packet[8..8 + modified_payload.len()]
                                .copy_from_slice(&modified_payload);
                            modified_packet
                        }
                    } else {
                        // No auth context, forward as-is
                        client_packet.clone()
                    }
                }
            } else {
                // Not an auth packet, forward as-is
                client_packet.clone()
            };

            // Forward to server

            server.write_all(&packet_to_send).await.map_err(|e| {
                ProxyError::Connection(format!("Failed to send packet to server: {}", e))
            })?;

            // Receive response from server
            let server_packet = Self::receive_tns_packet_from(&mut server).await?;
            let (server_packet_type, server_payload) = Self::extract_packet_payload(&server_packet);

            // Process server response for challenge data
            if server_packet_type == packet_type::DATA {
                // Check for challenge data (AUTH_SESSKEY + AUTH_VFR_DATA + optional PBKDF2 params)
                if let Some(challenge_data) = extract_full_challenge_data(&server_payload) {
                    if let Some(ref mut auth_ctx) = o5logon_auth {
                        if let Some(pbkdf2) = challenge_data.pbkdf2_params {
                            auth_ctx.set_challenge_pbkdf2(
                                challenge_data.auth_sesskey,
                                challenge_data.auth_vfr_data,
                                pbkdf2.csk_salt,
                                pbkdf2.vgen_count,
                                pbkdf2.sder_count,
                            );
                        } else {
                            auth_ctx.set_challenge(
                                challenge_data.auth_sesskey,
                                challenge_data.auth_vfr_data,
                            );
                        }
                        challenge_received = true;

                        // If we're in proxy_auth_mode, handle the auth entirely ourselves
                        // Don't forward the challenge to the client - they didn't initiate it
                        if proxy_auth_mode {
                            // Compute credentials
                            match auth_ctx.process_challenge() {
                                Ok(response_data) => {
                                    // Build the credential packet using proper 0x73 format
                                    let cred_payload = if let Some(ref orig_payload) =
                                        original_no_username_payload
                                    {
                                        // Use new function that builds proper credential packet structure
                                        build_proxy_credential_packet(
                                            orig_payload,
                                            &auth_ctx.username,
                                            &response_data,
                                        )
                                    } else {
                                        // Fallback: build minimal credential packet
                                        Self::build_minimal_credential_packet(
                                            auth_ctx.username.clone(),
                                            &response_data,
                                        )
                                    };

                                    // Build the credential packet in the SAME format as the server's challenge response.
                                    // Oracle 18c XE may use standard TNS format (2-byte length) while 21c+ uses native format (4-byte length).
                                    // Using rebuild_native_packet with the server_packet as template auto-detects the correct format.
                                    let cred_packet =
                                        Self::rebuild_native_packet(&server_packet, &cred_payload);

                                    // Send credentials to server
                                    server.write_all(&cred_packet).await.map_err(|e| {
                                        ProxyError::Connection(format!(
                                            "Failed to send credentials to server: {}",
                                            e
                                        ))
                                    })?;

                                    // Receive server response (should be auth success or failure)
                                    let auth_response = match tokio::time::timeout(
                                        Duration::from_secs(30),
                                        Self::receive_tns_packet_from(&mut server),
                                    )
                                    .await
                                    {
                                        Ok(Ok(resp)) => resp,
                                        Ok(Err(e)) => return Err(e),
                                        Err(_) => {
                                            return Err(ProxyError::Timeout(
                                                "Waiting for auth response from Oracle".into(),
                                            ))
                                        }
                                    };
                                    let (mut auth_resp_type, mut auth_resp_payload) =
                                        Self::extract_packet_payload(&auth_response);

                                    // Handle MARKER packets (Oracle 18c sends break markers before error responses)
                                    // TNS MARKER protocol: receive break (0x01) → send reset (0x02) → loop until DATA
                                    let mut auth_response = auth_response;
                                    let mut marker_count = 0;
                                    while auth_resp_type == packet_type::MARKER && marker_count < 5
                                    {
                                        marker_count += 1;
                                        debug!("Oracle auth: received MARKER packet #{}, sending reset", marker_count);

                                        // Build reset marker packet in same format as received
                                        let reset_marker =
                                            Self::build_marker_packet(&auth_response, 0x02);
                                        server.write_all(&reset_marker).await.map_err(|e| {
                                            ProxyError::Connection(format!(
                                                "Failed to send reset marker: {}",
                                                e
                                            ))
                                        })?;

                                        // Read next response
                                        auth_response = match tokio::time::timeout(
                                            Duration::from_secs(30),
                                            Self::receive_tns_packet_from(&mut server),
                                        )
                                        .await
                                        {
                                            Ok(Ok(resp)) => resp,
                                            Ok(Err(e)) => return Err(e),
                                            Err(_) => {
                                                return Err(ProxyError::Timeout(
                                                    "Waiting for post-MARKER auth response".into(),
                                                ))
                                            }
                                        };
                                        let extracted =
                                            Self::extract_packet_payload(&auth_response);
                                        auth_resp_type = extracted.0;
                                        auth_resp_payload = extracted.1;
                                    }

                                    if auth_resp_type == packet_type::DATA
                                        && is_auth_success(&auth_resp_payload)
                                    {
                                        info!("Oracle proxy authentication successful");

                                        // Forward success response to client
                                        self.client_mut()?
                                            .write_all(&auth_response)
                                            .await
                                            .map_err(|e| {
                                                ProxyError::Connection(format!(
                                                    "Failed to forward auth success to client: {}",
                                                    e
                                                ))
                                            })?;

                                        auth_complete = true;
                                        // Exit the auth loop - we're done
                                        break;
                                    } else if auth_resp_type == packet_type::DATA
                                        && is_auth_failure(&auth_resp_payload)
                                    {
                                        warn!("Oracle proxy authentication failed");
                                        // Forward failure to client
                                        self.client_mut()?
                                            .write_all(&auth_response)
                                            .await
                                            .map_err(|e| {
                                                ProxyError::Connection(format!(
                                                    "Failed to forward auth failure to client: {}",
                                                    e
                                                ))
                                            })?;
                                        return Err(ProxyError::Auth(
                                            "Oracle proxy authentication failed".into(),
                                        ));
                                    } else {
                                        // Unexpected response, forward it and continue
                                        self.client_mut()?
                                            .write_all(&auth_response)
                                            .await
                                            .map_err(|e| {
                                                ProxyError::Connection(format!(
                                                    "Failed to forward response to client: {}",
                                                    e
                                                ))
                                            })?;
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "O5LOGON PROXY MODE: Failed to compute credentials: {}",
                                        e
                                    );
                                    // Forward challenge to client as fallback
                                    self.client_mut()?.write_all(&server_packet).await.map_err(
                                        |e| {
                                            ProxyError::Connection(format!(
                                                "Failed to forward challenge to client: {}",
                                                e
                                            ))
                                        },
                                    )?;
                                    proxy_auth_mode = false; // Disable proxy mode, let client handle it
                                }
                            }
                            // Skip the normal forwarding below since we handled it
                            continue;
                        }
                    }
                }

                // Check for auth success/failure (normal mode)
                if is_auth_success(&server_payload) {
                    info!("Oracle authentication successful");
                    auth_complete = true;
                } else if is_auth_failure(&server_payload) {
                    warn!("Oracle authentication failed");
                    self.client_mut()?
                        .write_all(&server_packet)
                        .await
                        .map_err(|e| {
                            ProxyError::Connection(format!(
                                "Failed to forward auth failure to client: {}",
                                e
                            ))
                        })?;
                    return Err(ProxyError::Auth("Oracle authentication failed".into()));
                }
            } else if server_packet_type == packet_type::REFUSE {
                self.client_mut()?
                    .write_all(&server_packet)
                    .await
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to forward refuse to client: {}", e))
                    })?;
                return Err(ProxyError::Auth(
                    "Oracle refused during authentication".into(),
                ));
            }

            // Forward server response to client (normal mode)
            self.client_mut()?
                .write_all(&server_packet)
                .await
                .map_err(|e| {
                    ProxyError::Connection(format!("Failed to forward response to client: {}", e))
                })?;
        }

        if !auth_complete {
            warn!(
                "Oracle authentication did not complete within {} rounds",
                max_auth_rounds
            );
        }

        // Start relay phase
        self.state = HandlerState::Relaying;
        info!("Oracle authentication phase complete, starting relay");

        let client_stream = self.take_client()?;
        let server_stream = server;

        let mut session_manager = SessionManager::new(session_config.clone());
        session_manager.start_timers();

        let has_limits = session_config.max_duration.is_some()
            || session_config.idle_timeout.is_some()
            || session_config.max_queries.is_some();

        if has_limits {
            let managed = ManagedSession::with_network_server(
                client_stream,
                server_stream,
                session_manager,
                self.config.server.idle_timeout_secs,
            );
            let reason = managed.relay().await;
            info!("Oracle session ended: {:?}", reason);
        } else {
            let session = Session::with_network_streams(
                client_stream,
                server_stream,
                self.config.server.idle_timeout_secs,
            );
            let result = session.relay().await;
            result?;
            info!("Oracle session ended normally");
        }

        Ok(())
    }

    /// Extract payload from a packet (handles both standard TNS and native format)
    /// Returns (packet_type, payload_bytes)
    fn extract_packet_payload(packet: &[u8]) -> (u8, Vec<u8>) {
        if packet.len() < 8 {
            return (0, Vec::new());
        }

        let two_byte_len = u16::from_be_bytes([packet[0], packet[1]]);

        if two_byte_len == 0 {
            // Native format: [len4][type][flags][pad][pad][payload...]
            let packet_type = packet[4];
            let payload = if packet.len() > 8 {
                packet[8..].to_vec()
            } else {
                Vec::new()
            };
            (packet_type, payload)
        } else {
            // Standard TNS format: [len2][checksum2][type][flags][hdr_checksum2][payload...]
            let packet_type = packet[4];
            let payload = if packet.len() > 8 {
                packet[8..].to_vec()
            } else {
                Vec::new()
            };
            (packet_type, payload)
        }
    }

    /// Rebuild a native format packet with modified payload
    fn rebuild_native_packet(original: &[u8], new_payload: &[u8]) -> Vec<u8> {
        if original.len() < 8 {
            return original.to_vec();
        }

        let two_byte_len = u16::from_be_bytes([original[0], original[1]]);

        if two_byte_len == 0 {
            // Native format - rebuild with new payload
            let new_len = (8 + new_payload.len()) as u32;
            let mut packet = Vec::with_capacity(new_len as usize);

            // 4-byte length (big-endian)
            packet.extend_from_slice(&new_len.to_be_bytes());
            // packet type and flags (bytes 4-7 from original)
            packet.extend_from_slice(&original[4..8]);
            // new payload
            packet.extend_from_slice(new_payload);

            packet
        } else {
            // Standard TNS format - rebuild with new payload
            let new_len = (8 + new_payload.len()) as u16;
            let mut packet = Vec::with_capacity(new_len as usize);

            // 2-byte length (big-endian)
            packet.extend_from_slice(&new_len.to_be_bytes());
            // checksum, type, flags, header checksum (bytes 2-7 from original)
            packet.extend_from_slice(&original[2..8]);
            // new payload
            packet.extend_from_slice(new_payload);

            packet
        }
    }

    /// Build a MARKER packet (reset or break) in the same format as the received marker.
    /// Oracle TNS protocol: after receiving a break marker (0x01), send reset marker (0x02).
    fn build_marker_packet(received_marker: &[u8], marker_type: u8) -> Vec<u8> {
        let is_native = received_marker.len() >= 2
            && u16::from_be_bytes([received_marker[0], received_marker[1]]) == 0;

        if is_native {
            // Native format: [len4][type=0x0C][flags][pad][pad][marker_type, 0x00, marker_type]
            let total_len: u32 = 11; // 8 header + 3 payload
            let mut packet = Vec::with_capacity(total_len as usize);
            packet.extend_from_slice(&total_len.to_be_bytes());
            packet.push(packet_type::MARKER); // 0x0C
            packet.push(0x00); // flags
            packet.push(0x00);
            packet.push(0x00);
            packet.push(marker_type);
            packet.push(0x00);
            packet.push(marker_type);
            packet
        } else {
            // Standard TNS format: [len2][checksum2][type=0x0C][flags][hdr_checksum2][marker_type, 0x00, marker_type]
            let total_len: u16 = 11; // 8 header + 3 payload
            let mut packet = Vec::with_capacity(total_len as usize);
            packet.extend_from_slice(&total_len.to_be_bytes());
            packet.push(0x00); // checksum hi
            packet.push(0x00); // checksum lo
            packet.push(packet_type::MARKER); // 0x0C
            packet.push(0x00); // flags
            packet.push(0x00); // header checksum hi
            packet.push(0x00); // header checksum lo
            packet.push(marker_type);
            packet.push(0x00);
            packet.push(marker_type);
            packet
        }
    }

    /// Build a native-format DATA packet from payload
    #[allow(dead_code)]
    fn build_native_data_packet(payload: &[u8]) -> Vec<u8> {
        let total_len = (8 + payload.len()) as u32;
        let mut packet = Vec::with_capacity(total_len as usize);

        // 4-byte length (big-endian)
        packet.extend_from_slice(&total_len.to_be_bytes());
        // packet type = DATA (0x06)
        packet.push(0x06);
        // flags = 0x00
        packet.push(0x00);
        // padding
        packet.push(0x00);
        packet.push(0x00);
        // payload
        packet.extend_from_slice(payload);

        packet
    }

    /// Build a minimal credential packet with AUTH_SESSKEY and AUTH_PASSWORD
    /// Used when we don't have the original no-username packet to use as template
    fn build_minimal_credential_packet(
        username: String,
        response_data: &crate::protocol::oracle::AuthResponseData,
    ) -> Vec<u8> {
        let mut payload = Vec::with_capacity(500);

        // Build TTI header for auth packet (function code 0x73)
        // Format: 00 00 03 73 02 01 01 <username_len> 02 01 01 01 01 <field_count> 01 01 <username>
        let username_bytes = username.as_bytes();
        let field_count: u8 = 3; // AUTH_SESSKEY, AUTH_PASSWORD, AUTH_PBKDF2_SPEEDY_KEY

        payload.extend_from_slice(&[0x00, 0x00, 0x03, 0x73, 0x02, 0x01, 0x01]);
        payload.push(username_bytes.len() as u8);
        payload.extend_from_slice(&[0x02, 0x01, 0x01, 0x01, 0x01]);
        payload.push(field_count);
        payload.extend_from_slice(&[0x01, 0x01]);
        payload.extend_from_slice(username_bytes);

        // Add AUTH_SESSKEY field (hex-encoded client session key)
        let sesskey_hex: String = response_data
            .client_sesskey
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect();
        let sesskey_len = sesskey_hex.len() as u8;
        payload.extend_from_slice(&[0x01, 0x0C, 0x0C]); // Field marker
        payload.extend_from_slice(b"AUTH_SESSKEY");
        payload.extend_from_slice(&[0x01, sesskey_len, sesskey_len]);
        payload.extend_from_slice(sesskey_hex.as_bytes());

        // Add AUTH_PASSWORD field (hex-encoded encrypted password)
        let password_hex: String = response_data
            .auth_password
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect();
        let password_len = password_hex.len() as u8;
        payload.extend_from_slice(&[0x00, 0x01, 0x0D, 0x0D]); // Field separator + marker
        payload.extend_from_slice(b"AUTH_PASSWORD");
        payload.extend_from_slice(&[0x01, password_len, password_len]);
        payload.extend_from_slice(password_hex.as_bytes());

        // Add AUTH_PBKDF2_SPEEDY_KEY if present
        if let Some(ref speedy_key) = response_data.speedy_key {
            let speedy_hex: String = speedy_key.iter().map(|b| format!("{:02X}", b)).collect();
            // speedy_key is 80 bytes = 160 hex chars, need 2-byte length encoding
            let speedy_len = speedy_hex.len();
            payload.extend_from_slice(&[0x00, 0x01, 0x16, 0x16]); // Field separator + marker
            payload.extend_from_slice(b"AUTH_PBKDF2_SPEEDY_KEY");
            if speedy_len <= 0x7F {
                payload.extend_from_slice(&[0x01, speedy_len as u8, speedy_len as u8]);
            } else {
                // Use 0xA0 prefix for lengths >= 128 (160 hex chars = 0xA0)
                payload.extend_from_slice(&[0x01, 0xA0, 0xA0]);
            }
            payload.extend_from_slice(speedy_hex.as_bytes());
        }

        // Null terminator
        payload.push(0x00);

        payload
    }

    /// Build connection context for auth provider
    fn build_connection_context(&self) -> ConnectionContext {
        let mut context = ConnectionContext::new(
            self.client_addr,
            &self.target.host,
            self.target.port,
            DatabaseType::Oracle,
        );

        if let Some(session_id) = &self.session_id {
            context = context.with_session_id(session_id.to_string());
        }

        context
    }

    /// Receive a complete TNS packet from the client
    async fn receive_tns_packet(&mut self) -> Result<Vec<u8>> {
        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        // Read TNS header (8 bytes)
        let mut header_buf = [0u8; TNS_HEADER_SIZE];
        match timeout(
            connect_timeout,
            self.client_mut()?.read_exact(&mut header_buf),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(ProxyError::Connection(format!(
                    "Failed to read TNS header: {}",
                    e
                )));
            }
            Err(_) => {
                return Err(ProxyError::Timeout("Reading TNS header".into()));
            }
        }

        // Check if this is standard TNS format (2-byte length) or native format (4-byte length)
        let two_byte_len = u16::from_be_bytes([header_buf[0], header_buf[1]]);

        if two_byte_len == 0 {
            // This might be native format with 4-byte length
            // Format: [0, 0, len_hi, len_lo, type, flags, ...]
            // where bytes 0-3 are the 4-byte length
            let four_byte_len =
                u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]])
                    as usize;
            let packet_type = header_buf[4];

            if !(8..=65536).contains(&four_byte_len) {
                return Err(ProxyError::Protocol(format!(
                    "Invalid native packet length: {}",
                    four_byte_len
                )));
            }

            // Read remaining payload (total length minus the 8 bytes we already read)
            let payload_len = four_byte_len - 8;
            let mut packet = Vec::with_capacity(four_byte_len);
            packet.extend_from_slice(&header_buf);

            if payload_len > 0 {
                let mut payload = vec![0u8; payload_len];
                timeout(connect_timeout, self.client_mut()?.read_exact(&mut payload))
                    .await
                    .map_err(|_| ProxyError::Timeout("Reading native packet payload".into()))?
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to read native payload: {}", e))
                    })?;
                packet.extend_from_slice(&payload);
            }

            debug!(
                "Received native packet: type=0x{:02X}, len={}",
                packet_type, four_byte_len
            );
            return Ok(packet);
        }

        // Standard TNS format
        let header = parse_header(&header_buf)?;
        let total_length = header.length as usize;

        if total_length < TNS_HEADER_SIZE {
            return Err(ProxyError::Protocol(format!(
                "Invalid TNS packet length: {}",
                total_length
            )));
        }

        // Read remaining payload
        let payload_len = total_length - TNS_HEADER_SIZE;
        let mut packet = Vec::with_capacity(total_length);
        packet.extend_from_slice(&header_buf);

        if payload_len > 0 {
            let mut payload = vec![0u8; payload_len];
            timeout(connect_timeout, self.client_mut()?.read_exact(&mut payload))
                .await
                .map_err(|_| ProxyError::Timeout("Reading TNS payload".into()))?
                .map_err(|e| {
                    ProxyError::Connection(format!("Failed to read TNS payload: {}", e))
                })?;
            packet.extend_from_slice(&payload);
        }

        debug!(
            "Received TNS packet: type={} (0x{:02X}), len={}",
            header.type_name(),
            header.packet_type,
            total_length
        );

        Ok(packet)
    }

    /// Receive a complete TNS packet from a stream (static helper)
    /// Handles both standard TNS format (2-byte length) and native format (4-byte length)
    async fn receive_tns_packet_from(stream: &mut NetworkStream) -> Result<Vec<u8>> {
        // Read TNS header (8 bytes)
        let mut header_buf = [0u8; TNS_HEADER_SIZE];
        stream.read_exact(&mut header_buf).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to read TNS header from server: {}", e))
        })?;

        // Check if this is standard TNS format (2-byte length) or native format (4-byte length)
        let two_byte_len = u16::from_be_bytes([header_buf[0], header_buf[1]]);

        if two_byte_len == 0 {
            // Native format with 4-byte length
            let four_byte_len =
                u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]])
                    as usize;
            let packet_type = header_buf[4];

            if !(8..=65536).contains(&four_byte_len) {
                return Err(ProxyError::Protocol(format!(
                    "Invalid native packet length from server: {}",
                    four_byte_len
                )));
            }

            // Read remaining payload
            let payload_len = four_byte_len - 8;
            let mut packet = Vec::with_capacity(four_byte_len);
            packet.extend_from_slice(&header_buf);

            if payload_len > 0 {
                let mut payload = vec![0u8; payload_len];
                stream.read_exact(&mut payload).await.map_err(|e| {
                    ProxyError::Connection(format!(
                        "Failed to read native payload from server: {}",
                        e
                    ))
                })?;
                packet.extend_from_slice(&payload);
            }

            debug!(
                "Received server native packet: type=0x{:02X}, len={}",
                packet_type, four_byte_len
            );
            return Ok(packet);
        }

        // Standard TNS format
        let header = parse_header(&header_buf)?;
        let total_length = header.length as usize;

        if total_length < TNS_HEADER_SIZE {
            return Err(ProxyError::Protocol(format!(
                "Invalid TNS packet length from server: {}",
                total_length
            )));
        }

        // Read remaining payload
        let payload_len = total_length - TNS_HEADER_SIZE;
        let mut packet = Vec::with_capacity(total_length);
        packet.extend_from_slice(&header_buf);

        if payload_len > 0 {
            let mut payload = vec![0u8; payload_len];
            stream.read_exact(&mut payload).await.map_err(|e| {
                ProxyError::Connection(format!("Failed to read TNS payload from server: {}", e))
            })?;
            packet.extend_from_slice(&payload);
        }

        debug!(
            "Received server TNS packet: type={} (0x{:02X}), len={}",
            header.type_name(),
            header.packet_type,
            total_length
        );

        Ok(packet)
    }

    /// Connect to Oracle server
    ///
    /// Returns a `NetworkStream` - either plain TCP or TLS-wrapped depending on configuration.
    /// Oracle TCPS (port 1522) uses implicit TLS: the TLS handshake happens immediately
    /// after TCP connect, with no protocol-level negotiation like PostgreSQL's SSLRequest.
    async fn connect_to_server(&self) -> Result<NetworkStream> {
        let addr = format!("{}:{}", self.target.host, self.target.port);
        debug!("Connecting to Oracle at {}", addr);

        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        let tcp_stream = timeout(connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ProxyError::Timeout(format!("Connecting to {}", addr)))?
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to {}: {}", addr, e)))?;

        debug!("Connected to Oracle at {}", addr);

        // Auto-detect TLS: try TLS handshake first, fall back to plain TCP.
        // Oracle ADB (TCPS) requires TLS immediately, while XE/Free/EE use plain TCP.
        debug!("TLS auto-detect: trying TLS handshake");
        match timeout(
            Duration::from_secs(5),
            self.tls_connector.connect(tcp_stream, &self.target.host),
        )
        .await
        {
            Ok(Ok(tls_stream)) => {
                info!(
                    "TLS connection established to Oracle at {} (TCPS auto-detected)",
                    addr
                );
                Ok(NetworkStream::ClientTls(Box::new(tls_stream)))
            }
            Ok(Err(e)) => {
                // TLS handshake failed — server doesn't support TLS, reconnect plain TCP
                debug!("TLS handshake failed ({}), falling back to plain TCP", e);

                let tcp_stream = timeout(connect_timeout, TcpStream::connect(&addr))
                    .await
                    .map_err(|_| ProxyError::Timeout(format!("Reconnecting to {}", addr)))?
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to reconnect to {}: {}", addr, e))
                    })?;

                info!("Plain TCP connection established to Oracle at {}", addr);
                Ok(NetworkStream::Tcp(tcp_stream))
            }
            Err(_) => {
                // TLS handshake timed out — likely not a TLS server, reconnect plain TCP
                debug!("TLS handshake timed out, falling back to plain TCP");

                let tcp_stream = timeout(connect_timeout, TcpStream::connect(&addr))
                    .await
                    .map_err(|_| ProxyError::Timeout(format!("Reconnecting to {}", addr)))?
                    .map_err(|e| {
                        ProxyError::Connection(format!("Failed to reconnect to {}: {}", addr, e))
                    })?;

                info!("Plain TCP connection established to Oracle at {}", addr);
                Ok(NetworkStream::Tcp(tcp_stream))
            }
        }
    }

    /// Send a REFUSE packet to the client
    #[allow(dead_code)]
    async fn send_client_refuse(&mut self, user_reason: u8, message: &str) -> Result<()> {
        let refuse = RefusePacket::new(user_reason, 0, message);
        let packet = serialize_refuse(&refuse);

        self.client_mut()?.write_all(&packet).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to send REFUSE to client: {}", e))
        })?;
        self.client_mut()?
            .flush()
            .await
            .map_err(|e| ProxyError::Connection(format!("Failed to flush REFUSE: {}", e)))?;

        Ok(())
    }
}
