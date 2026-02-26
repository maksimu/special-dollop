//! Session relay for bidirectional traffic forwarding
//!
//! This module provides session relay capabilities including:
//! - Basic bidirectional relay ([`Session`])
//! - Managed relay with security controls ([`ManagedSession`])

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{split, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;

use super::super::stream::NetworkStream;
use super::manager::{DisconnectReason, SessionManager};
use crate::error::Result;
use crate::protocol::mysql::packets::{is_query_command, COM_QUIT};
use crate::query_logging::config::ConnectionLoggingConfig;
use crate::query_logging::extractor::{
    MysqlQueryExtractor, OracleQueryExtractor, PostgresQueryExtractor, QueryExtractor, QueryInfo,
    TdsQueryExtractor,
};
use crate::query_logging::log_entry::{QueryLog, SessionContext};
use crate::query_logging::service::{QueryLogSender, QueryLoggingService};

/// Context for query logging in the relay loop.
///
/// Bundles the protocol-specific query extractor, the log sender,
/// and session metadata needed to create log entries.
pub struct QueryLogContext {
    /// Protocol-specific query/response extractor.
    pub extractor: Box<dyn QueryExtractor>,
    /// Sender for submitting log entries to the background service.
    pub sender: QueryLogSender,
    /// Session metadata for populating log entries.
    pub session: SessionContext,
}

impl QueryLogContext {
    /// Create a QueryLogContext for the given protocol and logging config.
    ///
    /// Returns `None` if logging is disabled or service creation fails.
    /// This is the primary factory method used by protocol handlers to
    /// wire up query logging.
    pub fn create(
        protocol: &str,
        config: &ConnectionLoggingConfig,
        session_id: &str,
        user_id: &str,
        database: &str,
        client_addr: &str,
        server_addr: &str,
    ) -> Option<Self> {
        let sender = QueryLoggingService::start(config)?;

        let extractor: Box<dyn QueryExtractor> = match protocol {
            "mysql" => Box::new(MysqlQueryExtractor::new(
                config.include_query_text,
                config.max_query_length,
            )),
            "postgresql" | "postgres" => Box::new(PostgresQueryExtractor::new(
                config.include_query_text,
                config.max_query_length,
            )),
            "sqlserver" | "mssql" => Box::new(TdsQueryExtractor::new(
                config.include_query_text,
                config.max_query_length,
            )),
            "oracle" => Box::new(OracleQueryExtractor::new()),
            _ => {
                warn!("No query extractor for protocol: {}", protocol);
                return None;
            }
        };

        let session = SessionContext {
            session_id: session_id.to_string(),
            user_id: user_id.to_string(),
            database: database.to_string(),
            protocol: protocol.to_string(),
            client_addr: client_addr.to_string(),
            server_addr: server_addr.to_string(),
        };

        Some(Self {
            extractor,
            sender,
            session,
        })
    }
}

/// A relay session between client and server
///
/// Supports both plain TCP streams and TLS-encrypted NetworkStreams.
pub struct Session<C, S> {
    /// Client stream
    client: C,
    /// Server stream
    server: S,
    /// Idle timeout (0 = disabled)
    idle_timeout: Duration,
}

impl Session<TcpStream, TcpStream> {
    /// Create a new relay session with TCP streams
    pub fn new(client: TcpStream, server: TcpStream) -> Self {
        Self {
            client,
            server,
            idle_timeout: Duration::ZERO,
        }
    }

    /// Create a new relay session with TCP streams and idle timeout
    pub fn tcp_with_idle_timeout(
        client: TcpStream,
        server: TcpStream,
        idle_timeout_secs: u64,
    ) -> Self {
        Self {
            client,
            server,
            idle_timeout: Duration::from_secs(idle_timeout_secs),
        }
    }
}

impl Session<NetworkStream, TcpStream> {
    /// Create a new relay session with NetworkStream client and TCP server
    pub fn with_idle_timeout(
        client: NetworkStream,
        server: TcpStream,
        idle_timeout_secs: u64,
    ) -> Self {
        Self {
            client,
            server,
            idle_timeout: Duration::from_secs(idle_timeout_secs),
        }
    }
}

impl Session<NetworkStream, NetworkStream> {
    /// Create a new relay session with NetworkStream for both client and server
    pub fn with_network_streams(
        client: NetworkStream,
        server: NetworkStream,
        idle_timeout_secs: u64,
    ) -> Self {
        Self {
            client,
            server,
            idle_timeout: Duration::from_secs(idle_timeout_secs),
        }
    }
}

impl<C, S> Session<C, S>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Run the bidirectional relay
    ///
    /// This copies data in both directions until either side closes
    /// the connection, an error occurs, or the idle timeout is reached.
    pub async fn relay(self) -> Result<()> {
        debug!(
            "Starting relay session with idle_timeout={:?}",
            self.idle_timeout
        );
        let (client_read, client_write) = split(self.client);
        let (server_read, server_write) = split(self.server);
        let idle_timeout = self.idle_timeout;

        // Spawn tasks for each direction
        let client_to_server = tokio::spawn(copy_with_logging(
            client_read,
            server_write,
            "client->server",
            idle_timeout,
        ));

        let server_to_client = tokio::spawn(copy_with_logging(
            server_read,
            client_write,
            "server->client",
            idle_timeout,
        ));

        // Wait for either direction to complete
        tokio::select! {
            result = client_to_server => {
                debug!("Client to server copy finished: {:?}", result);
            }
            result = server_to_client => {
                debug!("Server to client copy finished: {:?}", result);
            }
        }

        debug!("Relay session ended");
        Ok(())
    }
}

/// Copy data from reader to writer with logging and optional idle timeout
async fn copy_with_logging<R, W>(
    mut reader: R,
    mut writer: W,
    direction: &'static str,
    idle_timeout: Duration,
) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 8192];
    let mut total_bytes = 0u64;

    loop {
        // Read with optional timeout
        let n = if idle_timeout.is_zero() {
            // No timeout configured
            reader.read(&mut buf).await?
        } else {
            // Apply idle timeout
            match timeout(idle_timeout, reader.read(&mut buf)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    debug!(
                        "{}: Idle timeout ({:?}) after {} bytes",
                        direction, idle_timeout, total_bytes
                    );
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("Idle timeout after {:?}", idle_timeout),
                    ));
                }
            }
        };

        if n == 0 {
            debug!("{}: EOF after {} bytes", direction, total_bytes);
            break;
        }

        trace!("{}: {} bytes", direction, n);
        writer.write_all(&buf[..n]).await?;
        writer.flush().await?;
        total_bytes += n as u64;
    }

    // Shutdown write side
    let _ = writer.shutdown().await;

    Ok(total_bytes)
}

// ============================================================================
// ManagedSession - Session relay with security controls
// ============================================================================

/// A managed relay session with security controls.
///
/// This session type integrates with [`SessionManager`] to enforce:
/// - Query counting (for max_queries limit)
/// - Idle timeout (disconnects inactive sessions)
/// - Disconnect signals (from max_duration timer or explicit trigger)
///
/// Unlike the basic [`Session`], this processes MySQL packets to identify
/// and count query commands.
pub struct ManagedSession<S = TcpStream> {
    /// Client stream (NetworkStream for TLS support)
    client: NetworkStream,
    /// Server stream (TCP or TLS connection to database)
    server: S,
    /// Session manager for limits and signals
    session_manager: Arc<Mutex<SessionManager>>,
    /// Default idle timeout from config (used if session config doesn't override)
    default_idle_timeout: Duration,
    /// Optional query logging context
    query_log_ctx: Option<QueryLogContext>,
}

impl ManagedSession<TcpStream> {
    /// Create a new managed session with TCP server.
    ///
    /// # Arguments
    ///
    /// * `client` - Client stream (may be TLS-encrypted)
    /// * `server` - Server TCP stream
    /// * `session_manager` - Manager for session limits and disconnect signals
    /// * `default_idle_timeout_secs` - Default idle timeout in seconds
    pub fn new(
        client: NetworkStream,
        server: TcpStream,
        session_manager: SessionManager,
        default_idle_timeout_secs: u64,
    ) -> Self {
        Self {
            client,
            server,
            session_manager: Arc::new(Mutex::new(session_manager)),
            default_idle_timeout: Duration::from_secs(default_idle_timeout_secs),
            query_log_ctx: None,
        }
    }
}

impl ManagedSession<NetworkStream> {
    /// Create a new managed session with NetworkStream server (for TLS backends).
    ///
    /// # Arguments
    ///
    /// * `client` - Client stream (may be TLS-encrypted)
    /// * `server` - Server NetworkStream (may be TLS-encrypted)
    /// * `session_manager` - Manager for session limits and disconnect signals
    /// * `default_idle_timeout_secs` - Default idle timeout in seconds
    pub fn with_network_server(
        client: NetworkStream,
        server: NetworkStream,
        session_manager: SessionManager,
        default_idle_timeout_secs: u64,
    ) -> Self {
        Self {
            client,
            server,
            session_manager: Arc::new(Mutex::new(session_manager)),
            default_idle_timeout: Duration::from_secs(default_idle_timeout_secs),
            query_log_ctx: None,
        }
    }
}

impl<S> ManagedSession<S> {
    /// Enable query logging for this session.
    ///
    /// When set, the relay loop will extract queries and responses from
    /// the protocol traffic and submit log entries to the background service.
    pub fn with_query_logging(mut self, ctx: QueryLogContext) -> Self {
        self.query_log_ctx = Some(ctx);
        self
    }
}

impl<S> ManagedSession<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Run the managed relay.
    ///
    /// This relay:
    /// 1. Counts query commands from client
    /// 2. Monitors for disconnect signals (max_duration, etc.)
    /// 3. Applies idle timeout to client reads
    /// 4. Sends ERR packet to client on forced disconnect
    ///
    /// # Returns
    ///
    /// The disconnect reason when the session ends.
    pub async fn relay(self) -> DisconnectReason {
        // Get disconnect receiver and idle timeout from session manager
        let (disconnect_rx, idle_timeout) = {
            let mut mgr = self.session_manager.lock().await;
            let rx = mgr.take_disconnect_rx();
            let timeout = mgr.get_idle_timeout().unwrap_or(self.default_idle_timeout);
            (rx, timeout)
        };

        let session_id = {
            let mgr = self.session_manager.lock().await;
            mgr.session_id()
        };

        debug!(
            session_id = %session_id,
            idle_timeout = ?idle_timeout,
            "Starting managed relay"
        );

        // Split streams
        let (client_read, client_write) = split(self.client);
        let (server_read, server_write) = split(self.server);

        // Run the managed relay loop
        let reason = managed_relay_loop(
            client_read,
            client_write,
            server_read,
            server_write,
            self.session_manager,
            disconnect_rx,
            idle_timeout,
            self.query_log_ctx,
        )
        .await;

        debug!(
            session_id = %session_id,
            reason = ?reason,
            "Managed relay ended"
        );

        reason
    }
}

/// Internal relay loop with query counting, disconnect monitoring, and query logging.
///
/// # Query Counting Limitations
///
/// This implementation uses a simplified approach to query counting that assumes
/// one MySQL command packet per read operation. This is accurate for typical
/// MySQL client behavior (request-response pattern) but may under-count queries
/// in edge cases:
///
/// - **Pipelined commands**: If a client sends multiple commands without waiting
///   for responses, only the first command in each read buffer is counted.
/// - **Large queries spanning multiple reads**: Handled correctly (command byte
///   is always in the first read).
///
/// This trade-off was chosen for simplicity and performance. The max_queries
/// limit is a "best effort" security control. For strict query accounting,
/// full MySQL packet parsing with buffering would be required.
///
/// # Query Logging
///
/// When `query_log_ctx` is provided, the relay loop will:
/// 1. Extract query info from client-to-server traffic via the protocol extractor
/// 2. Extract response info from server-to-client traffic
/// 3. Create `QueryLog` entries with timing and submit via `try_send()` (non-blocking)
#[allow(clippy::too_many_arguments)]
async fn managed_relay_loop<CR, CW, SR, SW>(
    mut client_read: CR,
    mut client_write: CW,
    mut server_read: SR,
    mut server_write: SW,
    session_manager: Arc<Mutex<SessionManager>>,
    disconnect_rx: Option<tokio::sync::oneshot::Receiver<DisconnectReason>>,
    idle_timeout: Duration,
    query_log_ctx: Option<QueryLogContext>,
) -> DisconnectReason
where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    SR: AsyncRead + Unpin,
    SW: AsyncWrite + Unpin,
{
    let mut client_buf = vec![0u8; 8192];
    let mut server_buf = vec![0u8; 8192];
    let mut disconnect_rx = disconnect_rx;
    let mut pending_disconnect: Option<DisconnectReason> = None;

    // Query logging state: VecDeque handles pipelined queries where multiple
    // queries may be in-flight before responses arrive.
    let mut pending_queries: VecDeque<(QueryInfo, std::time::Instant)> = VecDeque::new();
    // Destructure the query log context for use in the loop
    let (extractor, log_sender, session_ctx) = match query_log_ctx {
        Some(ctx) => (Some(ctx.extractor), Some(ctx.sender), Some(ctx.session)),
        None => (None, None, None),
    };

    let disconnect_reason = loop {
        tokio::select! {
            // Check for disconnect signal (if we have a receiver)
            reason = async {
                match &mut disconnect_rx {
                    Some(rx) => rx.await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(reason) = reason {
                    debug!("Received disconnect signal: {:?}", reason);
                    // Send ERR packet if there's an error code
                    if let Some(code) = reason.mysql_error_code() {
                        let _ = send_err_packet(&mut client_write, code, &reason.message()).await;
                    }
                    break reason;
                }
            }

            // Read from client with idle timeout
            result = timeout(idle_timeout, client_read.read(&mut client_buf)) => {
                match result {
                    Ok(Ok(0)) => {
                        // Client closed connection
                        debug!("Client EOF");
                        break DisconnectReason::ClientDisconnect;
                    }
                    Ok(Ok(n)) => {
                        trace!("client->server: {} bytes", n);

                        // Check for MySQL command byte (first byte of payload after packet header)
                        // MySQL packet: 3 bytes length + 1 byte seq + payload
                        // If we have at least 5 bytes, byte 4 is the command
                        if n >= 5 {
                            let cmd = client_buf[4];

                            // Check if this is a quit command
                            if cmd == COM_QUIT {
                                debug!("Client sent COM_QUIT");
                                let _ = server_write.write_all(&client_buf[..n]).await;
                                let _ = server_write.flush().await;
                                break DisconnectReason::ClientDisconnect;
                            }

                            // Count query commands
                            if is_query_command(cmd) {
                                let mut mgr = session_manager.lock().await;
                                if let Err(reason) = mgr.increment_query_count() {
                                    debug!("Query limit exceeded: {:?}", reason);
                                    // Mark for disconnect after forwarding this query
                                    pending_disconnect = Some(reason);
                                }
                            }
                        }

                        // Query logging: extract query info from client data
                        if let Some(ref extractor) = extractor {
                            if let Some(query_info) = extractor.extract_query(&client_buf[..n]) {
                                pending_queries.push_back((query_info, std::time::Instant::now()));
                            }
                        }

                        // Forward to server
                        if let Err(e) = server_write.write_all(&client_buf[..n]).await {
                            warn!("Error writing to server: {}", e);
                            break DisconnectReason::IoError(e.to_string());
                        }
                        if let Err(e) = server_write.flush().await {
                            warn!("Error flushing to server: {}", e);
                            break DisconnectReason::IoError(e.to_string());
                        }

                        // If we have a pending disconnect, send ERR and exit
                        if let Some(reason) = pending_disconnect.take() {
                            if let Some(code) = reason.mysql_error_code() {
                                let _ = send_err_packet(&mut client_write, code, &reason.message()).await;
                            }
                            break reason;
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("Error reading from client: {}", e);
                        break DisconnectReason::IoError(e.to_string());
                    }
                    Err(_) => {
                        // Idle timeout
                        debug!("Client idle timeout ({:?})", idle_timeout);
                        let reason = DisconnectReason::IdleTimeout { duration: idle_timeout };
                        if let Some(code) = reason.mysql_error_code() {
                            let _ = send_err_packet(&mut client_write, code, &reason.message()).await;
                        }
                        break reason;
                    }
                }
            }

            // Read from server (no timeout - server can be slow)
            result = server_read.read(&mut server_buf) => {
                match result {
                    Ok(0) => {
                        // Server closed connection
                        debug!("Server EOF");
                        break DisconnectReason::ServerDisconnect;
                    }
                    Ok(n) => {
                        trace!("server->client: {} bytes", n);

                        // Query logging: extract response and create log entry
                        if let (Some(ref extractor), Some(ref sender), Some(ref ctx)) =
                            (&extractor, &log_sender, &session_ctx)
                        {
                            if let Some((query_info, start_time)) = pending_queries.pop_front() {
                                if let Some(response_info) =
                                    extractor.extract_response(&server_buf[..n])
                                {
                                    let duration_ms =
                                        start_time.elapsed().as_millis() as u64;
                                    let entry = QueryLog {
                                        timestamp: chrono::Utc::now(),
                                        session_id: ctx.session_id.clone(),
                                        user_id: ctx.user_id.clone(),
                                        database: ctx.database.clone(),
                                        protocol: ctx.protocol.clone(),
                                        command_type: query_info.command_type,
                                        query_text: query_info.query_text,
                                        duration_ms,
                                        rows_affected: response_info.rows_affected,
                                        success: response_info.success,
                                        error_message: response_info.error_message,
                                        client_addr: ctx.client_addr.clone(),
                                        server_addr: ctx.server_addr.clone(),
                                    };
                                    if !sender.try_send(entry) {
                                        debug!("Query log entry dropped (channel full or closed)");
                                    }
                                } else {
                                    // Response didn't contain result metadata yet
                                    // (e.g., intermediate result set packets).
                                    // Put the pending query back at the front.
                                    pending_queries.push_front((query_info, start_time));
                                }
                            }
                        }

                        // Forward to client
                        if let Err(e) = client_write.write_all(&server_buf[..n]).await {
                            warn!("Error writing to client: {}", e);
                            break DisconnectReason::IoError(e.to_string());
                        }
                        if let Err(e) = client_write.flush().await {
                            warn!("Error flushing to client: {}", e);
                            break DisconnectReason::IoError(e.to_string());
                        }
                    }
                    Err(e) => {
                        warn!("Error reading from server: {}", e);
                        break DisconnectReason::IoError(e.to_string());
                    }
                }
            }
        }
    };

    // Gracefully shut down query logging, ensuring all buffered entries are flushed.
    if let Some(sender) = log_sender {
        sender.shutdown().await;
    }

    disconnect_reason
}

/// Send a MySQL ERR packet to the client.
async fn send_err_packet<W: AsyncWrite + Unpin>(
    writer: &mut W,
    error_code: u16,
    message: &str,
) -> std::io::Result<()> {
    // Build MySQL ERR packet
    // Format: 0xFF + error_code (2) + '#' + sql_state (5) + message
    let mut payload = Vec::with_capacity(9 + message.len());
    payload.push(0xFF); // ERR packet marker
    payload.extend_from_slice(&error_code.to_le_bytes());
    payload.push(b'#'); // SQL state marker
    payload.extend_from_slice(b"HY000"); // Generic SQL state
    payload.extend_from_slice(message.as_bytes());

    // Build packet header: 3 bytes length + 1 byte seq (use 1)
    let len = payload.len() as u32;
    let header = [
        (len & 0xFF) as u8,
        ((len >> 8) & 0xFF) as u8,
        ((len >> 16) & 0xFF) as u8,
        1u8, // Sequence ID
    ];

    writer.write_all(&header).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;

    debug!("Sent ERR packet: code={}, message={}", error_code, message);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn test_copy_with_logging() {
        let (mut client, server) = duplex(64);

        // Write some data
        let data = b"hello world";
        client.write_all(data).await.unwrap();
        drop(client); // Close to signal EOF

        // Copy should work - server implements AsyncRead (no timeout)
        let bytes = copy_with_logging(server, tokio::io::sink(), "test", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(bytes, data.len() as u64);
    }

    #[tokio::test]
    async fn test_copy_with_timeout() {
        let (mut client, server) = duplex(64);

        // Write some data
        let data = b"hello world";
        client.write_all(data).await.unwrap();
        drop(client); // Close to signal EOF

        // Copy with timeout should work
        let bytes = copy_with_logging(server, tokio::io::sink(), "test", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(bytes, data.len() as u64);
    }

    #[tokio::test]
    async fn test_copy_timeout_triggers() {
        let (_client, server) = duplex(64);
        // Don't write anything and don't close - this should timeout

        // Copy with short timeout should fail
        let result =
            copy_with_logging(server, tokio::io::sink(), "test", Duration::from_millis(50)).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn test_send_err_packet() {
        let (mut client, mut server) = duplex(256);

        // Send error packet
        send_err_packet(&mut client, 1317, "Query interrupted")
            .await
            .unwrap();
        drop(client);

        // Read and verify
        let mut buf = vec![0u8; 256];
        let n = server.read(&mut buf).await.unwrap();

        // Verify packet structure
        assert!(n > 4); // Header + payload
        assert_eq!(buf[4], 0xFF); // ERR marker
        assert_eq!(u16::from_le_bytes([buf[5], buf[6]]), 1317); // Error code
        assert_eq!(&buf[7..8], b"#"); // SQL state marker
        assert_eq!(&buf[8..13], b"HY000"); // SQL state
    }

    #[tokio::test]
    async fn test_managed_relay_idle_timeout() {
        use crate::auth::SessionConfig;

        let (client, _client_server) = duplex(256);
        let (server, _server_client) = duplex(256);

        let config = SessionConfig::default();
        let session_manager = SessionManager::new(config);
        let session_manager = Arc::new(Mutex::new(session_manager));

        let disconnect_rx = {
            let mut mgr = session_manager.lock().await;
            mgr.take_disconnect_rx()
        };

        // Run relay with very short idle timeout
        let (client_read, client_write) = split(client);
        let (server_read, server_write) = split(server);

        let reason = managed_relay_loop(
            client_read,
            client_write,
            server_read,
            server_write,
            session_manager,
            disconnect_rx,
            Duration::from_millis(50), // Very short timeout
            None,                      // No query logging
        )
        .await;

        assert!(matches!(reason, DisconnectReason::IdleTimeout { .. }));
    }

    #[tokio::test]
    async fn test_managed_relay_client_disconnect() {
        use crate::auth::SessionConfig;

        let (client, _client_server) = duplex(256);
        let (server, _server_client) = duplex(256);

        // Close client immediately
        drop(client);

        let config = SessionConfig::default();
        let session_manager = SessionManager::new(config);
        let session_manager = Arc::new(Mutex::new(session_manager));

        let disconnect_rx = {
            let mut mgr = session_manager.lock().await;
            mgr.take_disconnect_rx()
        };

        let (client_read, client_write) = split(_client_server);
        let (server_read, server_write) = split(server);

        let reason = managed_relay_loop(
            client_read,
            client_write,
            server_read,
            server_write,
            session_manager,
            disconnect_rx,
            Duration::from_secs(30),
            None, // No query logging
        )
        .await;

        assert!(matches!(reason, DisconnectReason::ClientDisconnect));
    }

    #[tokio::test]
    async fn test_managed_relay_with_query_logging() {
        use crate::auth::SessionConfig;
        use crate::protocol::mysql::packets::COM_QUERY;
        use crate::query_logging::config::ConnectionLoggingConfig;
        use crate::query_logging::extractor::MysqlQueryExtractor;
        use crate::query_logging::service::QueryLoggingService;

        let dir = tempfile::tempdir().unwrap();
        let pipe_path = dir.path().join("query.pipe");
        std::fs::File::create(&pipe_path).unwrap();
        let config = ConnectionLoggingConfig {
            query_logging_enabled: true,
            include_query_text: true,
            max_query_length: 10_000,
            pipe_path: Some(pipe_path.to_string_lossy().to_string()),
        };

        let sender = QueryLoggingService::start(&config).unwrap();
        let extractor = MysqlQueryExtractor::new(true, 10_000);
        let session_ctx = SessionContext {
            session_id: "relay-test".to_string(),
            user_id: "testuser".to_string(),
            database: "testdb".to_string(),
            protocol: "mysql".to_string(),
            client_addr: "127.0.0.1:12345".to_string(),
            server_addr: "10.0.0.1:3306".to_string(),
        };
        let query_log_ctx = QueryLogContext {
            extractor: Box::new(extractor),
            sender: sender.clone(),
            session: session_ctx,
        };

        // Create duplex streams simulating client and server
        let (mut client_writer, client_read_end) = duplex(4096);
        let (server_write_end, mut server_reader) = duplex(4096);

        let session_config = SessionConfig::default();
        let session_manager = SessionManager::new(session_config);
        let session_manager = Arc::new(Mutex::new(session_manager));

        let disconnect_rx = {
            let mut mgr = session_manager.lock().await;
            mgr.take_disconnect_rx()
        };

        let (client_read, client_write) = split(client_read_end);
        let (server_read, server_write) = split(server_write_end);

        // Spawn the relay loop
        let relay_handle = tokio::spawn(async move {
            managed_relay_loop(
                client_read,
                client_write,
                server_read,
                server_write,
                session_manager,
                disconnect_rx,
                Duration::from_secs(5),
                Some(query_log_ctx),
            )
            .await
        });

        // Send a MySQL COM_QUERY "SELECT 1"
        let query = b"SELECT 1";
        let payload_len = 1 + query.len(); // cmd byte + query
        let mut pkt = vec![
            (payload_len & 0xFF) as u8,
            ((payload_len >> 8) & 0xFF) as u8,
            0, // length high byte
            0, // sequence id
            COM_QUERY,
        ];
        pkt.extend_from_slice(query);
        client_writer.write_all(&pkt).await.unwrap();

        // Read the forwarded query from the "server" side
        let mut buf = vec![0u8; 256];
        let n = server_reader.read(&mut buf).await.unwrap();
        assert_eq!(n, pkt.len()); // Query was forwarded

        // Send a MySQL OK response back from "server"
        let ok_payload = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]; // OK + 0 affected
        let ok_len = ok_payload.len() as u32;
        let mut ok_pkt = vec![
            (ok_len & 0xFF) as u8,
            ((ok_len >> 8) & 0xFF) as u8,
            0,
            1, // sequence id
        ];
        ok_pkt.extend_from_slice(&ok_payload);
        server_reader.write_all(&ok_pkt).await.unwrap();

        // Give the relay loop time to process
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Close client to end the relay
        drop(client_writer);
        let reason = relay_handle.await.unwrap();
        assert!(matches!(reason, DisconnectReason::ClientDisconnect));

        // Shutdown the logging service
        sender.shutdown().await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify that the query was logged to the pipe file
        let content = std::fs::read_to_string(&pipe_path).unwrap();
        assert!(
            content.contains("SELECT 1"),
            "Log should contain query text"
        );
        assert!(
            content.contains("\"success\":true"),
            "Log should show success"
        );
    }
}
