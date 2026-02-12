use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    connect_tcp_with_timeout, is_keyboard_event_allowed_readonly, is_mouse_event_allowed_readonly,
    parse_blob_instruction, parse_end_instruction, parse_pipe_instruction, pipe_blob_bytes,
    record_client_input, send_and_record, send_bell, send_disconnect, send_error_best_effort,
    send_name, send_ready, CursorManager, EventBasedHandler, EventCallback, HandlerError,
    HandlerSecuritySettings, HandlerStats, HealthStatus, KeepAliveManager, MultiFormatRecorder,
    PipeStreamManager, ProtocolHandler, RecordingConfig, StandardCursor,
    DEFAULT_KEEPALIVE_INTERVAL_SECS, PIPE_NAME_STDIN, PIPE_STREAM_STDOUT,
};
use guacr_protocol::{format_chunked_blobs, TextProtocolEncoder};
use guacr_terminal::{
    format_clipboard_instructions, handle_mouse_selection, mouse_event_to_x11_sequence,
    parse_clipboard_blob, parse_key_instruction, parse_mouse_instruction,
    x11_keysym_to_bytes_with_backspace, x11_keysym_to_kitty_sequence, DirtyTracker, ModifierState,
    MouseSelection, SelectionResult, TerminalConfig, TerminalEmulator, TerminalRenderer,
};
use log::{debug, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// RFC 2217 Constants
// ---------------------------------------------------------------------------

/// Telnet IAC (Interpret As Command) byte
const IAC: u8 = 0xFF;
/// Telnet subnegotiation begin
const SB: u8 = 0xFA;
/// Telnet subnegotiation end
const SE: u8 = 0xF0;
/// Telnet WILL option
const WILL: u8 = 0xFB;
/// Telnet DO option
const DO: u8 = 0xFD;

/// RFC 2217 COM-PORT-OPTION (option 44)
const COM_PORT_OPTION: u8 = 44;

/// RFC 2217 client-to-server subnegotiation commands
const SET_BAUDRATE: u8 = 1;
const SET_DATASIZE: u8 = 2;
const SET_PARITY: u8 = 3;
const SET_STOPSIZE: u8 = 4;

// ---------------------------------------------------------------------------
// Serial Configuration
// ---------------------------------------------------------------------------

/// Parity mode for serial communication.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Parity {
    /// No parity bit
    None = 1,
    /// Odd parity
    Odd = 2,
    /// Even parity
    Even = 3,
    /// Mark parity (parity bit always 1)
    Mark = 4,
    /// Space parity (parity bit always 0)
    Space = 5,
}

impl Parity {
    /// Parse a parity value from a string parameter.
    fn from_param(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "odd" => Parity::Odd,
            "even" => Parity::Even,
            "mark" => Parity::Mark,
            "space" => Parity::Space,
            _ => Parity::None,
        }
    }
}

/// Stop bits for serial communication.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopBits {
    /// 1 stop bit
    One = 1,
    /// 2 stop bits
    Two = 2,
    /// 1.5 stop bits
    OnePointFive = 3,
}

impl StopBits {
    /// Parse a stop bits value from a string parameter.
    fn from_param(s: &str) -> Self {
        match s {
            "2" => StopBits::Two,
            "1.5" => StopBits::OnePointFive,
            _ => StopBits::One,
        }
    }
}

/// Configuration for the serial console handler.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    /// Default TCP port when not specified in connection parameters.
    /// Console servers commonly use ports 2001-2032, 3001, or 7001.
    pub default_port: u16,
    /// Default terminal rows
    pub default_rows: u16,
    /// Default terminal columns
    pub default_cols: u16,
    /// Default baud rate
    pub default_baud_rate: u32,
    /// Default data bits (5, 6, 7, or 8)
    pub default_data_bits: u8,
    /// Default parity
    pub default_parity: Parity,
    /// Default stop bits
    pub default_stop_bits: StopBits,
    /// Whether to perform RFC 2217 negotiation by default
    pub default_rfc2217: bool,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            default_port: 2001,
            default_rows: 24,
            default_cols: 80,
            default_baud_rate: 9600,
            default_data_bits: 8,
            default_parity: Parity::None,
            default_stop_bits: StopBits::One,
            default_rfc2217: true,
        }
    }
}

/// Parsed serial port parameters from connection HashMap.
#[derive(Debug, Clone)]
struct SerialParams {
    hostname: String,
    port: u16,
    baud_rate: u32,
    data_bits: u8,
    parity: Parity,
    stop_bits: StopBits,
    rfc2217: bool,
}

impl SerialParams {
    /// Parse serial parameters from the connection parameter map, applying defaults.
    fn from_params(
        params: &HashMap<String, String>,
        config: &SerialConfig,
    ) -> Result<Self, HandlerError> {
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?
            .clone();

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(config.default_port);

        let baud_rate: u32 = params
            .get("baud_rate")
            .or_else(|| params.get("baud-rate"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(config.default_baud_rate);

        let data_bits: u8 = params
            .get("data_bits")
            .or_else(|| params.get("data-bits"))
            .and_then(|v| v.parse().ok())
            .map(|v: u8| {
                if (5..=8).contains(&v) {
                    v
                } else {
                    config.default_data_bits
                }
            })
            .unwrap_or(config.default_data_bits);

        let parity = params
            .get("parity")
            .map(|v| Parity::from_param(v))
            .unwrap_or(config.default_parity);

        let stop_bits = params
            .get("stop_bits")
            .or_else(|| params.get("stop-bits"))
            .map(|v| StopBits::from_param(v))
            .unwrap_or(config.default_stop_bits);

        let rfc2217 = params
            .get("rfc2217")
            .or_else(|| params.get("rfc-2217"))
            .map(|v| v == "true" || v == "1")
            .unwrap_or(config.default_rfc2217);

        Ok(Self {
            hostname,
            port,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            rfc2217,
        })
    }
}

// ---------------------------------------------------------------------------
// RFC 2217 Negotiation Helpers
// ---------------------------------------------------------------------------

/// Build the initial WILL COM-PORT-OPTION negotiation sequence.
///
/// This tells the console server that we support RFC 2217.
/// The server should respond with DO COM-PORT-OPTION.
fn build_will_com_port() -> Vec<u8> {
    vec![IAC, WILL, COM_PORT_OPTION]
}

/// Build a DO COM-PORT-OPTION response.
///
/// Sent in response to the server's WILL COM-PORT-OPTION, acknowledging
/// that we accept the server's offer to negotiate serial parameters.
fn build_do_com_port() -> Vec<u8> {
    vec![IAC, DO, COM_PORT_OPTION]
}

/// Build a SET-BAUDRATE subnegotiation command.
///
/// Baud rate is encoded as a 4-byte big-endian unsigned integer.
/// Common values: 9600, 19200, 38400, 57600, 115200.
fn build_set_baudrate(baud_rate: u32) -> Vec<u8> {
    let baud_bytes = baud_rate.to_be_bytes();
    let mut buf = Vec::with_capacity(8);
    buf.push(IAC);
    buf.push(SB);
    buf.push(COM_PORT_OPTION);
    buf.push(SET_BAUDRATE);
    // Escape any 0xFF bytes in the baud rate value (Telnet IAC doubling)
    for &b in &baud_bytes {
        buf.push(b);
        if b == IAC {
            buf.push(IAC);
        }
    }
    buf.push(IAC);
    buf.push(SE);
    buf
}

/// Build a SET-DATASIZE subnegotiation command.
///
/// Data size is a single byte: 5, 6, 7, or 8.
fn build_set_datasize(data_bits: u8) -> Vec<u8> {
    vec![IAC, SB, COM_PORT_OPTION, SET_DATASIZE, data_bits, IAC, SE]
}

/// Build a SET-PARITY subnegotiation command.
///
/// Parity is a single byte: 1=None, 2=Odd, 3=Even, 4=Mark, 5=Space.
fn build_set_parity(parity: Parity) -> Vec<u8> {
    vec![IAC, SB, COM_PORT_OPTION, SET_PARITY, parity as u8, IAC, SE]
}

/// Build a SET-STOPSIZE subnegotiation command.
///
/// Stop size is a single byte: 1=One, 2=Two, 3=OnePointFive.
fn build_set_stopsize(stop_bits: StopBits) -> Vec<u8> {
    vec![
        IAC,
        SB,
        COM_PORT_OPTION,
        SET_STOPSIZE,
        stop_bits as u8,
        IAC,
        SE,
    ]
}

/// Build the complete RFC 2217 negotiation sequence for the given parameters.
///
/// This concatenates all subnegotiation commands into a single buffer
/// that can be sent in one write for efficiency.
fn build_rfc2217_negotiation(params: &SerialParams) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    // First, announce that we support COM-PORT-OPTION
    buf.extend_from_slice(&build_will_com_port());
    // Also send DO in case the server initiated WILL
    buf.extend_from_slice(&build_do_com_port());
    // Set serial parameters
    buf.extend_from_slice(&build_set_baudrate(params.baud_rate));
    buf.extend_from_slice(&build_set_datasize(params.data_bits));
    buf.extend_from_slice(&build_set_parity(params.parity));
    buf.extend_from_slice(&build_set_stopsize(params.stop_bits));
    buf
}

// ---------------------------------------------------------------------------
// Serial Console Handler
// ---------------------------------------------------------------------------

/// Serial Console over IP handler (RFC 2217).
///
/// Connects to a console server (terminal server, serial-to-Ethernet adapter,
/// or similar device) via raw TCP, optionally negotiates serial port parameters
/// using RFC 2217, and renders the serial console output through the standard
/// VT100 terminal emulation pipeline.
///
/// This is functionally a Telnet variant: the transport is TCP, the rendering
/// is identical VT100-to-JPEG, and the input handling reuses the same
/// Guacamole key/mouse/clipboard infrastructure. The only addition is the
/// RFC 2217 parameter negotiation at connection time.
///
/// ## Connection Parameters
///
/// | Parameter   | Default | Description                                  |
/// |-------------|---------|----------------------------------------------|
/// | hostname    | (required) | Console server address                    |
/// | port        | 2001    | TCP port                                     |
/// | baud_rate   | 9600    | Serial baud rate                             |
/// | data_bits   | 8       | Data bits (5/6/7/8)                          |
/// | parity      | none    | Parity (none/odd/even/mark/space)            |
/// | stop_bits   | 1       | Stop bits (1/1.5/2)                          |
/// | rfc2217     | true    | Whether to negotiate RFC 2217                |
pub struct SerialConsoleHandler {
    config: SerialConfig,
}

impl SerialConsoleHandler {
    pub fn new(config: SerialConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(SerialConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for SerialConsoleHandler {
    fn name(&self) -> &str {
        "serial"
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
        info!("Serial console handler starting connection");

        // Parse security settings
        let security = HandlerSecuritySettings::from_params(&params);
        info!(
            "Serial: Security settings - read_only={}, disable_copy={}, disable_paste={}",
            security.read_only, security.disable_copy, security.disable_paste
        );

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(&params);
        if recording_config.is_enabled() {
            info!(
                "Serial: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        // Parse terminal configuration (font, color-scheme, terminal-type, scrollback, backspace)
        let terminal_config = TerminalConfig::from_params(&params);
        info!(
            "Serial: Terminal config - type={}, scrollback={}, color_scheme={}, backspace={}",
            terminal_config.terminal_type,
            terminal_config.scrollback_size,
            terminal_config.color_scheme.name(),
            terminal_config.backspace_code
        );

        // Parse serial-specific parameters
        let serial_params = SerialParams::from_params(&params, &self.config)?;
        info!(
            "Serial: Parameters - baud={}, data_bits={}, parity={:?}, stop_bits={:?}, rfc2217={}",
            serial_params.baud_rate,
            serial_params.data_bits,
            serial_params.parity,
            serial_params.stop_bits,
            serial_params.rfc2217
        );

        // Character cell dimensions (high DPI style, same as Telnet)
        const CHAR_W: u32 = 19;
        const CHAR_H: u32 = 38;

        info!(
            "Serial: Using default handshake size (1024x768) - will resize after client connects"
        );
        let rows = self.config.default_rows;
        let cols = self.config.default_cols;
        let width_px = cols as u32 * CHAR_W;
        let height_px = rows as u32 * CHAR_H;

        info!(
            "Connecting to {}:{} (timeout: {}s)",
            serial_params.hostname, serial_params.port, security.connection_timeout_secs
        );

        // Connect via TCP with timeout
        let stream = connect_tcp_with_timeout(
            (serial_params.hostname.as_str(), serial_params.port),
            security.connection_timeout_secs,
        )
        .await?;

        info!("Serial console TCP connection established");

        let (mut read_half, mut write_half) = stream.into_split();

        // Perform RFC 2217 negotiation if enabled
        if serial_params.rfc2217 {
            info!(
                "Serial: Sending RFC 2217 negotiation (baud={}, data={}, parity={:?}, stop={:?})",
                serial_params.baud_rate,
                serial_params.data_bits,
                serial_params.parity,
                serial_params.stop_bits
            );
            let negotiation = build_rfc2217_negotiation(&serial_params);
            write_half.write_all(&negotiation).await.map_err(|e| {
                HandlerError::ProtocolError(format!("RFC 2217 negotiation failed: {}", e))
            })?;
            debug!(
                "Serial: RFC 2217 negotiation sent ({} bytes)",
                negotiation.len()
            );
        } else {
            debug!("Serial: RFC 2217 negotiation disabled, using raw TCP");
        }

        // Create terminal emulator
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
            match MultiFormatRecorder::new(&recording_config, &params, "serial", cols, rows) {
                Ok(rec) => {
                    info!("Serial: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("Serial: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Use fixed stream ID for main display (stream 0 is reserved in Guacamole)
        let stream_id: u32 = 1;

        // Initialize pipe stream manager for native terminal display support
        let mut pipe_manager = PipeStreamManager::new();

        let enable_pipe = params
            .get("enable-pipe")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        if enable_pipe {
            info!("Serial: Pipe streams enabled - opening STDOUT pipe for native terminal display");
            let pipe_instr = pipe_manager.enable_stdout();
            send_and_record(&to_client, &mut recorder, Bytes::from(pipe_instr))
                .await
                .map_err(HandlerError::ChannelError)?;
        }

        // Send ready and name instructions
        send_ready(&to_client, "serial-ready").await?;
        send_name(&to_client, "Serial Console").await?;

        // Send I-beam cursor bitmap (standard text cursor for terminals)
        let mut cursor_manager = CursorManager::new(false, false, 85);
        debug!("Serial: Sending IBeam cursor");
        let cursor_instrs = cursor_manager
            .send_standard_cursor(StandardCursor::IBeam)
            .map_err(|e| HandlerError::ProtocolError(format!("Cursor error: {}", e)))?;
        for instr in cursor_instrs {
            send_and_record(&to_client, &mut recorder, Bytes::from(instr))
                .await
                .map_err(HandlerError::ChannelError)?;
        }

        // Send size instruction to initialize display dimensions
        info!(
            "Serial: Sending size instruction ({}x{} px = {}x{} chars)",
            width_px, height_px, cols, rows
        );
        let size_instr = TerminalRenderer::format_size_instruction(0, width_px, height_px);
        send_and_record(&to_client, &mut recorder, Bytes::from(size_instr))
            .await
            .map_err(HandlerError::ChannelError)?;

        // Bidirectional forwarding state
        let mut buf = vec![0u8; 4096];
        let mut modifier_state = ModifierState::new();
        let mut mouse_selection = MouseSelection::new();
        let mut stored_clipboard = String::new();

        // Debounce timer for batching screen updates (16ms = 60fps)
        let mut debounce = tokio::time::interval(std::time::Duration::from_millis(16));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Keep-alive manager
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
                        trace!("Serial: Sending keep-alive sync");
                        if to_client.send(sync_instr).await.is_err() {
                            info!("Serial: Client channel closed, ending session");
                            break;
                        }
                    }
                }

                // Serial output -> Terminal -> Client
                result = read_half.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            info!("Serial console connection closed");
                            send_error_best_effort(&to_client, "Serial connection closed by server", 517).await;
                            break;
                        }
                        Ok(n) => {
                            // If STDOUT pipe is enabled, send raw data to client
                            if pipe_manager.is_stdout_enabled() {
                                let blob = pipe_blob_bytes(PIPE_STREAM_STDOUT, &buf[..n]);
                                send_and_record(&to_client, &mut recorder, blob).await
                                    .map_err(HandlerError::ChannelError)?;
                            }

                            // Record output if recording is enabled
                            if let Some(ref mut rec) = recorder {
                                let _ = rec.record_output(&buf[..n]);
                            }

                            // Process terminal output (for image rendering)
                            // Strip any Telnet IAC sequences from the data before feeding
                            // to the terminal emulator, since console servers may echo back
                            // negotiation responses mixed with terminal data.
                            let clean = strip_telnet_commands(&buf[..n]);
                            if !clean.is_empty() {
                                terminal.process(&clean)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }

                            // Check for BEL character (0x07) and send audio beep to client
                            if buf[..n].contains(&0x07) {
                                debug!("Serial: BEL detected, sending audio beep");
                                send_bell(&to_client, 100).await?;
                            }
                        }
                        Err(e) => {
                            warn!("Serial read error: {}", e);
                            let error_msg = format!("Serial connection error: {}", e);
                            send_error_best_effort(&to_client, &error_msg, 512).await;
                            break;
                        }
                    }
                }

                // Client input -> Serial
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("Serial: Client disconnected");
                        break;
                    };
                    // Record client-to-server instruction
                    record_client_input(&mut recorder, &msg);

                    let msg_str = String::from_utf8_lossy(&msg);

                    if let Some(key_event) = parse_key_instruction(&msg_str) {
                        // Update modifier state
                        if modifier_state.update_modifier(key_event.keysym, key_event.pressed) {
                            continue;
                        }

                        // Security: Check read-only mode
                        if security.read_only
                            && !is_keyboard_event_allowed_readonly(key_event.keysym, modifier_state.control)
                        {
                            trace!("Serial: Keyboard input blocked (read-only mode)");
                            continue;
                        }

                        // Handle paste shortcuts
                        let is_paste = key_event.pressed && (
                            (key_event.keysym == 0x56 && modifier_state.control && modifier_state.shift) ||
                            (key_event.keysym == 0x76 && modifier_state.meta)
                        );

                        if is_paste {
                            if !security.is_paste_allowed() {
                                debug!("Serial: Paste blocked (disabled or read-only mode)");
                                continue;
                            }
                            if stored_clipboard.is_empty() {
                                debug!("Serial: Paste shortcut pressed but clipboard is empty");
                                continue;
                            }

                            let max_size = security.clipboard_buffer_size;
                            let paste_text = if stored_clipboard.len() > max_size {
                                warn!("Serial: Clipboard truncated from {} to {} bytes", stored_clipboard.len(), max_size);
                                &stored_clipboard[..max_size]
                            } else {
                                &stored_clipboard
                            };

                            debug!("Serial: Pasting {} chars from clipboard", paste_text.len());

                            // Send using bracketed paste mode for safety
                            let mut paste_data = Vec::new();
                            paste_data.extend_from_slice(b"\x1b[200~");
                            paste_data.extend_from_slice(paste_text.as_bytes());
                            paste_data.extend_from_slice(b"\x1b[201~");

                            write_half.write_all(&paste_data).await
                                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            // Force dirty tracker reset after large paste
                            if paste_text.len() > 100 {
                                debug!("Serial: Large paste ({} chars), resetting dirty tracker", paste_text.len());
                                dirty_tracker = DirtyTracker::new(rows, cols);
                            }

                            continue;
                        }

                        // Handle copy shortcuts - ignore since selection already copies
                        let is_copy = key_event.pressed && (
                            (key_event.keysym == 0x43 && modifier_state.control && modifier_state.shift) ||
                            (key_event.keysym == 0x63 && modifier_state.meta)
                        );

                        if is_copy {
                            debug!("Serial: Copy shortcut pressed - ignoring (selection already copies)");
                            continue;
                        }

                        // Convert to terminal bytes
                        let kitty_level = terminal.kitty_keyboard_level();
                        let bytes = if kitty_level > 0 {
                            trace!("Serial: Using Kitty keyboard protocol level {}", kitty_level);
                            x11_keysym_to_kitty_sequence(
                                key_event.keysym,
                                key_event.pressed,
                                Some(&modifier_state),
                                backspace_code,
                                false,
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
                        debug!("Serial: Clipboard stream opened - syncing state (not pasting)");
                    } else if let Some(clipboard_text) = parse_clipboard_blob(&msg_str) {
                        stored_clipboard = clipboard_text;
                        debug!("Serial: Clipboard updated - stored {} chars", stored_clipboard.len());
                    } else if let Some(mouse_event) = parse_mouse_instruction(&msg_str) {
                        // Security: Check read-only mode for mouse clicks
                        if security.read_only && !is_mouse_event_allowed_readonly(mouse_event.button_mask) {
                            trace!("Serial: Mouse click blocked (read-only mode)");
                            continue;
                        }

                        if terminal.is_mouse_enabled() && mouse_event.button_mask != 0 {
                            let mouse_seq = mouse_event_to_x11_sequence(
                                mouse_event.x_px,
                                mouse_event.y_px,
                                mouse_event.button_mask as u8,
                                CHAR_W,
                                CHAR_H,
                            );
                            if !mouse_seq.is_empty() {
                                trace!("Serial: Mouse X11 sequence (button={}) at ({}, {})",
                                    mouse_event.button_mask, mouse_event.x_px, mouse_event.y_px);
                                write_half.write_all(&mouse_seq).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        } else {
                            match handle_mouse_selection(
                                mouse_event,
                                &mut mouse_selection,
                                &terminal,
                                CHAR_W,
                                CHAR_H,
                                cols,
                                rows,
                                modifier_state.shift,
                            ) {
                                SelectionResult::InProgress(overlay_instructions) => {
                                    trace!("Serial: Selection in progress, sending {} overlay instructions", overlay_instructions.len());
                                    for instr in overlay_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                                SelectionResult::Complete { text: selected_text, clear_instructions } => {
                                    if !security.is_copy_allowed() {
                                        debug!("Serial: Selection copy blocked (copy disabled)");
                                        for instr in clear_instructions {
                                            send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                                .map_err(HandlerError::ChannelError)?;
                                        }
                                        continue;
                                    }

                                    debug!("Serial: Selection complete, copying {} chars", selected_text.len());
                                    stored_clipboard = selected_text.clone();

                                    for instr in clear_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }

                                    let clipboard_stream_id = 10;
                                    let clipboard_instructions = format_clipboard_instructions(&selected_text, clipboard_stream_id);
                                    for instr in clipboard_instructions {
                                        send_and_record(&to_client, &mut recorder, Bytes::from(instr)).await
                                            .map_err(HandlerError::ChannelError)?;
                                    }
                                }
                                SelectionResult::None => {}
                            }
                        }
                    } else if let Some(pipe_instr) = parse_pipe_instruction(&msg_str) {
                        if pipe_instr.name == PIPE_NAME_STDIN {
                            debug!("Serial: STDIN pipe opened by client (stream {})", pipe_instr.stream_id);
                            pipe_manager.register_incoming(
                                pipe_instr.stream_id,
                                &pipe_instr.name,
                                &pipe_instr.mimetype,
                            );
                        } else {
                            debug!("Serial: Unknown pipe '{}' opened by client", pipe_instr.name);
                        }
                    } else if let Some(blob_instr) = parse_blob_instruction(&msg_str) {
                        if pipe_manager.is_stdin_stream(blob_instr.stream_id) {
                            if security.read_only {
                                debug!("Serial: STDIN pipe data blocked (read-only mode)");
                            } else {
                                debug!("Serial: Received {} bytes on STDIN pipe", blob_instr.data.len());
                                write_half.write_all(&blob_instr.data).await
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                    } else if let Some(end_stream_id) = parse_end_instruction(&msg_str) {
                        if pipe_manager.is_stdin_stream(end_stream_id) {
                            debug!("Serial: STDIN pipe closed by client");
                            pipe_manager.close(PIPE_NAME_STDIN);
                        }
                    }
                }

                // Debounce tick - render if screen changed
                _ = debounce.tick() => {
                    if terminal.is_dirty() {
                        if let Some(dirty) = dirty_tracker.find_dirty_region(terminal.screen()) {
                            let total_cells = (rows as usize) * (cols as usize);
                            let dirty_cells = dirty.cell_count();
                            let dirty_pct = (dirty_cells * 100) / total_cells;

                            // Check if this is a scroll operation
                            if let Some((scroll_dir, scroll_lines)) = dirty.is_scroll(rows, cols) {
                                if scroll_dir == 1 {
                                    trace!("Serial: Scroll up {} lines (copy optimization)", scroll_lines);

                                    let copy_instr = TerminalRenderer::format_copy_instruction(
                                        scroll_lines, 0, cols, rows - scroll_lines, 0, 0,
                                        CHAR_W, CHAR_H, 0,
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
                                        terminal.screen(), r_min_row, r_max_row, r_min_col, r_max_col,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    let base64_data = base64::Engine::encode(
                                        &base64::engine::general_purpose::STANDARD, &jpeg,
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
                                        terminal.screen(), terminal.size().0, terminal.size().1,
                                    ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                    let base64_data = base64::Engine::encode(
                                        &base64::engine::general_purpose::STANDARD, &jpeg,
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
                                trace!("Serial: Partial update: {}% dirty ({} cells)", dirty_pct, dirty_cells);

                                // Expand by 1 cell in all directions for JPEG artifacts
                                let r_min_row = dirty.min_row.saturating_sub(1);
                                let r_max_row = (dirty.max_row + 1).min(rows - 1);
                                let r_min_col = dirty.min_col.saturating_sub(1);
                                let r_max_col = (dirty.max_col + 1).min(cols - 1);

                                let (jpeg, x_px, y_px, _, _) = renderer.render_region(
                                    terminal.screen(), r_min_row, r_max_row, r_min_col, r_max_col,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                let base64_data = base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD, &jpeg,
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
                                trace!("Serial: Full screen update: {}% dirty", dirty_pct);

                                let jpeg = renderer.render_screen(
                                    terminal.screen(), terminal.size().0, terminal.size().1,
                                ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                                let base64_data = base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD, &jpeg,
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
                            // No dirty region found, render full screen
                            let jpeg = renderer.render_screen(
                                terminal.screen(), terminal.size().0, terminal.size().1,
                            ).map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                            let base64_data = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD, &jpeg,
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
                    debug!("Serial session ending");
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
                warn!("Serial: Failed to finalize recording: {}", e);
            } else {
                info!("Serial: Session recording finalized");
            }
        }

        send_disconnect(&to_client).await;
        info!("Serial console handler connection ended");
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
impl EventBasedHandler for SerialConsoleHandler {
    fn name(&self) -> &str {
        "serial"
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

// ---------------------------------------------------------------------------
// Telnet IAC stripping
// ---------------------------------------------------------------------------

/// Strip Telnet IAC command sequences from a byte buffer.
///
/// Console servers may interleave RFC 2217 responses (and other Telnet
/// negotiation sequences) with the actual terminal data stream. Feeding raw
/// IAC bytes to the VT100 parser would produce garbage, so we strip them.
///
/// This handles:
/// - Two-byte commands: IAC + WILL/WONT/DO/DONT/...  (all single-byte opcodes)
/// - Three-byte option negotiations: IAC + WILL/WONT/DO/DONT + option
/// - Subnegotiations: IAC SB ... IAC SE
/// - Escaped 0xFF: IAC IAC -> single 0xFF byte
fn strip_telnet_commands(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == IAC && i + 1 < data.len() {
            match data[i + 1] {
                // IAC IAC -> literal 0xFF
                IAC => {
                    out.push(0xFF);
                    i += 2;
                }
                // Subnegotiation: skip until IAC SE
                SB => {
                    i += 2;
                    while i < data.len() {
                        if data[i] == IAC && i + 1 < data.len() && data[i + 1] == SE {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // WILL, WONT, DO, DONT: 3-byte sequences
                0xFB..=0xFE => {
                    // IAC + command + option byte
                    i += 3;
                }
                // Any other IAC command (NOP, DM, BRK, IP, AO, AYT, EC, EL, GA)
                _ => {
                    i += 2;
                }
            }
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serial_handler_name() {
        let handler = SerialConsoleHandler::with_defaults();
        assert_eq!(
            <SerialConsoleHandler as ProtocolHandler>::name(&handler),
            "serial"
        );
    }

    #[tokio::test]
    async fn test_serial_handler_health() {
        let handler = SerialConsoleHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_serial_handler_stats() {
        let handler = SerialConsoleHandler::with_defaults();
        let stats = handler.stats().await.unwrap();
        assert_eq!(stats.total_connections, 0);
    }

    // -----------------------------------------------------------------------
    // RFC 2217 negotiation byte building
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_will_com_port() {
        let bytes = build_will_com_port();
        assert_eq!(bytes, vec![0xFF, 0xFB, 44]);
    }

    #[test]
    fn test_build_do_com_port() {
        let bytes = build_do_com_port();
        assert_eq!(bytes, vec![0xFF, 0xFD, 44]);
    }

    #[test]
    fn test_build_set_baudrate_9600() {
        let bytes = build_set_baudrate(9600);
        // IAC SB COM_PORT SET_BAUDRATE <4 bytes big-endian> IAC SE
        // 9600 = 0x00002580
        assert_eq!(
            bytes,
            vec![0xFF, 0xFA, 44, 1, 0x00, 0x00, 0x25, 0x80, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_set_baudrate_115200() {
        let bytes = build_set_baudrate(115200);
        // 115200 = 0x0001C200
        assert_eq!(
            bytes,
            vec![0xFF, 0xFA, 44, 1, 0x00, 0x01, 0xC2, 0x00, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_set_baudrate_iac_escaping() {
        // Baud rate 0xFF0000FF should double each 0xFF byte
        let bytes = build_set_baudrate(0xFF0000FF);
        assert_eq!(
            bytes,
            vec![0xFF, 0xFA, 44, 1, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_set_datasize() {
        assert_eq!(
            build_set_datasize(8),
            vec![0xFF, 0xFA, 44, 2, 8, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_datasize(7),
            vec![0xFF, 0xFA, 44, 2, 7, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_set_parity() {
        assert_eq!(
            build_set_parity(Parity::None),
            vec![0xFF, 0xFA, 44, 3, 1, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_parity(Parity::Odd),
            vec![0xFF, 0xFA, 44, 3, 2, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_parity(Parity::Even),
            vec![0xFF, 0xFA, 44, 3, 3, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_parity(Parity::Mark),
            vec![0xFF, 0xFA, 44, 3, 4, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_parity(Parity::Space),
            vec![0xFF, 0xFA, 44, 3, 5, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_set_stopsize() {
        assert_eq!(
            build_set_stopsize(StopBits::One),
            vec![0xFF, 0xFA, 44, 4, 1, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_stopsize(StopBits::Two),
            vec![0xFF, 0xFA, 44, 4, 2, 0xFF, 0xF0]
        );
        assert_eq!(
            build_set_stopsize(StopBits::OnePointFive),
            vec![0xFF, 0xFA, 44, 4, 3, 0xFF, 0xF0]
        );
    }

    #[test]
    fn test_build_rfc2217_negotiation_contains_all_commands() {
        let params = SerialParams {
            hostname: "test".to_string(),
            port: 2001,
            baud_rate: 19200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            rfc2217: true,
        };
        let bytes = build_rfc2217_negotiation(&params);

        // Should contain: WILL COM_PORT, DO COM_PORT, SET_BAUDRATE, SET_DATASIZE, SET_PARITY, SET_STOPSIZE
        // WILL COM_PORT
        assert!(bytes.windows(3).any(|w| w == [IAC, WILL, COM_PORT_OPTION]));
        // DO COM_PORT
        assert!(bytes.windows(3).any(|w| w == [IAC, DO, COM_PORT_OPTION]));
        // SET_BAUDRATE subneg
        assert!(bytes
            .windows(4)
            .any(|w| w == [IAC, SB, COM_PORT_OPTION, SET_BAUDRATE]));
        // SET_DATASIZE subneg
        assert!(bytes
            .windows(4)
            .any(|w| w == [IAC, SB, COM_PORT_OPTION, SET_DATASIZE]));
        // SET_PARITY subneg
        assert!(bytes
            .windows(4)
            .any(|w| w == [IAC, SB, COM_PORT_OPTION, SET_PARITY]));
        // SET_STOPSIZE subneg
        assert!(bytes
            .windows(4)
            .any(|w| w == [IAC, SB, COM_PORT_OPTION, SET_STOPSIZE]));
    }

    // -----------------------------------------------------------------------
    // Parameter parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_serial_params_defaults() {
        let config = SerialConfig::default();
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "192.168.1.1".to_string());

        let serial = SerialParams::from_params(&params, &config).unwrap();
        assert_eq!(serial.hostname, "192.168.1.1");
        assert_eq!(serial.port, 2001);
        assert_eq!(serial.baud_rate, 9600);
        assert_eq!(serial.data_bits, 8);
        assert_eq!(serial.parity, Parity::None);
        assert_eq!(serial.stop_bits, StopBits::One);
        assert!(serial.rfc2217);
    }

    #[test]
    fn test_serial_params_custom() {
        let config = SerialConfig::default();
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "console.example.com".to_string());
        params.insert("port".to_string(), "7001".to_string());
        params.insert("baud_rate".to_string(), "115200".to_string());
        params.insert("data_bits".to_string(), "7".to_string());
        params.insert("parity".to_string(), "even".to_string());
        params.insert("stop_bits".to_string(), "2".to_string());
        params.insert("rfc2217".to_string(), "false".to_string());

        let serial = SerialParams::from_params(&params, &config).unwrap();
        assert_eq!(serial.hostname, "console.example.com");
        assert_eq!(serial.port, 7001);
        assert_eq!(serial.baud_rate, 115200);
        assert_eq!(serial.data_bits, 7);
        assert_eq!(serial.parity, Parity::Even);
        assert_eq!(serial.stop_bits, StopBits::Two);
        assert!(!serial.rfc2217);
    }

    #[test]
    fn test_serial_params_hyphenated_keys() {
        let config = SerialConfig::default();
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "test".to_string());
        params.insert("baud-rate".to_string(), "38400".to_string());
        params.insert("data-bits".to_string(), "7".to_string());
        params.insert("stop-bits".to_string(), "1.5".to_string());
        params.insert("rfc-2217".to_string(), "true".to_string());

        let serial = SerialParams::from_params(&params, &config).unwrap();
        assert_eq!(serial.baud_rate, 38400);
        assert_eq!(serial.data_bits, 7);
        assert_eq!(serial.stop_bits, StopBits::OnePointFive);
        assert!(serial.rfc2217);
    }

    #[test]
    fn test_serial_params_missing_hostname() {
        let config = SerialConfig::default();
        let params = HashMap::new();
        let result = SerialParams::from_params(&params, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_serial_params_invalid_data_bits_uses_default() {
        let config = SerialConfig::default();
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "test".to_string());
        params.insert("data_bits".to_string(), "9".to_string()); // invalid

        let serial = SerialParams::from_params(&params, &config).unwrap();
        assert_eq!(serial.data_bits, 8); // falls back to default
    }

    #[test]
    fn test_serial_params_invalid_port_uses_default() {
        let config = SerialConfig::default();
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "test".to_string());
        params.insert("port".to_string(), "not_a_number".to_string());

        let serial = SerialParams::from_params(&params, &config).unwrap();
        assert_eq!(serial.port, 2001);
    }

    // -----------------------------------------------------------------------
    // Parity and StopBits parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_parity_from_param() {
        assert_eq!(Parity::from_param("none"), Parity::None);
        assert_eq!(Parity::from_param("None"), Parity::None);
        assert_eq!(Parity::from_param("NONE"), Parity::None);
        assert_eq!(Parity::from_param("odd"), Parity::Odd);
        assert_eq!(Parity::from_param("even"), Parity::Even);
        assert_eq!(Parity::from_param("mark"), Parity::Mark);
        assert_eq!(Parity::from_param("space"), Parity::Space);
        assert_eq!(Parity::from_param("invalid"), Parity::None);
        assert_eq!(Parity::from_param(""), Parity::None);
    }

    #[test]
    fn test_stop_bits_from_param() {
        assert_eq!(StopBits::from_param("1"), StopBits::One);
        assert_eq!(StopBits::from_param("2"), StopBits::Two);
        assert_eq!(StopBits::from_param("1.5"), StopBits::OnePointFive);
        assert_eq!(StopBits::from_param("invalid"), StopBits::One);
        assert_eq!(StopBits::from_param(""), StopBits::One);
    }

    // -----------------------------------------------------------------------
    // Baud rate encoding
    // -----------------------------------------------------------------------

    #[test]
    fn test_baud_rate_encoding_common_values() {
        // Verify 4-byte big-endian encoding for common baud rates
        assert_eq!(9600u32.to_be_bytes(), [0x00, 0x00, 0x25, 0x80]);
        assert_eq!(19200u32.to_be_bytes(), [0x00, 0x00, 0x4B, 0x00]);
        assert_eq!(38400u32.to_be_bytes(), [0x00, 0x00, 0x96, 0x00]);
        assert_eq!(57600u32.to_be_bytes(), [0x00, 0x00, 0xE1, 0x00]);
        assert_eq!(115200u32.to_be_bytes(), [0x00, 0x01, 0xC2, 0x00]);
    }

    // -----------------------------------------------------------------------
    // Telnet IAC stripping
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_telnet_plain_data() {
        let data = b"Hello, World!";
        assert_eq!(strip_telnet_commands(data), data.to_vec());
    }

    #[test]
    fn test_strip_telnet_will_do() {
        // IAC WILL COM_PORT mixed with data
        let mut data = Vec::new();
        data.extend_from_slice(b"AB");
        data.extend_from_slice(&[IAC, WILL, COM_PORT_OPTION]); // stripped
        data.extend_from_slice(b"CD");
        data.extend_from_slice(&[IAC, DO, COM_PORT_OPTION]); // stripped
        data.extend_from_slice(b"EF");
        assert_eq!(strip_telnet_commands(&data), b"ABCDEF".to_vec());
    }

    #[test]
    fn test_strip_telnet_subnegotiation() {
        // IAC SB ... IAC SE should be stripped
        let mut data = Vec::new();
        data.extend_from_slice(b"before");
        data.extend_from_slice(&[IAC, SB, 44, 1, 0x00, 0x00, 0x25, 0x80, IAC, SE]);
        data.extend_from_slice(b"after");
        assert_eq!(strip_telnet_commands(&data), b"beforeafter".to_vec());
    }

    #[test]
    fn test_strip_telnet_escaped_iac() {
        // IAC IAC should become a single 0xFF byte
        let data = vec![0x41, IAC, IAC, 0x42];
        assert_eq!(strip_telnet_commands(&data), vec![0x41, 0xFF, 0x42]);
    }

    #[test]
    fn test_strip_telnet_empty() {
        assert_eq!(strip_telnet_commands(&[]), Vec::<u8>::new());
    }

    #[test]
    fn test_strip_telnet_only_commands() {
        let data = vec![IAC, WILL, COM_PORT_OPTION, IAC, DO, COM_PORT_OPTION];
        assert_eq!(strip_telnet_commands(&data), Vec::<u8>::new());
    }

    // -----------------------------------------------------------------------
    // Default config values
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_serial_config() {
        let config = SerialConfig::default();
        assert_eq!(config.default_port, 2001);
        assert_eq!(config.default_rows, 24);
        assert_eq!(config.default_cols, 80);
        assert_eq!(config.default_baud_rate, 9600);
        assert_eq!(config.default_data_bits, 8);
        assert_eq!(config.default_parity, Parity::None);
        assert_eq!(config.default_stop_bits, StopBits::One);
        assert!(config.default_rfc2217);
    }

    #[test]
    fn test_custom_serial_config() {
        let config = SerialConfig {
            default_port: 3001,
            default_rows: 25,
            default_cols: 132,
            default_baud_rate: 115200,
            default_data_bits: 7,
            default_parity: Parity::Even,
            default_stop_bits: StopBits::Two,
            default_rfc2217: false,
        };
        assert_eq!(config.default_port, 3001);
        assert_eq!(config.default_rows, 25);
        assert_eq!(config.default_cols, 132);
        assert_eq!(config.default_baud_rate, 115200);
        assert_eq!(config.default_data_bits, 7);
        assert_eq!(config.default_parity, Parity::Even);
        assert_eq!(config.default_stop_bits, StopBits::Two);
        assert!(!config.default_rfc2217);
    }
}
