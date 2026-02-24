//! PostgreSQL protocol handler
//!
//! This handler implements the PostgreSQL authentication flow with credential injection:
//! 1. Receive client StartupMessage (or SSLRequest for TLS)
//! 2. If SSLRequest, upgrade to TLS, then receive StartupMessage
//! 3. Connect to real server (optionally with TLS)
//! 4. Send StartupMessage to server (with real credentials)
//! 5. Handle authentication (MD5 or SCRAM-SHA-256)
//! 6. Forward AuthenticationOk to client
//! 7. Relay traffic
//!
//! ## Key Differences from MySQL
//!
//! - PostgreSQL client speaks first (sends StartupMessage)
//! - No scramble/challenge in initial handshake
//! - Authentication methods: MD5, SCRAM-SHA-256
//! - Message format: \[tag: u8\]\[length: u32\]\[payload\]
//!
//! ## Known Limitations
//!
//! - **Cancel Requests**: Query cancellation (CancelRequest messages) is not supported.
//!   Clients attempting to cancel queries will receive an error. This may affect GUI
//!   tools (pgAdmin, DBeaver) that rely on cancel functionality for long-running queries.
//!
//! - **Extended Query Protocol**: Only simple query protocol is supported.
//!   Prepared statements via Parse/Bind/Execute messages are relayed but not inspected.
//!
//! - **SASLprep**: SCRAM-SHA-256 does not perform full Unicode normalization.
//!   See [`crate::protocol::postgres::auth`] for details.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::timeout;

use crate::auth::{AuthProvider, ConnectionContext, DatabaseType};
use crate::config::{Config, TargetWithCredentials};
use crate::error::{ProxyError, Result};
use crate::protocol::postgres::{
    auth::{compute_md5_password, ScramClient},
    codec::{
        build_error_response as codec_build_error_response, parse_error_notice,
        parse_parameter_status as codec_parse_parameter_status, read_message, read_startup_message,
        write_message, write_startup_message, StartupMessageType,
    },
    constants::*,
    messages::*,
};
use crate::tls::{TlsAcceptor, TlsConnector};

use super::super::connection_tracker::{ConnectionGuard, ConnectionTracker, TargetId, TunnelId};
use super::super::session::{ManagedSession, Session, SessionManager};
use super::super::stream::NetworkStream;

// ============================================================================
// PostgreSQL Handler State Machine
// ============================================================================

/// PostgreSQL handler state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum HandlerState {
    /// Initial state - waiting for client startup
    Init,
    /// Received SSLRequest, performing TLS upgrade
    TlsUpgrade,
    /// Received StartupMessage from client
    ReceivedStartup,
    /// Connected to backend server
    ConnectedToServer,
    /// Sent StartupMessage to server
    SentStartup,
    /// Authenticating with server
    Authenticating,
    /// Authentication complete
    Authenticated,
    /// Relaying traffic
    Relaying,
    /// Error state
    Error,
}

// ============================================================================
// PostgreSQL Handler
// ============================================================================

/// PostgreSQL protocol handler
pub struct PostgresHandler {
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
    /// Shutdown signal receiver
    #[allow(dead_code)]
    shutdown_rx: broadcast::Receiver<()>,
    /// TLS acceptor for upgrading client connections
    tls_acceptor: Option<TlsAcceptor>,
    /// TLS connector for connecting to backend servers
    tls_connector: Option<TlsConnector>,
    /// Connection tracker for connection limits
    connection_tracker: Option<Arc<ConnectionTracker>>,
    /// Session ID for gateway mode (credentials lookup)
    session_id: Option<uuid::Uuid>,
    /// Gateway-provided session UID for tunnel grouping
    session_uid: Option<String>,
}

impl PostgresHandler {
    /// Create a new PostgreSQL handler
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

    /// Create a new PostgreSQL handler with connection tracker
    pub fn with_tracker(
        client: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Option<Arc<ConnectionTracker>>,
    ) -> Result<Self> {
        // Get the target configuration for PostgreSQL
        let target = config
            .get_effective_target("postgresql")
            .ok_or_else(|| ProxyError::Config("No PostgreSQL target configured".to_string()))?;

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

    /// Create a new PostgreSQL handler with a session ID for gateway mode.
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
        Err(ProxyError::Config(
            "with_session_id requires target info - use with_session_id_and_target instead".into(),
        ))
    }

    /// Create a new PostgreSQL handler with session ID and target info for gateway mode.
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
        // Create TLS acceptor if TLS is enabled for client connections
        let tls_acceptor = if config.server.tls.enabled {
            let acceptor = TlsAcceptor::new(&config.server.tls).map_err(|e| {
                error!("Failed to create TLS acceptor: {}", e);
                ProxyError::from(e)
            })?;
            debug!("TLS acceptor created for PostgreSQL client connections");
            Some(acceptor)
        } else {
            None
        };

        // Create TLS connector if TLS is enabled for backend connections
        let tls_connector = if target.tls.enabled {
            let connector = TlsConnector::new(&target.tls).map_err(|e| {
                error!("Failed to create TLS connector: {}", e);
                ProxyError::from(e)
            })?;
            debug!("TLS connector created for PostgreSQL backend connections");
            Some(connector)
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
            tls_connector,
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

    /// Take the client stream
    fn take_client(&mut self) -> Result<NetworkStream> {
        self.client
            .take()
            .ok_or_else(|| ProxyError::Connection("Client stream not available".into()))
    }

    /// Handle the PostgreSQL connection
    pub async fn handle(mut self) -> Result<()> {
        let connect_timeout = Duration::from_secs(self.config.server.connect_timeout_secs);

        // Step 1: Receive initial message from client (StartupMessage or SSLRequest)
        let startup = self.receive_client_startup(connect_timeout).await?;
        self.state = HandlerState::ReceivedStartup;

        info!(
            "Client {} connecting as '{}' to database '{}'",
            self.client_addr,
            startup.user().unwrap_or("(unknown)"),
            startup.database().unwrap_or("(default)")
        );

        // Step 2: Get credentials from auth provider
        let mut context = ConnectionContext::new(
            self.client_addr,
            &self.target.host,
            self.target.port,
            DatabaseType::PostgreSQL,
        );

        // Use session_id from gateway handshake if available
        if let Some(session_id) = &self.session_id {
            context = context.with_session_id(session_id.to_string());
        }

        let session_config = self
            .auth_provider
            .get_session_config(&context)
            .await
            .map_err(|e| {
                error!("Failed to get session config: {}", e);
                ProxyError::Config(e.to_string())
            })?;

        let credentials = self.auth_provider.get_auth(&context).await.map_err(|e| {
            error!("Failed to get credentials: {}", e);
            ProxyError::CredentialRetrieval(e.to_string())
        })?;

        let real_username = credentials.method.username().to_string();
        let real_password = credentials
            .method
            .password()
            .ok_or_else(|| ProxyError::Auth("Auth method does not provide a password".into()))?
            .to_string();

        // Step 3: Connect to backend server
        let mut server = self.connect_to_server(connect_timeout).await?;
        self.state = HandlerState::ConnectedToServer;

        // Step 4: Send StartupMessage to server with real credentials
        let server_startup = if let Some(db) = startup.database() {
            StartupMessage::with_database(&real_username, db)
        } else {
            StartupMessage::new(&real_username)
        };
        write_startup_message(&mut server, &server_startup).await?;
        self.state = HandlerState::SentStartup;

        debug!("Sent StartupMessage to server as '{}'", real_username);

        // Step 5: Handle server authentication
        self.authenticate_with_server(&mut server, &real_username, &real_password)
            .await?;
        self.state = HandlerState::Authenticated;

        // Step 6: Check connection limits BEFORE sending AuthenticationOk to client
        // This ensures we can reject the connection with a proper error before the client
        // thinks they're connected
        //
        // Extract client fingerprint from the StartupMessage for security validation.
        // This helps detect if a different tool is trying to hijack the session.
        // PostgreSQL clients have limited identifying info compared to MySQL, so we
        // build a fingerprint from whatever parameters are available.
        //
        // Log all startup parameters for debugging
        debug!(
            startup_params = ?startup.all_parameters(),
            "PostgreSQL startup parameters received"
        );
        let application_name = startup.client_fingerprint();
        if let Some(ref app) = application_name {
            debug!(client_fingerprint = %app, "PostgreSQL client identified");
        } else {
            debug!("PostgreSQL client did not provide any identifiable parameters");
        }
        let (session_id, connection_guard) = self
            .check_connection_limits(&session_config, application_name)
            .await?;

        // Step 7: Send AuthenticationOk to client (only if connection limit check passed)
        // If this fails, we need to release the connection guard
        if let Err(e) = self.send_client_auth_ok().await {
            if let Some((guard, tracker)) = connection_guard {
                guard.release_spawn(tracker);
            }
            return Err(e);
        }

        // Step 8: Forward server parameters and ReadyForQuery to client
        // If this fails, we need to release the connection guard
        if let Err(e) = self.forward_server_ready(&mut server).await {
            if let Some((guard, tracker)) = connection_guard {
                guard.release_spawn(tracker);
            }
            return Err(e);
        }

        debug!(
            "Authentication complete for {} -> {}",
            self.client_addr, self.target.host
        );

        // Step 9: Start relay
        self.state = HandlerState::Relaying;
        self.start_relay(server, session_config, session_id, connection_guard)
            .await
    }

    /// Check connection limits and enforce single-session-per-target security
    /// Returns the session_id and optional connection guard
    ///
    /// Target locking is enforced whenever session_uid is available (Gateway mode),
    /// regardless of whether max_connections is set. This ensures only one session
    /// can access a target at a time even with unlimited connections.
    ///
    /// Application name validation: If the first connection provides an application_name,
    /// subsequent connections must provide the same value. This provides defense-in-depth
    /// against session hijacking by different tools (though it can be spoofed).
    async fn check_connection_limits(
        &mut self,
        session_config: &crate::auth::SessionConfig,
        application_name: Option<String>,
    ) -> Result<(String, Option<(ConnectionGuard, Arc<ConnectionTracker>)>)> {
        let session_uuid = uuid::Uuid::new_v4();
        let session_id = session_uuid.to_string();
        let max_connections = session_config.effective_max_connections();

        // Determine if we need connection tracking:
        // 1. When session_uid is available - for single-session-per-target enforcement
        // 2. When max_connections is set - for connection limit enforcement
        let needs_tracking = self.session_uid.is_some() || max_connections.is_some();

        let connection_guard = if needs_tracking {
            if let Some(ref tracker) = self.connection_tracker {
                // Use session_uid for tunnel grouping if provided (from Gateway),
                // otherwise fall back to connection-based grouping
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

                // Use try_register_for_target when session_uid is available
                // This enforces single-session-per-target security policy
                // even when max_connections is None (unlimited)
                // Also pass application_name for additional security validation
                let result = if let Some(ref uid) = self.session_uid {
                    let target_id = TargetId::new(&self.target.host, self.target.port);
                    tracker
                        .try_register_for_target_with_app(
                            target_id,
                            uid.clone(),
                            tunnel_id.clone(),
                            session_uuid,
                            max_connections, // Can be None for unlimited
                            grace_period,
                            application_name.clone(),
                            session_config.application_lock_timeout,
                        )
                        .await
                } else {
                    tracker
                        .try_register_with_limits_and_app(
                            tunnel_id.clone(),
                            session_uuid,
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
                            "Connection registered"
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
                        self.send_client_error(&e.to_string()).await?;
                        return Err(e);
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok((session_id, connection_guard))
    }

    /// Receive the client's startup message, handling SSLRequest if present
    async fn receive_client_startup(
        &mut self,
        timeout_duration: Duration,
    ) -> Result<StartupMessage> {
        debug!("Waiting for client startup message");

        // Read the initial message (could be SSLRequest or StartupMessage)
        let initial = timeout(timeout_duration, read_startup_message(self.client_mut()?))
            .await
            .map_err(|_| ProxyError::Timeout("Waiting for client startup".into()))??;

        // Check if this is an SSLRequest
        match initial {
            StartupMessageType::SSLRequest => {
                debug!("Client sent SSLRequest");

                if let Some(acceptor) = self.tls_acceptor.take() {
                    // Send 'S' to accept SSL
                    self.client_mut()?.write_all(b"S").await?;
                    debug!("Accepted SSL request, upgrading to TLS");

                    // Upgrade to TLS
                    let current_stream = self.take_client()?;
                    let tcp_stream = match current_stream.into_tcp() {
                        Ok(stream) => stream,
                        Err(_) => {
                            return Err(ProxyError::Protocol(
                                "Cannot upgrade: already using TLS".into(),
                            ));
                        }
                    };

                    let tls_stream = acceptor.accept(tcp_stream).await.map_err(|e| {
                        error!("TLS handshake failed: {}", e);
                        ProxyError::from(e)
                    })?;

                    self.client = Some(NetworkStream::ServerTls(Box::new(tls_stream)));
                    self.state = HandlerState::TlsUpgrade;

                    if let Some(version) = self.client_mut()?.tls_version() {
                        debug!("TLS handshake complete: {}", version);
                    }

                    // Now read the real StartupMessage over TLS
                    let startup_msg =
                        timeout(timeout_duration, read_startup_message(self.client_mut()?))
                            .await
                            .map_err(|_| ProxyError::Timeout("Waiting for TLS startup".into()))??;

                    match startup_msg {
                        StartupMessageType::Startup(msg) => Ok(msg),
                        _ => Err(ProxyError::Protocol(
                            "Expected StartupMessage after TLS".into(),
                        )),
                    }
                } else {
                    // No TLS available - reject
                    self.client_mut()?.write_all(b"N").await?;
                    debug!("Rejected SSL request (TLS not configured)");

                    // Read the actual StartupMessage that follows
                    let startup_msg =
                        timeout(timeout_duration, read_startup_message(self.client_mut()?))
                            .await
                            .map_err(|_| {
                                ProxyError::Timeout("Waiting for startup after SSL reject".into())
                            })??;

                    match startup_msg {
                        StartupMessageType::Startup(msg) => Ok(msg),
                        _ => Err(ProxyError::Protocol(
                            "Expected StartupMessage after SSL reject".into(),
                        )),
                    }
                }
            }
            StartupMessageType::Startup(msg) => Ok(msg),
            StartupMessageType::CancelRequest(_) => Err(ProxyError::Protocol(
                "Cancel requests not supported by proxy".into(),
            )),
        }
    }

    /// Connect to the backend PostgreSQL server, optionally with TLS
    async fn connect_to_server(&self, timeout_duration: Duration) -> Result<NetworkStream> {
        let addr = format!("{}:{}", self.target.host, self.target.port);
        debug!("Connecting to PostgreSQL server at {}", addr);

        let mut stream = timeout(timeout_duration, TcpStream::connect(&addr))
            .await
            .map_err(|_| ProxyError::Timeout(format!("Connecting to {}", addr)))?
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to {}: {}", addr, e)))?;

        debug!("Connected to server at {}", addr);

        // If TLS is configured, negotiate SSL with the server
        if let Some(ref connector) = self.tls_connector {
            debug!("Sending SSLRequest to server");

            // Send SSLRequest: [length: 8][code: 80877103]
            let ssl_request = [
                0u8, 0, 0, 8, // length = 8 (big-endian)
                0x04, 0xD2, 0x16, 0x2F, // SSL request code = 80877103
            ];
            stream.write_all(&ssl_request).await?;
            stream.flush().await?;

            // Read server response (single byte)
            let mut response = [0u8; 1];
            stream.read_exact(&mut response).await?;

            match response[0] {
                b'S' => {
                    debug!("Server accepted SSL, upgrading connection");
                    let tls_stream =
                        connector
                            .connect(stream, &self.target.host)
                            .await
                            .map_err(|e| {
                                error!("TLS handshake with server failed: {}", e);
                                ProxyError::from(e)
                            })?;
                    debug!("TLS connection established with server");
                    return Ok(NetworkStream::ClientTls(Box::new(tls_stream)));
                }
                b'N' => {
                    warn!("Server rejected SSL request, continuing without TLS");
                    // Server doesn't support SSL, continue with plain TCP
                    return Ok(NetworkStream::Tcp(stream));
                }
                other => {
                    return Err(ProxyError::Protocol(format!(
                        "Unexpected SSL response from server: 0x{:02X}",
                        other
                    )));
                }
            }
        }

        Ok(NetworkStream::Tcp(stream))
    }

    /// Authenticate with the backend server
    async fn authenticate_with_server(
        &mut self,
        server: &mut NetworkStream,
        username: &str,
        password: &str,
    ) -> Result<()> {
        self.state = HandlerState::Authenticating;

        loop {
            let (msg_type, payload) = read_message(server).await?;

            match msg_type {
                MSG_AUTH_REQUEST => {
                    let auth_complete = self
                        .handle_auth_message(server, &payload, username, password)
                        .await?;

                    if auth_complete {
                        debug!("Server authentication successful");
                        return Ok(());
                    }
                    // Otherwise continue loop to wait for AuthenticationOk
                }
                MSG_ERROR_RESPONSE => {
                    let error =
                        parse_error_notice(&payload).unwrap_or_else(|_| ErrorNoticeResponse::new());
                    let msg = error.message().unwrap_or("Unknown error");
                    error!("Server authentication failed: {}", msg);
                    self.send_client_error(msg).await?;
                    return Err(ProxyError::Auth(format!(
                        "Server rejected credentials: {}",
                        msg
                    )));
                }
                _ => {
                    warn!("Unexpected message during auth: type={}", msg_type as char);
                }
            }
        }
    }

    /// Handle an authentication message from the server
    /// Returns true if authentication is complete (AuthenticationOk received)
    async fn handle_auth_message(
        &mut self,
        server: &mut NetworkStream,
        payload: &[u8],
        username: &str,
        password: &str,
    ) -> Result<bool> {
        if payload.len() < 4 {
            return Err(ProxyError::Protocol("Invalid auth message".into()));
        }

        let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);

        match auth_type {
            AUTH_OK => {
                debug!("AuthenticationOk received");
                Ok(true) // Auth complete
            }
            AUTH_CLEARTEXT_PASSWORD => {
                debug!("Server requested cleartext password");
                self.send_password(server, password).await?;
                Ok(false) // Need to wait for AuthenticationOk
            }
            AUTH_MD5_PASSWORD => {
                if payload.len() < 8 {
                    return Err(ProxyError::Protocol("Invalid MD5 auth message".into()));
                }
                let salt: [u8; 4] = [payload[4], payload[5], payload[6], payload[7]];
                debug!("Server requested MD5 password");
                let md5_password = compute_md5_password(username, password, &salt);
                self.send_password(server, &md5_password).await?;
                Ok(false) // Need to wait for AuthenticationOk
            }
            AUTH_SASL => {
                debug!("Server requested SASL authentication");
                self.handle_scram_auth(server, payload, username, password)
                    .await?;
                Ok(true) // SCRAM handler reads AuthenticationOk internally
            }
            AUTH_SASL_CONTINUE | AUTH_SASL_FINAL => {
                // These are handled within handle_scram_auth
                Err(ProxyError::Protocol("Unexpected SASL message".into()))
            }
            _ => {
                warn!("Unsupported auth type: {}", auth_type);
                Err(ProxyError::UnsupportedProtocol(format!(
                    "Auth type {} not supported",
                    auth_type
                )))
            }
        }
    }

    /// Handle SCRAM-SHA-256 authentication
    async fn handle_scram_auth(
        &mut self,
        server: &mut NetworkStream,
        initial_payload: &[u8],
        username: &str,
        password: &str,
    ) -> Result<()> {
        // Parse available mechanisms from AuthenticationSASL
        let mechanisms_str = std::str::from_utf8(&initial_payload[4..])
            .map_err(|_| ProxyError::Protocol("Invalid SASL mechanisms".into()))?;

        let mechanisms: Vec<&str> = mechanisms_str
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect();
        debug!("Server SASL mechanisms: {:?}", mechanisms);

        if !mechanisms.contains(&"SCRAM-SHA-256") {
            return Err(ProxyError::UnsupportedProtocol(
                "Server does not support SCRAM-SHA-256".into(),
            ));
        }

        // Create SCRAM client and generate client-first message
        let mut scram = ScramClient::new(username, password);
        let client_first = scram.client_first_message();

        // Build and send SASLInitialResponse
        let sasl_response = SASLInitialResponse {
            mechanism: SASL_MECHANISM_SCRAM_SHA_256.to_string(),
            data: client_first,
        };
        let response_payload = build_sasl_initial_response(&sasl_response);
        write_message(server, MSG_PASSWORD, &response_payload).await?;
        debug!("Sent SASLInitialResponse");

        // Read AuthenticationSASLContinue
        let (msg_type, payload) = read_message(server).await?;
        if msg_type != MSG_AUTH_REQUEST {
            return Err(ProxyError::Protocol("Expected auth message".into()));
        }

        let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        if auth_type != AUTH_SASL_CONTINUE {
            return Err(ProxyError::Protocol("Expected SASL continue".into()));
        }

        let server_first = &payload[4..];
        debug!("Received server-first-message");

        // Process server-first and generate client-final
        let client_final = scram.process_server_first(server_first)?;

        // Send client-final as password message
        write_message(server, MSG_PASSWORD, &client_final).await?;
        debug!("Sent client-final-message");

        // Read AuthenticationSASLFinal
        let (msg_type, payload) = read_message(server).await?;
        if msg_type != MSG_AUTH_REQUEST {
            return Err(ProxyError::Protocol("Expected auth message".into()));
        }

        let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        if auth_type != AUTH_SASL_FINAL {
            // Could be AUTH_OK if server skips final verification
            if auth_type == AUTH_OK {
                debug!("Server sent AuthenticationOk without SASL final");
                return Ok(());
            }
            return Err(ProxyError::Protocol("Expected SASL final".into()));
        }

        let server_final = &payload[4..];
        scram.verify_server_final(server_final)?;
        debug!("SCRAM authentication verified");

        // Now wait for AuthenticationOk
        let (msg_type, payload) = read_message(server).await?;
        if msg_type != MSG_AUTH_REQUEST {
            return Err(ProxyError::Protocol("Expected AuthenticationOk".into()));
        }

        let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        if auth_type != AUTH_OK {
            return Err(ProxyError::Auth("Authentication not completed".into()));
        }

        debug!("SCRAM-SHA-256 authentication complete");
        Ok(())
    }

    /// Send a password message to the server
    async fn send_password(&mut self, server: &mut NetworkStream, password: &str) -> Result<()> {
        let msg = PasswordMessage::from_password(password);
        let payload = build_password_message(&msg);
        write_message(server, MSG_PASSWORD, &payload).await?;
        debug!("Sent password message");
        Ok(())
    }

    /// Send AuthenticationOk to the client
    async fn send_client_auth_ok(&mut self) -> Result<()> {
        let payload = build_auth_ok();
        write_message(self.client_mut()?, MSG_AUTH_REQUEST, &payload).await?;
        debug!("Sent AuthenticationOk to client");
        Ok(())
    }

    /// Forward server parameters and ReadyForQuery to client
    async fn forward_server_ready(&mut self, server: &mut NetworkStream) -> Result<()> {
        loop {
            let (msg_type, payload) = read_message(server).await?;

            match msg_type {
                MSG_PARAMETER_STATUS => {
                    // Forward parameter status to client
                    write_message(self.client_mut()?, msg_type, &payload).await?;
                    if let Ok(param) = codec_parse_parameter_status(&payload) {
                        debug!("Forwarded parameter: {}={}", param.name, param.value);
                    }
                }
                MSG_BACKEND_KEY_DATA => {
                    // Forward backend key data to client
                    write_message(self.client_mut()?, msg_type, &payload).await?;
                    debug!("Forwarded BackendKeyData");
                }
                MSG_READY_FOR_QUERY => {
                    // Forward ReadyForQuery to client
                    write_message(self.client_mut()?, msg_type, &payload).await?;
                    debug!("Forwarded ReadyForQuery - connection ready");
                    return Ok(());
                }
                MSG_ERROR_RESPONSE => {
                    // Forward error and fail
                    write_message(self.client_mut()?, msg_type, &payload).await?;
                    let error =
                        parse_error_notice(&payload).unwrap_or_else(|_| ErrorNoticeResponse::new());
                    return Err(ProxyError::Protocol(format!(
                        "Server error during setup: {}",
                        error.message().unwrap_or("Unknown error")
                    )));
                }
                MSG_NOTICE_RESPONSE => {
                    // Forward notices
                    write_message(self.client_mut()?, msg_type, &payload).await?;
                    debug!("Forwarded notice");
                }
                _ => {
                    warn!("Unexpected message during setup: type={}", msg_type as char);
                }
            }
        }
    }

    /// Send an error to the client
    async fn send_client_error(&mut self, message: &str) -> Result<()> {
        let error = ErrorNoticeResponse::error("ERROR", SQLSTATE_INVALID_AUTHORIZATION, message);
        let payload = codec_build_error_response(&error);
        write_message(self.client_mut()?, MSG_ERROR_RESPONSE, &payload).await?;
        Ok(())
    }

    /// Start the relay session
    async fn start_relay(
        mut self,
        server: NetworkStream,
        session_config: crate::auth::SessionConfig,
        session_id: String,
        connection_guard: Option<(ConnectionGuard, Arc<ConnectionTracker>)>,
    ) -> Result<()> {
        // Take the client stream - connection limits were already checked in handle()
        let client_stream = self.take_client()?;

        // Create session manager with the pre-generated session_id
        let mut session_manager =
            SessionManager::with_session_id(session_config.clone(), session_id.clone());

        // Start session timers
        session_manager.start_timers();

        // Use ManagedSession if limits are configured
        let has_limits = session_config.max_duration.is_some()
            || session_config.idle_timeout.is_some()
            || session_config.max_queries.is_some();

        if has_limits {
            let managed = ManagedSession::with_network_server(
                client_stream,
                server,
                session_manager,
                self.config.server.idle_timeout_secs,
            );
            let reason = managed.relay().await;

            if let Some((guard, tracker)) = connection_guard {
                guard.release_spawn(tracker);
            }

            debug!(
                session_id = %session_id,
                reason = ?reason,
                "PostgreSQL session ended"
            );
            Ok(())
        } else {
            let session = Session::with_network_streams(
                client_stream,
                server,
                self.config.server.idle_timeout_secs,
            );
            let result = session.relay().await;

            if let Some((guard, tracker)) = connection_guard {
                guard.release_spawn(tracker);
            }

            result
        }
    }
}

// ============================================================================
// Helper Functions for Message Building
// ============================================================================

/// Build AuthenticationOk payload
fn build_auth_ok() -> Vec<u8> {
    AUTH_OK.to_be_bytes().to_vec()
}

/// Build a password message payload
fn build_password_message(msg: &PasswordMessage) -> Vec<u8> {
    // The PasswordMessage.data already includes the null terminator if created with from_password()
    msg.data.clone()
}

/// Build a SASL initial response payload
fn build_sasl_initial_response(msg: &SASLInitialResponse) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(msg.mechanism.as_bytes());
    payload.push(0); // Null terminator
    payload.extend_from_slice(&(msg.data.len() as i32).to_be_bytes());
    payload.extend_from_slice(&msg.data);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_auth_ok() {
        let payload = build_auth_ok();
        assert_eq!(payload, vec![0, 0, 0, 0]);
    }

    #[test]
    fn test_build_password_message() {
        let msg = PasswordMessage::from_password("secret");
        let payload = build_password_message(&msg);
        assert_eq!(payload, b"secret\0");
    }

    #[test]
    fn test_build_sasl_initial_response() {
        let msg = SASLInitialResponse {
            mechanism: "SCRAM-SHA-256".to_string(),
            data: b"n,,n=user,r=nonce".to_vec(),
        };
        let payload = build_sasl_initial_response(&msg);

        // Verify mechanism is null-terminated
        assert!(payload.starts_with(b"SCRAM-SHA-256\0"));

        // Verify length is correct (4 bytes big-endian)
        let len_offset = "SCRAM-SHA-256".len() + 1;
        let len_bytes = &payload[len_offset..len_offset + 4];
        let len = i32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
        assert_eq!(len, 17); // "n,,n=user,r=nonce".len()
    }

    #[test]
    fn test_build_error_response() {
        let error = ErrorNoticeResponse::error("ERROR", "28000", "Invalid authorization");
        let payload = codec_build_error_response(&error);

        // Should contain severity field
        assert!(payload.contains(&ERROR_FIELD_SEVERITY));
        // Should contain code field
        assert!(payload.contains(&ERROR_FIELD_CODE));
        // Should contain message field
        assert!(payload.contains(&ERROR_FIELD_MESSAGE));
        // Should end with null terminator
        assert!(payload.ends_with(&[0]));
    }

    #[test]
    fn test_parse_error_response() {
        let mut payload = Vec::new();
        payload.push(ERROR_FIELD_SEVERITY);
        payload.extend_from_slice(b"ERROR\0");
        payload.push(ERROR_FIELD_CODE);
        payload.extend_from_slice(b"28P01\0");
        payload.push(ERROR_FIELD_MESSAGE);
        payload.extend_from_slice(b"password authentication failed\0");
        payload.push(0);

        let error = parse_error_notice(&payload).unwrap();
        assert_eq!(error.severity(), Some("ERROR"));
        assert_eq!(error.code(), Some("28P01"));
        assert_eq!(error.message(), Some("password authentication failed"));
    }

    #[test]
    fn test_parse_parameter_status() {
        let payload = b"server_version\x00140003\x00";
        let param = codec_parse_parameter_status(payload).unwrap();
        assert_eq!(param.name, "server_version");
        assert_eq!(param.value, "140003");
    }
}
