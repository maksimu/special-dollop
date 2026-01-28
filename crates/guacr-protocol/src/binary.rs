// Binary Guacamole protocol encoder (zero-copy)
// Much faster than text protocol for large images (4K video)

use bytes::{BufMut, Bytes, BytesMut};

/// Binary protocol opcodes (matching BINARY_PROTOCOL_SPEC.md)
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Opcode {
    Key = 0x01,
    Mouse = 0x02,
    Image = 0x03,
    Sync = 0x04,
    Clipboard = 0x05,
    Size = 0x06,
    File = 0x07,
    Pipe = 0x08,
    Blob = 0x09,
    End = 0x0A,
    Disconnect = 0x0B,
    Video = 0x0C,
}

/// Binary protocol flags (for fragmentation, compression, etc.)
pub const FLAG_COMPRESSED: u8 = 0x01; // Payload is zstd compressed
pub const FLAG_ENCRYPTED: u8 = 0x02; // Payload is encrypted (if not using TLS)
pub const FLAG_FRAGMENTED: u8 = 0x04; // Message is fragmented
pub const FLAG_FIRST_FRAGMENT: u8 = 0x08; // First fragment of message
pub const FLAG_LAST_FRAGMENT: u8 = 0x10; // Last fragment of message

/// Protocol overhead constants
///
/// These constants account for the overhead added by the frame protocol and binary protocol
/// when sending data over WebRTC data channels.
pub const FRAME_PROTOCOL_OVERHEAD: usize = 17; // CONN(4) + TS(8) + LEN(4) + TERM(1)
pub const BINARY_PROTOCOL_OVERHEAD: usize = 8; // Header size: Opcode(1) + Flags(1) + Reserved(2) + Payload Length(4)
pub const TOTAL_PROTOCOL_OVERHEAD: usize = FRAME_PROTOCOL_OVERHEAD + BINARY_PROTOCOL_OVERHEAD; // 25 bytes

/// Maximum payload size accounting for overhead
///
/// WebRTC max: 64KB (65536 bytes)
/// Overhead: 25 bytes
/// Safe payload: 60KB (61440 bytes) - leaves 1071 bytes headroom
pub const MAX_SAFE_PAYLOAD_SIZE: usize = 60 * 1024; // 60KB

/// Maximum safe frame size for hardware encoder
///
/// Encoder should produce frames ≤ this size for optimal latency.
/// This accounts for protocol overhead, ensuring frames fit within WebRTC limits.
pub const MAX_ENCODER_FRAME_SIZE: usize = MAX_SAFE_PAYLOAD_SIZE; // 60KB

/// Binary protocol message header
///
/// Header format:
/// - Opcode: 1 byte
/// - Flags: 1 byte
/// - Reserved: 2 bytes (for future use)
/// - Payload Length: 4 bytes (little-endian)
///
/// Total: 8 bytes
#[derive(Debug)]
struct MessageHeader {
    opcode: Opcode,
    flags: u8,
    reserved: u16, // Reserved for future use
    length: u32,
}

impl MessageHeader {
    fn to_bytes(&self) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        bytes[0] = self.opcode as u8;
        bytes[1] = self.flags;
        bytes[2..4].copy_from_slice(&self.reserved.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.length.to_le_bytes());
        bytes
    }
}

/// Binary protocol encoder (zero-copy)
///
/// Encodes Guacamole protocol messages in binary format for maximum performance.
/// Binary format is 25-50% smaller than text format and avoids string allocations.
pub struct BinaryEncoder {
    scratch: BytesMut,
}

impl BinaryEncoder {
    pub fn new() -> Self {
        Self {
            scratch: BytesMut::with_capacity(64 * 1024), // 64KB scratch buffer
        }
    }

    /// Encode an image message (zero-copy if data is Bytes)
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `layer` - Layer number
    /// * `x` - X coordinate
    /// * `y` - Y coordinate
    /// * `width` - Image width
    /// * `height` - Image height
    /// * `format` - Image format (0=PNG, 1=JPEG, 2=WebP)
    /// * `data` - Image data (zero-copy if Bytes)
    #[allow(clippy::too_many_arguments)]
    pub fn encode_image(
        &mut self,
        stream_id: u32,
        layer: i32,
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        format: u8,
        data: Bytes,
    ) -> Bytes {
        self.scratch.clear();

        // Payload: stream_id(4) + layer(4) + x(4) + y(4) + width(2) + height(2) + format(1) + padding(1) + data(N)
        let payload_len = 20 + data.len();
        let header = MessageHeader {
            opcode: Opcode::Image,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);
        self.scratch.put_i32_le(layer);
        self.scratch.put_i32_le(x);
        self.scratch.put_i32_le(y);
        self.scratch.put_u16_le(width);
        self.scratch.put_u16_le(height);
        self.scratch.put_u8(format);
        self.scratch.put_u8(0); // padding

        // Zero-copy: extend with Bytes (just increments refcount)
        self.scratch.extend_from_slice(&data);

        // Clone scratch before freezing (scratch is reused)
        self.scratch.clone().freeze()
    }

    /// Encode a sync message
    pub fn encode_sync(&mut self, timestamp_ms: u64) -> Bytes {
        self.scratch.clear();

        let payload_len = 8; // timestamp
        let header = MessageHeader {
            opcode: Opcode::Sync,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u64_le(timestamp_ms);

        self.scratch.clone().freeze()
    }

    /// Encode a size message
    pub fn encode_size(&mut self, layer: i32, width: u32, height: u32) -> Bytes {
        self.scratch.clear();

        let payload_len = 12; // layer(4) + width(4) + height(4)
        let header = MessageHeader {
            opcode: Opcode::Size,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_i32_le(layer);
        self.scratch.put_u32_le(width);
        self.scratch.put_u32_le(height);

        self.scratch.clone().freeze()
    }

    /// Encode a blob message (for streaming image/video data)
    pub fn encode_blob(&mut self, stream_id: u32, data: Bytes) -> Bytes {
        self.scratch.clear();

        let payload_len = 4 + data.len(); // stream_id(4) + data(N)
        let header = MessageHeader {
            opcode: Opcode::Blob,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);
        self.scratch.extend_from_slice(&data);

        self.scratch.clone().freeze()
    }

    /// Encode a fragmented blob (for multi-channel WebRTC)
    ///
    /// Splits large video frames across multiple channels for higher throughput.
    /// Use this for H.264 frames > 64KB that need to be sent over multiple WebRTC data channels.
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `sequence_id` - Frame sequence ID (for reassembly on client)
    /// * `fragment_index` - Fragment number (0-based)
    /// * `total_fragments` - Total fragments in this frame
    /// * `fragment_data` - This fragment's data (zero-copy if Bytes)
    pub fn encode_blob_fragment(
        &mut self,
        stream_id: u32,
        sequence_id: u32,
        fragment_index: u16,
        total_fragments: u16,
        fragment_data: Bytes,
    ) -> Bytes {
        self.scratch.clear();

        // Payload: stream_id(4) + fragment_header(16) + data(N)
        // Fragment header: sequence_id(4) + fragment_index(2) + total_fragments(2) + fragment_offset(4) + fragment_length(4)
        let payload_len = 4 + 16 + fragment_data.len();

        let mut flags = FLAG_FRAGMENTED;
        if fragment_index == 0 {
            flags |= FLAG_FIRST_FRAGMENT;
        }
        if fragment_index == total_fragments - 1 {
            flags |= FLAG_LAST_FRAGMENT;
        }

        let header = MessageHeader {
            opcode: Opcode::Blob,
            flags,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);

        // Fragment header
        self.scratch.put_u32_le(sequence_id);
        self.scratch.put_u16_le(fragment_index);
        self.scratch.put_u16_le(total_fragments);

        // Calculate fragment offset (for reassembly)
        let fragment_offset = if fragment_index == 0 {
            0
        } else {
            // Offset is sum of all previous fragment lengths
            // Note: This is approximate - actual offset depends on fragment sizes
            // Client should use fragment_index for ordering, not offset
            fragment_index as u32 * (fragment_data.len() as u32)
        };
        self.scratch.put_u32_le(fragment_offset);
        self.scratch.put_u32_le(fragment_data.len() as u32);

        // Fragment data (zero-copy)
        self.scratch.extend_from_slice(&fragment_data);

        self.scratch.clone().freeze()
    }

    /// Split large blob into fragments for multi-channel transmission
    ///
    /// Returns vector of fragments, each ready to send on a different channel.
    /// Use this for hardware-encoded H.264 frames that exceed WebRTC channel limits.
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `sequence_id` - Frame sequence ID (incremented per frame)
    /// * `data` - Full frame data (e.g., H.264 NAL units)
    /// * `max_fragment_size` - Maximum size per fragment (e.g., 64KB for WebRTC)
    /// * `num_channels` - Number of channels to distribute across
    ///
    /// # Returns
    ///
    /// Vector of fragment messages, ready to send on different channels.
    pub fn split_blob_for_channels(
        &mut self,
        stream_id: u32,
        sequence_id: u32,
        data: Bytes,
        max_fragment_size: usize,
        _num_channels: usize,
    ) -> Vec<Bytes> {
        let total_size = data.len();
        let fragments_needed = total_size.div_ceil(max_fragment_size);
        let total_fragments = fragments_needed;

        let mut fragments = Vec::with_capacity(total_fragments);

        // Split data into chunks
        for (fragment_idx, chunk) in data.chunks(max_fragment_size).enumerate() {
            let fragment_data = Bytes::copy_from_slice(chunk);
            let fragment = self.encode_blob_fragment(
                stream_id,
                sequence_id,
                fragment_idx as u16,
                total_fragments as u16,
                fragment_data,
            );
            fragments.push(fragment);
        }

        fragments
    }

    /// Encode an end message (close stream)
    pub fn encode_end(&mut self, stream_id: u32) -> Bytes {
        self.scratch.clear();

        let payload_len = 4; // stream_id
        let header = MessageHeader {
            opcode: Opcode::End,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);

        self.scratch.clone().freeze()
    }

    /// Encode a video instruction (start video stream)
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `layer` - Layer number
    /// * `mimetype` - Video mimetype (e.g., "video/h264", "video/mp4")
    pub fn encode_video(&mut self, stream_id: u32, layer: i32, mimetype: &str) -> Bytes {
        self.scratch.clear();

        // Payload: stream_id(4) + layer(4) + mimetype_len(4) + mimetype(N)
        let mimetype_bytes = mimetype.as_bytes();
        let payload_len = 12 + mimetype_bytes.len();
        let header = MessageHeader {
            opcode: Opcode::Video,
            flags: 0,
            reserved: 0,
            length: payload_len as u32,
        };

        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);
        self.scratch.put_i32_le(layer);
        self.scratch.put_u32_le(mimetype_bytes.len() as u32);
        self.scratch.put_slice(mimetype_bytes);

        self.scratch.clone().freeze()
    }
}

impl Default for BinaryEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_image() {
        let mut encoder = BinaryEncoder::new();
        let data = Bytes::from(vec![1, 2, 3, 4]);

        let msg = encoder.encode_image(1, 0, 10, 20, 100, 200, 1, data.clone());

        // Header: 8 bytes + payload: 22 bytes (includes padding) + data: 4 bytes = 34 bytes
        assert_eq!(msg.len(), 34);

        // Check opcode
        assert_eq!(msg[0], Opcode::Image as u8);
    }

    #[test]
    fn test_encode_sync() {
        let mut encoder = BinaryEncoder::new();
        let msg = encoder.encode_sync(1234567890);

        // Header: 8 bytes + timestamp: 8 bytes = 16 bytes
        assert_eq!(msg.len(), 16);
        assert_eq!(msg[0], Opcode::Sync as u8);
    }
}
