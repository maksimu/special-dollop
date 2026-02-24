//! Session relay for bidirectional traffic forwarding
//!
//! This module provides session relay capabilities including:
//! - Basic bidirectional relay ([`Session`])
//! - Managed relay with security controls ([`ManagedSession`])

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
        }
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

/// Internal relay loop with query counting and disconnect monitoring.
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
async fn managed_relay_loop<CR, CW, SR, SW>(
    mut client_read: CR,
    mut client_write: CW,
    mut server_read: SR,
    mut server_write: SW,
    session_manager: Arc<Mutex<SessionManager>>,
    disconnect_rx: Option<tokio::sync::oneshot::Receiver<DisconnectReason>>,
    idle_timeout: Duration,
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

    loop {
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
                    return reason;
                }
            }

            // Read from client with idle timeout
            result = timeout(idle_timeout, client_read.read(&mut client_buf)) => {
                match result {
                    Ok(Ok(0)) => {
                        // Client closed connection
                        debug!("Client EOF");
                        return DisconnectReason::ClientDisconnect;
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
                                return DisconnectReason::ClientDisconnect;
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

                        // Forward to server
                        if let Err(e) = server_write.write_all(&client_buf[..n]).await {
                            warn!("Error writing to server: {}", e);
                            return DisconnectReason::IoError(e.to_string());
                        }
                        if let Err(e) = server_write.flush().await {
                            warn!("Error flushing to server: {}", e);
                            return DisconnectReason::IoError(e.to_string());
                        }

                        // If we have a pending disconnect, send ERR and exit
                        if let Some(reason) = pending_disconnect.take() {
                            if let Some(code) = reason.mysql_error_code() {
                                let _ = send_err_packet(&mut client_write, code, &reason.message()).await;
                            }
                            return reason;
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("Error reading from client: {}", e);
                        return DisconnectReason::IoError(e.to_string());
                    }
                    Err(_) => {
                        // Idle timeout
                        debug!("Client idle timeout ({:?})", idle_timeout);
                        let reason = DisconnectReason::IdleTimeout { duration: idle_timeout };
                        if let Some(code) = reason.mysql_error_code() {
                            let _ = send_err_packet(&mut client_write, code, &reason.message()).await;
                        }
                        return reason;
                    }
                }
            }

            // Read from server (no timeout - server can be slow)
            result = server_read.read(&mut server_buf) => {
                match result {
                    Ok(0) => {
                        // Server closed connection
                        debug!("Server EOF");
                        return DisconnectReason::ServerDisconnect;
                    }
                    Ok(n) => {
                        trace!("server->client: {} bytes", n);
                        // Forward to client
                        if let Err(e) = client_write.write_all(&server_buf[..n]).await {
                            warn!("Error writing to client: {}", e);
                            return DisconnectReason::IoError(e.to_string());
                        }
                        if let Err(e) = client_write.flush().await {
                            warn!("Error flushing to client: {}", e);
                            return DisconnectReason::IoError(e.to_string());
                        }
                    }
                    Err(e) => {
                        warn!("Error reading from server: {}", e);
                        return DisconnectReason::IoError(e.to_string());
                    }
                }
            }
        }
    }
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
        )
        .await;

        assert!(matches!(reason, DisconnectReason::ClientDisconnect));
    }
}
