use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use log::info;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Remote Browser Isolation (RBI) handler
///
/// Provides isolated browser sessions using headless Chrome/Chromium.
/// Screen is captured and sent as images over WebRTC.
///
/// ## CRITICAL: Pixel-Perfect Dimension Alignment
///
/// To avoid blurry rendering from browser scaling:
/// 1. Browser sends size request: width_px, height_px
/// 2. Set browser viewport: browser.set_window_size(width_px, height_px)
/// 3. Send `size` instruction with SAME dimensions: size,0,width_px,height_px;
/// 4. Capture screenshot at SAME dimensions: screenshot(width_px, height_px)
/// 5. Result: PNG size = layer size = no scaling = crisp
///
/// NEVER use mismatched dimensions - causes browser to scale and blur the image.
pub struct RbiHandler {
    config: RbiConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RbiBackend {
    Chrome,            // Production: Full compatibility, 200-500MB
    Servo,             // Experimental: Rust-native, 50-100MB, limited sites
    ServoWithFallback, // Try Servo first, fallback to Chrome if broken
}

#[derive(Debug, Clone)]
pub struct RbiConfig {
    pub backend: RbiBackend,
    pub default_width: u32,
    pub default_height: u32,
    pub chromium_path: String,
    pub popup_handling: PopupHandling,
    pub resource_limits: ResourceLimits,
    pub capture_fps: u32,
    pub servo_allowlist: Vec<String>, // Domains known to work with Servo
    pub download_config: DownloadConfig,
}

#[derive(Debug, Clone)]
pub struct DownloadConfig {
    pub enabled: bool,           // Enable downloads (default: false for security)
    pub max_file_size_mb: usize, // Maximum file size (default: 10MB)
    pub allowed_extensions: Vec<String>, // Allowed file extensions (e.g., ["pdf", "txt"])
    pub blocked_extensions: Vec<String>, // Blocked file extensions (e.g., ["exe", "bat"])
    pub require_approval: bool,  // Require user approval (future)
    pub max_downloads_per_session: usize, // Rate limiting (default: 5)
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopupHandling {
    Block,                  // Block all popups (default, most secure)
    AllowList(Vec<String>), // Only allow specific domains (e.g., OAuth)
    NavigateMainWindow,     // Navigate main window to popup URL
}

impl PopupHandling {
    /// Check if downloads should be enabled based on popup handling config
    #[allow(dead_code)]
    fn is_download_enabled(&self) -> bool {
        // Downloads are controlled separately via DownloadConfig
        // This is just a helper for Chrome flags
        false // Default: block downloads
    }
}

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_memory_mb: usize, // Kill browser if exceeds (default: 500MB)
    pub max_cpu_percent: u32, // Throttle if exceeds (default: 80%)
    pub timeout_seconds: u64, // Max session duration (default: 3600 = 1 hour)
}

impl Default for RbiConfig {
    fn default() -> Self {
        Self {
            backend: RbiBackend::ServoWithFallback, // Try Servo, fallback to Chrome
            default_width: 1920,
            default_height: 1080,
            chromium_path: "/usr/bin/chromium".to_string(),
            popup_handling: PopupHandling::Block,
            resource_limits: ResourceLimits {
                max_memory_mb: 500,
                max_cpu_percent: 80,
                timeout_seconds: 3600,
            },
            capture_fps: 30,
            servo_allowlist: vec![
                "docs.rs".to_string(),
                "github.com".to_string(),
                "stackoverflow.com".to_string(),
                "wikipedia.org".to_string(),
                "rust-lang.org".to_string(),
            ],
            download_config: DownloadConfig::default(),
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Block by default (like KCM)
            max_file_size_mb: 10,
            allowed_extensions: vec![
                "pdf".to_string(),
                "txt".to_string(),
                "csv".to_string(),
                "json".to_string(),
                "xml".to_string(),
                "png".to_string(),
                "jpg".to_string(),
                "jpeg".to_string(),
                "gif".to_string(),
            ],
            blocked_extensions: vec![
                "exe".to_string(),
                "bat".to_string(),
                "sh".to_string(),
                "dmg".to_string(),
                "msi".to_string(),
                "app".to_string(),
                "deb".to_string(),
                "rpm".to_string(),
                "zip".to_string(), // Can contain executables
            ],
            require_approval: true,
            max_downloads_per_session: 5,
        }
    }
}

impl RbiHandler {
    pub fn new(config: RbiConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(RbiConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for RbiHandler {
    fn name(&self) -> &str {
        "http"
    }

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("RBI handler starting");

        let url = params
            .get("url")
            .ok_or_else(|| HandlerError::MissingParameter("url".to_string()))?;

        info!("RBI launching browser for URL: {}", url);

        // IMPLEMENTATION GUIDE - Dedicated Chrome with Security
        //
        // Use headless_chrome or chromiumoxide crate
        //
        // 1. Launch DEDICATED Chrome instance (NO SHARING for security):
        //    let browser = Browser::new(LaunchOptions {
        //        headless: true,
        //        sandbox: true,
        //        args: vec![
        //            "--no-first-run",
        //            "--disable-gpu",
        //            "--disable-dev-shm-usage",
        //            "--disable-software-rasterizer",
        //            // POPUP BLOCKING:
        //            "--disable-popup-blocking=false",  // Keep popup blocking ON
        //        ],
        //        ..Default::default()
        //    })?;
        //
        // 2. Apply resource limits (cgroups on Linux):
        //    #[cfg(target_os = "linux")]
        //    apply_cgroup_limits(browser.process_id(), self.config.resource_limits)?;
        //
        // 3. Block popups via JavaScript injection:
        //    tab.evaluate(r#"
        //        // Override window.open to block popups
        //        window.open = function(url, name, specs) {
        //            // Check allowlist
        //            if (isAllowed(url)) {
        //                window.location.href = url;  // Navigate instead
        //            }
        //            return null;  // Block popup
        //        };
        //    "#, false)?;
        //
        // 4. Navigate to URL:
        //    let tab = browser.new_tab()?;
        //    tab.navigate_to(url)?;
        //    tab.wait_until_navigated()?;
        //
        // 5. Capture loop (30-60 FPS based on config):
        //    let mut interval = tokio::time::interval(
        //        Duration::from_millis(1000 / self.config.capture_fps as u64)
        //    );
        //    let mut last_screenshot: Option<Vec<u8>> = None;
        //
        //    loop {
        //        tokio::select! {
        //            _ = interval.tick() => {
        //                // Capture screenshot
        //                let screenshot = tab.capture_screenshot(
        //                    Format::PNG,
        //                    None,     // Full page
        //                    true,     // From surface
        //                )?;
        //
        //                // Only send if changed (dirty checking)
        //                if has_changed(&screenshot, &last_screenshot) {
        //                    send_image_instruction(&screenshot, &to_client).await?;
        //                    last_screenshot = Some(screenshot);
        //                }
        //            }
        //
        //            Some(msg) = from_client.recv() => {
        //                // Parse input (mouse, keyboard)
        //                // Inject into browser
        //            }
        //        }
        //    }
        //
        // 6. Resource monitoring:
        //    tokio::spawn(async move {
        //        loop {
        //            let mem_usage = get_process_memory(browser_pid)?;
        //            if mem_usage > max_memory {
        //                browser.kill()?;  // Hard limit exceeded
        //                break;
        //            }
        //            tokio::time::sleep(Duration::from_secs(5)).await;
        //        }
        //    });
        //
        // SERVO INTEGRATION PATH (Future):
        //
        // When Servo matures (v1.0+), add servo backend:
        //
        //    pub enum RbiBackend {
        //        Chrome,  // Full compatibility, 200-500MB
        //        Servo,   // Rust-native, 50-100MB, limited compatibility
        //    }
        //
        //    if servo_compatibility.is_compatible(url) {
        //        // Use Servo - lighter, Rust-native
        //        launch_servo(url)?
        //    } else {
        //        // Fallback to Chrome
        //        launch_chrome(url)?
        //    }
        //
        // Servo benefits:
        //   - Pure Rust (no subprocess overhead)
        //   - 50-100MB vs 200-500MB
        //   - Memory safe
        //   - Direct embedding
        //   - Better security isolation
        //
        // Reference: docs/guacr/RBI_SERVO_AND_DATABASE_OPTIONS.md

        // Get browser dimensions from params or use defaults
        let width = params
            .get("width")
            .and_then(|w| w.parse().ok())
            .unwrap_or(self.config.default_width);
        let height = params
            .get("height")
            .and_then(|h| h.parse().ok())
            .unwrap_or(self.config.default_height);

        // Use browser client wrapper
        use crate::browser_client::BrowserClient;
        let mut browser_client = BrowserClient::new(width, height, self.config.clone());

        // Connect and handle session
        browser_client
            .connect(url, to_client, from_client)
            .await
            .map_err(HandlerError::ConnectionFailed)?;

        info!("RBI handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

// Event-based handler implementation for zero-copy integration
#[async_trait]
impl EventBasedHandler for RbiHandler {
    fn name(&self) -> &str {
        "http"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Wrap the channel-based interface
        let (to_client, mut handler_rx) = mpsc::channel::<Bytes>(128);

        let sender = InstructionSender::new(callback);
        let sender_arc = Arc::new(sender);

        // Spawn task to forward channel messages to event callback (zero-copy)
        let sender_clone = Arc::clone(&sender_arc);
        tokio::spawn(async move {
            while let Some(msg) = handler_rx.recv().await {
                sender_clone.send(msg); // Zero-copy: Bytes is reference-counted
            }
        });

        // Call the existing channel-based connect method
        self.connect(params, to_client, from_client).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rbi_handler_new() {
        let handler = RbiHandler::with_defaults();
        assert_eq!(
            <_ as guacr_handlers::ProtocolHandler>::name(&handler),
            "http"
        );
    }

    #[test]
    fn test_rbi_config() {
        let config = RbiConfig::default();
        assert_eq!(config.default_width, 1920);
        assert_eq!(config.default_height, 1080);
    }
}
