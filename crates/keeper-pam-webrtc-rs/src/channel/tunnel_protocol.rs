//! Trait and implementations for tunnel protocol handlers (SOCKS5, port-forward).
//!
//! The `TunnelProtocol` trait abstracts the protocol-specific parts of tunnel setup:
//! - Server side: handshake with the local TCP client, produce OpenConnection payload
//! - Client side: parse OpenConnection payload to determine backend target
//!
//! TCP forwarding infrastructure (read loops, WebRTC frame encoding, backpressure)
//! stays in the channel layer since it depends on internal types.

use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use log::debug;
use tokio::net::TcpStream;

use crate::buffer_pool::BufferPool;

/// Result of a tunnel protocol's server-side handshake.
pub(crate) struct HandshakeResult {
    /// The encoded payload for the OpenConnection control message.
    /// For SOCKS5: acquired from buffer pool, caller must release after use.
    /// For PortForward: small inline allocation (4 bytes), not from pool.
    pub open_connection_payload: BytesMut,
}

/// Result of parsing an incoming OpenConnection payload on the client side
pub(crate) struct OpenConnectionTarget {
    pub host: String,
    pub port: u16,
}

/// Trait for tunnel protocol handlers (SOCKS5, port-forward, etc.)
///
/// Server side: handshake with the local TCP client, produce OpenConnection payload
/// Client side: parse OpenConnection payload to determine backend target
#[async_trait::async_trait]
pub(crate) trait TunnelProtocol: Send + Sync {
    /// Protocol name for logging
    fn name(&self) -> &str;

    /// Server side: Perform any protocol-specific handshake on the accepted TCP stream.
    /// Returns the split stream halves and handshake result.
    /// For SOCKS5: does greeting/auth/command parsing, returns target in payload.
    /// For PortForward: no-op handshake, returns conn_no-only payload.
    async fn server_handshake(
        &self,
        stream: TcpStream,
        conn_no: u32,
        buffer_pool: &BufferPool,
    ) -> Result<(
        tokio::net::tcp::OwnedReadHalf,
        tokio::net::tcp::OwnedWriteHalf,
        HandshakeResult,
    )>;

    /// Client side: Parse an OpenConnection payload to extract the backend target.
    /// The data slice starts AFTER conn_no has been consumed.
    /// For SOCKS5: decodes host_len + host + port from payload.
    /// For PortForward: returns the pre-configured target (ignores payload).
    fn parse_open_connection(&self, data: &[u8]) -> Result<OpenConnectionTarget>;

    /// Get the deferred response to send on ConnectionOpened, if any.
    /// SOCKS5: returns the success response bytes. PortForward: returns None.
    fn deferred_response(&self) -> Option<Bytes>;

    /// Server side: Handle rejection of a non-localhost connection.
    /// SOCKS5: sends auth failure response before closing.
    /// PortForward: just drops the connection (default).
    async fn reject_non_localhost(&self, stream: TcpStream) -> Result<()> {
        drop(stream);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SOCKS5 implementation
// ---------------------------------------------------------------------------

pub(crate) struct Socks5TunnelProtocol;

#[async_trait::async_trait]
impl TunnelProtocol for Socks5TunnelProtocol {
    fn name(&self) -> &str {
        "socks5"
    }

    async fn server_handshake(
        &self,
        stream: TcpStream,
        conn_no: u32,
        buffer_pool: &BufferPool,
    ) -> Result<(
        tokio::net::tcp::OwnedReadHalf,
        tokio::net::tcp::OwnedWriteHalf,
        HandshakeResult,
    )> {
        let mut handshake = pam_socks5::Socks5Handshake::new(stream);
        let (address, port) = handshake.handshake().await?;

        let host = address.as_str().to_string();
        debug!(
            "SOCKS5 handshake complete: {}:{} (conn_no: {})",
            host, port, conn_no
        );

        // Build OpenConnection payload using the buffer pool (zero-copy strategy)
        let host_bytes = host.as_bytes();
        let mut open_data = buffer_pool.acquire();
        open_data.clear();
        open_data.put_u32(conn_no);
        open_data.put_u32(host_bytes.len() as u32);
        open_data.extend_from_slice(host_bytes);
        open_data.put_u16(port);

        // Get the TcpStream back and split into owned halves
        let stream: TcpStream = handshake.into_inner();
        let (reader, writer) = stream.into_split();

        Ok((
            reader,
            writer,
            HandshakeResult {
                open_connection_payload: open_data,
            },
        ))
    }

    fn parse_open_connection(&self, data: &[u8]) -> Result<OpenConnectionTarget> {
        let target = pam_socks5::decode_open_connection_payload(data)?;
        Ok(OpenConnectionTarget {
            host: target.host,
            port: target.port,
        })
    }

    fn deferred_response(&self) -> Option<Bytes> {
        // Zero-allocation: static bytes, no heap alloc
        Some(Bytes::from_static(&pam_socks5::SOCKS5_SUCCESS_RESPONSE))
    }

    async fn reject_non_localhost(&self, stream: TcpStream) -> Result<()> {
        let mut handshake = pam_socks5::Socks5Handshake::new(stream);
        handshake.reject_non_localhost().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Port-forward implementation
// ---------------------------------------------------------------------------

pub(crate) struct PortForwardTunnelProtocol {
    pub target_host: String,
    pub target_port: u16,
}

#[async_trait::async_trait]
impl TunnelProtocol for PortForwardTunnelProtocol {
    fn name(&self) -> &str {
        "port-forward"
    }

    async fn server_handshake(
        &self,
        stream: TcpStream,
        conn_no: u32,
        _buffer_pool: &BufferPool,
    ) -> Result<(
        tokio::net::tcp::OwnedReadHalf,
        tokio::net::tcp::OwnedWriteHalf,
        HandshakeResult,
    )> {
        // No handshake for port-forward - just split the stream
        let (reader, writer) = stream.into_split();
        // 4 bytes is small enough that buffer pool overhead isn't worth it
        let mut payload = BytesMut::with_capacity(4);
        payload.extend_from_slice(&conn_no.to_be_bytes());

        Ok((
            reader,
            writer,
            HandshakeResult {
                open_connection_payload: payload,
            },
        ))
    }

    fn parse_open_connection(&self, _data: &[u8]) -> Result<OpenConnectionTarget> {
        // Port-forward ignores the payload; returns pre-configured target
        Ok(OpenConnectionTarget {
            host: self.target_host.clone(),
            port: self.target_port,
        })
    }

    fn deferred_response(&self) -> Option<Bytes> {
        None
    }
}
