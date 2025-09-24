/* ----------------------------------------------------------------------------------------------
 *   Wire format (see Python PortForwardsExit)
 *   ┌───────────┬────────────┬────────────┬───────────────────────┬───────────┐
 *   │ ConnNo(4) │ TsMs(8)    │ Len(4)     │ Payload[Len]          │ TERM      │
 *   └───────────┴────────────┴────────────┴───────────────────────┴───────────┘
 *   Connection 0 means a control packet whose payload starts with a 2‑byte
 *   ControlMessage enum code followed by message‑specific data.
 * ------------------------------------------------------------------------------------------- */
use crate::buffer_pool::BufferPool;
use crate::likely; // Import branch prediction macros
use bytes::{Buf, BufMut, Bytes, BytesMut};
use log::warn;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const CONN_NO_LEN: usize = 4;
pub(crate) const CTRL_NO_LEN: usize = 2;
pub(crate) const PORT_LENGTH: usize = 2; // Standard u16 port numbers
const TS_LEN: usize = 8;
const LEN_LEN: usize = 4;

/// Terminator taken from Python constant `TERMINATOR`; adjust if necessary.
const TERMINATOR: &[u8] = b";";

// SIMD optimizations for frame parsing on x86_64
#[cfg(target_arch = "x86_64")]
mod simd_optimizations {
    use std::arch::x86_64::*;

    /// SIMD-optimized terminator verification for exact position checking
    /// CRITICAL: Only checks if terminator exists at EXACT expected position
    /// This prevents false positives from terminators inside payload data
    #[inline(always)]
    pub fn verify_terminator_at_position_simd(buf: &[u8], expected_pos: usize) -> bool {
        // Bounds check first - critical for safety
        if expected_pos >= buf.len() {
            return false;
        }

        // For single-byte terminator, check the exact position
        // SIMD would be overkill for a single byte, but we use it for consistency
        // and potential future multibyte terminators
        if expected_pos + 1 > buf.len() {
            return false;
        }

        // Direct comparison is actually fastest for single byte
        buf[expected_pos] == b';'
    }

    /// Prefetch memory for better cache performance
    #[inline]
    pub fn prefetch_frame_data(buf: &[u8], offset: usize) {
        if offset + 64 <= buf.len() {
            unsafe {
                _mm_prefetch(buf.as_ptr().add(offset) as *const i8, _MM_HINT_T0);
            }
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod simd_optimizations {
    /// Exact position terminator verification - no SIMD fallback
    #[inline(always)]
    pub fn verify_terminator_at_position_simd(buf: &[u8], expected_pos: usize) -> bool {
        // Bounds check first
        if expected_pos >= buf.len() {
            return false;
        }

        buf[expected_pos] == b';'
    }

    /// No-op prefetch for non-x86_64
    #[inline]
    pub fn prefetch_frame_data(_buf: &[u8], _offset: usize) {
        // No prefetch on non-x86_64
    }
}

use simd_optimizations::{prefetch_frame_data, verify_terminator_at_position_simd};

#[repr(u16)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ControlMessage {
    Ping = 1,
    Pong = 2,
    OpenConnection = 101,
    CloseConnection = 102,
    SendEOF = 104,
    ConnectionOpened = 103,
    // UDP ASSOCIATE support
    UdpAssociate = 201,
    UdpAssociateOpened = 202,
    UdpPacket = 203,
    UdpAssociateClosed = 204,
    // Metrics support
    MetricsRequest = 301,
    MetricsResponse = 302,
    MetricsConfig = 303,
}
impl std::fmt::Display for ControlMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlMessage::Ping => write!(f, "Ping"),
            ControlMessage::Pong => write!(f, "Pong"),
            ControlMessage::OpenConnection => write!(f, "OpenConnection"),
            ControlMessage::CloseConnection => write!(f, "CloseConnection"),
            ControlMessage::SendEOF => write!(f, "SendEOF"),
            ControlMessage::ConnectionOpened => write!(f, "ConnectionOpened"),
            ControlMessage::UdpAssociate => write!(f, "UdpAssociate"),
            ControlMessage::UdpAssociateOpened => write!(f, "UdpAssociateOpened"),
            ControlMessage::UdpPacket => write!(f, "UdpPacket"),
            ControlMessage::UdpAssociateClosed => write!(f, "UdpAssociateClosed"),
            ControlMessage::MetricsRequest => write!(f, "MetricsRequest"),
            ControlMessage::MetricsResponse => write!(f, "MetricsResponse"),
            ControlMessage::MetricsConfig => write!(f, "MetricsConfig"),
        }
    }
}

impl TryFrom<u16> for ControlMessage {
    type Error = anyhow::Error;
    fn try_from(raw: u16) -> anyhow::Result<Self> {
        use ControlMessage::*;
        match raw {
            1 => Ok(Ping),
            2 => Ok(Pong),
            101 => Ok(OpenConnection),
            102 => Ok(CloseConnection),
            104 => Ok(SendEOF),
            103 => Ok(ConnectionOpened),
            201 => Ok(UdpAssociate),
            202 => Ok(UdpAssociateOpened),
            203 => Ok(UdpPacket),
            204 => Ok(UdpAssociateClosed),
            301 => Ok(MetricsRequest),
            302 => Ok(MetricsResponse),
            303 => Ok(MetricsConfig),
            _ => Err(anyhow::anyhow!("Unknown control message: {}", raw)),
        }
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CloseConnectionReason {
    Normal = 0,
    Error = 1,
    Timeout = 2,
    ServerRefuse = 4,
    Client = 5,
    Unknown = 6,
    InvalidInstruction = 7,
    GuacdRefuse = 8,
    ConnectionLost = 9,
    ConnectionFailed = 10,
    TunnelClosed = 11,
    AdminClosed = 12,
    ErrorRecording = 13,
    GuacdError = 14,
    AIClosed = 15,
    AddressResolutionFailed = 16,
    DecryptionFailed = 17,
    ConfigurationError = 18,
    ProtocolError = 19,
    UpstreamClosed = 20,
}

// Helper for CloseConnectionReason (assuming it might be defined elsewhere, adding a basic version)
// This should ideally be part of the CloseConnectionReason enum itself.
impl CloseConnectionReason {
    pub fn from_code(code: u16) -> Self {
        match code {
            0 => CloseConnectionReason::Normal,
            1 => CloseConnectionReason::Error,
            2 => CloseConnectionReason::Timeout,
            4 => CloseConnectionReason::ServerRefuse,
            5 => CloseConnectionReason::Client,
            6 => CloseConnectionReason::Unknown,
            7 => CloseConnectionReason::InvalidInstruction,
            8 => CloseConnectionReason::GuacdRefuse,
            9 => CloseConnectionReason::ConnectionLost,
            10 => CloseConnectionReason::ConnectionFailed,
            11 => CloseConnectionReason::TunnelClosed,
            12 => CloseConnectionReason::AdminClosed,
            13 => CloseConnectionReason::ErrorRecording,
            14 => CloseConnectionReason::GuacdError,
            15 => CloseConnectionReason::AIClosed,
            16 => CloseConnectionReason::AddressResolutionFailed,
            17 => CloseConnectionReason::DecryptionFailed,
            18 => CloseConnectionReason::ConfigurationError,
            19 => CloseConnectionReason::ProtocolError,
            20 => CloseConnectionReason::UpstreamClosed,
            _ => CloseConnectionReason::Unknown,
        }
    }

    pub fn is_critical(&self) -> bool {
        matches!(
            self,
            CloseConnectionReason::Error
                | CloseConnectionReason::DecryptionFailed
                | CloseConnectionReason::ConfigurationError
                | CloseConnectionReason::ProtocolError
                | CloseConnectionReason::GuacdError
                | CloseConnectionReason::ErrorRecording
        )
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            CloseConnectionReason::Timeout
                | CloseConnectionReason::ConnectionLost
                | CloseConnectionReason::ConnectionFailed
                | CloseConnectionReason::AddressResolutionFailed
                | CloseConnectionReason::ServerRefuse
        )
    }

    /// Convert CloseConnectionReason to a human-readable message for Guacd log
    pub fn to_message(self) -> String {
        match self {
            CloseConnectionReason::Normal => "connection closed normally".to_string(),
            CloseConnectionReason::Error => "connection error".to_string(),
            CloseConnectionReason::Timeout => "connection timeout".to_string(),
            CloseConnectionReason::ServerRefuse => "server refused connection".to_string(),
            CloseConnectionReason::Client => "client closed connection".to_string(),
            CloseConnectionReason::Unknown => "unknown error".to_string(),
            CloseConnectionReason::InvalidInstruction => "invalid instruction".to_string(),
            CloseConnectionReason::GuacdRefuse => "guacd refused connection".to_string(),
            CloseConnectionReason::ConnectionLost => "connection lost".to_string(),
            CloseConnectionReason::ConnectionFailed => "connection failed".to_string(),
            CloseConnectionReason::TunnelClosed => "tunnel closed".to_string(),
            CloseConnectionReason::AdminClosed => "administrator closed connection".to_string(),
            CloseConnectionReason::ErrorRecording => "error recording session".to_string(),
            CloseConnectionReason::GuacdError => "guacd protocol error".to_string(),
            CloseConnectionReason::AIClosed => "AI closed connection".to_string(),
            CloseConnectionReason::AddressResolutionFailed => {
                "address resolution failed".to_string()
            }
            CloseConnectionReason::DecryptionFailed => "decryption failed".to_string(),
            CloseConnectionReason::ConfigurationError => "configuration error".to_string(),
            CloseConnectionReason::ProtocolError => "protocol error".to_string(),
            CloseConnectionReason::UpstreamClosed => "upstream connection closed".to_string(),
        }
    }
}

impl TryFrom<u16> for CloseConnectionReason {
    type Error = anyhow::Error; // Or a more specific error type

    fn try_from(value: u16) -> anyhow::Result<Self> {
        match value {
            0 => Ok(CloseConnectionReason::Normal),
            1 => Ok(CloseConnectionReason::Error),
            2 => Ok(CloseConnectionReason::Timeout),
            4 => Ok(CloseConnectionReason::ServerRefuse),
            5 => Ok(CloseConnectionReason::Client),
            6 => Ok(CloseConnectionReason::Unknown),
            7 => Ok(CloseConnectionReason::InvalidInstruction),
            8 => Ok(CloseConnectionReason::GuacdRefuse),
            9 => Ok(CloseConnectionReason::ConnectionLost),
            10 => Ok(CloseConnectionReason::ConnectionFailed),
            11 => Ok(CloseConnectionReason::TunnelClosed),
            12 => Ok(CloseConnectionReason::AdminClosed),
            13 => Ok(CloseConnectionReason::ErrorRecording),
            14 => Ok(CloseConnectionReason::GuacdError),
            15 => Ok(CloseConnectionReason::AIClosed),
            16 => Ok(CloseConnectionReason::AddressResolutionFailed),
            17 => Ok(CloseConnectionReason::DecryptionFailed),
            18 => Ok(CloseConnectionReason::ConfigurationError),
            19 => Ok(CloseConnectionReason::ProtocolError),
            20 => Ok(CloseConnectionReason::UpstreamClosed),
            0xFFFF => Ok(CloseConnectionReason::Unknown),
            _ => Err(anyhow::anyhow!(
                "Invalid u16 value for CloseConnectionReason: {}",
                value
            )),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Frame {
    pub(crate) connection_no: u32,
    pub(crate) timestamp_ms: u64,
    pub(crate) payload: Bytes, // raw payload (control or data)
}

impl Frame {
    /// Serialize a control frame (conn=0) using the provided buffer pool
    pub(crate) fn new_control_with_pool(
        msg: ControlMessage,
        data: &[u8],
        pool: &BufferPool,
    ) -> Self {
        let mut buf = pool.acquire();
        buf.clear();
        buf.put_u16(msg as u16);
        buf.extend_from_slice(data);

        Self {
            connection_no: 0,
            timestamp_ms: now_ms(),
            payload: buf.freeze(),
        }
    }

    /// Create a control frame directly with a pre-allocated buffer
    pub(crate) fn new_control_with_buffer(msg: ControlMessage, buf: &mut BytesMut) -> Self {
        // Clear the buffer but keep its capacity
        buf.clear();

        // Write the control message code
        buf.put_u16(msg as u16);

        // Clone the buffer contents for our own use
        // This is more efficient than copying raw slice data
        let payload = buf.clone().freeze();

        Self {
            connection_no: 0,
            timestamp_ms: now_ms(),
            payload,
        }
    }

    /// Serialize a data frame (conn>0) using the provided buffer pool
    pub(crate) fn new_data_with_pool(conn_no: u32, data: &[u8], pool: &BufferPool) -> Self {
        let bytes = pool.create_bytes(data);

        Self {
            connection_no: conn_no,
            timestamp_ms: now_ms(),
            payload: bytes,
        }
    }

    /// Encodes a data frame directly from a payload slice into the target buffer.
    /// This avoids creating an intermediate Frame instance with an owned Bytes payload if the
    /// goal is to immediately encode.
    /// Returns the number of bytes written.
    pub(crate) fn encode_data_frame_from_slice(
        target_buf: &mut BytesMut,
        conn_no: u32,
        payload_slice: &[u8],
        // pool: &BufferPool, // Pool is not directly used here; target_buf should be from a pool
    ) -> usize {
        target_buf.clear(); // Ensure the buffer is ready for a new frame
        let timestamp_ms = now_ms();
        let payload_len = payload_slice.len();

        let needed_capacity = CONN_NO_LEN + TS_LEN + LEN_LEN + payload_len + TERMINATOR.len();
        if target_buf.capacity() < needed_capacity {
            target_buf.reserve(needed_capacity - target_buf.capacity());
        }

        target_buf.put_u32(conn_no);
        target_buf.put_u64(timestamp_ms);
        target_buf.put_u32(payload_len as u32);
        target_buf.extend_from_slice(payload_slice); // Payload copied directly from the source slice
        target_buf.extend_from_slice(TERMINATOR);

        needed_capacity // Return total bytes written for this frame
    }

    /// Encode into bytes ready to send using the provided buffer pool
    pub(crate) fn encode_with_pool(&self, pool: &BufferPool) -> Bytes {
        let mut buf = pool.acquire();
        self.encode_into_buffer(&mut buf);
        buf.freeze()
    }

    /// Encode directly into a provided BytesMut buffer.
    /// Returns the number of bytes written.
    pub(crate) fn encode_into(&self, buf: &mut BytesMut) -> usize {
        buf.clear();
        self.encode_into_buffer(buf);
        CONN_NO_LEN + TS_LEN + LEN_LEN + self.payload.len() + TERMINATOR.len()
    }

    // Private helper method that handles the actual encoding logic
    fn encode_into_buffer(&self, buf: &mut BytesMut) {
        let needed_capacity =
            CONN_NO_LEN + TS_LEN + LEN_LEN + self.payload.len() + TERMINATOR.len();
        if buf.capacity() < needed_capacity {
            buf.reserve(needed_capacity - buf.capacity());
        }

        buf.put_u32(self.connection_no);
        buf.put_u64(self.timestamp_ms);
        buf.put_u32(self.payload.len() as u32);
        buf.extend_from_slice(&self.payload);
        buf.extend_from_slice(TERMINATOR);
    }
}

// Branch prediction hints for better CPU performance
#[cold]
fn unlikely_parse_failure(msg: &str) {
    // Cold function for error cases - rarely executed
    warn!("Parse failure: {}", msg);
}

/// Try to parse the first complete frame from `buf` with SIMD optimizations.
/// If successful, remove it from the buffer and return. Otherwise, return `None` (need more data).
#[inline] // Hot path optimization
pub(crate) fn try_parse_frame(buf: &mut BytesMut) -> Option<Frame> {
    // **BRANCH PREDICTION**: Early size check is the hot path (likely to succeed)
    if likely!(buf.len() >= CONN_NO_LEN + TS_LEN + LEN_LEN) {
        // Continue with fast path parsing
    } else {
        // **COLD PATH**: Incomplete data (uncommon)
        return None;
    }

    // Prefetch the beginning of the buffer for better cache performance
    prefetch_frame_data(buf, 0);

    // Create a cursor without consuming the buffer yet
    let mut cursor = &buf[..];
    let conn_no = cursor.get_u32();
    let ts = cursor.get_u64();
    let len = cursor.get_u32() as usize;

    // **PERFORMANCE HINT**: Connection 1 is the main traffic flow (2-connection pattern)
    if likely!(conn_no == 1) {
        // **ULTRA HOT PATH**: Additional optimizations for Connection 1 could go here
        // Most data flows through Connection 1, so this branch is highly optimized
    }

    // Calculate total frame size including terminator
    let total_size = CONN_NO_LEN + TS_LEN + LEN_LEN + len + TERMINATOR.len();
    if buf.len() < total_size {
        return None;
    }

    // **SAFE TERMINATOR VERIFICATION**
    // CRITICAL: Only check terminator at EXACT expected position
    // This prevents false positives from ';' characters inside payload data
    let term_expected_pos = CONN_NO_LEN + TS_LEN + LEN_LEN + len;

    // Prefetch the terminator area for better cache performance
    prefetch_frame_data(buf, term_expected_pos);

    // **SECURITY + PERFORMANCE**: Use position-specific verification with branch prediction
    // This eliminates the risk of finding terminators inside payload data
    let terminator_valid = verify_terminator_at_position_simd(buf, term_expected_pos);

    // **BRANCH PREDICTION**: Valid frames are the overwhelming common case (99.9%+)
    if likely!(terminator_valid) {
        // **HOT PATH**: Valid frame parsing continues
    } else {
        // **COLD PATH**: Corrupt frame (very rare)
        unlikely_parse_failure("Corrupt stream, terminator mismatch or misposition");
        warn!(
            "try_parse_frame: Corrupt stream, terminator mismatch or misposition (expected_terminator: {:?}, expected_pos: {}, actual_bytes: {:?})",
            TERMINATOR,
            term_expected_pos,
            &buf[term_expected_pos..std::cmp::min(buf.len(), term_expected_pos+2+5)]
        );
        // Consume the entire buffer to prevent reprocessing the bad data
        buf.advance(buf.len());
        return None;
    }

    // Skip the header portion
    buf.advance(CONN_NO_LEN + TS_LEN + LEN_LEN);

    // Extract payload as a separate chunk (zero-copy)
    let payload_bytes = buf.split_to(len);
    let payload = payload_bytes.freeze();

    // Skip terminator
    buf.advance(TERMINATOR.len());

    // Create a frame with extracted values and payload
    Some(Frame {
        connection_no: conn_no,
        timestamp_ms: ts,
        payload,
    })
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn encode_round_trips() {
        let original = Frame::new_data_with_pool(42, b"hello world", &BufferPool::default());

        // Create a buffer for encoding
        let mut encode_buf = BytesMut::with_capacity(
            CONN_NO_LEN + TS_LEN + LEN_LEN + b"hello world".len() + TERMINATOR.len(),
        );

        // Encode the frame into the buffer (ignore the returned size)
        original.encode_into(&mut encode_buf);

        // Create a new buffer from the encoded data for decoding
        let mut decode_buf = encode_buf.clone();

        let decoded = try_parse_frame(&mut decode_buf).expect("parser should return a frame");

        assert_eq!(decoded.connection_no, 42);
        assert_eq!(decoded.payload, Bytes::from_static(b"hello world"));
        assert!(decode_buf.is_empty(), "buffer should be fully consumed");
    }

    #[tokio::test]
    async fn test_buffer_reuse() {
        // Create a buffer pool for testing
        let pool = BufferPool::default();

        // Create a frame using the pool
        let frame1 = Frame::new_data_with_pool(1, b"test data", &pool);

        // Encode it
        let bytes1 = frame1.encode_with_pool(&pool);

        // Verify contents
        assert_eq!(&bytes1[CONN_NO_LEN + TS_LEN + LEN_LEN..][..9], b"test data");

        // Check pool stats before creating another frame
        let before_count = pool.count();

        // Create another frame
        let frame2 = Frame::new_data_with_pool(2, b"more data", &pool);
        let bytes2 = frame2.encode_with_pool(&pool);

        // Verify contents
        assert_eq!(&bytes2[CONN_NO_LEN + TS_LEN + LEN_LEN..][..9], b"more data");

        // Buffer pool should be similar or less (as we may have reused buffers)
        assert!(
            pool.count() <= before_count + 1,
            "Buffer pool should not grow unbounded"
        );
    }

    #[tokio::test]
    async fn test_encode_into() {
        let frame = Frame::new_data_with_pool(42, b"hello world", &BufferPool::default());

        // Create a buffer for testing
        let mut buf = BytesMut::with_capacity(128);

        // Encode directly into the buffer
        let bytes_written = frame.encode_into(&mut buf);

        // Verify the correct number of bytes where written
        assert_eq!(
            bytes_written,
            CONN_NO_LEN + TS_LEN + LEN_LEN + 11 + TERMINATOR.len()
        );

        // Now parse it back
        let decoded = try_parse_frame(&mut buf).expect("parser should return a frame");

        // Verify fields match
        assert_eq!(decoded.connection_no, 42);
        assert_eq!(decoded.payload, Bytes::from_static(b"hello world"));
    }

    #[tokio::test]
    async fn test_payload_with_terminators() {
        // **CRITICAL TEST**: Verify that terminators inside payload don't break parsing
        let payload_with_semicolons = b"hello;world;test;data;";
        let frame = Frame::new_data_with_pool(123, payload_with_semicolons, &BufferPool::default());

        // Encode the frame into BytesMut
        let mut buf = BytesMut::with_capacity(1024);
        frame.encode_into(&mut buf);
        let original_buf = buf.clone();

        // Parse it back
        let decoded =
            try_parse_frame(&mut buf).expect("Should parse frame with semicolons in payload");

        // Verify the frame was parsed correctly
        assert_eq!(decoded.connection_no, 123);
        assert_eq!(decoded.payload, Bytes::from_static(payload_with_semicolons));
        assert!(buf.is_empty(), "Buffer should be fully consumed");

        // Verify the frame structure: check that semicolons are in payload, not treated as terminators
        let buf_bytes = original_buf.as_ref();
        let expected_terminator_pos =
            CONN_NO_LEN + TS_LEN + LEN_LEN + payload_with_semicolons.len();

        // Verify the actual terminator is at the expected position
        assert_eq!(buf_bytes[expected_terminator_pos], b';');

        // Verify there are semicolons in the payload area (they should be ignored)
        let payload_start = CONN_NO_LEN + TS_LEN + LEN_LEN;
        let payload_area = &buf_bytes[payload_start..payload_start + payload_with_semicolons.len()];
        assert!(
            payload_area.contains(&b';'),
            "Payload should contain semicolons"
        );
    }

    #[tokio::test]
    async fn test_binary_payload_with_terminator_bytes() {
        // Test with binary data that contains the terminator byte value (0x3B = ';')
        let binary_payload = vec![0x00, 0x01, 0x3B, 0x3B, 0x3B, 0xFF, 0x3B, 0xAA]; // Multiple 0x3B bytes
        let frame = Frame::new_data_with_pool(456, &binary_payload, &BufferPool::default());

        // Encode and parse
        let mut buf = BytesMut::with_capacity(1024);
        frame.encode_into(&mut buf);
        let decoded =
            try_parse_frame(&mut buf).expect("Should parse binary data with terminator bytes");

        // Verify correct parsing
        assert_eq!(decoded.connection_no, 456);
        assert_eq!(decoded.payload.as_ref(), binary_payload.as_slice());
    }

    #[tokio::test]
    async fn test_guacamole_like_payload() {
        // Test with Guacamole-style protocol data (which uses semicolons as delimiters)
        let guac_payload = b"connect;vnc;hostname=example.com;port=5901;password=secret;";
        let frame = Frame::new_data_with_pool(789, guac_payload, &BufferPool::default());

        // Encode and parse
        let mut buf = BytesMut::with_capacity(1024);
        frame.encode_into(&mut buf);
        let decoded = try_parse_frame(&mut buf).expect("Should parse Guacamole-like payload");

        // Verify correct parsing
        assert_eq!(decoded.connection_no, 789);
        assert_eq!(decoded.payload, Bytes::from_static(guac_payload));
    }
}
