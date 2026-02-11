//! Shared cursor management for graphical protocols
//!
//! Provides client-side cursor rendering support for RDP, VNC, and RBI protocols.
//! This matches Apache Guacamole's approach where cursor bitmaps are sent to the client
//! and rendered locally in the browser for smooth 60fps cursor movement.
//!
//! Standard cursors (pointer, I-beam, dot, hidden) are embedded as RGBA bitmaps and
//! sent via the same img/blob/end/cursor instruction sequence as custom cursors. This
//! is required because the Guacamole `cursor` instruction expects a numeric layer index,
//! not a cursor name string.

use bytes::Bytes;
use guacr_protocol::{format_cursor, format_size};
use log::{debug, warn};

// --- Embedded cursor bitmaps (RGBA, 4 bytes per pixel) ---

/// Standard pointer arrow cursor: 11x19 pixels, hotspot at (1, 1).
/// Black arrow with white 1px outline, matching the Apache guacd default pointer.
/// 11 * 19 * 4 = 836 bytes.
#[rustfmt::skip]
const POINTER_CURSOR_RGBA: &[u8] = &[
    // Row 0 (y=0): tip of arrow
    0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 1 (y=1)
    0,0,0,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 2
    0,0,0,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 3
    0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 4
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 5
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 6
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 7
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 8
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0,
    // Row 9
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0,
    // Row 10
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255,
    // Row 11 - bottom of filled triangle, start of notch
    0,0,0,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 12
    0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 13
    0,0,0,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 14
    0,0,0,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 15
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0,
    // Row 16
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,255, 255,255,255,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0,
    // Row 17
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,255, 255,255,255,255, 0,0,0,255, 0,0,0,0, 0,0,0,0,
    // Row 18
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,255, 0,0,0,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
];

/// Hidden cursor: 1x1 fully transparent pixel.
const HIDDEN_CURSOR_RGBA: &[u8] = &[0, 0, 0, 0];

/// I-beam (text) cursor: 7x16 pixels, hotspot at (3, 8).
/// White thin vertical bar with serifs at top and bottom.
/// White color ensures visibility on dark terminal backgrounds.
#[rustfmt::skip]
const IBEAM_CURSOR_RGBA: &[u8] = &[
    // Row 0: top serif
    0,0,0,0, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,0,
    // Row 1
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 2
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 3
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 4
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 5
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 6
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 7
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 8
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 9
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 10
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 11
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 12
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 13
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 14
    0,0,0,0, 0,0,0,0, 0,0,0,0, 255,255,255,255, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    // Row 15: bottom serif
    0,0,0,0, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,0,
];

/// Dot cursor: 5x5 pixels, hotspot at (2, 2).
/// Small filled white circle for remote-controlled mode.
/// White color ensures visibility on dark backgrounds.
#[rustfmt::skip]
const DOT_CURSOR_RGBA: &[u8] = &[
    // Row 0
    0,0,0,0,         255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,0,
    // Row 1
    255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255,
    // Row 2
    255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255,
    // Row 3
    255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255, 255,255,255,255,
    // Row 4
    0,0,0,0,         255,255,255,255, 255,255,255,255, 255,255,255,255, 0,0,0,0,
];

/// Cursor manager for client-side cursor rendering
///
/// Handles cursor bitmap encoding and Guacamole cursor instruction generation.
/// Supports both standard cursors (pointer, I-beam, etc.) and custom cursor bitmaps.
pub struct CursorManager {
    /// Next stream ID for cursor image data
    next_stream_id: u32,
    /// Whether JPEG encoding is supported by client
    supports_jpeg: bool,
    /// Whether WebP encoding is supported by client
    supports_webp: bool,
    /// JPEG quality (1-100)
    jpeg_quality: u8,
}

impl CursorManager {
    /// Create a new cursor manager
    pub fn new(supports_jpeg: bool, supports_webp: bool, jpeg_quality: u8) -> Self {
        Self {
            next_stream_id: 1000, // Start at 1000 to avoid conflicts with frame stream IDs
            supports_jpeg,
            supports_webp,
            jpeg_quality: jpeg_quality.clamp(1, 100),
        }
    }

    /// Send a custom cursor bitmap to the client
    ///
    /// Encodes the cursor bitmap and sends img/blob/end instructions followed by
    /// a cursor instruction to set the cursor image and hotspot.
    ///
    /// # Arguments
    ///
    /// * `rgba_data` - RGBA pixel data (4 bytes per pixel)
    /// * `width` - Cursor width in pixels
    /// * `height` - Cursor height in pixels
    /// * `hotspot_x` - X coordinate of the cursor hotspot (click point)
    /// * `hotspot_y` - Y coordinate of the cursor hotspot (click point)
    ///
    /// # Returns
    ///
    /// Vector of Guacamole protocol instructions to send to client
    pub fn send_custom_cursor(
        &mut self,
        rgba_data: &[u8],
        width: u32,
        height: u32,
        hotspot_x: i32,
        hotspot_y: i32,
    ) -> Result<Vec<String>, String> {
        // Use buffer layer -1 for cursor (standard Guacamole convention)
        let cursor_layer = -1;
        let stream_id = self.next_stream_id;
        self.next_stream_id += 1;

        // Encode cursor bitmap (prefer WebP > JPEG > PNG)
        let (encoded_data, mimetype) = if self.supports_webp {
            // WebP lossless for cursors (perfect quality, 33% smaller than PNG)
            match Self::encode_webp_lossless(rgba_data, width, height) {
                Ok(data) => (data, "image/webp"),
                Err(e) => {
                    warn!("WebP encoding failed: {}, falling back to PNG", e);
                    (Self::encode_png(rgba_data, width, height)?, "image/png")
                }
            }
        } else if self.supports_jpeg {
            // JPEG for non-transparent cursors (smaller than PNG)
            // Check if cursor has transparency
            let has_transparency = rgba_data.chunks(4).any(|pixel| pixel[3] < 255);
            if has_transparency {
                (Self::encode_png(rgba_data, width, height)?, "image/png")
            } else {
                match Self::encode_jpeg(rgba_data, width, height, self.jpeg_quality) {
                    Ok(data) => (data, "image/jpeg"),
                    Err(e) => {
                        warn!("JPEG encoding failed: {}, falling back to PNG", e);
                        (Self::encode_png(rgba_data, width, height)?, "image/png")
                    }
                }
            }
        } else {
            // PNG fallback (always supported)
            (Self::encode_png(rgba_data, width, height)?, "image/png")
        };

        // Base64 encode
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encoded_data);

        // Build instruction sequence
        let mut instructions = Vec::new();

        // 1. size instruction to set cursor buffer layer dimensions
        // Required by Guacamole protocol: buffer layer must be sized before drawing.
        // Apache guacd sends size before png/img for all cursor updates.
        let size_instr = format_size(cursor_layer, width, height);
        instructions.push(size_instr);

        // 3. img instruction to load cursor into buffer layer
        // Format: img <stream> <mask> <layer> <mimetype> <x> <y>
        // mask=12 (0x0C) = GUAC_COMP_SRC (matches Apache guacd cursor.c)
        let img_instr = format!(
            "3.img,{}.{},2.12,{}.{},{}.{},1.0,1.0;",
            stream_id.to_string().len(),
            stream_id,
            cursor_layer.to_string().len(),
            cursor_layer,
            mimetype.len(),
            mimetype
        );
        instructions.push(img_instr);

        // 4. blob instruction with base64 data
        let blob_instr = format!(
            "4.blob,{}.{},{}.{};",
            stream_id.to_string().len(),
            stream_id,
            base64_data.len(),
            base64_data
        );
        instructions.push(blob_instr);

        // 5. end instruction to close stream
        let end_instr = format!("3.end,{}.{};", stream_id.to_string().len(), stream_id);
        instructions.push(end_instr);

        // 6. cursor instruction to set the cursor
        let cursor_instr = format_cursor(hotspot_x, hotspot_y, cursor_layer, 0, 0, width, height);
        instructions.push(cursor_instr);

        debug!(
            "Cursor: Sent custom cursor {}x{} with hotspot ({}, {}), {} bytes {}",
            width,
            height,
            hotspot_x,
            hotspot_y,
            encoded_data.len(),
            mimetype
        );

        Ok(instructions)
    }

    /// Send a standard cursor type to the client
    ///
    /// Draws the cursor bitmap onto buffer layer -1 and sends a `cursor` instruction
    /// referencing that layer. This is required because the Guacamole `cursor` instruction
    /// expects a numeric layer index -- passing a cursor name string is invalid protocol
    /// and will be silently ignored by the client.
    ///
    /// # Arguments
    ///
    /// * `cursor_type` - Standard cursor type
    ///
    /// # Returns
    ///
    /// Vector of Guacamole protocol instructions (img, blob, end, cursor)
    pub fn send_standard_cursor(
        &mut self,
        cursor_type: StandardCursor,
    ) -> Result<Vec<String>, String> {
        match cursor_type {
            StandardCursor::Pointer => self.send_custom_cursor(POINTER_CURSOR_RGBA, 11, 19, 1, 1),
            StandardCursor::None => self.send_custom_cursor(HIDDEN_CURSOR_RGBA, 1, 1, 0, 0),
            StandardCursor::IBeam => self.send_custom_cursor(IBEAM_CURSOR_RGBA, 7, 16, 3, 8),
            StandardCursor::Dot => self.send_custom_cursor(DOT_CURSOR_RGBA, 5, 5, 2, 2),
        }
    }

    /// Encode RGBA data as PNG
    fn encode_png(rgba_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        use image::{ImageEncoder, RgbaImage};

        let img = RgbaImage::from_raw(width, height, rgba_data.to_vec())
            .ok_or_else(|| "Invalid image dimensions".to_string())?;

        let mut png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
        encoder
            .write_image(&img, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("PNG encode failed: {}", e))?;

        Ok(png_data)
    }

    /// Encode RGBA data as JPEG (for non-transparent cursors)
    fn encode_jpeg(
        rgba_data: &[u8],
        width: u32,
        height: u32,
        quality: u8,
    ) -> Result<Vec<u8>, String> {
        use image::{ImageEncoder, RgbaImage};

        let img = RgbaImage::from_raw(width, height, rgba_data.to_vec())
            .ok_or_else(|| "Invalid image dimensions".to_string())?;

        let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

        let mut jpeg_data = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder
            .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e| format!("JPEG encode failed: {}", e))?;

        Ok(jpeg_data)
    }

    /// Encode RGBA data as WebP (lossless)
    fn encode_webp_lossless(rgba_data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        use webp::{Encoder, WebPMemory};

        let encoder = Encoder::from_rgba(rgba_data, width, height);
        let webp: WebPMemory = encoder.encode_lossless();
        Ok(webp.to_vec())
    }
}

/// Standard cursor types (built-in to Guacamole client)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardCursor {
    /// Standard pointer arrow
    Pointer,
    /// Text I-beam cursor
    IBeam,
    /// No cursor (hidden)
    None,
    /// Small dot cursor (for remote-controlled mode)
    Dot,
}

/// Helper to send cursor instructions via a channel
///
/// This is a convenience function for handlers that use mpsc channels.
pub async fn send_cursor_instructions(
    instructions: Vec<String>,
    to_client: &tokio::sync::mpsc::Sender<Bytes>,
) -> Result<(), String> {
    for instr in instructions {
        to_client
            .send(Bytes::from(instr))
            .await
            .map_err(|e| format!("Failed to send cursor instruction: {}", e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_manager_new() {
        let mgr = CursorManager::new(true, true, 85);
        assert_eq!(mgr.jpeg_quality, 85);
        assert!(mgr.supports_jpeg);
        assert!(mgr.supports_webp);
    }

    #[test]
    fn test_send_standard_cursor_pointer() {
        let mut mgr = CursorManager::new(false, false, 85);
        let result = mgr.send_standard_cursor(StandardCursor::Pointer);
        assert!(result.is_ok());
        let instructions = result.unwrap();
        assert_eq!(instructions.len(), 5); // size, img, blob, end, cursor
        assert!(instructions[0].starts_with("4.size"));
        assert!(instructions[1].starts_with("3.img"));
        assert!(instructions[2].starts_with("4.blob"));
        assert!(instructions[3].starts_with("3.end"));
        assert!(instructions[4].starts_with("6.cursor"));
        // cursor instruction must reference layer -1 (numeric), not a name string
        assert!(instructions[4].contains("-1"));
    }

    #[test]
    fn test_send_standard_cursor_none() {
        let mut mgr = CursorManager::new(false, false, 85);
        let result = mgr.send_standard_cursor(StandardCursor::None);
        assert!(result.is_ok());
        let instructions = result.unwrap();
        assert_eq!(instructions.len(), 5); // size, img, blob, end, cursor
                                           // Hidden cursor: 1x1 at hotspot (0,0)
        assert!(instructions[4].starts_with("6.cursor"));
    }

    #[test]
    fn test_send_standard_cursor_ibeam() {
        let mut mgr = CursorManager::new(false, false, 85);
        let result = mgr.send_standard_cursor(StandardCursor::IBeam);
        assert!(result.is_ok());
        let instructions = result.unwrap();
        assert_eq!(instructions.len(), 5); // size, img, blob, end, cursor
    }

    #[test]
    fn test_send_standard_cursor_dot() {
        let mut mgr = CursorManager::new(false, false, 85);
        let result = mgr.send_standard_cursor(StandardCursor::Dot);
        assert!(result.is_ok());
        let instructions = result.unwrap();
        assert_eq!(instructions.len(), 5); // size, img, blob, end, cursor
    }

    #[test]
    fn test_send_custom_cursor() {
        let mut mgr = CursorManager::new(false, false, 85);

        // Create a simple 2x2 red cursor
        let rgba_data = vec![
            255, 0, 0, 255, // Red pixel
            255, 0, 0, 255, // Red pixel
            255, 0, 0, 255, // Red pixel
            255, 0, 0, 255, // Red pixel
        ];

        let result = mgr.send_custom_cursor(&rgba_data, 2, 2, 1, 1);
        assert!(result.is_ok());

        let instructions = result.unwrap();
        assert_eq!(instructions.len(), 5); // size, img, blob, end, cursor
        assert!(instructions[0].starts_with("4.size"));
        assert!(instructions[1].starts_with("3.img"));
        assert!(instructions[2].starts_with("4.blob"));
        assert!(instructions[3].starts_with("3.end"));
        assert!(instructions[4].starts_with("6.cursor"));
    }
}
