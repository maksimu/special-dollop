//! MySQL protocol handler
//!
//! This handler implements the MySQL authentication flow with credential injection:
//! 1. Connect to real server FIRST (to get server version)
//! 2. Receive server handshake (get real version and scramble)
//! 3. Send HandshakeV10 to client (with real server version)
//! 4. Receive client auth (discard credentials) - OR handle SSL upgrade request
//! 5. Send auth with real credentials to server
//! 6. Handle OK/ERR (including auth switch)
//! 7. Relay traffic
//!
//! ## TLS Support
//!
//! When TLS is enabled, the flow changes slightly:
//! 1. HandshakeV10 includes CLIENT_SSL capability flag
//! 2. Client may send SSL request (truncated packet with SSL flag)
//! 3. Proxy performs TLS handshake with client
//! 4. Client re-sends full auth over TLS
//! 5. Rest of flow continues as normal

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::timeout;
use zeroize::Zeroize;

use super::super::connection_tracker::{ConnectionTracker, TargetId, TunnelId};
use super::super::session::{ManagedSession, QueryLogContext, Session, SessionManager};
use super::super::stream::NetworkStream;
use crate::auth::{AuthProvider, ConnectionContext, DatabaseType};
use crate::config::{Config, TargetWithCredentials};
use crate::error::{ProxyError, Result};

use crate::protocol::mysql::{
    auth::{compute_auth_for_plugin, generate_scramble},
    packets::*,
    parser::*,
};
use crate::tls::TlsAcceptor;

// ============================================================================
// MySQL Authentication Protocol Constants
// ============================================================================

/// Auth switch request indicator (0xFE)
/// Sent by server to request a different authentication plugin
const AUTH_SWITCH_REQUEST: u8 = 0xFE;

/// More auth data indicator (0x01)
/// Sent by server during caching_sha2_password multi-round auth
const AUTH_MORE_DATA: u8 = 0x01;

/// Fast auth success indicator
/// Server has cached the client's public key, auth succeeded
const CACHING_SHA2_FAST_AUTH_SUCCESS: u8 = 0x03;

/// Full auth required indicator
/// Server needs cleartext password (over TLS) or RSA-encrypted password
const CACHING_SHA2_FULL_AUTH_REQUIRED: u8 = 0x04;

/// MySQL handler state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum HandlerState {
    /// Initial state
    Init,
    /// Sent handshake to client
    SentHandshake,
    /// Received client auth
    ReceivedClientAuth,
    /// Connected to server
    ConnectedToServer,
    /// Received server handshake
    ReceivedServerHandshake,
    /// Sent auth to server
    SentServerAuth,
    /// Authentication complete
    Authenticated,
    /// Relaying traffic
    Relaying,
    /// Error state
    Error,
}

/// MySQL protocol handler
pub struct MySQLHandler {
    /// Client stream (Option for safe taking during TLS upgrade)
    client: Option<NetworkStream>,
    /// Client address
    client_addr: SocketAddr,
    /// Configuration
    config: Arc<Config>,
    /// Target database configuration for this handler
    target: TargetWithCredentials,
    /// Authentication provider
    auth_provider: Arc<dyn AuthProvider>,
    /// Handler state
    state: HandlerState,
    /// Shutdown signal receiver (for future graceful shutdown support)
    #[allow(dead_code)]
    shutdown_rx: broadcast::Receiver<()>,
    /// Scramble sent to client
    client_scramble: [u8; 20],
    /// Connection ID
    connection_id: u32,
    /// TLS acceptor for upgrading client connections (if TLS enabled)
    tls_acceptor: Option<TlsAcceptor>,
    /// Last sequence ID from client auth (for correct response seq ID)
    client_auth_seq_id: u8,
    /// Connection tracker for single-connection enforcement
    connection_tracker: Option<Arc<ConnectionTracker>>,
    /// Session ID for gateway mode (credentials lookup)
    session_id: Option<uuid::Uuid>,
    /// Gateway-provided session UID for tunnel grouping
    session_uid: Option<String>,
}

impl MySQLHandler {
    /// Create a new MySQL handler
    ///
    /// # Errors
    ///
    /// Returns an error if TLS is enabled in the configuration but the TLS
    /// acceptor cannot be created (e.g., certificate file not found).
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

    /// Create a new MySQL handler with a connection tracker.
    ///
    /// The connection tracker is used to enforce single-connection mode
    /// when configured in the session policy.
    ///
    /// # Errors
    ///
    /// Returns an error if TLS is enabled in the configuration but the TLS
    /// acceptor cannot be created (e.g., certificate file not found), or if
    /// no MySQL target is configured.
    pub fn with_tracker(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
    ) -> Result<Self> {
        // Get the target configuration for MySQL
        let target = config
            .get_effective_target("mysql")
            .ok_or_else(|| ProxyError::Config("No MySQL target configured".to_string()))?;

        Self::with_target(
            client,
            client_addr,
            config,
            auth_provider,
            shutdown_rx,
            connection_tracker,
            target,
            None, // No session_id in settings mode
            None, // No session_uid in settings mode
        )
    }

    /// Create a new MySQL handler with a session ID for gateway mode.
    ///
    /// In gateway mode, the target and credentials come from the handshake,
    /// not from configuration. The session_id is used to look up credentials
    /// from the HandshakeAuthProvider.
    #[allow(dead_code)]
    pub fn with_session_id(
        _client: TcpStream,
        _client_addr: SocketAddr,
        _config: Arc<Config>,
        _auth_provider: Arc<dyn AuthProvider>,
        _shutdown_rx: broadcast::Receiver<()>,
        _connection_tracker: Option<Arc<ConnectionTracker>>,
        _session_id: uuid::Uuid,
    ) -> Result<Self> {
        // For gateway mode, we need to peek at the credential store to get target info
        // The auth_provider should be a HandshakeAuthProvider with the credentials stored
        // We create a minimal target - the actual credentials come from auth_provider

        // Get target info from the credential store via the session_id
        // For now, we need to look this up - but we don't have direct access
        // Instead, we'll require the caller to provide target info

        // Actually, let's require target host/port to be passed in
        Err(ProxyError::Config(
            "with_session_id requires target info - use with_session_id_and_target instead".into(),
        ))
    }

    /// Create a new MySQL handler with session ID and target info for gateway mode.
    ///
    /// This is used after a handshake when credentials and target info
    /// are provided dynamically rather than from configuration.
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
        // Create a minimal target with the host/port from handshake
        // Credentials will be fetched from auth_provider using session_id
        let target = TargetWithCredentials {
            host: target_host,
            port: target_port,
            database: None,
            tls: crate::tls::TlsClientConfig::default(),
            credentials: crate::config::CredentialsConfig {
                username: String::new(), // Not used - comes from auth_provider
                password: String::new(), // Not used - comes from auth_provider
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
            Some(session_id), // Pass session_id for gateway mode credential lookup
            session_uid,      // Pass session_uid for tunnel grouping
        )
    }

    /// Internal constructor with explicit target.
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
        // Create TLS acceptor if TLS is enabled
        let tls_acceptor = if config.server.tls.enabled {
            let acceptor = TlsAcceptor::new(&config.server.tls).map_err(|e| {
                error!("Failed to create TLS acceptor: {}", e);
                ProxyError::from(e)
            })?;
            debug!("TLS acceptor created for client connections");
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
            client_scramble: generate_scramble(),
            connection_id: rand::random(),
            tls_acceptor,
            client_auth_seq_id: 0,
            connection_tracker,
            session_id,
            session_uid,
        })
    }

    /// Get a mutable reference to the client stream
    fn client_mut(&mut self) -> Result<&mut NetworkStream> {
        self.client
            .as_mut()
            .ok_or_else(|| ProxyError::Connection("Client stream not available".into()))
    }

    /// Take the client stream (for operations that need ownership)
    fn take_client(&mut self) -> Result<NetworkStream> {
        self.client
            .take()
            .ok_or_else(|| ProxyError::Connection("Client stream not available".into()))
    }

    /// Handle the MySQL connection
    pub async fn handle(mut self) -> Result<()> {
        // Step 1: Connect to real server FIRST (to get version info)
        let mut server = self.connect_to_server().await?;
        self.state = HandlerState::ConnectedToServer;

        // Step 2: Receive server handshake (get real version and scramble)
        debug!("Waiting for server handshake");
        let (header, payload) = read_packet(&mut server).await?;
        debug!(
            "Received server handshake: {} bytes, seq={}",
            payload.len(),
            header.sequence_id
        );

        let server_handshake = parse_handshake_v10(&payload)?;
        debug!(
            "Server: {} (conn_id={}, auth_plugin={})",
            server_handshake.server_version,
            server_handshake.connection_id,
            server_handshake.auth_plugin_name
        );
        self.state = HandlerState::ReceivedServerHandshake;

        // Step 3: Send HandshakeV10 to client (with real server version)
        self.send_client_handshake(&server_handshake).await?;
        self.state = HandlerState::SentHandshake;

        // Step 4: Receive client auth response
        let client_response = self.receive_client_auth().await?;
        self.state = HandlerState::ReceivedClientAuth;

        info!(
            "Client {} authenticated as '{}' (credentials will be replaced)",
            self.client_addr, client_response.username
        );

        // Step 5-7: Complete server authentication and start relay
        self.complete_server_auth(server, &server_handshake, &client_response)
            .await
    }

    /// Send HandshakeV10 to the client (using real server version)
    async fn send_client_handshake(&mut self, server_handshake: &HandshakeV10) -> Result<()> {
        debug!(
            "Sending HandshakeV10 to client with server version: {}",
            server_handshake.server_version
        );

        let mut handshake = HandshakeV10 {
            connection_id: self.connection_id,
            // Use the REAL server version so clients behave correctly
            server_version: server_handshake.server_version.clone(),
            // Forward server's character set
            character_set: server_handshake.character_set,
            // Forward server's status flags
            status_flags: server_handshake.status_flags,
            ..Default::default()
        };

        // Set OUR scramble (we discard client credentials anyway, but need consistent scramble)
        handshake
            .auth_plugin_data_part_1
            .copy_from_slice(&self.client_scramble[..8]);
        handshake.auth_plugin_data_part_2 = self.client_scramble[8..].to_vec();

        // Use intersection of our capabilities and server's capabilities
        // This ensures we don't advertise features the backend doesn't support
        let server_caps = server_handshake.capability_flags();
        let mut our_caps = DEFAULT_SERVER_CAPABILITIES;

        // Add CLIENT_SSL capability if TLS is available
        if self.tls_acceptor.is_some() {
            our_caps |= CLIENT_SSL;
            debug!("TLS available, advertising CLIENT_SSL capability");
        }

        let combined_caps = our_caps & server_caps;

        // Re-add capabilities that the proxy handles independently of the backend server
        let mut final_caps = combined_caps;

        // Re-add CLIENT_SSL if we have TLS (even if server doesn't support it,
        // since client->proxy TLS is independent of proxy->server TLS)
        if self.tls_acceptor.is_some() {
            final_caps |= CLIENT_SSL;
        }

        // Re-add CLIENT_CONNECT_ATTRS - we need to receive connect_attrs from clients
        // for application name tracking, even if the backend doesn't support them
        final_caps |= CLIENT_CONNECT_ATTRS;

        handshake.set_capability_flags(final_caps);
        handshake.auth_plugin_data_length = 21; // 8 + 12 + 1 (null terminator)

        // Use server's auth plugin preference
        handshake.auth_plugin_name = server_handshake.auth_plugin_name.clone();

        // Build and send packet
        let payload = build_handshake_v10(&handshake);
        write_packet(self.client_mut()?, 0, &payload).await?;

        debug!(
            "HandshakeV10 sent: version={}, capabilities=0x{:08X}, auth_plugin={}, tls_available={}",
            handshake.server_version,
            handshake.capability_flags(),
            handshake.auth_plugin_name,
            self.tls_acceptor.is_some()
        );
        Ok(())
    }

    /// Receive and parse client auth response
    ///
    /// This method handles both regular auth responses and SSL upgrade requests.
    /// If the client requests SSL and TLS is available, this method will:
    /// 1. Upgrade the connection to TLS
    /// 2. Re-read the full auth response over TLS
    async fn receive_client_auth(&mut self) -> Result<HandshakeResponse41> {
        debug!("Waiting for client auth response");

        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        let (header, payload) = timeout(connect_timeout, read_packet(self.client_mut()?))
            .await
            .map_err(|_| ProxyError::Timeout("Waiting for client auth".into()))??;

        debug!(
            "Received client auth: {} bytes, seq={}",
            payload.len(),
            header.sequence_id
        );

        // Check for SSL request
        // SSL request is a truncated handshake response with just capabilities (32 bytes)
        // and the CLIENT_SSL flag set
        if payload.len() == 32 {
            let caps = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            if caps & CLIENT_SSL != 0 {
                debug!("Client requested SSL upgrade (capabilities=0x{:08X})", caps);

                // Check if we have TLS available
                if let Some(acceptor) = self.tls_acceptor.take() {
                    info!("Upgrading client connection to TLS");

                    // Take ownership of current stream and upgrade it
                    let current_stream = self.take_client()?;

                    // Extract the TCP stream - connection must be plain TCP at this point
                    let tcp_stream = match current_stream.into_tcp() {
                        Ok(stream) => stream,
                        Err(_) => {
                            return Err(ProxyError::Protocol(
                                "Cannot upgrade: already using TLS".into(),
                            ));
                        }
                    };

                    // Perform TLS handshake
                    let tls_stream = acceptor.accept(tcp_stream).await.map_err(|e| {
                        error!("TLS handshake failed: {}", e);
                        ProxyError::from(e)
                    })?;

                    // Update client stream to TLS
                    self.client = Some(NetworkStream::ServerTls(Box::new(tls_stream)));

                    // Log TLS details
                    if let Some(version) = self.client_mut()?.tls_version() {
                        debug!("TLS handshake complete: {}", version);
                    }
                    if let Some(cipher) = self.client_mut()?.cipher_suite() {
                        debug!("TLS cipher suite: {}", cipher);
                    }

                    // Now read the real auth response over TLS
                    debug!("Reading auth response over TLS");
                    let (header, payload) =
                        timeout(connect_timeout, read_packet(self.client_mut()?))
                            .await
                            .map_err(|_| ProxyError::Timeout("Waiting for TLS auth".into()))??;

                    debug!(
                        "Received TLS auth: {} bytes, seq={}",
                        payload.len(),
                        header.sequence_id
                    );

                    // Store seq ID for correct response (after TLS, seq=2, so response needs seq=3)
                    self.client_auth_seq_id = header.sequence_id;
                    return parse_handshake_response41(&payload);
                } else {
                    warn!("Client requested SSL, but TLS is not configured");
                    self.send_client_error(1045, "SSL connections not supported by this proxy")
                        .await?;
                    return Err(ProxyError::UnsupportedProtocol("SSL".into()));
                }
            }
        }

        // Store seq ID for correct response (non-TLS, seq=1, so response needs seq=2)
        self.client_auth_seq_id = header.sequence_id;
        parse_handshake_response41(&payload)
    }

    /// Connect to the real MySQL server
    async fn connect_to_server(&self) -> Result<TcpStream> {
        let addr = format!("{}:{}", self.target.host, self.target.port);
        debug!("Connecting to server at {}", addr);

        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        let stream = timeout(connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ProxyError::Timeout(format!("Connecting to {}", addr)))?
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to {}: {}", addr, e)))?;

        debug!("Connected to server at {}", addr);
        Ok(stream)
    }

    /// Complete server authentication and start relay
    async fn complete_server_auth(
        mut self,
        mut server: TcpStream,
        server_handshake: &HandshakeV10,
        client_response: &HandshakeResponse41,
    ) -> Result<()> {
        // Build connection context for the auth provider
        let mut context = ConnectionContext::new(
            self.client_addr,
            &self.target.host,
            self.target.port,
            DatabaseType::MySQL,
        );

        // Use session_id from gateway handshake if available
        if let Some(session_id) = &self.session_id {
            context = context.with_session_id(session_id.to_string());
        }

        // Get session configuration from the auth provider
        let session_config = self
            .auth_provider
            .get_session_config(&context)
            .await
            .map_err(|e| {
                error!("Failed to get session config from auth provider: {}", e);
                ProxyError::Config(e.to_string())
            })?;

        // Retrieve logging config BEFORE get_auth(), which consumes credentials from the store
        let logging_config = super::resolve_logging_config(
            self.auth_provider.as_ref(),
            &context,
            &self.config.query_logging,
        )
        .await;

        debug!(
            "Session config: max_duration={:?}, idle_timeout={:?}, max_queries={:?}, max_connections={:?}, require_token={}",
            session_config.max_duration,
            session_config.idle_timeout,
            session_config.max_queries,
            session_config.max_connections_per_session,
            session_config.require_token
        );

        // Validate token if required
        let record_uid = if session_config.require_token {
            // Extract token from client's password/auth_response field
            let token = String::from_utf8_lossy(&client_response.auth_response);
            debug!("Token validation required, validating token");

            let validation = self
                .auth_provider
                .validate_token(&token)
                .await
                .map_err(|e| {
                    error!("Token validation error: {}", e);
                    ProxyError::TokenValidation(e.to_string())
                })?;

            if !validation.valid {
                let error_msg = validation
                    .error
                    .unwrap_or_else(|| "Invalid token".to_string());
                warn!(
                    client = %self.client_addr,
                    "Token validation failed: {}",
                    error_msg
                );
                self.send_client_error(1045, &format!("Access denied: {}", error_msg))
                    .await?;
                return Err(ProxyError::TokenValidation(error_msg));
            }

            // Check token expiration
            if validation.is_expired() {
                warn!(
                    client = %self.client_addr,
                    "Token has expired"
                );
                self.send_client_error(1045, "Access denied: Token has expired")
                    .await?;
                return Err(ProxyError::TokenValidation("Token expired".into()));
            }

            info!(
                client = %self.client_addr,
                record_uid = ?validation.record_uid,
                "Token validated successfully"
            );

            validation.record_uid
        } else {
            None
        };

        // Get credentials from the auth provider
        let credentials = self.auth_provider.get_auth(&context).await.map_err(|e| {
            error!("Failed to get credentials from auth provider: {}", e);
            ProxyError::CredentialRetrieval(e.to_string())
        })?;

        // Extract password from credentials
        let password = credentials
            .method
            .password()
            .ok_or_else(|| ProxyError::Auth("Auth method does not provide a password".into()))?;

        // Server handshake already received in handle() - now send auth
        let server_scramble = server_handshake.get_scramble();

        // Use the server's advertised auth plugin (e.g., caching_sha2_password for MySQL 8.0)
        let server_plugin = if server_handshake.auth_plugin_name.is_empty() {
            "mysql_native_password"
        } else {
            &server_handshake.auth_plugin_name
        };
        debug!(
            "Server auth plugin: {}, scramble_len: {}",
            server_plugin,
            server_scramble.len()
        );

        // Compute auth response using the correct algorithm for the server's plugin
        let auth_response = compute_auth_for_plugin(server_plugin, password, &server_scramble);
        debug!(
            "Auth response computed: {} bytes for plugin {}",
            auth_response.len(),
            server_plugin
        );

        // Determine database to use
        let database = self
            .target
            .database
            .clone()
            .or_else(|| client_response.database.clone());

        // Build response with real credentials
        // Use minimal required capabilities to avoid compatibility issues
        let mut response = HandshakeResponse41::default();

        // Start with essential capabilities only
        let mut caps = CLIENT_PROTOCOL_41 | CLIENT_SECURE_CONNECTION | CLIENT_PLUGIN_AUTH;

        // Add database support if we have a database
        if database.is_some() {
            caps |= CLIENT_CONNECT_WITH_DB;
        }

        // Add some common safe capabilities from the intersection
        let intersection = client_response.capability_flags & server_handshake.capability_flags();
        if intersection & CLIENT_LONG_PASSWORD != 0 {
            caps |= CLIENT_LONG_PASSWORD;
        }
        if intersection & CLIENT_TRANSACTIONS != 0 {
            caps |= CLIENT_TRANSACTIONS;
        }
        if intersection & CLIENT_MULTI_RESULTS != 0 {
            caps |= CLIENT_MULTI_RESULTS;
        }

        response.capability_flags = caps;
        response.max_packet_size = 0x00FFFFFF; // 16MB - standard safe value
        response.character_set = 0x21; // utf8_general_ci - universally supported
        response.username = credentials.method.username().to_string();
        response.auth_response = auth_response;
        response.database = database;
        response.auth_plugin_name = Some(server_plugin.to_string());

        let response_payload = build_handshake_response41(&response);

        debug!(
            "Sending auth to server: user={}, caps=0x{:08X}, db={:?}",
            response.username, response.capability_flags, response.database
        );

        write_packet(&mut server, 1, &response_payload).await?;

        debug!("Sent auth to server as '{}'", response.username);
        self.state = HandlerState::SentServerAuth;

        // Step 6: Handle server response (may require multiple rounds for auth switch)
        let mut seq_id = 2u8;
        loop {
            let (header, payload) = read_packet(&mut server).await?;
            debug!(
                "Server auth response: {} bytes, seq={}, first_byte=0x{:02X}",
                payload.len(),
                header.sequence_id,
                payload.first().unwrap_or(&0)
            );

            if is_err_packet(&payload) {
                let err = parse_err_packet(&payload, CLIENT_PROTOCOL_41)?;
                error!(
                    "Server auth failed: {} - {}",
                    err.error_code, err.error_message
                );
                // Forward error to client
                write_packet(self.client_mut()?, seq_id, &payload).await?;
                return Err(ProxyError::Auth(format!(
                    "Server rejected credentials: {}",
                    err.error_message
                )));
            }

            // Handle auth switch request BEFORE is_ok_packet check
            // because is_ok_packet incorrectly matches 0xFE (EOF-as-OK)
            // In auth context, 0xFE is always AuthSwitchRequest, not EOF
            if payload[0] == AUTH_SWITCH_REQUEST && payload.len() > 1 {
                debug!("Received auth switch request");

                // Parse auth switch request
                // Format: 0xFE + plugin_name (null-terminated) + plugin_data
                let plugin_end = payload[1..]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(payload.len() - 1);
                let plugin_name = String::from_utf8_lossy(&payload[1..1 + plugin_end]).to_string();
                let plugin_data = if 1 + plugin_end + 1 < payload.len() {
                    &payload[1 + plugin_end + 1..]
                } else {
                    &[]
                };

                debug!(
                    "Auth switch to plugin: {}, data_len: {}",
                    plugin_name,
                    plugin_data.len()
                );

                // Get the switch scramble - may have trailing null
                let switch_scramble = if plugin_data.last() == Some(&0) {
                    &plugin_data[..plugin_data.len() - 1]
                } else {
                    plugin_data
                };

                // Use the dispatcher to select the correct algorithm based on plugin name
                let auth_response =
                    compute_auth_for_plugin(&plugin_name, password, switch_scramble);

                seq_id = header.sequence_id + 1;
                write_packet(&mut server, seq_id, &auth_response).await?;
                debug!("Sent auth switch response, seq={}", seq_id);
                seq_id += 1;
                continue;
            }

            if is_ok_packet(&payload) {
                // Authentication successful
                debug!("Server auth OK");

                // Create SessionManager first to get session_id
                let mut session_manager = SessionManager::new(session_config.clone());
                let session_id = session_manager.session_id();

                // Extract client fingerprint from connect attributes for security validation.
                // Creates a unique identifier from program_name, client version, platform, etc.
                debug!(
                    connect_attrs = ?client_response.connect_attrs,
                    "MySQL client connect_attrs received"
                );
                let application_name = client_response.client_fingerprint();
                if let Some(ref app) = application_name {
                    debug!(client_fingerprint = %app, "MySQL client identified");
                } else {
                    debug!("MySQL client did not provide any identifiable attributes");
                }

                // Register with ConnectionTracker for:
                // 1. Single-session-per-target enforcement (when session_uid is available)
                // 2. Connection limit enforcement (when max_connections is set)
                // 3. Application name validation (defense-in-depth against session hijacking)
                // Uses grace period to allow brief multi-connection setup (e.g., MySQL Workbench).
                // MUST happen BEFORE sending OK to client so we can reject if needed.
                let max_connections = session_config.effective_max_connections();
                let needs_tracking = self.session_uid.is_some() || max_connections.is_some();
                let _connection_guard = if needs_tracking {
                    if let Some(ref tracker) = self.connection_tracker {
                        // Determine tunnel ID based on session_uid (from Gateway handshake),
                        // record_uid (from token validation), or connection info
                        let tunnel_id = if let Some(ref uid) = self.session_uid {
                            // session_uid from Gateway handshake - preferred for grouping
                            TunnelId::from_record_uid(uid)
                        } else if let Some(ref uid) = record_uid {
                            // record_uid from token validation
                            TunnelId::from_record_uid(uid)
                        } else {
                            // Fallback to connection-based grouping
                            TunnelId::from_connection(
                                &self.client_addr.to_string(),
                                &self.target.host,
                                self.target.port,
                            )
                        };

                        let grace_period = session_config.connection_grace_period;

                        // Use try_register_for_target_with_app when session_uid is available
                        // This enforces single-session-per-target security policy
                        // and validates application_name for defense-in-depth
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
                                    "Connection registered with limits"
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
                                    "Connection rejected"
                                );
                                self.send_client_error(1040, &e.to_string()).await?;
                                return Err(e);
                            }
                        }
                    } else {
                        debug!("Connection limits enabled but no tracker available");
                        None
                    }
                } else {
                    None
                };

                // Send OK to client with correct sequence ID (client's last seq + 1)
                // If this fails, we need to release the connection guard
                let response_seq_id = self.client_auth_seq_id.wrapping_add(1);
                debug!("Sending OK to client with seq={}", response_seq_id);
                if let Err(e) = write_packet(self.client_mut()?, response_seq_id, &payload).await {
                    if let Some((guard, tracker)) = _connection_guard {
                        guard.release_spawn(tracker);
                    }
                    return Err(e);
                }

                debug!(
                    "Authentication complete for {} -> {}",
                    self.client_addr, self.target.host
                );

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

                // Start session timers (max_duration, etc.)
                session_manager.start_timers();

                // Set up query logging if configured
                // (logging_config was retrieved before get_auth to avoid credential store consumption race)
                let query_log_ctx = if let Some(ref log_config) = logging_config {
                    let server_addr = format!("{}:{}", self.target.host, self.target.port);
                    let session_id_str = session_id.to_string();
                    QueryLogContext::create(
                        "mysql",
                        log_config,
                        &session_id_str,
                        &response.username,
                        client_response.database.as_deref().unwrap_or(""),
                        &self.client_addr.to_string(),
                        &server_addr,
                    )
                } else {
                    None
                };

                // Use ManagedSession for security controls, or basic Session if no limits.
                // Also use ManagedSession when query logging is configured (it needs
                // the managed relay loop's protocol-aware packet inspection).
                let has_limits = session_config.max_duration.is_some()
                    || session_config.idle_timeout.is_some()
                    || session_config.max_queries.is_some();

                if has_limits || query_log_ctx.is_some() {
                    // Use ManagedSession with query counting and disconnect monitoring
                    let mut managed = ManagedSession::new(
                        client_stream,
                        server,
                        session_manager,
                        self.config.server.idle_timeout_secs,
                    );
                    if let Some(ctx) = query_log_ctx {
                        managed = managed.with_query_logging(ctx);
                    }
                    let reason = managed.relay().await;

                    // Release connection guard if we have one
                    if let Some((guard, tracker)) = _connection_guard {
                        guard.release_spawn(tracker);
                    }

                    debug!(
                        session_id = %session_id,
                        reason = ?reason,
                        "Session ended"
                    );
                    return Ok(());
                } else {
                    // Use basic Session for minimal overhead when no limits
                    let session = Session::with_idle_timeout(
                        client_stream,
                        server,
                        self.config.server.idle_timeout_secs,
                    );
                    let result = session.relay().await;

                    // Release connection guard if we have one
                    if let Some((guard, tracker)) = _connection_guard {
                        guard.release_spawn(tracker);
                    }

                    return result;
                }
            }

            // Handle more auth data request for caching_sha2_password
            if payload[0] == AUTH_MORE_DATA {
                if payload.len() > 1 && payload[1] == CACHING_SHA2_FAST_AUTH_SUCCESS {
                    // Fast auth success - server has cached public key
                    debug!("caching_sha2_password fast auth success");
                    continue;
                } else if payload.len() > 1 && payload[1] == CACHING_SHA2_FULL_AUTH_REQUIRED {
                    // Full auth required - send cleartext password with null terminator
                    debug!("caching_sha2_password full auth requested");

                    // Build cleartext password packet (password + null terminator)
                    let mut cleartext = password.as_bytes().to_vec();
                    cleartext.push(0x00); // Null terminator required by protocol

                    seq_id = header.sequence_id + 1;
                    write_packet(&mut server, seq_id, &cleartext).await?;

                    // Zero the cleartext password from memory for security
                    cleartext.zeroize();

                    debug!("Sent cleartext auth for full auth path, seq={}", seq_id);
                    seq_id += 1;
                    continue;
                }
            }

            // Unknown packet
            warn!(
                "Unexpected packet during auth: {:02X?}",
                &payload[..std::cmp::min(16, payload.len())]
            );
            return Err(ProxyError::Protocol(format!(
                "Unexpected packet during auth: 0x{:02X}",
                payload[0]
            )));
        }
    }

    /// Send an error packet to the client
    async fn send_client_error(&mut self, code: u16, message: &str) -> Result<()> {
        let err = ErrPacket::new(code, message);
        let payload = build_err_packet(&err, CLIENT_PROTOCOL_41);
        write_packet(self.client_mut()?, 2, &payload).await?;
        Ok(())
    }
}
