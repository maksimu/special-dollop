// Browser client integration for RBI (Remote Browser Isolation)
// Provides headless browser session management with proper input handling

use crate::adaptive_fps::AdaptiveFps;
use crate::clipboard::RbiClipboard;
use crate::cursor::format_standard_cursor;
use crate::dirty_tracker::RbiDirtyTracker;
use crate::file_upload::{format_upload_dialog_instruction, UploadEngine};
use crate::js_dialog::{JsDialogConfig, JsDialogManager};
use crate::screencast::{ScreencastConfig, ScreencastProcessor};
use crate::scroll_detector::ScrollDetector;
// Events module provides types used for more complex RBI scenarios
use crate::handler::{RbiBackend, RbiConfig};
use crate::input::{KeyboardShortcut, RbiInputHandler};
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
    input_handler: RbiInputHandler,
    clipboard: RbiClipboard,
    upload_engine: UploadEngine,
    dialog_manager: JsDialogManager,
}

impl BrowserClient {
    /// Create a new browser client
    pub fn new(width: u32, height: u32, config: RbiConfig) -> Self {
        let mut clipboard = RbiClipboard::new(config.clipboard_buffer_size);
        clipboard.set_restrictions(config.disable_copy, config.disable_paste);

        // Use upload config from RbiConfig
        let upload_config = config.upload_config.clone();

        // Setup dialog config
        let dialog_config = JsDialogConfig {
            show_dialogs: true,
            auto_dismiss_alert_ms: Some(10000), // Auto-dismiss alerts after 10s
            allow_beforeunload: false,          // Block beforeunload by default
            ..Default::default()
        };

        Self {
            binary_encoder: BinaryEncoder::new(),
            stream_id: 1,
            width,
            height,
            input_handler: RbiInputHandler::new(),
            clipboard,
            upload_engine: UploadEngine::new(upload_config),
            dialog_manager: JsDialogManager::new(dialog_config),
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

        // Send ready instruction with proper LENGTH.VALUE format
        let ready_value = "rbi-ready";
        let ready_instr = format!("5.ready,{}.{};", ready_value.len(), ready_value);
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
            info!(
                "RBI: Downloads enabled with config: max_size={}MB, allowed={:?}, blocked={:?}",
                self.config.download_config.max_file_size_mb,
                self.config.download_config.allowed_extensions,
                self.config.download_config.blocked_extensions
            );
        } else {
            info!("RBI: Downloads disabled (default for security)");
        }

        // Install clipboard event listener (no Chrome patches needed)
        if !self.config.disable_copy {
            if let Err(e) = chrome_session.install_clipboard_listener().await {
                warn!("RBI: Failed to install clipboard listener: {}", e);
            }
        }

        // Install cursor tracker
        if let Err(e) = chrome_session.install_cursor_tracker().await {
            warn!("RBI: Failed to install cursor tracker: {}", e);
        }

        // Install scroll tracker
        if let Err(e) = chrome_session.install_scroll_tracker().await {
            warn!("RBI: Failed to install scroll tracker: {}", e);
        }

        // Enable file chooser interception if uploads are enabled
        if self.upload_engine.manager().is_enabled() {
            if let Err(e) = chrome_session.enable_file_chooser_interception().await {
                warn!("RBI: Failed to enable file chooser interception: {}", e);
            } else {
                info!("RBI: File upload support enabled");
            }
        }

        // Performance optimizations
        let mut dirty_tracker = RbiDirtyTracker::new();
        let mut adaptive_fps = AdaptiveFps::new(5, self.config.capture_fps);
        let mut scroll_detector = ScrollDetector::new();

        // Screencast mode (use if enabled in config, otherwise fall back to screenshots)
        let use_screencast = self.config.use_screencast.unwrap_or(false);

        if use_screencast {
            info!("RBI: Screencast mode enabled - H.264 video streaming");

            // Start screencast
            let screencast_config = ScreencastConfig::default();
            if let Err(e) = chrome_session
                .start_screencast(
                    screencast_config.format.as_str(),
                    screencast_config.quality,
                    screencast_config.max_width,
                    screencast_config.max_height,
                )
                .await
            {
                warn!(
                    "RBI: Failed to start screencast, falling back to screenshots: {}",
                    e
                );
                // Continue with screenshot mode
            } else {
                // Screencast started successfully
                let _screencast_processor = ScreencastProcessor::new(screencast_config);

                // TODO: Listen for screencast frame events
                // This requires chromiumoxide to expose Page.screencastFrame events
                // For now, we'll use screenshot mode as the implementation
                warn!("RBI: Screencast events not yet implemented in chromiumoxide, using screenshots");
            }
        } else {
            info!(
                "RBI: Screenshot mode - adaptive FPS (5-{}), dirty tracking",
                self.config.capture_fps
            );
        }

        // Main event loop
        let mut capture_interval =
            interval(Duration::from_millis(1000 / self.config.capture_fps as u64));

        // Clipboard polling interval (every 500ms)
        let mut clipboard_interval = interval(Duration::from_millis(500));

        // Resource monitoring interval (every 5 seconds)
        let mut resource_interval = interval(Duration::from_secs(5));
        let mut resource_check_count = 0u32;

        loop {
            tokio::select! {
                // Capture screenshots at adaptive FPS with dirty tracking
                _ = capture_interval.tick() => {
                    match chrome_session.capture_screenshot().await {
                        Ok(Some(screenshot)) => {
                            // Check if frame has changed (dirty tracking)
                            let frame_changed = dirty_tracker.has_changed(&screenshot);

                            if frame_changed {
                                // Send only changed frames
                                let msg = self.handle_screenshot(&screenshot)?;
                                if let Err(e) = to_client.send(msg).await {
                                    warn!("RBI: Failed to send screenshot: {}", e);
                                    break;
                                }
                            }

                            // Boost FPS if actively scrolling
                            if scroll_detector.is_scrolling() {
                                adaptive_fps.boost_fps();
                            }

                            // Update capture interval based on activity (adaptive FPS)
                            let new_interval = adaptive_fps.update(frame_changed);
                            capture_interval = interval(new_interval);

                            // Log FPS changes for monitoring
                            if frame_changed && adaptive_fps.is_active() {
                                debug!("RBI: Activity detected, FPS={}", adaptive_fps.current_fps());
                            } else if !frame_changed && adaptive_fps.is_idle() {
                                debug!("RBI: Static content, FPS={}", adaptive_fps.current_fps());
                            } else if scroll_detector.is_scrolling() {
                                debug!("RBI: Scrolling active, FPS={}", adaptive_fps.current_fps());
                            }
                        }
                        Ok(None) => {
                            // Not time to capture yet
                        }
                        Err(e) => {
                            warn!("RBI: Screenshot capture error: {}", e);
                        }
                    }
                }

                // Resource monitoring (every 5 seconds)
                _ = resource_interval.tick() => {
                    resource_check_count = resource_check_count.wrapping_add(1);

                    // Quick memory check
                    match chrome_session.check_resources(self.config.resource_limits.max_memory_mb).await {
                        Ok(false) => {
                            error!("RBI: Memory limit exceeded ({}MB max)", self.config.resource_limits.max_memory_mb);
                            break;
                        }
                        Err(e) => {
                            debug!("RBI: Resource check error: {}", e);
                        }
                        Ok(true) => {}
                    }

                    // Detailed metrics every 30 seconds (6 checks)
                    if resource_check_count.is_multiple_of(6) {
                        // Chrome performance metrics
                        match chrome_session.get_performance_metrics().await {
                            Ok(metrics) => {
                                info!(
                                    "RBI: Performance - heap={}MB, dom_nodes={}, resources={}",
                                    metrics.js_heap_used_mb,
                                    metrics.dom_node_count,
                                    metrics.resource_count
                                );

                                if metrics.is_heavy_page() {
                                    warn!(
                                        "RBI: Heavy page detected - {} DOM nodes, {} resources",
                                        metrics.dom_node_count,
                                        metrics.resource_count
                                    );
                                }
                            }
                            Err(e) => {
                                debug!("RBI: Performance metrics unavailable: {}", e);
                            }
                        }

                        // Optimization metrics
                        let dirty_stats = dirty_tracker.stats();
                        let fps_stats = adaptive_fps.stats();
                        let scroll_stats = scroll_detector.stats();

                        info!(
                            "RBI: Optimization stats - FPS={} ({}%), frames: captured={}, sent={}, skipped={} ({}% compression)",
                            fps_stats.current_fps,
                            (fps_stats.current_fps * 100) / fps_stats.max_fps,
                            dirty_stats.frames_captured,
                            dirty_stats.frames_sent,
                            dirty_stats.frames_skipped,
                            dirty_stats.compression_ratio as u32
                        );

                        if scroll_stats.scroll_events > 0 {
                            let (avg_x, avg_y) = scroll_stats.avg_distance_per_scroll();
                            info!(
                                "RBI: Scroll stats - events={}, avg_distance=({:.0}, {:.0})",
                                scroll_stats.scroll_events,
                                avg_x,
                                avg_y
                            );
                        }

                        // Reset stats for next period
                        dirty_tracker.reset_stats();
                        scroll_detector.reset_stats();
                    }
                }

                // Poll for clipboard changes (browser → client)
                _ = clipboard_interval.tick() => {
                    if !self.config.disable_copy {
                        match chrome_session.poll_clipboard().await {
                            Ok(Some(text)) => {
                                // Send clipboard to client
                                if let Some(instr) = self.clipboard.handle_browser_clipboard(
                                    text.as_bytes(), "text/plain"
                                ).ok().flatten() {
                                    if let Err(e) = to_client.send(instr).await {
                                        warn!("RBI: Failed to send clipboard: {}", e);
                                    }
                                }
                            }
                            Ok(None) => {
                                // No clipboard change
                            }
                            Err(e) => {
                                debug!("RBI: Clipboard poll error: {}", e);
                            }
                        }
                    }

                    // Also poll for cursor changes
                    match chrome_session.poll_cursor().await {
                        Ok(Some(cursor_type)) => {
                            // Send cursor instruction to client
                            if let Some(cursor_instr) = format_standard_cursor(cursor_type) {
                                if let Err(e) = to_client.send(Bytes::from(cursor_instr)).await {
                                    warn!("RBI: Failed to send cursor: {}", e);
                                }
                            }
                        }
                        Ok(None) => {
                            // No cursor change
                        }
                        Err(e) => {
                            debug!("RBI: Cursor poll error: {}", e);
                        }
                    }

                    // Poll for scroll changes
                    match chrome_session.poll_scroll().await {
                        Ok(Some(position)) => {
                            if let Some((delta_x, delta_y)) = scroll_detector.update(position) {
                                // Scroll detected - boost FPS and capture immediately
                                adaptive_fps.boost_fps();

                                // Determine scroll significance
                                let viewport_height = self.height as i32;
                                let is_significant = scroll_detector.is_significant_scroll(delta_y, viewport_height);
                                let is_page_scroll = scroll_detector.is_page_scroll(delta_y, viewport_height);
                                let velocity = scroll_detector.velocity();

                                debug!(
                                    "RBI: Scroll detected: delta=({}, {}), velocity={:.0}px/s, significant={}, page_scroll={}",
                                    delta_x, delta_y, velocity, is_significant, is_page_scroll
                                );

                                // Immediate frame capture for smooth scrolling
                                if is_significant {
                                    match chrome_session.capture_screenshot().await {
                                        Ok(Some(screenshot)) => {
                                            if dirty_tracker.has_changed(&screenshot) {
                                                let msg = self.handle_screenshot(&screenshot)?;
                                                if let Err(e) = to_client.send(msg).await {
                                                    warn!("RBI: Failed to send scroll frame: {}", e);
                                                } else {
                                                    debug!("RBI: Sent immediate scroll frame");
                                                }
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(e) => {
                                            debug!("RBI: Scroll frame capture error: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            // No scroll change
                        }
                        Err(e) => {
                            debug!("RBI: Scroll poll error: {}", e);
                        }
                    }

                    // Poll for file chooser (upload) requests
                    if self.upload_engine.manager().is_enabled() {
                        match chrome_session.poll_file_chooser().await {
                            Ok(Some(request)) => {
                                info!("RBI: File chooser opened - multiple={}", request.multiple);
                                // Send upload dialog request to client
                                let instr = format_upload_dialog_instruction(&request);
                                if let Err(e) = to_client.send(instr).await {
                                    warn!("RBI: Failed to send upload dialog: {}", e);
                                }
                                // Track the pending request
                                self.upload_engine.manager_mut().handle_dialog_request(
                                    request.multiple,
                                    request.accept,
                                );
                            }
                            Ok(None) => {}
                            Err(e) => {
                                debug!("RBI: File chooser poll error: {}", e);
                            }
                        }
                    }

                    // Check for dialog timeouts
                    if let Some(response) = self.dialog_manager.check_timeout() {
                        debug!("RBI: Dialog timed out - id={}", response.id);
                        // Dialog was auto-dismissed, no need to send anything
                    }
                }

                // Handle client input
                Some(msg) = from_client.recv() => {
                    if let Err(e) = self.handle_client_input(&mut chrome_session, &msg, &to_client).await {
                        warn!("RBI: Error handling input: {}", e);
                    }
                }

                else => {
                    debug!("RBI: Connection closed");
                    break;
                }
            }
        }

        info!("RBI: Chrome session ended");
        Ok(())
    }

    /// Handle client input (keyboard, mouse, touch, navigation, clipboard)
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
                        let pressed = pressed == 1;

                        // Use the new input handler for proper state tracking
                        let _key_event = self.input_handler.handle_keyboard(keysym, pressed);

                        // Check for keyboard shortcuts (Ctrl+C, Ctrl+V, etc.)
                        if let Some(shortcut) = self.input_handler.check_shortcut(keysym, pressed) {
                            match shortcut {
                                KeyboardShortcut::Copy => {
                                    info!("RBI: Copy shortcut detected");
                                    // Browser handles copy internally
                                }
                                KeyboardShortcut::Paste => {
                                    info!("RBI: Paste shortcut detected");
                                    // Browser handles paste internally
                                }
                                KeyboardShortcut::Cut => {
                                    info!("RBI: Cut shortcut detected");
                                }
                                KeyboardShortcut::SelectAll => {
                                    info!("RBI: Select All shortcut detected");
                                }
                            }
                            // Let the browser handle the shortcut
                        }

                        // Inject the key event into browser
                        chrome_session.inject_keyboard(keysym, pressed).await?;
                    }
                }
            }
            "mouse" => {
                if instr.args.len() >= 3 {
                    if let (Ok(x), Ok(y), Ok(mask)) = (
                        instr.args[0].parse::<i32>(),
                        instr.args[1].parse::<i32>(),
                        instr.args[2].parse::<u32>(),
                    ) {
                        // Clamp coordinates
                        let x = x.max(0).min(self.width as i32 - 1);
                        let y = y.max(0).min(self.height as i32 - 1);

                        // Use the new input handler for proper button tracking
                        let mouse_event = self.input_handler.handle_mouse(x, y, mask);

                        // Handle button presses
                        for button in &mouse_event.buttons_pressed {
                            let button_num = match button {
                                crate::input::MouseButton::Left => 0,
                                crate::input::MouseButton::Middle => 1,
                                crate::input::MouseButton::Right => 2,
                            };
                            chrome_session.inject_mouse(x, y, button_num, true).await?;
                        }

                        // Handle button releases
                        for button in &mouse_event.buttons_released {
                            let button_num = match button {
                                crate::input::MouseButton::Left => 0,
                                crate::input::MouseButton::Middle => 1,
                                crate::input::MouseButton::Right => 2,
                            };
                            chrome_session.inject_mouse(x, y, button_num, false).await?;
                        }

                        // Handle scroll
                        if let Some(delta_y) = mouse_event.scroll_delta_y {
                            chrome_session.inject_scroll(x, y, 0, delta_y).await?;
                        }
                    }
                }
            }
            "touch" => {
                // Touch event: touch,<id>,<x>,<y>,<radius_x>,<radius_y>,<angle>,<force>;
                if instr.args.len() >= 7 {
                    if let (
                        Ok(id),
                        Ok(x),
                        Ok(y),
                        Ok(radius_x),
                        Ok(radius_y),
                        Ok(angle),
                        Ok(force),
                    ) = (
                        instr.args[0].parse::<i32>(),
                        instr.args[1].parse::<i32>(),
                        instr.args[2].parse::<i32>(),
                        instr.args[3].parse::<i32>(),
                        instr.args[4].parse::<i32>(),
                        instr.args[5].parse::<f64>(),
                        instr.args[6].parse::<f64>(),
                    ) {
                        if let Some(touch_event) = self
                            .input_handler
                            .handle_touch(id, x, y, radius_x, radius_y, angle, force)
                        {
                            chrome_session.inject_touch(&touch_event).await?;
                        }
                    }
                }
            }
            "size" => {
                if instr.args.len() >= 2 {
                    if let (Ok(w), Ok(h)) =
                        (instr.args[0].parse::<u32>(), instr.args[1].parse::<u32>())
                    {
                        info!("RBI: Resize requested: {}x{}", w, h);
                        self.width = w;
                        self.height = h;
                        chrome_session.resize(w, h).await?;
                    }
                }
            }
            "clipboard" => {
                // Clipboard instruction: clipboard,<mimetype>;
                // Followed by blob instructions with data
                if let Some(mimetype) = instr.args.first() {
                    debug!("RBI: Clipboard stream started, mimetype: {}", mimetype);
                    // Clipboard data will come in blob instructions
                }
            }
            "blob" => {
                // Blob instruction: blob,<stream_id>,<base64_data>;
                if instr.args.len() >= 2 {
                    let data = instr.args[1];
                    use base64::Engine;
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data) {
                        // Handle clipboard data
                        if let Some(browser_data) = self
                            .clipboard
                            .handle_client_clipboard(&decoded, "text/plain")?
                        {
                            chrome_session.set_clipboard(&browser_data).await?;
                        }
                    }
                }
            }
            "navigate" => {
                // Navigation: navigate,<position>;
                // position: -1 = back, 0 = refresh, 1 = forward
                if let Some(pos_str) = instr.args.first() {
                    if let Ok(position) = pos_str.parse::<i32>() {
                        if !self.config.allow_url_manipulation && position != 0 {
                            warn!("RBI: URL manipulation disabled, blocking navigation");
                        } else {
                            chrome_session.navigate_history(position).await?;
                        }
                    }
                }
            }
            "goto" => {
                // Go to URL: goto,<url>;
                if let Some(url) = instr.args.first() {
                    if !self.config.allow_url_manipulation {
                        warn!("RBI: URL manipulation disabled, blocking goto");
                    } else if !self.is_url_allowed(url) {
                        warn!("RBI: URL not in allowlist: {}", url);
                    } else {
                        chrome_session.navigate_to(url).await?;
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
            "file" => {
                // Start file upload: file,<stream_id>,<mimetype>,<filename>;
                if instr.args.len() >= 3 {
                    let stream_id = &instr.args[0];
                    let mimetype = &instr.args[1];
                    let filename = &instr.args[2];

                    // Size will come from ack or be determined from blob data
                    // For now, use 0 and track actual size from blobs
                    match self
                        .upload_engine
                        .start_upload(stream_id, filename, mimetype, 0)
                    {
                        Ok(upload_id) => {
                            info!("RBI: Upload started - id={}, file={}", upload_id, filename);
                        }
                        Err(e) => {
                            warn!("RBI: Upload rejected: {}", e);
                            // Send error ack
                            let ack = format!(
                                "3.ack,{}.{},6.UPLOAD,5.error;",
                                stream_id.len(),
                                stream_id
                            );
                            let _ = to_client.send(Bytes::from(ack)).await;
                        }
                    }
                }
            }
            "upload-blob" => {
                // Upload data chunk: upload-blob,<upload_id>,<base64_data>;
                if instr.args.len() >= 2 {
                    let upload_id = &instr.args[0];
                    let data = instr.args[1];

                    use base64::Engine;
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data) {
                        match self.upload_engine.handle_chunk(upload_id, &decoded) {
                            Ok(progress) => {
                                debug!(
                                    "RBI: Upload progress - id={}, {}%",
                                    upload_id, progress as u32
                                );
                            }
                            Err(e) => {
                                warn!("RBI: Upload chunk error: {}", e);
                            }
                        }
                    }
                }
            }
            "upload-end" => {
                // End file upload: upload-end,<upload_id>;
                if let Some(upload_id) = instr.args.first() {
                    match self.upload_engine.complete_upload(upload_id) {
                        Ok((info, data)) => {
                            info!(
                                "RBI: Upload complete - file={}, size={}",
                                info.filename,
                                data.len()
                            );

                            // Submit file to browser
                            let file_data =
                                vec![(info.filename.clone(), info.mimetype.clone(), data)];
                            if let Err(e) = chrome_session.submit_upload_files(&file_data).await {
                                warn!("RBI: Failed to submit upload to browser: {}", e);
                            }
                        }
                        Err(e) => {
                            warn!("RBI: Upload completion error: {}", e);
                        }
                    }
                }
            }
            "upload-cancel" => {
                // Cancel file upload: upload-cancel,<upload_id>;
                if let Some(upload_id) = instr.args.first() {
                    if let Err(e) = self.upload_engine.cancel_upload(upload_id) {
                        warn!("RBI: Upload cancel error: {}", e);
                    } else {
                        info!("RBI: Upload cancelled - id={}", upload_id);
                    }
                }
            }
            "dialog-response" => {
                // Response to JS dialog: dialog-response,<id>,<confirmed>,<input>;
                if instr.args.len() >= 2 {
                    let id = instr.args[0].to_string();
                    let confirmed = instr.args[1] == "1" || instr.args[1] == "true";
                    let input = instr.args.get(2).map(|s| s.to_string());

                    let response = crate::js_dialog::JsDialogResponse {
                        id,
                        confirmed,
                        input,
                    };

                    if let Err(e) = self.dialog_manager.handle_response(response) {
                        warn!("RBI: Dialog response error: {}", e);
                    }
                }
            }
            _ => {
                debug!("RBI: Unknown instruction: {}", instr.opcode);
            }
        }

        Ok(())
    }

    /// Check if URL is allowed by patterns
    fn is_url_allowed(&self, url: &str) -> bool {
        if self.config.allowed_url_patterns.is_empty() {
            return true; // No restrictions
        }

        for pattern in &self.config.allowed_url_patterns {
            if let Some(suffix) = pattern.strip_prefix('*') {
                // Wildcard pattern - must match as domain suffix
                // e.g., "*.example.com" should match "sub.example.com" but not "malicious-example.com"
                if url.ends_with(suffix) || url.contains(&format!("/{}", suffix)) {
                    return true;
                }
            } else if url.contains(pattern) {
                return true;
            }
        }

        false
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
    /// Returns Guacamole instructions for the screenshot.
    /// Automatically detects format (JPEG or PNG) from image header.
    pub fn handle_screenshot(&mut self, screenshot: &[u8]) -> Result<Bytes, String> {
        // Convert screenshot to Bytes
        let screenshot_bytes = Bytes::from(screenshot.to_vec());

        // Detect image format from header
        // JPEG: starts with 0xFF 0xD8
        // PNG: starts with 0x89 0x50 0x4E 0x47
        let format = if screenshot.len() >= 2 && screenshot[0] == 0xFF && screenshot[1] == 0xD8 {
            1 // JPEG
        } else {
            0 // PNG (default)
        };

        // Format as binary protocol message (zero-copy)
        let msg = self.binary_encoder.encode_image(
            self.stream_id,
            0, // layer
            0, // x
            0, // y
            self.width as u16,
            self.height as u16,
            format,
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
