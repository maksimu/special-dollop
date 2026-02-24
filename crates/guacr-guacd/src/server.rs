// Server-side guacd wire protocol implementation
//
// Handles incoming TCP connections using the Guacamole handshake protocol,
// dispatching to registered protocol handlers.

use bytes::Bytes;
use guacr_handlers::{HandlerError, ProtocolHandlerRegistry};
use guacr_protocol::{format_instruction, GuacamoleParser};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::args::{get_protocol_arg_names, get_protocol_args};

/// Default guacd port
pub const GUACD_DEFAULT_PORT: u16 = 4822;

/// Protocol version supported
pub const GUACAMOLE_PROTOCOL_VERSION: &str = "VERSION_1_5_0";

/// Maximum instruction size (64KB) - prevents memory exhaustion attacks
pub const MAX_INSTRUCTION_SIZE: usize = 64 * 1024;

/// Default handshake timeout (30 seconds)
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 30;

/// Handshake error types
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Unknown protocol: {0}")]
    UnknownProtocol(String),

    #[error("Missing required argument: {0}")]
    MissingArgument(String),

    #[error("Handler error: {0}")]
    Handler(#[from] HandlerError),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,
}

/// Result of parsing a select instruction
#[derive(Debug)]
pub struct SelectResult {
    /// Protocol name (e.g., "ssh", "rdp", "vnc")
    pub protocol: String,
    /// Connection ID if joining existing connection
    pub connection_id: Option<String>,
}

/// Result of parsing a connect instruction
#[derive(Debug)]
pub struct ConnectResult {
    /// Connection parameters as key-value pairs
    pub params: HashMap<String, String>,
}

/// Guacd-compatible handshake handler
pub struct GuacdHandshake<S> {
    stream: BufReader<S>,
    protocol: Option<String>,
    arg_names: Vec<&'static str>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> GuacdHandshake<S> {
    /// Create a new handshake handler
    pub fn new(stream: S) -> Self {
        Self {
            stream: BufReader::new(stream),
            protocol: None,
            arg_names: Vec::new(),
        }
    }

    /// Read a single Guacamole instruction from the stream
    ///
    /// Includes protection against memory exhaustion by limiting instruction size
    /// to MAX_INSTRUCTION_SIZE (64KB).
    async fn read_instruction(
        &mut self,
    ) -> std::result::Result<(String, Vec<String>), HandshakeError> {
        let mut buf = String::new();

        // Read until semicolon, with size limit to prevent memory exhaustion
        loop {
            let byte_buf = self.stream.fill_buf().await?;
            if byte_buf.is_empty() {
                return Err(HandshakeError::ConnectionClosed);
            }

            // Check size limit before adding more data
            if buf.len() + byte_buf.len() > MAX_INSTRUCTION_SIZE {
                return Err(HandshakeError::Protocol(format!(
                    "Instruction exceeds maximum size of {} bytes",
                    MAX_INSTRUCTION_SIZE
                )));
            }

            // Find semicolon in buffer
            if let Some(pos) = byte_buf.iter().position(|&b| b == b';') {
                let chunk = std::str::from_utf8(&byte_buf[..=pos])
                    .map_err(|_| HandshakeError::Protocol("Invalid UTF-8".to_string()))?;
                buf.push_str(chunk);
                self.stream.consume(pos + 1);
                break;
            } else {
                let chunk = std::str::from_utf8(byte_buf)
                    .map_err(|_| HandshakeError::Protocol("Invalid UTF-8".to_string()))?;
                buf.push_str(chunk);
                let len = byte_buf.len();
                self.stream.consume(len);
            }
        }

        // Parse the instruction
        let bytes = Bytes::from(buf);
        let instr = GuacamoleParser::parse_instruction(&bytes)
            .map_err(|e| HandshakeError::Parse(e.to_string()))?;

        Ok((
            instr.opcode.to_string(),
            instr.args.iter().map(|s| s.to_string()).collect(),
        ))
    }

    /// Write a Guacamole instruction to the stream
    async fn write_instruction(
        &mut self,
        opcode: &str,
        args: &[&str],
    ) -> std::result::Result<(), HandshakeError> {
        let instr = format_instruction(opcode, args);
        let writer = self.stream.get_mut();
        writer.write_all(instr.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Wait for and process the select instruction
    pub async fn read_select(&mut self) -> std::result::Result<SelectResult, HandshakeError> {
        let (opcode, args) = self.read_instruction().await?;

        if opcode != "select" {
            return Err(HandshakeError::Protocol(format!(
                "Expected 'select' instruction, got '{}'",
                opcode
            )));
        }

        if args.is_empty() {
            return Err(HandshakeError::Protocol(
                "select instruction requires protocol argument".to_string(),
            ));
        }

        let protocol = args[0].clone();

        // Check if this is a connection ID (joining existing connection)
        // Connection IDs start with "$" in Guacamole
        let connection_id = if protocol.starts_with('$') {
            Some(protocol.clone())
        } else {
            None
        };

        // Store protocol for later
        self.protocol = Some(protocol.clone());

        // Get arg names for this protocol
        self.arg_names = get_protocol_arg_names(&protocol);

        Ok(SelectResult {
            protocol,
            connection_id,
        })
    }

    /// Send the args instruction listing required parameters
    pub async fn send_args(&mut self) -> std::result::Result<(), HandshakeError> {
        let protocol = self.protocol.as_ref().ok_or_else(|| {
            HandshakeError::Protocol("Must call read_select before send_args".to_string())
        })?;

        // Get arg names for protocol
        let arg_names = get_protocol_arg_names(protocol);
        if arg_names.is_empty() {
            return Err(HandshakeError::UnknownProtocol(protocol.clone()));
        }

        self.arg_names = arg_names.clone();

        // Send args instruction
        self.write_instruction("args", &arg_names).await?;

        Ok(())
    }

    /// Read the connect instruction and extract parameters
    pub async fn read_connect(&mut self) -> std::result::Result<ConnectResult, HandshakeError> {
        let (opcode, args) = self.read_instruction().await?;

        if opcode != "connect" {
            return Err(HandshakeError::Protocol(format!(
                "Expected 'connect' instruction, got '{}'",
                opcode
            )));
        }

        // Map arg values to names
        let mut params = HashMap::new();
        for (i, value) in args.iter().enumerate() {
            if i < self.arg_names.len() && !value.is_empty() {
                params.insert(self.arg_names[i].to_string(), value.clone());
            }
        }

        // Validate required arguments
        if let Some(protocol) = &self.protocol {
            if let Some(descriptors) = get_protocol_args(protocol) {
                for desc in descriptors {
                    if desc.required && !params.contains_key(desc.name) {
                        return Err(HandshakeError::MissingArgument(desc.name.to_string()));
                    }
                }
            }
        }

        Ok(ConnectResult { params })
    }

    /// Send the ready instruction with connection ID
    pub async fn send_ready(
        &mut self,
        connection_id: &str,
    ) -> std::result::Result<(), HandshakeError> {
        self.write_instruction("ready", &[connection_id]).await
    }

    /// Send an error instruction
    pub async fn send_error(
        &mut self,
        message: &str,
        status: &str,
    ) -> std::result::Result<(), HandshakeError> {
        self.write_instruction("error", &[message, status]).await
    }

    /// Consume the handshake handler and return the underlying stream
    pub fn into_inner(self) -> S {
        self.stream.into_inner()
    }

    /// Get the negotiated protocol name
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }
}

/// Guacd status codes for error responses
pub mod status {
    pub const SUCCESS: &str = "0";
    pub const UNSUPPORTED: &str = "256";
    pub const SERVER_ERROR: &str = "512";
    pub const SERVER_BUSY: &str = "513";
    pub const UPSTREAM_TIMEOUT: &str = "514";
    pub const UPSTREAM_ERROR: &str = "515";
    pub const RESOURCE_NOT_FOUND: &str = "516";
    pub const RESOURCE_CONFLICT: &str = "517";
    pub const RESOURCE_CLOSED: &str = "518";
    pub const UPSTREAM_NOT_FOUND: &str = "519";
    pub const UPSTREAM_UNAVAILABLE: &str = "520";
    pub const SESSION_CONFLICT: &str = "521";
    pub const SESSION_TIMEOUT: &str = "522";
    pub const SESSION_CLOSED: &str = "523";
    pub const CLIENT_BAD_REQUEST: &str = "768";
    pub const CLIENT_UNAUTHORIZED: &str = "769";
    pub const CLIENT_FORBIDDEN: &str = "771";
    pub const CLIENT_TIMEOUT: &str = "776";
    pub const CLIENT_OVERRUN: &str = "781";
    pub const CLIENT_BAD_TYPE: &str = "783";
    pub const CLIENT_TOO_MANY: &str = "797";
}

/// Handle to a running guacd-compatible server
pub struct GuacdServer {
    shutdown_tx: broadcast::Sender<()>,
    server_handle: JoinHandle<()>,
    active_connections: Arc<AtomicUsize>,
    local_addr: std::net::SocketAddr,
}

impl GuacdServer {
    /// Bind to an address and start accepting connections
    pub async fn bind(
        addr: &str,
        registry: Arc<ProtocolHandlerRegistry>,
    ) -> std::result::Result<Self, HandshakeError> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        log::info!("guacd-compatible server listening on {}", local_addr);

        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let active_connections = Arc::new(AtomicUsize::new(0));

        let server_handle = {
            let shutdown_tx = shutdown_tx.clone();
            let active_connections = Arc::clone(&active_connections);

            tokio::spawn(async move {
                let mut shutdown_rx = shutdown_tx.subscribe();

                loop {
                    tokio::select! {
                        biased;

                        _ = shutdown_rx.recv() => {
                            log::info!("guacd server received shutdown signal");
                            break;
                        }

                        result = listener.accept() => {
                            match result {
                                Ok((socket, peer_addr)) => {
                                    log::debug!("Accepted connection from {}", peer_addr);

                                    let registry = Arc::clone(&registry);
                                    let active_connections = Arc::clone(&active_connections);
                                    let mut conn_shutdown_rx = shutdown_tx.subscribe();

                                    active_connections.fetch_add(1, Ordering::SeqCst);

                                    tokio::spawn(async move {
                                        tokio::select! {
                                            result = handle_guacd_connection(socket, registry) => {
                                                if let Err(e) = result {
                                                    log::error!("Connection error from {}: {}", peer_addr, e);
                                                }
                                            }
                                            _ = conn_shutdown_rx.recv() => {
                                                log::debug!("Connection from {} interrupted by shutdown", peer_addr);
                                            }
                                        }

                                        active_connections.fetch_sub(1, Ordering::SeqCst);
                                        log::debug!("Connection from {} closed", peer_addr);
                                    });
                                }
                                Err(e) => {
                                    log::error!("Failed to accept connection: {}", e);
                                }
                            }
                        }
                    }
                }

                log::info!("guacd server stopped accepting new connections");
            })
        };

        Ok(Self {
            shutdown_tx,
            server_handle,
            active_connections,
            local_addr,
        })
    }

    /// Get the number of active connections
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    /// Get the local address the server is bound to
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.local_addr
    }

    /// Initiate graceful shutdown
    pub async fn shutdown(self) {
        log::info!(
            "Initiating guacd server shutdown ({} active connections)",
            self.active_connections()
        );
        let _ = self.shutdown_tx.send(());
        let _ = self.server_handle.await;
        log::info!("guacd server shutdown complete");
    }

    /// Initiate graceful shutdown with timeout for connections to drain
    pub async fn shutdown_with_drain(self, timeout: Duration) {
        log::info!(
            "Initiating guacd server shutdown with {:?} drain timeout ({} active connections)",
            timeout,
            self.active_connections()
        );

        let _ = self.shutdown_tx.send(());

        let active_connections = Arc::clone(&self.active_connections);
        let drain_result = tokio::time::timeout(timeout, async move {
            while active_connections.load(Ordering::SeqCst) > 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;

        if drain_result.is_err() {
            log::warn!(
                "Drain timeout reached, {} connections still active",
                self.active_connections()
            );
        }

        let _ = self.server_handle.await;
        log::info!("guacd server shutdown complete");
    }

    /// Stop accepting new connections but let existing ones finish
    pub fn stop_accepting(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Run a guacd-compatible TCP server (blocking)
///
/// Listens on the specified address and handles Guacamole protocol handshakes,
/// delegating to the appropriate protocol handler from the registry.
///
/// This function runs forever. For controllable shutdown, use [`GuacdServer::bind`] instead.
pub async fn run_guacd_server(
    addr: &str,
    registry: Arc<ProtocolHandlerRegistry>,
) -> std::result::Result<(), HandshakeError> {
    let listener = TcpListener::bind(addr).await?;
    log::info!("guacd-compatible server listening on {}", addr);

    loop {
        let (socket, peer_addr) = listener.accept().await?;
        log::debug!("Accepted connection from {}", peer_addr);

        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            if let Err(e) = handle_guacd_connection(socket, registry).await {
                log::error!("Connection error from {}: {}", peer_addr, e);
            }
        });
    }
}

/// Handle a single guacd connection
///
/// Performs the handshake and delegates to the appropriate protocol handler.
/// The handshake phase has a timeout to prevent slowloris-style attacks.
pub async fn handle_guacd_connection(
    socket: TcpStream,
    registry: Arc<ProtocolHandlerRegistry>,
) -> std::result::Result<(), HandshakeError> {
    handle_guacd_connection_with_timeout(
        socket,
        registry,
        Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
    )
    .await
}

/// Handle a single guacd connection with custom handshake timeout
pub async fn handle_guacd_connection_with_timeout(
    socket: TcpStream,
    registry: Arc<ProtocolHandlerRegistry>,
    handshake_timeout: Duration,
) -> std::result::Result<(), HandshakeError> {
    // Wrap handshake in timeout to prevent slowloris attacks
    let handshake_result = tokio::time::timeout(handshake_timeout, async {
        let mut handshake = GuacdHandshake::new(socket);

        // 1. Read select instruction
        let select = handshake.read_select().await?;
        let safe_protocol: String = select
            .protocol
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        log::debug!("Client selected protocol: {}", safe_protocol);

        // Check if protocol is supported
        if !registry.has(&select.protocol) {
            handshake
                .send_error(
                    &format!("Unsupported protocol: {}", safe_protocol),
                    status::UNSUPPORTED,
                )
                .await?;
            return Err(HandshakeError::UnknownProtocol(select.protocol));
        }

        // 2. Send args instruction
        handshake.send_args().await?;

        // 3. Read connect instruction
        let connect = handshake.read_connect().await?;
        log::debug!("Received connection parameters for {}", safe_protocol);

        // 4. Generate connection ID and send ready
        let connection_id = format!("${}", uuid::Uuid::new_v4());
        handshake.send_ready(&connection_id).await?;

        Ok((handshake, select, connect))
    })
    .await;

    let (handshake, select, connect) = match handshake_result {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(HandshakeError::Timeout),
    };

    // 5. Get handler and start session
    let handler = registry
        .get(&select.protocol)
        .ok_or_else(|| HandshakeError::UnknownProtocol(select.protocol.clone()))?;

    // Create channels for bidirectional communication
    let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(256);
    let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(256);

    // Get the underlying stream for the interactive phase
    let stream = handshake.into_inner();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut read_half = BufReader::new(read_half);

    // Spawn task to read from client and send to handler
    let read_task = tokio::spawn(async move {
        let mut buf = String::new();
        loop {
            buf.clear();
            loop {
                let byte_buf = match read_half.fill_buf().await {
                    Ok(b) => b,
                    Err(_) => return,
                };
                if byte_buf.is_empty() {
                    return;
                }

                if let Some(pos) = byte_buf.iter().position(|&b| b == b';') {
                    if let Ok(chunk) = std::str::from_utf8(&byte_buf[..=pos]) {
                        buf.push_str(chunk);
                    }
                    read_half.consume(pos + 1);
                    break;
                } else {
                    if let Ok(chunk) = std::str::from_utf8(byte_buf) {
                        buf.push_str(chunk);
                    }
                    let len = byte_buf.len();
                    read_half.consume(len);
                }
            }

            if from_client_tx.send(Bytes::from(buf.clone())).await.is_err() {
                return;
            }
        }
    });

    // Spawn task to write handler output to client
    let write_task = tokio::spawn(async move {
        while let Some(data) = to_client_rx.recv().await {
            if write_half.write_all(&data).await.is_err() {
                return;
            }
            if write_half.flush().await.is_err() {
                return;
            }
        }
    });

    // Run the handler
    // Note: to_client_tx is consumed by connect() and dropped when it returns,
    // which closes the channel and lets write_task drain naturally.
    let handler_result = handler
        .connect(connect.params, to_client_tx, from_client_rx)
        .await;

    // Clean up: abort read_task immediately (no more input needed),
    // but give write_task a short window to flush any pending instructions
    // (e.g. the disconnect opcode sent by the handler before returning).
    // Without this the WebRTC crate sees unexpected EOF instead of a clean disconnect.
    read_task.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), write_task).await;

    match handler_result {
        Ok(()) => Ok(()),
        Err(HandlerError::Disconnected(_)) => Ok(()),
        Err(e) => Err(HandshakeError::Handler(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handshake_mock() {
        use tokio::io::duplex;

        let (client, server) = duplex(1024);
        let mut handshake = GuacdHandshake::new(server);

        // Simulate client sending select
        let mut client = client;
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            client.write_all(b"6.select,3.ssh;").await.unwrap();
            client.flush().await.unwrap();
        });

        // Read select on server side
        let select = handshake.read_select().await.unwrap();
        assert_eq!(select.protocol, "ssh");
        assert!(select.connection_id.is_none());
    }
}
