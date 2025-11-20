use async_trait::async_trait;
#[allow(unused_imports)] // Engine trait needed for .decode() method
use base64::Engine;
use bytes::Bytes;
use guacr_handlers::{EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use guacr_terminal::{x11_keysym_to_bytes, DirtyTracker, TerminalEmulator, TerminalRenderer};
use log::{debug, error, info, trace};
use russh::client;
use russh_keys::key;
use std::collections::HashMap;
use std::sync::Arc;
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

    #[allow(dead_code)]
    fn parse_key_instruction(&self, args: &[String]) -> Option<(u32, bool)> {
        if args.len() >= 2 {
            let keysym: u32 = args[0].parse().ok()?;
            let pressed: bool = args[1] == "1";
            Some((keysym, pressed))
        } else {
            None
        }
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

        // Parse size parameter from browser and calculate dynamic dimensions
        // New approach: rows/cols from config, cell size from browser screen size
        let (rows, cols, width_px, height_px, char_width, char_height) = if let Some(size_str) =
            params.get("size")
        {
            // Size comes as comma-separated values
            let parts: Vec<&str> = size_str.split(',').collect();
            if parts.len() >= 2 {
                if let (Ok(w_px), Ok(h_px)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    // Use config for terminal size (e.g., 80x24)
                    let cols = self.config.default_cols;
                    let rows = self.config.default_rows;

                    // Cap dimensions to prevent oversized terminals (like guacd does)
                    // Large images cause WebRTC backpressure and slow transmission
                    const MAX_WIDTH: u32 = 1024;
                    const MAX_HEIGHT: u32 = 768;
                    let w_px_capped = w_px.min(MAX_WIDTH);
                    let h_px_capped = h_px.min(MAX_HEIGHT);

                    // Calculate character cell size from capped dimensions
                    let char_w = (w_px_capped / cols as u32).max(1);
                    let char_h = (h_px_capped / rows as u32).max(1);

                    // Realign to character grid (prevents partial cells)
                    let aligned_width = cols as u32 * char_w;
                    let aligned_height = rows as u32 * char_h;

                    info!(
                        "SSH: Browser {}x{} px → capped {}x{} px → {}x{} chars @ {}x{} px/cell → aligned {}x{} px",
                        w_px, h_px, w_px_capped, h_px_capped, cols, rows, char_w, char_h, aligned_width, aligned_height
                    );
                    (rows, cols, aligned_width, aligned_height, char_w, char_h)
                } else {
                    info!("SSH: Failed to parse size parameter, using defaults");
                    // Default: 80x24 terminal fitting in 1024x768 (like guacd)
                    let cols = self.config.default_cols;
                    let rows = self.config.default_rows;
                    const MAX_WIDTH: u32 = 1024;
                    const MAX_HEIGHT: u32 = 768;
                    let char_w = (MAX_WIDTH / cols as u32).max(1);
                    let char_h = (MAX_HEIGHT / rows as u32).max(1);
                    let w = cols as u32 * char_w;
                    let h = rows as u32 * char_h;
                    (rows, cols, w, h, char_w, char_h)
                }
            } else {
                info!("SSH: Size parameter missing dimensions, using defaults");
                let cols = self.config.default_cols;
                let rows = self.config.default_rows;
                const MAX_WIDTH: u32 = 1024;
                const MAX_HEIGHT: u32 = 768;
                let char_w = (MAX_WIDTH / cols as u32).max(1);
                let char_h = (MAX_HEIGHT / rows as u32).max(1);
                let w = cols as u32 * char_w;
                let h = rows as u32 * char_h;
                (rows, cols, w, h, char_w, char_h)
            }
        } else {
            info!("SSH: No size parameter provided, using defaults");
            let cols = self.config.default_cols;
            let rows = self.config.default_rows;
            const MAX_WIDTH: u32 = 1024;
            const MAX_HEIGHT: u32 = 768;
            let char_w = (MAX_WIDTH / cols as u32).max(1);
            let char_h = (MAX_HEIGHT / rows as u32).max(1);
            let w = cols as u32 * char_w;
            let h = rows as u32 * char_h;
            (rows, cols, w, h, char_w, char_h)
        };

        info!(
            "SSH handler: Connecting to {}@{}:{}",
            username, hostname, port
        );

        // Create SSH config
        let ssh_config = client::Config::default();
        let mut sh = client::connect(
            Arc::new(ssh_config),
            (hostname.as_str(), port),
            SshClientHandler,
        )
        .await
        .map_err(|e| {
            error!("SSH handler: Connection failed: {}", e);
            HandlerError::ConnectionFailed(e.to_string())
        })?;

        debug!("SSH handler: Connected to SSH server, starting authentication");

        // Authenticate
        let auth_result = if let Some(pwd) = password {
            debug!("SSH handler: Authenticating with password");
            sh.authenticate_password(username, pwd).await
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
            sh.authenticate_publickey(username, Arc::new(key_pair))
                .await
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

        // Use fixed stream ID for main display (reusing stream replaces content, not stacking)
        let stream_id: u32 = 1;

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

        debug!("SSH: Display initialized - waiting for SSH data before rendering");

        // Debounce timer for batching screen updates
        // 16ms = 60fps for smooth interactive feel (was 100ms)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Bidirectional forwarding
        loop {
            tokio::select! {
                // Use biased selection to prioritize rendering over input
                // This prevents the "one char behind" issue
                biased;

                // SSH channel messages -> Terminal -> Client
                Some(msg) = channel.wait() => {
                    match msg {
                        russh::ChannelMsg::Data { ref data } => {
                            trace!("SSH: Received {} bytes", data.len());

                            // Check for OSC 52 clipboard sequences before processing
                            // Format: ESC ] 52 ; c ; base64_data ESC \ or BEL
                            // This is what tmux/vim use to copy to clipboard
                            if let Some(clipboard_data) = extract_osc52_clipboard(data) {
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

                            // Process terminal output
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

                    if msg_str.contains(".key,") {
                        // Extract keysym and pressed from instruction
                        if let Some(args_part) = msg_str.split_once(".key,") {
                            if let Some((first_arg, rest)) = args_part.1.split_once(',') {
                                // Parse keysym from "LENGTH.VALUE" format
                                if let Some((_, keysym_str)) = first_arg.split_once('.') {
                                    if let Ok(keysym) = keysym_str.parse::<u32>() {
                                        // Parse pressed from next arg
                                        if let Some((_, pressed_val)) = rest.split_once('.') {
                                            let pressed = pressed_val.starts_with('1');

                                            // Convert to terminal bytes
                                            let bytes = x11_keysym_to_bytes(keysym, pressed, None);
                                            if !bytes.is_empty() {
                                                channel.data(&bytes[..]).await
                                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if msg_str.contains(".clipboard,") {
                        // Handle clipboard paste from browser
                        // Format: clipboard,<mimetype>,<data>;
                        if let Some(clipboard_part) = msg_str.strip_prefix("clipboard,") {
                            // Parse mimetype (usually "text/plain")
                            if let Some((mimetype_part, rest)) = clipboard_part.split_once(',') {
                                if let Some((_, mimetype)) = mimetype_part.split_once('.') {
                                    if mimetype == "text/plain" {
                                        // Extract clipboard data
                                        if let Some((data_part, _)) = rest.split_once(';') {
                                            if let Some((_, clipboard_data)) = data_part.split_once('.') {
                                                debug!("SSH: Received clipboard data ({} bytes)", clipboard_data.len());

                                                // Send to SSH using bracketed paste mode if available
                                                // This prevents command execution and preserves formatting
                                                // Bracketed paste: \x1b[200~ + data + \x1b[201~
                                                let mut paste_data = Vec::new();
                                                paste_data.extend_from_slice(b"\x1b[200~"); // Start bracketed paste
                                                paste_data.extend_from_slice(clipboard_data.as_bytes());
                                                paste_data.extend_from_slice(b"\x1b[201~"); // End bracketed paste

                                                channel.data(&paste_data[..]).await
                                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if msg_str.contains(".size,") {
                        // Handle resize - extract exact pixel dimensions from browser
                        // Parse: "size,<layer>,<width>,<height>;"
                        if let Some(size_part) = msg_str.strip_prefix("size,") {
                            if let Some((_, rest)) = size_part.split_once(',') { // Skip layer
                                if let Some((width_part, rest)) = rest.split_once(',') {
                                    if let Some((_, width_str)) = width_part.split_once('.') {
                                        if let Some((height_part, _)) = rest.split_once(';') {
                                            if let Some((_, height_str)) = height_part.split_once('.') {
                                                if let (Ok(new_width_px), Ok(new_height_px)) =
                                                    (width_str.parse::<u32>(), height_str.parse::<u32>()) {

                                                    // Calculate new rows/cols using same cell dimensions
                                                    let new_cols = (new_width_px / char_width).max(1) as u16;
                                                    let new_rows = (new_height_px / char_height).max(1) as u16;

                                                    // CRITICAL: Recalculate pixel dimensions to align with character grid
                                                    let aligned_width = new_cols as u32 * char_width;
                                                    let aligned_height = new_rows as u32 * char_height;

                                                    terminal.resize(new_rows, new_cols);
                                                    channel.window_change(new_cols as u32, new_rows as u32, 0, 0)
                                                        .await.map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                                    debug!("SSH: Resize {}x{} px → {}x{} chars → aligned to {}x{} px",
                                                        new_width_px, new_height_px, new_cols, new_rows, aligned_width, aligned_height);

                                                    let size_instr = TerminalRenderer::format_size_instruction(0, aligned_width, aligned_height);
                                                    to_client.send(Bytes::from(size_instr)).await
                                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Debounce tick - render if screen changed
                _ = debounce.tick() => {
                    if terminal.is_dirty() {
                        // Find what changed (dirty region optimization like guacd)
                        if let Some(dirty) = dirty_tracker.find_dirty_region(terminal.screen()) {
                            let total_cells = (rows as usize) * (cols as usize);
                            let dirty_cells = dirty.cell_count();
                            let dirty_pct = (dirty_cells * 100) / total_cells;

                            // Check if this is a scroll operation (most common case)
                            if let Some((scroll_dir, scroll_lines)) = dirty.is_scroll(rows, cols) {
                                if scroll_dir == 1 {
                                    // Scroll up: copy rows 1..N to rows 0..N-1, render new bottom line(s)
                                    trace!("SSH: Scroll up {} lines (copy optimization)", scroll_lines);

                                    // Copy existing content upward
                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        scroll_lines,  // src_row: start from line N
                                        0,             // src_col: from left edge
                                        cols,          // width: full width
                                        rows - scroll_lines, // height: all except scrolled lines
                                        0,             // dst_row: to top
                                        0,             // dst_col: to left edge
                                        char_width,    // char_width: dynamic cell width
                                        char_height,   // char_height: dynamic cell height
                                        0,             // layer: default layer
                                    );
                                    to_client.send(Bytes::from(copy_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // CRITICAL: Clear the bottom region before rendering new content
                                    // The copy instruction doesn't clear the source, so old content remains
                                    let clear_instrs = TerminalRenderer::format_clear_region_instructions(
                                        rows - scroll_lines, // row: start of bottom region
                                        0,                   // col: from left edge
                                        cols,                // width: full width
                                        scroll_lines,        // height: number of scrolled lines
                                        char_width,          // char_width: dynamic cell width
                                        char_height,         // char_height: dynamic cell height
                                    );
                                    for instr in clear_instrs {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }

                                    // Render only the new bottom line(s) that appeared
                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region(
                                        terminal.screen(),
                                        rows - scroll_lines,
                                        rows - 1,
                                        0,
                                        cols - 1,
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

                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        0,             // src_row: from top
                                        0,             // src_col: from left edge
                                        cols,          // width: full width
                                        rows - scroll_lines, // height: all except scrolled lines
                                        scroll_lines,  // dst_row: move down N lines
                                        0,             // dst_col: to left edge
                                        char_width,    // char_width: dynamic cell width
                                        char_height,   // char_height: dynamic cell height
                                        0,             // layer: default layer
                                    );
                                    to_client.send(Bytes::from(copy_instr)).await
                                        .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                    // CRITICAL: Clear the top region before rendering new content
                                    // The copy instruction doesn't clear the source, so old content remains
                                    let clear_instrs = TerminalRenderer::format_clear_region_instructions(
                                        0,             // row: top of screen
                                        0,             // col: from left edge
                                        cols,          // width: full width
                                        scroll_lines,  // height: number of scrolled lines
                                        char_width,    // char_width: dynamic cell width
                                        char_height,   // char_height: dynamic cell height
                                    );
                                    for instr in clear_instrs {
                                        to_client.send(Bytes::from(instr)).await
                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                    }

                                    // Render only the new top line(s) that appeared
                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region(
                                        terminal.screen(),
                                        0,
                                        scroll_lines - 1,
                                        0,
                                        cols - 1,
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
                        }

                        terminal.clear_dirty();
                    }
                }
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
        let handler = SshHandler::with_defaults();

        let args = vec!["65293".to_string(), "1".to_string()]; // Enter key pressed
        let result = handler.parse_key_instruction(&args);

        assert!(result.is_some());
        let (keysym, pressed) = result.unwrap();
        assert_eq!(keysym, 65293);
        assert!(pressed);
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
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(128);

        let callback_clone = Arc::clone(&callback);
        tokio::spawn(async move {
            while let Some(msg) = to_client_rx.recv().await {
                callback_clone.send_instruction(msg);
            }
        });

        self.connect(params, to_client_tx, from_client).await?;
        Ok(())
    }
}
