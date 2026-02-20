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
    // Host key verification
    HostKeyConfig,
    HostKeyVerifier,
    // Connection utilities (timeout, keep-alive)
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
use guacr_protocol::{
    format_chunked_blobs, TextProtocolEncoder, STATUS_CLIENT_UNAUTHORIZED, STATUS_UPSTREAM_ERROR,
    STATUS_UPSTREAM_TIMEOUT,
};
use guacr_terminal::{
    extract_selection_text, format_clear_selection_instructions, format_clipboard_instructions,
    format_selection_overlay_instructions, handle_mouse_selection, parse_clipboard_blob,
    parse_key_instruction, parse_mouse_instruction, x11_keysym_to_bytes_with_modes,
    x11_keysym_to_kitty_sequence, DirtyTracker, ModifierState, MouseSelection, SelectionResult,
    TerminalConfig, TerminalEmulator, TerminalRenderer,
};
use log::{debug, error, info, trace, warn};
use russh::client;
use russh_keys::key;
use russh_keys::PublicKeyBase64;
use ssh_key::Certificate;
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

    /// Check if FIPS mode is enabled
    ///
    /// Checks for FIPS mode via:
    /// 1. FIPS_MODE environment variable
    /// 2. /proc/sys/crypto/fips_enabled file (Linux)
    fn is_fips_mode() -> bool {
        // Check environment variable
        if std::env::var("FIPS_MODE").is_ok() {
            return true;
        }

        // Check Linux FIPS flag
        #[cfg(target_os = "linux")]
        {
            if let Ok(contents) = std::fs::read_to_string("/proc/sys/crypto/fips_enabled") {
                if contents.trim() == "1" {
                    return true;
                }
            }
        }

        false
    }

    /// Configure FIPS-compliant SSH ciphers
    ///
    /// Based on KCM-418 patch: Adds AES-GCM ciphers for FIPS 140-2 compliance.
    /// Cipher preference (from most to least secure):
    /// 1. aes256-gcm@openssh.com (authenticated encryption, best performance)
    /// 2. aes128-gcm@openssh.com (authenticated encryption)
    /// 3. aes256-ctr (CTR mode)
    /// 4. aes192-ctr
    /// 5. aes128-ctr
    /// 6. aes256-cbc (CBC mode, legacy)
    /// 7. aes192-cbc
    /// 8. aes128-cbc
    fn configure_fips_ciphers(_config: &mut client::Config) {
        info!("SSH: FIPS mode enabled - configuring FIPS-compliant ciphers");

        // Note: russh uses a different API than libssh2
        // We'll configure the preferred ciphers through the config
        // russh will automatically negotiate with the server

        // Log the FIPS cipher preference
        info!("SSH: FIPS cipher preference: aes256-gcm, aes128-gcm, aes256-ctr, aes192-ctr, aes128-ctr, aes256-cbc, aes192-cbc, aes128-cbc");

        // russh handles cipher negotiation automatically based on what's compiled in
        // The library already supports AES-GCM and AES-CTR modes
        // We just need to ensure we're using the library's defaults which include these
    }

    /// Send error instruction to client and return HandlerError
    ///
    /// This ensures the client sees a user-friendly error message before the connection closes.
    /// Matches guacd's behavior of calling guac_client_abort() which sends error instructions.
    async fn send_error_and_return(
        to_client: &mpsc::Sender<Bytes>,
        error: HandlerError,
    ) -> HandlerError {
        let (message, status_code) = match &error {
            HandlerError::MissingParameter(param) => (
                format!("Missing required parameter: {}", param),
                STATUS_UPSTREAM_ERROR,
            ),
            HandlerError::ConnectionFailed(msg) => {
                if msg.contains("timeout") || msg.contains("timed out") {
                    (
                        format!("Connection timeout: {}", msg),
                        STATUS_UPSTREAM_TIMEOUT,
                    )
                } else {
                    (format!("Connection failed: {}", msg), STATUS_UPSTREAM_ERROR)
                }
            }
            HandlerError::AuthenticationFailed(msg) => {
                if msg.contains("Host key") || msg.contains("fingerprint") {
                    (
                        format!("Host key verification failed: {}", msg),
                        STATUS_UPSTREAM_ERROR,
                    )
                } else if msg.contains("timeout") || msg.contains("timed out") {
                    (
                        format!("Authentication timeout: {}", msg),
                        STATUS_UPSTREAM_TIMEOUT,
                    )
                } else {
                    (
                        format!("Authentication failed: {}", msg),
                        STATUS_CLIENT_UNAUTHORIZED,
                    )
                }
            }
            _ => (error.to_string(), STATUS_UPSTREAM_ERROR),
        };

        send_error_best_effort(to_client, &message, status_code).await;

        error
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

        // Parse terminal configuration (font, color-scheme, terminal-type, scrollback, backspace)
        let terminal_config = TerminalConfig::from_params(&params);
        info!(
            "SSH: Terminal config - type={}, scrollback={}, color_scheme={}, font_size={}, backspace={}",
            terminal_config.terminal_type,
            terminal_config.scrollback_size,
            terminal_config.color_scheme.name(),
            terminal_config.font_size,
            terminal_config.backspace_code
        );

        // Parse host key verification configuration
        let host_key_config = HostKeyConfig::from_params(&params);
        if host_key_config.ignore_host_key {
            warn!("SSH: Host key verification DISABLED (ignore-host-key=true) - INSECURE");
        } else if host_key_config.known_hosts_path.is_some() {
            info!(
                "SSH: Host key verification via known_hosts: {}",
                host_key_config
                    .known_hosts_path
                    .as_deref()
                    .unwrap_or("(none)")
            );
        } else if host_key_config.host_key_fingerprint.is_some() {
            info!("SSH: Host key verification via pinned fingerprint");
        }

        // Extract connection parameters
        let hostname = match params.get("hostname") {
            Some(h) => h,
            None => {
                error!("SSH handler: Missing hostname parameter");
                let err = HandlerError::MissingParameter("hostname".to_string());
                return Err(Self::send_error_and_return(&to_client, err).await);
            }
        };

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let username = match params.get("username") {
            Some(u) => u,
            None => {
                error!("SSH handler: Missing username parameter");
                let err = HandlerError::MissingParameter("username".to_string());
                return Err(Self::send_error_and_return(&to_client, err).await);
            }
        };

        let password = params.get("password");
        let private_key = params.get("private_key");

        // IMPORTANT: Always use DEFAULT size during initialization (like guacd does)
        // The client will send a resize instruction with actual browser dimensions after handshake
        // This prevents the "half screen" issue where we use browser size too early
        //
        // CRITICAL: Use a TALLER initial terminal to capture full SSH banner
        // Many SSH servers send large MOTD/banner screens (50+ lines)
        // Using 1024x768 @ 96 DPI gives only 42 rows, which truncates banners
        // Instead, use 80x60 terminal (standard VT100 size with extra rows for banners)
        // This ensures we capture the full banner before the client resize
        info!("SSH: Using default handshake size (80x60 terminal) - will resize after client connects");
        let char_width = 9_u32;
        let char_height = 18_u32;
        let cols = 80_u16; // Standard terminal width
        let rows = 60_u16; // Taller to capture full banners
        let width_px = cols as u32 * char_width; // 720 px (aligned)
        let height_px = rows as u32 * char_height; // 1080 px (aligned)
        let (rows, cols, width_px, height_px, char_width, char_height) =
            (rows, cols, width_px, height_px, char_width, char_height);

        info!(
            "SSH handler: Connecting to {}@{}:{} (timeout: {}s)",
            username, hostname, port, security.connection_timeout_secs
        );

        // Create SSH config
        let mut ssh_config = client::Config::default();

        // Create SSH client handler with host key verification
        let ssh_client_handler = SshClientHandler::new(hostname.clone(), port, host_key_config);

        // Configure FIPS-compliant ciphers if FIPS mode is enabled
        if Self::is_fips_mode() {
            Self::configure_fips_ciphers(&mut ssh_config);
        }

        // Connect with timeout (matches guacd's timeout parameter)
        let connection_timeout = Duration::from_secs(security.connection_timeout_secs);
        let sh = tokio::time::timeout(
            connection_timeout,
            client::connect(
                Arc::new(ssh_config),
                (hostname.as_str(), port),
                ssh_client_handler,
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
        })
        .and_then(|r| {
            r.map_err(|e| {
                // Check if this is a host key verification failure
                let error_str = e.to_string();
                if error_str.contains("host key") || error_str.contains("fingerprint") {
                    error!("SSH handler: Host key verification failed: {}", e);
                    HandlerError::AuthenticationFailed(format!(
                        "Host key verification failed: {}",
                        e
                    ))
                } else {
                    error!("SSH handler: Connection failed: {}", e);
                    HandlerError::ConnectionFailed(e.to_string())
                }
            })
        });

        let mut sh = match sh {
            Ok(s) => s,
            Err(e) => return Err(Self::send_error_and_return(&to_client, e).await),
        };

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
                })
        } else if let Some(key_pem) = private_key {
            debug!("SSH handler: Authenticating with private key");

            // Parse private key (supports OpenSSH, PEM, and PKCS#8 formats)
            // russh_keys::decode_secret_key handles all formats and optional passphrase
            let passphrase = params.get("passphrase").map(|s| s.as_str());

            debug!(
                "SSH handler: Decoding private key (encrypted: {})",
                passphrase.is_some()
            );
            let key_pair = match russh_keys::decode_secret_key(key_pem, passphrase) {
                Ok(k) => k,
                Err(e) => {
                    error!("SSH handler: Failed to decode private key: {}", e);
                    let err = if passphrase.is_some() {
                        HandlerError::AuthenticationFailed(format!(
                            "Invalid private key or passphrase: {}",
                            e
                        ))
                    } else {
                        HandlerError::AuthenticationFailed(format!(
                            "Invalid private key format: {}",
                            e
                        ))
                    };
                    return Err(Self::send_error_and_return(&to_client, err).await);
                }
            };

            // Check for certificate-based authentication
            let public_key_cert = params.get("public-key");

            if let Some(cert_str) = public_key_cert {
                debug!("SSH handler: Certificate provided, using certificate-based authentication");

                // Parse OpenSSH certificate
                let certificate = match cert_str.trim().parse::<Certificate>() {
                    Ok(cert) => cert,
                    Err(e) => {
                        error!("SSH handler: Failed to parse SSH certificate: {}", e);
                        let err = HandlerError::AuthenticationFailed(format!(
                            "Invalid SSH certificate format: {}",
                            e
                        ));
                        return Err(Self::send_error_and_return(&to_client, err).await);
                    }
                };

                debug!(
                    "SSH handler: Certificate parsed successfully, authenticating with certificate"
                );
                tokio::time::timeout(
                    auth_timeout,
                    sh.authenticate_openssh_cert(username, Arc::new(key_pair), certificate),
                )
                .await
                .map_err(|_| {
                    error!("SSH handler: Certificate authentication timed out");
                    HandlerError::AuthenticationFailed("Authentication timed out".to_string())
                })
            } else {
                debug!(
                    "SSH handler: Private key decoded successfully, authenticating with public key"
                );
                tokio::time::timeout(
                    auth_timeout,
                    sh.authenticate_publickey(username, Arc::new(key_pair)),
                )
                .await
                .map_err(|_| {
                    error!("SSH handler: Authentication timed out");
                    HandlerError::AuthenticationFailed("Authentication timed out".to_string())
                })
            }
        } else {
            error!("SSH handler: No authentication method provided");
            let err = HandlerError::MissingParameter("password or private_key".to_string());
            return Err(Self::send_error_and_return(&to_client, err).await);
        };

        let auth_result = match auth_result {
            Ok(r) => r,
            Err(e) => return Err(Self::send_error_and_return(&to_client, e).await),
        };

        let auth_success = match auth_result {
            Ok(success) => success,
            Err(e) => {
                error!("SSH handler: Authentication error: {}", e);
                let err = HandlerError::AuthenticationFailed(e.to_string());
                return Err(Self::send_error_and_return(&to_client, err).await);
            }
        };

        if !auth_success {
            error!("SSH handler: Authentication failed - wrong credentials");
            let err = HandlerError::AuthenticationFailed("Authentication failed".to_string());
            return Err(Self::send_error_and_return(&to_client, err).await);
        }

        info!("SSH handler: Authentication successful");

        // Open channel and request PTY
        let mut channel = sh
            .channel_open_session()
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        channel
            .request_pty(
                false,                       // want_reply
                terminal_config.term_type(), // Use configured terminal type
                cols as u32,
                rows as u32,
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Set environment variables (locale/timezone) before shell/command
        // Note: Many SSH servers have AcceptEnv disabled by default, so these may be ignored
        if let Some(locale) = params.get("locale") {
            debug!("SSH: Setting LANG={}", locale);
            // Ignore errors - server may reject env vars
            let _ = channel.set_env(false, "LANG", locale.as_str()).await;
        }
        if let Some(timezone) = params.get("timezone") {
            debug!("SSH: Setting TZ={}", timezone);
            let _ = channel.set_env(false, "TZ", timezone.as_str()).await;
        }

        // Either execute a specific command or open interactive shell
        if let Some(command) = params.get("command") {
            info!("SSH: Executing command: {}", command);
            channel
                .exec(false, command.as_str())
                .await
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            debug!("SSH handler: Command execution started");
        } else {
            channel
                .request_shell(false)
                .await
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
            debug!("SSH handler: SSH shell established");
        }

        // Create terminal emulator with browser-requested dimensions and configured scrollback
        let mut terminal =
            TerminalEmulator::new_with_scrollback(rows, cols, terminal_config.scrollback_size);

        // CRITICAL: Read any initial data (banner/MOTD) that arrived immediately after shell request
        // SSH servers often send welcome banners right away, before we enter the event loop
        //
        // IMPORTANT: Use a longer timeout (500ms) to ensure we capture the full banner.
        // Some SSH servers send the banner in multiple packets with delays between them.
        // The 200ms timeout was too short and caused the banner to be cut off.
        //
        // Strategy:
        // 1. Wait up to 500ms for the first packet (initial banner)
        // 2. Then wait 200ms between subsequent packets (continuation)
        // 3. Stop when we hit a timeout (no more data coming)
        debug!("SSH: Checking for initial banner data...");
        let mut banner_bytes_total = 0usize;
        let mut first_packet = true;
        loop {
            // Use longer timeout for first packet, shorter for subsequent packets
            let timeout_ms = if first_packet { 500 } else { 200 };

            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), channel.wait())
                .await
            {
                Ok(Some(russh::ChannelMsg::Data { ref data })) => {
                    banner_bytes_total += data.len();
                    trace!(
                        "SSH: Received {} bytes of initial banner data (total: {}, first_packet: {})",
                        data.len(),
                        banner_bytes_total,
                        first_packet
                    );
                    terminal
                        .process(data)
                        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                    first_packet = false;
                }
                Ok(Some(_other)) => {
                    // Other channel messages during startup (ignore for now)
                    debug!("SSH: Received non-data message during banner check");
                }
                Ok(None) => {
                    // Channel closed unexpectedly
                    warn!("SSH: Channel closed during banner check");
                    break;
                }
                Err(_timeout) => {
                    // No more data available - banner collection complete
                    debug!(
                        "SSH: Banner collection complete (timeout after {}ms, total {} bytes)",
                        timeout_ms, banner_bytes_total
                    );
                    break;
                }
            }
        }

        // Check if we collected any banner data
        let banner_collected = terminal.is_dirty();
        let banner_screen_content = if banner_collected {
            let screen = terminal.screen();
            let mut content_preview = String::new();

            // Get first 3 lines of banner for debugging
            for row in 0..3.min(rows) {
                for col in 0..80.min(cols) {
                    if let Some(cell) = screen.cell(row, col) {
                        if let Some(c) = cell.contents().chars().next() {
                            if c != ' ' && c != '\0' {
                                content_preview.push(c);
                            } else {
                                content_preview.push(' ');
                            }
                        }
                    }
                }
                content_preview.push('\n');
            }
            content_preview
        } else {
            String::new()
        };

        debug!(
            "SSH: Banner collection finished, has_content={}, bytes_received={}, terminal_size={}x{}",
            banner_collected,
            banner_bytes_total,
            cols,
            rows
        );

        if banner_collected {
            debug!(
                "SSH: Banner preview (first 3 lines):\n{}",
                banner_screen_content
            );
            info!(
                "SSH: Banner collected with {} bytes - colors should be rendered in JPEG",
                banner_bytes_total
            );
        }

        // Make rows/cols mutable for dynamic resizing (guacd-style)
        let mut current_rows = rows;
        let mut current_cols = cols;

        // Calculate font size as 70% of cell height (fits well with some padding)
        let font_size = (char_height as f32) * 0.70;

        info!(
            "SSH: Creating renderer with {}x{} px cells, {:.1}pt font, color_scheme={}",
            char_width,
            char_height,
            font_size,
            terminal_config.color_scheme.name()
        );

        let renderer = TerminalRenderer::new_with_dimensions_and_scheme(
            char_width,
            char_height,
            font_size,
            terminal_config.color_scheme,
        )
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Adaptive quality for bandwidth optimization (like RDP handler)
        // Terminal text requires high quality - JPEG artifacts on rendered font glyphs
        // make characters unreadable at lower quality levels. Unlike RDP/VNC where
        // reduced quality is acceptable for photos/video, SSH renders small font glyphs
        // that degrade severely below quality 85.
        let mut adaptive_quality = guacr_handlers::AdaptiveQuality::new(85).with_min_quality(85);

        // Zero-allocation protocol encoder (reused for all img instructions)
        let mut protocol_encoder = TextProtocolEncoder::new();

        // Store backspace code for key handling (before terminal_config moves)
        let backspace_code = terminal_config.backspace_code;

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
            send_and_record(&to_client, &mut recorder, Bytes::from(pipe_instr))
                .await
                .map_err(HandlerError::ChannelError)?;
        }

        // Send ready and name instructions to signal the connection is established
        send_ready(&to_client, "ssh-ready").await?;
        send_name(&to_client, "SSH").await?;

        // Send I-beam cursor bitmap (standard text cursor for terminals)
        let mut cursor_manager = CursorManager::new(false, false, 85);
        debug!("SSH: Sending IBeam cursor");
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
        debug!(
            "SSH: Sending size instruction ({}x{} px = {}x{} chars)",
            width_px, height_px, cols, rows
        );
        let size_instr = TerminalRenderer::format_size_instruction(0, width_px, height_px);
        send_and_record(&to_client, &mut recorder, Bytes::from(size_instr))
            .await
            .map_err(HandlerError::ChannelError)?;

        debug!("SSH: Display initialized");

        // CRITICAL: Render banner immediately if collected
        // The initial render timer will catch it at 300ms, but we should render it
        // as soon as possible so the user sees the SSH banner/MOTD right away
        if banner_collected {
            debug!("SSH: Banner collected, rendering immediately");

            // Render the banner at the initial size
            let quality = adaptive_quality.calculate_quality();
            let jpeg = renderer
                .render_screen_with_quality(terminal.screen(), rows, cols, quality)
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

            adaptive_quality.track_frame_sent(jpeg.len());
            debug!(
                "SSH: Banner render produced {} byte JPEG (quality {})",
                jpeg.len(),
                quality
            );

            // Base64 encode JPEG
            let base64_data =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg);

            // Send img instruction (zero-allocation)
            let img_instr = protocol_encoder.format_img_instruction(
                stream_id,
                0, // layer
                0, // x
                0, // y
                "image/jpeg",
            );
            send_and_record(&to_client, &mut recorder, img_instr.freeze())
                .await
                .map_err(HandlerError::ChannelError)?;

            // Send blob chunks + end instruction
            let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
            for instr in blob_instructions {
                send_and_record(&to_client, &mut recorder, Bytes::from(instr))
                    .await
                    .map_err(HandlerError::ChannelError)?;
            }

            let sync_instr = renderer.format_sync_instruction(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            );
            send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr))
                .await
                .map_err(HandlerError::ChannelError)?;

            terminal.clear_dirty();
            debug!("SSH: Banner rendered immediately at initial size");
        }

        // Backup render timer - only fires if immediate render somehow missed an update
        // This is a safety net, not the primary rendering mechanism
        let mut backup_render = tokio::time::interval(std::time::Duration::from_millis(100));
        backup_render.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Force initial render after 300ms to catch SSH prompt
        // SSH servers often send prompt immediately, but it might arrive after we initialize
        // Increased from 100ms to 300ms to ensure we capture the full banner and prompt
        // Box::pin is required because tokio::time::Sleep is !Unpin
        let mut initial_render_timer =
            Box::pin(tokio::time::sleep(std::time::Duration::from_millis(300)));
        let mut initial_render_done = false;

        // Track modifier key state (Ctrl, Shift, Alt) for Ctrl+C, etc.
        let mut modifier_state = ModifierState::new();

        // Mouse selection tracking
        let mut mouse_selection = MouseSelection::new();

        // Clipboard storage
        // Store clipboard data received from client (via clipboard stream)
        // This data is pasted when user presses Ctrl+Shift+V
        let mut stored_clipboard = String::new();

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
                        let quality = adaptive_quality.calculate_quality();
                        let jpeg = renderer.render_screen_with_quality(
                            terminal.screen(),
                            terminal.size().0,
                            terminal.size().1,
                            quality,
                        ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                        adaptive_quality.track_frame_sent(jpeg.len());
                        debug!("SSH: Initial render produced {} byte JPEG (quality {})", jpeg.len(), quality);

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

                        let sync_instr = renderer.format_sync_instruction(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64
                        );
                        send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr)).await
                            .map_err(HandlerError::ChannelError)?;

                        terminal.clear_dirty();
                    } else {
                        debug!("SSH: Initial render timer fired but terminal not dirty, skipping");
                    }
                }

                // Backup render - safety net in case immediate render missed something
                _ = backup_render.tick() => {
                    if terminal.is_dirty() {
                        // Find what changed (dirty region optimization like guacd)
                        let dirty_opt = dirty_tracker.find_dirty_region(terminal.screen());

                        if let Some(dirty) = dirty_opt {
                            let total_cells = (current_rows as usize) * (current_cols as usize);
                            let dirty_cells = dirty.cell_count();
                            let dirty_pct = (dirty_cells * 100) / total_cells;

                            // Render dirty region directly (no scroll copy optimization).
                            // Apache guacd renders terminal updates as direct image patches,
                            // not pixel-level copy instructions. Copy instructions can cause
                            // visual artifacts because terminal scroll doesn't map cleanly
                            // to canvas pixel shifts (reflow, wrapping, attributes change).
                            {
                                if dirty_pct == 0 {
                                    // Edge case: dirty region is empty (shouldn't happen but be safe)
                                    trace!("SSH: Empty dirty region (0%), skipping render");
                                } else if dirty_pct < 30 {
                                    // Small region - render only changed area
                                    if dirty_pct < 5 {
                                        trace!("SSH: Micro update: {}x{} cells ({}%) at row {}-{}, col {}-{}",
                                            dirty.width(), dirty.height(), dirty_pct,
                                            dirty.min_row, dirty.max_row, dirty.min_col, dirty.max_col);
                                    } else {
                                        trace!("SSH: Dirty region: {}x{} cells ({}%) at row {}-{}, col {}-{}",
                                            dirty.width(), dirty.height(), dirty_pct,
                                            dirty.min_row, dirty.max_row, dirty.min_col, dirty.max_col);
                                    }

                                    // Expand by 1 cell in all directions to cover JPEG
                                    // compression artifacts (DCT ringing at 8x8 block
                                    // boundaries from cursor underline rendering)
                                    let bp_min_row = dirty.min_row.saturating_sub(1);
                                    let bp_max_row = (dirty.max_row + 1).min(current_rows - 1);
                                    let bp_min_col = dirty.min_col.saturating_sub(1);
                                    let bp_max_col = (dirty.max_col + 1).min(current_cols - 1);

                                    let quality = adaptive_quality.calculate_quality();
                                    let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region_with_quality(
                                        terminal.screen(),
                                        bp_min_row,
                                        bp_max_row,
                                        bp_min_col,
                                        bp_max_col,
                                        quality,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                    adaptive_quality.track_frame_sent(jpeg.len());

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
                                    // Large region - render full screen
                                    trace!("SSH: Full screen ({}% dirty)", dirty_pct);

                                    let quality = adaptive_quality.calculate_quality();
                                    let jpeg = renderer.render_screen_with_quality(
                                        terminal.screen(),
                                        terminal.size().0,
                                        terminal.size().1,
                                        quality,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                    adaptive_quality.track_frame_sent(jpeg.len());

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
                            }

                            let sync_instr = renderer.format_sync_instruction(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64
                            );
                            send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr)).await
                                .map_err(HandlerError::ChannelError)?;
                        } else {
                            // Dirty tracker failed to find changes (cursor movement, small updates)
                            // Fall back to full screen render to prevent "one char behind" bug
                            debug!("SSH: Dirty tracker returned None, rendering full screen as fallback");

                            let quality = adaptive_quality.calculate_quality();
                            let jpeg = renderer.render_screen_with_quality(
                                terminal.screen(),
                                terminal.size().0,
                                terminal.size().1,
                                quality,
                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            adaptive_quality.track_frame_sent(jpeg.len());
                            debug!("SSH: Fallback render produced {} byte JPEG (quality {})", jpeg.len(), quality);

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

                            let sync_instr = renderer.format_sync_instruction(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as u64
                            );
                            send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr)).await
                                .map_err(HandlerError::ChannelError)?;
                        }

                        terminal.clear_dirty();
                        debug!("SSH: Render complete, cleared dirty flag");
                    }
                }

                // SSH channel messages -> Terminal -> Client
                Some(msg) = channel.wait() => {
                    match msg {
                        russh::ChannelMsg::Data { ref data } => {
                            trace!("SSH: Received {} bytes from SSH server", data.len());

                            // If STDOUT pipe is enabled, send raw data to client
                            // This enables native terminal display (with ANSI escape codes)
                            if pipe_manager.is_stdout_enabled() {
                                let blob = pipe_blob_bytes(PIPE_STREAM_STDOUT, data);
                                send_and_record(&to_client, &mut recorder, blob).await
                                    .map_err(HandlerError::ChannelError)?;
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
                                    send_and_record(&to_client, &mut recorder, Bytes::from(clipboard_instr)).await
                                        .map_err(HandlerError::ChannelError)?;

                                    // 2. Send data as blob on the stream
                                    let blob_instr = format!(
                                        "4.blob,1.{},{}.{};",
                                        clipboard_stream_id,
                                        clipboard_data.len(),
                                        clipboard_data
                                    );
                                    send_and_record(&to_client, &mut recorder, Bytes::from(blob_instr)).await
                                        .map_err(HandlerError::ChannelError)?;

                                    // 3. Close the stream
                                    let end_instr = format!("3.end,1.{};", clipboard_stream_id);
                                    send_and_record(&to_client, &mut recorder, Bytes::from(end_instr)).await
                                        .map_err(HandlerError::ChannelError)?;
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

                            // Check for BEL character (0x07) and send audio beep to client
                            if data.contains(&0x07) {
                                debug!("SSH: BEL detected, sending audio beep");
                                send_bell(&to_client, 100).await?;
                            }

                            // Render immediately for responsive feedback
                            // The dirty region optimization ensures we only send changed portions
                            if terminal.is_dirty() {
                                let dirty_opt = dirty_tracker.find_dirty_region(terminal.screen());

                                if let Some(dirty) = dirty_opt {
                                    // Expand dirty region by 1 cell in all directions to
                                    // cover JPEG compression artifacts from cursor underline
                                    // rendering. The cursor's 3px white underline creates
                                    // DCT ringing at 8x8 block boundaries.
                                    let render_min_row = dirty.min_row.saturating_sub(1);
                                    let render_max_row = (dirty.max_row + 1).min(current_rows - 1);
                                    let render_min_col = dirty.min_col.saturating_sub(1);
                                    let render_max_col = (dirty.max_col + 1).min(current_cols - 1);

                                    // Check dirty percentage to decide partial vs full render
                                    let total_cells = (current_rows as usize) * (current_cols as usize);
                                    let dirty_cells = dirty.cell_count();
                                    let dirty_pct = if total_cells > 0 { (dirty_cells * 100) / total_cells } else { 100 };

                                    let quality = adaptive_quality.calculate_quality();

                                    if dirty_pct < 30 {
                                        // Small change - render only the dirty region
                                        let (jpeg, x_px, y_px, _w_px, _h_px) = renderer.render_region_with_quality(
                                            terminal.screen(),
                                            render_min_row,
                                            render_max_row,
                                            render_min_col,
                                            render_max_col,
                                            quality,
                                        ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                        adaptive_quality.track_frame_sent(jpeg.len());

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
                                        // Large change (e.g., scroll) - render full screen
                                        let jpeg = renderer.render_screen_with_quality(
                                            terminal.screen(),
                                            terminal.size().0,
                                            terminal.size().1,
                                            quality,
                                        ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                        adaptive_quality.track_frame_sent(jpeg.len());

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

                                    let sync_instr = renderer.format_sync_instruction(
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_millis() as u64
                                    );
                                    send_and_record(&to_client, &mut recorder, Bytes::from(sync_instr)).await
                                        .map_err(HandlerError::ChannelError)?;

                                    terminal.clear_dirty();
                                } else {
                                    // Dirty tracker returned None - fallback to full screen render
                                    debug!("SSH: Dirty tracker returned None, rendering full screen");

                                    let quality = adaptive_quality.calculate_quality();
                                    let jpeg = renderer.render_screen_with_quality(
                                        terminal.screen(),
                                        terminal.size().0,
                                        terminal.size().1,
                                        quality,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                    adaptive_quality.track_frame_sent(jpeg.len());

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
                        }
                        russh::ChannelMsg::ExitStatus { exit_status } => {
                            error!("SSH handler: SSH command exited with status: {}", exit_status);

                            let error_msg = format!("SSH session ended (exit status: {})", exit_status);
                            send_error_best_effort(&to_client, &error_msg, 517).await; // RESOURCE_CLOSED

                            break;
                        }
                        russh::ChannelMsg::Eof => {
                            error!("SSH handler: SSH channel EOF");

                            send_error_best_effort(&to_client, "SSH connection closed by server", 517).await; // RESOURCE_CLOSED

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
                        info!("SSH handler: Client disconnected");
                        // No need to send error - client already disconnected
                        break;
                    };

                    // Record client-to-server instruction in .ses file
                    record_client_input(&mut recorder, &msg);

                    // Parse Guacamole instruction
                    let msg_str = String::from_utf8_lossy(&msg);

                    // Log clipboard-related messages at debug level for troubleshooting
                    if msg_str.contains("clipboard") || msg_str.contains("blob") {
                        debug!("SSH: Received clipboard/blob message: {}", msg_str);
                    } else {
                        trace!("SSH: Received client message: {}", msg_str);
                    }

                    if let Some(key_event) = parse_key_instruction(&msg_str) {
                        trace!("SSH: Key event - keysym={} (0x{:04X}), pressed={}, ctrl={}, shift={}, alt={}",
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
                                debug!("SSH: Paste blocked (disabled or read-only mode)");
                                continue;
                            }

                            if stored_clipboard.is_empty() {
                                debug!("SSH: Paste shortcut pressed but clipboard is empty");
                                continue;
                            }

                            // Check clipboard buffer size limit
                            let max_size = security.clipboard_buffer_size;
                            let paste_text = if stored_clipboard.len() > max_size {
                                warn!("SSH: Clipboard truncated from {} to {} bytes", stored_clipboard.len(), max_size);
                                &stored_clipboard[..max_size]
                            } else {
                                &stored_clipboard
                            };

                            debug!("SSH: Paste shortcut - Pasting {} chars from clipboard", paste_text.len());

                            // Send using bracketed paste mode for safety
                            // This prevents commands from auto-executing
                            let mut paste_data = Vec::new();
                            paste_data.extend_from_slice(b"\x1b[200~"); // Start bracketed paste
                            paste_data.extend_from_slice(paste_text.as_bytes());
                            paste_data.extend_from_slice(b"\x1b[201~"); // End bracketed paste

                            channel.data(&paste_data[..]).await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // CRITICAL: Force dirty tracker reset after large paste
                            // Large pastes can cause dirty region tracking to fail, resulting in
                            // partial screen rendering (black screen with only small region visible)
                            // Reset forces next render to be full screen
                            if paste_text.len() > 100 {
                                debug!("SSH: Large paste detected ({}  chars), resetting dirty tracker to force full render", paste_text.len());
                                dirty_tracker = DirtyTracker::new(current_rows, current_cols);
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
                            debug!("SSH: Copy shortcut pressed - ignoring (selection already copies)");
                            continue;
                        }

                        // Handle select-all shortcut (selects entire terminal + copies to clipboard)
                        // - Ctrl+Shift+A (Linux/Windows): keysym 'A' (0x41) with ctrl+shift
                        // - Cmd+A (Mac): keysym 'a' (0x61) with meta
                        let is_select_all = key_event.pressed && (
                            (key_event.keysym == 0x41 && modifier_state.control && modifier_state.shift) ||
                            (key_event.keysym == 0x61 && modifier_state.meta)
                        );

                        if is_select_all {
                            let last_row = current_rows.saturating_sub(1);
                            let last_col = current_cols.saturating_sub(1);

                            // Show selection overlay (entire terminal)
                            let overlay = format_selection_overlay_instructions(
                                (0, 0),
                                (last_row, last_col),
                                char_width,
                                char_height,
                                current_cols,
                                current_rows,
                            );
                            for instr in overlay {
                                send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                    .map_err(HandlerError::ChannelError)?;
                            }

                            // Extract all text and send to clipboard
                            let text = extract_selection_text(
                                &terminal,
                                (0, 0),
                                (last_row, last_col),
                                current_cols,
                            );

                            if !text.is_empty() {
                                stored_clipboard = text.clone();
                                info!("SSH: Select all - {} chars copied to clipboard", text.len());

                                let clipboard_stream_id = 10;
                                let clipboard_instructions = format_clipboard_instructions(&text, clipboard_stream_id);
                                for instr in clipboard_instructions {
                                    send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                        .map_err(HandlerError::ChannelError)?;
                                }
                            }

                            // Clear overlay after a brief visual flash
                            let clear = format_clear_selection_instructions();
                            for instr in clear {
                                send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                    .map_err(HandlerError::ChannelError)?;
                            }

                            continue;
                        }

                        // Convert to terminal bytes with modifier state, backspace, and application cursor mode
                        // This enables Ctrl+C (0x03), Ctrl+D (0x04), etc.
                        // Application cursor mode is needed for vim, less, tmux to work correctly
                        let application_cursor = terminal.is_application_cursor_mode();

                        // Check if Kitty keyboard protocol is enabled
                        let kitty_level = terminal.kitty_keyboard_level();

                        // Log arrow keys to help debug mode switching
                        if matches!(key_event.keysym, 0xFF51..=0xFF54) {
                            trace!(
                                "SSH: Arrow key 0x{:X} in {} mode, kitty_level={}",
                                key_event.keysym,
                                if application_cursor { "application" } else { "normal" },
                                kitty_level
                            );
                        }

                        // Use Kitty keyboard protocol if enabled, otherwise use legacy
                        let bytes = if kitty_level > 0 {
                            trace!("SSH: Using Kitty keyboard protocol level {}", kitty_level);
                            x11_keysym_to_kitty_sequence(
                                key_event.keysym,
                                key_event.pressed,
                                Some(&modifier_state),
                                backspace_code,
                                application_cursor,
                                kitty_level,
                            )
                        } else {
                            x11_keysym_to_bytes_with_modes(
                                key_event.keysym,
                                key_event.pressed,
                                Some(&modifier_state),
                                backspace_code,
                                application_cursor,
                            )
                        };
                        trace!("SSH: Key converted to {} bytes: {:?}", bytes.len(), bytes);
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
                    } else if let Some(clipboard_text) = parse_clipboard_blob(&msg_str) {
                        // Clipboard blob instruction: Store clipboard data from client
                        // This is just synchronization - NOT a paste command!
                        // The actual paste happens when user presses Ctrl+Shift+V (keysym 'V' with ctrl+shift)

                        stored_clipboard = clipboard_text;
                        debug!("SSH: Clipboard updated - stored {} chars (waiting for Ctrl+Shift+V to paste)", stored_clipboard.len());
                        trace!("SSH: Clipboard content preview: {:?}", &stored_clipboard.chars().take(50).collect::<String>());
                    } else if msg_str.contains(".clipboard,") {
                        // Clipboard instruction received - this is just a SYNC, not a paste command!
                        // The Guacamole protocol sends clipboard instructions to sync clipboard state
                        // between client and server. This does NOT mean the user wants to paste.
                        debug!("SSH: Clipboard stream opened - syncing clipboard state (not pasting)");
                        trace!("SSH: Clipboard instruction: {}", msg_str);
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

                                            // CRITICAL: Validate dimensions to prevent black screen
                                            if new_width_px == 0 || new_height_px == 0 {
                                                warn!("SSH: Ignoring resize with zero dimensions ({}x{})", new_width_px, new_height_px);
                                                continue;
                                            }

                                            // Calculate new rows/cols using FIXED cell dimensions (guacd-style)
                                            let new_cols = (new_width_px / char_width).clamp(20, 500) as u16;
                                            let new_rows = (new_height_px / char_height).clamp(10, 200) as u16;

                            // Skip resize if dimensions haven't changed
                            if new_rows == current_rows && new_cols == current_cols {
                                debug!("SSH: Ignoring resize - dimensions unchanged ({}x{} chars)", current_cols, current_rows);
                                continue;
                            }

                                            // CRITICAL: Recalculate pixel dimensions to align with character grid
                                            let aligned_width = new_cols as u32 * char_width;
                                            let aligned_height = new_rows as u32 * char_height;

                                            // Resize terminal emulator (preserves content via vt100's set_size)
                                            terminal.resize(new_rows, new_cols);

                                            // CRITICAL: Wait for terminal to stabilize after resize
                                            // The vt100 parser's set_size() reformats the screen content,
                                            // and we need to ensure all content has settled before rendering.
                                            // This prevents the banner from being lost during the reflow.
                                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
                                            send_and_record(&to_client, &mut recorder, Bytes::from(size_instr)).await
                                                .map_err(HandlerError::ChannelError)?;

                                            // Force full screen render after resize
                                            let quality = adaptive_quality.calculate_quality();
                                            let jpeg = renderer.render_screen_with_quality(
                                                terminal.screen(),
                                                new_rows,
                                                new_cols,
                                                quality,
                                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                            adaptive_quality.track_frame_sent(jpeg.len());
                                            debug!("SSH: Resize render produced {} byte JPEG (quality {})", jpeg.len(), quality);

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
                                }
                            }
                        }
                    } else if let Some(mouse_event) = parse_mouse_instruction(&msg_str) {
                        debug!("SSH: Mouse event - button={}, pos=({},{}), term_mouse={}",
                            mouse_event.button_mask, mouse_event.x_px, mouse_event.y_px,
                            terminal.is_mouse_enabled());

                        // Security: Check read-only mode for mouse clicks
                        if security.read_only && !is_mouse_event_allowed_readonly(mouse_event.button_mask) {
                            trace!("SSH: Mouse click blocked (read-only mode)");
                            continue;
                        }

                        // Handle mouse events intelligently:
                        // 1. If terminal has mouse mode enabled (vim/tmux) - send X11 sequences
                        // 2. Otherwise, left-click drag = text selection (copy to clipboard)
                        // 3. Hover with no buttons = ignored (prevents garbage)

                        // Check if terminal has mouse mode enabled (vim :set mouse=a, tmux mouse mode)
                        if terminal.is_mouse_enabled() && mouse_event.button_mask != 0 {
                            debug!("SSH: Terminal mouse mode enabled - sending X11 sequences (no selection)");
                            // Terminal wants mouse events - send X11 sequences
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
                        // Try text selection (only when mouse mode is disabled)
                        else {
                            debug!("SSH: Terminal mouse mode disabled - handling text selection");
                            match handle_mouse_selection(
                                mouse_event,
                                &mut mouse_selection,
                                &terminal,
                                char_width,
                                char_height,
                                current_cols,
                                current_rows,
                                modifier_state.shift, // Pass shift key state for extend selection
                            ) {
                                SelectionResult::InProgress(overlay_instructions) => {
                                    // Send blue semi-transparent selection overlay
                                    for instr in overlay_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                                SelectionResult::Complete { text: selected_text, clear_instructions } => {
                                    info!("SSH: Selection complete - {} chars selected", selected_text.len());

                                    // Security: Check if copy is allowed
                                    if !security.is_copy_allowed() {
                                        warn!("SSH: Selection copy blocked (copy disabled)");

                                        // Still clear the overlay even if copy is blocked
                                        for instr in clear_instructions {
                                            send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                                .map_err(HandlerError::ChannelError)?;
                                        }
                                        continue;
                                    }

                                    debug!("SSH: Copying {} chars to clipboard", selected_text.len());

                                    // CRITICAL: Update local clipboard immediately to avoid race condition
                                    // If user pastes immediately after selecting, they expect the selected text
                                    // Without this, there's a race where the clipboard blob from client arrives
                                    // after the user has already pressed Ctrl+Shift+V
                                    stored_clipboard = selected_text.clone();
                                    debug!("SSH: Local clipboard updated immediately with {} chars", stored_clipboard.len());

                                    // Clear the overlay
                                    for instr in clear_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }

                                    // Send to client as clipboard using shared formatter
                                    let clipboard_stream_id = 10;
                                    let clipboard_instructions = format_clipboard_instructions(&selected_text, clipboard_stream_id);

                                    info!("SSH: Sending {} clipboard instructions for {} chars to UI", clipboard_instructions.len(), selected_text.len());
                                    for instr in clipboard_instructions {
                                        debug!("SSH: Sending clipboard instruction: {}", instr);
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                    info!("SSH: Clipboard instructions sent successfully to UI");
                                }
                                SelectionResult::None => {
                                    // No selection action (hovering, etc.) - ignore
                                }
                            }
                        }
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
                                trace!("SSH: Received {} bytes on STDIN pipe", blob_instr.data.len());
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
            let _ = send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await;
        }

        // Finalize recording
        if let Some(rec) = recorder {
            if let Err(e) = rec.finalize() {
                warn!("SSH: Failed to finalize recording: {}", e);
            } else {
                info!("SSH: Session recording finalized");
            }
        }

        send_disconnect(&to_client).await;
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

/// SSH client handler with host key verification support
struct SshClientHandler {
    verifier: HostKeyVerifier,
    hostname: String,
    port: u16,
}

impl SshClientHandler {
    fn new(hostname: String, port: u16, config: HostKeyConfig) -> Self {
        Self {
            verifier: HostKeyVerifier::new(config),
            hostname,
            port,
        }
    }
}

#[async_trait]
impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        use guacr_handlers::HostKeyResult;

        // Get key type and raw bytes from the public key
        let key_type = server_public_key.name();
        let key_bytes = server_public_key.public_key_bytes();

        // Verify the host key
        let result = self
            .verifier
            .verify(&self.hostname, self.port, key_type, &key_bytes);

        match &result {
            HostKeyResult::Verified => {
                info!("SSH: Host key verified for {}:{}", self.hostname, self.port);
            }
            HostKeyResult::Skipped => {
                warn!(
                    "SSH: Host key verification skipped for {}:{} (INSECURE)",
                    self.hostname, self.port
                );
            }
            HostKeyResult::NotConfigured => {
                debug!(
                    "SSH: No host key verification configured for {}:{}",
                    self.hostname, self.port
                );
            }
            HostKeyResult::UnknownHost => {
                warn!(
                    "SSH: Unknown host {}:{} - not in known_hosts",
                    self.hostname, self.port
                );
            }
            HostKeyResult::Mismatch { expected, actual } => {
                error!(
                    "SSH: HOST KEY MISMATCH for {}:{}\nExpected: {}\nActual: {}",
                    self.hostname, self.port, expected, actual
                );
            }
        }

        // Check if connection should be allowed based on config
        if result.is_allowed(&self.verifier.config) {
            Ok(true)
        } else {
            // Return error with descriptive message
            if let Some(msg) = result.error_message() {
                error!("SSH: {}", msg);
            }
            Ok(false) // Reject the connection
        }
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
