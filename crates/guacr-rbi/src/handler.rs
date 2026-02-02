use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler,
    EventCallback,
    HandlerError,
    HandlerStats,
    HealthStatus,
    ProtocolHandler,
    // Security
    RbiSecuritySettings,
    // Recording
    RecordingConfig,
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
    pub use_screencast: Option<bool>, // Use Page.startScreencast instead of screenshots
    pub servo_allowlist: Vec<String>, // Domains known to work with Servo
    pub download_config: DownloadConfig,
    pub upload_config: crate::file_upload::UploadConfig,
    // Security settings
    pub allowed_url_patterns: Vec<String>, // URL whitelist patterns
    pub allowed_resource_patterns: Vec<String>, // Resource URL whitelist
    pub disable_copy: bool,                // Block copy from browser
    pub disable_paste: bool,               // Block paste to browser
    pub clipboard_buffer_size: usize,      // Clipboard buffer size (256KB-50MB)
    // Navigation settings
    pub allow_url_manipulation: bool, // Allow user to change URL
    pub ignore_ssl_errors: bool,      // Ignore SSL cert errors for initial URL
    // Localization settings
    pub timezone: Option<String>, // Timezone (e.g., "America/New_York")
    pub accept_language: Option<String>, // Accept-Language header (e.g., "en-US,en;q=0.9")
    // Audio settings
    pub audio_config: crate::audio::AudioConfig,
    // Autofill settings
    pub autofill_rules: Vec<crate::autofill::AutofillRule>,
    pub autofill_credentials: Option<crate::autofill::AutofillCredentials>,
    // Profile settings
    pub profile_storage_directory: Option<String>, // Persist browser profile
    pub create_profile_directory: bool,            // Create directory if missing
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

impl RbiConfig {
    /// Auto-detect Chromium/Chrome installation path
    fn find_chromium_path() -> String {
        // Common Chromium/Chrome paths across different Linux distributions
        let candidates = vec![
            "/usr/bin/chromium",                                            // Debian/Ubuntu
            "/usr/bin/chromium-browser",                                    // Rocky/RHEL/CentOS
            "/usr/lib64/chromium-browser/chromium-browser",                 // Rocky/RHEL alternate
            "/usr/bin/google-chrome",                                       // Google Chrome
            "/usr/bin/google-chrome-stable",                                // Google Chrome stable
            "/snap/bin/chromium",                                           // Snap package
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", // macOS
            "/Applications/Chromium.app/Contents/MacOS/Chromium",           // macOS Chromium
        ];

        for path in candidates {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }

        // Fallback: try to find via which command
        if let Ok(output) = std::process::Command::new("which")
            .arg("chromium-browser")
            .output()
        {
            if output.status.success() {
                if let Ok(path) = String::from_utf8(output.stdout) {
                    let path = path.trim();
                    if !path.is_empty() && std::path::Path::new(path).exists() {
                        return path.to_string();
                    }
                }
            }
        }

        // Last resort: return default and let it fail with helpful error
        "/usr/bin/chromium".to_string()
    }
}

impl Default for RbiConfig {
    fn default() -> Self {
        // Auto-detect Chromium path
        let chromium_path = Self::find_chromium_path();

        Self {
            backend: RbiBackend::ServoWithFallback, // Try Servo, fallback to Chrome
            default_width: 1920,
            default_height: 1080,
            chromium_path,
            popup_handling: PopupHandling::Block,
            resource_limits: ResourceLimits {
                max_memory_mb: 500,
                max_cpu_percent: 80,
                timeout_seconds: 3600,
            },
            capture_fps: 30,
            use_screencast: Some(false), // Default to screenshots (more compatible)
            servo_allowlist: vec![
                "docs.rs".to_string(),
                "github.com".to_string(),
                "stackoverflow.com".to_string(),
                "wikipedia.org".to_string(),
                "rust-lang.org".to_string(),
            ],
            download_config: DownloadConfig::default(),
            upload_config: crate::file_upload::UploadConfig::default(),
            // Security settings - open by default (can be restricted per-connection)
            allowed_url_patterns: Vec::new(), // Empty = allow all
            allowed_resource_patterns: Vec::new(),
            disable_copy: false,
            disable_paste: false,
            clipboard_buffer_size: 256 * 1024, // 256KB default
            // Navigation settings
            allow_url_manipulation: true,
            ignore_ssl_errors: false,
            // Localization settings
            timezone: None,        // Use browser default
            accept_language: None, // Use browser default
            // Audio settings
            audio_config: crate::audio::AudioConfig::default(),
            // Autofill settings
            autofill_rules: Vec::new(),
            autofill_credentials: None,
            // Profile settings
            profile_storage_directory: None, // Don't persist by default
            create_profile_directory: false,
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

    #[allow(unused_variables)]
    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("RBI handler starting");

        // Parse RBI-specific security settings
        let security = RbiSecuritySettings::from_params(&params);
        info!(
            "RBI: Security - read_only={}, download={}, upload={}, print={}, allowlist_len={}",
            security.base.read_only,
            security.is_download_allowed(),
            security.is_upload_allowed(),
            security.is_print_allowed(),
            security.url_allowlist.len()
        );

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);
        if recording_config.is_enabled() {
            info!(
                "RBI: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        let url = params
            .get("url")
            .ok_or_else(|| HandlerError::MissingParameter("url".to_string()))?;

        // Security: Check URL against allowlist/blocklist
        if !security.is_url_allowed(url) {
            return Err(HandlerError::SecurityViolation(format!(
                "URL not allowed: {}",
                url
            )));
        }

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

        // IMPORTANT: Always use DEFAULT size during initialization (like guacd does)
        // The client will send a resize instruction with actual browser dimensions after handshake
        // This prevents "half screen" display issues
        info!("RBI: Using default handshake size - will resize after client connects");
        let _width = self.config.default_width;
        let _height = self.config.default_height;

        // Use the appropriate backend
        #[cfg(feature = "chrome")]
        {
            use crate::browser_client::BrowserClient;
            let mut browser_client = BrowserClient::new(_width, _height, self.config.clone());

            // Connect and handle session
            browser_client
                .connect(url, to_client, from_client)
                .await
                .map_err(HandlerError::ConnectionFailed)?;

            info!("RBI handler ended (Chrome/CDP)");
            Ok(())
        }
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
        guacr_handlers::connect_with_event_adapter(
            |params, to_client, from_client| self.connect(params, to_client, from_client),
            params,
            callback,
            from_client,
            4096, // channel capacity
        )
        .await
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
