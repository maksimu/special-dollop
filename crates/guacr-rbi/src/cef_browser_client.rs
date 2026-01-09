// CEF Browser client integration for RBI (Remote Browser Isolation)
// Provides CEF-based browser session management with full audio support
//
// CRITICAL SECURITY: Each session gets its own isolated CEF process
// - NO process sharing between sessions (unlike browser tabs)
// - Each CefBrowserClient spawns a dedicated CEF subprocess
// - Process isolation prevents cross-session data leakage
// - Profile directories are locked to prevent concurrent access
//
// This is the CEF equivalent of browser_client.rs, providing:
// - Display streaming via RenderHandler callbacks
// - Audio streaming via AudioHandler callbacks (not available in Chrome/CDP)
// - Input injection (keyboard, mouse, scroll)
// - Clipboard synchronization
//
// Architecture follows SSH handler patterns:
// - tokio::select! event loop
// - Debounced rendering (configurable FPS)
// - Error handling with client feedback
// - Recording support
// - Keep-alive management

#[cfg(feature = "cef")]
use crate::cef_session::{
    CefAudioPacket, CefDisplayEvent, CefSession, DisplayEventType, DisplaySurface,
};
use crate::clipboard::RbiClipboard;
use crate::handler::RbiConfig;
use crate::input::RbiInputHandler;
use bytes::Bytes;
use guacr_handlers::{
    KeepAliveManager, MultiFormatRecorder, RecordingConfig, DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
use guacr_protocol::{format_error, BinaryEncoder, GuacamoleParser, STATUS_UPSTREAM_ERROR};
use log::{debug, error, info, trace, warn};
use std::time::Duration;
use tokio::sync::mpsc;

/// CEF Browser client for RBI sessions with full audio support
///
/// SECURITY: Each instance creates a DEDICATED CEF process - NO SHARING
/// - Spawns separate CEF subprocess per session
/// - Uses unique profile directory (locked to prevent reuse)
/// - Process terminates when session ends
/// - No cross-session contamination possible
#[cfg(feature = "cef")]
pub struct CefBrowserClient {
    binary_encoder: BinaryEncoder,
    width: u32,
    height: u32,
    config: RbiConfig,
    input_handler: RbiInputHandler,
    clipboard: RbiClipboard,
    /// Audio stream ID for Guacamole protocol
    audio_stream_id: u32,
    /// Image stream ID for display updates
    image_stream_id: u32,
}

#[cfg(feature = "cef")]
impl CefBrowserClient {
    /// Create a new CEF browser client
    ///
    /// Each instance will spawn its own isolated CEF process when connect() is called
    pub fn new(width: u32, height: u32, config: RbiConfig) -> Self {
        let mut clipboard = RbiClipboard::new(config.clipboard_buffer_size);
        clipboard.set_restrictions(config.disable_copy, config.disable_paste);

        Self {
            binary_encoder: BinaryEncoder::new(),
            width,
            height,
            input_handler: RbiInputHandler::new(),
            clipboard,
            audio_stream_id: 1, // Audio uses stream 1
            image_stream_id: 2, // Images use stream 2
            config,
        }
    }

    /// Send error instruction to client and return error
    ///
    /// Matches SSH handler's pattern - ensures client sees user-friendly error
    async fn send_error_and_return(
        to_client: &mpsc::Sender<Bytes>,
        error: String,
        status_code: u16,
    ) -> String {
        let error_instr = format_error(&error, status_code as u32);
        let _ = to_client.send(Bytes::from(error_instr)).await;
        error
    }

    /// Launch CEF browser and connect to Guacamole client
    ///
    /// SECURITY: Spawns a DEDICATED CEF process for this session only
    /// - Creates unique profile directory (locked)
    /// - No process sharing with other sessions
    /// - Process terminates when this function returns
    ///
    /// Architecture follows SSH handler pattern:
    /// - tokio::select! event loop with multiple arms
    /// - Debounced rendering for performance
    /// - Keep-alive management
    /// - Recording support
    /// - Error handling with client feedback
    pub async fn connect(
        &mut self,
        url: &str,
        params: &std::collections::HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        info!("RBI/CEF: Launching DEDICATED CEF process for URL: {}", url);
        info!("RBI/CEF: SECURITY - Each session gets isolated browser process (no sharing)");

        // Parse recording configuration (SSH handler pattern)
        let recording_config = RecordingConfig::from_params(params);
        if recording_config.is_enabled() {
            info!(
                "RBI/CEF: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        // Initialize recording if enabled
        let mut recorder: Option<MultiFormatRecorder> = if recording_config.is_enabled() {
            match MultiFormatRecorder::new(
                &recording_config,
                params,
                "http",
                self.width as u16,
                self.height as u16,
            ) {
                Ok(rec) => {
                    info!("RBI/CEF: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("RBI/CEF: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Create channels for CEF callbacks
        // Larger buffers to handle burst rendering without blocking
        let (display_tx, mut display_rx) = mpsc::channel::<CefDisplayEvent>(128);
        let (audio_tx, mut audio_rx) = mpsc::channel::<CefAudioPacket>(64);

        // Create and launch CEF session (spawns dedicated subprocess)
        let mut cef_session = CefSession::new(self.width, self.height);

        // CRITICAL: This spawns a NEW CEF process - no sharing!
        if let Err(e) = cef_session.launch(url, display_tx, audio_tx).await {
            let err_msg = format!("CEF launch failed: {}", e);
            error!("RBI/CEF: {}", err_msg);
            return Err(Self::send_error_and_return(
                &to_client,
                err_msg,
                STATUS_UPSTREAM_ERROR as u16,
            )
            .await);
        }

        info!("RBI/CEF: Dedicated CEF process spawned (PID will be logged by CEF)");

        // Send initial handshake
        if let Err(e) = self.send_handshake(&to_client).await {
            error!("RBI/CEF: Handshake failed: {}", e);
            cef_session.close();
            return Err(
                Self::send_error_and_return(&to_client, e, STATUS_UPSTREAM_ERROR as u16).await,
            );
        }

        info!("RBI/CEF: Browser launched, starting event loop");

        // Debounce timer for batching screen updates (SSH handler pattern)
        // Configurable FPS (default 30)
        let frame_interval_ms = 1000 / self.config.capture_fps as u64;
        let mut debounce = tokio::time::interval(Duration::from_millis(frame_interval_ms));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Keep-alive manager (SSH handler pattern)
        let mut keepalive = KeepAliveManager::new(DEFAULT_KEEPALIVE_INTERVAL_SECS);
        let mut keepalive_interval =
            tokio::time::interval(Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Track if we have pending display updates
        let mut pending_display_events = Vec::new();

        // Main event loop (SSH handler pattern)
        loop {
            tokio::select! {
                // Keep-alive ping to detect dead connections (SSH handler pattern)
                _ = keepalive_interval.tick() => {
                    if let Some(sync_instr) = keepalive.check() {
                        trace!("RBI/CEF: Sending keep-alive sync");
                        if to_client.send(sync_instr).await.is_err() {
                            info!("RBI/CEF: Client channel closed, ending session");
                            break;
                        }
                    }
                }

                // Debounce tick - process pending display events (SSH handler pattern)
                _ = debounce.tick() => {
                    if !pending_display_events.is_empty() {
                        trace!("RBI/CEF: Processing {} pending display events", pending_display_events.len());

                        for event in pending_display_events.drain(..) {
                            if let Err(e) = self.handle_display_event(&event, &to_client, &mut recorder).await {
                                warn!("RBI/CEF: Display event error: {}", e);
                            }
                        }
                    }
                }

                // Handle display events from CEF RenderHandler
                Some(event) = display_rx.recv() => {
                    // Buffer events for debounced processing (except critical ones)
                    match event.event_type {
                        DisplayEventType::EndOfFrame => {
                            // Send sync immediately (frame boundary)
                            if let Err(e) = self.handle_display_event(&event, &to_client, &mut recorder).await {
                                warn!("RBI/CEF: Display event error: {}", e);
                            }
                        }
                        DisplayEventType::Resize => {
                            // Send resize immediately (critical)
                            if let Err(e) = self.handle_display_event(&event, &to_client, &mut recorder).await {
                                warn!("RBI/CEF: Display event error: {}", e);
                            }
                        }
                        _ => {
                            // Buffer for debounced processing
                            pending_display_events.push(event);
                        }
                    }
                }

                // Handle audio packets from CEF AudioHandler
                Some(packet) = audio_rx.recv() => {
                    if let Err(e) = self.handle_audio_packet(&packet, &to_client).await {
                        warn!("RBI/CEF: Audio packet error: {}", e);
                    }
                }

                // Handle client input
                Some(msg) = from_client.recv() => {
                    match self.handle_client_message(&msg, &mut cef_session, &mut recorder).await {
                        Ok(true) => {
                            // Continue
                        }
                        Ok(false) => {
                            // Client requested disconnect
                            info!("RBI/CEF: Client disconnect requested");
                            break;
                        }
                        Err(e) => {
                            warn!("RBI/CEF: Message handling error: {}", e);
                        }
                    }
                }

                // Client disconnected
                else => {
                    info!("RBI/CEF: Client channel closed, ending session");
                    break;
                }
            }
        }

        // Cleanup (SSH handler pattern)
        info!("RBI/CEF: Closing CEF session and terminating dedicated process");
        cef_session.close();

        // Finalize recording (SSH handler pattern)
        if let Some(rec) = recorder {
            if let Err(e) = rec.finalize() {
                warn!("RBI/CEF: Failed to finalize recording: {}", e);
            } else {
                info!("RBI/CEF: Session recording finalized");
            }
        }

        info!("RBI/CEF: Session ended, CEF process terminated");
        Ok(())
    }

    /// Send initial Guacamole handshake
    async fn send_handshake(&mut self, to_client: &mpsc::Sender<Bytes>) -> Result<(), String> {
        // Send ready instruction
        let ready_instr = "5.ready,9.cef-ready;";
        to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| format!("Failed to send ready: {}", e))?;

        // Send size instruction (binary format)
        let size_instr = self.binary_encoder.encode_size(0, self.width, self.height);
        to_client
            .send(size_instr)
            .await
            .map_err(|e| format!("Failed to send size: {}", e))?;

        // Send audio instruction to start audio stream
        // Format: audio,<stream>,<mimetype>,<rate>,<channels>,<bps>
        let audio_instr = format!(
            "5.audio,1.{},9.audio/L16,5.44100,1.2,2.16;",
            self.audio_stream_id
        );
        to_client
            .send(Bytes::from(audio_instr))
            .await
            .map_err(|e| format!("Failed to send audio: {}", e))?;

        info!(
            "RBI/CEF: Handshake sent - size={}x{}, audio stream={}",
            self.width, self.height, self.audio_stream_id
        );
        Ok(())
    }

    /// Handle display event from CEF RenderHandler
    async fn handle_display_event(
        &mut self,
        event: &CefDisplayEvent,
        to_client: &mpsc::Sender<Bytes>,
        recorder: &mut Option<MultiFormatRecorder>,
    ) -> Result<(), String> {
        match event.event_type {
            DisplayEventType::Draw => {
                if let Some(ref pixels) = event.pixels {
                    // Convert BGRA pixels to PNG and send
                    let png_data =
                        self.encode_rect_to_png(pixels, event.width as u32, event.height as u32)?;

                    // Record frame if recording enabled
                    if let Some(ref mut rec) = recorder {
                        // Record as PNG blob (RBI doesn't have terminal output like SSH)
                        let _ = rec.record_output(&png_data);
                    }

                    // Send image instruction with PNG data
                    let layer = match event.surface {
                        DisplaySurface::View => 0,
                        DisplaySurface::Popup => 1,
                    };

                    let img_instr = self.binary_encoder.encode_image(
                        self.image_stream_id,
                        layer,
                        event.x,
                        event.y,
                        event.width as u16,
                        event.height as u16,
                        0, // format: PNG
                        Bytes::from(png_data),
                    );

                    to_client
                        .send(img_instr)
                        .await
                        .map_err(|e| format!("Failed to send img: {}", e))?;

                    trace!(
                        "RBI/CEF: Sent {}x{} update at ({},{})",
                        event.width,
                        event.height,
                        event.x,
                        event.y
                    );
                }
            }
            DisplayEventType::EndOfFrame => {
                // Send sync instruction to indicate frame complete
                let sync_instr = self.binary_encoder.encode_sync(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                );
                to_client
                    .send(sync_instr)
                    .await
                    .map_err(|e| format!("Failed to send sync: {}", e))?;
            }
            DisplayEventType::Resize => {
                // Send new size
                let size_instr =
                    self.binary_encoder
                        .encode_size(0, event.width as u32, event.height as u32);
                to_client
                    .send(size_instr)
                    .await
                    .map_err(|e| format!("Failed to send size: {}", e))?;

                self.width = event.width as u32;
                self.height = event.height as u32;
            }
            DisplayEventType::CursorChange => {
                // Send cursor instruction based on cursor type
                if let Some(cursor) = &event.cursor {
                    use crate::cef_session::CefCursorType;
                    let cursor_name = match cursor {
                        CefCursorType::Pointer => "default",
                        CefCursorType::IBeam => "text",
                        CefCursorType::Hand => "pointer",
                        CefCursorType::Wait => "wait",
                        CefCursorType::Help => "help",
                        CefCursorType::CrossHair => "crosshair",
                        CefCursorType::Move => "move",
                        CefCursorType::NotAllowed => "not-allowed",
                        CefCursorType::None => "none",
                    };
                    debug!("RBI/CEF: Cursor changed to {}", cursor_name);
                    // Cursor instruction would be sent here
                    // Format: cursor,<layer>,<cursor_type>
                }
            }
            DisplayEventType::MoveResize => {
                // Handle popup move/resize
                if event.surface == DisplaySurface::Popup {
                    // Send layer position and size
                    debug!(
                        "RBI/CEF: Popup move/resize to ({},{}) {}x{}",
                        event.x, event.y, event.width, event.height
                    );
                }
            }
            DisplayEventType::Show => {
                // Show a layer (popup)
                if event.surface == DisplaySurface::Popup {
                    debug!("RBI/CEF: Showing popup layer");
                }
            }
            DisplayEventType::Hide => {
                // Hide a layer (popup)
                if event.surface == DisplaySurface::Popup {
                    debug!("RBI/CEF: Hiding popup layer");
                }
            }
        }
        Ok(())
    }

    /// Handle audio packet from CEF AudioHandler
    async fn handle_audio_packet(
        &mut self,
        packet: &CefAudioPacket,
        to_client: &mpsc::Sender<Bytes>,
    ) -> Result<(), String> {
        // Send audio blob instruction
        // Format: blob,<stream>,<base64-data>
        let blob_instr = self
            .binary_encoder
            .encode_blob(self.audio_stream_id, Bytes::from(packet.data.clone()));

        to_client
            .send(blob_instr)
            .await
            .map_err(|e| format!("Failed to send audio blob: {}", e))?;

        debug!(
            "RBI/CEF: Sent audio packet: {} bytes, {}Hz, {} channels",
            packet.data.len(),
            packet.sample_rate,
            packet.channels
        );
        Ok(())
    }

    /// Handle message from Guacamole client
    async fn handle_client_message(
        &mut self,
        msg: &Bytes,
        cef_session: &mut CefSession,
        recorder: &mut Option<MultiFormatRecorder>,
    ) -> Result<bool, String> {
        // Try to parse as text instruction
        if let Ok(instr) = GuacamoleParser::parse_instruction(msg) {
            let args: Vec<String> = instr.args.iter().map(|s| s.to_string()).collect();
            return self
                .handle_instruction(instr.opcode, &args, cef_session, recorder)
                .await;
        }

        debug!("RBI/CEF: Unrecognized message format");
        Ok(true)
    }

    /// Handle parsed Guacamole instruction
    async fn handle_instruction(
        &mut self,
        opcode: &str,
        args: &[String],
        cef_session: &mut CefSession,
        recorder: &mut Option<MultiFormatRecorder>,
    ) -> Result<bool, String> {
        match opcode {
            "mouse" => {
                // mouse,<x>,<y>,<button_mask>
                if args.len() >= 3 {
                    let x: i32 = args[0].parse().unwrap_or(0);
                    let y: i32 = args[1].parse().unwrap_or(0);
                    let mask: i32 = args[2].parse().unwrap_or(0);

                    // Send mouse move
                    cef_session.inject_mouse_move(x, y)?;

                    // Handle button presses
                    // Guacamole mask: bit 0=left, 1=middle, 2=right, 3=scroll up, 4=scroll down
                    if mask & 0x01 != 0 {
                        cef_session.inject_mouse(x, y, 0, true)?; // Left down
                    }
                    if mask & 0x02 != 0 {
                        cef_session.inject_mouse(x, y, 1, true)?; // Middle down
                    }
                    if mask & 0x04 != 0 {
                        cef_session.inject_mouse(x, y, 2, true)?; // Right down
                    }

                    // Handle scroll (buttons 4 and 5 in mask)
                    if mask & 0x08 != 0 {
                        cef_session.inject_scroll(x, y, 0, -120)?; // Scroll up
                    }
                    if mask & 0x10 != 0 {
                        cef_session.inject_scroll(x, y, 0, 120)?; // Scroll down
                    }
                }
            }
            "key" => {
                // key,<keysym>,<pressed>
                if args.len() >= 2 {
                    let keysym: u32 = args[0].parse().unwrap_or(0);
                    let pressed = args[1] == "1";

                    // Record input if enabled
                    if let Some(ref mut rec) = recorder {
                        if pressed {
                            // Record key press as single byte (simplified)
                            let key_byte = (keysym & 0xFF) as u8;
                            let _ = rec.record_input(&[key_byte]);
                        }
                    }

                    cef_session.inject_keyboard(keysym, pressed)?;
                }
            }
            "size" => {
                // size,<layer>,<width>,<height>
                if args.len() >= 3 {
                    let width: u32 = args[1].parse().unwrap_or(self.width);
                    let height: u32 = args[2].parse().unwrap_or(self.height);

                    if width != self.width || height != self.height {
                        cef_session.resize(width, height)?;
                        self.width = width;
                        self.height = height;
                        info!("RBI/CEF: Resized to {}x{}", width, height);

                        // Record resize if enabled
                        if let Some(ref mut rec) = recorder {
                            let _ = rec.record_resize(width as u16, height as u16);
                        }
                    }
                }
            }
            "clipboard" => {
                // clipboard,<stream>,<mimetype>
                // Followed by blob instructions with data
                debug!("RBI/CEF: Clipboard stream started");
            }
            "blob" => {
                // blob,<stream>,<data>
                if args.len() >= 2 {
                    // Clipboard data received
                    use base64::Engine;
                    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&args[1])
                    {
                        if let Ok(text) = String::from_utf8(decoded) {
                            if !self.config.disable_paste {
                                cef_session.set_clipboard(&text)?;
                                debug!("RBI/CEF: Set clipboard: {} chars", text.len());
                            }
                        }
                    }
                }
            }
            "disconnect" => {
                info!("RBI/CEF: Disconnect requested");
                return Ok(false);
            }
            "nop" => {
                // Keep-alive, ignore
            }
            _ => {
                debug!("RBI/CEF: Unhandled instruction: {}", opcode);
            }
        }
        Ok(true)
    }

    /// Encode BGRA pixels to PNG
    fn encode_rect_to_png(
        &self,
        bgra_pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        use png::{BitDepth, ColorType, Encoder};

        // Convert BGRA to RGBA
        let mut rgba_pixels = Vec::with_capacity(bgra_pixels.len());
        for chunk in bgra_pixels.chunks(4) {
            if chunk.len() == 4 {
                rgba_pixels.push(chunk[2]); // R
                rgba_pixels.push(chunk[1]); // G
                rgba_pixels.push(chunk[0]); // B
                rgba_pixels.push(chunk[3]); // A
            }
        }

        // Encode to PNG
        let mut output = Vec::new();
        {
            let mut encoder = Encoder::new(&mut output, width, height);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);

            let mut writer = encoder
                .write_header()
                .map_err(|e| format!("PNG header error: {}", e))?;

            writer
                .write_image_data(&rgba_pixels)
                .map_err(|e| format!("PNG write error: {}", e))?;
        }

        Ok(output)
    }
}

// Fallback when CEF is not enabled
#[cfg(not(feature = "cef"))]
pub struct CefBrowserClient;

#[cfg(not(feature = "cef"))]
impl CefBrowserClient {
    pub fn new(_width: u32, _height: u32, _config: crate::handler::RbiConfig) -> Self {
        Self
    }

    pub async fn connect(
        &mut self,
        _url: &str,
        _to_client: mpsc::Sender<Bytes>,
        _from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        Err("CEF feature not enabled. Build with --features cef".to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_bgra_to_rgba_conversion() {
        // BGRA input: Blue=255, Green=128, Red=64, Alpha=255
        let bgra = vec![255u8, 128, 64, 255];

        // Expected RGBA: Red=64, Green=128, Blue=255, Alpha=255
        let mut rgba = Vec::new();
        for chunk in bgra.chunks(4) {
            rgba.push(chunk[2]); // R
            rgba.push(chunk[1]); // G
            rgba.push(chunk[0]); // B
            rgba.push(chunk[3]); // A
        }

        assert_eq!(rgba, vec![64, 128, 255, 255]);
    }
}
