//! SQL Server TDS protocol handler
//!
//! This handler implements the TDS authentication flow with credential injection:
//! 1. Receive PRELOGIN from client
//! 2. Connect to SQL Server and forward PRELOGIN
//! 3. Receive server PRELOGIN response, forward to client
//! 4. Handle TLS upgrade if encryption negotiated
//! 5. Receive LOGIN7 from client (discard credentials)
//! 6. Request real credentials from AuthProvider
//! 7. Construct new LOGIN7 with real credentials, send to server
//! 8. Forward LOGINACK/ERROR to client
//! 9. Relay traffic

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};
use zeroize::Zeroize;

use super::super::connection_tracker::{ConnectionTracker, TargetId, TunnelId};
use super::super::session::{ManagedSession, QueryLogContext, Session, SessionManager};
use super::super::stream::NetworkStream;

use crate::auth::{AuthProvider, ConnectionContext, DatabaseType};
use crate::config::{Config, TargetWithCredentials};
use crate::error::{ProxyError, Result};
use crate::protocol::sqlserver::{
    constants::{packet_type, token_type, EncryptionMode, TDS_HEADER_SIZE},
    parser::{
        build_error_packet, modify_login7_credentials, parse_login7, parse_prelogin,
        serialize_prelogin, serialize_prelogin_response,
    },
    Login7, TdsClientTlsStream, TdsHeader, TdsServerTlsStream,
};
use crate::tls::{TlsAcceptor, TlsConnector};

// ============================================================================
// SQL Server Handler State
// ============================================================================

/// SQL Server handler state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum HandlerState {
    /// Initial state - waiting for client PRELOGIN
    Init,
    /// Received PRELOGIN from client
    ReceivedClientPrelogin,
    /// Connected to SQL Server
    ConnectedToServer,
    /// Forwarded PRELOGIN to server
    SentPreloginToServer,
    /// Received PRELOGIN response from server
    ReceivedServerPrelogin,
    /// TLS handshake in progress
    TlsHandshake,
    /// Waiting for LOGIN7 from client
    WaitingForLogin7,
    /// Received LOGIN7 from client
    ReceivedLogin7,
    /// Sent modified LOGIN7 to server
    SentLogin7ToServer,
    /// Authentication complete
    Authenticated,
    /// Relaying traffic
    Relaying,
    /// Error state
    Error,
}

// ============================================================================
// SQL Server Handler
// ============================================================================

/// SQL Server TDS protocol handler
pub struct SqlServerHandler {
    /// Client stream (Option for safe taking during TLS upgrade)
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
    /// TLS acceptor for client connections (if TLS enabled)
    #[allow(dead_code)] // Reserved for future TLS implementation
    tls_acceptor: Option<TlsAcceptor>,
    /// Connection tracker for session enforcement
    #[allow(dead_code)] // Reserved for connection tracking implementation
    connection_tracker: Option<Arc<ConnectionTracker>>,
    /// Session ID for gateway mode
    session_id: Option<uuid::Uuid>,
    /// Gateway-provided session UID for tunnel grouping
    #[allow(dead_code)] // Reserved for session UID tracking
    session_uid: Option<String>,
    /// Negotiated encryption mode
    negotiated_encryption: EncryptionMode,
    /// TDS packet ID counter
    #[allow(dead_code)] // Reserved for multi-packet support
    packet_id: u8,
}

impl SqlServerHandler {
    /// Create a new SQL Server handler
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

    /// Create a new SQL Server handler with connection tracker
    pub fn with_tracker(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
    ) -> Result<Self> {
        // Get the target configuration for SQL Server
        let target = config
            .get_effective_target("sqlserver")
            .ok_or_else(|| ProxyError::Config("No SQL Server target configured".to_string()))?;

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
    ) -> Result<Self> {
        let target = TargetWithCredentials {
            host: target_host,
            port: target_port,
            database: None,
            tls: crate::tls::TlsClientConfig::default(),
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
        // Create TLS acceptor if enabled
        // Use TLS 1.2-only for SQL Server - some ODBC drivers don't handle TLS 1.3 properly
        let tls_acceptor = if config.server.tls.enabled {
            let acceptor = TlsAcceptor::new_tls12_only(&config.server.tls).map_err(|e| {
                error!("Failed to create TLS acceptor: {}", e);
                ProxyError::from(e)
            })?;
            debug!("TLS acceptor created for SQL Server client connections (TLS 1.2 only)");
            Some(acceptor)
        } else {
            None
        };

        Ok(Self {
            client: Some(NetworkStream::tcp(client)),
            client_addr,
            config,
            target,
            auth_provider,
            state: HandlerState::Init,
            shutdown_rx,
            tls_acceptor,
            connection_tracker,
            session_id,
            session_uid,
            negotiated_encryption: EncryptionMode::Off,
            packet_id: 1,
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
        // Step 1: Receive PRELOGIN from client
        debug!("Waiting for client PRELOGIN");
        let client_prelogin_packet = self.receive_tds_packet().await?;

        if client_prelogin_packet.0.packet_type != packet_type::PRELOGIN {
            return Err(ProxyError::Protocol(format!(
                "Expected PRELOGIN (0x12), got 0x{:02X}",
                client_prelogin_packet.0.packet_type
            )));
        }

        let client_prelogin = parse_prelogin(&client_prelogin_packet.1)
            .map_err(|e| ProxyError::Protocol(format!("Failed to parse client PRELOGIN: {}", e)))?;

        debug!(
            "Client PRELOGIN: version={}.{}, encryption={:?}",
            client_prelogin.version.major,
            client_prelogin.version.minor,
            client_prelogin.encryption
        );
        self.state = HandlerState::ReceivedClientPrelogin;

        // Step 2: Connect to SQL Server
        let mut server = self.connect_to_server().await?;
        self.state = HandlerState::ConnectedToServer;

        // Step 3: Forward PRELOGIN to server
        // If proxy has TLS enabled, modify client's PRELOGIN to request encryption=On from server
        // This ensures both client-proxy and proxy-server connections use TLS
        debug!("Forwarding PRELOGIN to server");
        let prelogin_to_server = if self.tls_acceptor.is_some()
            && client_prelogin.encryption == EncryptionMode::Off
        {
            // Client doesn't want encryption, but proxy requires it for proper operation
            // Modify the PRELOGIN to request encryption from server
            info!("Client requested encryption=Off but proxy has TLS enabled. Requesting encryption=On from server.");
            let mut modified_prelogin = client_prelogin.clone();
            modified_prelogin.encryption = EncryptionMode::On;
            serialize_prelogin(&modified_prelogin)
        } else {
            // Use original PRELOGIN
            let mut packet = client_prelogin_packet.0.serialize().to_vec();
            packet.extend_from_slice(&client_prelogin_packet.1);
            packet
        };

        server.write_all(&prelogin_to_server).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to send PRELOGIN to server: {}", e))
        })?;
        self.state = HandlerState::SentPreloginToServer;

        // Step 4: Receive server PRELOGIN response
        debug!("Waiting for server PRELOGIN response");
        let server_prelogin_packet = Self::receive_tds_packet_from(&mut server).await?;

        // SQL Server should respond with PRELOGIN (0x12), but some versions/configurations
        // (particularly SQL Server 2022 under ARM emulation) may respond with TABULAR_RESULT (0x04)
        // containing PRELOGIN data. Accept both packet types.
        if server_prelogin_packet.0.packet_type != packet_type::PRELOGIN
            && server_prelogin_packet.0.packet_type != packet_type::TABULAR_RESULT
        {
            let response_hex: String = server_prelogin_packet
                .1
                .iter()
                .take(64)
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ");
            warn!(
                "SQL Server returned unexpected packet type 0x{:02X}. Response data: {}",
                server_prelogin_packet.0.packet_type, response_hex
            );
            return Err(ProxyError::Protocol(format!(
                "Expected server PRELOGIN (0x12), got 0x{:02X}",
                server_prelogin_packet.0.packet_type
            )));
        }

        if server_prelogin_packet.0.packet_type == packet_type::TABULAR_RESULT {
            debug!("Server responded with TABULAR_RESULT (0x04) instead of PRELOGIN - attempting to parse as PRELOGIN");
        }

        let server_prelogin = parse_prelogin(&server_prelogin_packet.1)
            .map_err(|e| ProxyError::Protocol(format!("Failed to parse server PRELOGIN: {}", e)))?;

        debug!(
            "Server PRELOGIN: version={}.{}, encryption={:?}",
            server_prelogin.version.major,
            server_prelogin.version.minor,
            server_prelogin.encryption
        );
        self.state = HandlerState::ReceivedServerPrelogin;

        // Determine negotiated encryption mode
        // If proxy has TLS enabled, we've already requested encryption=On from the server
        let proxy_has_tls = self.tls_acceptor.is_some();
        let server_encryption = server_prelogin.encryption;

        // When proxy has TLS enabled and client originally requested Off, we've already
        // requested On from the server. Server should respond with On (or possibly Required).
        // We need to tell the client encryption=On regardless of what they originally requested.
        self.negotiated_encryption = if proxy_has_tls {
            // Proxy has TLS - we use encryption for both client and server
            if server_encryption == EncryptionMode::Off {
                // Server still said Off - this is unexpected since we requested On
                // Force encryption anyway (this maintains compatibility, server may accept TLS)
                info!("Server returned encryption=Off despite our request for On. Will still attempt TLS.");
            }
            // Use the higher of server's response or On
            if server_encryption == EncryptionMode::Required {
                EncryptionMode::Required
            } else {
                EncryptionMode::On
            }
        } else {
            // No proxy TLS - negotiate normally
            self.negotiate_encryption(client_prelogin.encryption, server_prelogin.encryption)
        };
        debug!(
            "Negotiated encryption: {:?} (server responded: {:?}, proxy_has_tls: {})",
            self.negotiated_encryption, server_encryption, proxy_has_tls
        );

        // Step 5: Forward server PRELOGIN response to client
        // If we're forcing encryption, modify the response to tell client encryption=On
        let response_packet = if self.negotiated_encryption != server_encryption {
            // Build a modified PRELOGIN response with our encryption setting
            // Note: Server PRELOGIN responses use packet type 0x04 (TABULAR_RESULT), not 0x12
            let mut modified_prelogin = server_prelogin.clone();
            modified_prelogin.encryption = self.negotiated_encryption;
            let modified_payload = serialize_prelogin_response(&modified_prelogin);
            debug!("Sending modified PRELOGIN response to client: {} bytes, encryption={:?} (was {:?})",
                modified_payload.len(), self.negotiated_encryption, server_encryption);
            modified_payload
        } else {
            // Forward original response unchanged
            let mut packet = server_prelogin_packet.0.serialize().to_vec();
            packet.extend_from_slice(&server_prelogin_packet.1);
            debug!(
                "Forwarding PRELOGIN response to client: {} bytes, encryption={:?}",
                packet.len(),
                server_prelogin.encryption
            );
            packet
        };

        self.client_mut()?
            .write_all(&response_packet)
            .await
            .map_err(|e| {
                ProxyError::Connection(format!("Failed to send PRELOGIN response to client: {}", e))
            })?;
        self.client_mut()?.flush().await.map_err(|e| {
            ProxyError::Connection(format!("Failed to flush PRELOGIN response: {}", e))
        })?;

        // Step 6: Handle TLS if encryption was negotiated
        // SQL Server TDS encryption: after PRELOGIN, if encryption is On/Required,
        // client expects TLS handshake before LOGIN7
        // Note: We may need TLS with client even when server doesn't support it
        let client_needs_tls = self.negotiated_encryption == EncryptionMode::On
            || self.negotiated_encryption == EncryptionMode::Required;
        let server_needs_tls = server_encryption == EncryptionMode::On
            || server_encryption == EncryptionMode::Required;

        let server = if client_needs_tls {
            info!(
                "TLS negotiated with client ({:?}) - upgrading client connection",
                self.negotiated_encryption
            );
            self.state = HandlerState::TlsHandshake;

            // Upgrade client connection to TLS (accept TLS from client)
            let client_stream = self.take_client()?;
            let tcp_stream = client_stream
                .into_tcp()
                .map_err(|_| ProxyError::Connection("Client already using TLS".into()))?;

            let acceptor = self.tls_acceptor.take().ok_or_else(|| {
                ProxyError::Config(
                    "TLS required but TLS acceptor not configured. Enable server.tls in config."
                        .into(),
                )
            })?;

            debug!("Performing TLS handshake with client (TDS-wrapped mode)");

            // SQL Server uses TDS-encapsulated TLS: TLS records are wrapped in TDS packets
            // Client sends TLS in TDS PRELOGIN (0x12) packets
            // Server responds with TLS in TDS TABULAR_RESULT (0x04) packets
            let tds_client_stream = TdsClientTlsStream::new(tcp_stream);

            let mut client_tls = acceptor
                .accept_stream(tds_client_stream)
                .await
                .map_err(|e| {
                    error!("Client TLS handshake failed: {}", e);
                    ProxyError::Connection(format!("Client TLS handshake failed: {}", e))
                })?;
            info!("Client TLS handshake complete (TDS-wrapped)");

            // Enable passthrough on client TDS wrapper - after TLS handshake,
            // data is sent as raw TLS over TCP without TDS wrapping
            client_tls.get_mut().0.set_passthrough(true);

            // Store TLS client connection
            self.client = Some(NetworkStream::TdsTls(Box::new(client_tls)));

            // Only upgrade server connection if server also needs TLS
            if server_needs_tls {
                // Upgrade server connection to TLS (connect TLS to server)
                // Use insecure connector - SQL Server often uses self-signed certs
                // with non-standard X.509 versions that rustls can't parse
                debug!("Performing TLS handshake with SQL Server (TDS-wrapped mode)");
                let connector = TlsConnector::new_insecure().map_err(|e| {
                    error!("Failed to create TLS connector: {}", e);
                    ProxyError::from(e)
                })?;

                // Wrap server connection in TDS TLS stream
                let tds_server_stream = TdsServerTlsStream::new(server);
                let mut server_tls = connector
                    .connect_stream(tds_server_stream, &self.target.host)
                    .await
                    .map_err(|e| {
                        error!("Server TLS handshake failed: {}", e);
                        ProxyError::Connection(format!("Server TLS handshake failed: {}", e))
                    })?;
                info!("Server TLS handshake complete (TDS-wrapped)");

                // Enable passthrough on server TDS wrapper
                server_tls.get_mut().0.set_passthrough(true);

                // For server, we also have a TLS stream over TDS wrapper
                NetworkStream::TdsServerTls(Box::new(server_tls))
            } else {
                // Client uses TLS but server originally said plaintext (encryption=Off)
                // Since we requested encryption=On from the server, try TLS with server anyway
                info!("Client TLS established, attempting TLS with server (we requested encryption=On)");

                debug!("Performing TLS handshake with SQL Server (TDS-wrapped mode)");
                let connector = TlsConnector::new_insecure().map_err(|e| {
                    error!("Failed to create TLS connector: {}", e);
                    ProxyError::from(e)
                })?;

                // Wrap server connection in TDS TLS stream
                let tds_server_stream = TdsServerTlsStream::new(server);
                let mut server_tls = connector
                    .connect_stream(tds_server_stream, &self.target.host)
                    .await
                    .map_err(|e| {
                        error!("Server TLS handshake failed: {}", e);
                        ProxyError::Connection(format!("Server TLS handshake failed: {}", e))
                    })?;
                info!("Server TLS handshake complete (TDS-wrapped)");

                // Enable passthrough on server TDS wrapper
                server_tls.get_mut().0.set_passthrough(true);

                // Return TLS stream
                NetworkStream::TdsServerTls(Box::new(server_tls))
            }
        } else {
            debug!("No encryption - proceeding with plaintext");
            NetworkStream::tcp(server)
        };

        self.state = HandlerState::WaitingForLogin7;

        // Step 7: Receive LOGIN7 from client
        debug!("Waiting for client LOGIN7");
        let (client_tds_header, client_login7_payload) = self.receive_tds_packet().await?;

        if client_tds_header.packet_type != packet_type::LOGIN7 {
            return Err(ProxyError::Protocol(format!(
                "Expected LOGIN7 (0x10), got 0x{:02X}",
                client_tds_header.packet_type
            )));
        }

        // Build the raw packet bytes (TDS header + payload)
        let mut client_login7_raw: Vec<u8> =
            Vec::with_capacity(TDS_HEADER_SIZE + client_login7_payload.len());
        client_login7_raw.extend_from_slice(&client_tds_header.serialize());
        client_login7_raw.extend_from_slice(&client_login7_payload);

        let client_login7 = parse_login7(&client_login7_payload)
            .map_err(|e| ProxyError::Protocol(format!("Failed to parse LOGIN7: {}", e)))?;

        // Debug: Show original client packet flags for comparison
        debug!(
            "Original client LOGIN7: option_flags3=0x{:02X}, tds_version=0x{:08X}",
            client_login7.option_flags3, client_login7.tds_version
        );

        info!(
            "SQL Server client {} connecting as '{}' to database '{}' (app: {})",
            self.client_addr,
            client_login7.username,
            client_login7.database,
            client_login7.app_name
        );
        self.state = HandlerState::ReceivedLogin7;

        // Step 8: Get real credentials and complete authentication
        self.complete_server_auth(server, &client_login7, &client_login7_raw)
            .await
    }

    /// Complete server authentication with credential injection
    async fn complete_server_auth(
        mut self,
        mut server: NetworkStream,
        client_login7: &Login7,
        client_login7_raw: &[u8],
    ) -> Result<()> {
        info!(
            "Starting credential injection (LOGIN7 raw len={})",
            client_login7_raw.len()
        );

        // Build connection context
        let mut context = ConnectionContext::new(
            self.client_addr,
            &self.target.host,
            self.target.port,
            DatabaseType::SQLServer,
        );

        if let Some(session_id) = &self.session_id {
            context = context.with_session_id(session_id.to_string());
        }

        // Get session configuration
        let session_config = self
            .auth_provider
            .get_session_config(&context)
            .await
            .map_err(|e| {
                error!("Failed to get session config: {}", e);
                ProxyError::Config(e.to_string())
            })?;

        // Retrieve logging config BEFORE get_auth(), which consumes credentials from the store
        let logging_config = super::resolve_logging_config(
            self.auth_provider.as_ref(),
            &context,
            &self.config.query_logging,
        )
        .await;

        // Get real credentials
        let auth_credentials = self.auth_provider.get_auth(&context).await.map_err(|e| {
            error!("Failed to get credentials: {}", e);
            ProxyError::Auth(e.to_string())
        })?;

        let mut username = auth_credentials.method.username().to_string();
        let mut password = auth_credentials
            .method
            .password()
            .ok_or_else(|| ProxyError::Auth("No password in credentials".into()))?
            .to_string();

        debug!("Injecting credentials for SQL Server authentication");

        // Determine database to use: prefer target config, then auth credentials, then client's
        let target_database = self
            .target
            .database
            .as_deref()
            .or(auth_credentials.database.as_deref());

        debug!(
            "Database selection: target.database={:?}, auth_credentials.database={:?}, final={:?}",
            self.target.database, auth_credentials.database, target_database
        );

        // Modify the original client LOGIN7 packet with new credentials
        // This preserves packet structure while only changing username/password/database
        let login7_packet =
            modify_login7_credentials(client_login7_raw, &username, &password, target_database)
                .map_err(|e| ProxyError::Protocol(format!("Failed to modify LOGIN7: {}", e)))?;

        // Debug: show LOGIN7 details
        debug!(
            "Sending LOGIN7: user={}, db={:?}, original_len={}, modified_len={}",
            username,
            target_database,
            client_login7_raw.len(),
            login7_packet.len()
        );

        // Resolve database name to owned String before we take mutable borrows on self.
        // target_database borrows self.target.database, so we must release it here.
        let resolved_database = target_database
            .unwrap_or(&client_login7.database)
            .to_string();

        // Show full LOGIN7 packet for debugging
        let hex_dump: String = login7_packet
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        debug!(
            "LOGIN7 full packet ({} bytes): {}",
            login7_packet.len(),
            hex_dump
        );

        // Show option flags from original packet for debugging
        if login7_packet.len() > TDS_HEADER_SIZE + 27 {
            let flags_start = TDS_HEADER_SIZE;
            debug!(
                "LOGIN7 flags: option1=0x{:02X}, option2=0x{:02X}, type=0x{:02X}, option3=0x{:02X}",
                login7_packet[flags_start + 24],
                login7_packet[flags_start + 25],
                login7_packet[flags_start + 26],
                login7_packet[flags_start + 27]
            );
        }

        // Session enforcement: Register connection with tracker BEFORE sending LOGIN7 to server
        // This ensures we reject connections before the client sees any LOGINACK
        let session_id = self.session_id.unwrap_or_else(uuid::Uuid::new_v4);
        let max_connections = session_config.effective_max_connections();
        let needs_tracking = self.session_uid.is_some() || max_connections.is_some();
        let application_name = Some(client_login7.app_name.clone());

        let _connection_guard = if needs_tracking {
            if let Some(ref tracker) = self.connection_tracker {
                // Determine tunnel ID based on session_uid or connection info
                let tunnel_id = if let Some(ref uid) = self.session_uid {
                    TunnelId::from_record_uid(uid)
                } else {
                    TunnelId::from_connection(
                        &self.client_addr.to_string(),
                        &self.target.host,
                        self.target.port,
                    )
                };

                let grace_period = session_config.connection_grace_period;

                // Register connection with appropriate method based on session_uid availability
                let result = if let Some(ref uid) = self.session_uid {
                    let target_id = TargetId::new(&self.target.host, self.target.port);
                    tracker
                        .try_register_for_target_with_app(
                            target_id,
                            uid.clone(),
                            tunnel_id.clone(),
                            session_id,
                            max_connections,
                            grace_period,
                            application_name.clone(),
                            session_config.application_lock_timeout,
                        )
                        .await
                } else {
                    tracker
                        .try_register_with_limits_and_app(
                            tunnel_id.clone(),
                            session_id,
                            max_connections,
                            grace_period,
                            application_name.clone(),
                        )
                        .await
                };

                match result {
                    Ok(guard) => {
                        debug!(
                            session_id = %session_id,
                            tunnel_id = %tunnel_id,
                            session_uid = ?self.session_uid,
                            max_connections = ?max_connections,
                            application_name = ?application_name,
                            grace_period_ms = ?grace_period.map(|d| d.as_millis()),
                            "SQL Server connection registered with limits"
                        );
                        Some((guard, tracker.clone()))
                    }
                    Err(e) => {
                        warn!(
                            session_id = %session_id,
                            tunnel_id = %tunnel_id,
                            session_uid = ?self.session_uid,
                            application_name = ?application_name,
                            error = %e,
                            "SQL Server connection rejected (before LOGIN7 sent to server)"
                        );
                        // Send error to client before returning
                        // Error 17809: "Could not connect because the maximum number of '...' user connections has been reached"
                        self.send_client_error(17809, &format!("Connection rejected: {}", e))
                            .await?;
                        return Err(e);
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // Debug: Check server connection state before sending LOGIN7 - use INFO level for visibility
        let server_type = match &server {
            NetworkStream::Tcp(_) => "Tcp (plaintext)",
            NetworkStream::TdsServerTls(_) => "TdsServerTls (encrypted)",
            _ => "Other",
        };
        info!(
            "Sending LOGIN7 ({} bytes) to server via {}",
            login7_packet.len(),
            server_type
        );

        // Debug: Show first 32 bytes of LOGIN7 packet
        if login7_packet.len() >= 32 {
            info!("LOGIN7 first 32 bytes: {:02X?}", &login7_packet[..32]);
        }

        server.write_all(&login7_packet).await.map_err(|e| {
            error!("Failed to write LOGIN7 to server: {}", e);
            ProxyError::Connection(format!("Failed to send LOGIN7 to server: {}", e))
        })?;
        server
            .flush()
            .await
            .map_err(|e| ProxyError::Connection(format!("Failed to flush LOGIN7: {}", e)))?;

        // Zeroize credentials
        username.zeroize();
        password.zeroize();

        self.state = HandlerState::SentLogin7ToServer;
        debug!("Sent LOGIN7 with injected credentials to server");

        // Step 9: Receive server response
        // First, try a small timeout read to see if server sends any data
        debug!("Waiting for server response after LOGIN7...");

        // Try to read with timeout to see if server responds or just closes
        let response_packet = match tokio::time::timeout(
            Duration::from_secs(10),
            Self::receive_tds_packet_from(&mut server),
        )
        .await
        {
            Ok(Ok(packet)) => packet,
            Ok(Err(e)) => {
                // Read failed
                warn!(
                    "Failed to read server response: {}. Server may have rejected LOGIN7.",
                    e
                );
                return Err(e);
            }
            Err(_) => {
                return Err(ProxyError::Timeout(
                    "Server did not respond to LOGIN7 within 10s".into(),
                ));
            }
        };

        // Check if it's a TABULAR_RESULT (which contains LOGINACK or ERROR tokens)
        if response_packet.0.packet_type != packet_type::TABULAR_RESULT {
            warn!(
                "Unexpected response type: 0x{:02X}",
                response_packet.0.packet_type
            );
        }

        // Look for LOGINACK or ERROR token
        let payload = &response_packet.1;
        let mut auth_success = false;

        // Simple token scan - in a full implementation we'd parse all tokens
        for (i, &byte) in payload.iter().enumerate() {
            if byte == token_type::LOGINACK {
                // LOGINACK token found
                debug!("LOGINACK token found at offset {}", i);
                auth_success = true;
                break;
            }
            if byte == token_type::ERROR {
                // ERROR token found
                debug!("ERROR token found at offset {}", i);
                // Parse error message (simplified)
                // Format: 0xAA + length(2) + error_number(4) + state(1) + class(1) + ...
                if i + 10 < payload.len() {
                    let error_num = i32::from_le_bytes([
                        payload[i + 3],
                        payload[i + 4],
                        payload[i + 5],
                        payload[i + 6],
                    ]);
                    warn!("SQL Server error: {}", error_num);
                }
                break;
            }
        }

        // Forward response to client (whether success or failure)
        let mut response_full = response_packet.0.serialize().to_vec();
        response_full.extend_from_slice(payload);
        self.client_mut()?
            .write_all(&response_full)
            .await
            .map_err(|e| {
                ProxyError::Connection(format!("Failed to send response to client: {}", e))
            })?;

        if !auth_success {
            return Err(ProxyError::Auth("SQL Server authentication failed".into()));
        }

        self.state = HandlerState::Authenticated;
        info!(
            "SQL Server authentication complete for {} -> {}:{}",
            self.client_addr, self.target.host, self.target.port
        );

        // Connection tracking was already done before sending LOGIN7 to server
        // _connection_guard is already set from earlier in this function

        // Start traffic relay
        self.state = HandlerState::Relaying;

        // Take the client stream for the relay session
        // If this fails, we need to release the connection guard
        let client_stream = match self.take_client() {
            Ok(stream) => stream,
            Err(e) => {
                if let Some((guard, tracker)) = _connection_guard {
                    guard.release_spawn(tracker);
                }
                return Err(e);
            }
        };

        let mut session_manager = SessionManager::new(session_config.clone());
        let session_id = session_manager.session_id();
        session_manager.start_timers();

        // Set up query logging if configured
        // (logging_config was retrieved before get_auth to avoid credential store consumption race)
        let query_log_ctx = if let Some(ref log_config) = logging_config {
            let server_addr = format!("{}:{}", self.target.host, self.target.port);
            let session_id_str = session_id.to_string();
            QueryLogContext::create(
                "sqlserver",
                log_config,
                &session_id_str,
                &username,
                &resolved_database,
                &self.client_addr.to_string(),
                &server_addr,
            )
        } else {
            None
        };

        let has_limits = session_config.max_duration.is_some()
            || session_config.idle_timeout.is_some()
            || session_config.max_queries.is_some();

        if has_limits || query_log_ctx.is_some() {
            let mut managed = ManagedSession::with_network_server(
                client_stream,
                server,
                session_manager,
                self.config.server.idle_timeout_secs,
            );
            if let Some(ctx) = query_log_ctx {
                managed = managed.with_query_logging(ctx);
            }
            let reason = managed.relay().await;

            // Release connection guard after relay completes
            if let Some((guard, tracker)) = _connection_guard {
                guard.release_spawn(tracker);
            }

            info!("SQL Server session ended: {:?}", reason);
        } else {
            let session = Session::with_network_streams(
                client_stream,
                server,
                self.config.server.idle_timeout_secs,
            );
            let result = session.relay().await;

            // Release connection guard after relay completes
            if let Some((guard, tracker)) = _connection_guard {
                guard.release_spawn(tracker);
            }

            result?;
            info!("SQL Server session ended normally");
        }

        Ok(())
    }

    /// Receive a TDS packet from the client
    async fn receive_tds_packet(&mut self) -> Result<(TdsHeader, Vec<u8>)> {
        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        // Read TDS header (8 bytes)
        let mut header_buf = [0u8; TDS_HEADER_SIZE];
        timeout(
            connect_timeout,
            self.client_mut()?.read_exact(&mut header_buf),
        )
        .await
        .map_err(|_| ProxyError::Timeout("Reading TDS header".into()))?
        .map_err(|e| ProxyError::Connection(format!("Failed to read TDS header: {}", e)))?;

        let header = TdsHeader::parse(&header_buf)
            .ok_or_else(|| ProxyError::Protocol("Invalid TDS header".into()))?;

        // Read payload
        let payload_len = header.payload_length();
        let mut payload = vec![0u8; payload_len];

        if payload_len > 0 {
            timeout(connect_timeout, self.client_mut()?.read_exact(&mut payload))
                .await
                .map_err(|_| ProxyError::Timeout("Reading TDS payload".into()))?
                .map_err(|e| {
                    ProxyError::Connection(format!("Failed to read TDS payload: {}", e))
                })?;
        }

        debug!(
            "Received TDS packet: type=0x{:02X}, status=0x{:02X}, len={}",
            header.packet_type, header.status, header.length
        );

        Ok((header, payload))
    }

    /// Receive a TDS packet from a stream (static helper)
    async fn receive_tds_packet_from<S: AsyncReadExt + Unpin>(
        stream: &mut S,
    ) -> Result<(TdsHeader, Vec<u8>)> {
        // Read TDS header (8 bytes)
        let mut header_buf = [0u8; TDS_HEADER_SIZE];
        stream.read_exact(&mut header_buf).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to read TDS header from server: {}", e))
        })?;

        let header = TdsHeader::parse(&header_buf)
            .ok_or_else(|| ProxyError::Protocol("Invalid TDS header from server".into()))?;

        // Read payload
        let payload_len = header.payload_length();
        let mut payload = vec![0u8; payload_len];

        if payload_len > 0 {
            stream.read_exact(&mut payload).await.map_err(|e| {
                ProxyError::Connection(format!("Failed to read TDS payload from server: {}", e))
            })?;
        }

        debug!(
            "Received server TDS packet: type=0x{:02X}, status=0x{:02X}, len={}",
            header.packet_type, header.status, header.length
        );

        Ok((header, payload))
    }

    /// Connect to SQL Server
    async fn connect_to_server(&self) -> Result<TcpStream> {
        let addr = format!("{}:{}", self.target.host, self.target.port);
        debug!("Connecting to SQL Server at {}", addr);

        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        let stream = timeout(connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ProxyError::Timeout(format!("Connecting to {}", addr)))?
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to {}: {}", addr, e)))?;

        debug!("Connected to SQL Server at {}", addr);
        Ok(stream)
    }

    /// Negotiate encryption mode between client and server preferences
    fn negotiate_encryption(
        &self,
        client: EncryptionMode,
        server: EncryptionMode,
    ) -> EncryptionMode {
        // Simplified negotiation logic
        match (client, server) {
            // Both off -> off
            (EncryptionMode::Off, EncryptionMode::Off) => EncryptionMode::Off,
            // Either requires -> required
            (EncryptionMode::Required, _) | (_, EncryptionMode::Required) => {
                EncryptionMode::Required
            }
            // Either on -> on (login encryption)
            (EncryptionMode::On, _) | (_, EncryptionMode::On) => EncryptionMode::On,
            // Not supported -> off
            (EncryptionMode::NotSupported, _) | (_, EncryptionMode::NotSupported) => {
                EncryptionMode::Off
            }
        }
    }

    /// Send a TDS error message to the client
    async fn send_client_error(&mut self, error_number: i32, message: &str) -> Result<()> {
        // Log stream type for debugging
        let stream_type = match &self.client {
            Some(NetworkStream::Tcp(_)) => "Tcp",
            Some(NetworkStream::ServerTls(_)) => "ServerTls",
            Some(NetworkStream::ClientTls(_)) => "ClientTls",
            Some(NetworkStream::TdsTls(_)) => "TdsTls",
            Some(NetworkStream::TdsServerTls(_)) => "TdsServerTls",
            None => "None",
        };

        info!(
            error_number = error_number,
            message = message,
            stream_type = stream_type,
            "Sending TDS error to client"
        );

        // Build error packet using the helper function
        // Severity 16 = general error that terminates the statement
        let packet = build_error_packet(error_number, message, "keeperdb-proxy", 16);

        let client = self.client_mut()?;
        client.write_all(&packet).await.map_err(|e| {
            ProxyError::Connection(format!("Failed to send error to client: {}", e))
        })?;
        client.flush().await.map_err(|e| {
            ProxyError::Connection(format!("Failed to flush error to client: {}", e))
        })?;

        info!(
            error_number = error_number,
            packet_len = packet.len(),
            "TDS error sent to client successfully"
        );

        // Give client time to read the error before connection closes
        sleep(Duration::from_millis(500)).await;

        Ok(())
    }
}
