use std::fmt;
use std::io;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::{debug_hot_path, warn_hot_path};
use anyhow::Result;
use bytes::Bytes;
use futures::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn}; // Import centralized hot path macros

// Connection message types for channel communication
#[derive(Debug)]
pub(crate) enum ConnectionMessage {
    Data(Bytes),
    Eof,
}

// Trait for async read/write operations
pub(crate) trait AsyncReadWrite:
    AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static
{
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

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
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

// Simplified connection - no complex stats tracking
pub(crate) struct Conn {
    pub(crate) data_tx: mpsc::UnboundedSender<ConnectionMessage>,
    pub(crate) backend_task: JoinHandle<()>,
    pub(crate) to_webrtc: JoinHandle<()>,
}

impl Conn {
    /// Create a new connection with a dedicated backend task
    pub async fn new_with_backend(
        backend: Box<dyn AsyncReadWrite>,
        _existing_task: JoinHandle<()>, // Ignored - we create our own
        conn_no: u32,
        channel_id: String,
    ) -> Self {
        let (data_tx, data_rx) = mpsc::unbounded_channel::<ConnectionMessage>();

        // Create backend task that handles the actual backend I/O
        let backend_task = tokio::spawn(backend_task_runner(
            backend,
            data_rx,
            conn_no,
            channel_id.clone(),
        ));

        // Create placeholder for the to_webrtc task (backend->WebRTC handled by setup_outbound_task)
        let to_webrtc = tokio::spawn(async move {
            debug!(target: "connection_lifecycle", channel_id=%channel_id, conn_no,
                   "to_webrtc task started (backend->WebRTC handled by setup_outbound_task)");
        });

        Self {
            data_tx,
            backend_task,
            to_webrtc,
        }
    }

    /// Shutdown the connection gracefully
    pub async fn shutdown(self) -> Result<()> {
        // Close the data channel
        drop(self.data_tx);

        // Wait for tasks to complete
        if let Err(e) = self.backend_task.await {
            if !e.is_cancelled() {
                warn!(target: "connection_lifecycle", error=%e, "Backend task ended with error during shutdown");
            }
        }

        if let Err(e) = self.to_webrtc.await {
            if !e.is_cancelled() {
                warn!(target: "connection_lifecycle", error=%e, "to_webrtc task ended with error during shutdown");
            }
        }

        Ok(())
    }
}

// Backend task runner
async fn backend_task_runner(
    mut backend: Box<dyn AsyncReadWrite>,
    mut data_rx: mpsc::UnboundedReceiver<ConnectionMessage>,
    conn_no: u32,
    channel_id: String,
) {
    debug!(target: "connection_lifecycle", channel_id=%channel_id, conn_no,
           "Backend task started");

    while let Some(message) = data_rx.recv().await {
        match message {
            ConnectionMessage::Data(payload) => {
                // Write to backend without complex stats tracking
                match backend.write_all(payload.as_ref()).await {
                    Ok(_) => {
                        if let Err(flush_err) = backend.flush().await {
                            warn_hot_path!(
                                channel_id = %channel_id,
                                conn_no = conn_no,
                                error = %flush_err,
                                "Backend flush error, client disconnected"
                            );
                            break; // Exit the task on flush error
                        }

                        debug_hot_path!(
                            channel_id = %channel_id,
                            conn_no = conn_no,
                            bytes_written = payload.len(),
                            "Backend write successful"
                        );
                    }
                    Err(write_err) => {
                        warn_hot_path!(
                            channel_id = %channel_id,
                            conn_no = conn_no,
                            error = %write_err,
                            "Backend write error, client disconnected"
                        );
                        break; // Exit the task on writing error
                    }
                }
            }
            ConnectionMessage::Eof => {
                // Handle EOF - call real TCP shutdown
                if let Err(e) = AsyncReadWrite::shutdown(&mut backend).await {
                    warn!(target: "connection_lifecycle", channel_id=%channel_id, conn_no, error=%e,
                          "Failed to shutdown backend on EOF");
                } else {
                    info!(target: "connection_lifecycle", channel_id=%channel_id, conn_no,
                          "Backend shutdown on EOF (connection remains alive for RDP patterns)");
                }
                // Note: We don't break here - connection stays alive after EOF for RDP
            }
        }
    }

    // Shutdown backend on task exit
    if let Err(e) = AsyncReadWrite::shutdown(&mut backend).await {
        debug!(target: "connection_lifecycle", channel_id=%channel_id, conn_no, error=%e,
               "Error shutting down backend in task cleanup");
    }

    debug!(target: "connection_lifecycle", channel_id=%channel_id, conn_no,
           "Backend task exited");
}

/// Tunnel timeout configuration
#[derive(Debug, Clone)]
pub struct TunnelTimeouts {
    pub read: Duration,
    pub close_connection: Duration,
    pub guacd_handshake: Duration,
}

impl Default for TunnelTimeouts {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(15),
            close_connection: Duration::from_secs(5),
            guacd_handshake: Duration::from_secs(10),
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
        Self {
            allowed_hosts,
            allowed_ports,
        }
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
