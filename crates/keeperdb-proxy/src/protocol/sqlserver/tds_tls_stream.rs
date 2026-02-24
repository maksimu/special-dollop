//! TDS-encapsulated TLS stream
//!
//! During SQL Server TLS handshake, TLS records are wrapped inside TDS packets:
//! - Client sends TLS data wrapped in TDS PRELOGIN packets (type 0x12)
//! - Server sends TLS data wrapped in TDS TABULAR_RESULT packets (type 0x04)
//!
//! This module provides a wrapper stream that handles the encapsulation,
//! allowing standard TLS libraries to work transparently.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use super::constants::{packet_type, TDS_HEADER_SIZE};

/// Maximum TDS packet size for TLS data
const MAX_TDS_PACKET_SIZE: usize = 4096;

/// TDS-wrapped TLS stream for client connections
///
/// Handles TLS encapsulation for the proxy-to-client direction:
/// - Reads TLS data from TDS PRELOGIN packets (type 0x12) sent by client
/// - Writes TLS data wrapped in TDS TABULAR_RESULT packets (type 0x04)
///
/// After TLS handshake completes, call `set_passthrough(true)` to switch
/// to pass-through mode where data is sent/received without TDS wrapping.
pub struct TdsClientTlsStream {
    inner: TcpStream,
    /// Buffer for incoming TLS data extracted from TDS packets
    read_buf: VecDeque<u8>,
    /// Partial TDS header being read
    header_buf: Vec<u8>,
    /// Bytes remaining in current TDS packet payload
    payload_remaining: usize,
    /// Buffer for pending TLS data to be sent (accumulated until flush)
    pending_tls_data: Vec<u8>,
    /// Buffer for building TDS packets when flushing
    write_buf: Vec<u8>,
    /// When true, bypass TDS wrapping (used after TLS handshake completes)
    passthrough: bool,
}

impl TdsClientTlsStream {
    /// Create a new TDS/TLS wrapper for client connections
    pub fn new(stream: TcpStream) -> Self {
        Self {
            inner: stream,
            read_buf: VecDeque::with_capacity(8192),
            header_buf: Vec::with_capacity(TDS_HEADER_SIZE),
            payload_remaining: 0,
            pending_tls_data: Vec::with_capacity(8192),
            write_buf: Vec::with_capacity(MAX_TDS_PACKET_SIZE),
            passthrough: false,
        }
    }

    /// Unwrap to inner TCP stream (after TLS handshake completes)
    pub fn into_inner(self) -> TcpStream {
        self.inner
    }

    /// Enable/disable passthrough mode
    ///
    /// In passthrough mode, data is sent/received directly without TDS wrapping.
    /// This should be enabled after the TLS handshake completes.
    pub fn set_passthrough(&mut self, enabled: bool) {
        debug!("TdsClientTlsStream: passthrough mode = {}", enabled);
        self.passthrough = enabled;
    }

    /// Parse TDS header and return payload length
    fn parse_tds_header(header: &[u8]) -> Option<usize> {
        if header.len() < TDS_HEADER_SIZE {
            return None;
        }
        // TDS header: type(1) + status(1) + length(2 big-endian) + ...
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        if length < TDS_HEADER_SIZE {
            return None;
        }
        Some(length - TDS_HEADER_SIZE)
    }

    /// Create TDS header for outgoing packet
    fn create_tds_header(
        packet_type: u8,
        payload_len: usize,
        is_last: bool,
    ) -> [u8; TDS_HEADER_SIZE] {
        let total_len = (TDS_HEADER_SIZE + payload_len) as u16;
        let status = if is_last { 0x01 } else { 0x00 };
        [
            packet_type,
            status,
            (total_len >> 8) as u8,
            (total_len & 0xFF) as u8,
            0x00,
            0x00, // SPID
            0x00, // Packet ID
            0x00, // Window
        ]
    }
}

impl AsyncRead for TdsClientTlsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;

        // In passthrough mode, read directly from inner stream
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }

        // If we have buffered TLS data, return it first
        if !this.read_buf.is_empty() {
            let to_copy = std::cmp::min(buf.remaining(), this.read_buf.len());
            trace!("TdsClientTlsStream: returning {} buffered bytes", to_copy);
            for _ in 0..to_copy {
                if let Some(byte) = this.read_buf.pop_front() {
                    buf.put_slice(&[byte]);
                }
            }
            return Poll::Ready(Ok(()));
        }

        // If we're in the middle of reading a TDS payload, continue
        if this.payload_remaining > 0 {
            let mut temp_buf = vec![0u8; std::cmp::min(this.payload_remaining, 4096)];
            let mut temp_read_buf = ReadBuf::new(&mut temp_buf);

            match Pin::new(&mut this.inner).poll_read(cx, &mut temp_read_buf) {
                Poll::Ready(Ok(())) => {
                    let n = temp_read_buf.filled().len();
                    if n == 0 {
                        debug!(
                            "TdsClientTlsStream: EOF while reading payload, {} remaining",
                            this.payload_remaining
                        );
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Connection closed while reading TDS payload",
                        )));
                    }
                    this.payload_remaining -= n;

                    // Log first few bytes to help debug TLS issues
                    let preview_len = std::cmp::min(n, 16);
                    debug!("TdsClientTlsStream: read {} payload bytes, first {}: {:02X?}, {} remaining",
                        n, preview_len, &temp_read_buf.filled()[..preview_len], this.payload_remaining);

                    // Copy to output buffer
                    let to_copy = std::cmp::min(buf.remaining(), n);
                    buf.put_slice(&temp_read_buf.filled()[..to_copy]);

                    // Buffer any excess
                    if to_copy < n {
                        this.read_buf.extend(&temp_read_buf.filled()[to_copy..]);
                    }

                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(e)) => {
                    debug!("TdsClientTlsStream: read error: {}", e);
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        // Need to read a new TDS header
        while this.header_buf.len() < TDS_HEADER_SIZE {
            let needed = TDS_HEADER_SIZE - this.header_buf.len();
            let mut temp_buf = vec![0u8; needed];
            let mut temp_read_buf = ReadBuf::new(&mut temp_buf);

            match Pin::new(&mut this.inner).poll_read(cx, &mut temp_read_buf) {
                Poll::Ready(Ok(())) => {
                    let n = temp_read_buf.filled().len();
                    if n == 0 {
                        if this.header_buf.is_empty() {
                            // Clean EOF
                            debug!("TdsClientTlsStream: clean EOF (no partial header)");
                            return Poll::Ready(Ok(()));
                        }
                        debug!(
                            "TdsClientTlsStream: EOF with partial header ({} bytes)",
                            this.header_buf.len()
                        );
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Connection closed while reading TDS header",
                        )));
                    }
                    trace!("TdsClientTlsStream: read {} header bytes", n);
                    this.header_buf.extend_from_slice(temp_read_buf.filled());
                }
                Poll::Ready(Err(e)) => {
                    debug!("TdsClientTlsStream: header read error: {}", e);
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        // Parse header
        let payload_len = Self::parse_tds_header(&this.header_buf).ok_or_else(|| {
            debug!(
                "TdsClientTlsStream: invalid TDS header: {:02X?}",
                &this.header_buf
            );
            io::Error::new(io::ErrorKind::InvalidData, "Invalid TDS header")
        })?;

        let packet_type = this.header_buf[0];
        debug!(
            "TdsClientTlsStream: TDS packet type=0x{:02X}, payload_len={}",
            packet_type, payload_len
        );

        // Reset header buffer for next packet
        this.header_buf.clear();
        this.payload_remaining = payload_len;

        // Recurse to read payload
        if payload_len > 0 {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl AsyncWrite for TdsClientTlsStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;

        // In passthrough mode, write directly to inner stream
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }

        // Buffer the TLS data - we'll send it all in one TDS packet on flush
        trace!(
            "TdsClientTlsStream: buffering {} TLS bytes (total buffered: {})",
            buf.len(),
            this.pending_tls_data.len() + buf.len()
        );
        this.pending_tls_data.extend_from_slice(buf);

        // Report all bytes as written (buffered)
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;

        // In passthrough mode, flush directly
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_flush(cx);
        }

        // If no pending data, just flush underlying stream
        if this.pending_tls_data.is_empty() {
            return Pin::new(&mut this.inner).poll_flush(cx);
        }

        debug!(
            "TdsClientTlsStream: flushing {} TLS bytes",
            this.pending_tls_data.len()
        );

        // Send all pending TLS data in a single TDS packet (or multiple if too large)
        let max_payload = MAX_TDS_PACKET_SIZE - TDS_HEADER_SIZE;

        // Build TDS packet(s) with all pending data
        // For simplicity, send as one message with final packet marked as EOM
        this.write_buf.clear();

        let mut offset = 0;
        while offset < this.pending_tls_data.len() {
            let remaining = this.pending_tls_data.len() - offset;
            let chunk_size = std::cmp::min(remaining, max_payload);
            let is_last = offset + chunk_size >= this.pending_tls_data.len();

            let header = Self::create_tds_header(packet_type::TABULAR_RESULT, chunk_size, is_last);
            this.write_buf.extend_from_slice(&header);
            this.write_buf
                .extend_from_slice(&this.pending_tls_data[offset..offset + chunk_size]);

            offset += chunk_size;
        }

        debug!(
            "TdsClientTlsStream: built {} bytes of TDS packets",
            this.write_buf.len()
        );

        // Write all packets
        let mut written = 0;
        while written < this.write_buf.len() {
            match Pin::new(&mut this.inner).poll_write(cx, &this.write_buf[written..]) {
                Poll::Ready(Ok(n)) => {
                    if n == 0 {
                        debug!("TdsClientTlsStream: write returned 0");
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "Failed to write TDS packet",
                        )));
                    }
                    written += n;
                    trace!(
                        "TdsClientTlsStream: wrote {} bytes, total {}/{}",
                        n,
                        written,
                        this.write_buf.len()
                    );
                }
                Poll::Ready(Err(e)) => {
                    debug!("TdsClientTlsStream: write error: {}", e);
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => {
                    if written > 0 {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    return Poll::Pending;
                }
            }
        }

        // Clear pending data after successful write
        this.pending_tls_data.clear();
        this.write_buf.clear();
        debug!("TdsClientTlsStream: successfully flushed TDS packets");

        // Now flush the underlying stream
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// TDS-wrapped TLS stream for server connections
///
/// Handles TLS encapsulation for the proxy-to-server direction:
/// - Writes TLS data wrapped in TDS PRELOGIN packets (type 0x12) to server
/// - Reads TLS data from TDS TABULAR_RESULT packets (type 0x04) from server
///
/// After TLS handshake completes, call `set_passthrough(true)` to switch
/// to pass-through mode where data is sent/received without TDS wrapping.
pub struct TdsServerTlsStream {
    inner: TcpStream,
    /// Buffer for incoming TLS data extracted from TDS packets
    read_buf: VecDeque<u8>,
    /// Partial TDS header being read
    header_buf: Vec<u8>,
    /// Bytes remaining in current TDS packet payload
    payload_remaining: usize,
    /// Buffer for pending TLS data to be sent (accumulated until flush)
    pending_tls_data: Vec<u8>,
    /// Buffer for building TDS packets when flushing
    write_buf: Vec<u8>,
    /// When true, bypass TDS wrapping (used after TLS handshake completes)
    passthrough: bool,
}

impl TdsServerTlsStream {
    /// Create a new TDS/TLS wrapper for server connections
    pub fn new(stream: TcpStream) -> Self {
        Self {
            inner: stream,
            read_buf: VecDeque::with_capacity(8192),
            header_buf: Vec::with_capacity(TDS_HEADER_SIZE),
            payload_remaining: 0,
            pending_tls_data: Vec::with_capacity(8192),
            write_buf: Vec::with_capacity(MAX_TDS_PACKET_SIZE),
            passthrough: false,
        }
    }

    /// Unwrap to inner TCP stream (after TLS handshake completes)
    pub fn into_inner(self) -> TcpStream {
        self.inner
    }

    /// Enable/disable passthrough mode
    ///
    /// In passthrough mode, data is sent/received directly without TDS wrapping.
    /// This should be enabled after the TLS handshake completes.
    pub fn set_passthrough(&mut self, enabled: bool) {
        debug!("TdsServerTlsStream: passthrough mode = {}", enabled);
        self.passthrough = enabled;
    }

    /// Parse TDS header and return payload length
    fn parse_tds_header(header: &[u8]) -> Option<usize> {
        if header.len() < TDS_HEADER_SIZE {
            return None;
        }
        let length = u16::from_be_bytes([header[2], header[3]]) as usize;
        if length < TDS_HEADER_SIZE {
            return None;
        }
        Some(length - TDS_HEADER_SIZE)
    }

    /// Create TDS header for outgoing packet
    fn create_tds_header(
        packet_type: u8,
        payload_len: usize,
        is_last: bool,
    ) -> [u8; TDS_HEADER_SIZE] {
        let total_len = (TDS_HEADER_SIZE + payload_len) as u16;
        let status = if is_last { 0x01 } else { 0x00 };
        [
            packet_type,
            status,
            (total_len >> 8) as u8,
            (total_len & 0xFF) as u8,
            0x00,
            0x00, // SPID
            0x00, // Packet ID
            0x00, // Window
        ]
    }
}

impl AsyncRead for TdsServerTlsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;

        // In passthrough mode, read directly from inner stream
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }

        // If we have buffered TLS data, return it first
        if !this.read_buf.is_empty() {
            let to_copy = std::cmp::min(buf.remaining(), this.read_buf.len());
            for _ in 0..to_copy {
                if let Some(byte) = this.read_buf.pop_front() {
                    buf.put_slice(&[byte]);
                }
            }
            return Poll::Ready(Ok(()));
        }

        // If we're in the middle of reading a TDS payload, continue
        if this.payload_remaining > 0 {
            let mut temp_buf = vec![0u8; std::cmp::min(this.payload_remaining, 4096)];
            let mut temp_read_buf = ReadBuf::new(&mut temp_buf);

            match Pin::new(&mut this.inner).poll_read(cx, &mut temp_read_buf) {
                Poll::Ready(Ok(())) => {
                    let n = temp_read_buf.filled().len();
                    if n == 0 {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Connection closed while reading TDS payload",
                        )));
                    }
                    this.payload_remaining -= n;

                    let to_copy = std::cmp::min(buf.remaining(), n);
                    buf.put_slice(&temp_read_buf.filled()[..to_copy]);

                    if to_copy < n {
                        this.read_buf.extend(&temp_read_buf.filled()[to_copy..]);
                    }

                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        // Need to read a new TDS header
        while this.header_buf.len() < TDS_HEADER_SIZE {
            let needed = TDS_HEADER_SIZE - this.header_buf.len();
            let mut temp_buf = vec![0u8; needed];
            let mut temp_read_buf = ReadBuf::new(&mut temp_buf);

            match Pin::new(&mut this.inner).poll_read(cx, &mut temp_read_buf) {
                Poll::Ready(Ok(())) => {
                    let n = temp_read_buf.filled().len();
                    if n == 0 {
                        if this.header_buf.is_empty() {
                            return Poll::Ready(Ok(()));
                        }
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Connection closed while reading TDS header",
                        )));
                    }
                    this.header_buf.extend_from_slice(temp_read_buf.filled());
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        let payload_len = Self::parse_tds_header(&this.header_buf)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid TDS header"))?;

        this.header_buf.clear();
        this.payload_remaining = payload_len;

        if payload_len > 0 {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl AsyncWrite for TdsServerTlsStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;

        // In passthrough mode, write directly to inner stream
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }

        // Buffer the TLS data - we'll send it all in TDS packets on flush
        trace!(
            "TdsServerTlsStream: buffering {} TLS bytes (total buffered: {})",
            buf.len(),
            this.pending_tls_data.len() + buf.len()
        );
        this.pending_tls_data.extend_from_slice(buf);

        // Report all bytes as written (buffered)
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;

        // In passthrough mode, flush directly
        if this.passthrough {
            return Pin::new(&mut this.inner).poll_flush(cx);
        }

        // If no pending data, just flush underlying stream
        if this.pending_tls_data.is_empty() {
            return Pin::new(&mut this.inner).poll_flush(cx);
        }

        debug!(
            "TdsServerTlsStream: flushing {} TLS bytes",
            this.pending_tls_data.len()
        );

        // Send all pending TLS data in TDS packets
        let max_payload = MAX_TDS_PACKET_SIZE - TDS_HEADER_SIZE;

        // Build TDS packet(s) with all pending data
        this.write_buf.clear();

        let mut offset = 0;
        while offset < this.pending_tls_data.len() {
            let remaining = this.pending_tls_data.len() - offset;
            let chunk_size = std::cmp::min(remaining, max_payload);
            let is_last = offset + chunk_size >= this.pending_tls_data.len();

            // Use type 0x12 (PRELOGIN) for proxy-to-server TLS
            let header = Self::create_tds_header(packet_type::PRELOGIN, chunk_size, is_last);
            this.write_buf.extend_from_slice(&header);
            this.write_buf
                .extend_from_slice(&this.pending_tls_data[offset..offset + chunk_size]);

            offset += chunk_size;
        }

        debug!(
            "TdsServerTlsStream: built {} bytes of TDS packets",
            this.write_buf.len()
        );

        // Write all packets
        let mut written = 0;
        while written < this.write_buf.len() {
            match Pin::new(&mut this.inner).poll_write(cx, &this.write_buf[written..]) {
                Poll::Ready(Ok(n)) => {
                    if n == 0 {
                        debug!("TdsServerTlsStream: write returned 0");
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "Failed to write TDS packet",
                        )));
                    }
                    written += n;
                    trace!(
                        "TdsServerTlsStream: wrote {} bytes, total {}/{}",
                        n,
                        written,
                        this.write_buf.len()
                    );
                }
                Poll::Ready(Err(e)) => {
                    debug!("TdsServerTlsStream: write error: {}", e);
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => {
                    if written > 0 {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    return Poll::Pending;
                }
            }
        }

        // Clear pending data after successful write
        this.pending_tls_data.clear();
        this.write_buf.clear();
        debug!("TdsServerTlsStream: successfully flushed TDS packets");

        // Now flush the underlying stream
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tds_header() {
        // Valid header: type=0x12, status=0x01, length=100
        let header = [0x12, 0x01, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(TdsClientTlsStream::parse_tds_header(&header), Some(92)); // 100 - 8

        // Invalid header (too short)
        let short = [0x12, 0x01, 0x00];
        assert_eq!(TdsClientTlsStream::parse_tds_header(&short), None);

        // Invalid header (length < 8)
        let invalid = [0x12, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(TdsClientTlsStream::parse_tds_header(&invalid), None);
    }

    #[test]
    fn test_create_tds_header() {
        let header = TdsClientTlsStream::create_tds_header(0x04, 100, true);
        assert_eq!(header[0], 0x04); // packet type
        assert_eq!(header[1], 0x01); // status (EOM)
        assert_eq!(u16::from_be_bytes([header[2], header[3]]), 108); // length = 8 + 100

        let header_not_last = TdsClientTlsStream::create_tds_header(0x12, 50, false);
        assert_eq!(header_not_last[0], 0x12);
        assert_eq!(header_not_last[1], 0x00); // status (not EOM)
        assert_eq!(
            u16::from_be_bytes([header_not_last[2], header_not_last[3]]),
            58
        );
    }
}
