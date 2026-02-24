//! Connection handler for database proxy connections

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tracing::{span, Level};

use super::connection_tracker::ConnectionTracker;
use super::handlers::{MySQLHandler, OracleHandler, PostgresHandler, SqlServerHandler};
use crate::auth::AuthProvider;
use crate::config::Config;
use crate::error::{ProxyError, Result};
use crate::handshake::{CredentialStore, HandshakeAuthProvider, HandshakeHandler};
use crate::protocol::{DetectedProtocol, ProtocolDetector};

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state, detecting protocol
    Detecting,
    /// Protocol detected, authenticating
    Authenticating,
    /// Authenticated, relaying traffic
    Relaying,
    /// Connection closed
    Closed,
}

/// A database proxy connection
pub struct Connection {
    /// Client TCP stream
    stream: TcpStream,
    /// Client address
    client_addr: SocketAddr,
    /// Configuration
    config: Arc<Config>,
    /// Authentication provider
    auth_provider: Arc<dyn AuthProvider>,
    /// Connection state
    state: ConnectionState,
    /// Shutdown signal receiver
    shutdown_rx: broadcast::Receiver<()>,
    /// Connection tracker for single-connection enforcement
    connection_tracker: Arc<ConnectionTracker>,
    /// Credential store for gateway handshake mode (None = settings mode)
    credential_store: Option<Arc<CredentialStore>>,
}

impl Connection {
    /// Create a new connection in settings mode (credentials provided at config time).
    pub fn new(
        stream: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Arc<ConnectionTracker>,
    ) -> Self {
        Self {
            stream,
            client_addr,
            config,
            auth_provider,
            state: ConnectionState::Detecting,
            shutdown_rx,
            connection_tracker,
            credential_store: None,
        }
    }

    /// Create a new connection in gateway mode (credentials via handshake).
    pub fn new_gateway_mode(
        stream: TcpStream,
        client_addr: SocketAddr,
        config: Arc<Config>,
        credential_store: Arc<CredentialStore>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Arc<ConnectionTracker>,
    ) -> Self {
        // Create HandshakeAuthProvider wrapping the store
        let auth_provider = Arc::new(HandshakeAuthProvider::new(Arc::clone(&credential_store)));

        Self {
            stream,
            client_addr,
            config,
            auth_provider,
            state: ConnectionState::Detecting,
            shutdown_rx,
            connection_tracker,
            credential_store: Some(credential_store),
        }
    }

    /// Handle the connection
    pub async fn handle(mut self) -> Result<()> {
        let span =
            span!(target: "keeperdb_proxy", Level::INFO, "connection", client = %self.client_addr);
        let _enter = span.enter();

        debug!("New connection");

        // Detect protocol
        self.state = ConnectionState::Detecting;
        let detector = ProtocolDetector::default();
        let (protocol, peeked_data) = detector.detect(&self.stream).await;

        info!(
            "Detected protocol: {} (first bytes: {:02X?})",
            protocol,
            &peeked_data[..std::cmp::min(8, peeked_data.len())]
        );

        // Route based on detected protocol
        match protocol {
            DetectedProtocol::GatewayHandshake => self.handle_handshake().await,
            DetectedProtocol::MySQL => self.handle_mysql().await,
            DetectedProtocol::PostgreSQL => self.handle_postgres().await,
            DetectedProtocol::SQLServer => self.handle_sqlserver().await,
            DetectedProtocol::Oracle => self.handle_oracle().await,
            DetectedProtocol::Unknown => {
                if !peeked_data.is_empty() {
                    warn!(
                        "Unknown protocol, first bytes: {:02X?}",
                        &peeked_data[..std::cmp::min(8, peeked_data.len())]
                    );
                } else {
                    warn!("Unknown protocol, no data received");
                }
                Err(ProxyError::UnsupportedProtocol("Unknown".into()))
            }
        }
    }

    /// Handle MySQL protocol
    async fn handle_mysql(self) -> Result<()> {
        let handler = MySQLHandler::with_tracker(
            self.stream,
            self.client_addr,
            self.config,
            self.auth_provider,
            self.shutdown_rx,
            Some(self.connection_tracker),
        )?;
        handler.handle().await
    }

    /// Handle PostgreSQL protocol
    async fn handle_postgres(self) -> Result<()> {
        let handler = PostgresHandler::with_tracker(
            self.stream,
            self.client_addr,
            self.config,
            self.auth_provider,
            self.shutdown_rx,
            Some(self.connection_tracker),
        )?;
        handler.handle().await
    }

    /// Handle SQL Server protocol
    async fn handle_sqlserver(self) -> Result<()> {
        let handler = SqlServerHandler::with_tracker(
            self.stream,
            self.client_addr,
            self.config,
            self.auth_provider,
            self.shutdown_rx,
            Some(self.connection_tracker),
        )?;
        handler.handle().await
    }

    /// Handle Oracle protocol
    async fn handle_oracle(self) -> Result<()> {
        let handler = OracleHandler::with_tracker(
            self.stream,
            self.client_addr,
            self.config,
            self.auth_provider,
            self.shutdown_rx,
            Some(self.connection_tracker),
        )?;
        handler.handle().await
    }

    /// Handle Gateway handshake protocol.
    ///
    /// This is used in gateway mode where credentials are delivered via handshake.
    /// After the handshake completes, the connection proceeds with the database
    /// protocol (MySQL or PostgreSQL) using the credentials from the handshake.
    async fn handle_handshake(self) -> Result<()> {
        let store = self.credential_store.clone().ok_or_else(|| {
            ProxyError::Config("Handshake received but not in gateway mode".into())
        })?;

        info!(client = %self.client_addr, "Starting Gateway handshake handler");

        // Perform handshake, passing config defaults for session settings
        let handshake_handler = HandshakeHandler::with_config(
            self.stream,
            Arc::clone(&store),
            self.config.session.clone(),
        );
        let (result, stream) = handshake_handler.handle().await?;

        info!(
            client = %self.client_addr,
            session_id = %result.session_id,
            database_type = ?result.database_type,
            target = %result.target_host,
            "Handshake completed, routing to database handler"
        );

        // Create a new auth provider that will look up credentials by session_id
        let auth_provider = Arc::new(HandshakeAuthProvider::new(store));

        // Route to appropriate database handler based on handshake result
        match result.database_type {
            crate::auth::DatabaseType::MySQL => {
                let handler = MySQLHandler::with_session_id_and_target(
                    stream,
                    self.client_addr,
                    self.config,
                    auth_provider,
                    self.shutdown_rx,
                    Some(self.connection_tracker),
                    result.session_id,
                    result.target_host,
                    result.target_port,
                    result.session_uid,
                )?;
                handler.handle().await
            }
            crate::auth::DatabaseType::PostgreSQL => {
                let handler = PostgresHandler::with_session_id_and_target(
                    stream,
                    self.client_addr,
                    self.config,
                    auth_provider,
                    self.shutdown_rx,
                    Some(self.connection_tracker),
                    result.session_id,
                    result.target_host,
                    result.target_port,
                    result.session_uid,
                )?;
                handler.handle().await
            }
            crate::auth::DatabaseType::SQLServer => {
                let handler = SqlServerHandler::with_session_id_and_target(
                    stream,
                    self.client_addr,
                    self.config,
                    auth_provider,
                    self.shutdown_rx,
                    Some(self.connection_tracker),
                    result.session_id,
                    result.target_host,
                    result.target_port,
                    result.session_uid,
                )?;
                handler.handle().await
            }
            crate::auth::DatabaseType::Oracle => {
                let handler = OracleHandler::with_session_id_and_target(
                    stream,
                    self.client_addr,
                    self.config,
                    auth_provider,
                    self.shutdown_rx,
                    Some(self.connection_tracker),
                    result.session_id,
                    result.target_host.clone(),
                    result.target_port,
                    result.session_uid,
                    result.tls_config,
                    result.database,
                )?;
                handler.handle().await
            }
        }
    }
}
