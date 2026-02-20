use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    // Connection utilities (timeout, keep-alive)
    connect_tcp_with_timeout,
    is_keyboard_event_allowed_readonly,
    is_mouse_event_allowed_readonly,
    parse_blob_instruction,
    parse_end_instruction,
    parse_pipe_instruction,
    pipe_blob_bytes,
    // Recording helpers
    record_client_input,
    send_and_record,
    // Session lifecycle
    send_bell,
    send_disconnect,
    send_error_best_effort,
    send_name,
    send_ready,
    // Cursor
    CursorManager,
    EventBasedHandler,
    EventCallback,
    HandlerError,
    // Security
    HandlerSecuritySettings,
    HandlerStats,
    HealthStatus,
    KeepAliveManager,
    MultiFormatRecorder,
    // Pipe streams (for native terminal display)
    PipeStreamManager,
    ProtocolHandler,
    // Recording
    RecordingConfig,
    StandardCursor,
    DEFAULT_KEEPALIVE_INTERVAL_SECS,
    PIPE_NAME_STDIN,
    PIPE_STREAM_STDOUT,
};
use guacr_protocol::{format_chunked_blobs, TextProtocolEncoder};
use guacr_terminal::{
    format_clipboard_instructions, handle_mouse_selection, mouse_event_to_x11_sequence,
    parse_clipboard_blob, parse_key_instruction, parse_mouse_instruction,
    x11_keysym_to_bytes_with_backspace, x11_keysym_to_kitty_sequence, DirtyTracker, ModifierState,
    MouseSelection, SelectionResult, TerminalConfig, TerminalEmulator, TerminalRenderer,
};
#[cfg(feature = "threat-detection")]
use log::error;
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[cfg(feature = "threat-detection")]
use guacr_threat_detection::{ThreatDetector, ThreatDetectorConfig};

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

        // Parse security settings
        let security = HandlerSecuritySettings::from_params(&params);
        info!(
            "Telnet: Security settings - read_only={}, disable_copy={}, disable_paste={}",
            security.read_only, security.disable_copy, security.disable_paste
        );

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);
        if recording_config.is_enabled() {
            info!(
                "Telnet: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        // Parse terminal configuration (font, color-scheme, terminal-type, scrollback, backspace)
        let terminal_config = TerminalConfig::from_params(&params);
        info!(
            "Telnet: Terminal config - type={}, scrollback={}, color_scheme={}, backspace={}",
            terminal_config.terminal_type,
            terminal_config.scrollback_size,
            terminal_config.color_scheme.name(),
            terminal_config.backspace_code
        );

        // Extract connection parameters
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        // IMPORTANT: Always use DEFAULT size during initialization (like guacd does)
        // The client will send a resize instruction with actual browser dimensions after handshake
        // This matches guacd behavior and prevents "half screen" display issues
        //
        // Telnet uses fixed 19x38 cell dimensions (high DPI style)
        // Default: 1024x768 @ 96 DPI equivalent → ~53x20 chars
        const CHAR_W: u32 = 19;
        const CHAR_H: u32 = 38;

        info!(
            "Telnet: Using default handshake size (1024x768) - will resize after client connects"
        );
        let rows = self.config.default_rows;
        let cols = self.config.default_cols;
        let width_px = cols as u32 * CHAR_W;
        let height_px = rows as u32 * CHAR_H;
        let (rows, cols, width_px, height_px) = (rows, cols, width_px, height_px);

        info!(
            "Connecting to {}:{} (timeout: {}s)",
            hostname, port, security.connection_timeout_secs
        );

        // Connect via TCP with timeout (matches guacd behavior)
        let stream =
            connect_tcp_with_timeout((hostname.as_str(), port), security.connection_timeout_secs)
                .await?;

        info!("Telnet connection established");

        let (mut read_half, mut write_half) = stream.into_split();

        // Create terminal emulator with browser-requested dimensions and configured scrollback
        let mut terminal =
            TerminalEmulator::new_with_scrollback(rows, cols, terminal_config.scrollback_size);

        // Calculate font size from cell height (70% fits well)
        let font_size = (CHAR_H as f32) * 0.70;
        let renderer = TerminalRenderer::new_with_dimensions_and_scheme(
            CHAR_W,
            CHAR_H,
            font_size,
            terminal_config.color_scheme,
        )
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Zero-allocation protocol encoder (reused for all img instructions)
        let mut protocol_encoder = TextProtocolEncoder::new();

        // Store backspace code for key handling
        let backspace_code = terminal_config.backspace_code;

        // Dirty region tracker for optimization
        let mut dirty_tracker = DirtyTracker::new(rows, cols);

        // Initialize recording if enabled
        let mut recorder: Option<MultiFormatRecorder> = if recording_config.is_enabled() {
            match MultiFormatRecorder::new(&recording_config, &params, "telnet", cols, rows) {
                Ok(rec) => {
                    info!("Telnet: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("Telnet: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Use fixed stream ID for main display (reusing stream replaces content, not stacking)
        let stream_id: u32 = 1;

        // Initialize pipe stream manager for native terminal display support
        let mut pipe_manager = PipeStreamManager::new();

        // Check if pipe streams are enabled (connection parameter)
        let enable_pipe = params
            .get("enable-pipe")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        if enable_pipe {
            info!("Telnet: Pipe streams enabled - opening STDOUT pipe for native terminal display");
            let pipe_instr = pipe_manager.enable_stdout();
            send_and_record(&to_client, &mut recorder, Bytes::from(pipe_instr))
                .await
                .map_err(HandlerError::ChannelError)?;
        }

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
                    proactive_mode: params
                        .get("threat_detection_proactive_mode")
                        .map(|s| s == "true")
                        .unwrap_or(false),
                    approval_timeout_ms: params
                        .get("threat_detection_approval_timeout_ms")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(2000),
                    fail_closed_on_error: params
                        .get("threat_detection_fail_closed_on_error")
                        .map(|s| s == "true")
                        .unwrap_or(false),
                    show_approval_status: params
                        .get("threat_detection_show_approval_status")
                        .map(|s| s == "true")
                        .unwrap_or(true),
                    auto_approve_safe_commands: params
                        .get("threat_detection_auto_approve_safe_commands")
                        .map(|s| s == "true")
                        .unwrap_or(true),
                    config_allow_ai_session_terminate: params
                        .get("threat_detection_config_allow_ai_session_terminate")
                        .map(|s| s == "true")
                        .unwrap_or(true),
                    resource_ai_session_terminate_enabled: params
                        .get("threat_detection_resource_ai_session_terminate_enabled")
                        .map(|s| s == "true")
                        .unwrap_or(true),
                    level_terminate_flags: HashMap::new(),
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
        let session_id = uuid::Uuid::new_v4().to_string();
        #[cfg(not(feature = "threat-detection"))]
        let _session_id = String::new();
        #[cfg(feature = "threat-detection")]
        let hostname_for_threat = hostname.clone();
        #[cfg(feature = "threat-detection")]
        let username_for_threat = params.get("username").cloned().unwrap_or_default();

        // Send ready and name instructions
        send_ready(&to_client, "telnet-ready").await?;
        send_name(&to_client, "Telnet").await?;

        // Send I-beam cursor bitmap (standard text cursor for terminals)
        let mut cursor_manager = CursorManager::new(false, false, 85);
        debug!("Telnet: Sending IBeam cursor");
        let cursor_instrs = cursor_manager
            .send_standard_cursor(StandardCursor::IBeam)
            .map_err(|e| HandlerError::ProtocolError(format!("Cursor error: {}", e)))?;
        for instr in cursor_instrs {
            send_and_record(&to_client, &mut recorder, Bytes::from(instr))
                .await
                .map_err(HandlerError::ChannelError)?;
        }

        // CRITICAL: Send size instruction to initialize display dimensions
        // Browser needs this BEFORE any img/drawing operations!
        // Use character-aligned dimensions (not browser's exact request) to prevent scaling
        info!(
            "Telnet: Sending size instruction ({}x{} px = {}x{} chars)",
            width_px, height_px, cols, rows
        );
        let size_instr = TerminalRenderer::format_size_instruction(0, width_px, height_px);
        send_and_record(&to_client, &mut recorder, Bytes::from(size_instr))
            .await
            .map_err(HandlerError::ChannelError)?;

        // Bidirectional forwarding
        let mut buf = vec![0u8; 4096];
        let mut modifier_state = ModifierState::new();
        let mut mouse_selection = MouseSelection::new();

        // Clipboard storage
        // Store clipboard data received from client (via clipboard stream)
        // This data is pasted when user presses Ctrl+Shift+V
        let mut stored_clipboard = String::new();

        // Debounce timer for batching screen updates (16ms = 60fps)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Keep-alive manager (matches guacd's guac_socket_require_keep_alive behavior)
        let mut keepalive = KeepAliveManager::new(DEFAULT_KEEPALIVE_INTERVAL_SECS);
        let mut keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(
            DEFAULT_KEEPALIVE_INTERVAL_SECS,
        ));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Keep-alive ping to detect dead connections
                _ = keepalive_interval.tick() => {
                    if let Some(sync_instr) = keepalive.check() {
                        trace!("Telnet: Sending keep-alive sync");
                        if to_client.send(sync_instr).await.is_err() {
                            info!("Telnet: Client channel closed, ending session");
                            break;
                        }
                    }
                }
                // Telnet output -> Terminal -> Client
                result = read_half.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            info!("Telnet connection closed");

                            send_error_best_effort(&to_client, "Telnet connection closed by server", 517).await; // RESOURCE_CLOSED

                            break;
                        }
                        Ok(n) => {
                            // If STDOUT pipe is enabled, send raw data to client
                            // This enables native terminal display (with ANSI escape codes)
                            if pipe_manager.is_stdout_enabled() {
                                let blob = pipe_blob_bytes(PIPE_STREAM_STDOUT, &buf[..n]);
                                send_and_record(&to_client, &mut recorder, blob).await
                                    .map_err(HandlerError::ChannelError)?;
                            }

                            // Threat detection: Analyze live terminal output from server
                            #[cfg(feature = "threat-detection")]
                            if let Some(ref detector) = threat_detector {
                                match detector.analyze_terminal_output(&session_id, &buf[..n], &username_for_threat, &hostname_for_threat, "telnet").await {
                                    Ok(threat) => {
                                        if threat.should_terminate() {
                                            error!("Telnet: TERMINATING SESSION due to threat in terminal output: {}", threat.description);
                                            let msg = format!("Session terminated: {}", threat.description);
                                            send_error_best_effort(&to_client, &msg, 517).await; // RESOURCE_CLOSED
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Telnet: Threat detection error (non-fatal): {}", e);
                                    }
                                }
                            }

                            // Record output if recording is enabled
                            if let Some(ref mut rec) = recorder {
                                let _ = rec.record_output(&buf[..n]);
                            }

                            // Process terminal output (for image rendering)
                            terminal.process(&buf[..n])
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // Check for BEL character (0x07) and send audio beep to client
                            if buf[..n].contains(&0x07) {
                                debug!("Telnet: BEL detected, sending audio beep");
                                send_bell(&to_client, 100).await?;
                            }

                            // Dirty tracker updates automatically when find_dirty_region is called
                        }
                        Err(e) => {
                            warn!("Telnet read error: {}", e);

                            let error_msg = format!("Telnet connection error: {}", e);
                            send_error_best_effort(&to_client, &error_msg, 512).await; // UPSTREAM_ERROR

                            break;
                        }
                    }
                }

                // Client input -> Telnet
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Telnet: Client disconnected");
                        break;
                    };
                    // Record client-to-server instruction
                    record_client_input(&mut recorder, &msg);
                    // Parse Guacamole instruction
                    let msg_str = String::from_utf8_lossy(&msg);

                    if let Some(key_event) = parse_key_instruction(&msg_str) {
                        // Update modifier state if this is a modifier key
                        if modifier_state.update_modifier(key_event.keysym, key_event.pressed) {
                            // Don't send anything for modifier keys alone
                            continue;
                        }

                        // Security: Check read-only mode
                        if security.read_only
                            && !is_keyboard_event_allowed_readonly(key_event.keysym, modifier_state.control)
                        {
                            trace!("Telnet: Keyboard input blocked (read-only mode)");
                            continue;
                        }

                        // Handle paste shortcuts (matching guacd's behavior):
                        // - Ctrl+Shift+V (Linux/Windows): keysym 'V' (0x56) with ctrl+shift
                        // - Cmd+V (Mac): keysym 'v' (0x76) with meta
                        let is_paste = key_event.pressed && (
                            (key_event.keysym == 0x56 && modifier_state.control && modifier_state.shift) ||
                            (key_event.keysym == 0x76 && modifier_state.meta)
                        );

                        if is_paste {
                            // Security: Check if paste is allowed
                            if !security.is_paste_allowed() {
                                debug!("Telnet: Paste blocked (disabled or read-only mode)");
                                continue;
                            }

                            if stored_clipboard.is_empty() {
                                debug!("Telnet: Paste shortcut pressed but clipboard is empty");
                                continue;
                            }

                            // Check clipboard buffer size limit
                            let max_size = security.clipboard_buffer_size;
                            let paste_text = if stored_clipboard.len() > max_size {
                                warn!("Telnet: Clipboard truncated from {} to {} bytes", stored_clipboard.len(), max_size);
                                &stored_clipboard[..max_size]
                            } else {
                                &stored_clipboard
                            };

                            debug!("Telnet: Paste shortcut - Pasting {} chars from clipboard", paste_text.len());

                            // Send using bracketed paste mode for safety
                            let mut paste_data = Vec::new();
                            paste_data.extend_from_slice(b"\x1b[200~"); // Start bracketed paste
                            paste_data.extend_from_slice(paste_text.as_bytes());
                            paste_data.extend_from_slice(b"\x1b[201~"); // End bracketed paste

                            write_half.write_all(&paste_data[..]).await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // CRITICAL: Force dirty tracker reset after large paste
                            // Large pastes can cause dirty region tracking to fail, resulting in
                            // partial screen rendering (black screen with only small region visible)
                            // Reset forces next render to be full screen
                            if paste_text.len() > 100 {
                                debug!("Telnet: Large paste detected ({} chars), resetting dirty tracker to force full render", paste_text.len());
                                dirty_tracker = DirtyTracker::new(rows, cols);
                            }

                            continue; // Don't send the 'V' key itself
                        }

                        // Handle copy shortcuts - ignore them since selection already copies
                        // - Ctrl+Shift+C (Linux/Windows): keysym 'C' (0x43) with ctrl+shift
                        // - Cmd+C (Mac): keysym 'c' (0x63) with meta
                        let is_copy = key_event.pressed && (
                            (key_event.keysym == 0x43 && modifier_state.control && modifier_state.shift) ||
                            (key_event.keysym == 0x63 && modifier_state.meta)
                        );

                        if is_copy {
                            debug!("Telnet: Copy shortcut pressed - ignoring (selection already copies)");
                            continue;
                        }

                        // Check if Kitty keyboard protocol is enabled
                        let kitty_level = terminal.kitty_keyboard_level();

                        // Convert to terminal bytes with current modifier state and configured backspace
                        // Use Kitty keyboard protocol if enabled, otherwise use legacy
                        let bytes = if kitty_level > 0 {
                            trace!("Telnet: Using Kitty keyboard protocol level {}", kitty_level);
                            x11_keysym_to_kitty_sequence(
                                key_event.keysym,
                                key_event.pressed,
                                Some(&modifier_state),
                                backspace_code,
                                false, // Telnet doesn't use application cursor mode
                                kitty_level,
                            )
                        } else {
                            x11_keysym_to_bytes_with_backspace(
                                key_event.keysym,
                                key_event.pressed,
                                Some(&modifier_state),
                                backspace_code,
                            )
                        };
                        if !bytes.is_empty() {
                            // Threat detection: Analyze live keyboard input before sending to server
                            #[cfg(feature = "threat-detection")]
                            if let Some(ref detector) = threat_detector {
                                if let Ok(keystroke_sequence) = String::from_utf8(bytes.clone()) {
                                    match detector.analyze_keystroke_sequence(&session_id, &keystroke_sequence, &username_for_threat, &hostname_for_threat, "telnet").await {
                                        Ok(threat) => {
                                            if threat.should_terminate() {
                                                error!("Telnet: TERMINATING SESSION due to threat in keyboard input: {}", threat.description);
                                                let msg = format!("Session terminated: {}", threat.description);
                                                send_error_best_effort(&to_client, &msg, 517).await; // RESOURCE_CLOSED
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Telnet: Threat detection error (non-fatal): {}", e);
                                        }
                                    }
                                }
                            }

                            // Record input if enabled
                            if let Some(ref mut rec) = recorder {
                                if recording_config.recording_include_keys {
                                    let _ = rec.record_input(&bytes);
                                }
                            }

                            write_half.write_all(&bytes).await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        }
                    } else if msg_str.contains(".clipboard,") {
                        // Clipboard instruction received - this is just a SYNC, not a paste command!
                        // The Guacamole protocol sends clipboard instructions to sync clipboard state
                        // between client and server. This does NOT mean the user wants to paste.
                        debug!("Telnet: Clipboard stream opened - syncing clipboard state (not pasting)");
                    } else if let Some(clipboard_text) = parse_clipboard_blob(&msg_str) {
                        // Clipboard blob instruction: Store clipboard data from client
                        // This is just synchronization - NOT a paste command!
                        // The actual paste happens when user presses Ctrl+Shift+V (keysym 'V' with ctrl+shift)

                        stored_clipboard = clipboard_text;
                        debug!("Telnet: Clipboard updated - stored {} chars (waiting for Ctrl+Shift+V to paste)", stored_clipboard.len());
                    } else if let Some(mouse_event) = parse_mouse_instruction(&msg_str) {
                        // Security: Check read-only mode for mouse clicks
                        if security.read_only && !is_mouse_event_allowed_readonly(mouse_event.button_mask) {
                            trace!("Telnet: Mouse click blocked (read-only mode)");
                            continue;
                        }

                        // Handle mouse events intelligently:
                        // 1. If terminal has mouse mode enabled (vim/tmux) - send X11 sequences
                        // 2. Otherwise, left-click drag = text selection (copy to clipboard)
                        // 3. Hover with no buttons = ignored (prevents garbage)

                        // Check if terminal has mouse mode enabled (vim :set mouse=a, tmux mouse mode)
                        if terminal.is_mouse_enabled() && mouse_event.button_mask != 0 {
                            // Terminal wants mouse events - send X11 sequences
                            let mouse_seq = mouse_event_to_x11_sequence(
                                mouse_event.x_px,
                                mouse_event.y_px,
                                mouse_event.button_mask as u8,
                                CHAR_W,
                                CHAR_H
                            );

                            if !mouse_seq.is_empty() {
                                trace!("Telnet: Mouse X11 sequence (button={}) at ({}, {})",
                                    mouse_event.button_mask, mouse_event.x_px, mouse_event.y_px);
                                write_half.write_all(&mouse_seq).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                        // Try text selection (only when mouse mode is disabled)
                        else {
                            match handle_mouse_selection(
                                mouse_event,
                                &mut mouse_selection,
                                &terminal,
                                CHAR_W,
                                CHAR_H,
                                cols,
                                rows,
                                modifier_state.shift, // Pass shift key state for extend selection
                            ) {
                                SelectionResult::InProgress(overlay_instructions) => {
                                    // Send visual feedback (blue overlay) to client
                                    trace!("Telnet: Selection in progress, sending {} overlay instructions", overlay_instructions.len());
                                    for instr in overlay_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                                SelectionResult::Complete { text: selected_text, clear_instructions } => {
                                    // Security: Check if copy is allowed
                                    if !security.is_copy_allowed() {
                                        debug!("Telnet: Selection copy blocked (copy disabled)");

                                        // Still clear the overlay even if copy is blocked
                                        for instr in clear_instructions {
                                            send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                                .map_err(HandlerError::ChannelError)?;
                                        }
                                        continue;
                                    }

                                    debug!("Telnet: Selection complete, copying {} chars", selected_text.len());

                                    // CRITICAL: Update local clipboard immediately to avoid race condition
                                    // If user pastes immediately after selecting, they expect the selected text
                                    // Without this, there's a race where the clipboard blob from client arrives
                                    // after the user has already pressed Ctrl+Shift+V
                                    stored_clipboard = selected_text.clone();
                                    debug!("Telnet: Local clipboard updated immediately with {} chars", stored_clipboard.len());

                                    // Clear the overlay
                                    for instr in clear_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }

                                    // Send to client as clipboard
                                    let clipboard_stream_id = 10;
                                    let clipboard_instructions = format_clipboard_instructions(&selected_text, clipboard_stream_id);

                                    for instr in clipboard_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                                SelectionResult::None => {
                                    // No selection action (hovering, etc.) - ignore
                                }
                            }
                        }
                    } else if let Some(pipe_instr) = parse_pipe_instruction(&msg_str) {
                        // Handle incoming pipe stream (e.g., STDIN from client)
                        if pipe_instr.name == PIPE_NAME_STDIN {
                            debug!("Telnet: STDIN pipe opened by client (stream {})", pipe_instr.stream_id);
                            pipe_manager.register_incoming(
                                pipe_instr.stream_id,
                                &pipe_instr.name,
                                &pipe_instr.mimetype,
                            );
                        } else {
                            debug!("Telnet: Unknown pipe '{}' opened by client", pipe_instr.name);
                        }
                    } else if let Some(blob_instr) = parse_blob_instruction(&msg_str) {
                        // Handle blob data on STDIN pipe
                        if pipe_manager.is_stdin_stream(blob_instr.stream_id) {
                            // Security: Check if input is allowed
                            if security.read_only {
                                debug!("Telnet: STDIN pipe data blocked (read-only mode)");
                            } else {
                                debug!("Telnet: Received {} bytes on STDIN pipe", blob_instr.data.len());
                                write_half.write_all(&blob_instr.data).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                    } else if let Some(end_stream_id) = parse_end_instruction(&msg_str) {
                        // Handle end of pipe stream
                        if pipe_manager.is_stdin_stream(end_stream_id) {
                            debug!("Telnet: STDIN pipe closed by client");
                            pipe_manager.close(PIPE_NAME_STDIN);
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
                                    send_and_record(&to_client, &mut recorder, Bytes::from(copy_instr)).await
                                        .map_err(HandlerError::ChannelError)?;

                                    // Render only the new bottom line(s)
                                    // Expand by 1 cell in all directions for JPEG artifacts
                                    let r_min_row = dirty.min_row.saturating_sub(1);
                                    let r_max_row = (dirty.max_row + 1).min(rows - 1);
                                    let r_min_col = dirty.min_col.saturating_sub(1);
                                    let r_max_col = (dirty.max_col + 1).min(cols - 1);

                                    let (jpeg, x_px, y_px, _, _) = renderer.render_region(
                                        terminal.screen(),
                                        r_min_row,
                                        r_max_row,
                                        r_min_col,
                                        r_max_col,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    // Base64 encode and send via modern zero-allocation protocol
                                    let base64_data = base64::Engine::encode(
                                        &base64::engine::general_purpose::STANDARD,
                                        &jpeg,
                                    );

                                    let img_instr = protocol_encoder.format_img_instruction(
                                        stream_id, 0, x_px as i32, y_px as i32, "image/jpeg",
                                    );
                                    send_and_record(&to_client, &mut recorder, img_instr.freeze()).await
                                        .map_err(HandlerError::ChannelError)?;

                                    let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                    for instr in blob_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                } else {
                                    // Scroll down: render full screen
                                    let jpeg = renderer.render_screen(
                                        terminal.screen(),
                                        terminal.size().0,
                                        terminal.size().1,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    // Base64 encode and send via modern zero-allocation protocol
                                    let base64_data = base64::Engine::encode(
                                        &base64::engine::general_purpose::STANDARD,
                                        &jpeg,
                                    );

                                    let img_instr = protocol_encoder.format_img_instruction(
                                        stream_id, 0, 0, 0, "image/jpeg",
                                    );
                                    send_and_record(&to_client, &mut recorder, img_instr.freeze()).await
                                        .map_err(HandlerError::ChannelError)?;

                                    let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                    for instr in blob_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                            } else if dirty_pct < 30 {
                                // Partial update: render only dirty region
                                trace!("Telnet: Partial update: {}% dirty ({} cells)", dirty_pct, dirty_cells);

                                // Expand by 1 cell in all directions for JPEG artifacts
                                let r_min_row = dirty.min_row.saturating_sub(1);
                                let r_max_row = (dirty.max_row + 1).min(rows - 1);
                                let r_min_col = dirty.min_col.saturating_sub(1);
                                let r_max_col = (dirty.max_col + 1).min(cols - 1);

                                let (jpeg, x_px, y_px, _, _) = renderer.render_region(
                                    terminal.screen(),
                                    r_min_row,
                                    r_max_row,
                                    r_min_col,
                                    r_max_col,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                // Base64 encode and send via modern zero-allocation protocol
                                let base64_data = base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    &jpeg,
                                );

                                let img_instr = protocol_encoder.format_img_instruction(
                                    stream_id, 0, x_px as i32, y_px as i32, "image/jpeg",
                                );
                                send_and_record(&to_client, &mut recorder, img_instr.freeze()).await
                                    .map_err(HandlerError::ChannelError)?;

                                let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                for instr in blob_instructions {
                                    send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                        .map_err(HandlerError::ChannelError)?;
                                }
                            } else {
                                // Large update (>= 30% dirty): render full screen
                                trace!("Telnet: Full screen update: {}% dirty", dirty_pct);

                                let jpeg = renderer.render_screen(
                                    terminal.screen(),
                                    terminal.size().0,
                                    terminal.size().1,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                let base64_data = base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    &jpeg,
                                );

                                let img_instr = protocol_encoder.format_img_instruction(
                                    stream_id, 0, 0, 0, "image/jpeg",
                                );
                                send_and_record(&to_client, &mut recorder, img_instr.freeze()).await
                                    .map_err(HandlerError::ChannelError)?;

                                let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                for instr in blob_instructions {
                                    send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                        .map_err(HandlerError::ChannelError)?;
                                }
                            }
                        } else {
                            // Full screen update
                            let jpeg = renderer.render_screen(
                                terminal.screen(),
                                terminal.size().0,
                                terminal.size().1,
                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // Base64 encode and send via modern zero-allocation protocol
                            let base64_data = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &jpeg,
                            );

                            let img_instr = protocol_encoder.format_img_instruction(
                                stream_id, 0, 0, 0, "image/jpeg",
                            );
                            send_and_record(&to_client, &mut recorder, img_instr.freeze()).await
                                .map_err(HandlerError::ChannelError)?;

                            let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                            for instr in blob_instructions {
                                send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                    .map_err(HandlerError::ChannelError)?;
                            }
                        }

                        // Frame boundary marker
                        let sync_instr = renderer.format_sync_instruction(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64
                        );
                        send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr)).await
                            .map_err(HandlerError::ChannelError)?;

                        terminal.clear_dirty();
                    }
                }

                else => {
                    debug!("Telnet session ending");
                    break;
                }
            }
        }

        // Close any open pipe streams
        let end_instructions = pipe_manager.close_all();
        for instr in end_instructions {
            let _ = to_client.send(Bytes::from(instr)).await;
        }

        // Finalize recording
        if let Some(rec) = recorder {
            if let Err(e) = rec.finalize() {
                warn!("Telnet: Failed to finalize recording: {}", e);
            } else {
                info!("Telnet: Session recording finalized");
            }
        }

        send_disconnect(&to_client).await;
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
