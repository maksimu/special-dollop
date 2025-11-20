// Chrome session manager using chromiumoxide for RBI
// Provides browser launch, navigation, screenshot capture, and input handling

use log::warn;
use tokio::time::{Duration, Instant};

#[cfg(feature = "chrome")]
use chromiumoxide::browser::{Browser, BrowserConfigBuilder};
#[cfg(feature = "chrome")]
use chromiumoxide::page::Page;
#[cfg(feature = "chrome")]
use futures::StreamExt;

/// Chrome session manager for RBI
///
/// Manages a dedicated Chrome instance with:
/// - Chrome DevTools Protocol (CDP) communication via chromiumoxide
/// - Screenshot capture at configurable FPS
/// - Input injection (keyboard, mouse)
/// - Popup blocking/handling
/// - Resource monitoring
pub struct ChromeSession {
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    width: u32,
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    height: u32,
    #[cfg(feature = "chrome")]
    browser: Option<Browser>,
    #[cfg(feature = "chrome")]
    page: Option<Page>,
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    last_capture_time: Instant,
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    capture_interval: Duration,
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    download_count: usize, // Track downloads for rate limiting
}

impl ChromeSession {
    /// Create a new Chrome session
    pub fn new(width: u32, height: u32, capture_fps: u32, _chromium_path: &str) -> Self {
        let capture_interval = Duration::from_millis(1000 / capture_fps as u64);

        Self {
            width,
            height,
            #[cfg(feature = "chrome")]
            browser: None,
            #[cfg(feature = "chrome")]
            page: None,
            last_capture_time: Instant::now(),
            capture_interval,
            download_count: 0,
        }
    }

    /// Launch Chrome with CDP enabled using chromiumoxide
    #[cfg(feature = "chrome")]
    pub async fn launch(
        &mut self,
        url: &str,
        chromium_path: &str,
        popup_handling: &crate::handler::PopupHandling,
    ) -> Result<(), String> {
        info!("RBI: Launching Chrome for URL: {}", url);

        // Build Chrome config with security flags
        let mut args = vec![
            format!("--window-size={},{}", self.width, self.height),
            "--headless=new".to_string(),
            "--disable-gpu".to_string(),
            "--disable-dev-shm-usage".to_string(),
            "--disable-software-rasterizer".to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-background-networking".to_string(),
            "--disable-background-timer-throttling".to_string(),
            "--disable-backgrounding-occluded-windows".to_string(),
            "--disable-breakpad".to_string(),
            "--disable-client-side-phishing-detection".to_string(),
            "--disable-component-extensions-with-background-pages".to_string(),
            "--disable-default-apps".to_string(),
            "--disable-extensions".to_string(),
            "--disable-features=TranslateUI".to_string(),
            "--disable-hang-monitor".to_string(),
            "--disable-ipc-flooding-protection".to_string(),
            "--disable-prompt-on-repost".to_string(),
            "--disable-renderer-backgrounding".to_string(),
            "--disable-sync".to_string(),
            "--disable-web-resources".to_string(),
            "--metrics-recording-only".to_string(),
            "--safebrowsing-disable-auto-update".to_string(),
            "--enable-automation".to_string(),
            "--password-store=basic".to_string(),
            "--use-mock-keychain".to_string(),
            // Security flags (equivalent to KCM patches)
            "--disable-bluetooth".to_string(),
            "--disable-usb-keyboard-detect".to_string(),
            "--disable-speech-api".to_string(),
            "--disable-notifications".to_string(),
        ];

        // Popup blocking configuration
        match popup_handling {
            crate::handler::PopupHandling::Block => {
                args.push("--disable-popup-blocking=false".to_string()); // Keep popup blocking ON
            }
            crate::handler::PopupHandling::AllowList(_) => {
                args.push("--disable-popup-blocking=false".to_string());
                // Will handle via JavaScript injection
            }
            crate::handler::PopupHandling::NavigateMainWindow => {
                // Allow popups but will navigate main window instead
            }
        }

        // Configure download behavior
        if !popup_handling.is_download_enabled() {
            args.push("--disable-downloads".to_string());
        }

        // Launch browser with chromiumoxide
        // Note: chromiumoxide API may vary by version - adjust as needed
        let config = BrowserConfigBuilder::default()
            .args(args)
            .chrome_executable(chromium_path.into())
            .build()
            .map_err(|e| format!("Failed to build browser config: {}", e))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| format!("Failed to launch Chrome: {}", e))?;

        // Spawn handler task to process browser events
        tokio::spawn(async move {
            while let Some(_) = handler.next().await {
                // Handle browser events (target created, etc.)
            }
        });

        // Create new page and navigate
        let page = browser
            .new_page(url)
            .await
            .map_err(|e| format!("Failed to create page: {}", e))?;

        // Wait for page to load
        page.wait_for_navigation()
            .await
            .map_err(|e| format!("Failed to wait for navigation: {}", e))?;

        self.browser = Some(browser);
        self.page = Some(page);

        info!("RBI: Chrome launched successfully");
        Ok(())
    }

    /// Launch Chrome (fallback when chrome feature is disabled)
    #[cfg(not(feature = "chrome"))]
    pub async fn launch(
        &mut self,
        _url: &str,
        _chromium_path: &str,
        _popup_handling: &crate::handler::PopupHandling,
    ) -> Result<(), String> {
        Err("Chrome feature not enabled. Enable with 'chrome' feature flag.".to_string())
    }

    /// Capture screenshot via chromiumoxide
    #[cfg(feature = "chrome")]
    pub async fn capture_screenshot(&mut self) -> Result<Option<Vec<u8>>, String> {
        // Check if enough time has passed since last capture
        if self.last_capture_time.elapsed() < self.capture_interval {
            return Ok(None);
        }

        let page = self
            .page
            .as_ref()
            .ok_or_else(|| "Page not initialized".to_string())?;

        // Capture screenshot using chromiumoxide
        // Note: Adjust API calls based on actual chromiumoxide version
        let screenshot = page
            .screenshot(None) // Use default PNG format
            .await
            .map_err(|e| format!("Failed to capture screenshot: {}", e))?;

        // Convert screenshot bytes to Vec<u8>
        let screenshot_vec = screenshot.to_vec();

        self.last_capture_time = Instant::now();
        Ok(Some(screenshot_vec))
    }

    /// Capture screenshot (fallback)
    #[cfg(not(feature = "chrome"))]
    pub async fn capture_screenshot(&mut self) -> Result<Option<Vec<u8>>, String> {
        Err("Chrome feature not enabled".to_string())
    }

    /// Inject keyboard input via chromiumoxide
    #[cfg(feature = "chrome")]
    pub async fn inject_keyboard(&self, keysym: u32, pressed: bool) -> Result<(), String> {
        let page = self
            .page
            .as_ref()
            .ok_or_else(|| "Page not initialized".to_string())?;

        // Convert keysym to key string
        let key = self.keysym_to_key(keysym)?;

        if pressed {
            // Use page's type_str method for keyboard input
            page.type_str(&key)
                .await
                .map_err(|e| format!("Failed to type key: {}", e))?;
        } else {
            // chromiumoxide doesn't have explicit key release
            debug!("RBI: Key release for keysym {} (key: {})", keysym, key);
        }

        Ok(())
    }

    /// Inject keyboard (fallback)
    #[cfg(not(feature = "chrome"))]
    pub async fn inject_keyboard(&self, _keysym: u32, _pressed: bool) -> Result<(), String> {
        Err("Chrome feature not enabled".to_string())
    }

    /// Inject mouse input via chromiumoxide
    #[cfg(feature = "chrome")]
    pub async fn inject_mouse(
        &self,
        x: i32,
        y: i32,
        button: u8,
        pressed: bool,
    ) -> Result<(), String> {
        let page = self
            .page
            .as_ref()
            .ok_or_else(|| "Page not initialized".to_string())?;

        let x = x.max(0) as f64;
        let y = y.max(0) as f64;

        if pressed {
            match button {
                0 => {
                    // Left click
                    page.click((x, y))
                        .await
                        .map_err(|e| format!("Failed to click: {}", e))?;
                }
                1 => {
                    // Middle button - use JavaScript
                    page.evaluate(format!("document.elementFromPoint({}, {}).dispatchEvent(new MouseEvent('mousedown', {{bubbles: true, cancelable: true, clientX: {}, clientY: {}, button: 1}}))", x, y, x, y))
                        .await
                        .map_err(|e| format!("Failed to middle click: {}", e))?;
                }
                2 => {
                    // Right click - use JavaScript for context menu
                    page.evaluate(format!("document.elementFromPoint({}, {}).dispatchEvent(new MouseEvent('contextmenu', {{bubbles: true, cancelable: true, clientX: {}, clientY: {}}}))", x, y, x, y))
                        .await
                        .map_err(|e| format!("Failed to right click: {}", e))?;
                }
                _ => {}
            }
        } else {
            // Mouse release - chromiumoxide handles this automatically
            debug!("RBI: Mouse release at ({}, {})", x, y);
        }

        Ok(())
    }

    /// Inject mouse (fallback)
    #[cfg(not(feature = "chrome"))]
    pub async fn inject_mouse(
        &self,
        _x: i32,
        _y: i32,
        _button: u8,
        _pressed: bool,
    ) -> Result<(), String> {
        Err("Chrome feature not enabled".to_string())
    }

    /// Block popups via JavaScript injection
    #[cfg(feature = "chrome")]
    pub async fn block_popups(&self, allow_list: &[String]) -> Result<(), String> {
        let page = self
            .page
            .as_ref()
            .ok_or_else(|| "Page not initialized".to_string())?;

        let js_code = format!(
            r#"
            (function() {{
                const allowList = {};
                const originalOpen = window.open;
                window.open = function(url, name, specs) {{
                    try {{
                        const domain = new URL(url).hostname;
                        if (allowList.some(allowed => domain.includes(allowed))) {{
                            window.location.href = url;
                            return null;
                        }}
                        console.log('Popup blocked:', url);
                        return null;
                    }} catch (e) {{
                        console.log('Popup blocked (invalid URL):', url);
                        return null;
                    }}
                }};
            }})();
        "#,
            serde_json::to_string(allow_list).unwrap_or("[]".to_string())
        );

        page.evaluate(js_code)
            .await
            .map_err(|e| format!("Failed to inject popup blocking script: {}", e))?;

        info!("RBI: Popup blocking script injected");
        Ok(())
    }

    /// Block popups (fallback)
    #[cfg(not(feature = "chrome"))]
    pub async fn block_popups(&self, _allow_list: &[String]) -> Result<(), String> {
        Err("Chrome feature not enabled".to_string())
    }

    /// Monitor resource usage
    pub async fn check_resources(&self, max_memory_mb: usize) -> Result<bool, String> {
        // TODO: Get Chrome process memory usage
        // For now, just return OK
        // In production, you'd use process monitoring or cgroups
        let _ = max_memory_mb;
        Ok(true)
    }

    /// Convert X11 keysym to key string for chromiumoxide
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    fn keysym_to_key(&self, keysym: u32) -> Result<String, String> {
        // Basic key mapping - chromiumoxide expects key names
        match keysym {
            // Letters A-Z
            0x0041..=0x005A => {
                let key = ((keysym - 0x0041) + 'A' as u32) as u8 as char;
                Ok(key.to_lowercase().to_string())
            }
            // Numbers 0-9
            0x0030..=0x0039 => {
                let key = ((keysym - 0x0030) + '0' as u32) as u8 as char;
                Ok(key.to_string())
            }
            // Special keys
            0xFF08 => Ok("Backspace".to_string()),
            0xFF09 => Ok("Tab".to_string()),
            0xFF0D => Ok("Enter".to_string()),
            0xFF1B => Ok("Escape".to_string()),
            0xFF20 => Ok(" ".to_string()),
            0xFF52 => Ok("ArrowUp".to_string()),
            0xFF54 => Ok("ArrowDown".to_string()),
            0xFF51 => Ok("ArrowLeft".to_string()),
            0xFF53 => Ok("ArrowRight".to_string()),
            _ => {
                warn!(
                    "RBI: Unknown keysym: 0x{:04x}, using 'Unidentified'",
                    keysym
                );
                Ok("Unidentified".to_string())
            }
        }
    }

    /// Get page reference (for advanced operations)
    #[cfg(feature = "chrome")]
    pub fn page(&self) -> Option<&Page> {
        self.page.as_ref()
    }

    /// Handle file download request
    ///
    /// Downloads file from URL and streams it to client via Guacamole protocol.
    /// Applies security checks: size limits, file type restrictions, rate limiting.
    #[cfg(feature = "chrome")]
    pub async fn handle_download(
        &mut self,
        url: &str,
        suggested_filename: &str,
        download_config: &crate::handler::DownloadConfig,
        to_client: &tokio::sync::mpsc::Sender<bytes::Bytes>,
    ) -> Result<(), String> {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine;
        use bytes::Bytes;
        use guacr_protocol::streams::{format_blob, format_end};

        // Check if downloads are enabled
        if !download_config.enabled {
            return Err("Downloads are disabled".to_string());
        }

        // Check rate limiting
        if self.download_count >= download_config.max_downloads_per_session {
            return Err(format!(
                "Maximum downloads per session ({}) exceeded",
                download_config.max_downloads_per_session
            ));
        }

        // Check file extension
        let ext = suggested_filename
            .split('.')
            .last()
            .unwrap_or("")
            .to_lowercase();

        if download_config.blocked_extensions.contains(&ext) {
            return Err(format!("File type .{} is not allowed", ext));
        }

        if !download_config.allowed_extensions.is_empty()
            && !download_config.allowed_extensions.contains(&ext)
        {
            return Err(format!("File type .{} is not in allowed list", ext));
        }

        // Download file via HTTP
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Failed to download file: {}", e))?;

        // Check content length
        if let Some(len) = response.content_length() {
            let max_size = download_config.max_file_size_mb * 1024 * 1024;
            if len > max_size as u64 {
                return Err(format!(
                    "File too large: {} bytes (max: {} bytes)",
                    len, max_size
                ));
            }
        }

        // Read file data
        let file_data = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read file data: {}", e))?
            .to_vec();

        // Check actual size (in case Content-Length was missing)
        let max_size = download_config.max_file_size_mb * 1024 * 1024;
        if file_data.len() > max_size {
            return Err(format!(
                "File too large: {} bytes (max: {} bytes)",
                file_data.len(),
                max_size
            ));
        }

        // Detect MIME type
        let mimetype = self.detect_mime_type(suggested_filename);

        // Send file via Guacamole protocol
        // Increment download_count first to get unique stream ID
        self.download_count += 1;
        let stream_id = (self.download_count as u32) + 1000; // Start at 1000 to avoid conflicts

        // Send file instruction: file,<stream>,<mimetype>,<filename>;
        let file_instr = format!(
            "7.file,{}.{},{}.{},{}.{};",
            stream_id.to_string().len(),
            stream_id,
            mimetype.len(),
            mimetype,
            suggested_filename.len(),
            suggested_filename
        );
        to_client
            .send(Bytes::from(file_instr))
            .await
            .map_err(|e| format!("Failed to send file instruction: {}", e))?;

        // Send file data in chunks via blob instruction (base64-encoded)
        let chunk_size = 48 * 1024; // 48KB chunks (64KB base64 = ~48KB raw)
        for chunk in file_data.chunks(chunk_size) {
            let base64_data = BASE64.encode(chunk);
            let blob_instr = format_blob(stream_id, &base64_data);
            to_client
                .send(Bytes::from(blob_instr))
                .await
                .map_err(|e| format!("Failed to send blob chunk: {}", e))?;
        }

        // Send end instruction: end,<stream>;
        let end_instr = format_end(stream_id);
        to_client
            .send(Bytes::from(end_instr))
            .await
            .map_err(|e| format!("Failed to send end instruction: {}", e))?;

        info!(
            "RBI: File '{}' sent to client ({} bytes, {} chunks)",
            suggested_filename,
            file_data.len(),
            (file_data.len() + chunk_size - 1) / chunk_size
        );

        Ok(())
    }

    /// Handle download (fallback)
    #[cfg(not(feature = "chrome"))]
    pub async fn handle_download(
        &mut self,
        _url: &str,
        _suggested_filename: &str,
        _download_config: &crate::handler::DownloadConfig,
        _to_client: &tokio::sync::mpsc::Sender<bytes::Bytes>,
    ) -> Result<(), String> {
        Err("Chrome feature not enabled".to_string())
    }

    /// Detect MIME type from filename
    #[cfg_attr(not(feature = "chrome"), allow(dead_code))]
    fn detect_mime_type(&self, filename: &str) -> &str {
        let ext = filename.split('.').next_back().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "pdf" => "application/pdf",
            "txt" => "text/plain",
            "csv" => "text/csv",
            "json" => "application/json",
            "xml" => "application/xml",
            "zip" => "application/zip",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "html" => "text/html",
            "css" => "text/css",
            "js" => "application/javascript",
            _ => "application/octet-stream",
        }
    }
}

impl Drop for ChromeSession {
    fn drop(&mut self) {
        #[cfg(feature = "chrome")]
        {
            // chromiumoxide handles browser cleanup automatically
            if let Some(browser) = self.browser.take() {
                // Browser will be closed when dropped
                drop(browser);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_session_new() {
        let session = ChromeSession::new(1920, 1080, 30, "/usr/bin/chromium");
        assert_eq!(session.width, 1920);
        assert_eq!(session.height, 1080);
    }
}
