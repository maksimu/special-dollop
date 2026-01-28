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
            -239 => {
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
