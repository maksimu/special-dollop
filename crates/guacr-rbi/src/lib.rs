// guacr-rbi: Remote Browser Isolation handler
//
// Provides isolated browser sessions using CEF (Chromium Embedded Framework),
// the same browser engine used by KCM's libguac-client-http.
//
// ## Backends
//
// - **CEF (default)**: Full feature parity with KCM including audio streaming
// - **Chrome/CDP (fallback)**: Uses stock Chrome via DevTools Protocol (no audio)
//
// ## Features
//
// - Comprehensive keyboard/mouse/touch input handling
// - Audio streaming (CEF only - via AudioHandler callbacks)
// - Clipboard synchronization (256KB - 50MB configurable)
// - Dirty rect optimization for display updates
// - Navigation history (back/forward/refresh)
// - URL pattern whitelisting
// - Popup blocking with allowlists
// - Resource limits (memory, CPU, session duration)

mod audio;
mod autofill;
#[cfg(feature = "chrome")]
mod browser_client;
#[cfg(feature = "cef")]
mod cef_browser_client;
#[cfg(feature = "cef")]
mod cef_session;
#[cfg(feature = "chrome")]
mod chrome_session;
mod clipboard;
mod clipboard_polling;
mod cursor;
mod events;
mod file_upload;
mod handler;
mod input;
mod js_dialog;
mod profile_isolation;
mod tabs;

// Re-export public API

// CEF session (primary backend with audio support)
#[cfg(feature = "cef")]
pub use cef_session::{
    CefAudioPacket, CefDisplayEvent, CefSession, CefSharedState,
    DisplayEventType as CefDisplayEventType, DisplaySurface as CefDisplaySurface,
    AUDIO_PACKET_SIZE as CEF_AUDIO_PACKET_SIZE, BYTES_PER_PIXEL, MAX_HEIGHT as CEF_MAX_HEIGHT,
    MAX_WIDTH as CEF_MAX_WIDTH,
};

// Chrome/CDP session (fallback, no audio)
#[cfg(feature = "chrome")]
pub use chrome_session::PerformanceMetrics;

pub use audio::{
    float_to_pcm, AudioConfig, AudioPacket, AudioStream, AUDIO_PACKET_SIZE, GET_AUDIO_STATE_JS,
    WEB_AUDIO_MONITOR_JS,
};
pub use autofill::{
    generate_autofill_js, generate_totp, AutofillCredentials, AutofillManager, AutofillRule,
    TotpAlgorithm, TotpConfig,
};
#[cfg(feature = "chrome")]
pub use browser_client::BrowserClient;
#[cfg(feature = "cef")]
pub use cef_browser_client::CefBrowserClient;
pub use clipboard::{RbiClipboard, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE};
pub use clipboard_polling::{
    ClipboardPollingConfig, ClipboardState, CLIPBOARD_READ_JS, CLIPBOARD_WRITE_JS,
    COPY_EVENT_LISTENER_JS, GET_CLIPBOARD_DATA_JS, SELECTION_COPY_JS,
};
pub use cursor::{
    format_cursor_instruction, format_standard_cursor, CursorState, CursorType, CURSOR_TRACKER_JS,
    GET_CURSOR_JS,
};
pub use events::{
    BrowserState, ClipboardDetails, DisplayEvent, DisplayEventType, DisplayRect, DisplaySurface,
    InputEvent, KeyboardEventDetails, MouseEventDetails, NavigateHistoryDetails,
    NavigateUrlDetails, ResizeEventDetails, TouchEventDetails, UrlPattern, MAX_CLIPBOARD_LENGTH,
    MAX_HEIGHT, MAX_NAVIGATIONS, MAX_URL_LENGTH, MAX_URL_PATTERNS, MAX_WIDTH,
};
pub use file_upload::{
    detect_mime_type, format_upload_dialog_instruction, validate_mime_type, ActiveUpload,
    UploadConfig, UploadEngine, UploadInfo, UploadManager, UploadRequest, UploadState,
    MAX_CONCURRENT_UPLOADS, MAX_UPLOAD_SIZE,
};
pub use handler::{
    DownloadConfig, PopupHandling, RbiBackend, RbiConfig, RbiHandler, ResourceLimits,
};
pub use input::{
    is_printable,
    keysym_to_js_keycode,
    keysym_to_unicode,
    BrowserKeyEvent,
    BrowserMouseEvent,
    BrowserTouchEvent,
    CefEventFlags,
    InputState,
    KeyMapping,
    KeyboardShortcut,
    KeyboardState,
    MouseButton,
    MouseState,
    RbiInputHandler,
    TouchEventType,
    TouchState,
    KEYSYM_ALTGR,
    // Constants
    KEYSYM_ALT_LEFT,
    KEYSYM_ALT_RIGHT,
    KEYSYM_CTRL_LEFT,
    KEYSYM_CTRL_RIGHT,
    KEYSYM_META_LEFT,
    KEYSYM_META_RIGHT,
    KEYSYM_SHIFT_LEFT,
    KEYSYM_SHIFT_RIGHT,
    KEY_MAPPINGS,
    MAX_TOUCH_EVENTS,
    MOUSE_BUTTON_LEFT,
    MOUSE_BUTTON_MIDDLE,
    MOUSE_BUTTON_RIGHT,
    MOUSE_SCROLL_DISTANCE,
    MOUSE_SCROLL_DOWN,
    MOUSE_SCROLL_UP,
};
pub use js_dialog::{
    format_dialog_instruction, JsDialogConfig, JsDialogManager, JsDialogRequest, JsDialogResponse,
    JsDialogType, DIALOG_INTERCEPTOR_JS, DIALOG_TIMEOUT_SECS, MAX_MESSAGE_LENGTH,
};
pub use profile_isolation::{DbusIsolation, ProfileCreationMode, ProfileLock, ProfileLockError};
pub use tabs::{TabInfo, TabManager, MAX_TABS, TAB_ID_INVALID, TAB_ID_PENDING};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RbiError {
    #[error("Browser launch failed: {0}")]
    BrowserLaunchFailed(String),

    #[error("Navigation failed: {0}")]
    NavigationFailed(String),

    #[error("Input error: {0}")]
    InputError(String),

    #[error("Clipboard error: {0}")]
    ClipboardError(String),

    #[error("Display error: {0}")]
    DisplayError(String),

    #[error("URL not allowed: {0}")]
    UrlNotAllowed(String),

    #[error("Resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RbiError>;

/// Check if a URL is allowed based on patterns
pub fn check_url_allowed(url: &str, patterns: &[UrlPattern]) -> bool {
    if patterns.is_empty() {
        return true; // No restrictions
    }
    patterns.iter().any(|p| p.matches(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Core Handler Tests
    // =========================================================================

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
        assert!(!config.upload_config.enabled);
    }

    #[tokio::test]
    async fn test_rbi_handler_health_check() {
        use guacr_handlers::ProtocolHandler;
        let handler = RbiHandler::with_defaults();
        let health = handler.health_check().await;
        assert!(health.is_ok());
    }

    // =========================================================================
    // Input Handler Tests
    // =========================================================================

    #[test]
    fn test_input_handler_keyboard() {
        let mut handler = RbiInputHandler::new();

        // Press Enter
        let event = handler.handle_keyboard(0xFF0D, true);
        assert!(event.pressed);
        assert_eq!(event.js_key_code, 13);

        // Release Enter
        let event = handler.handle_keyboard(0xFF0D, false);
        assert!(!event.pressed);
    }

    #[test]
    fn test_input_handler_mouse() {
        let mut handler = RbiInputHandler::new();

        let event = handler.handle_mouse(100, 200, MOUSE_BUTTON_LEFT);
        assert_eq!(event.x, 100);
        assert_eq!(event.y, 200);
        assert!(event.buttons_pressed.contains(&MouseButton::Left));
    }

    #[test]
    fn test_input_handler_shortcuts() {
        let mut handler = RbiInputHandler::new();

        // Press Ctrl
        handler.handle_keyboard(KEYSYM_CTRL_LEFT, true);

        // Press C
        handler.handle_keyboard('c' as u32, true);

        // Should detect Ctrl+C
        let shortcut = handler.check_shortcut('c' as u32, true);
        assert_eq!(shortcut, Some(KeyboardShortcut::Copy));
    }

    #[test]
    fn test_keysym_to_unicode() {
        assert_eq!(keysym_to_unicode(0xFF0D), Some('\r')); // Enter
        assert_eq!(keysym_to_unicode(0xFF09), Some('\t')); // Tab
    }

    #[test]
    fn test_keysym_to_js_keycode() {
        assert_eq!(keysym_to_js_keycode(0xFF0D), 13); // Enter
        assert_eq!(keysym_to_js_keycode(0xFF08), 8); // Backspace
        assert_eq!(keysym_to_js_keycode(0xFF09), 9); // Tab
        assert_eq!(keysym_to_js_keycode(0xFF1B), 27); // Escape
    }

    // =========================================================================
    // Clipboard Tests
    // =========================================================================

    #[test]
    fn test_clipboard() {
        let mut clipboard = RbiClipboard::with_defaults();
        let data = b"Test clipboard data";

        let result = clipboard.handle_browser_clipboard(data, "text/plain");
        assert!(result.is_ok());

        let stored = clipboard.get_data().unwrap();
        assert_eq!(stored, data.to_vec());
    }

    #[test]
    fn test_clipboard_restrictions() {
        let mut clipboard = RbiClipboard::new(1024 * 1024);

        // Test copy disabled - returns Ok(None), not error
        clipboard.set_restrictions(true, false);
        let result = clipboard.handle_browser_clipboard(b"test", "text/plain");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Silently ignored

        // Test paste disabled
        clipboard.set_restrictions(false, true);
        let result = clipboard.handle_client_clipboard(b"test", "text/plain");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_clipboard_size_limits() {
        let mut clipboard = RbiClipboard::new(CLIPBOARD_MIN_SIZE);
        clipboard.set_restrictions(false, false);

        // Data within limits should work
        let small_data = vec![0u8; 1000];
        let result = clipboard.handle_browser_clipboard(&small_data, "text/plain");
        assert!(result.is_ok());

        let stored = clipboard.get_data();
        assert!(stored.is_ok());
        assert_eq!(stored.unwrap().len(), 1000);
    }

    // =========================================================================
    // URL Pattern Tests
    // =========================================================================

    #[test]
    fn test_url_patterns() {
        let patterns = vec![
            UrlPattern::parse("*.google.com").unwrap(),
            UrlPattern::parse("https://github.com").unwrap(),
        ];

        assert!(check_url_allowed("https://www.google.com", &patterns));
        assert!(check_url_allowed("https://github.com/rust-lang", &patterns));
        assert!(!check_url_allowed("https://evil.com", &patterns));
    }

    // =========================================================================
    // Display Event Tests
    // =========================================================================

    #[test]
    fn test_display_events() {
        let event = DisplayEvent::draw(DisplaySurface::View, DisplayRect::new(10, 20, 100, 200));
        assert_eq!(event.event_type, DisplayEventType::Draw);
        assert_eq!(event.rect.x, 10);
        assert_eq!(event.rect.width, 100);
    }

    #[test]
    fn test_input_event_creation() {
        let event = InputEvent::keyboard(0xFF0D, true);

        if let InputEvent::Keyboard(details) = event {
            assert_eq!(details.keysym, 0xFF0D);
            assert!(details.pressed);
        } else {
            panic!("Expected keyboard event");
        }
    }

    // =========================================================================
    // Upload Tests
    // =========================================================================

    #[test]
    fn test_upload_config_validation() {
        let config = UploadConfig {
            enabled: true,
            allowed_extensions: vec!["pdf".to_string(), "txt".to_string()],
            blocked_extensions: vec!["exe".to_string()],
            max_size: 1024,
            max_concurrent: 3,
        };

        assert!(config.is_extension_allowed("pdf"));
        assert!(!config.is_extension_allowed("exe"));
        assert!(config.is_size_allowed(512));
        assert!(config.is_size_allowed(1024));
        assert!(!config.is_size_allowed(2048));
    }

    #[test]
    fn test_upload_engine_workflow() {
        let config = UploadConfig {
            enabled: true,
            allowed_extensions: vec![],
            blocked_extensions: vec![],
            max_size: 1024,
            max_concurrent: 2,
        };

        let mut engine = UploadEngine::new(config);

        // Request dialog
        let request = engine.manager_mut().handle_dialog_request(false, vec![]);
        assert!(request.is_some());

        let request = request.unwrap();

        // Start upload
        let upload_id = engine
            .start_upload(&request.id, "test.txt", "text/plain", 100)
            .unwrap();

        // Handle chunks
        let progress = engine.handle_chunk(&upload_id, &[0u8; 50]).unwrap();
        assert_eq!(progress, 50.0);

        let progress = engine.handle_chunk(&upload_id, &[1u8; 50]).unwrap();
        assert_eq!(progress, 100.0);

        // Complete
        let (info, data) = engine.complete_upload(&upload_id).unwrap();
        assert_eq!(info.filename, "test.txt");
        assert_eq!(data.len(), 100);
    }

    #[test]
    fn test_mime_type_detection() {
        assert_eq!(detect_mime_type("file.pdf"), "application/pdf");
        assert_eq!(detect_mime_type("image.png"), "image/png");
        assert_eq!(
            detect_mime_type("doc.docx"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(detect_mime_type("unknown.xyz"), "application/octet-stream");
    }

    // =========================================================================
    // JS Dialog Tests
    // =========================================================================

    #[test]
    fn test_js_dialog_auto_dismiss() {
        let config = JsDialogConfig {
            show_dialogs: true,
            auto_confirm: Some(true),
            ..Default::default()
        };

        let mut manager = JsDialogManager::new(config);

        let (_, response) = manager
            .handle_dialog(
                JsDialogType::Confirm,
                "Proceed?".to_string(),
                None,
                "https://example.com".to_string(),
            )
            .unwrap();

        assert!(response.is_some());
        assert!(response.unwrap().confirmed);
    }

    // =========================================================================
    // Autofill Tests
    // =========================================================================

    #[test]
    fn test_autofill_rule_parsing() {
        // Note: AutofillRule uses kebab-case for JSON fields (e.g., "page-pattern")
        let json = r##"{
            "page-pattern": "login.example.com",
            "username-field": "#user",
            "password-field": "//input[@type='password']",
            "submit": "button[type=submit]"
        }"##;

        let rule: AutofillRule = serde_json::from_str(json).unwrap();
        assert!(rule.page_pattern.is_some());
        assert_eq!(rule.username_field.as_deref(), Some("#user"));
        assert!(rule.password_field.as_deref().unwrap().starts_with("//"));
    }

    #[test]
    fn test_autofill_js_generation() {
        let rules = vec![AutofillRule {
            page_pattern: Some("example.com".to_string()),
            username_field: Some("#user".to_string()),
            password_field: Some("#pass".to_string()),
            totp_field: None,
            submit: None,
            cannot_submit: None,
        }];

        let credentials = AutofillCredentials {
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            totp_config: None,
        };

        let js = generate_autofill_js(&rules, &credentials, None, None);

        // Verify JS contains credentials
        assert!(js.contains("admin"));
        assert!(js.contains("#user"));

        // Verify iframe traversal is included
        assert!(js.contains("iframe"));
        assert!(js.contains("contentDocument"));
    }

    #[test]
    fn test_totp_generation() {
        let config = TotpConfig {
            secret: "JBSWY3DPEHPK3PXP".to_string(),
            digits: 6,
            period: 30,
            algorithm: TotpAlgorithm::Sha1,
        };

        let result = generate_totp(&config);
        assert!(result.is_ok());

        let (code, expiration) = result.unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert!(expiration > 0);
    }

    // =========================================================================
    // Tab Manager Tests
    // =========================================================================

    #[test]
    fn test_tab_manager() {
        let mut manager = TabManager::new();

        // Create tabs
        let tab1 = manager.create_tab("https://example.com");
        assert!(tab1.is_some());

        let tab2 = manager.create_tab("https://google.com");
        assert!(tab2.is_some());

        assert_eq!(manager.count(), 2);

        // Switch tabs
        assert!(manager.switch_to_tab(tab2.unwrap()));
        assert!(manager.active_tab().is_some());

        // Close tab
        assert!(manager.close_tab(tab1.unwrap()));
        assert_eq!(manager.count(), 1);
    }

    // =========================================================================
    // Cursor Tests
    // =========================================================================

    #[test]
    fn test_cursor_type_parsing() {
        assert_eq!(CursorType::from_css("pointer"), CursorType::Pointer);
        assert_eq!(CursorType::from_css("text"), CursorType::Text);
        assert_eq!(CursorType::from_css("wait"), CursorType::Wait);
        assert_eq!(CursorType::from_css("unknown"), CursorType::Default);
    }

    // =========================================================================
    // Audio Tests
    // =========================================================================

    #[test]
    fn test_audio_config_defaults() {
        let config = AudioConfig::default();
        assert_eq!(config.channels, 2);
        assert_eq!(config.sample_rate, 44100);
        assert_eq!(config.bits_per_sample, 16);
        assert!(!config.enabled); // Disabled by default for CDP
    }

    #[test]
    fn test_float_to_pcm() {
        let samples = [0.0f32, 1.0, -1.0];
        let pcm = float_to_pcm(&samples, 1, 16);

        // Each sample is 2 bytes (16-bit)
        assert_eq!(pcm.len(), 6);

        // First sample (0.0) should be 0
        let sample0 = i16::from_le_bytes([pcm[0], pcm[1]]);
        assert_eq!(sample0, 0);

        // Second sample (1.0) should be max positive
        let sample1 = i16::from_le_bytes([pcm[2], pcm[3]]);
        assert_eq!(sample1, 32767);

        // Third sample (-1.0) should be near max negative
        let sample2 = i16::from_le_bytes([pcm[4], pcm[5]]);
        assert!(sample2 <= -32767);
    }

    // =========================================================================
    // Performance Metrics Tests (Chrome feature only)
    // =========================================================================

    #[cfg(feature = "chrome")]
    #[test]
    fn test_performance_metrics() {
        let metrics = PerformanceMetrics::default();
        assert_eq!(metrics.js_heap_used_mb, 0);
        assert!(metrics.is_memory_ok(100));
        assert!(!metrics.is_heavy_page());

        let heavy = PerformanceMetrics {
            dom_node_count: 10000,
            resource_count: 300,
            ..Default::default()
        };
        assert!(heavy.is_heavy_page());
    }

    // =========================================================================
    // BrowserClient Tests (Chrome feature only)
    // =========================================================================

    #[cfg(feature = "chrome")]
    #[test]
    fn test_browser_client_creation() {
        let config = RbiConfig::default();
        let client = BrowserClient::new(1920, 1080, config);
        // Just verify it can be created without panic
        drop(client);
    }
}

// =============================================================================
// Chrome Integration Tests
// =============================================================================
// These tests require Chrome/Chromium to be installed and the `chrome` feature enabled.
//
// Run with:
//   cargo test -p guacr-rbi --features chrome -- --ignored
//
// Or run a specific test:
//   cargo test -p guacr-rbi --features chrome test_chrome_full_session -- --ignored --nocapture
//
// Chrome Installation:
//   macOS:   brew install --cask chromium
//   Linux:   apt install chromium-browser
//   Docker:  zenika/alpine-chrome:with-node

#[cfg(all(test, feature = "chrome"))]
mod chrome_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

    // Chrome tests must run sequentially - only one Chrome instance at a time
    static CHROME_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Find Chrome/Chromium executable
    fn find_chrome() -> Option<String> {
        let paths = [
            // macOS
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            // Linux
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/snap/bin/chromium",
        ];

        for path in paths {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }

        // Try PATH
        if let Ok(output) = std::process::Command::new("which").arg("chromium").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }

        None
    }

    fn skip_if_no_chrome() -> bool {
        if find_chrome().is_none() {
            eprintln!("⚠️  Skipping Chrome test - Chrome/Chromium not found");
            eprintln!("   Install with: brew install --cask chromium");
            return true;
        }
        false
    }

    /// Full end-to-end RBI session test
    ///
    /// This test:
    /// 1. Launches Chrome via chromiumoxide
    /// 2. Navigates to example.com
    /// 3. Captures screenshots
    /// 4. Sends keyboard/mouse input
    /// 5. Verifies session remains responsive
    #[tokio::test]
    #[ignore] // Run with --ignored flag
    async fn test_chrome_full_session() {
        // Lock ensures only one Chrome test runs at a time
        let _lock = CHROME_TEST_LOCK.lock().unwrap();

        if skip_if_no_chrome() {
            return;
        }

        let chrome_path = find_chrome().unwrap();
        println!("Using Chrome: {}", chrome_path);

        let config = RbiConfig {
            chromium_path: chrome_path,
            backend: RbiBackend::Chrome,
            capture_fps: 15,
            resource_limits: ResourceLimits {
                max_memory_mb: 500,
                max_cpu_percent: 80,
                timeout_seconds: 60,
            },
            ..Default::default()
        };

        let handler = RbiHandler::new(config);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.com".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Phase 1: Wait for handshake
        let mut got_ready = false;
        let mut got_size = false;
        let mut got_image = false;

        for _ in 0..50 {
            match timeout(Duration::from_secs(1), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);

                    // Text protocol ready instruction
                    if msg_str.contains("ready") {
                        got_ready = true;
                        println!("✓ Got ready instruction");
                    }
                    // Binary protocol size instruction (opcode 0x06)
                    if msg.len() >= 8 && msg[0] == 0x06 {
                        got_size = true;
                        println!("✓ Got size instruction (binary)");
                    }
                    // Image data (binary opcode 0x03 or large payload)
                    if msg.len() > 100 && (msg[0] == 0x03 || msg.len() > 1000) {
                        got_image = true;
                        println!("✓ Got image data ({} bytes)", msg.len());
                    }

                    if got_ready && got_size && got_image {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }

        assert!(
            got_ready || got_size || got_image,
            "Should receive handshake data"
        );

        // Phase 2: Send input
        let click = "5.mouse,3.500,3.400,1.1;";
        from_client_tx
            .send(Bytes::from(click))
            .await
            .expect("Send click");
        println!("✓ Sent mouse click");

        for c in "test".chars() {
            let keysym = c as u32;
            let key_instr = format!("3.key,{}.{},1.1;", keysym.to_string().len(), keysym);
            from_client_tx
                .send(Bytes::from(key_instr))
                .await
                .expect("Send key");
        }
        println!("✓ Sent keyboard input");

        // Phase 3: Verify still alive
        tokio::time::sleep(Duration::from_secs(1)).await;

        let mut received_after_input = false;
        for _ in 0..5 {
            match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
                Ok(Some(_)) => {
                    received_after_input = true;
                    break;
                }
                _ => continue,
            }
        }

        println!("✓ Session responsive after input: {}", received_after_input);

        // Cleanup
        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;

        println!("\n=== Chrome Test Summary ===");
        println!("Ready:      {}", if got_ready { "✓" } else { "✗" });
        println!("Size:       {}", if got_size { "✓" } else { "✗" });
        println!("Screenshot: {}", if got_image { "✓" } else { "✗" });
        println!(
            "Responsive: {}",
            if received_after_input { "✓" } else { "✗" }
        );
    }

    /// Test basic Chrome connection
    #[tokio::test]
    #[ignore]
    async fn test_chrome_connection() {
        // Lock ensures only one Chrome test runs at a time
        let _lock = CHROME_TEST_LOCK.lock().unwrap();

        if skip_if_no_chrome() {
            return;
        }

        let chrome_path = find_chrome().unwrap();

        let config = RbiConfig {
            chromium_path: chrome_path,
            backend: RbiBackend::Chrome,
            ..Default::default()
        };

        let handler = RbiHandler::new(config);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.com".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for ready
        let msg = timeout(CONNECT_TIMEOUT, to_client_rx.recv())
            .await
            .expect("Timeout")
            .expect("Channel closed");

        let msg_str = String::from_utf8_lossy(&msg);
        println!("Received: {}", msg_str);
        assert!(
            msg_str.contains("ready") || msg.len() > 100,
            "Expected ready or image data"
        );

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(10), handle).await;
    }
}
