use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    // Connection utilities
    connect_tcp_with_timeout,
    is_mouse_event_allowed_readonly,
    EventBasedHandler,
    EventCallback,
    HandlerError,
    // Security
    HandlerSecuritySettings,
    HandlerStats,
    HealthStatus,
    KeepAliveManager,
    MultiFormatRecorder,
    ProtocolHandler,
    // Recording
    RecordingConfig,
    DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
use guacr_protocol::{BinaryEncoder, GuacamoleParser};
use guacr_terminal::{FrameBuffer, ScrollDetector, ScrollDirection};
use image::{ImageEncoder, RgbaImage};
use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::mpsc;

use crate::vnc_protocol::VncProtocol;

/// VNC protocol handler
///
/// Connects to VNC servers and provides remote desktop access via the Guacamole protocol.
///
/// ## IMPORTANT: Rendering Method
///
/// VNC MUST use PNG images (NOT Guacamole drawing instructions like rect/cfill).
/// Why:
/// - VNC streams framebuffer data: arbitrary graphics, photos, complex UI
/// - Drawing instructions only work for simple colored rectangles
/// - Cannot represent the visual complexity of VNC sessions
/// - Expected bandwidth: ~50-200KB/frame (acceptable for graphics-rich content)
#[derive(Clone)]
pub struct VncHandler {
    config: VncConfig,
}

#[derive(Debug, Clone)]
pub struct VncConfig {
    pub default_port: u16,
    pub default_width: u32,
    pub default_height: u32,
    /// JPEG quality for image encoding (1-100, default 85)
    /// Higher = better quality but larger files
    /// 85 is optimal balance for RDP-like performance
    pub jpeg_quality: u8,
    /// Use JPEG encoding instead of PNG (default true for bandwidth savings)
    pub use_jpeg: bool,
    /// Client supports WebP format (40% smaller than JPEG)
    pub supports_webp: bool,
    /// Client supports JPEG format
    pub supports_jpeg: bool,
    /// Frame rate limit in FPS (default 30)
    pub frame_rate: u32,
}

impl Default for VncConfig {
    fn default() -> Self {
        Self {
            default_port: 5900,
            default_width: 1920,
            default_height: 1080,
            jpeg_quality: 85,     // Same as RDP for consistency
            use_jpeg: true,       // Enable by default for bandwidth savings
            supports_webp: false, // Will be overridden by client capabilities
            supports_jpeg: false, // Will be overridden by client capabilities
            frame_rate: 30,       // 30 FPS default (can go up to 60)
        }
    }
}

impl VncHandler {
    pub fn new(config: VncConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(VncConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for VncHandler {
    fn name(&self) -> &str {
        "vnc"
    }

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("VNC handler starting connection");

        // Parse VNC settings
        let settings = VncSettings::from_params(&params, &self.config)
            .map_err(HandlerError::InvalidParameter)?;

        // Create VNC client
        let mut client = VncClient::new(
            settings.width,
            settings.height,
            settings.read_only,
            settings.security.clone(),
            settings.recording_config.clone(),
            settings.jpeg_quality,
            settings.use_jpeg,
            settings.supports_webp,
            settings.supports_jpeg,
            settings.frame_rate,
            to_client,
            &params,
        );

        // Connect and run session
        client
            .connect(
                &settings.hostname,
                settings.port,
                settings.password.as_deref(),
                from_client,
                #[cfg(feature = "sftp")]
                Some(&settings),
                #[cfg(not(feature = "sftp"))]
                None,
            )
            .await
            .map_err(HandlerError::ConnectionFailed)?;

        info!("VNC handler connection ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

// Event-based handler implementation
#[async_trait]
impl EventBasedHandler for VncHandler {
    fn name(&self) -> &str {
        "vnc"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Use common event adapter helper (eliminates boilerplate)
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

// ============================================================================
// VNC Settings - Parameter parsing and validation
// ============================================================================

/// VNC connection settings
#[derive(Debug, Clone)]
pub struct VncSettings {
    pub hostname: String,
    pub port: u16,
    pub password: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Read-only mode - blocks keyboard/mouse input
    pub read_only: bool,
    /// Security settings
    pub security: HandlerSecuritySettings,
    /// Recording configuration
    pub recording_config: RecordingConfig,
    /// JPEG quality (1-100)
    pub jpeg_quality: u8,
    /// Use JPEG encoding (vs PNG)
    pub use_jpeg: bool,
    /// Client supports WebP format
    pub supports_webp: bool,
    /// Client supports JPEG format
    pub supports_jpeg: bool,
    /// Frame rate limit (FPS)
    pub frame_rate: u32,
    #[cfg(feature = "sftp")]
    pub enable_sftp: bool,
    #[cfg(feature = "sftp")]
    pub sftp_hostname: Option<String>,
    #[cfg(feature = "sftp")]
    pub sftp_username: Option<String>,
    #[cfg(feature = "sftp")]
    pub sftp_password: Option<String>,
    #[cfg(feature = "sftp")]
    pub sftp_private_key: Option<String>,
    #[cfg(feature = "sftp")]
    pub sftp_private_key_passphrase: Option<String>,
    #[cfg(feature = "sftp")]
    pub sftp_port: u16,
}

impl VncSettings {
    pub fn from_params(
        params: &HashMap<String, String>,
        defaults: &VncConfig,
    ) -> Result<Self, String> {
        let hostname = params
            .get("hostname")
            .ok_or_else(|| "Missing required parameter: hostname".to_string())?
            .clone();

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(defaults.default_port);

        let password = params.get("password").cloned();

        // IMPORTANT: Always use DEFAULT size during initialization (like guacd does)
        // The client will send a resize instruction with actual browser dimensions after handshake
        // This prevents "half screen" display issues
        info!("VNC: Using default handshake size - will resize after client connects");
        let width = defaults.default_width;
        let height = defaults.default_height;

        // Parse security settings
        let security = HandlerSecuritySettings::from_params(params);
        let read_only = security.read_only;
        info!(
            "VNC: Security settings - read_only={}, disable_copy={}, disable_paste={}",
            security.read_only, security.disable_copy, security.disable_paste
        );

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(params);
        if recording_config.is_enabled() {
            info!(
                "VNC: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        // Parse image encoding settings
        let jpeg_quality = params
            .get("jpeg_quality")
            .and_then(|q| q.parse().ok())
            .unwrap_or(defaults.jpeg_quality)
            .clamp(1, 100);

        let use_jpeg = params
            .get("use_jpeg")
            .map(|v| v == "true")
            .unwrap_or(defaults.use_jpeg);

        let frame_rate = params
            .get("frame_rate")
            .and_then(|f| f.parse().ok())
            .unwrap_or(defaults.frame_rate)
            .clamp(1, 60);

        // Parse client image format support
        let supported_formats = params
            .get("image")
            .map(|s| s.split(',').map(|f| f.trim()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["image/png"]);

        let supports_webp = supported_formats.iter().any(|f| f.contains("webp"));
        let supports_jpeg = supported_formats.iter().any(|f| f.contains("jpeg"));

        info!(
            "VNC: Client image support - WebP: {}, JPEG: {}, formats: {:?}",
            supports_webp, supports_jpeg, supported_formats
        );

        info!(
            "VNC: Image encoding - use_jpeg={}, quality={}, frame_rate={} FPS",
            use_jpeg, jpeg_quality, frame_rate
        );

        #[cfg(feature = "sftp")]
        let (
            enable_sftp,
            sftp_hostname,
            sftp_username,
            sftp_password,
            sftp_private_key,
            sftp_private_key_passphrase,
            sftp_port,
        ) = {
            let enable_sftp = params
                .get("enableSftp")
                .or_else(|| params.get("enable_sftp"))
                .map(|v| v == "true")
                .unwrap_or(false);

            if enable_sftp {
                let hostname = params
                    .get("sftphostname")
                    .or_else(|| params.get("sftp_hostname"))
                    .ok_or_else(|| "sftphostname required when enableSftp=true".to_string())?
                    .clone();
                let username = params
                    .get("sftpusername")
                    .or_else(|| params.get("sftp_username"))
                    .ok_or_else(|| "sftpusername required when enableSftp=true".to_string())?
                    .clone();
                let password = params
                    .get("sftppassword")
                    .or_else(|| params.get("sftp_password"))
                    .cloned();
                let private_key = params
                    .get("sftpprivatekey")
                    .or_else(|| params.get("sftp_private_key"))
                    .cloned();
                let passphrase = params
                    .get("sftppassphrase")
                    .or_else(|| params.get("sftp_private_key_passphrase"))
                    .cloned();
                let port = params
                    .get("sftpport")
                    .or_else(|| params.get("sftp_port"))
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(22);

                (
                    enable_sftp,
                    Some(hostname),
                    Some(username),
                    password,
                    private_key,
                    passphrase,
                    port,
                )
            } else {
                (false, None, None, None, None, None, 22)
            }
        };

        info!(
            "VNC Settings: {}:{}, {}x{}, read_only={}",
            hostname, port, width, height, read_only
        );

        Ok(Self {
            hostname,
            port,
            password,
            width,
            height,
            read_only,
            security,
            recording_config,
            jpeg_quality,
            use_jpeg,
            supports_webp,
            supports_jpeg,
            frame_rate,
            #[cfg(feature = "sftp")]
            enable_sftp,
            #[cfg(feature = "sftp")]
            sftp_hostname,
            #[cfg(feature = "sftp")]
            sftp_username,
            #[cfg(feature = "sftp")]
            sftp_password,
            #[cfg(feature = "sftp")]
            sftp_private_key,
            #[cfg(feature = "sftp")]
            sftp_private_key_passphrase,
            #[cfg(feature = "sftp")]
            sftp_port,
        })
    }
}

// ============================================================================
// VNC Client - Connection and event loop
// ============================================================================

/// VNC client wrapper for VNC connections
struct VncClient {
    framebuffer: FrameBuffer,
    binary_encoder: BinaryEncoder,
    stream_id: u32,
    width: u32,
    height: u32,
    /// Read-only mode - blocks keyboard/mouse input
    read_only: bool,
    /// Security settings (includes connection timeout)
    security: HandlerSecuritySettings,
    /// Recording configuration
    #[allow(dead_code)] // TODO: Use for .ses recording
    recording_config: RecordingConfig,
    /// Active recorder
    #[allow(dead_code)] // TODO: Use for .ses recording
    recorder: Option<MultiFormatRecorder>,
    /// Scroll detector for bandwidth optimization (shared with RDP)
    scroll_detector: ScrollDetector,
    /// JPEG quality (1-100)
    jpeg_quality: u8,
    /// Use JPEG encoding (vs PNG)
    use_jpeg: bool,
    /// Client supports WebP format
    supports_webp: bool,
    /// Client supports JPEG format
    supports_jpeg: bool,
    /// Frame rate limit (FPS) - currently unused for VNC (server controls rate)
    #[allow(dead_code)]
    frame_rate: u32,
    #[cfg(feature = "sftp")]
    sftp_session: Option<russh_sftp::client::SftpSession>,
    to_client: mpsc::Sender<Bytes>,
}

impl VncClient {
    #[allow(clippy::too_many_arguments)]
    fn new(
        width: u32,
        height: u32,
        read_only: bool,
        security: HandlerSecuritySettings,
        recording_config: RecordingConfig,
        jpeg_quality: u8,
        use_jpeg: bool,
        supports_webp: bool,
        supports_jpeg: bool,
        frame_rate: u32,
        to_client: mpsc::Sender<Bytes>,
        params: &HashMap<String, String>,
    ) -> Self {
        // Initialize recording if enabled
        let recorder = if recording_config.is_enabled() {
            match MultiFormatRecorder::new(
                &recording_config,
                params,
                "vnc",
                width as u16,
                height as u16,
            ) {
                Ok(rec) => {
                    info!("VNC: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("VNC: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            framebuffer: FrameBuffer::new(width, height),
            binary_encoder: BinaryEncoder::new(),
            stream_id: 1,
            width,
            height,
            read_only,
            security,
            recording_config,
            recorder,
            scroll_detector: ScrollDetector::new(width, height),
            jpeg_quality,
            use_jpeg,
            supports_webp,
            supports_jpeg,
            frame_rate,
            #[cfg(feature = "sftp")]
            sftp_session: None,
            to_client,
        }
    }

    /// Encode RGBA framebuffer data as JPEG
    ///
    /// Converts RGBA pixels to RGB and encodes as JPEG with specified quality.
    /// This provides 5-10x bandwidth reduction compared to PNG encoding.
    ///
    /// # Arguments
    ///
    /// * `data` - RGBA pixel data (4 bytes per pixel)
    /// * `width` - Image width in pixels
    /// * `height` - Image height in pixels
    /// * `quality` - JPEG quality (1-100, higher = better quality)
    ///
    /// # Returns
    ///
    /// JPEG-encoded image data
    fn encode_jpeg(data: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
        let img = RgbaImage::from_raw(width, height, data.to_vec())
            .ok_or_else(|| "Invalid image dimensions".to_string())?;

        let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

        let mut jpeg_data = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder
            .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e| format!("JPEG encode failed: {}", e))?;

        Ok(jpeg_data)
    }

    /// Encode RGBA data as WebP (lossy)
    fn encode_webp_lossy(
        data: &[u8],
        width: u32,
        height: u32,
        quality: f32,
    ) -> Result<Vec<u8>, String> {
        use webp::{Encoder, WebPMemory};

        let encoder = Encoder::from_rgba(data, width, height);
        let webp: WebPMemory = encoder.encode(quality);
        Ok(webp.to_vec())
    }

    /// Encode RGBA data as WebP (lossless)
    fn encode_webp_lossless(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        use webp::{Encoder, WebPMemory};

        let encoder = Encoder::from_rgba(data, width, height);
        let webp: WebPMemory = encoder.encode_lossless();
        Ok(webp.to_vec())
    }

    /// Encode a framebuffer region using configured encoding (WebP, JPEG, or PNG)
    ///
    /// Uses WebP by default for 40% bandwidth savings vs JPEG, with fallback.
    fn encode_region(&mut self, rect: guacr_terminal::FrameRect) -> Result<Vec<u8>, String> {
        // Calculate update size for smart encoding
        let total_pixels = self.width * self.height;
        let rect_pixels = rect.width * rect.height;
        let is_large_update = rect_pixels > total_pixels / 10; // >10% of screen

        if self.supports_webp {
            // WebP: Best of both worlds
            let region_pixels = self.framebuffer.get_region_pixels(rect);
            if is_large_update {
                // Large update: WebP lossy
                let quality = self.jpeg_quality as f32 / 100.0;
                Self::encode_webp_lossy(&region_pixels, rect.width, rect.height, quality)
            } else {
                // Small update: WebP lossless
                Self::encode_webp_lossless(&region_pixels, rect.width, rect.height)
            }
        } else if self.supports_jpeg && self.use_jpeg {
            // Fallback: JPEG encoding (only if client supports it)
            let region_pixels = self.framebuffer.get_region_pixels(rect);
            Self::encode_jpeg(&region_pixels, rect.width, rect.height, self.jpeg_quality)
        } else {
            // Fallback: PNG encoding (always supported)
            self.framebuffer
                .encode_region(rect)
                .map_err(|e| format!("PNG encoding failed: {}", e))
        }
    }

    /// Send sync instruction for frame timing
    ///
    /// Helps with session recording playback timing and client-side frame synchronization.
    async fn send_sync(&self) -> Result<(), String> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let sync_instr = format!("4.sync,{}.{};", timestamp.to_string().len(), timestamp);

        self.to_client
            .send(Bytes::from(sync_instr))
            .await
            .map_err(|e| format!("Failed to send sync: {}", e))
    }

    async fn connect(
        &mut self,
        hostname: &str,
        port: u16,
        password: Option<&str>,
        mut from_client: mpsc::Receiver<Bytes>,
        #[cfg(feature = "sftp")] settings: Option<&VncSettings>,
        #[cfg(not(feature = "sftp"))] _settings: Option<&VncSettings>,
    ) -> Result<(), String> {
        info!(
            "VNC: Connecting to {}:{} (timeout: {}s)",
            hostname, port, self.security.connection_timeout_secs
        );

        // Connect with timeout (matches guacd behavior)
        let mut stream =
            connect_tcp_with_timeout((hostname, port), self.security.connection_timeout_secs)
                .await
                .map_err(|e| format!("{}", e))?;

        info!("VNC: TCP connection established");

        let (_version, _pixel_format, server_width, server_height, server_name) =
            VncProtocol::handshake(&mut stream, password)
                .await
                .map_err(|e| format!("VNC handshake failed: {}", e))?;

        info!(
            "VNC: Handshake complete - {}x{}, server: {}",
            server_width, server_height, server_name
        );

        self.width = server_width as u32;
        self.height = server_height as u32;
        self.framebuffer = FrameBuffer::new(self.width, self.height);

        // Send ready instruction with proper LENGTH.VALUE format
        let ready_value = "vnc-ready";
        let ready_instr = format!("5.ready,{}.{};", ready_value.len(), ready_value);
        self.to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| format!("Failed to send ready: {}", e))?;

        let size_instr = self.binary_encoder.encode_size(0, self.width, self.height);
        self.to_client
            .send(size_instr)
            .await
            .map_err(|e| format!("Failed to send size: {}", e))?;

        #[cfg(feature = "sftp")]
        if let Some(settings) = settings {
            if settings.enable_sftp {
                let sftp_hostname = settings.sftp_hostname.as_ref().unwrap();
                let sftp_username = settings.sftp_username.as_ref().unwrap();
                match crate::sftp_integration::establish_sftp_session(
                    sftp_hostname,
                    settings.sftp_port,
                    sftp_username,
                    settings.sftp_password.as_deref(),
                    settings.sftp_private_key.as_deref(),
                    settings.sftp_private_key_passphrase.as_deref(),
                )
                .await
                {
                    Ok(sftp) => {
                        self.sftp_session = Some(sftp);
                        info!("VNC: SFTP session established");
                    }
                    Err(e) => {
                        warn!("VNC: Failed to establish SFTP session: {}", e);
                    }
                }
            }
        }

        VncProtocol::send_framebuffer_update_request(
            &mut stream,
            false,
            0,
            0,
            server_width,
            server_height,
        )
        .await
        .map_err(|e| format!("Failed to request framebuffer update: {}", e))?;

        info!("VNC: Connection established, waiting for framebuffer updates");

        let mut read_buf = vec![0u8; 65536];

        // Keep-alive manager (matches guacd's guac_socket_require_keep_alive behavior)
        let mut keepalive = KeepAliveManager::new(DEFAULT_KEEPALIVE_INTERVAL_SECS);
        let mut keepalive_interval =
            tokio::time::interval(Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Keep-alive ping to detect dead connections
                _ = keepalive_interval.tick() => {
                    if let Some(sync_instr) = keepalive.check() {
                        trace!("VNC: Sending keep-alive sync");
                        if self.to_client.send(sync_instr).await.is_err() {
                            info!("VNC: Client channel closed, ending session");
                            break;
                        }
                    }
                }

                result = stream.read(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                            info!("VNC: Connection closed by server");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = self.process_vnc_messages(&mut stream, &read_buf[..n]).await {
                                error!("VNC: Error processing messages: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            error!("VNC: Read error: {}", e);
                            break;
                        }
                    }
                }

                Some(msg) = from_client.recv() => {
                    if let Err(e) = self.handle_client_input(&mut stream, &msg).await {
                        warn!("VNC: Error handling client input: {}", e);
                    }
                }

                else => {
                    debug!("VNC: Connection closed");
                    break;
                }
            }
        }

        info!("VNC: Connection ended");
        Ok(())
    }

    async fn process_vnc_messages<S>(&mut self, stream: &mut S, data: &[u8]) -> Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if data.len() >= 4 && data[0] == 0 {
            match VncProtocol::parse_framebuffer_update_from_buffer(data) {
                Ok((rectangles, _bytes_consumed)) => {
                    for rect in rectangles {
                        self.handle_framebuffer_rectangle(rect).await?;
                    }

                    VncProtocol::send_framebuffer_update_request(
                        stream,
                        true,
                        0,
                        0,
                        self.width as u16,
                        self.height as u16,
                    )
                    .await?;
                }
                Err(e) => {
                    warn!("VNC: Failed to parse FramebufferUpdate: {}", e);
                }
            }
        }

        Ok(())
    }

    async fn handle_client_input<S>(&mut self, stream: &mut S, msg: &Bytes) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        let instr = GuacamoleParser::parse_instruction(msg)
            .map_err(|e| format!("Failed to parse instruction: {}", e))?;

        match instr.opcode {
            "key" => {
                // Security: Check read-only mode
                if self.read_only {
                    trace!("VNC: Keyboard input blocked (read-only mode)");
                    return Ok(());
                }

                if instr.args.len() >= 2 {
                    if let (Ok(keysym), Ok(pressed)) =
                        (instr.args[0].parse::<u32>(), instr.args[1].parse::<u8>())
                    {
                        VncProtocol::send_key_event(stream, keysym, pressed == 1).await?;
                    }
                }
            }
            "mouse" => {
                if instr.args.len() >= 3 {
                    // Protocol order: x, y, mask (per Guacamole protocol spec)
                    if let (Ok(x), Ok(y), Ok(mask)) = (
                        instr.args[0].parse::<i32>(),
                        instr.args[1].parse::<i32>(),
                        instr.args[2].parse::<u8>(),
                    ) {
                        // Security: Check read-only mode for mouse clicks
                        if self.read_only && !is_mouse_event_allowed_readonly(mask as u32) {
                            trace!("VNC: Mouse click blocked (read-only mode)");
                            return Ok(());
                        }

                        let x = x.max(0).min(self.width as i32 - 1) as u16;
                        let y = y.max(0).min(self.height as i32 - 1) as u16;

                        VncProtocol::send_pointer_event(stream, x, y, mask).await?;
                    }
                }
            }
            "size" => {
                if let Some(width_str) = instr.args.first() {
                    if let Some(height_str) = instr.args.get(1) {
                        if let (Ok(w), Ok(h)) =
                            (width_str.parse::<u32>(), height_str.parse::<u32>())
                        {
                            info!("VNC: Resize requested: {}x{}", w, h);

                            // Reset scroll detector for new dimensions
                            self.scroll_detector.reset(w, h);
                            self.width = w;
                            self.height = h;

                            VncProtocol::send_framebuffer_update_request(
                                stream, false, 0, 0, w as u16, h as u16,
                            )
                            .await?;
                        }
                    }
                }
            }
            #[cfg(feature = "sftp")]
            "file" => {
                if let Some(ref mut sftp) = self.sftp_session {
                    if let Err(e) = crate::sftp_integration::handle_sftp_file_request(
                        sftp,
                        &instr.args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                        &self.to_client,
                    )
                    .await
                    {
                        warn!("VNC: SFTP file operation failed: {}", e);
                    }
                } else {
                    warn!("VNC: File transfer requested but SFTP not enabled");
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn handle_framebuffer_rectangle(
        &mut self,
        rect: crate::vnc_protocol::VncRectangle,
    ) -> Result<(), String> {
        if rect.encoding == -239 {
            warn!("VNC: CopyRect encoding not yet implemented, requesting full update");
            return Ok(());
        }

        if rect.pixels.is_empty() {
            return Ok(());
        }

        let mut rgba_vec = vec![0u8; (rect.width as u32 * rect.height as u32 * 4) as usize];

        for i in 0..(rect.width as usize * rect.height as usize) {
            let src_idx = i * 3;
            let dst_idx = i * 4;

            if src_idx + 2 < rect.pixels.len() && dst_idx + 3 < rgba_vec.len() {
                rgba_vec[dst_idx] = rect.pixels[src_idx];
                rgba_vec[dst_idx + 1] = rect.pixels[src_idx + 1];
                rgba_vec[dst_idx + 2] = rect.pixels[src_idx + 2];
                rgba_vec[dst_idx + 3] = 255;
            }
        }

        self.framebuffer.update_region(
            rect.x as u32,
            rect.y as u32,
            rect.width as u32,
            rect.height as u32,
            &rgba_vec,
        );

        self.framebuffer.optimize_dirty_rects();
        let dirty_rects: Vec<_> = self.framebuffer.dirty_rects().to_vec();
        if dirty_rects.is_empty() {
            return Ok(());
        }

        // Check for scroll operation (shared with RDP)
        let framebuffer_pixels = self.framebuffer.get_all_pixels();
        if let Some(scroll_op) = self.scroll_detector.detect_scroll(&framebuffer_pixels) {
            trace!(
                "VNC: Detected scroll {:?} by {} pixels",
                scroll_op.direction,
                scroll_op.pixels
            );

            // Send copy instruction for scroll optimization
            match scroll_op.direction {
                ScrollDirection::Up => {
                    // Content moved up: copy existing content, render new bottom region
                    let copy_instr = guacr_protocol::format_transfer(
                        0,                              // src layer
                        0,                              // src x
                        scroll_op.pixels,               // src y (from line N)
                        self.width,                     // width
                        self.height - scroll_op.pixels, // height (all except scrolled)
                        12,                             // function: SRC (simple copy)
                        0,                              // dst layer
                        0,                              // dst x
                        0,                              // dst y (to top)
                    );
                    self.to_client
                        .send(Bytes::from(copy_instr))
                        .await
                        .map_err(|e| format!("Failed to send copy instruction: {}", e))?;

                    // Only render new content at bottom (bandwidth savings!)
                    let new_region_y = self.height - scroll_op.pixels;
                    let new_region_data = self.encode_region(guacr_terminal::FrameRect {
                        x: 0,
                        y: new_region_y,
                        width: self.width,
                        height: scroll_op.pixels,
                    })?;

                    let msg = self.binary_encoder.encode_image(
                        self.stream_id,
                        0,
                        0,
                        new_region_y as i32,
                        self.width as u16,
                        scroll_op.pixels as u16,
                        0,
                        Bytes::from(new_region_data),
                    );

                    self.to_client
                        .send(msg)
                        .await
                        .map_err(|e| format!("Failed to send image: {}", e))?;

                    // Send sync for frame timing
                    self.send_sync().await?;
                }
                ScrollDirection::Down => {
                    // Content moved down: copy existing content, render new top region
                    let copy_instr = guacr_protocol::format_transfer(
                        0,                              // src layer
                        0,                              // src x
                        0,                              // src y (from top)
                        self.width,                     // width
                        self.height - scroll_op.pixels, // height
                        12,                             // function: SRC
                        0,                              // dst layer
                        0,                              // dst x
                        scroll_op.pixels,               // dst y (move down by N)
                    );
                    self.to_client
                        .send(Bytes::from(copy_instr))
                        .await
                        .map_err(|e| format!("Failed to send copy instruction: {}", e))?;

                    // Only render new content at top
                    let new_region_data = self.encode_region(guacr_terminal::FrameRect {
                        x: 0,
                        y: 0,
                        width: self.width,
                        height: scroll_op.pixels,
                    })?;

                    let msg = self.binary_encoder.encode_image(
                        self.stream_id,
                        0,
                        0,
                        0,
                        self.width as u16,
                        scroll_op.pixels as u16,
                        0,
                        Bytes::from(new_region_data),
                    );

                    self.to_client
                        .send(msg)
                        .await
                        .map_err(|e| format!("Failed to send image: {}", e))?;

                    // Send sync for frame timing
                    self.send_sync().await?;
                }
            }

            // Clear dirty rects since we handled the scroll
            self.framebuffer.clear_dirty();
            return Ok(());
        }

        // No scroll detected - use smart dirty region strategy
        // Calculate total dirty area to decide rendering approach
        let total_dirty_pixels: u32 = dirty_rects.iter().map(|r| r.width * r.height).sum();
        let total_pixels = self.width * self.height;
        let dirty_percent = (total_dirty_pixels * 100) / total_pixels;

        if dirty_percent < 30 {
            // Small changes (<30%): Render each dirty rect separately
            // This is more efficient for small updates (cursor movements, text edits)
            trace!(
                "VNC: Small update ({}%), rendering {} dirty rects",
                dirty_percent,
                dirty_rects.len()
            );

            for dirty_rect in &dirty_rects {
                let encoded_data = self.encode_region(*dirty_rect)?;
                let encoded_bytes = Bytes::from(encoded_data);

                let msg = self.binary_encoder.encode_image(
                    self.stream_id,
                    0,
                    dirty_rect.x as i32,
                    dirty_rect.y as i32,
                    dirty_rect.width as u16,
                    dirty_rect.height as u16,
                    0,
                    encoded_bytes,
                );

                self.to_client
                    .send(msg)
                    .await
                    .map_err(|e| format!("Failed to send image: {}", e))?;
            }

            // Send sync after all dirty rects
            self.send_sync().await?;
        } else {
            // Large changes (>=30%): Render full screen
            // More efficient for large updates due to:
            // 1. Better JPEG compression on larger images
            // 2. Less protocol overhead (one image vs many)
            // 3. Simpler client-side compositing
            trace!(
                "VNC: Large update ({}%), rendering full screen",
                dirty_percent
            );

            let full_screen = guacr_terminal::FrameRect {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            };

            let encoded_data = self.encode_region(full_screen)?;
            let encoded_bytes = Bytes::from(encoded_data);

            let msg = self.binary_encoder.encode_image(
                self.stream_id,
                0,
                0,
                0,
                self.width as u16,
                self.height as u16,
                0,
                encoded_bytes,
            );

            self.to_client
                .send(msg)
                .await
                .map_err(|e| format!("Failed to send image: {}", e))?;

            // Send sync after full screen
            self.send_sync().await?;
        }

        self.framebuffer.clear_dirty();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vnc_handler_new() {
        let handler = VncHandler::with_defaults();
        assert_eq!(<VncHandler as ProtocolHandler>::name(&handler), "vnc");
    }

    #[test]
    fn test_vnc_config_defaults() {
        let config = VncConfig::default();
        assert_eq!(config.default_port, 5900);
        assert_eq!(config.default_width, 1920);
        assert_eq!(config.default_height, 1080);
    }

    #[tokio::test]
    async fn test_vnc_handler_health() {
        let handler = VncHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[test]
    fn test_vnc_settings_from_params() {
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "server.example.com".to_string());

        let defaults = VncConfig::default();
        let settings = VncSettings::from_params(&params, &defaults).unwrap();

        assert_eq!(settings.hostname, "server.example.com");
        assert_eq!(settings.port, 5900);
        assert_eq!(settings.width, 1920);
    }
}
