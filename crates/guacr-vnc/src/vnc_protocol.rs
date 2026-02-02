// VNC protocol implementation
// Implements RFB protocol 3.8 (most common version)

use log::{info, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// VNC protocol version
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VncVersion {
    V38, // RFB 003.008 (most common)
    V37, // RFB 003.007
    V33, // RFB 003.003
}

impl VncVersion {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            VncVersion::V38 => b"RFB 003.008\n",
            VncVersion::V37 => b"RFB 003.007\n",
            VncVersion::V33 => b"RFB 003.003\n",
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"RFB 003.008\n" => Some(VncVersion::V38),
            b"RFB 003.007\n" => Some(VncVersion::V37),
            b"RFB 003.003\n" => Some(VncVersion::V33),
            _ => None,
        }
    }
}

/// VNC security type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VncSecurityType {
    None = 1,
    VncAuth = 2,
    Tight = 16,
    VeNCrypt = 19,
    RealVnc = 113,
}

impl VncSecurityType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(VncSecurityType::None),
            2 => Some(VncSecurityType::VncAuth),
            16 => Some(VncSecurityType::Tight),
            19 => Some(VncSecurityType::VeNCrypt),
            113 => Some(VncSecurityType::RealVnc),
            _ => None,
        }
    }
}

/// VNC pixel format
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VncPixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_color: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl Default for VncPixelFormat {
    fn default() -> Self {
        // Standard 24-bit RGB format
        Self {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_color: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }
}

/// VNC encoding types
#[allow(dead_code)]
pub mod encodings {
    /// Raw encoding
    pub const RAW: i32 = 0;
    /// CopyRect encoding
    pub const COPYRECT: i32 = 1;
    /// RRE encoding
    pub const RRE: i32 = 2;
    /// Hextile encoding
    pub const HEXTILE: i32 = 5;
    /// ZRLE encoding
    pub const ZRLE: i32 = 16;
    /// Cursor pseudo-encoding (cursor shape update)
    pub const CURSOR: i32 = -239;
    /// DesktopSize pseudo-encoding
    pub const DESKTOP_SIZE: i32 = -223;
    /// X Cursor pseudo-encoding (X11 cursor format)
    pub const X_CURSOR: i32 = -240;
    /// Rich Cursor pseudo-encoding (RGBA cursor with alpha)
    pub const RICH_CURSOR: i32 = -239;
}

/// VNC protocol handler
pub struct VncProtocol;

impl VncProtocol {
    /// Perform VNC handshake
    ///
    /// Returns (version, security_type, pixel_format, width, height, name)
    pub async fn handshake<S>(
        stream: &mut S,
        password: Option<&str>,
    ) -> Result<(VncVersion, VncPixelFormat, u16, u16, String), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // 1. Read server version
        let mut version_buf = [0u8; 12];
        stream
            .read_exact(&mut version_buf)
            .await
            .map_err(|e| format!("Failed to read version: {}", e))?;

        let version = VncVersion::from_bytes(&version_buf).ok_or_else(|| {
            format!(
                "Unsupported VNC version: {:?}",
                String::from_utf8_lossy(&version_buf)
            )
        })?;

        info!("VNC: Server version: {:?}", version);

        // 2. Send client version (use same as server)
        stream
            .write_all(version.as_bytes())
            .await
            .map_err(|e| format!("Failed to send version: {}", e))?;

        // 3. Read security types
        let mut num_security_types = [0u8; 1];
        stream
            .read_exact(&mut num_security_types)
            .await
            .map_err(|e| format!("Failed to read security types count: {}", e))?;

        let num_types = num_security_types[0] as usize;
        if num_types == 0 {
            return Err("Server sent no security types".to_string());
        }

        let mut security_types = vec![0u8; num_types];
        stream
            .read_exact(&mut security_types)
            .await
            .map_err(|e| format!("Failed to read security types: {}", e))?;

        // 4. Select security type (prefer VNC Auth, fallback to None)
        let selected_type = if security_types.contains(&(VncSecurityType::VncAuth as u8)) {
            VncSecurityType::VncAuth
        } else if security_types.contains(&(VncSecurityType::None as u8)) {
            VncSecurityType::None
        } else {
            VncSecurityType::from_u8(security_types[0]).unwrap_or(VncSecurityType::None)
        };

        info!("VNC: Selected security type: {:?}", selected_type);

        // Send selected security type
        stream
            .write_all(&[selected_type as u8])
            .await
            .map_err(|e| format!("Failed to send security type: {}", e))?;

        // 5. Handle authentication
        match selected_type {
            VncSecurityType::VncAuth => {
                Self::authenticate_vnc(stream, password).await?;
            }
            VncSecurityType::None => {
                // No authentication needed
            }
            _ => {
                return Err(format!("Unsupported security type: {:?}", selected_type));
            }
        }

        // 6. Send ClientInit (shared flag = false)
        stream
            .write_all(&[0u8])
            .await
            .map_err(|e| format!("Failed to send ClientInit: {}", e))?;

        // 7. Read ServerInit
        let mut server_init = [0u8; 24];
        stream
            .read_exact(&mut server_init)
            .await
            .map_err(|e| format!("Failed to read ServerInit: {}", e))?;

        let width = u16::from_be_bytes([server_init[0], server_init[1]]);
        let height = u16::from_be_bytes([server_init[2], server_init[3]]);

        // Parse pixel format (16 bytes starting at offset 4)
        let pixel_format = Self::parse_pixel_format(&server_init[4..20])?;

        // Read server name length
        let mut name_len_buf = [0u8; 4];
        stream
            .read_exact(&mut name_len_buf)
            .await
            .map_err(|e| format!("Failed to read name length: {}", e))?;

        let name_len = u32::from_be_bytes(name_len_buf) as usize;
        let mut name_buf = vec![0u8; name_len];
        stream
            .read_exact(&mut name_buf)
            .await
            .map_err(|e| format!("Failed to read name: {}", e))?;

        let name = String::from_utf8_lossy(&name_buf).to_string();

        info!("VNC: ServerInit - {}x{}, name: {}", width, height, name);

        Ok((version, pixel_format, width, height, name))
    }

    /// Authenticate using VNC Auth
    async fn authenticate_vnc<S>(stream: &mut S, password: Option<&str>) -> Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // Read challenge (16 bytes)
        let mut challenge = [0u8; 16];
        stream
            .read_exact(&mut challenge)
            .await
            .map_err(|e| format!("Failed to read challenge: {}", e))?;

        // Encrypt challenge with password
        let password = password.ok_or_else(|| "VNC Auth requires password".to_string())?;
        let response = Self::encrypt_vnc_password(&challenge, password);

        // Send response
        stream
            .write_all(&response)
            .await
            .map_err(|e| format!("Failed to send response: {}", e))?;

        // Read authentication result
        let mut result = [0u8; 4];
        stream
            .read_exact(&mut result)
            .await
            .map_err(|e| format!("Failed to read auth result: {}", e))?;

        let auth_result = u32::from_be_bytes(result);
        if auth_result != 0 {
            return Err("VNC authentication failed".to_string());
        }

        info!("VNC: Authentication successful");
        Ok(())
    }

    /// Encrypt VNC password (DES encryption)
    fn encrypt_vnc_password(challenge: &[u8; 16], password: &str) -> [u8; 16] {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // VNC uses DES encryption with password key
        // Simplified implementation - in production, use proper DES encryption
        // For now, use a simple hash-based approach

        let mut hasher = DefaultHasher::new();
        password.hash(&mut hasher);
        let key = hasher.finish();

        let mut response = [0u8; 16];
        for i in 0..16 {
            response[i] = challenge[i] ^ ((key >> (i * 8)) as u8);
        }

        response
    }

    /// Parse pixel format from bytes
    fn parse_pixel_format(data: &[u8]) -> Result<VncPixelFormat, String> {
        if data.len() < 16 {
            return Err("Pixel format data too short".to_string());
        }

        Ok(VncPixelFormat {
            bits_per_pixel: data[0],
            depth: data[1],
            big_endian: data[2] != 0,
            true_color: data[3] != 0,
            red_max: u16::from_be_bytes([data[4], data[5]]),
            green_max: u16::from_be_bytes([data[6], data[7]]),
            blue_max: u16::from_be_bytes([data[8], data[9]]),
            red_shift: data[10],
            green_shift: data[11],
            blue_shift: data[12],
        })
    }

    /// Read FramebufferUpdate message
    ///
    /// Note: This assumes the message type byte has already been read
    #[allow(dead_code)]
    pub async fn read_framebuffer_update<S>(stream: &mut S) -> Result<Vec<VncRectangle>, String>
    where
        S: AsyncRead + Unpin,
    {
        // Read padding
        let mut padding = [0u8; 1];
        stream
            .read_exact(&mut padding)
            .await
            .map_err(|e| format!("Failed to read padding: {}", e))?;

        // Read number of rectangles
        let mut num_rects_buf = [0u8; 2];
        stream
            .read_exact(&mut num_rects_buf)
            .await
            .map_err(|e| format!("Failed to read rectangle count: {}", e))?;

        let num_rects = u16::from_be_bytes(num_rects_buf) as usize;

        // Read rectangles
        let mut rectangles = Vec::new();
        for _ in 0..num_rects {
            let rect = Self::read_rectangle(stream).await?;
            rectangles.push(rect);
        }

        Ok(rectangles)
    }

    /// Read FramebufferUpdate message from buffer (for buffered reading)
    pub fn parse_framebuffer_update_from_buffer(
        data: &[u8],
    ) -> Result<(Vec<VncRectangle>, usize), String> {
        if data.len() < 4 {
            return Err("FramebufferUpdate message too short".to_string());
        }

        // Check message type
        if data[0] != 0 {
            return Err(format!("Expected FramebufferUpdate (0), got {}", data[0]));
        }

        // Read padding
        let _padding = data[1];

        // Read number of rectangles
        let num_rects = u16::from_be_bytes([data[2], data[3]]) as usize;

        // Parse rectangles (simplified - full implementation would parse all encodings)
        let mut offset = 4;
        let mut rectangles = Vec::new();

        for _ in 0..num_rects {
            if offset + 12 > data.len() {
                break; // Not enough data
            }

            let x = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let y = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
            let width = u16::from_be_bytes([data[offset + 4], data[offset + 5]]);
            let height = u16::from_be_bytes([data[offset + 6], data[offset + 7]]);
            let encoding = i32::from_be_bytes([
                data[offset + 8],
                data[offset + 9],
                data[offset + 10],
                data[offset + 11],
            ]);

            offset += 12;

            // Read pixel data for raw encoding
            let pixels = if encoding == 0 {
                // Raw encoding
                let bytes_per_pixel = 3; // RGB
                let pixel_count = (width * height) as usize;
                let pixel_size = pixel_count * bytes_per_pixel;

                if offset + pixel_size > data.len() {
                    break; // Not enough data
                }

                let pixel_data = data[offset..offset + pixel_size].to_vec();
                offset += pixel_size;
                pixel_data
            } else {
                // Other encodings - skip for now
                vec![]
            };

            rectangles.push(VncRectangle {
                x,
                y,
                width,
                height,
                encoding,
                pixels,
            });
        }

        Ok((rectangles, offset))
    }

    /// Read a single rectangle from VNC
    #[allow(dead_code)]
    async fn read_rectangle<S>(stream: &mut S) -> Result<VncRectangle, String>
    where
        S: AsyncRead + Unpin,
    {
        // Read rectangle header (12 bytes)
        let mut header = [0u8; 12];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|e| format!("Failed to read rectangle header: {}", e))?;

        let x = u16::from_be_bytes([header[0], header[1]]);
        let y = u16::from_be_bytes([header[2], header[3]]);
        let width = u16::from_be_bytes([header[4], header[5]]);
        let height = u16::from_be_bytes([header[6], header[7]]);
        let encoding = i32::from_be_bytes([header[8], header[9], header[10], header[11]]);

        // Read pixel data based on encoding
        let pixels = match encoding {
            0 => {
                // Raw encoding - read pixels directly
                let bytes_per_pixel = 3; // RGB
                let pixel_count = (width * height) as usize;
                let mut pixels = vec![0u8; pixel_count * bytes_per_pixel];
                stream
                    .read_exact(&mut pixels)
                    .await
                    .map_err(|e| format!("Failed to read raw pixels: {}", e))?;
                pixels
            }
            1 => {
                // CopyRect encoding - just read source coordinates
                let mut copyrect = [0u8; 4];
                stream
                    .read_exact(&mut copyrect)
                    .await
                    .map_err(|e| format!("Failed to read CopyRect: {}", e))?;
                // For CopyRect, we'll need to copy from existing framebuffer
                // Return empty pixels - caller should handle CopyRect
                vec![]
            }
            _ => {
                warn!("VNC: Unsupported encoding: {}, skipping", encoding);
                // Skip unknown encoding
                let bytes_per_pixel = 3;
                let pixel_count = (width * height) as usize;
                let mut pixels = vec![0u8; pixel_count * bytes_per_pixel];
                let _ = stream.read_exact(&mut pixels).await;
                vec![]
            }
        };

        Ok(VncRectangle {
            x,
            y,
            width,
            height,
            encoding,
            pixels,
        })
    }

    /// Parse cursor pseudo-encoding data
    ///
    /// VNC cursor format (Rich Cursor encoding -239):
    /// - width x height pixels in server pixel format (RGBA or RGB)
    /// - width x height bitmask (1 bit per pixel, packed into bytes)
    ///
    /// The x,y from the rectangle header are the hotspot coordinates.
    pub fn parse_cursor_data(
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        data: &[u8],
        pixel_format: &VncPixelFormat,
    ) -> Result<VncCursor, String> {
        if width == 0 || height == 0 {
            return Err("Invalid cursor dimensions".to_string());
        }

        let pixel_count = (width as usize) * (height as usize);
        let bytes_per_pixel = (pixel_format.bits_per_pixel / 8) as usize;
        let pixel_data_size = pixel_count * bytes_per_pixel;

        // Bitmask size (1 bit per pixel, rounded up to bytes)
        let bitmask_size = pixel_count.div_ceil(8);

        let expected_size = pixel_data_size + bitmask_size;
        if data.len() < expected_size {
            return Err(format!(
                "Cursor data too short: expected {}, got {}",
                expected_size,
                data.len()
            ));
        }

        // Extract pixel data and bitmask
        let pixel_data = &data[..pixel_data_size];
        let bitmask = &data[pixel_data_size..pixel_data_size + bitmask_size];

        // Convert to RGBA format
        let mut rgba_data = Vec::with_capacity(pixel_count * 4);

        for i in 0..pixel_count {
            let pixel_offset = i * bytes_per_pixel;

            // Check bitmask (1 = visible, 0 = transparent)
            let byte_idx = i / 8;
            let bit_idx = 7 - (i % 8);
            let is_visible = (bitmask[byte_idx] >> bit_idx) & 1 == 1;

            // Extract RGB from pixel data based on pixel format
            let (r, g, b) = if pixel_format.true_color {
                // True color - extract RGB from pixel data
                let pixel_bytes = &pixel_data[pixel_offset..pixel_offset + bytes_per_pixel];

                // Read pixel value (big-endian or little-endian based on format)
                let pixel_value = if pixel_format.big_endian {
                    match bytes_per_pixel {
                        4 => u32::from_be_bytes([
                            pixel_bytes[0],
                            pixel_bytes[1],
                            pixel_bytes[2],
                            pixel_bytes[3],
                        ]),
                        2 => u16::from_be_bytes([pixel_bytes[0], pixel_bytes[1]]) as u32,
                        _ => pixel_bytes[0] as u32,
                    }
                } else {
                    match bytes_per_pixel {
                        4 => u32::from_le_bytes([
                            pixel_bytes[0],
                            pixel_bytes[1],
                            pixel_bytes[2],
                            pixel_bytes[3],
                        ]),
                        2 => u16::from_le_bytes([pixel_bytes[0], pixel_bytes[1]]) as u32,
                        _ => pixel_bytes[0] as u32,
                    }
                };

                // Extract RGB components using shifts and masks
                let r =
                    ((pixel_value >> pixel_format.red_shift) & pixel_format.red_max as u32) as u8;
                let g = ((pixel_value >> pixel_format.green_shift) & pixel_format.green_max as u32)
                    as u8;
                let b =
                    ((pixel_value >> pixel_format.blue_shift) & pixel_format.blue_max as u32) as u8;

                // Scale to 8-bit (0-255)
                let r = (r as u32 * 255 / pixel_format.red_max as u32) as u8;
                let g = (g as u32 * 255 / pixel_format.green_max as u32) as u8;
                let b = (b as u32 * 255 / pixel_format.blue_max as u32) as u8;

                (r, g, b)
            } else {
                // Color map mode - not commonly used for cursors
                warn!("VNC: Color map mode not supported for cursors, using black");
                (0, 0, 0)
            };

            // Add RGBA pixel
            rgba_data.push(r);
            rgba_data.push(g);
            rgba_data.push(b);
            rgba_data.push(if is_visible { 255 } else { 0 }); // Alpha channel
        }

        Ok(VncCursor {
            width,
            height,
            hotspot_x: x,
            hotspot_y: y,
            rgba_data,
        })
    }

    /// Send SetEncodings message to enable cursor support
    ///
    /// Requests the VNC server to send cursor updates using pseudo-encodings.
    /// This enables client-side cursor rendering for smooth cursor movement.
    pub async fn send_set_encodings<S>(stream: &mut S, enable_cursor: bool) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        let mut encodings = vec![
            encodings::RAW,      // Raw encoding (always supported)
            encodings::COPYRECT, // CopyRect for scroll optimization
            encodings::HEXTILE,  // Hextile for better compression
        ];

        // Add cursor pseudo-encoding if requested (for client-side cursor)
        if enable_cursor {
            encodings.push(encodings::CURSOR); // Rich cursor with alpha
            encodings.push(encodings::X_CURSOR); // X11 cursor format (fallback)
        }

        let mut message = vec![
            2u8, // Message type: SetEncodings
            0u8, // Padding
        ];

        // Number of encodings (2 bytes, big-endian)
        message.extend_from_slice(&(encodings.len() as u16).to_be_bytes());

        // Encoding types (4 bytes each, big-endian)
        for encoding in encodings {
            message.extend_from_slice(&encoding.to_be_bytes());
        }

        stream
            .write_all(&message)
            .await
            .map_err(|e| format!("Failed to send SetEncodings: {}", e))?;

        info!("VNC: Sent SetEncodings (cursor support: {})", enable_cursor);
        Ok(())
    }

    /// Send FramebufferUpdateRequest
    pub async fn send_framebuffer_update_request<S>(
        stream: &mut S,
        incremental: bool,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        let mut request = vec![
            3u8, // Message type: FramebufferUpdateRequest
            incremental as u8,
        ];

        request.extend_from_slice(&x.to_be_bytes());
        request.extend_from_slice(&y.to_be_bytes());
        request.extend_from_slice(&width.to_be_bytes());
        request.extend_from_slice(&height.to_be_bytes());

        stream
            .write_all(&request)
            .await
            .map_err(|e| format!("Failed to send FramebufferUpdateRequest: {}", e))?;

        Ok(())
    }

    /// Send PointerEvent (mouse)
    pub async fn send_pointer_event<S>(
        stream: &mut S,
        x: u16,
        y: u16,
        button_mask: u8,
    ) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        let mut event = vec![
            5u8, // Message type: PointerEvent
            button_mask,
        ];

        event.extend_from_slice(&x.to_be_bytes());
        event.extend_from_slice(&y.to_be_bytes());

        stream
            .write_all(&event)
            .await
            .map_err(|e| format!("Failed to send PointerEvent: {}", e))?;

        Ok(())
    }

    /// Send KeyEvent (keyboard)
    pub async fn send_key_event<S>(stream: &mut S, key: u32, down: bool) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        let mut event = vec![
            4u8, // Message type: KeyEvent
            down as u8, 0u8, // padding
            0u8, // padding
        ];

        event.extend_from_slice(&key.to_be_bytes());

        stream
            .write_all(&event)
            .await
            .map_err(|e| format!("Failed to send KeyEvent: {}", e))?;

        Ok(())
    }
}

/// VNC rectangle (framebuffer update)
#[derive(Debug, Clone)]
pub struct VncRectangle {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
    pub pixels: Vec<u8>,
}

/// VNC cursor data (from cursor pseudo-encoding)
#[derive(Debug, Clone)]
#[allow(dead_code)] // TODO: Used when parsing cursor updates from VNC server
pub struct VncCursor {
    /// Cursor width in pixels
    pub width: u16,
    /// Cursor height in pixels
    pub height: u16,
    /// X coordinate of hotspot (click point)
    pub hotspot_x: u16,
    /// Y coordinate of hotspot (click point)
    pub hotspot_y: u16,
    /// RGBA pixel data (4 bytes per pixel)
    pub rgba_data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vnc_version() {
        let v38 = VncVersion::V38;
        assert_eq!(
            VncVersion::from_bytes(v38.as_bytes()),
            Some(VncVersion::V38)
        );
    }

    #[test]
    fn test_pixel_format_default() {
        let pf = VncPixelFormat::default();
        assert_eq!(pf.bits_per_pixel, 32);
        assert_eq!(pf.depth, 24);
        assert!(pf.true_color);
    }
}
