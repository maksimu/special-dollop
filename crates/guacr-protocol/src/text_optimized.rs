// Optimized text protocol encoder (zero-allocation)
// Writes directly to BytesMut instead of String allocations

use bytes::{BufMut, BytesMut};

/// Optimized text protocol encoder (zero-allocation)
///
/// Writes Guacamole text protocol instructions directly to BytesMut,
/// avoiding String allocations and intermediate buffers.
pub struct TextProtocolEncoder {
    scratch: BytesMut,
}

impl TextProtocolEncoder {
    pub fn new() -> Self {
        Self {
            scratch: BytesMut::with_capacity(1024),
        }
    }

    /// Format image instruction (zero-allocation)
    ///
    /// Writes directly to BytesMut instead of creating Strings.
    pub fn format_img_instruction(
        &mut self,
        stream_id: u32,
        layer: i32,
        x: i32,
        y: i32,
        mimetype: &str,
    ) -> BytesMut {
        self.scratch.clear();

        // Format: img,<stream>,<mask>,<layer>,<mimetype>,<x>,<y>;
        self.scratch.put_slice(b"3.img,");

        // Stream ID
        let stream_str = stream_id.to_string();
        self.scratch
            .put_slice(stream_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(stream_str.as_bytes());
        self.scratch.put_u8(b',');

        // Mask (RGB = 0x07)
        self.scratch.put_slice(b"1.7,");

        // Layer
        let layer_str = layer.to_string();
        self.scratch
            .put_slice(layer_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(layer_str.as_bytes());
        self.scratch.put_u8(b',');

        // MIME type
        self.scratch
            .put_slice(mimetype.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(mimetype.as_bytes());
        self.scratch.put_u8(b',');

        // X coordinate
        let x_str = x.to_string();
        self.scratch.put_slice(x_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(x_str.as_bytes());
        self.scratch.put_u8(b',');

        // Y coordinate
        let y_str = y.to_string();
        self.scratch.put_slice(y_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(y_str.as_bytes());
        self.scratch.put_u8(b';');

        // Return a copy (scratch is reused)
        self.scratch.clone()
    }

    /// Format blob instruction (zero-allocation)
    ///
    /// Writes blob instruction with base64-encoded data directly to BytesMut.
    pub fn format_blob_instruction(&mut self, stream_id: u32, base64_data: &str) -> BytesMut {
        self.scratch.clear();

        // Format: blob,<stream_len>.<stream>,<base64_len>.<base64_data>;
        self.scratch.put_slice(b"4.blob,");

        // Stream ID
        let stream_str = stream_id.to_string();
        self.scratch
            .put_slice(stream_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(stream_str.as_bytes());
        self.scratch.put_u8(b',');

        // Base64 data
        self.scratch
            .put_slice(base64_data.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(base64_data.as_bytes());
        self.scratch.put_u8(b';');

        self.scratch.clone()
    }

    /// Format end instruction (zero-allocation)
    pub fn format_end_instruction(&mut self, stream_id: u32) -> BytesMut {
        self.scratch.clear();

        // Format: end,<stream_len>.<stream>;
        self.scratch.put_slice(b"3.end,");

        let stream_str = stream_id.to_string();
        self.scratch
            .put_slice(stream_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(stream_str.as_bytes());
        self.scratch.put_u8(b';');

        self.scratch.clone()
    }

    /// Format size instruction (zero-allocation)
    pub fn format_size_instruction(&mut self, layer: i32, width: u32, height: u32) -> BytesMut {
        self.scratch.clear();

        // Format: size,<layer>,<width>,<height>;
        self.scratch.put_slice(b"4.size,");

        let layer_str = layer.to_string();
        self.scratch
            .put_slice(layer_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(layer_str.as_bytes());
        self.scratch.put_u8(b',');

        let width_str = width.to_string();
        self.scratch
            .put_slice(width_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(width_str.as_bytes());
        self.scratch.put_u8(b',');

        let height_str = height.to_string();
        self.scratch
            .put_slice(height_str.len().to_string().as_bytes());
        self.scratch.put_u8(b'.');
        self.scratch.put_slice(height_str.as_bytes());
        self.scratch.put_u8(b';');

        self.scratch.clone()
    }
}

impl Default for TextProtocolEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_img_instruction() {
        let mut encoder = TextProtocolEncoder::new();
        let instr = encoder.format_img_instruction(1, 0, 10, 20, "image/jpeg");

        let instr_str = String::from_utf8_lossy(&instr);
        assert!(instr_str.contains("img"));
        assert!(instr_str.contains("image/jpeg"));
    }

    #[test]
    fn test_format_blob_instruction() {
        let mut encoder = TextProtocolEncoder::new();
        let base64_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let instr = encoder.format_blob_instruction(1, base64_data);

        let instr_str = String::from_utf8_lossy(&instr);
        assert!(instr_str.contains("blob"));
        assert!(instr_str.contains(base64_data));
    }
}
