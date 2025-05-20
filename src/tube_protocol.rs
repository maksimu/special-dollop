/* ----------------------------------------------------------------------------------------------
 *   Wire format (see Python PortForwardsExit)
 *   ┌───────────┬────────────┬────────────┬───────────────────────┬───────────┐
 *   │ ConnNo(4) │ TsMs(8)    │ Len(4)     │ Payload[Len]          │ TERM      │
 *   └───────────┴────────────┴────────────┴───────────────────────┴───────────┘
 *   Connection 0 means a control packet whose payload starts with a 2‑byte
 *   ControlMessage enum code followed by message‑specific data.
 * ------------------------------------------------------------------------------------------- */
use std::time::{SystemTime, UNIX_EPOCH};
use bytes::{Buf, BufMut, BytesMut, Bytes};
use crate::buffer_pool::BufferPool;

pub(crate) const CONN_NO_LEN: usize = 4;
pub(crate) const CTRL_NO_LEN: usize = 2;
const TS_LEN: usize = 8;
const LEN_LEN: usize = 4;

/// Terminator taken from Python constant `TERMINATOR`; adjust if necessary.
const TERMINATOR: &[u8] = b";";

#[repr(u16)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ControlMessage {
    Ping = 1,
    Pong = 2,
    OpenConnection = 101,
    CloseConnection = 102,
    SendEOF = 104,
    ConnectionOpened = 103,
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
        }
    }
}

impl TryFrom<u16> for ControlMessage {
    type Error = anyhow::Error;
    fn try_from(raw: u16) -> anyhow::Result<Self, Self::Error> {
        use ControlMessage::*;
        match raw {
            1 => Ok(Ping),
            2 => Ok(Pong),
            101 => Ok(OpenConnection),
            102 => Ok(CloseConnection),
            104 => Ok(SendEOF),
            103 => Ok(ConnectionOpened),
            _ => Err(anyhow::anyhow!("Unknown control message: {}", raw)),
        }
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CloseConnectionReason {
    Normal = 0,
    ConnectionFailed = 1,
    ConnectionLost = 2,
    Timeout = 3,
    AIClosed = 4,
    AddressResolutionFailed = 5,
    DecryptionFailed = 6,
    ConfigurationError = 7,
    ProtocolError = 8,
    Unknown = 0xFFFF,
}

// Helper for CloseConnectionReason (assuming it might be defined elsewhere, adding a basic version)
// This should ideally be part of the CloseConnectionReason enum itself.
impl CloseConnectionReason {
    pub fn from_code(code: u16) -> Self {
        match code {
            0 => CloseConnectionReason::Normal,
            1 => CloseConnectionReason::ConnectionFailed,
            2 => CloseConnectionReason::ConnectionLost,
            3 => CloseConnectionReason::Timeout,
            4 => CloseConnectionReason::AIClosed,
            5 => CloseConnectionReason::AddressResolutionFailed,
            6 => CloseConnectionReason::DecryptionFailed,
            7 => CloseConnectionReason::ConfigurationError,
            8 => CloseConnectionReason::ProtocolError,
            _ => CloseConnectionReason::Unknown,
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
    pub(crate) fn new_control_with_pool(msg: ControlMessage, data: &[u8], pool: &BufferPool) -> Self {
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
        let needed_capacity = CONN_NO_LEN + TS_LEN + LEN_LEN + self.payload.len() + TERMINATOR.len();
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

/// Try to parse the first complete frame from `buf`.  If successful, remove it
/// from the buffer and return.  Otherwise, return `None` (need more data).
pub(crate) fn try_parse_frame(buf: &mut BytesMut) -> Option<Frame> {
    // Early check for minimum size before any processing
    if buf.len() < CONN_NO_LEN + TS_LEN + LEN_LEN {
        return None;
    }
    
    // Create a cursor without consuming the buffer yet
    let mut cursor = &buf[..];
    let conn_no = cursor.get_u32();
    let ts = cursor.get_u64();
    let len = cursor.get_u32() as usize;
    
    // Calculate total frame size including terminator
    let total_size = CONN_NO_LEN + TS_LEN + LEN_LEN + len + TERMINATOR.len();
    if buf.len() < total_size {
        return None;
    }
    
    // Verify the terminator before any allocation
    let term_start = CONN_NO_LEN + TS_LEN + LEN_LEN + len;
    if &buf[term_start..term_start + TERMINATOR.len()] != TERMINATOR {
        // corrupt stream; drop everything
        log::warn!("try_parse_frame: Corrupt stream, terminator mismatch. Expected {:?}, got {:?}", 
                  TERMINATOR, &buf[term_start..term_start + TERMINATOR.len()]);
        buf.clear();
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
        let mut encode_buf = BytesMut::with_capacity(CONN_NO_LEN + TS_LEN + LEN_LEN + b"hello world".len() + TERMINATOR.len());

        // Encode the frame into the buffer (ignore the returned size)
        original.encode_into(&mut encode_buf);

        // Create a new buffer from the encoded data for decoding
        let mut decode_buf = encode_buf.clone();

        let decoded = try_parse_frame(&mut decode_buf)
            .expect("parser should return a frame");

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
        assert!(pool.count() <= before_count + 1, 
            "Buffer pool should not grow unbounded");
    }
    
    #[tokio::test]
    async fn test_encode_into() {
        let frame = Frame::new_data_with_pool(42, b"hello world", &BufferPool::default());
        
        // Create a buffer for testing
        let mut buf = BytesMut::with_capacity(128);
        
        // Encode directly into the buffer
        let bytes_written = frame.encode_into(&mut buf);
        
        // Verify the correct number of bytes where written
        assert_eq!(bytes_written, CONN_NO_LEN + TS_LEN + LEN_LEN + 11 + TERMINATOR.len());
        
        // Now parse it back
        let decoded = try_parse_frame(&mut buf)
            .expect("parser should return a frame");
            
        // Verify fields match
        assert_eq!(decoded.connection_no, 42);
        assert_eq!(decoded.payload, Bytes::from_static(b"hello world"));
    }
}
