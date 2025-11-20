use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use guacr_terminal::{
    mouse_event_to_x11_sequence, x11_keysym_to_bytes, DirtyTracker, ModifierState,
    TerminalEmulator, TerminalRenderer,
};
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[cfg(feature = "threat-detection")]
use guacr_threat_detection::{ThreatDetector, ThreatDetectorConfig};
#[cfg(feature = "threat-detection")]
use uuid;

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

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
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

        // Create terminal emulator with browser-requested dimensions and scrollback buffer
        let mut terminal = TerminalEmulator::new_with_scrollback(rows, cols, 150);
        let renderer =
            TerminalRenderer::new().map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Dirty region tracker for optimization
        let mut dirty_tracker = DirtyTracker::new(rows, cols);

        // Use fixed stream ID for main display (reusing stream replaces content, not stacking)
        let stream_id: u32 = 1;

        // Initialize threat detection if enabled
        #[cfg(feature = "threat-detection")]
        let threat_detector = {
            if let Some(baml_endpoint) = params.get("threat_detection_baml_endpoint") {
                let config = ThreatDetectorConfig {
                    baml_endpoint: baml_endpoint.clone(),
                    baml_api_key: params.get("threat_detection_baml_api_key").cloned(),
                    enabled: true,
                    auto_terminate: params
                        .get("threat_detection_auto_terminate")
                        .map(|s| s == "true")
                        .unwrap_or(true),
                    min_log_level: params
                        .get("threat_detection_min_log_level")
                        .and_then(|s| match s.as_str() {
                            "critical" => Some(guacr_threat_detection::ThreatLevel::Critical),
                            "high" => Some(guacr_threat_detection::ThreatLevel::High),
                            "medium" => Some(guacr_threat_detection::ThreatLevel::Medium),
                            "low" => Some(guacr_threat_detection::ThreatLevel::Low),
                            _ => None,
                        })
                        .unwrap_or(guacr_threat_detection::ThreatLevel::Low),
                    command_history_size: params
                        .get("threat_detection_command_history_size")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(10),
                    timeout_seconds: params
                        .get("threat_detection_timeout_seconds")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(5),
                    deny_tags: HashMap::new(), // TODO: Parse from params if needed
                    allow_tags: HashMap::new(),
                    enable_tag_checking: true,
                };

                match ThreatDetector::new(config) {
                    Ok(detector) => {
                        info!(
                            "Telnet: Threat detection enabled with BAML endpoint: {}",
                            baml_endpoint
                        );
                        Some(Arc::new(detector))
                    }
                    Err(e) => {
                        warn!("Telnet: Failed to initialize threat detection: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        };
        // Threat detection variables used when feature is enabled
        #[cfg(not(feature = "threat-detection"))]
        let _threat_detector: Option<()> = None;
        #[cfg(feature = "threat-detection")]
        let _session_id = uuid::Uuid::new_v4().to_string();
        #[cfg(not(feature = "threat-detection"))]
        let _session_id = String::new();
        #[cfg(feature = "threat-detection")]
        let _hostname_for_threat = hostname.clone();
        #[cfg(feature = "threat-detection")]
        let _username_for_threat = params.get("username").cloned().unwrap_or_default();

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
        let mut modifier_state = ModifierState::new();

        // Debounce timer for batching screen updates (16ms = 60fps)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
                            // Threat detection: Analyze live terminal output from server
                            #[cfg(feature = "threat-detection")]
                            if let Some(ref detector) = threat_detector {
                                match detector.analyze_terminal_output(&session_id, &buf[..n], &username_for_threat, &hostname_for_threat, "telnet").await {
                                    Ok(threat) => {
                                        if threat.should_terminate() {
                                            error!("Telnet: TERMINATING SESSION due to threat in terminal output: {}", threat.description);
                                            let error_msg = format!("error,0.Session terminated: {};", threat.description);
                                            to_client.send(Bytes::from(error_msg)).await
                                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Telnet: Threat detection error (non-fatal): {}", e);
                                    }
                                }
                            }

                            // Process terminal output
                            terminal.process(&buf[..n])
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // Dirty tracker updates automatically when find_dirty_region is called
                        }
                        Err(e) => {
                            warn!("Telnet read error: {}", e);
                            break;
                        }
                    }
                }

                // Client input -> Telnet
                Some(msg) = from_client.recv() => {
                    // Parse Guacamole instruction
                    let msg_str = String::from_utf8_lossy(&msg);

                    if msg_str.contains(".key,") {
                        // Parse and send key
                        if let Some(args_part) = msg_str.split_once(".key,") {
                            if let Some((first_arg, rest)) = args_part.1.split_once(',') {
                                if let Some((_, keysym_str)) = first_arg.split_once('.') {
                                    if let Ok(keysym) = keysym_str.parse::<u32>() {
                                        if let Some((_, pressed_val)) = rest.split_once('.') {
                                            let pressed = pressed_val.starts_with('1');

                                            // Update modifier state if this is a modifier key
                                            if modifier_state.update_modifier(keysym, pressed) {
                                                // Don't send anything for modifier keys alone
                                                continue;
                                            }

                                            // Convert to terminal bytes with current modifier state
                                            let bytes = x11_keysym_to_bytes(keysym, pressed, Some(&modifier_state));
                                            if !bytes.is_empty() {
                                                // Threat detection: Analyze live keyboard input before sending to server
                                                #[cfg(feature = "threat-detection")]
                                                if let Some(ref detector) = threat_detector {
                                                    if let Ok(keystroke_sequence) = String::from_utf8(bytes.clone()) {
                                                        match detector.analyze_keystroke_sequence(&session_id, &keystroke_sequence, &username_for_threat, &hostname_for_threat, "telnet").await {
                                                            Ok(threat) => {
                                                                if threat.should_terminate() {
                                                                    error!("Telnet: TERMINATING SESSION due to threat in keyboard input: {}", threat.description);
                                                                    let error_msg = format!("error,0.Session terminated: {};", threat.description);
                                                                    to_client.send(Bytes::from(error_msg)).await
                                                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                                                    break;
                                                                }
                                                            }
                                                            Err(e) => {
                                                                debug!("Telnet: Threat detection error (non-fatal): {}", e);
                                                            }
                                                        }
                                                    }
                                                }

                                                write_half.write_all(&bytes).await
                                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if msg_str.contains(".mouse,") {
                        // Handle mouse events for vim/tmux support (ported from SSH)
                        // Format: "mouse,<layer>,<x>,<y>,<button_mask>;"
                        if let Some(mouse_part) = msg_str.strip_prefix("mouse,") {
                            let parts: Vec<&str> = mouse_part.split(',').collect();
                            if parts.len() >= 4 {
                                if let (Some(x_part), Some(y_part), Some(button_part)) =
                                    (parts.get(1), parts.get(2), parts.get(3)) {

                                    if let (Some((_, x_str)), Some((_, y_str))) =
                                        (x_part.split_once('.'), y_part.split_once('.')) {

                                        let button_str = button_part.split(';').next().unwrap_or(button_part);
                                        if let Some((_, button_mask_str)) = button_str.split_once('.') {

                                            if let (Ok(x_px), Ok(y_px), Ok(button_mask)) =
                                                (x_str.parse::<u32>(), y_str.parse::<u32>(), button_mask_str.parse::<u8>()) {

                                                // Convert to X11 mouse escape sequence
                                                let mouse_seq = mouse_event_to_x11_sequence(
                                                    x_px, y_px, button_mask, CHAR_W, CHAR_H
                                                );

                                                if !mouse_seq.is_empty() {
                                                    trace!("Telnet: Mouse event at ({}, {}) px → ({}, {}) chars, button={}",
                                                        x_px, y_px, x_px / CHAR_W, y_px / CHAR_H, button_mask);
                                                    write_half.write_all(&mouse_seq).await
                                                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Debounce tick - render if screen changed (ported from SSH)
                _ = debounce.tick() => {
                    if terminal.is_dirty() {
                        // Find what changed (dirty region optimization)
                        if let Some(dirty) = dirty_tracker.find_dirty_region(terminal.screen()) {
                            let total_cells = (rows as usize) * (cols as usize);
                            let dirty_cells = dirty.cell_count();
                            let dirty_pct = (dirty_cells * 100) / total_cells;

                            // Check if this is a scroll operation
                            if let Some((scroll_dir, scroll_lines)) = dirty.is_scroll(rows, cols) {
                                if scroll_dir == 1 {
                                    // Scroll up: copy rows 1..N to rows 0..N-1, render new bottom line(s)
                                    trace!("Telnet: Scroll up {} lines (copy optimization)", scroll_lines);

                                    // Get character dimensions from renderer
                                    const CHAR_W: u32 = 19;
                                    const CHAR_H: u32 = 38;

                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        scroll_lines,  // src_row
                                        0,             // src_col
                                        cols,          // width_chars
                                        rows - scroll_lines, // height_chars
                                        0,             // dst_row
                                        0,             // dst_col
                                        CHAR_W,        // char_width
                                        CHAR_H,        // char_height
                                        0,             // layer
                                    );
                                    to_client.send(Bytes::from(copy_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // Render only the new bottom line(s)
                                    let (jpeg, _, _, _, _) = renderer.render_region(
                                        terminal.screen(),
                                        dirty.min_row,
                                        dirty.max_row,
                                        dirty.min_col,
                                        dirty.max_col,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    #[allow(deprecated)]
                                    let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);
                                    for instr in img_instructions {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }
                                } else {
                                    // Scroll down: render full screen
                                    let jpeg = renderer.render_screen(
                                        terminal.screen(),
                                        terminal.size().0,
                                        terminal.size().1,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    #[allow(deprecated)]
                                    let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);
                                    for instr in img_instructions {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }
                                }
                            } else {
                                // Partial update: render only dirty region
                                trace!("Telnet: Partial update: {}% dirty ({} cells)", dirty_pct, dirty_cells);

                                let (jpeg, _, _, _, _) = renderer.render_region(
                                    terminal.screen(),
                                    dirty.min_row,
                                    dirty.max_row,
                                    dirty.min_col,
                                    dirty.max_col,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                #[allow(deprecated)]
                                let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);
                                for instr in img_instructions {
                                    to_client.send(Bytes::from(instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                }
                            }
                        } else {
                            // Full screen update
                            let jpeg = renderer.render_screen(
                                terminal.screen(),
                                terminal.size().0,
                                terminal.size().1,
                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            #[allow(deprecated)]
                            let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);
                            for instr in img_instructions {
                                to_client.send(Bytes::from(instr)).await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }
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

// Event-based handler implementation for zero-copy integration
#[async_trait]
impl EventBasedHandler for TelnetHandler {
    fn name(&self) -> &str {
        "telnet"
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
    fn test_telnet_handler_new() {
        let handler = TelnetHandler::with_defaults();
        assert_eq!(<TelnetHandler as ProtocolHandler>::name(&handler), "telnet");
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
