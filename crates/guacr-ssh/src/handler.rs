use async_trait::async_trait;
#[allow(unused_imports)] // Engine trait needed for OSC52 .decode() method
use base64::Engine;
use bytes::Bytes;
use guacr_handlers::{
    is_keyboard_event_allowed_readonly,
    is_mouse_event_allowed_readonly,
    parse_blob_instruction,
    parse_end_instruction,
    parse_pipe_instruction,
    pipe_blob_bytes,
    EventBasedHandler,
    EventCallback,
    HandlerError,
    // Security
    HandlerSecuritySettings,
    HandlerStats,
    HealthStatus,
    // Connection utilities (timeout, keep-alive)
    KeepAliveManager,
    MultiFormatRecorder,
    // Pipe streams (for native terminal display)
    PipeStreamManager,
    ProtocolHandler,
    // Recording
    RecordingConfig,
    DEFAULT_KEEPALIVE_INTERVAL_SECS,
    PIPE_NAME_STDIN,
    PIPE_STREAM_STDOUT,
};
use guacr_terminal::{
    format_clipboard_instructions, handle_mouse_selection, parse_clipboard_blob,
    parse_key_instruction, parse_mouse_instruction, x11_keysym_to_bytes, DirtyTracker,
    ModifierState, MouseSelection, TerminalEmulator, TerminalRenderer,
};
use log::{debug, error, info, trace, warn};
use russh::client;
use russh_keys::key;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// SSH protocol handler
///
/// Connects to SSH servers and provides terminal access via the Guacamole protocol.
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
pub struct SshHandler {
    config: SshConfig,
}

#[derive(Debug, Clone)]
pub struct SshConfig {
    pub default_port: u16,
    pub default_rows: u16,
    pub default_cols: u16,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            default_port: 22,
            default_rows: 24,
            default_cols: 80,
        }
    }
}

impl SshHandler {
    pub fn new(config: SshConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(SshConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for SshHandler {
    fn name(&self) -> &str {
        "ssh"
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
        info!("SSH handler starting connection");

        // Parse security settings
        let security = HandlerSecuritySettings::from_params(&params);
        info!(
            "SSH: Security settings - read_only={}, disable_copy={}, disable_paste={}",
            security.read_only, security.disable_copy, security.disable_paste
        );

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);
        if recording_config.is_enabled() {
            info!(
                "SSH: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        // Extract connection parameters
        let hostname = params.get("hostname").ok_or_else(|| {
            error!("SSH handler: Missing hostname parameter");
            HandlerError::MissingParameter("hostname".to_string())
        })?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let username = params.get("username").ok_or_else(|| {
            error!("SSH handler: Missing username parameter");
            HandlerError::MissingParameter("username".to_string())
        })?;

        let password = params.get("password");
        let private_key = params.get("private_key");

        // IMPORTANT: Always use DEFAULT size during initialization (like guacd does)
        // The client will send a resize instruction with actual browser dimensions after handshake
        // This prevents the "half screen" issue where we use browser size too early
        //
        // Match guacd's handshake behavior: start with 1024x768 @ 96 DPI
        // DPI 96 → 9x18 char cells → 113x42 terminal → 1017x756 aligned
        // The resize handler (lines 681-723) will adjust to actual browser size
        info!("SSH: Using default handshake size (1024x768 @ 96 DPI) - will resize after client connects");
        let char_width = 9_u32;
        let char_height = 18_u32;
        let cols = (1024 / char_width).clamp(20, 500) as u16; // 113 cols
        let rows = (768 / char_height).clamp(10, 200) as u16; // 42 rows
        let width_px = cols as u32 * char_width; // 1017 px (aligned)
        let height_px = rows as u32 * char_height; // 756 px (aligned)
        let (rows, cols, width_px, height_px, char_width, char_height) =
            (rows, cols, width_px, height_px, char_width, char_height);

        info!(
            "SSH handler: Connecting to {}@{}:{} (timeout: {}s)",
            username, hostname, port, security.connection_timeout_secs
        );

        // Create SSH config
        let ssh_config = client::Config::default();

        // Connect with timeout (matches guacd's timeout parameter)
        let connection_timeout = Duration::from_secs(security.connection_timeout_secs);
        let mut sh = tokio::time::timeout(
            connection_timeout,
            client::connect(
                Arc::new(ssh_config),
                (hostname.as_str(), port),
                SshClientHandler,
            ),
        )
        .await
        .map_err(|_| {
            error!(
                "SSH handler: Connection timed out after {} seconds",
                security.connection_timeout_secs
            );
            HandlerError::ConnectionFailed(format!(
                "Connection timed out after {} seconds",
                security.connection_timeout_secs
            ))
        })?
        .map_err(|e| {
            error!("SSH handler: Connection failed: {}", e);
            HandlerError::ConnectionFailed(e.to_string())
        })?;

        debug!("SSH handler: Connected to SSH server, starting authentication");

        // Authenticate with timeout (same timeout as connection)
        let auth_timeout = Duration::from_secs(security.connection_timeout_secs);
        let auth_result = if let Some(pwd) = password {
            debug!("SSH handler: Authenticating with password");
            tokio::time::timeout(auth_timeout, sh.authenticate_password(username, pwd))
                .await
                .map_err(|_| {
                    error!("SSH handler: Authentication timed out");
                    HandlerError::AuthenticationFailed("Authentication timed out".to_string())
                })?
        } else if let Some(key_pem) = private_key {
            debug!("SSH handler: Authenticating with private key");

            // Parse private key (supports OpenSSH, PEM, and PKCS#8 formats)
            // russh_keys::decode_secret_key handles all formats and optional passphrase
            let passphrase = params.get("passphrase").map(|s| s.as_str());

            debug!(
                "SSH handler: Decoding private key (encrypted: {})",
                passphrase.is_some()
            );
            let key_pair = russh_keys::decode_secret_key(key_pem, passphrase).map_err(|e| {
                error!("SSH handler: Failed to decode private key: {}", e);
                if passphrase.is_some() {
                    HandlerError::AuthenticationFailed(format!(
                        "Invalid private key or passphrase: {}",
                        e
                    ))
                } else {
                    HandlerError::AuthenticationFailed(format!("Invalid private key format: {}", e))
                }
            })?;

            debug!("SSH handler: Private key decoded successfully, authenticating");
            tokio::time::timeout(
                auth_timeout,
                sh.authenticate_publickey(username, Arc::new(key_pair)),
            )
            .await
            .map_err(|_| {
                error!("SSH handler: Authentication timed out");
                HandlerError::AuthenticationFailed("Authentication timed out".to_string())
            })?
        } else {
            error!("SSH handler: No authentication method provided");
            return Err(HandlerError::MissingParameter(
                "password or private_key".to_string(),
            ));
        };

        if !auth_result.map_err(|e| {
            error!("SSH handler: Authentication error: {}", e);
            HandlerError::AuthenticationFailed(e.to_string())
        })? {
            error!("SSH handler: Authentication failed - wrong credentials");
            return Err(HandlerError::AuthenticationFailed(
                "Authentication failed".to_string(),
            ));
        }

        info!("SSH handler: Authentication successful");

        // Open channel and request PTY
        let mut channel = sh
            .channel_open_session()
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        channel
            .request_pty(
                false, // want_reply
                "xterm-256color",
                cols as u32,
                rows as u32,
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        channel
            .request_shell(false)
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        debug!("SSH handler: SSH shell established");

        // Create terminal emulator with browser-requested dimensions
        let mut terminal = TerminalEmulator::new(rows, cols);

        // CRITICAL: Read any initial data (banner/MOTD) that arrived immediately after shell request
        // SSH servers often send welcome banners right away, before we enter the event loop
        // Use a 200ms timeout between packets - this gives slower servers time to generate content
        // while not blocking too long if no banner is sent
        debug!("SSH: Checking for initial banner data...");
        let mut banner_bytes_total = 0usize;
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(200), channel.wait()).await
            {
                Ok(Some(russh::ChannelMsg::Data { ref data })) => {
                    banner_bytes_total += data.len();
                    debug!(
                        "SSH: Received {} bytes of initial banner data (total: {})",
                        data.len(),
                        banner_bytes_total
                    );
                    terminal
                        .process(data)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                }
                Ok(Some(_other)) => {
                    // Other channel messages during startup (ignore for now)
                    debug!("SSH: Received non-data message during banner check");
                }
                Ok(None) => {
                    // Channel closed unexpectedly
                    debug!("SSH: Channel closed during banner check");
                    break;
                }
                Err(_timeout) => {
                    // No more data available - banner collection complete
                    debug!(
                        "SSH: Banner collection complete (timeout after 200ms, total {} bytes)",
                        banner_bytes_total
                    );
                    break;
                }
            }
        }

        // Check if we collected any banner data
        let banner_collected = terminal.is_dirty();
        debug!(
            "SSH: Banner collection finished, has_content={}",
            banner_collected
        );

        // Make rows/cols mutable for dynamic resizing (guacd-style)
        let mut current_rows = rows;
        let mut current_cols = cols;

        // Calculate font size as 70% of cell height (fits well with some padding)
        let font_size = (char_height as f32) * 0.70;

        info!(
            "SSH: Creating renderer with {}x{} px cells, {:.1}pt font",
            char_width, char_height, font_size
        );

        let renderer = TerminalRenderer::new_with_dimensions(char_width, char_height, font_size)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Dirty region tracker (guacd optimization - only send changed portions)
        let mut dirty_tracker = DirtyTracker::new(rows, cols);

        // Initialize recording if enabled
        let mut recorder: Option<MultiFormatRecorder> = if recording_config.is_enabled() {
            match MultiFormatRecorder::new(&recording_config, &params, "ssh", cols, rows) {
                Ok(rec) => {
                    info!("SSH: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("SSH: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Use fixed stream ID for main display (reusing stream replaces content, not stacking)
        let stream_id: u32 = 1;

        // Initialize pipe stream manager for native terminal display support
        // This enables CLI clients to receive raw terminal output (with ANSI codes)
        // instead of rendered images, allowing display in native terminal apps
        let mut pipe_manager = PipeStreamManager::new();

        // Check if pipe streams are enabled (connection parameter)
        let enable_pipe = params
            .get("enable-pipe")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        if enable_pipe {
            info!("SSH: Pipe streams enabled - opening STDOUT pipe for native terminal display");
            let pipe_instr = pipe_manager.enable_stdout();
            to_client
                .send(Bytes::from(pipe_instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        }

        // Send ready instruction first to signal the connection is established
        debug!("SSH: Sending ready instruction");
        let ready_instr = TerminalRenderer::format_ready_instruction("ssh-ready");
        to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // CRITICAL: Send size instruction to initialize display dimensions
        // Browser needs this BEFORE any img/drawing operations!
        // Use character-aligned dimensions (not browser's exact request) to prevent scaling
        debug!(
            "SSH: Sending size instruction ({}x{} px = {}x{} chars)",
            width_px, height_px, cols, rows
        );
        let size_instr = TerminalRenderer::format_size_instruction(0, width_px, height_px);
        to_client
            .send(Bytes::from(size_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        debug!("SSH: Display initialized");

        // If we collected banner data, render it immediately (don't wait for debounce)
        if banner_collected {
            debug!("SSH: Rendering collected banner immediately");
            let jpeg = renderer
                .render_screen(terminal.screen(), terminal.size().0, terminal.size().1)
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

            debug!("SSH: Banner render produced {} byte JPEG", jpeg.len());

            #[allow(deprecated)]
            let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);

            for instr in img_instructions {
                to_client
                    .send(Bytes::from(instr))
                    .await
                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
            }

            let sync_instr = renderer.format_sync_instruction(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            );
            to_client
                .send(Bytes::from(sync_instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

            terminal.clear_dirty();
            debug!("SSH: Banner rendered and sent to client");
        }

        // Debounce timer for batching screen updates
        // 16ms = 60fps for smooth interactive feel (was 100ms)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Force initial render after 100ms to catch SSH prompt
        // SSH servers often send prompt immediately, but it might arrive after we initialize
        // Box::pin is required because tokio::time::Sleep is !Unpin
        let mut initial_render_timer =
            Box::pin(tokio::time::sleep(std::time::Duration::from_millis(100)));
        let mut initial_render_done = false;

        // Track modifier key state (Ctrl, Shift, Alt) for Ctrl+C, etc.
        let mut modifier_state = ModifierState::new();

        // Mouse selection tracking
        let mut mouse_selection = MouseSelection::new();

        // Keep-alive manager (matches guacd's guac_socket_require_keep_alive behavior)
        let mut keepalive = KeepAliveManager::new(DEFAULT_KEEPALIVE_INTERVAL_SECS);
        let mut keepalive_interval =
            tokio::time::interval(Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Bidirectional forwarding
        loop {
            tokio::select! {
                // Use FAIR (non-biased) select to ensure all arms get polled
                // Biased mode was causing debounce tick to never fire after initial render
                // Fair mode gives each ready arm an equal chance, preventing starvation

                // Keep-alive ping to detect dead connections
                _ = keepalive_interval.tick() => {
                    if let Some(sync_instr) = keepalive.check() {
                        trace!("SSH: Sending keep-alive sync");
                        if to_client.send(sync_instr).await.is_err() {
                            info!("SSH: Client channel closed, ending session");
                            break;
                        }
                    }
                }

                // Initial render timer - catch SSH prompt that arrives after initialization
                _ = initial_render_timer.as_mut(), if !initial_render_done => {
                    debug!("SSH: Initial render timer fired (is_dirty={})", terminal.is_dirty());
                    initial_render_done = true;

                    // Only render if terminal has content (is_dirty means data was processed)
                    if terminal.is_dirty() {
                        debug!("SSH: Rendering initial screen");

                        // Force a full screen render to catch any SSH prompt that arrived
                        let jpeg = renderer.render_screen(
                            terminal.screen(),
                            terminal.size().0,
                            terminal.size().1,
                        ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                        debug!("SSH: Initial render produced {} byte JPEG", jpeg.len());

                        #[allow(deprecated)]
                        let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);

                        for instr in img_instructions {
                            to_client.send(Bytes::from(instr)).await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }

                        let sync_instr = renderer.format_sync_instruction(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64
                        );
                        to_client.send(Bytes::from(sync_instr)).await
                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                        terminal.clear_dirty();
                    } else {
                        debug!("SSH: Initial render timer fired but terminal not dirty, skipping");
                    }
                }

                // Debounce tick - render if screen changed
                // IMPORTANT: This MUST come before input arms in biased select to ensure
                // rendering has priority over input processing (prevents "one char behind")
                _ = debounce.tick() => {
                    if terminal.is_dirty() {
                        // Find what changed (dirty region optimization like guacd)
                        let dirty_opt = dirty_tracker.find_dirty_region(terminal.screen());

                        if let Some(dirty) = dirty_opt {
                            let total_cells = (current_rows as usize) * (current_cols as usize);
                            let dirty_cells = dirty.cell_count();
                            let dirty_pct = (dirty_cells * 100) / total_cells;

                            // Check if this is a scroll operation (most common case)
                            if let Some((scroll_dir, scroll_lines)) = dirty.is_scroll(current_rows, current_cols) {
                                if scroll_dir == 1 {
                                    // Scroll up: copy rows 1..N to rows 0..N-1, render new bottom line(s)
                                    trace!("SSH: Scroll up {} lines (copy optimization)", scroll_lines);

                                    // Send copy instruction to shift existing content up
                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        scroll_lines,  // src_row: start from line N
                                        0,             // src_col: from left edge
                                        current_cols,  // width: full width
                                        current_rows - scroll_lines, // height: all except scrolled lines
                                        0,             // dst_row: to top
                                        0,             // dst_col: to left edge
                                        char_width,    // char_width: dynamic cell width
                                        char_height,   // char_height: dynamic cell height
                                        0,             // layer: default layer
                                    );
                                    to_client.send(Bytes::from(copy_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // Render only the new line(s) at the bottom
                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region(
                                        terminal.screen(),
                                        current_rows - scroll_lines,  // Start at line where new content begins
                                        current_rows,                 // End at bottom
                                        0,                            // Full width
                                        current_cols,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    #[allow(deprecated)]
                                    let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, x_px as i32, y_px as i32);

                                    for instr in img_instructions {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }
                                } else {
                                    // Scroll down: copy rows 0..N-1 to rows 1..N, render new top line(s)
                                    trace!("SSH: Scroll down {} lines (copy optimization)", scroll_lines);

                                    // Send copy instruction to shift existing content down
                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        0,             // src_row: start from top
                                        0,             // src_col: from left edge
                                        current_cols,  // width: full width
                                        current_rows - scroll_lines, // height: all except scrolled lines
                                        scroll_lines,  // dst_row: move down by N lines
                                        0,             // dst_col: to left edge
                                        char_width,    // char_width: dynamic cell width
                                        char_height,   // char_height: dynamic cell height
                                        0,             // layer: default layer
                                    );
                                    to_client.send(Bytes::from(copy_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // Render only the new line(s) at the top
                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region(
                                        terminal.screen(),
                                        0,              // Start at top
                                        scroll_lines,   // End at line N
                                        0,              // Full width
                                        current_cols,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    #[allow(deprecated)]
                                    let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, x_px as i32, y_px as i32);

                                    for instr in img_instructions {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }
                                }
                            } else {
                                // Not a scroll - use normal dirty region rendering
                                if dirty_pct < 30 {
                                    trace!("SSH: Dirty region: {}x{} cells ({}%)", dirty.width(), dirty.height(), dirty_pct);

                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region(
                                        terminal.screen(),
                                        dirty.min_row,
                                        dirty.max_row,
                                        dirty.min_col,
                                        dirty.max_col,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    #[allow(deprecated)]
                                    let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, x_px as i32, y_px as i32);

                                    for instr in img_instructions {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }
                                } else {
                                    trace!("SSH: Full screen ({}% dirty)", dirty_pct);

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
                            }

                            let sync_instr = renderer.format_sync_instruction(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64
                            );
                            to_client.send(Bytes::from(sync_instr)).await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        } else {
                            // Dirty tracker failed to find changes (cursor movement, small updates)
                            // Fall back to full screen render to prevent "one char behind" bug
                            debug!("SSH: Dirty tracker returned None, rendering full screen as fallback");

                            let jpeg = renderer.render_screen(
                                terminal.screen(),
                                terminal.size().0,
                                terminal.size().1,
                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            debug!("SSH: Fallback render produced {} byte JPEG", jpeg.len());

                            #[allow(deprecated)]
                            let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);

                            for instr in img_instructions {
                                to_client.send(Bytes::from(instr)).await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }

                            let sync_instr = renderer.format_sync_instruction(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64
                            );
                            to_client.send(Bytes::from(sync_instr)).await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }

                        terminal.clear_dirty();
                        debug!("SSH: Render complete, cleared dirty flag");
                    }
                }

                // SSH channel messages -> Terminal -> Client
                Some(msg) = channel.wait() => {
                    match msg {
                        russh::ChannelMsg::Data { ref data } => {
                            debug!("SSH: Received {} bytes from SSH server", data.len());

                            // If STDOUT pipe is enabled, send raw data to client
                            // This enables native terminal display (with ANSI escape codes)
                            if pipe_manager.is_stdout_enabled() {
                                let blob = pipe_blob_bytes(PIPE_STREAM_STDOUT, data);
                                to_client.send(blob).await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }

                            // Check for OSC 52 clipboard sequences before processing
                            // Format: ESC ] 52 ; c ; base64_data ESC \ or BEL
                            // This is what tmux/vim use to copy to clipboard
                            if let Some(clipboard_data) = extract_osc52_clipboard(data) {
                                // Security: Check if copy is allowed
                                if !security.is_copy_allowed() {
                                    debug!("SSH: OSC 52 clipboard copy blocked (copy disabled)");
                                } else {
                                    debug!("SSH: Detected OSC 52 clipboard copy ({} bytes)", clipboard_data.len());

                                    // Send clipboard using Guacamole stream protocol
                                    // 1. Allocate stream with clipboard instruction
                                    let clipboard_stream_id = 1;  // Use stream 1 for clipboard
                                    let clipboard_instr = format!(
                                        "9.clipboard,1.{},10.text/plain;",
                                        clipboard_stream_id
                                    );
                                    to_client.send(Bytes::from(clipboard_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // 2. Send data as blob on the stream
                                    let blob_instr = format!(
                                        "4.blob,1.{},{}.{};",
                                        clipboard_stream_id,
                                        clipboard_data.len(),
                                        clipboard_data
                                    );
                                    to_client.send(Bytes::from(blob_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // 3. Close the stream
                                    let end_instr = format!("3.end,1.{};", clipboard_stream_id);
                                    to_client.send(Bytes::from(end_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                }
                            }

                            // Record output if recording is enabled
                            if let Some(ref mut rec) = recorder {
                                let _ = rec.record_output(data);
                            }

                            // Process terminal output (for image rendering)
                            // When pipe is enabled with INTERPRET_OUTPUT, we still render
                            terminal.process(data)
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // Don't render immediately - let debounce batch updates
                        }
                        russh::ChannelMsg::ExitStatus { exit_status } => {
                            error!("SSH handler: SSH command exited with status: {}", exit_status);
                            break;
                        }
                        russh::ChannelMsg::Eof => {
                            error!("SSH handler: SSH channel EOF");
                            break;
                        }
                        other => {
                            error!("SSH handler: Received other channel message: {:?}", other);
                        }
                    }
                }

                // Client input -> SSH
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        error!("SSH handler: Client disconnected");
                        break;
                    };

                    // Parse Guacamole instruction
                    let msg_str = String::from_utf8_lossy(&msg);
                    debug!("SSH: Received client message: {}", msg_str);

                    if let Some(key_event) = parse_key_instruction(&msg_str) {
                        debug!("SSH: Key event - keysym={} (0x{:04X}), pressed={}, ctrl={}, shift={}, alt={}",
                            key_event.keysym, key_event.keysym, key_event.pressed,
                            modifier_state.control, modifier_state.shift, modifier_state.alt);

                        // Update modifier state (Ctrl, Shift, Alt)
                        // Returns true if this was a modifier key (don't send to SSH)
                        if modifier_state.update_modifier(key_event.keysym, key_event.pressed) {
                            debug!("SSH: Modifier key updated - ctrl={}, shift={}, alt={}",
                                modifier_state.control, modifier_state.shift, modifier_state.alt);
                            continue;
                        }

                        // Security: Check read-only mode
                        // In read-only mode, only allow Ctrl+C (copy) and similar selection keys
                        if security.read_only
                            && !is_keyboard_event_allowed_readonly(key_event.keysym, modifier_state.control)
                        {
                            trace!("SSH: Keyboard input blocked (read-only mode)");
                            continue;
                        }

                        // Convert to terminal bytes with modifier state
                        // This enables Ctrl+C (0x03), Ctrl+D (0x04), etc.
                        // Let the remote terminal handle paste based on its OS/config
                        let bytes = x11_keysym_to_bytes(key_event.keysym, key_event.pressed, Some(&modifier_state));
                        debug!("SSH: Key converted to {} bytes: {:?}", bytes.len(), bytes);
                        if !bytes.is_empty() {
                            // Record input if enabled
                            if let Some(ref mut rec) = recorder {
                                if recording_config.recording_include_keys {
                                    let _ = rec.record_input(&bytes);
                                }
                            }

                            channel.data(&bytes[..]).await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        }
                    } else if msg_str.contains(".clipboard,") {
                        // Clipboard instruction received - the actual data comes in a blob message
                        debug!("SSH: Clipboard stream opened - data incoming");
                    } else if let Some(clipboard_text) = parse_clipboard_blob(&msg_str) {
                        // Security: Check if paste is allowed
                        if !security.is_paste_allowed() {
                            debug!("SSH: Paste blocked (disabled or read-only mode)");
                            continue;
                        }

                        // Check clipboard buffer size limit
                        let max_size = security.clipboard_buffer_size;
                        let paste_text = if clipboard_text.len() > max_size {
                            warn!("SSH: Clipboard truncated from {} to {} bytes", clipboard_text.len(), max_size);
                            &clipboard_text[..max_size]
                        } else {
                            &clipboard_text
                        };

                        // Handle blob data (clipboard data from client)
                        // When user presses Ctrl+V (or Cmd+V), browser sends clipboard as blob
                        // Paste immediately to match target terminal's paste behavior
                        debug!("SSH: Pasting {} chars from clipboard", paste_text.len());

                        // Send using bracketed paste mode for safety
                        // This prevents commands from auto-executing
                        let mut paste_data = Vec::new();
                        paste_data.extend_from_slice(b"\x1b[200~"); // Start bracketed paste
                        paste_data.extend_from_slice(paste_text.as_bytes());
                        paste_data.extend_from_slice(b"\x1b[201~"); // End bracketed paste

                        channel.data(&paste_data[..]).await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    } else if msg_str.contains(".size,") {
                        // Handle resize - extract exact pixel dimensions from browser
                        // Format: "4.size,4.1057,3.768;" where args are: width, height
                        // After ".size,": "4.1057,3.768;"
                        if let Some(args_part) = msg_str.split_once(".size,") {
                            // Split by comma to get: ["4.1057", "3.768;"]
                            let parts: Vec<&str> = args_part.1.split(',').collect();
                            if parts.len() >= 2 {
                                // Parse width: "4.1057" -> extract "1057"
                                if let Some((_, width_str)) = parts[0].split_once('.') {
                                    // Parse height: "3.768;" -> extract "768" (remove trailing ;)
                                    let height_part = parts[1].trim_end_matches(';');
                                    if let Some((_, height_str)) = height_part.split_once('.') {
                                        if let (Ok(new_width_px), Ok(new_height_px)) =
                                            (width_str.parse::<u32>(), height_str.parse::<u32>()) {

                                            // Calculate new rows/cols using FIXED cell dimensions (guacd-style)
                                            let new_cols = (new_width_px / char_width).clamp(20, 500) as u16;
                                            let new_rows = (new_height_px / char_height).clamp(10, 200) as u16;

                                            // CRITICAL: Recalculate pixel dimensions to align with character grid
                                            let aligned_width = new_cols as u32 * char_width;
                                            let aligned_height = new_rows as u32 * char_height;

                                            // Resize terminal emulator
                                            terminal.resize(new_rows, new_cols);

                                            // Update current dimensions
                                            current_rows = new_rows;
                                            current_cols = new_cols;

                                            // Recreate dirty tracker for new dimensions
                                            dirty_tracker = DirtyTracker::new(new_rows, new_cols);

                                            // Send PTY window change to SSH server
                                            channel.window_change(new_cols as u32, new_rows as u32, 0, 0)
                                                .await.map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                            info!("SSH: Resize {}x{} px → {}x{} chars @ {}x{} px/cell → {}x{} px render",
                                                new_width_px, new_height_px, new_cols, new_rows,
                                                char_width, char_height, aligned_width, aligned_height);

                                            // Record resize event
                                            if let Some(ref mut rec) = recorder {
                                                let _ = rec.record_resize(new_cols, new_rows);
                                            }

                                            // Send size instruction to client
                                            let size_instr = TerminalRenderer::format_size_instruction(0, aligned_width, aligned_height);
                                            to_client.send(Bytes::from(size_instr)).await
                                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                            // Force full screen render after resize
                                            let jpeg = renderer.render_screen(
                                                terminal.screen(),
                                                new_rows,
                                                new_cols,
                                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                            #[allow(deprecated)]
                                            let img_instructions = renderer.format_img_instructions(&jpeg, stream_id, 0, 0, 0);
                                            for instr in img_instructions {
                                                to_client.send(Bytes::from(instr)).await
                                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                            }

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
                                }
                            }
                        }
                    } else if let Some(mouse_event) = parse_mouse_instruction(&msg_str) {
                        // Security: Check read-only mode for mouse clicks
                        if security.read_only && !is_mouse_event_allowed_readonly(mouse_event.button_mask) {
                            trace!("SSH: Mouse click blocked (read-only mode)");
                            continue;
                        }

                        // Handle mouse events intelligently:
                        // 1. Left-click drag = text selection (copy to clipboard)
                        // 2. Clicks/drags with buttons pressed = X11 sequences (for vim/tmux)
                        // 3. Hover with no buttons = ignored (prevents garbage)

                        // Try text selection first (left button drag)
                        if let Some(selected_text) = handle_mouse_selection(
                            mouse_event,
                            &mut mouse_selection,
                            &terminal,
                            char_width,
                            char_height,
                            current_cols,
                            current_rows,
                        ) {
                            // Security: Check if copy is allowed
                            if !security.is_copy_allowed() {
                                debug!("SSH: Selection copy blocked (copy disabled)");
                                continue;
                            }

                            debug!("SSH: Selection complete, copying {} chars", selected_text.len());

                            // Send to client as clipboard using shared formatter
                            let clipboard_stream_id = 10;
                            let clipboard_instructions = format_clipboard_instructions(&selected_text, clipboard_stream_id);

                            for instr in clipboard_instructions {
                                to_client.send(Bytes::from(instr)).await
                                    .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                            }
                        } else if mouse_event.button_mask != 0 {
                            // A button is pressed (but not a selection) - send to vim/tmux
                            // This enables vim :set mouse=a, tmux mouse mode, etc.
                            use guacr_terminal::mouse_event_to_x11_sequence;
                            let mouse_seq = mouse_event_to_x11_sequence(
                                mouse_event.x_px,
                                mouse_event.y_px,
                                mouse_event.button_mask as u8,
                                char_width,
                                char_height
                            );

                            if !mouse_seq.is_empty() {
                                trace!("SSH: Mouse X11 sequence (button={}) at ({}, {})",
                                    mouse_event.button_mask, mouse_event.x_px, mouse_event.y_px);
                                channel.data(&mouse_seq[..]).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                        // Else: no buttons pressed, just hovering - ignore to prevent garbage
                    } else if let Some(pipe_instr) = parse_pipe_instruction(&msg_str) {
                        // Handle incoming pipe stream (e.g., STDIN from client)
                        if pipe_instr.name == PIPE_NAME_STDIN {
                            debug!("SSH: STDIN pipe opened by client (stream {})", pipe_instr.stream_id);
                            pipe_manager.register_incoming(
                                pipe_instr.stream_id,
                                &pipe_instr.name,
                                &pipe_instr.mimetype,
                            );
                        } else {
                            debug!("SSH: Unknown pipe '{}' opened by client", pipe_instr.name);
                        }
                    } else if let Some(blob_instr) = parse_blob_instruction(&msg_str) {
                        // Handle blob data on STDIN pipe
                        if pipe_manager.is_stdin_stream(blob_instr.stream_id) {
                            // Security: Check if input is allowed
                            if security.read_only {
                                debug!("SSH: STDIN pipe data blocked (read-only mode)");
                            } else {
                                debug!("SSH: Received {} bytes on STDIN pipe", blob_instr.data.len());
                                channel.data(&blob_instr.data[..]).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                    } else if let Some(end_stream_id) = parse_end_instruction(&msg_str) {
                        // Handle end of pipe stream
                        if pipe_manager.is_stdin_stream(end_stream_id) {
                            debug!("SSH: STDIN pipe closed by client");
                            pipe_manager.close(PIPE_NAME_STDIN);
                        }
                    }
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
                warn!("SSH: Failed to finalize recording: {}", e);
            } else {
                info!("SSH: Session recording finalized");
            }
        }

        info!("SSH handler connection ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Extract clipboard data from OSC 52 escape sequence
/// Format: ESC ] 52 ; c ; <base64_data> ESC \ or BEL
/// Returns decoded clipboard data if found
fn extract_osc52_clipboard(data: &[u8]) -> Option<String> {
    // OSC 52 sequences: ESC ] 52 ; c ; <base64_data> ST
    // ESC = 0x1b, ] = 0x5d, ST = ESC \ (0x1b 0x5c) or BEL (0x07)

    // Look for ESC ] 52 ; c ;
    let mut i = 0;
    while i + 7 < data.len() {
        if data[i] == 0x1b && data[i + 1] == 0x5d {
            // ESC ]
            // Check if this is OSC 52 (clipboard set)
            let osc_start = i + 2;
            if data[osc_start..].starts_with(b"52;c;") || data[osc_start..].starts_with(b"52;;") {
                let data_start = if data[osc_start..].starts_with(b"52;c;") {
                    osc_start + 5
                } else {
                    osc_start + 4
                };

                // Find terminator: ESC \ or BEL
                let mut j = data_start;
                while j < data.len() {
                    if data[j] == 0x07 {
                        // BEL
                        // Found terminator
                        let base64_data = &data[data_start..j];
                        if let Ok(decoded) =
                            base64::engine::general_purpose::STANDARD.decode(base64_data)
                        {
                            if let Ok(text) = String::from_utf8(decoded) {
                                return Some(text);
                            }
                        }
                        break;
                    } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == 0x5c {
                        // ESC \
                        // Found terminator
                        let base64_data = &data[data_start..j];
                        if let Ok(decoded) =
                            base64::engine::general_purpose::STANDARD.decode(base64_data)
                        {
                            if let Ok(text) = String::from_utf8(decoded) {
                                return Some(text);
                            }
                        }
                        break;
                    }
                    j += 1;
                }
            }
        }
        i += 1;
    }

    None
}

// Simple SSH client handler
struct SshClientHandler;

#[async_trait]
impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Accept all server keys for now (INSECURE - TODO: proper verification)
        Ok(true)
    }
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for SshHandler {
    fn name(&self) -> &str {
        "ssh"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Standard channel capacity prevents blocking during burst rendering
        // 4096 capacity = ~300 full renders worth of instructions
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
    fn test_ssh_handler_new() {
        let handler = SshHandler::with_defaults();
        assert_eq!(ProtocolHandler::name(&handler), "ssh");
    }

    #[tokio::test]
    async fn test_ssh_handler_health() {
        let handler = SshHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[test]
    fn test_parse_key_instruction() {
        // Full Guacamole instruction: "3.key,5.65293,1.1;" (Enter key pressed)
        let instruction = "3.key,5.65293,1.1;";
        let result = parse_key_instruction(instruction);

        assert!(result.is_some());
        let key_event = result.unwrap();
        assert_eq!(key_event.keysym, 65293);
        assert!(key_event.pressed);
    }
}
