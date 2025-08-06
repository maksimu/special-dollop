use anyhow::Result;
use bytes::Bytes;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Dual-stack socket binding utilities for IPv6/IPv4 compatibility
pub mod dual_stack {
    use anyhow::{anyhow, Result};
    use tokio::net::UdpSocket;
    use tracing::debug;

    /// Creates a dual-stack UDP socket, preferring IPv6 but falling back to IPv4
    /// Binds to \[::]:port first, then 0.0.0.0:port if IPv6 fails
    pub async fn bind_udp_dual_stack(port: u16) -> Result<UdpSocket> {
        // Try IPv6 first (dual-stack on most systems)
        let ipv6_addr = format!("[::]:{port}");
        match UdpSocket::bind(&ipv6_addr).await {
            Ok(socket) => {
                debug!("UDP bound to IPv6 dual-stack address: {}", ipv6_addr);
                Ok(socket)
            }
            Err(ipv6_err) => {
                debug!("IPv6 UDP bind failed ({}), trying IPv4", ipv6_err);
                // Fallback to IPv4
                let ipv4_addr = format!("0.0.0.0:{port}");
                match UdpSocket::bind(&ipv4_addr).await {
                    Ok(socket) => {
                        debug!("UDP bound to IPv4 address: {}", ipv4_addr);
                        Ok(socket)
                    }
                    Err(ipv4_err) => Err(anyhow!(
                        "Failed to bind UDP socket - IPv6 error: {}, IPv4 error: {}",
                        ipv6_err,
                        ipv4_err
                    )),
                }
            }
        }
    }

    /// Creates a dual-stack localhost UDP socket for testing/internal use
    /// Binds to \[::1]:port first, then 127.0.0.1:port if IPv6 fails
    pub async fn bind_udp_localhost(port: u16) -> Result<UdpSocket> {
        // Try IPv6 localhost first
        let ipv6_addr = format!("[::1]:{port}");
        match UdpSocket::bind(&ipv6_addr).await {
            Ok(socket) => {
                debug!("UDP bound to IPv6 localhost: {}", ipv6_addr);
                Ok(socket)
            }
            Err(ipv6_err) => {
                debug!("IPv6 localhost UDP bind failed ({ipv6_err}), trying IPv4");
                // Fallback to IPv4 localhost
                let ipv4_addr = format!("127.0.0.1:{port}");
                match UdpSocket::bind(&ipv4_addr).await {
                    Ok(socket) => {
                        debug!("UDP bound to IPv4 localhost: {ipv4_addr}");
                        Ok(socket)
                    }
                    Err(ipv4_err) => Err(anyhow!(
                        "Failed to bind UDP localhost socket - IPv6 error: {}, IPv4 error: {}",
                        ipv6_err,
                        ipv4_err
                    )),
                }
            }
        }
    }
}

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

// Simplified connection with event-driven sending
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
                            warn!(
                                channel_id = %channel_id,
                                conn_no = conn_no,
                                error = %flush_err,
                                "Backend flush error, client disconnected"
                            );
                            break; // Exit the task on flush error
                        }

                        debug!(
                            channel_id = %channel_id,
                            conn_no = conn_no,
                            bytes_written = payload.len(),
                            "Backend write successful"
                        );
                    }
                    Err(write_err) => {
                        warn!(
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
    pub guacd_handshake: Duration,
}

impl Default for TunnelTimeouts {
    fn default() -> Self {
        Self {
            read: Duration::from_secs(15),
            guacd_handshake: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
struct DnsCacheEntry {
    ips: smallvec::SmallVec<[IpAddr; 4]>, // Most domains have ≤4 IPs, avoid heap allocation
    expires_at: Instant,
}

/// High-performance network access checker with async DNS and caching
#[derive(Debug, Clone)]
pub struct NetworkAccessChecker {
    allowed_networks: Arc<[ipnet::IpNet]>, // CIDR networks for IP matching
    allowed_hostnames: Arc<[String]>,      // Exact hostname matches
    allowed_wildcards: Arc<[String]>,      // Wildcard domains (*.example.com)
    allowed_ports: Arc<[u16]>,             // Immutable slice for faster lookups
    dns_cache: Arc<tokio::sync::RwLock<HashMap<Arc<str>, DnsCacheEntry>>>, // Use Arc<str> to avoid cloning
    dns_cache_ttl: Duration,
}

impl NetworkAccessChecker {
    pub fn new(allowed_hosts: Vec<String>, allowed_ports: Vec<u16>) -> Self {
        let mut allowed_networks = Vec::new();
        let mut allowed_hostnames = Vec::new();
        let mut allowed_wildcards = Vec::new();

        for host in allowed_hosts {
            if host.starts_with("*.") {
                // Wildcard domain like "*.google.com"
                allowed_wildcards.push(host[1..].to_string()); // Store ".google.com"
            } else if let Ok(network) = host.parse::<ipnet::IpNet>() {
                // CIDR network like "192.168.1.0/24"
                allowed_networks.push(network);
            } else if let Ok(ip) = host.parse::<IpAddr>() {
                // Single IP address or special "allow all" cases
                let network = match ip {
                    IpAddr::V4(ipv4) => {
                        if ipv4.is_unspecified() {
                            // 0.0.0.0 means allow all IPv4
                            ipnet::IpNet::V4(ipnet::Ipv4Net::new(ipv4, 0).unwrap())
                        // 0.0.0.0/0
                        } else {
                            ipnet::IpNet::V4(ipnet::Ipv4Net::new(ipv4, 32).unwrap())
                            // Single IP
                        }
                    }
                    IpAddr::V6(ipv6) => {
                        if ipv6.is_unspecified() {
                            // :: means allow all IPv6
                            ipnet::IpNet::V6(ipnet::Ipv6Net::new(ipv6, 0).unwrap())
                        // ::/0
                        } else {
                            ipnet::IpNet::V6(ipnet::Ipv6Net::new(ipv6, 128).unwrap())
                            // Single IP
                        }
                    }
                };
                allowed_networks.push(network);
            } else {
                // Exact hostname like "example.com"
                allowed_hostnames.push(host);
            }
        }

        // Sort ports for binary search optimization
        let mut sorted_ports = allowed_ports;
        sorted_ports.sort_unstable();

        Self {
            allowed_networks: allowed_networks.into(),
            allowed_hostnames: allowed_hostnames.into(),
            allowed_wildcards: allowed_wildcards.into(),
            allowed_ports: sorted_ports.into(),
            dns_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            dns_cache_ttl: Duration::from_secs(300), // 5 minutes
        }
    }

    /// Zero-allocation IP network checking
    #[inline]
    fn is_ip_allowed_fast(&self, ip: IpAddr) -> bool {
        // Hot path: use iterator without debug logs
        self.allowed_networks
            .iter()
            .any(|network| network.contains(&ip))
    }

    /// Fast port checking with binary search (O(log n))
    #[inline]
    pub fn is_port_allowed(&self, port: u16) -> bool {
        self.allowed_ports.is_empty() || self.allowed_ports.binary_search(&port).is_ok()
    }

    /// DNS resolution with minimal allocations
    async fn resolve_with_minimal_allocation(
        &self,
        domain: &str,
    ) -> Result<smallvec::SmallVec<[IpAddr; 4]>, io::Error> {
        // **OPTIMIZATION**: Use a thread-local static buffer to avoid String allocation
        use std::fmt::Write;

        // Use a reasonable size buffer on the stack for most hostnames
        // Use port 0 which works for DNS resolution but doesn't assume a specific service
        let mut buffer = heapless::String::<256>::new();
        if write!(&mut buffer, "{domain}:0").is_err() {
            // Domain name too long for stack buffer, fall back to heap allocation
            let addrs = tokio::net::lookup_host(format!("{domain}:0")).await?;
            return Ok(addrs.map(|addr| addr.ip()).collect());
        }

        // **ZERO-ALLOCATION DNS LOOKUP** (except for the actual network call)
        let addrs = tokio::net::lookup_host(buffer.as_str()).await?;
        Ok(addrs.map(|addr| addr.ip()).collect())
    }

    /// **PERFORMANCE OPTIMIZED**: Combined permission check + DNS resolution
    /// Returns resolved IPs if host is allowed, None if denied
    /// Eliminates double DNS lookup for SOCKS5 (check + connect)
    pub async fn resolve_if_allowed(
        &self,
        domain_name_or_ip: &str,
    ) -> Option<smallvec::SmallVec<[IpAddr; 4]>> {
        // If no rules specified, allow everything
        if self.allowed_networks.is_empty()
            && self.allowed_hostnames.is_empty()
            && self.allowed_wildcards.is_empty()
        {
            return self
                .resolve_with_minimal_allocation(domain_name_or_ip)
                .await
                .ok();
        }

        // **ZERO-ALLOCATION HOT PATH 1**: Check exact hostname match
        for hostname in self.allowed_hostnames.iter() {
            if hostname == domain_name_or_ip {
                return self
                    .resolve_with_minimal_allocation(domain_name_or_ip)
                    .await
                    .ok();
            }
        }

        // **ZERO-ALLOCATION HOT PATH 2**: Check wildcard match
        for wildcard_suffix in self.allowed_wildcards.iter() {
            if domain_name_or_ip.ends_with(wildcard_suffix) {
                return self
                    .resolve_with_minimal_allocation(domain_name_or_ip)
                    .await
                    .ok();
            }
        }

        // **ZERO-ALLOCATION HOT PATH 3**: Direct IP check
        if let Ok(ip) = domain_name_or_ip.parse::<IpAddr>() {
            return if self.is_ip_allowed_fast(ip) {
                let mut ips = smallvec::SmallVec::new();
                ips.push(ip);
                Some(ips)
            } else {
                None
            };
        }

        // **DNS PATH**: Check if resolved IPs match allowed networks
        if !self.allowed_networks.is_empty() {
            // Check cache first - reuse existing cache logic
            {
                let cache = self.dns_cache.read().await;
                if let Some(entry) = cache.get(domain_name_or_ip) {
                    if entry.expires_at > Instant::now() {
                        // Check if any cached IP is allowed
                        let allowed = entry.ips.iter().any(|&ip| self.is_ip_allowed_fast(ip));
                        return if allowed {
                            Some(entry.ips.clone())
                        } else {
                            None
                        };
                    }
                }
            }

            // Resolve and cache
            match self
                .resolve_with_minimal_allocation(domain_name_or_ip)
                .await
            {
                Ok(ips) => {
                    let allowed = ips.iter().any(|&ip| self.is_ip_allowed_fast(ip));

                    // Cache the result
                    {
                        let mut cache = self.dns_cache.write().await;
                        let domain_key: Arc<str> = domain_name_or_ip.into();
                        cache.insert(
                            domain_key,
                            DnsCacheEntry {
                                ips: ips.clone(),
                                expires_at: Instant::now() + self.dns_cache_ttl,
                            },
                        );

                        // Cleanup expired entries periodically
                        if cache.len() > 1000 {
                            let now = Instant::now();
                            cache.retain(|_, entry| entry.expires_at > now);
                        }
                    }

                    if allowed {
                        Some(ips)
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        } else {
            None // No rules allow this domain
        }
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
            ConversationType::Postgresql => write!(f, "postgres"),
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
            "postgresql" | "postgres" => Ok(ConversationType::Postgresql),
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
