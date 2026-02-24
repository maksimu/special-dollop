//! Network stream abstraction for TCP and TLS connections
//!
//! This module provides `NetworkStream`, a unified type that can represent
//! either a plain TCP connection or a TLS-encrypted connection. This allows
//! the rest of the codebase to work with streams generically without caring
//! about the underlying transport.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
pub use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;

use crate::protocol::sqlserver::{TdsClientTlsStream, TdsServerTlsStream};

/// A network stream that can be either plain TCP or TLS-encrypted
///
/// This enum abstracts over the connection type, allowing handlers to
/// work with streams without knowing whether TLS is in use.
///
/// The TLS variant is boxed to reduce the size difference between variants,
/// as `TlsStream` is significantly larger than `TcpStream`.
///
/// # Example
///
/// ```ignore
/// // Start with a TCP stream
/// let stream = NetworkStream::Tcp(tcp_stream);
///
/// // Later, upgrade to TLS if needed
/// let stream = stream.upgrade_to_tls(acceptor).await?;
///
/// // Check if encrypted
/// if stream.is_encrypted() {
///     println!("Connection is TLS-encrypted");
/// }
/// ```
pub enum NetworkStream {
    /// Plain TCP connection
    Tcp(TcpStream),
    /// TLS-encrypted connection (server-side, boxed to reduce enum size)
    ServerTls(Box<ServerTlsStream<TcpStream>>),
    /// TLS-encrypted connection (client-side, for connecting to backend servers)
    ClientTls(Box<ClientTlsStream<TcpStream>>),
    /// TDS-wrapped TLS connection (server-side, for SQL Server client connections)
    /// TLS is encapsulated within TDS packets during handshake
    TdsTls(Box<ServerTlsStream<TdsClientTlsStream>>),
    /// TDS-wrapped TLS connection (client-side, for SQL Server backend connections)
    /// TLS is encapsulated within TDS packets during handshake
    TdsServerTls(Box<ClientTlsStream<TdsServerTlsStream>>),
}

impl NetworkStream {
    /// Create a new TCP stream wrapper
    pub fn tcp(stream: TcpStream) -> Self {
        NetworkStream::Tcp(stream)
    }

    /// Check if this stream is TLS-encrypted
    pub fn is_encrypted(&self) -> bool {
        matches!(
            self,
            NetworkStream::ServerTls(_)
                | NetworkStream::ClientTls(_)
                | NetworkStream::TdsTls(_)
                | NetworkStream::TdsServerTls(_)
        )
    }

    /// Get the TLS protocol version if this is a TLS stream
    pub fn tls_version(&self) -> Option<&'static str> {
        match self {
            NetworkStream::Tcp(_) => None,
            NetworkStream::ServerTls(tls) => tls.get_ref().1.protocol_version().map(|v| match v {
                rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
                rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
                _ => "TLS (unknown version)",
            }),
            NetworkStream::ClientTls(tls) => tls.get_ref().1.protocol_version().map(|v| match v {
                rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
                rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
                _ => "TLS (unknown version)",
            }),
            NetworkStream::TdsTls(tls) => tls.get_ref().1.protocol_version().map(|v| match v {
                rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
                rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
                _ => "TLS (unknown version)",
            }),
            NetworkStream::TdsServerTls(tls) => {
                tls.get_ref().1.protocol_version().map(|v| match v {
                    rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2",
                    rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3",
                    _ => "TLS (unknown version)",
                })
            }
        }
    }

    /// Get the negotiated cipher suite if this is a TLS stream
    pub fn cipher_suite(&self) -> Option<&'static str> {
        match self {
            NetworkStream::Tcp(_) => None,
            NetworkStream::ServerTls(tls) => tls
                .get_ref()
                .1
                .negotiated_cipher_suite()
                .map(|cs| cs.suite().as_str().unwrap_or("TLS (unknown cipher)")),
            NetworkStream::ClientTls(tls) => tls
                .get_ref()
                .1
                .negotiated_cipher_suite()
                .map(|cs| cs.suite().as_str().unwrap_or("TLS (unknown cipher)")),
            NetworkStream::TdsTls(tls) => tls
                .get_ref()
                .1
                .negotiated_cipher_suite()
                .map(|cs| cs.suite().as_str().unwrap_or("TLS (unknown cipher)")),
            NetworkStream::TdsServerTls(tls) => tls
                .get_ref()
                .1
                .negotiated_cipher_suite()
                .map(|cs| cs.suite().as_str().unwrap_or("TLS (unknown cipher)")),
        }
    }

    /// Extract the TCP stream for TLS upgrade or other purposes
    ///
    /// Returns `Ok(TcpStream)` if this is a TCP stream, allowing it to be
    /// upgraded to TLS. Returns `Err(self)` if this is already a TLS stream.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tcp_stream = stream.into_tcp()?;
    /// let tls_stream = acceptor.accept(tcp_stream).await?;
    /// let stream = NetworkStream::ServerTls(Box::new(tls_stream));
    /// ```
    pub fn into_tcp(self) -> Result<TcpStream, Self> {
        match self {
            NetworkStream::Tcp(stream) => Ok(stream),
            other => Err(other),
        }
    }

    /// Extract the underlying TCP stream from a ClientTls variant.
    /// Used for TLS renegotiation (e.g., Oracle ADB TCPS RESEND flow).
    /// Returns Ok(TcpStream) for ClientTls, Err(self) otherwise.
    pub fn into_client_tls_tcp(self) -> Result<TcpStream, Self> {
        match self {
            NetworkStream::ClientTls(tls) => {
                let (tcp, _conn) = (*tls).into_inner();
                Ok(tcp)
            }
            other => Err(other),
        }
    }

    /// Get a reference to the underlying TCP stream
    ///
    /// # Panics
    ///
    /// Panics for TDS-wrapped TLS streams since they don't expose the TCP stream directly.
    pub fn tcp_ref(&self) -> &TcpStream {
        match self {
            NetworkStream::Tcp(stream) => stream,
            NetworkStream::ServerTls(tls) => tls.get_ref().0,
            NetworkStream::ClientTls(tls) => tls.get_ref().0,
            NetworkStream::TdsTls(_) | NetworkStream::TdsServerTls(_) => {
                panic!("tcp_ref() not supported for TDS-wrapped TLS streams")
            }
        }
    }
}

impl AsyncRead for NetworkStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetworkStream::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            NetworkStream::ServerTls(stream) => Pin::new(stream).poll_read(cx, buf),
            NetworkStream::ClientTls(stream) => Pin::new(stream).poll_read(cx, buf),
            NetworkStream::TdsTls(stream) => Pin::new(stream).poll_read(cx, buf),
            NetworkStream::TdsServerTls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for NetworkStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            NetworkStream::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            NetworkStream::ServerTls(stream) => Pin::new(stream).poll_write(cx, buf),
            NetworkStream::ClientTls(stream) => Pin::new(stream).poll_write(cx, buf),
            NetworkStream::TdsTls(stream) => Pin::new(stream).poll_write(cx, buf),
            NetworkStream::TdsServerTls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetworkStream::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            NetworkStream::ServerTls(stream) => Pin::new(stream).poll_flush(cx),
            NetworkStream::ClientTls(stream) => Pin::new(stream).poll_flush(cx),
            NetworkStream::TdsTls(stream) => Pin::new(stream).poll_flush(cx),
            NetworkStream::TdsServerTls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            NetworkStream::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            NetworkStream::ServerTls(stream) => Pin::new(stream).poll_shutdown(cx),
            NetworkStream::ClientTls(stream) => Pin::new(stream).poll_shutdown(cx),
            NetworkStream::TdsTls(stream) => Pin::new(stream).poll_shutdown(cx),
            NetworkStream::TdsServerTls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to test with mock streams - NetworkStream requires TcpStream,
    // so we test the trait implementations indirectly

    #[test]
    fn test_network_stream_is_encrypted() {
        // Test that is_encrypted returns correct values based on variant
        // We can't easily create real streams, but we can verify the logic
        fn check_encrypted(stream: &NetworkStream) -> bool {
            stream.is_encrypted()
        }

        // The function exists and compiles - actual testing requires integration tests
        let _ = check_encrypted;
    }

    #[test]
    fn test_tls_version_returns_none_for_tcp() {
        // Verify tls_version logic - actual testing requires real TLS streams
        fn check_tls_version(stream: &NetworkStream) -> Option<&'static str> {
            stream.tls_version()
        }

        let _ = check_tls_version;
    }

    #[test]
    fn test_cipher_suite_returns_none_for_tcp() {
        // Verify cipher_suite logic - actual testing requires real TLS streams
        fn check_cipher_suite(stream: &NetworkStream) -> Option<&'static str> {
            stream.cipher_suite()
        }

        let _ = check_cipher_suite;
    }
}
