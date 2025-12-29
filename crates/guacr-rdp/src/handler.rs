//! RDP protocol handler
//!
//! Implements the ProtocolHandler trait for RDP connections using FreeRDP.

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use guacr_protocol::GuacamoleParser;
use image::{ImageEncoder, RgbaImage};
use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;

#[allow(unused_imports)]
use crate::error::{RdpError, Result};

use crate::client::FreeRdpClient;
use crate::settings::RdpSettings;
use crate::CLIPBOARD_DEFAULT_SIZE;

/// RDP protocol handler configuration
#[derive(Debug, Clone)]
pub struct RdpConfig {
    /// Default RDP port
    pub default_port: u16,
    /// Default display width
    pub default_width: u32,
    /// Default display height
    pub default_height: u32,
    /// Default security mode
    pub security_mode: String,
    /// Clipboard buffer size
    pub clipboard_buffer_size: usize,
    /// Disable clipboard copy from server
    pub disable_copy: bool,
    /// Disable clipboard paste to server
    pub disable_paste: bool,
    /// Frame rate limit (FPS)
    pub frame_rate: u32,
    /// JPEG quality for image encoding (1-100)
    pub jpeg_quality: u8,
}

impl Default for RdpConfig {
    fn default() -> Self {
        Self {
            default_port: 3389,
            default_width: 1920,
            default_height: 1080,
            security_mode: "nla".to_string(),
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,
            disable_copy: false,
            disable_paste: false,
            frame_rate: 30,
            jpeg_quality: 85,
        }
    }
}

/// RDP protocol handler
///
/// Connects to RDP servers using FreeRDP and provides remote desktop
/// access via the Guacamole protocol.
#[derive(Clone)]
pub struct RdpHandler {
    config: RdpConfig,
}

impl RdpHandler {
    /// Create a new RDP handler with the given configuration
    pub fn new(config: RdpConfig) -> Self {
        Self { config }
    }

    /// Create a new RDP handler with default configuration
    pub fn with_defaults() -> Self {
        Self::new(RdpConfig::default())
    }

    /// Encode framebuffer region as JPEG
    fn encode_jpeg(
        data: &[u8],
        width: u32,
        height: u32,
        quality: u8,
    ) -> std::result::Result<Vec<u8>, HandlerError> {
        let img = RgbaImage::from_raw(width, height, data.to_vec())
            .ok_or_else(|| HandlerError::ProtocolError("Invalid image dimensions".to_string()))?;

        let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

        let mut jpeg_data = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder
            .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e| HandlerError::ProtocolError(format!("JPEG encode failed: {}", e)))?;

        Ok(jpeg_data)
    }

    /// Quick hash of framebuffer data to detect changes
    ///
    /// Uses sampling to avoid hashing entire buffer while still detecting
    /// most changes. Much faster than full byte comparison.
    fn hash_framebuffer(data: &[u8]) -> u64 {
        // Sample every Nth byte to create a quick hash
        // For a 1920x1080 frame (8MB), sampling every 1024th byte gives 8K samples
        const SAMPLE_STRIDE: usize = 1024;

        let mut hash: u64 = 0;
        let mut i = 0;
        while i < data.len() {
            hash = hash.wrapping_mul(31).wrapping_add(data[i] as u64);
            i += SAMPLE_STRIDE;
        }

        // Also include length to catch size changes
        hash = hash.wrapping_mul(31).wrapping_add(data.len() as u64);
        hash
    }

    /// Format a Guacamole image instruction
    fn format_img_instruction(
        stream_id: u32,
        layer: i32,
        mimetype: &str,
        x: u32,
        y: u32,
    ) -> String {
        let stream_str = stream_id.to_string();
        let layer_str = layer.to_string();
        let x_str = x.to_string();
        let y_str = y.to_string();

        format!(
            "3.img,{}.{},{}.{},{}.{},{}.{},{}.{};",
            stream_str.len(),
            stream_str,
            layer_str.len(),
            layer_str,
            mimetype.len(),
            mimetype,
            x_str.len(),
            x_str,
            y_str.len(),
            y_str
        )
    }

    /// Format a Guacamole blob instruction (with chunking)
    fn format_blob_instructions(stream_id: u32, data: &[u8]) -> Vec<String> {
        const BLOB_MAX_LENGTH: usize = 6048;
        let stream_str = stream_id.to_string();
        let base64_data = base64::engine::general_purpose::STANDARD.encode(data);

        base64_data
            .as_bytes()
            .chunks(BLOB_MAX_LENGTH)
            .map(|chunk| {
                let chunk_str = String::from_utf8_lossy(chunk);
                format!(
                    "4.blob,{}.{},{}.{};",
                    stream_str.len(),
                    stream_str,
                    chunk_str.len(),
                    chunk_str
                )
            })
            .collect()
    }

    /// Format a Guacamole end instruction
    fn format_end_instruction(stream_id: u32) -> String {
        let stream_str = stream_id.to_string();
        format!("3.end,{}.{};", stream_str.len(), stream_str)
    }
}

#[async_trait]
impl ProtocolHandler for RdpHandler {
    fn name(&self) -> &str {
        "rdp"
    }

    async fn health_check(&self) -> std::result::Result<HealthStatus, HandlerError> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> std::result::Result<HandlerStats, HandlerError> {
        Ok(HandlerStats::default())
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> std::result::Result<(), HandlerError> {
        // Parse settings
        let settings = RdpSettings::from_params(&params)
            .map_err(|e| HandlerError::ConnectionFailed(e.to_string()))?;

        info!(
            "RDP: Connecting to {}:{} as {}",
            settings.hostname, settings.port, settings.username
        );

        // Create FreeRDP client
        let mut client = FreeRdpClient::new(settings)
            .map_err(|e| HandlerError::ConnectionFailed(e.to_string()))?;

        // Connect (blocking - would need to be run in spawn_blocking for production)
        client
            .connect()
            .map_err(|e| HandlerError::ConnectionFailed(e.to_string()))?;

        let (width, height) = client.dimensions();
        info!("RDP: Connected, framebuffer {}x{}", width, height);

        // Send ready instruction
        let ready_instr = guacr_protocol::format_instruction("ready", &["rdp"]);
        to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // Send size instruction
        let width_str = width.to_string();
        let height_str = height.to_string();
        let size_instr =
            guacr_protocol::format_instruction("size", &["0", &width_str, &height_str]);
        to_client
            .send(Bytes::from(size_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // Frame interval (e.g., 33ms for 30fps)
        let frame_interval = Duration::from_millis(1000 / self.config.frame_rate as u64);
        let mut frame_timer = interval(frame_interval);
        frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut stream_id: u32 = 0;
        // Use a simple hash to detect framebuffer changes instead of storing full copy
        let mut last_frame_hash: u64 = 0;

        // Main event loop
        loop {
            tokio::select! {
                // Frame timer tick - check for display updates
                _ = frame_timer.tick() => {
                    if !client.is_connected() {
                        info!("RDP: Disconnected by server");
                        break;
                    }

                    // Process FreeRDP events (in production, this should be non-blocking)
                    match client.check_events() {
                        Ok(true) => {
                            // Events processed, check for framebuffer updates
                            if let Some(framebuffer) = client.get_framebuffer() {
                                // Quick hash to detect changes (much faster than full comparison)
                                let frame_hash = Self::hash_framebuffer(&framebuffer);
                                let should_send = frame_hash != last_frame_hash;

                                if should_send {
                                    // Encode as JPEG
                                    let (w, h) = client.dimensions();
                                    match Self::encode_jpeg(&framebuffer, w, h, self.config.jpeg_quality) {
                                        Ok(jpeg_data) => {
                                            // Send image instruction
                                            let img_instr = Self::format_img_instruction(
                                                stream_id, 0, "image/jpeg", 0, 0
                                            );
                                            if to_client.send(Bytes::from(img_instr)).await.is_err() {
                                                // Release buffer back to pool before breaking
                                                client.release_framebuffer(framebuffer);
                                                break;
                                            }

                                            // Send blob instructions (chunked)
                                            for blob_instr in Self::format_blob_instructions(stream_id, &jpeg_data) {
                                                if to_client.send(Bytes::from(blob_instr)).await.is_err() {
                                                    break;
                                                }
                                            }

                                            // Send end instruction
                                            let end_instr = Self::format_end_instruction(stream_id);
                                            if to_client.send(Bytes::from(end_instr)).await.is_err() {
                                                // Release buffer back to pool before breaking
                                                client.release_framebuffer(framebuffer);
                                                break;
                                            }

                                            stream_id = stream_id.wrapping_add(1);
                                            last_frame_hash = frame_hash;
                                        }
                                        Err(e) => {
                                            warn!("RDP: Failed to encode frame: {}", e);
                                        }
                                    }
                                }

                                // Release buffer back to pool for reuse
                                client.release_framebuffer(framebuffer);
                            }
                        }
                        Ok(false) => {
                            // Disconnected
                            break;
                        }
                        Err(e) => {
                            error!("RDP: Event processing error: {}", e);
                            break;
                        }
                    }
                }

                // Client message received
                Some(msg) = from_client.recv() => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    trace!("RDP: Received from client: {}", msg_str);

                    // Parse Guacamole instructions
                    if let Ok(instr) = GuacamoleParser::parse_instruction_str(&msg_str) {
                        let args: Vec<String> = instr.args.iter().map(|s| s.to_string()).collect();
                        match instr.opcode {
                            "key" => {
                                // Key instruction: keysym, pressed
                                if args.len() >= 2 {
                                    if let (Ok(keysym), Ok(pressed)) = (
                                        args[0].parse::<u32>(),
                                        args[1].parse::<u32>(),
                                    ) {
                                        // Convert keysym to scancode (simplified)
                                        let scancode = (keysym & 0xFF) as u16;
                                        let _ = client.send_key(scancode, pressed != 0);
                                    }
                                }
                            }
                            "mouse" => {
                                // Mouse instruction: x, y, button_mask
                                if args.len() >= 3 {
                                    if let (Ok(x), Ok(y), Ok(buttons)) = (
                                        args[0].parse::<u16>(),
                                        args[1].parse::<u16>(),
                                        args[2].parse::<u16>(),
                                    ) {
                                        let _ = client.send_mouse(x, y, buttons);
                                    }
                                }
                            }
                            "size" => {
                                // Resize request - would need reconnect for some servers
                                if args.len() >= 2 {
                                    debug!(
                                        "RDP: Resize requested to {}x{}",
                                        args.first().map(|s| s.as_str()).unwrap_or("?"),
                                        args.get(1).map(|s| s.as_str()).unwrap_or("?")
                                    );
                                    // TODO: Implement resize
                                }
                            }
                            "clipboard" => {
                                // Clipboard instruction - data comes in blob
                                debug!("RDP: Clipboard stream opened");
                            }
                            "blob" => {
                                // Clipboard data
                                if args.len() >= 2 {
                                    if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(&args[1]) {
                                        let text = String::from_utf8_lossy(&data).to_string();
                                        let _ = client.clipboard_mut().set_client_clipboard(text);
                                    }
                                }
                            }
                            "disconnect" => {
                                info!("RDP: Client requested disconnect");
                                break;
                            }
                            _ => {
                                trace!("RDP: Unhandled instruction: {}", instr.opcode);
                            }
                        }
                    }
                }

                // Channel closed
                else => {
                    info!("RDP: Client channel closed");
                    break;
                }
            }
        }

        // Cleanup
        client.disconnect();
        info!("RDP: Session ended");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rdp_handler_creation() {
        let handler = RdpHandler::with_defaults();
        assert_eq!(handler.name(), "rdp");
    }

    #[test]
    fn test_rdp_handler_custom_config() {
        let config = RdpConfig {
            default_port: 3390,
            default_width: 2560,
            default_height: 1440,
            security_mode: "tls".to_string(),
            clipboard_buffer_size: 1024 * 1024,
            disable_copy: true,
            disable_paste: false,
            frame_rate: 60,
            jpeg_quality: 90,
        };

        let handler = RdpHandler::new(config.clone());
        assert_eq!(handler.config.default_port, 3390);
        assert_eq!(handler.config.frame_rate, 60);
        assert!(handler.config.disable_copy);
    }

    #[test]
    fn test_rdp_config_defaults() {
        let config = RdpConfig::default();
        assert_eq!(config.default_port, 3389);
        assert_eq!(config.default_width, 1920);
        assert_eq!(config.default_height, 1080);
        assert_eq!(config.security_mode, "nla");
        assert_eq!(config.frame_rate, 30);
        assert_eq!(config.jpeg_quality, 85);
        assert!(!config.disable_copy);
        assert!(!config.disable_paste);
    }

    #[tokio::test]
    async fn test_rdp_handler_health_check() {
        let handler = RdpHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_rdp_handler_stats() {
        let handler = RdpHandler::with_defaults();
        let stats = handler.stats().await.unwrap();
        assert_eq!(stats.active_connections, 0);
    }

    #[test]
    fn test_format_img_instruction() {
        let instr = RdpHandler::format_img_instruction(1, 0, "image/jpeg", 100, 200);
        // Should be: 3.img,1.1,1.0,10.image/jpeg,3.100,3.200;
        assert!(instr.starts_with("3.img,"));
        assert!(instr.contains("image/jpeg"));
        assert!(instr.ends_with(';'));
    }

    #[test]
    fn test_format_blob_instructions_small() {
        let data = b"Hello, World!";
        let blobs = RdpHandler::format_blob_instructions(1, data);

        assert_eq!(blobs.len(), 1);
        assert!(blobs[0].starts_with("4.blob,"));
        assert!(blobs[0].ends_with(';'));
    }

    #[test]
    fn test_format_blob_instructions_chunking() {
        // Create data larger than BLOB_MAX_LENGTH (6048 bytes)
        let data = vec![0u8; 10000];
        let blobs = RdpHandler::format_blob_instructions(1, &data);

        // Should be split into multiple blobs
        assert!(blobs.len() > 1);
        for blob in &blobs {
            assert!(blob.starts_with("4.blob,"));
            assert!(blob.ends_with(';'));
        }
    }

    #[test]
    fn test_format_end_instruction() {
        let instr = RdpHandler::format_end_instruction(42);
        assert_eq!(instr, "3.end,2.42;");
    }

    #[test]
    fn test_encode_jpeg() {
        // Create a small test image (10x10 red pixels)
        let width = 10;
        let height = 10;
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.push(255); // R
            data.push(0); // G
            data.push(0); // B
            data.push(255); // A
        }

        let jpeg = RdpHandler::encode_jpeg(&data, width, height, 80).unwrap();

        // JPEG should start with FFD8 magic bytes
        assert!(jpeg.len() > 2);
        assert_eq!(jpeg[0], 0xFF);
        assert_eq!(jpeg[1], 0xD8);
    }
}
