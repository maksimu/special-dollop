use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use guacr_rdp::FrameBuffer;
use log::info;
use std::collections::HashMap;
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopupHandling {
    Block,                  // Block all popups (default, most secure)
    AllowList(Vec<String>), // Only allow specific domains (e.g., OAuth)
    NavigateMainWindow,     // Navigate main window to popup URL
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

    async fn connect(
        &self,
        params: HashMap<String, String>,
        _to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
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

        let mut _framebuffer =
            FrameBuffer::new(self.config.default_width, self.config.default_height);

        // Stub event loop - implement above plan
        while from_client.recv().await.is_some() {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rbi_handler_new() {
        let handler = RbiHandler::with_defaults();
        assert_eq!(handler.name(), "http");
    }

    #[test]
    fn test_rbi_config() {
        let config = RbiConfig::default();
        assert_eq!(config.default_width, 1920);
        assert_eq!(config.default_height, 1080);
    }
}
