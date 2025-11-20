// Browser client integration for RBI (Remote Browser Isolation)
// Provides headless browser session management

use crate::handler::{RbiBackend, RbiConfig};
use bytes::Bytes;
use guacr_protocol::{BinaryEncoder, GuacamoleParser};
use log::{debug, error, info, warn};
use tokio::sync::mpsc;

/// Browser client for RBI sessions
pub struct BrowserClient {
    binary_encoder: BinaryEncoder,
    stream_id: u32,
    width: u32,
    height: u32,
    config: RbiConfig,
}

impl BrowserClient {
    /// Create a new browser client
    pub fn new(width: u32, height: u32, config: RbiConfig) -> Self {
        Self {
            binary_encoder: BinaryEncoder::new(),
            stream_id: 1,
            width,
            height,
            config,
        }
    }

    /// Launch browser and navigate to URL
    ///
    /// # Arguments
    ///
    /// * `url` - URL to navigate to
    /// * `to_client` - Channel to send Guacamole instructions
    /// * `from_client` - Channel to receive Guacamole instructions
    pub async fn connect(
        &mut self,
        url: &str,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        info!("RBI: Launching browser for URL: {}", url);

        // TODO: Implement browser launch based on backend
        //
        // For Chrome backend:
        // 1. Launch headless Chrome with security flags
        // 2. Navigate to URL
        // 3. Capture screenshots periodically
        // 4. Handle input events (mouse, keyboard)
        //
        // For Servo backend (future):
        // 1. Launch Servo engine
        // 2. Navigate to URL
        // 3. Capture screenshots
        // 4. Handle input events

        match self.config.backend {
            RbiBackend::Chrome => {
                self.launch_chrome(url, to_client, from_client).await?;
            }
            RbiBackend::Servo => {
                return Err("Servo backend not yet implemented".to_string());
            }
            RbiBackend::ServoWithFallback => {
                // Try Servo first, fallback to Chrome
                if self.is_servo_compatible(url) {
                    return Err("Servo backend not yet implemented".to_string());
                } else {
                    self.launch_chrome(url, to_client, from_client).await?;
                }
            }
        }

        Ok(())
    }

    /// Launch Chrome browser using chromiumoxide
    async fn launch_chrome(
        &mut self,
        url: &str,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        use crate::chrome_session::ChromeSession;
        use tokio::time::{interval, Duration};

        info!("RBI: Launching Chrome browser for URL: {}", url);

        // Launch Chrome with chromiumoxide
        let mut chrome_session = ChromeSession::new(
            self.width,
            self.height,
            self.config.capture_fps,
            &self.config.chromium_path,
        );

        chrome_session
            .launch(url, &self.config.chromium_path, &self.config.popup_handling)
            .await
            .map_err(|e| format!("Chrome launch failed: {}", e))?;

        // Block popups if configured
        match &self.config.popup_handling {
            crate::handler::PopupHandling::Block => {
                chrome_session.block_popups(&[]).await?;
            }
            crate::handler::PopupHandling::AllowList(allowed) => {
                chrome_session.block_popups(allowed).await?;
            }
            crate::handler::PopupHandling::NavigateMainWindow => {
                // Allow popups but will navigate main window instead
            }
        }

        // Send ready instruction
        let ready_instr = format!("5.ready,{}.{};", 9, "rbi-ready");
        to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| format!("Failed to send ready: {}", e))?;

        // Send size instruction
        let size_instr = self.binary_encoder.encode_size(0, self.width, self.height);
        to_client
            .send(size_instr)
            .await
            .map_err(|e| format!("Failed to send size: {}", e))?;

        info!("RBI: Chrome browser launched, starting capture loop");

        // Setup download interception if enabled
        #[cfg(feature = "chrome")]
        if self.config.download_config.enabled {
            // TODO: Setup CDP download event listener
            // Listen for Page.downloadWillBegin events
            info!(
                "RBI: Downloads enabled with config: max_size={}MB, allowed={:?}, blocked={:?}",
                self.config.download_config.max_file_size_mb,
                self.config.download_config.allowed_extensions,
                self.config.download_config.blocked_extensions
            );
        } else {
            info!("RBI: Downloads disabled (default for security)");
        }

        // Main event loop
        let mut capture_interval =
            interval(Duration::from_millis(1000 / self.config.capture_fps as u64));

        loop {
            tokio::select! {
                // Capture screenshots at configured FPS
                _ = capture_interval.tick() => {
                    match chrome_session.capture_screenshot().await {
                        Ok(Some(screenshot)) => {
                            // Send screenshot to client
                            let msg = self.handle_screenshot(&screenshot)?;
                            if let Err(e) = to_client.send(msg).await {
                                warn!("RBI: Failed to send screenshot: {}", e);
                                break;
                            }
                        }
                        Ok(None) => {
                            // Not time to capture yet
                        }
                        Err(e) => {
                            warn!("RBI: Screenshot capture error: {}", e);
                        }
                    }

                    // Check resource limits
                    match chrome_session.check_resources(self.config.resource_limits.max_memory_mb).await {
                        Ok(false) => {
                            error!("RBI: Resource limit exceeded");
                            break;
                        }
                        Err(e) => {
                            warn!("RBI: Resource check error: {}", e);
                        }
                        Ok(true) => {
                            // Resources OK
                        }
                    }
                }

                // Handle client input
                Some(msg) = from_client.recv() => {
                    if let Err(e) = self.handle_client_input(&mut chrome_session, &msg, &to_client).await {
                        warn!("RBI: Error handling input: {}", e);
                    }
                }

                // Handle download requests (can be triggered manually or via CDP events)
                // Note: Full CDP download event integration requires chromiumoxide API support
                // For now, downloads can be triggered via custom protocol instructions

                else => {
                    debug!("RBI: Connection closed");
                    break;
                }
            }
        }

        info!("RBI: Chrome session ended");
        Ok(())
    }

    /// Handle client input (keyboard, mouse, download)
    async fn handle_client_input(
        &mut self,
        chrome_session: &mut crate::chrome_session::ChromeSession,
        msg: &Bytes,
        to_client: &mpsc::Sender<Bytes>,
    ) -> Result<(), String> {
        let instr = GuacamoleParser::parse_instruction(msg)
            .map_err(|e| format!("Failed to parse instruction: {}", e))?;

        match instr.opcode {
            "key" => {
                if instr.args.len() >= 2 {
                    if let (Ok(keysym), Ok(pressed)) =
                        (instr.args[0].parse::<u32>(), instr.args[1].parse::<u8>())
                    {
                        chrome_session.inject_keyboard(keysym, pressed == 1).await?;
                    }
                }
            }
            "mouse" => {
                if instr.args.len() >= 4 {
                    if let (Ok(mask), Ok(x), Ok(y), Ok(_buttons)) = (
                        instr.args[0].parse::<u8>(),
                        instr.args[1].parse::<i32>(),
                        instr.args[2].parse::<i32>(),
                        instr.args[3].parse::<u8>(),
                    ) {
                        // Extract button state from mask
                        let left_pressed = (mask & 0x01) != 0;
                        let middle_pressed = (mask & 0x02) != 0;
                        let right_pressed = (mask & 0x04) != 0;

                        // Clamp coordinates
                        let x = x.max(0).min(self.width as i32 - 1);
                        let y = y.max(0).min(self.height as i32 - 1);

                        // Send button events
                        if left_pressed {
                            chrome_session.inject_mouse(x, y, 0, true).await?;
                        }
                        if middle_pressed {
                            chrome_session.inject_mouse(x, y, 1, true).await?;
                        }
                        if right_pressed {
                            chrome_session.inject_mouse(x, y, 2, true).await?;
                        }
                    }
                }
            }
            "size" => {
                if let Some(width_str) = instr.args.first() {
                    if let Some(height_str) = instr.args.get(1) {
                        if let (Ok(w), Ok(h)) =
                            (width_str.parse::<u32>(), height_str.parse::<u32>())
                        {
                            info!("RBI: Resize requested: {}x{}", w, h);
                            // TODO: Resize browser viewport via chromiumoxide
                            // chrome_session.resize(w, h).await?;
                        }
                    }
                }
            }
            "download" => {
                // Handle download request: download,<url>,<filename>;
                if instr.args.len() >= 2 {
                    let url = &instr.args[0];
                    let filename = &instr.args[1];
                    if let Err(e) = chrome_session
                        .handle_download(url, filename, &self.config.download_config, to_client)
                        .await
                    {
                        warn!("RBI: Download failed: {}", e);
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Check if URL is compatible with Servo
    fn is_servo_compatible(&self, url: &str) -> bool {
        // Check against allowlist
        for allowed in &self.config.servo_allowlist {
            if url.contains(allowed) {
                return true;
            }
        }
        false
    }

    /// Handle screenshot from browser
    ///
    /// Returns Guacamole instructions for the screenshot
    pub fn handle_screenshot(&mut self, screenshot: &[u8]) -> Result<Bytes, String> {
        // Convert screenshot to Bytes
        let screenshot_bytes = Bytes::from(screenshot.to_vec());

        // Format as binary protocol message (zero-copy)
        let msg = self.binary_encoder.encode_image(
            self.stream_id,
            0, // layer
            0, // x
            0, // y
            self.width as u16,
            self.height as u16,
            0, // format: PNG
            screenshot_bytes,
        );

        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::RbiConfig;

    #[test]
    fn test_browser_client_new() {
        let config = RbiConfig::default();
        let client = BrowserClient::new(1920, 1080, config);
        assert_eq!(client.width, 1920);
        assert_eq!(client.height, 1080);
    }
}
