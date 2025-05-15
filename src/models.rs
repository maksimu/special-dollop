use std::fmt;
use std::io;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Duration;
use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use anyhow::Result;

// Static empty bytes for when empty responses are needed
pub(crate) static EMPTY_BYTES: once_cell::sync::Lazy<Bytes> = once_cell::sync::Lazy::new(|| Bytes::new());

// Protocol handler trait for supporting different protocols
pub trait ProtocolHandler {
    /// Initialize the protocol handler
    fn initialize(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Process incoming data from the client and determine where to send it
    fn process_client_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>>;

    /// Process incoming data from the server and determine where to send it
    fn process_server_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>>;

    /// Handle protocol-specific commands or state changes
    fn handle_command<'a>(&'a mut self, command: &'a str, params: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    /// Handle a new connection
    fn handle_new_connection(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>>;

    /// Handle EOF on a connection
    fn handle_eof(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>>;

    /// Handle a connection close message
    fn handle_close(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>>;

    /// Set the WebRTC channel for sending data
    fn set_webrtc_channel(&mut self, channel: crate::webrtc_data_channel::WebRTCDataChannel);

    /// Notifies the handler when WebRTC channel state changes
    /// This allows handlers to reconnect and send pending messages when the channel becomes available
    fn on_webrtc_channel_state_change(&mut self, state: &str) -> BoxFuture<'_, Result<()>>;

    /// Process any pending messages if the WebRTC channel is available
    fn process_pending_messages(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Shutdown protocol handler
    fn shutdown(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Get handler status
    fn status(&self) -> String;
}

// Connection statistics
#[derive(Debug, Default, Clone)]
pub(crate) struct ConnectionStats {
    pub(crate) receive_size: usize,
    pub(crate) transfer_size: usize,
    pub(crate) receive_latency_sum: u64,
    pub(crate) receive_latency_count: u64,
    pub(crate) transfer_latency_sum: u64,
    pub(crate) transfer_latency_count: u64,
    pub(crate) ping_time: Option<u64>,
    pub(crate) message_counter: u32,
}

impl ConnectionStats {
    // Calculate average receive latency
    pub fn receive_latency_avg(&self) -> u64 {
        if self.receive_latency_count == 0 {
            0
        } else {
            self.receive_latency_sum / self.receive_latency_count
        }
    }
    
    // Calculate average transfer latency
    pub fn transfer_latency_avg(&self) -> u64 {
        if self.transfer_latency_count == 0 {
            0
        } else {
            self.transfer_latency_sum / self.transfer_latency_count
        }
    }
}

// Trait for async read/write operations
pub(crate) trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {
    fn shutdown(&mut self) -> BoxFuture<'_, io::Result<()>>;
}

// Implement AsyncReadWrite for Box<dyn AsyncReadWrite>
impl<T: ?Sized + AsyncReadWrite> AsyncReadWrite for Box<T> {
    fn shutdown(&mut self) -> BoxFuture<'_, io::Result<()>> {
        (**self).shutdown()
    }
}

// Stream wrapper for split streams
pub(crate) struct StreamHalf {
    pub(crate) reader: Option<OwnedReadHalf>,
    pub(crate) writer: OwnedWriteHalf,
}

impl AsyncRead for StreamHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(reader) = &mut self.get_mut().reader {
            Pin::new(reader).poll_read(cx, buf)
        } else {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Read half is not available",
            )))
        }
    }
}


impl AsyncWrite for StreamHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().writer).poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_shutdown(cx)
    }
}

impl AsyncReadWrite for StreamHalf {
    fn shutdown(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move { self.writer.shutdown().await })
    }
}

impl AsyncReadWrite for TcpStream {
    fn shutdown(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move { AsyncWriteExt::shutdown(self).await })
    }
}

/// Per‑connection state.
pub(crate) struct Conn {
    pub(crate) backend: Box<dyn AsyncReadWrite>,
    pub(crate) to_webrtc: JoinHandle<()>,
    pub(crate) stats: ConnectionStats,
}

/// Tunnel timeout configuration
#[derive(Debug, Clone)]
pub struct TunnelTimeouts {
    pub read: Duration,
    pub ping_timeout: Duration,
    pub open_connection: Duration,
    pub close_connection: Duration,
}

impl Default for TunnelTimeouts {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(15),
            ping_timeout: Duration::from_secs(5),
            open_connection: Duration::from_secs(10),
            close_connection: Duration::from_secs(5),
        }
    }
}

/// Network access checker
#[derive(Debug, Clone)]
pub struct NetworkAccessChecker {
    allowed_hosts: Vec<String>,
    allowed_ports: Vec<u16>,
}

impl NetworkAccessChecker {
    pub fn new(allowed_hosts: Vec<String>, allowed_ports: Vec<u16>) -> Self {
        Self { allowed_hosts, allowed_ports }
    }

    pub fn is_host_allowed(&self, host: &str) -> bool {
        if self.allowed_hosts.is_empty() {
            return true;
        }
        self.allowed_hosts.iter().any(|h| {
            if h.starts_with("*.") {
                host.ends_with(&h[1..])
            } else {
                host == h
            }
        })
    }

    pub fn is_port_allowed(&self, port: u16) -> bool {
        if self.allowed_ports.is_empty() {
            return true;
        }
        self.allowed_ports.contains(&port)
    }
    
    // Getter for allowed hosts
    pub fn allowed_hosts(&self) -> &Vec<String> {
        &self.allowed_hosts
    }
    
    // Getter for allowed ports
    pub fn allowed_ports(&self) -> &Vec<u16> {
        &self.allowed_ports
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ConversationType {
    Tunnel,
    Ssh,
    Rdp,
    Vnc,
    Http,
    Kubernetes,
    Telnet,
    Mysql,
    SqlServer,
    Postgresql,
}

// Implement Display for enum -> string conversion
impl fmt::Display for ConversationType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConversationType::Tunnel => write!(f, "tunnel"),
            ConversationType::Ssh => write!(f, "ssh"),
            ConversationType::Rdp => write!(f, "rdp"),
            ConversationType::Vnc => write!(f, "vnc"),
            ConversationType::Http => write!(f, "http"),
            ConversationType::Kubernetes => write!(f, "kubernetes"),
            ConversationType::Telnet => write!(f, "telnet"),
            ConversationType::Mysql => write!(f, "mysql"),
            ConversationType::SqlServer => write!(f, "sql-server"),
            ConversationType::Postgresql => write!(f, "postgresql"),
        }
    }
}

// Custom error type for string parsing failures
#[derive(Debug, Clone, PartialEq)]
pub struct ParseConversationTypeError;

impl fmt::Display for ParseConversationTypeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "failed to parse conversation type")
    }
}

// Implement FromStr for string -> enum conversion
impl FromStr for ConversationType {
    type Err = ParseConversationTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tunnel" => Ok(ConversationType::Tunnel),
            "ssh" => Ok(ConversationType::Ssh),
            "rdp" => Ok(ConversationType::Rdp),
            "vnc" => Ok(ConversationType::Vnc),
            "http" => Ok(ConversationType::Http),
            "kubernetes" => Ok(ConversationType::Kubernetes),
            "telnet" => Ok(ConversationType::Telnet),
            "mysql" => Ok(ConversationType::Mysql),
            "sql-server" => Ok(ConversationType::SqlServer),
            "postgresql" => Ok(ConversationType::Postgresql),
            _ => Err(ParseConversationTypeError),
        }
    }
}

pub fn is_guacd_session(conversation_type: &ConversationType) -> bool {
    matches!(
        conversation_type,
        ConversationType::Rdp
            | ConversationType::Vnc
            | ConversationType::Ssh
            | ConversationType::Telnet
            | ConversationType::Http
            | ConversationType::Kubernetes
            | ConversationType::Mysql
            | ConversationType::SqlServer
            | ConversationType::Postgresql
    )
}