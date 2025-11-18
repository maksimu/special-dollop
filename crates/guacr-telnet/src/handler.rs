use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use guacr_terminal::{x11_keysym_to_bytes, TerminalEmulator, TerminalRenderer};
use log::{debug, info, warn};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Telnet protocol handler
///
/// Simpler than SSH - just TCP connection with terminal emulation.
///
/// ## Rendering Method
///
/// Uses JPEG images with fontdue-rendered glyphs for actual text display.
/// JPEG is 5-10x faster than PNG with minimal visual loss at quality 95.
/// Alternative drawing instructions (rect/cfill) can't represent bitmap glyphs.
///
/// ## CRITICAL: Character-Aligned Dimensions
///
/// To avoid blurry rendering:
/// 1. Browser sends size request: width_px, height_px
/// 2. Calculate terminal size: cols = width_px / CHAR_WIDTH, rows = height_px / CHAR_HEIGHT
/// 3. **Realign dimensions**: aligned_width = cols * CHAR_WIDTH, aligned_height = rows * CHAR_HEIGHT
/// 4. Send `size` instruction with aligned dimensions
/// 5. Result: No browser scaling = crisp character cells
pub struct TelnetHandler {
    config: TelnetConfig,
}

#[derive(Debug, Clone)]
pub struct TelnetConfig {
    pub default_port: u16,
    pub default_rows: u16,
    pub default_cols: u16,
}

impl Default for TelnetConfig {
    fn default() -> Self {
        Self {
            default_port: 23,
            default_rows: 24,
            default_cols: 80,
        }
    }
}

impl TelnetHandler {
    pub fn new(config: TelnetConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(TelnetConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for TelnetHandler {
    fn name(&self) -> &str {
        "telnet"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("Telnet handler starting connection");

        // Extract connection parameters
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        // Parse size parameter from browser (format: "width,height,dpi" or just dimensions)
        // Telnet uses fixed 19x38 cell dimensions for now (TODO: make dynamic like SSH)
        const CHAR_W: u32 = 19;
        const CHAR_H: u32 = 38;

        let (rows, cols, width_px, height_px) = if let Some(size_str) = params.get("size") {
            // Size comes as comma-separated values
            let parts: Vec<&str> = size_str.split(',').collect();
            if parts.len() >= 2 {
                if let (Ok(w_px), Ok(h_px)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    let calc_cols = (w_px / CHAR_W).max(1) as u16;
                    let calc_rows = (h_px / CHAR_H).max(1) as u16;
                    let aligned_width = calc_cols as u32 * CHAR_W;
                    let aligned_height = calc_rows as u32 * CHAR_H;
                    info!(
                        "Telnet: Browser requested {}x{} px → {}x{} chars → aligned to {}x{} px",
                        w_px, h_px, calc_cols, calc_rows, aligned_width, aligned_height
                    );
                    (calc_rows, calc_cols, aligned_width, aligned_height)
                } else {
                    info!("Telnet: Failed to parse size parameter, using defaults");
                    let w = self.config.default_cols as u32 * CHAR_W;
                    let h = self.config.default_rows as u32 * CHAR_H;
                    (self.config.default_rows, self.config.default_cols, w, h)
                }
            } else {
                info!("Telnet: Size parameter missing dimensions, using defaults");
                let w = self.config.default_cols as u32 * CHAR_W;
                let h = self.config.default_rows as u32 * CHAR_H;
                (self.config.default_rows, self.config.default_cols, w, h)
            }
        } else {
            info!("Telnet: No size parameter provided, using defaults");
            let w = self.config.default_cols as u32 * CHAR_W;
            let h = self.config.default_rows as u32 * CHAR_H;
            (self.config.default_rows, self.config.default_cols, w, h)
        };

        info!("Connecting to {}:{}", hostname, port);

        // Connect via TCP
        let stream = TcpStream::connect((hostname.as_str(), port))
            .await
            .map_err(|e| HandlerError::ConnectionFailed(e.to_string()))?;

        info!("Telnet connection established");

        let (mut read_half, mut write_half) = stream.into_split();

        // Create terminal emulator with browser-requested dimensions
        let mut terminal = TerminalEmulator::new(rows, cols);
        let renderer =
            TerminalRenderer::new().map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Use fixed stream ID for main display (reusing stream replaces content, not stacking)
        let stream_id: u32 = 1;

        // Send ready instruction
        info!("Telnet: Sending ready instruction");
        let ready_instr = TerminalRenderer::format_ready_instruction("telnet-ready");
        to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // CRITICAL: Send size instruction to initialize display dimensions
        // Browser needs this BEFORE any img/drawing operations!
        // Use character-aligned dimensions (not browser's exact request) to prevent scaling
        info!(
            "Telnet: Sending size instruction ({}x{} px = {}x{} chars)",
            width_px, height_px, cols, rows
        );
        let size_instr = TerminalRenderer::format_size_instruction(0, width_px, height_px);
        to_client
            .send(Bytes::from(size_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // Bidirectional forwarding
        let mut buf = vec![0u8; 4096];

        loop {
            tokio::select! {
                // Telnet output -> Terminal -> Client
                result = read_half.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            info!("Telnet connection closed");
                            break;
                        }
                        Ok(n) => {
                            // Process terminal output
                            terminal.process(&buf[..n])
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            if terminal.is_dirty() {
                                // Render screen to JPEG with fontdue-rendered glyphs
                                let jpeg = renderer.render_screen(
                                    terminal.screen(),
                                    terminal.size().0,
                                    terminal.size().1,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                // Send JPEG via img/blob/end sequence
                                // Note: Using character-aligned dimensions prevents browser scaling
                                #[allow(deprecated)]
                                let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);

                                for instr in img_instructions {
                                    to_client.send(Bytes::from(instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                }

                                // Frame boundary marker
                                let sync_instr = renderer.format_sync_instruction(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_millis() as u64
                                );
                                to_client.send(Bytes::from(sync_instr)).await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                terminal.clear_dirty();
                            }
                        }
                        Err(e) => {
                            warn!("Telnet read error: {}", e);
                            break;
                        }
                    }
                }

                // Client input -> Telnet
                Some(msg) = from_client.recv() => {
                    // Parse Guacamole instruction (simplified for now)
                    let msg_str = String::from_utf8_lossy(&msg);

                    if msg_str.contains(".key,") {
                        // Parse and send key
                        if let Some(args_part) = msg_str.split_once(".key,") {
                            if let Some((first_arg, rest)) = args_part.1.split_once(',') {
                                if let Some((_, keysym_str)) = first_arg.split_once('.') {
                                    if let Ok(keysym) = keysym_str.parse::<u32>() {
                                        if let Some((_, pressed_val)) = rest.split_once('.') {
                                            let pressed = pressed_val.starts_with('1');

                                            let bytes = x11_keysym_to_bytes(keysym, pressed);
                                            if !bytes.is_empty() {
                                                write_half.write_all(&bytes).await
                                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                else => {
                    debug!("Telnet session ending");
                    break;
                }
            }
        }

        info!("Telnet handler connection ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telnet_handler_new() {
        let handler = TelnetHandler::with_defaults();
        assert_eq!(handler.name(), "telnet");
    }

    #[tokio::test]
    async fn test_telnet_handler_health() {
        let handler = TelnetHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_telnet_handler_stats() {
        let handler = TelnetHandler::with_defaults();
        let stats = handler.stats().await.unwrap();
        assert_eq!(stats.total_connections, 0);
    }
}
