use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    // Connection utilities
    connect_tcp_with_timeout,
    is_mouse_event_allowed_readonly,
    // Session lifecycle
    send_disconnect,
    send_error_best_effort,
    send_name,
    send_ready,
    // Cursor support
    CursorManager,
    EventBasedHandler,
    EventCallback,
    HandlerError,
    // Security
    HandlerSecuritySettings,
    HandlerStats,
    HealthStatus,
    InstructionSender,
    KeepAliveManager,
    MultiFormatRecorder,
    ProtocolHandler,
    // Recording
    RecordingConfig,
    RecordingDirection,
    StandardCursor,
    DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
use guacr_protocol::{
    format_instruction, GuacamoleParser, TextProtocolEncoder, STATUS_UPSTREAM_ERROR,
    STATUS_UPSTREAM_TIMEOUT,
};
use guacr_terminal::BufferPool;
use image::{ImageEncoder, RgbaImage};
use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

// IronRDP imports
use ironrdp::connector::{self, ClientConnector, ConnectionResult, Credentials, DesktopSize};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageOutput};
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::MousePdu;
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use tokio_rustls::TlsConnector;

// DisplayControl for dynamic resize
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;

type TokioTlsStream = tokio_rustls::client::TlsStream<TcpStream>;

// Re-export supporting modules
use crate::channel_handler::RdpChannelHandler;

// Import shared types from guacr-terminal
use crate::window_detector::WindowMoveDetector;
use guacr_terminal::{
    FrameBuffer, GraphicsMode, GraphicsRenderer, RdpClipboard, RdpInputHandler, ScrollDetector,
    ScrollDirection, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE,
};

/// RDP server type detection for compatibility
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RdpServerType {
    /// Native Windows RDP server (supports CredSSP, DVC)
    WindowsNative,
    /// xrdp/FreeRDP-based server (no CredSSP, no DVC, needs autologon)
    Xrdp,
    /// Unknown server type (use safe defaults)
    Unknown,
}

/// RDP protocol handler
///
/// Connects to RDP servers and provides remote desktop access via the Guacamole protocol.
///
/// ## IMPORTANT: Rendering Method
///
/// RDP MUST use PNG images (NOT Guacamole drawing instructions like rect/cfill).
/// Why:
/// - RDP streams arbitrary graphics: photos, video, complex UI, gradients
/// - Drawing instructions only work for simple colored rectangles
/// - Cannot represent the visual complexity of Windows/Linux desktops
/// - Expected bandwidth: ~50-200KB/frame (acceptable for graphics-rich content)
#[derive(Clone)]
pub struct RdpHandler {
    config: RdpConfig,
}

#[derive(Debug, Clone)]
pub struct RdpConfig {
    pub default_port: u16,
    pub default_width: u32,
    pub default_height: u32,
    pub default_dpi: u32,
    pub security_mode: String,
    /// Clipboard buffer size in bytes (256KB - 50MB)
    pub clipboard_buffer_size: usize,
    /// Whether clipboard copy is disabled
    pub disable_copy: bool,
    /// Whether clipboard paste is disabled
    pub disable_paste: bool,
    /// Use JPEG encoding instead of PNG (default true for 33x bandwidth savings)
    pub use_jpeg: bool,
    /// JPEG quality (1-100, default 92)
    pub jpeg_quality: u8,
    /// Client supports WebP format (40% smaller than JPEG)
    pub supports_webp: bool,
    /// Client supports JPEG format
    pub supports_jpeg: bool,
    /// Terminal graphics mode (None = normal Guacamole protocol)
    pub terminal_mode: Option<GraphicsMode>,
    /// Terminal columns (for terminal mode)
    pub terminal_cols: u16,
    /// Terminal rows (for terminal mode)
    pub terminal_rows: u16,
}

impl Default for RdpConfig {
    fn default() -> Self {
        Self {
            default_port: 3389,
            default_width: 1920,
            default_height: 1080,
            default_dpi: 96, // Standard Windows DPI (matches guacd default)
            security_mode: "nla".to_string(), // Network Level Authentication
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,
            disable_copy: false,
            disable_paste: false,
            use_jpeg: true,       // Enable JPEG by default (33x bandwidth savings)
            jpeg_quality: 92,     // High quality, minimal artifacts
            supports_webp: false, // Will be overridden by client capabilities
            supports_jpeg: false, // Will be overridden by client capabilities
            terminal_mode: None,  // Default to normal Guacamole protocol
            terminal_cols: 240,
            terminal_rows: 60,
        }
    }
}

impl RdpHandler {
    pub fn new(config: RdpConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(RdpConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for RdpHandler {
    fn name(&self) -> &str {
        "rdp"
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
        info!("RDP handler starting connection");

        // Parse RDP settings
        let settings = RdpSettings::from_params(&params, &self.config)
            .map_err(HandlerError::InvalidParameter)?;

        // Create session
        let session = IronRdpSession::new(
            settings.width,
            settings.height,
            settings.dpi,
            settings.clipboard_buffer_size,
            settings.disable_copy,
            settings.disable_paste,
            settings.read_only,
            settings.security.connection_timeout_secs,
            settings.recording_config.clone(),
            to_client,
            &params,
            self.config.terminal_mode,
            self.config.terminal_cols,
            self.config.terminal_rows,
            settings.use_jpeg,
            settings.jpeg_quality,
            settings.supports_webp,
            settings.supports_jpeg,
        );

        // Connect and run session
        session
            .connect_and_run(
                &settings.hostname,
                settings.port,
                &settings.username,
                &settings.password,
                settings.domain.as_deref(),
                &settings.security_mode,
                from_client,
                Some(&settings),
            )
            .await
            .map_err(HandlerError::ConnectionFailed)?;

        info!("RDP handler connection ended");
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
impl EventBasedHandler for RdpHandler {
    fn name(&self) -> &str {
        "rdp"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // RDP sends 3 instructions per frame (img, blob, end) and can have multiple
        // frames in flight, so we need a larger buffer than text-based protocols
        let (to_client, mut handler_rx) = mpsc::channel::<Bytes>(1024);

        let sender = InstructionSender::new(callback);
        let sender_arc = Arc::new(sender);

        let sender_clone = Arc::clone(&sender_arc);
        tokio::spawn(async move {
            while let Some(msg) = handler_rx.recv().await {
                if let Err(e) = sender_clone.send(msg).await {
                    log::error!("RDP: Failed to send instruction: {}", e);
                    break;
                }
            }
        });

        self.connect(params, to_client, from_client).await?;

        Ok(())
    }
}

// ============================================================================
// RDP Settings - Parameter parsing and validation
// ============================================================================

/// RDP connection settings
#[derive(Debug, Clone)]
pub struct RdpSettings {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub security_mode: String,
    pub clipboard_buffer_size: usize,
    pub disable_copy: bool,
    pub disable_paste: bool,
    /// Use JPEG encoding (33x bandwidth savings vs PNG)
    pub use_jpeg: bool,
    /// JPEG quality (1-100)
    pub jpeg_quality: u8,
    /// Client supports WebP format (40% smaller than JPEG)
    pub supports_webp: bool,
    /// Client supports JPEG format
    pub supports_jpeg: bool,
    /// Read-only mode - blocks keyboard/mouse input
    pub read_only: bool,
    /// Security settings (includes connection timeout)
    pub security: HandlerSecuritySettings,
    pub domain: Option<String>,
    /// Server type hint for compatibility (windows, xrdp, auto)
    pub server_type: Option<String>,
    #[allow(dead_code)] // TODO: Used for connection brokers
    pub load_balance_info: Option<String>,
    #[allow(dead_code)] // TODO: Used for cert validation
    pub ignore_cert: bool,
    #[allow(dead_code)] // TODO: Used for cert validation
    pub cert_fingerprint: Option<String>,
    /// Recording configuration
    pub recording_config: RecordingConfig,
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

impl RdpSettings {
    pub fn from_params(
        params: &HashMap<String, String>,
        defaults: &RdpConfig,
    ) -> Result<Self, String> {
        let hostname = params
            .get("hostname")
            .ok_or_else(|| "Missing required parameter: hostname".to_string())?
            .clone();

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(defaults.default_port);

        let username = params
            .get("username")
            .ok_or_else(|| "Missing required parameter: username".to_string())?
            .clone();

        let password = params
            .get("password")
            .ok_or_else(|| "Missing required parameter: password".to_string())?
            .clone();

        // Parse width/height/dpi from connection parameters
        // Client sends "size" as "width,height,dpi" (e.g., "1920,1080,96")
        // If not provided, use defaults
        let (width, height, dpi) = if let Some(size_str) = params.get("size") {
            let parts: Vec<&str> = size_str.split(',').collect();
            if parts.len() >= 3 {
                let w = parts[0].parse().unwrap_or(defaults.default_width);
                let h = parts[1].parse().unwrap_or(defaults.default_height);
                let d = parts[2].parse().unwrap_or(defaults.default_dpi);
                (w, h, d)
            } else if parts.len() == 2 {
                let w = parts[0].parse().unwrap_or(defaults.default_width);
                let h = parts[1].parse().unwrap_or(defaults.default_height);
                (w, h, defaults.default_dpi)
            } else {
                (
                    defaults.default_width,
                    defaults.default_height,
                    defaults.default_dpi,
                )
            }
        } else {
            // Fallback to separate width/height/dpi params if size not present
            let w = params
                .get("width")
                .and_then(|w| w.parse().ok())
                .unwrap_or(defaults.default_width);
            let h = params
                .get("height")
                .and_then(|h| h.parse().ok())
                .unwrap_or(defaults.default_height);
            let d = params
                .get("dpi")
                .and_then(|d| d.parse().ok())
                .unwrap_or(defaults.default_dpi);
            (w, h, d)
        };

        info!("RDP: Display settings - {}x{} @ {} DPI", width, height, dpi);

        let security_mode = params
            .get("security")
            .cloned()
            .unwrap_or_else(|| defaults.security_mode.clone());

        let clipboard_buffer_size = params
            .get("clipboard-buffer-size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(defaults.clipboard_buffer_size);

        let clipboard_buffer_size =
            clipboard_buffer_size.clamp(CLIPBOARD_MIN_SIZE, CLIPBOARD_MAX_SIZE);

        let disable_copy = params
            .get("disable-copy")
            .map(|s| s == "true")
            .unwrap_or(defaults.disable_copy);

        let disable_paste = params
            .get("disable-paste")
            .map(|s| s == "true")
            .unwrap_or(defaults.disable_paste);

        let read_only = params
            .get("read-only")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);

        let use_jpeg = params
            .get("use-jpeg")
            .map(|s| s == "true")
            .unwrap_or(defaults.use_jpeg);

        let jpeg_quality = params
            .get("jpeg-quality")
            .and_then(|q| q.parse().ok())
            .unwrap_or(defaults.jpeg_quality)
            .clamp(1, 100);

        // Parse client image format support
        let supported_formats = params
            .get("image")
            .map(|s| s.split(',').map(|f| f.trim()).collect::<Vec<_>>())
            .unwrap_or_else(|| vec!["image/png"]);

        // Force JPEG only - client supports WebP but we'll use JPEG for maximum compatibility
        let supports_webp = false;
        let supports_jpeg = supported_formats.iter().any(|f| f.contains("jpeg"));

        info!(
            "RDP: Client image support - WebP: {}, JPEG: {}, formats: {:?}",
            supports_webp, supports_jpeg, supported_formats
        );

        // Parse security settings (includes connection timeout)
        let security = HandlerSecuritySettings::from_params(params);

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(params);

        let domain = params.get("domain").cloned();
        let server_type = params.get("server-type").cloned();
        let load_balance_info = params.get("load-balance-info").cloned();
        let ignore_cert = params
            .get("ignore-cert")
            .map(|s| s == "true")
            .unwrap_or(false);
        let cert_fingerprint = params.get("cert-fingerprint").cloned();

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
            "RDP Settings: {}@{}:{}, {}x{}, security: {}, clipboard: {} bytes, read_only: {}",
            username,
            hostname,
            port,
            width,
            height,
            security_mode,
            clipboard_buffer_size,
            read_only
        );

        if recording_config.is_enabled() {
            info!(
                "RDP: Recording enabled - ses={}, asciicast={}, typescript={}",
                recording_config.is_ses_enabled(),
                recording_config.is_asciicast_enabled(),
                recording_config.is_typescript_enabled()
            );
        }

        Ok(Self {
            hostname,
            port,
            username,
            password,
            width,
            height,
            dpi,
            security_mode,
            clipboard_buffer_size,
            disable_copy,
            disable_paste,
            use_jpeg,
            jpeg_quality,
            supports_webp,
            supports_jpeg,
            read_only,
            security,
            domain,
            server_type,
            load_balance_info,
            ignore_cert,
            cert_fingerprint,
            recording_config,
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
// Helper Functions
// ============================================================================

// ============================================================================
// RDP Session - Complete connection and event loop
// ============================================================================

/// Complete ironrdp session manager
struct IronRdpSession {
    framebuffer: FrameBuffer,
    #[allow(dead_code)] // TODO: Used for buffer pooling optimization
    buffer_pool: Arc<BufferPool>,
    #[allow(dead_code)] // TODO: Used for clipboard integration
    clipboard: RdpClipboard,
    input_handler: RdpInputHandler,
    // Zero-copy buffers for encoding (reused across frames)
    base64_buffer: String,
    protocol_encoder: TextProtocolEncoder,
    // Image encoding settings
    use_jpeg: bool,
    jpeg_quality: u8,
    supports_webp: bool,
    supports_jpeg: bool,
    /// Stream ID counter for Guacamole img/blob/end protocol
    stream_id: u32,
    channel_handler: RdpChannelHandler,
    /// Scroll detector for bandwidth optimization (90-99% savings on scrolling)
    scroll_detector: ScrollDetector,
    /// Window move detector for outline optimization (50-100x savings during window moves)
    window_detector: WindowMoveDetector,
    #[allow(dead_code)] // TODO: Used for clipboard policy
    disable_copy: bool,
    #[allow(dead_code)] // TODO: Used for clipboard policy
    disable_paste: bool,
    /// Read-only mode - blocks keyboard/mouse input
    read_only: bool,
    /// Connection timeout in seconds
    connection_timeout_secs: u64,
    /// Recording configuration
    #[allow(dead_code)] // Config stored for reference, recorder does the work
    recording_config: RecordingConfig,
    /// Active recorder for .guac format session recording
    recorder: Option<MultiFormatRecorder>,
    to_client: mpsc::Sender<Bytes>,
    width: u32,
    height: u32,
    dpi: u32,
    #[allow(dead_code)] // TODO: Used to track encoding mode
    use_hardware_encoding: bool,
    #[cfg(feature = "sftp")]
    sftp_session: Option<russh_sftp::client::SftpSession>,
    /// Terminal graphics renderer (for terminal-only environments)
    terminal_renderer: Option<GraphicsRenderer>,

    // Adaptive quality tracking (Guacamole-style dynamic quality)
    adaptive_quality: guacr_handlers::AdaptiveQuality,

    // Sync flow control (prevents overwhelming slow clients)
    sync_control: guacr_handlers::SyncFlowControl,

    // Tracks when the last sync was sent (for timeout detection in keepalive)
    sync_sent_at: Option<std::time::Instant>,

    // Track if first frame has been sent (skip sync wait on first frame)
    first_frame_sent: bool,

    // Cursor manager for client-side cursor rendering (matches KCM behavior)
    cursor_manager: CursorManager,
}

impl IronRdpSession {
    #[allow(clippy::too_many_arguments)]
    fn new(
        width: u32,
        height: u32,
        dpi: u32,
        clipboard_buffer_size: usize,
        disable_copy: bool,
        disable_paste: bool,
        read_only: bool,
        connection_timeout_secs: u64,
        recording_config: RecordingConfig,
        to_client: mpsc::Sender<Bytes>,
        params: &HashMap<String, String>,
        terminal_mode: Option<GraphicsMode>,
        terminal_cols: u16,
        terminal_rows: u16,
        use_jpeg: bool,
        jpeg_quality: u8,
        supports_webp: bool,
        supports_jpeg: bool,
    ) -> Self {
        let frame_size = (width * height * 4) as usize;
        let buffer_pool = Arc::new(BufferPool::new(8, frame_size));

        // Pre-allocate zero-copy buffers (reused across frames)
        let base64_buffer = String::with_capacity(frame_size * 4 / 3); // base64 is ~33% larger
        let protocol_encoder = TextProtocolEncoder::new();

        // Initialize recording if enabled
        let recorder = if recording_config.is_enabled() {
            // RDP doesn't have cols/rows like terminal, use width/height
            match MultiFormatRecorder::new(
                &recording_config,
                params,
                "rdp",
                width as u16,
                height as u16,
            ) {
                Ok(rec) => {
                    info!("RDP: Session recording initialized");
                    Some(rec)
                }
                Err(e) => {
                    warn!("RDP: Failed to initialize recording: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Initialize terminal renderer if terminal mode is enabled
        let terminal_renderer = terminal_mode.map(|mode| {
            info!("RDP: Terminal graphics mode enabled: {:?}", mode);
            GraphicsRenderer::new(mode, terminal_cols, terminal_rows)
        });

        Self {
            framebuffer: FrameBuffer::new(width, height),
            buffer_pool,
            clipboard: RdpClipboard::new(clipboard_buffer_size),
            input_handler: RdpInputHandler::new(),
            base64_buffer,
            protocol_encoder,
            use_jpeg,
            jpeg_quality,
            supports_webp,
            supports_jpeg,
            stream_id: 1,
            scroll_detector: ScrollDetector::new(width, height),
            window_detector: WindowMoveDetector::new(),
            channel_handler: RdpChannelHandler::new(
                clipboard_buffer_size,
                disable_copy,
                disable_paste,
            ),
            disable_copy,
            disable_paste,
            read_only,
            connection_timeout_secs,
            recording_config,
            recorder,
            to_client,
            width,
            height,
            dpi,
            use_hardware_encoding: false, // Hardware encoding disabled for now
            #[cfg(feature = "sftp")]
            sftp_session: None,
            terminal_renderer,

            // Initialize adaptive quality at max configured quality
            adaptive_quality: guacr_handlers::AdaptiveQuality::new(jpeg_quality),

            // Initialize sync flow control (15s timeout, 3 strikes - matches guacd)
            sync_control: guacr_handlers::SyncFlowControl::new(),

            // No pending sync initially
            sync_sent_at: None,

            // Track first frame (skip sync wait to avoid blocking initial display)
            first_frame_sent: false,

            // Initialize cursor manager for client-side cursor rendering
            cursor_manager: CursorManager::new(supports_jpeg, supports_webp, jpeg_quality),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_and_run(
        mut self,
        hostname: &str,
        port: u16,
        username: &str,
        password: &str,
        domain: Option<&str>,
        _security_mode: &str,
        from_client: mpsc::Receiver<Bytes>,
        settings: Option<&RdpSettings>,
    ) -> Result<(), String> {
        info!(
            "RDP: Connecting to {}:{} (timeout: {}s)",
            hostname, port, self.connection_timeout_secs
        );

        // Build IronRDP connector config
        let settings_ref = settings.ok_or("RDP settings required")?;
        let config = self.build_ironrdp_config(username, password, domain, settings_ref);

        // Establish TCP connection with timeout (matches guacd behavior)
        let stream =
            match connect_tcp_with_timeout((hostname, port), self.connection_timeout_secs).await {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("{}", e);
                    send_error_best_effort(&self.to_client, &msg, STATUS_UPSTREAM_TIMEOUT).await;
                    return Err(msg);
                }
            };

        info!("RDP: TCP connection established");

        // Perform RDP handshake and authentication
        let (connection_result, framed) = match self
            .perform_rdp_handshake(stream, config, hostname.to_string())
            .await
        {
            Ok(result) => result,
            Err(e) => {
                let msg = format!("RDP handshake failed: {}", e);
                send_error_best_effort(&self.to_client, &msg, STATUS_UPSTREAM_ERROR).await;
                return Err(msg);
            }
        };

        info!(
            "RDP: Connection established - {}x{}",
            connection_result.desktop_size.width, connection_result.desktop_size.height
        );

        // Update session dimensions if they differ from requested
        self.width = u32::from(connection_result.desktop_size.width);
        self.height = u32::from(connection_result.desktop_size.height);
        self.framebuffer = FrameBuffer::new(self.width, self.height);
        self.scroll_detector.reset(self.width, self.height);

        // Send ready and name instructions to client (matches Apache guacd behavior)
        send_ready(&self.to_client, "rdp-ready")
            .await
            .map_err(|e| e.to_string())?;
        send_name(&self.to_client, "RDP")
            .await
            .map_err(|e| e.to_string())?;

        // Set initial cursor to pointer (client-side cursor rendering, matches KCM)
        if !self.read_only {
            let cursor_instrs = self
                .cursor_manager
                .send_standard_cursor(StandardCursor::Pointer)
                .map_err(|e| format!("Failed to generate cursor: {}", e))?;
            for instr in cursor_instrs {
                self.send_and_record(&instr).await?;
            }
            info!("RDP: Initial cursor set to pointer (client-side rendering)");
        }

        // Send size instruction
        let size_instr = self.protocol_encoder.format_size_instruction(
            0, // layer
            self.width,
            self.height,
        );
        let size_instr_str = String::from_utf8_lossy(&size_instr).to_string();
        info!("RDP: Sending size instruction: {}", size_instr_str);
        self.send_and_record(&size_instr_str).await?;

        info!("RDP: Session ready, starting active session");

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
                        info!("RDP: SFTP session established");
                        self.sftp_session = Some(sftp);
                    }
                    Err(e) => {
                        warn!(
                            "RDP: SFTP connection failed: {}, continuing without SFTP",
                            e
                        );
                    }
                }
            }
        }

        // Run the active RDP session
        self.run_active_session(connection_result, framed, from_client)
            .await?;

        Ok(())
    }

    fn detect_server_type(&self, settings: &RdpSettings) -> RdpServerType {
        // Check explicit server-type parameter first
        if let Some(ref server_type_str) = settings.server_type {
            return match server_type_str.to_lowercase().as_str() {
                "windows" => RdpServerType::WindowsNative,
                "xrdp" => RdpServerType::Xrdp,
                _ => RdpServerType::Unknown,
            };
        }

        // Auto-detection heuristics:
        // 1. If domain is set, likely Windows Active Directory
        if settings.domain.is_some() {
            return RdpServerType::WindowsNative;
        }

        // 2. Default to xrdp-compatible mode (works with both xrdp and Windows)
        //    This is the safest default as:
        //    - xrdp requires: CredSSP=false, autologon=true, no DVC
        //    - Windows works with: CredSSP=false, autologon=true (just slower auth)
        RdpServerType::Xrdp
    }

    fn build_ironrdp_config(
        &self,
        username: &str,
        password: &str,
        domain: Option<&str>,
        settings: &RdpSettings,
    ) -> connector::Config {
        // Detect server type based on configuration hints
        // Windows RDP servers typically support CredSSP, while xrdp/FreeRDP servers don't
        let server_type = self.detect_server_type(settings);
        let (enable_credssp, autologon) = match server_type {
            RdpServerType::WindowsNative => (true, false), // Windows RDP supports CredSSP
            RdpServerType::Xrdp => (false, true),          // xrdp needs autologon, no CredSSP
            RdpServerType::Unknown => (false, true),       // Safe defaults for unknown servers
        };

        info!(
            "RDP: Detected server type: {:?}, CredSSP: {}, Autologon: {}",
            server_type, enable_credssp, autologon
        );

        connector::Config {
            credentials: Credentials::UsernamePassword {
                username: username.to_string(),
                password: password.to_string(),
            },
            domain: domain.map(|s| s.to_string()),
            enable_tls: true,
            enable_credssp,
            keyboard_type: KeyboardType::IbmEnhanced,
            keyboard_subtype: 0,
            keyboard_layout: 0,
            keyboard_functional_keys_count: 12,
            ime_file_name: String::new(),
            dig_product_id: String::new(),
            desktop_size: DesktopSize {
                width: self.width as u16,
                height: self.height as u16,
            },
            bitmap: None,
            client_build: 0,
            client_name: "guacr-rdp".to_owned(),
            client_dir: "C:\\\\Windows\\\\System32\\\\mstscax.dll".to_owned(),
            #[cfg(target_os = "macos")]
            platform: MajorPlatformType::MACINTOSH,
            #[cfg(target_os = "linux")]
            platform: MajorPlatformType::UNIX,
            #[cfg(target_os = "windows")]
            platform: MajorPlatformType::WINDOWS,
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            platform: MajorPlatformType::UNIX,
            enable_server_pointer: true, // Enable pointer events from RDP server (required for cursor)
            request_data: None,
            autologon,
            enable_audio_playback: false,
            pointer_software_rendering: false, // Client-side cursor rendering via PointerBitmap events (matches KCM)
            performance_flags: PerformanceFlags::default(),
            desktop_scale_factor: 0,
            hardware_id: None,
            license_cache: None,
            timezone_info: TimezoneInfo::default(),
        }
    }

    async fn perform_rdp_handshake(
        &self,
        stream: TcpStream,
        config: connector::Config,
        server_name: String,
    ) -> Result<
        (
            ConnectionResult,
            ironrdp_tokio::Framed<ironrdp_tokio::MovableTokioStream<TokioTlsStream>>,
        ),
        String,
    > {
        use ironrdp_tokio::{connect_begin, connect_finalize, mark_as_upgraded, Framed};

        let client_addr = stream
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;

        let mut framed = Framed::<ironrdp_tokio::MovableTokioStream<_>>::new(stream);

        // Note: xrdp doesn't support Dynamic Virtual Channels (DVC), so we don't add DisplayControl
        // guacd also doesn't use DVC with xrdp for this reason
        let mut connector = ClientConnector::new(config, client_addr);

        // Begin RDP connection (X.224, MCS)
        let should_upgrade = connect_begin(&mut framed, &mut connector)
            .await
            .map_err(|e| format!("RDP connection begin failed: {}", e))?;

        info!("RDP: Initial handshake complete, performing TLS upgrade");

        // TLS upgrade
        let initial_stream = framed.into_inner_no_leftover();
        let (upgraded_stream, server_public_key) = self
            .tls_upgrade(initial_stream, &server_name)
            .await
            .map_err(|e| format!("TLS upgrade failed: {}", e))?;

        let upgraded = mark_as_upgraded(should_upgrade, &mut connector);
        let mut upgraded_framed =
            Framed::<ironrdp_tokio::MovableTokioStream<_>>::new(upgraded_stream);

        // Finalize connection (authentication, capability negotiation)
        // PR #1043 changed parameter order and made network_client required
        let mut dummy_network_client = DummyNetworkClient;
        let connection_result = connect_finalize(
            upgraded,
            connector,
            &mut upgraded_framed,
            &mut dummy_network_client,
            server_name.into(),
            server_public_key,
            None, // kerberos_config
        )
        .await
        .map_err(|e| format!("RDP connection finalize failed: {}", e))?;

        info!("RDP: Connection established - client-side cursor rendering enabled (matches KCM)");
        Ok((connection_result, upgraded_framed))
    }

    async fn tls_upgrade(
        &self,
        stream: TcpStream,
        server_name: &str,
    ) -> Result<(TokioTlsStream, Vec<u8>), String> {
        use tokio_rustls::rustls;

        // Install default crypto provider (ring) if not already installed
        // This is required for Rustls 0.23+
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(
                DangerousNoCertificateVerification,
            ))
            .with_no_client_auth();

        // Disable TLS resumption (not supported by CredSSP)
        config.resumption = rustls::client::Resumption::disabled();

        let connector = TlsConnector::from(std::sync::Arc::new(config));
        let server_name_owned = rustls::pki_types::ServerName::try_from(server_name.to_string())
            .map_err(|e| format!("Invalid server name: {}", e))?;

        let mut tls_stream = connector
            .connect(server_name_owned, stream)
            .await
            .map_err(|e| format!("TLS connection failed: {}", e))?;

        // Flush to ensure handshake completes
        tls_stream
            .flush()
            .await
            .map_err(|e| format!("TLS flush failed: {}", e))?;

        // Extract server public key
        let (_, session) = tls_stream.get_ref();
        let cert = session
            .peer_certificates()
            .and_then(|certs| certs.first())
            .ok_or_else(|| "No peer certificate found".to_string())?;

        let server_public_key = extract_tls_server_public_key(cert.as_ref())
            .map_err(|e| format!("Failed to extract server public key: {}", e))?;

        Ok((tls_stream, server_public_key))
    }

    async fn run_active_session(
        mut self,
        connection_result: ConnectionResult,
        mut framed: ironrdp_tokio::Framed<ironrdp_tokio::MovableTokioStream<TokioTlsStream>>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        use ironrdp_tokio::FramedWrite;

        let mut active_stage = ActiveStage::new(connection_result);
        let mut image = DecodedImage::new(
            ironrdp::graphics::image_processing::PixelFormat::RgbA32,
            self.width as u16,
            self.height as u16,
        );

        // Keep-alive manager (matches guacd's guac_socket_require_keep_alive behavior)
        let mut keepalive = KeepAliveManager::new(DEFAULT_KEEPALIVE_INTERVAL_SECS);
        let mut keepalive_interval =
            tokio::time::interval(Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        info!("RDP: Active session started");

        loop {
            tokio::select! {
                // Keep-alive ping to detect dead connections
                _ = keepalive_interval.tick() => {
                    // Check for sync timeout (client not responding)
                    if let Some(sent_at) = self.sync_sent_at {
                        if self.sync_control.check_timeout(sent_at.elapsed()) {
                            error!("RDP: Client not responding to sync - disconnecting");
                            break;
                        }
                        if !self.sync_control.is_waiting_for_sync() {
                            self.sync_sent_at = None;
                        }
                    }

                    if let Some(sync_instr) = keepalive.check() {
                        trace!("RDP: Sending keep-alive sync");
                        if self.to_client.send(sync_instr).await.is_err() {
                            info!("RDP: Client channel closed, ending session");
                            return Ok(());
                        }
                    }
                }

                // Handle incoming RDP frames
                result = framed.read_pdu() => {
                    match result {
                        Ok((action, payload)) => {
                            trace!("RDP: Received frame - action: {:?}, {} bytes", action, payload.len());

                            let outputs = active_stage
                                .process(&mut image, action, &payload)
                                .map_err(|e| format!("Failed to process RDP frame: {}", e))?;

                            if outputs.is_empty() && payload.len() > 1000 {
                                warn!("RDP: Large frame ({} bytes) produced 0 outputs - possible graphics data loss", payload.len());
                            }

                            trace!("RDP: ActiveStage returned {} outputs", outputs.len());
                            for (idx, output) in outputs.iter().enumerate() {
                                trace!("RDP: Output {}: {:?}", idx, output);
                            }

                            for output in outputs {
                                match output {
                                    ActiveStageOutput::ResponseFrame(frame) => {
                                        framed
                                            .write_all(&frame)
                                            .await
                                            .map_err(|e| format!("Failed to write response: {}", e))?;
                                    }
                                    ActiveStageOutput::GraphicsUpdate(rect) => {
                                        // Skip frame if waiting for client sync ack (flow control)
                                        if self.sync_control.is_waiting_for_sync() {
                                            trace!("RDP: Skipping frame - waiting for client sync ack");
                                            continue;
                                        }

                                        // Send graphics update to client
                                        debug!("RDP: GraphicsUpdate received - rect: {:?}, image size: {}x{}",
                                            rect, image.width(), image.height());

                                        // Debug: Check image data
                                        let sample_size = std::cmp::min(100, image.data().len());
                                        let sample_pixels = &image.data()[0..sample_size];
                                        let all_zeros = sample_pixels.iter().all(|&b| b == 0);
                                        let all_same = sample_pixels.windows(4).all(|w| w == &sample_pixels[0..4]);

                                        // Skip all-zeros frames ONLY if:
                                        // 1. It's the first frame (initial connection artifact), OR
                                        // 2. The rectangle is invalid (0x0 size)
                                        let is_invalid_rect = rect.right == 0 || rect.bottom == 0;
                                        let should_skip = all_zeros && (!self.first_frame_sent || is_invalid_rect);

                                        if should_skip {
                                            warn!("RDP: Skipping all-zeros frame (first_frame={}, invalid_rect={})",
                                                !self.first_frame_sent, is_invalid_rect);
                                            // IronRDP sometimes sends all-zeros on initial connection or with 0x0 rects
                                            // Skip these to avoid black screen artifacts
                                        } else {
                                            if all_same {
                                                debug!("RDP: Image appears solid color: RGBA({},{},{},{})",
                                                    sample_pixels[0], sample_pixels[1], sample_pixels[2], sample_pixels[3]);
                                            } else {
                                                trace!("RDP: Image has varied pixel data (first 4 pixels: {:?})",
                                                    &sample_pixels[0..std::cmp::min(16, sample_size)]);
                                            }

                                            self.send_graphics_update_with_rect(&image, rect).await?;
                                        }

                                        if !self.first_frame_sent {
                                            self.first_frame_sent = true;
                                            debug!("RDP: First frame sent");
                                        }
                                    }
                                    ActiveStageOutput::PointerDefault => {
                                        trace!("RDP: Pointer set to default");
                                        if !self.read_only {
                                            match self.cursor_manager.send_standard_cursor(StandardCursor::Pointer) {
                                                Ok(instrs) => {
                                                    for instr in instrs {
                                                        if let Err(e) = self.send_and_record(&instr).await {
                                                            warn!("RDP: Failed to send default cursor: {}", e);
                                                            break;
                                                        }
                                                    }
                                                }
                                                Err(e) => warn!("RDP: Failed to generate default cursor: {}", e),
                                            }
                                        }
                                    }
                                    ActiveStageOutput::PointerHidden => {
                                        trace!("RDP: Pointer hidden");
                                        if !self.read_only {
                                            match self.cursor_manager.send_standard_cursor(StandardCursor::None) {
                                                Ok(instrs) => {
                                                    for instr in instrs {
                                                        if let Err(e) = self.send_and_record(&instr).await {
                                                            warn!("RDP: Failed to hide cursor: {}", e);
                                                            break;
                                                        }
                                                    }
                                                }
                                                Err(e) => warn!("RDP: Failed to generate hidden cursor: {}", e),
                                            }
                                        }
                                    }
                                    ActiveStageOutput::PointerPosition { x, y } => {
                                        trace!("RDP: Pointer moved to ({}, {}) - client handles cursor locally", x, y);
                                        // Client-side cursor position is handled by the browser automatically
                                        // No need to send position updates (cursor layer follows mouse)
                                    }
                                    ActiveStageOutput::PointerBitmap(pointer) => {
                                        debug!("RDP: Custom pointer bitmap received: {}x{} at hotspot ({}, {})",
                                            pointer.width, pointer.height, pointer.hotspot_x, pointer.hotspot_y);

                                        // Send custom cursor to client using shared cursor manager
                                        if let Err(e) = self.send_custom_cursor(&pointer).await {
                                            warn!("RDP: Failed to send custom cursor: {}", e);
                                        }
                                    }
                                    ActiveStageOutput::FrameStart { frame_id } => {
                                        trace!("RDP: Frame {} start", frame_id);
                                        self.window_detector.frame_start(frame_id);
                                    }
                                    ActiveStageOutput::FrameEnd { frame_id } => {
                                        trace!("RDP: Frame {} end", frame_id);
                                        self.window_detector.frame_end(frame_id);
                                    }
                                    ActiveStageOutput::Terminate(reason) => {
                                        info!("RDP: Session terminated: {:?}", reason);
                                        return Ok(());
                                    }
                                    other => {
                                        debug!("RDP: Unhandled ActiveStageOutput variant: {:?}", other);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("RDP: Read error: {}", e);
                            return Err(format!("RDP read error: {}", e));
                        }
                    }
                }

                // Handle client input
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("RDP: Client disconnected");
                        break;
                    };
                    if let Err(e) = self.handle_client_input_ironrdp(&mut framed, &msg, &mut active_stage, &mut image).await {
                        warn!("RDP: Error handling client input: {}", e);
                    }
                }

                else => {
                    info!("RDP: Event loop ended");
                    break;
                }
            }
        }

        // Send disconnect instruction to client (matches Apache guacd behavior)
        send_disconnect(&self.to_client).await;

        // Finalize recording
        if let Some(recorder) = self.recorder.take() {
            if let Err(e) = recorder.finalize() {
                warn!("RDP: Failed to finalize recording: {}", e);
            } else {
                info!("RDP: Session recording finalized");
            }
        }

        Ok(())
    }

    /// Send custom RDP cursor to client via shared cursor manager
    async fn send_custom_cursor(
        &mut self,
        pointer: &ironrdp::graphics::pointer::DecodedPointer,
    ) -> Result<(), String> {
        if self.read_only {
            return Ok(());
        }

        let cursor_data = &pointer.bitmap_data;
        let width = pointer.width as u32;
        let height = pointer.height as u32;

        // Convert BGRA to RGBA (IronRDP uses BGRA format)
        let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);
        for chunk in cursor_data.chunks(4) {
            if chunk.len() == 4 {
                rgba_data.push(chunk[2]); // R
                rgba_data.push(chunk[1]); // G
                rgba_data.push(chunk[0]); // B
                rgba_data.push(chunk[3]); // A
            }
        }

        // Use shared cursor manager (handles encoding and instruction generation)
        let instructions = self.cursor_manager.send_custom_cursor(
            &rgba_data,
            width,
            height,
            pointer.hotspot_x as i32,
            pointer.hotspot_y as i32,
        )?;

        // Send all cursor instructions
        for instr in instructions {
            self.send_and_record(&instr).await?;
        }

        debug!(
            "RDP: Sent custom cursor {}x{} with hotspot ({}, {})",
            width, height, pointer.hotspot_x, pointer.hotspot_y
        );

        Ok(())
    }

    // Removed send_graphics_update() - it was causing full-screen redraws on mouse moves
    // Always use send_graphics_update_with_rect() with the actual dirty rect from IronRDP

    async fn send_graphics_update_with_rect(
        &mut self,
        image: &DecodedImage,
        rect: ironrdp_pdu::geometry::InclusiveRectangle,
    ) -> Result<(), String> {
        // Convert DecodedImage to our framebuffer format and send to client
        let image_data = image.data();

        // Convert IronRDP InclusiveRectangle to our coordinates
        // InclusiveRectangle uses inclusive bounds (right/bottom are included)
        let x = rect.left as u32;
        let y = rect.top as u32;

        // CRITICAL: Check for zero-rect (cursor-only updates or no-ops)
        // IronRDP sends rect { left: 0, top: 0, right: 0, bottom: 0 } for cursor moves
        // These should NOT be rendered as 1x1 pixel updates!
        // Note: InclusiveRectangle where left==right and top==bottom is a 1x1 rect
        // We want to skip when left==right==top==bottom==0 (zero-size, not 1x1)
        if rect.left == rect.right && rect.top == rect.bottom && rect.left == 0 && rect.top == 0 {
            trace!(
                "RDP: Skipping zero-rect update (cursor-only or no-op): {:?}",
                rect
            );
            return Ok(());
        }

        let width = (rect.right - rect.left + 1) as u32;
        let height = (rect.bottom - rect.top + 1) as u32;

        debug!(
            "RDP: send_graphics_update_with_rect called - dirty rect: {}x{} at ({}, {}), image: {}x{}",
            width, height, x, y, self.width, self.height
        );

        // NOTE: Alpha channel fix is now handled in framebuffer.update_region()
        // IronRDP doesn't set alpha=255, so we fix it during the copy to avoid extra allocation

        // If terminal mode is enabled, render to terminal instead of Guacamole protocol
        if let Some(renderer) = &self.terminal_renderer {
            trace!("RDP: Rendering to terminal graphics");
            let terminal_output = renderer.render(image_data, self.width, self.height);

            // Send terminal output directly to client
            if let Err(e) = self.to_client.send(terminal_output).await {
                return Err(format!("Failed to send terminal graphics: {}", e));
            }

            // Record if enabled
            if let Some(_recorder) = &self.recorder {
                // For terminal mode, we could record the terminal output
                // For now, just skip recording or record as binary data
                trace!("RDP: Terminal mode - recording not yet implemented");
            }

            return Ok(());
        }

        // Check for window move operation first (50-100x bandwidth savings)
        if self.window_detector.is_window_move(rect, image_data) {
            debug!(
                "RDP: Window move detected - sending outline for {}x{} at ({}, {})",
                width, height, x, y
            );

            // Send rectangle outline instead of full content
            let outline_instr = guacr_protocol::format_rect(0, x, y, width, height);
            self.send_and_record(&outline_instr).await?;

            // Send sync
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let sync_instr = format!("4.sync,{}.{};", timestamp.to_string().len(), timestamp);
            self.send_and_record(&sync_instr).await?;

            // Store pending sync for flow control
            self.sync_control.set_pending_sync(timestamp);
            self.sync_sent_at = Some(std::time::Instant::now());

            return Ok(());
        }

        // Check for scroll operation (90-99% bandwidth savings)
        if let Some(scroll_op) = self.scroll_detector.detect_scroll(image_data) {
            trace!(
                "RDP: Detected scroll {:?} by {} pixels",
                scroll_op.direction,
                scroll_op.pixels
            );

            match scroll_op.direction {
                ScrollDirection::Up => {
                    // Content moved up: copy existing content, render new bottom region
                    let copy_instr = guacr_protocol::format_transfer(
                        0,                              // src_layer
                        0,                              // src_x
                        scroll_op.pixels,               // src_y: from line N
                        self.width,                     // width
                        self.height - scroll_op.pixels, // height: all except scrolled
                        12,                             // 12 = SRC function (simple copy)
                        0,                              // dst_layer
                        0,                              // dst_x
                        0,                              // dst_y: to top
                    );
                    self.send_and_record(&copy_instr).await?;

                    // Render only new content at bottom
                    let new_region_y = self.height - scroll_op.pixels;
                    self.handle_graphics_update(
                        0,
                        new_region_y,
                        self.width,
                        scroll_op.pixels,
                        image_data,
                    )
                    .await?;
                }
                ScrollDirection::Down => {
                    // Content moved down: copy existing content, render new top region
                    let copy_instr = guacr_protocol::format_transfer(
                        0,                              // src_layer
                        0,                              // src_x
                        0,                              // src_y: from top
                        self.width,                     // width
                        self.height - scroll_op.pixels, // height: all except scrolled
                        12,                             // 12 = SRC function (simple copy)
                        0,                              // dst_layer
                        0,                              // dst_x
                        scroll_op.pixels,               // dst_y: move down by N
                    );
                    self.send_and_record(&copy_instr).await?;

                    // Render only new content at top
                    self.handle_graphics_update(0, 0, self.width, scroll_op.pixels, image_data)
                        .await?;
                }
            }

            // Send sync instruction
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let sync_instr = format!("4.sync,{}.{};", timestamp.to_string().len(), timestamp);
            self.send_and_record(&sync_instr).await?;

            // Store pending sync for flow control
            self.sync_control.set_pending_sync(timestamp);
            self.sync_sent_at = Some(std::time::Instant::now());

            return Ok(());
        }

        // Use the dirty rect from IronRDP instead of always rendering full screen
        debug!(
            "RDP: No scroll detected, using dirty rect {}x{} at ({}, {})",
            width, height, x, y
        );

        // Update ONLY the dirty region from the full-screen image (efficient!)
        // IronRDP gives us the full screen, but we only copy the changed region
        self.framebuffer
            .update_region_from_fullscreen(x, y, width, height, image_data, self.width);

        // Encode and send the dirty region from framebuffer
        self.encode_and_send_dirty_rects().await
    }

    async fn handle_client_input_ironrdp(
        &mut self,
        framed: &mut ironrdp_tokio::Framed<ironrdp_tokio::MovableTokioStream<TokioTlsStream>>,
        msg: &Bytes,
        active_stage: &mut ActiveStage,
        image: &mut DecodedImage,
    ) -> Result<(), String> {
        use ironrdp_tokio::FramedWrite;

        // Debug: Log raw instruction
        trace!(
            "RDP: Raw client instruction: {}",
            String::from_utf8_lossy(msg)
        );

        let instr = GuacamoleParser::parse_instruction(msg)
            .map_err(|e| format!("Failed to parse instruction: {}", e))?;

        // Record client input (if recording is enabled and includes keys/mouse)
        self.record_client_input(msg);

        match instr.opcode {
            "sync" => {
                // Client is acknowledging a sync instruction (flow control)
                if let Some(ts_str) = instr.args.first() {
                    if let Ok(client_ts) = ts_str.parse::<u64>() {
                        if let Some(pending_ts) = self.sync_control.pending_timestamp() {
                            if client_ts >= pending_ts {
                                // Client caught up with our sync
                                self.sync_control.clear_pending();
                                self.sync_control.reset_timeout_count();
                                self.sync_sent_at = None;
                                trace!("RDP: Client acknowledged sync (ts={})", client_ts);
                            }
                        }
                    }
                }
                // Sync acks are not processed further
                return Ok(());
            }
            "key" => {
                if instr.args.len() >= 2 {
                    let keysym: u32 = instr.args[0].parse().map_err(|_| "Invalid keysym")?;
                    let pressed = instr.args[1] == "1";

                    // Security: Check read-only mode
                    // In graphical mode, allow Ctrl+C/Ctrl+Insert for copy
                    if self.read_only {
                        // TODO: Track modifier state for Ctrl detection
                        // For now, block all keyboard input in read-only mode
                        trace!("RDP: Keyboard input blocked (read-only mode)");
                        return Ok(());
                    }

                    let key_event = match self.input_handler.handle_keyboard(keysym, pressed) {
                        Ok(ev) => ev,
                        Err(_) => {
                            // Unknown keysym - skip silently (already logged by handle_keyboard)
                            return Ok(());
                        }
                    };

                    // Convert to IronRDP FastPathInputEvent
                    let mut flags = if pressed {
                        KeyboardFlags::empty()
                    } else {
                        KeyboardFlags::RELEASE
                    };
                    if key_event.extended {
                        flags |= KeyboardFlags::EXTENDED;
                    }

                    let event = FastPathInputEvent::KeyboardEvent(flags, key_event.scancode);
                    let outputs = active_stage
                        .process_fastpath_input(image, &[event])
                        .map_err(|e| format!("Failed to process keyboard input: {}", e))?;

                    // Send response frames to RDP server
                    for output in outputs {
                        if let ActiveStageOutput::ResponseFrame(frame) = output {
                            framed
                                .write_all(&frame)
                                .await
                                .map_err(|e| format!("Failed to write keyboard response: {}", e))?;
                        }
                    }

                    debug!(
                        "RDP: Keyboard event sent - scancode: 0x{:02x}, pressed: {}",
                        key_event.scancode, pressed
                    );
                }
            }
            "mouse" => {
                if instr.args.len() >= 3 {
                    // Debug: Log raw mouse instruction
                    info!(
                        "RDP: Raw mouse instruction - args[0]='{}' args[1]='{}' args[2]='{}'",
                        instr.args[0], instr.args[1], instr.args[2]
                    );

                    // Parse coordinates as integers (per Guacamole protocol spec)
                    // Protocol order: x, y, mask (NOT mask, x, y!)
                    let x: i32 = instr.args[0]
                        .parse()
                        .map_err(|e| format!("Invalid x '{}': {}", instr.args[0], e))?;
                    let y: i32 = instr.args[1]
                        .parse()
                        .map_err(|e| format!("Invalid y '{}': {}", instr.args[1], e))?;
                    let mask: u8 = instr.args[2]
                        .parse()
                        .map_err(|e| format!("Invalid mouse mask '{}': {}", instr.args[2], e))?;

                    info!(
                        "RDP: Parsed mouse - x={} y={} mask=0x{:02x} ({})",
                        x, y, mask, mask
                    );

                    // Security: Check read-only mode for mouse clicks
                    if self.read_only && !is_mouse_event_allowed_readonly(mask as u32) {
                        trace!("RDP: Mouse click blocked (read-only mode)");
                        return Ok(());
                    }

                    let pointer_event = self.input_handler.handle_mouse(mask, x, y)?;

                    // Convert Guacamole mouse mask to RDP PointerFlags
                    // Guacamole mask: bit 0=left, bit 1=middle, bit 2=right, bit 3=scroll up, bit 4=scroll down
                    let mut flags = PointerFlags::empty();
                    let mut wheel_units: i16 = 0;

                    // Handle button state changes
                    // RDP protocol: DOWN flag means "button just pressed", no DOWN means "button released"
                    if pointer_event.left_down {
                        flags |= PointerFlags::LEFT_BUTTON | PointerFlags::DOWN;
                    } else if pointer_event.left_up {
                        flags |= PointerFlags::LEFT_BUTTON; // No DOWN flag = release
                    } else if pointer_event.left_button {
                        // Button held while moving
                        flags |=
                            PointerFlags::LEFT_BUTTON | PointerFlags::DOWN | PointerFlags::MOVE;
                    } else {
                        // Just moving, no button
                        flags |= PointerFlags::MOVE;
                    }

                    if pointer_event.middle_down {
                        flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL | PointerFlags::DOWN;
                    } else if pointer_event.middle_up {
                        flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL;
                    } else if pointer_event.middle_button {
                        flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL
                            | PointerFlags::DOWN
                            | PointerFlags::MOVE;
                    }

                    if pointer_event.right_down {
                        flags |= PointerFlags::RIGHT_BUTTON | PointerFlags::DOWN;
                    } else if pointer_event.right_up {
                        flags |= PointerFlags::RIGHT_BUTTON;
                    } else if pointer_event.right_button {
                        flags |=
                            PointerFlags::RIGHT_BUTTON | PointerFlags::DOWN | PointerFlags::MOVE;
                    }

                    if pointer_event.scroll_up {
                        flags |= PointerFlags::VERTICAL_WHEEL;
                        wheel_units = 120; // Standard wheel delta
                    }
                    if pointer_event.scroll_down {
                        flags |= PointerFlags::VERTICAL_WHEEL | PointerFlags::WHEEL_NEGATIVE;
                        wheel_units = -120;
                    }

                    let mouse_pdu = MousePdu {
                        flags,
                        number_of_wheel_rotation_units: wheel_units,
                        x_position: pointer_event.x as u16,
                        y_position: pointer_event.y as u16,
                    };

                    let event = FastPathInputEvent::MouseEvent(mouse_pdu);
                    let outputs = active_stage
                        .process_fastpath_input(image, &[event])
                        .map_err(|e| format!("Failed to process mouse input: {}", e))?;

                    // Only send ResponseFrame back to the server.
                    // GraphicsUpdate from process_fastpath_input is IronRDP's internal
                    // pointer compositing into the framebuffer. Real screen updates from
                    // mouse actions (drag, click effects) arrive via read_pdu() in the
                    // main event loop. Sending these here causes full-screen redraws on
                    // every mouse move and cursor ghost artifacts.
                    for output in outputs {
                        if let ActiveStageOutput::ResponseFrame(frame) = output {
                            framed
                                .write_all(&frame)
                                .await
                                .map_err(|e| format!("Failed to write mouse response: {}", e))?;
                        }
                    }

                    debug!(
                        "RDP: Mouse event sent - x: {}, y: {}, flags: {:?}",
                        pointer_event.x, pointer_event.y, flags
                    );
                }
            }
            "clipboard" => {
                match self.channel_handler.handle_client_clipboard(msg) {
                    Ok(Some(cliprdr_data)) => {
                        info!(
                            "RDP: Clipboard data ready: {} bytes",
                            cliprdr_data.data.len()
                        );
                        // TODO: Send clipboard via CLIPRDR channel when implemented
                    }
                    Ok(None) => {}
                    Err(e) => warn!("RDP: Clipboard error: {}", e),
                }
            }
            "size" => {
                // Client size instruction format: size,<layer>,<width>,<height>;
                // We ignore the layer (args[0]) and use width/height (args[1], args[2])
                if instr.args.len() >= 3 {
                    let width: u32 = instr.args[1].parse().map_err(|_| "Invalid width")?;
                    let height: u32 = instr.args[2].parse().map_err(|_| "Invalid height")?;

                    // Standard guacd: DPI is set via connection parameters only, not dynamically updated
                    // Use the DPI value established during session initialization
                    info!(
                        "RDP: Client resize request: {}x{} @ {} DPI (layer: {}, current: {}x{} @ {} DPI)",
                        width, height, self.dpi, instr.args[0], self.width, self.height, self.dpi
                    );

                    // Update dimensions
                    self.width = width;
                    self.height = height;

                    // Recreate framebuffer and scroll detector with new dimensions
                    self.framebuffer = FrameBuffer::new(width, height);
                    self.scroll_detector.reset(width, height);

                    // Try server-side resize via DisplayControl DVC
                    match self.send_display_resize(active_stage, width, height).await {
                        Ok(_) => {
                            info!("RDP: DisplayControl resize sent successfully");
                        }
                        Err(e) => {
                            debug!(
                                "RDP: DisplayControl resize failed: {} - client will scale",
                                e
                            );
                        }
                    }
                }
            }
            _ => {
                debug!("RDP: Unhandled instruction: {}", instr.opcode);
            }
        }

        Ok(())
    }

    /// Encode RGBA framebuffer data as JPEG
    ///
    /// Converts RGBA pixels to RGB and encodes as JPEG with specified quality.
    /// This provides 33x bandwidth reduction compared to PNG encoding.
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
    ///
    /// Converts RGBA pixels to WebP with specified quality.
    /// This provides 40% bandwidth reduction compared to JPEG encoding.
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
    ///
    /// Converts RGBA pixels to WebP lossless format.
    /// Perfect quality with 33% smaller size than PNG.
    fn encode_webp_lossless(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        use webp::{Encoder, WebPMemory};

        let encoder = Encoder::from_rgba(data, width, height);
        let webp: WebPMemory = encoder.encode_lossless();
        Ok(webp.to_vec())
    }

    /// Encode RGBA data as PNG
    ///
    /// Converts RGBA pixels to PNG format (lossless).
    fn encode_png(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        let img = RgbaImage::from_raw(width, height, data.to_vec())
            .ok_or_else(|| "Invalid image dimensions".to_string())?;

        let mut png_data = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
        encoder
            .write_image(&img, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("PNG encode failed: {}", e))?;

        Ok(png_data)
    }

    /// Encode and send dirty rectangles from framebuffer
    async fn encode_and_send_dirty_rects(&mut self) -> Result<(), String> {
        self.framebuffer.optimize_dirty_rects();

        // Check if we have dirty rects (without cloning)
        if self.framebuffer.dirty_rects().is_empty() {
            // This should rarely happen - it means IronRDP sent us an update but
            // after processing there's nothing to render. This is OK for cursor-only updates.
            trace!("RDP: No dirty rects after optimization - skipping render (cursor-only update)");
            return Ok(());
        }

        // Calculate dirty region coverage (without cloning)
        let total_pixels = (self.width * self.height) as usize;
        let dirty_pixels: usize = self
            .framebuffer
            .dirty_rects()
            .iter()
            .map(|r| (r.width * r.height) as usize)
            .sum();
        let coverage_pct = (dirty_pixels * 100) / total_pixels;

        // Smart rendering: <30% = partial, >30% = full screen
        // This matches the FreeRDP optimization pattern
        let num_rects = self.framebuffer.dirty_rects().len();
        if coverage_pct < 30 {
            trace!(
                "RDP: Rendering {} dirty regions ({}% of screen)",
                num_rects,
                coverage_pct
            );
        } else {
            trace!(
                "RDP: Rendering full screen ({}% dirty, {} regions)",
                coverage_pct,
                num_rects
            );
        }

        // Use JPEG or PNG encoding based on configuration
        // Clone rects here since we need to iterate while mutating buffers
        let dirty_rects: Vec<_> = self.framebuffer.dirty_rects().to_vec();
        for rect in &dirty_rects {
            debug!(
                "RDP: Encoding rect {}x{} at ({}, {})",
                rect.width, rect.height, rect.x, rect.y
            );

            // Smart encoding: JPEG for large updates, PNG for small updates
            // JPEG is lossy and causes artifacts on re-encoding, so only use it for
            // large updates where bandwidth savings matter.
            // PNG is lossless and perfect for small dirty regions.
            let total_pixels = self.width * self.height;
            let rect_pixels = rect.width * rect.height;
            let is_large_update = rect_pixels > total_pixels / 10; // >10% of screen

            // Calculate adaptive quality for this frame
            let adaptive_quality = self.adaptive_quality.calculate_quality();

            // Get region pixels from framebuffer
            let region_pixels = self.framebuffer.get_region_pixels(*rect);

            let (encoded_data, mimetype) = if self.supports_webp {
                // WebP: Best of both worlds (40% smaller than JPEG, perfect quality)
                if is_large_update {
                    // Large update: WebP lossy with adaptive quality
                    let quality = adaptive_quality as f32 / 100.0;
                    let webp_data =
                        Self::encode_webp_lossy(&region_pixels, rect.width, rect.height, quality)
                            .map_err(|e| format!("WebP encoding failed: {}", e))?;
                    debug!(
                        "RDP: WebP lossy encoded - {} bytes (quality {}/{}, {}% of screen)",
                        webp_data.len(),
                        adaptive_quality,
                        self.jpeg_quality,
                        (rect_pixels * 100) / total_pixels
                    );
                    (webp_data, "image/webp")
                } else {
                    // Small update: WebP lossless (2KB vs 5KB PNG)
                    let webp_data =
                        Self::encode_webp_lossless(&region_pixels, rect.width, rect.height)
                            .map_err(|e| format!("WebP encoding failed: {}", e))?;
                    debug!(
                        "RDP: WebP lossless encoded - {} bytes ({}% of screen)",
                        webp_data.len(),
                        (rect_pixels * 100) / total_pixels
                    );
                    (webp_data, "image/webp")
                }
            } else if self.supports_jpeg && self.use_jpeg && is_large_update {
                // Fallback: JPEG for large updates with adaptive quality
                let jpeg_data =
                    Self::encode_jpeg(&region_pixels, rect.width, rect.height, adaptive_quality)
                        .map_err(|e| format!("JPEG encoding failed: {}", e))?;

                debug!(
                    "RDP: JPEG encoded - {} bytes (quality {}/{}, {}% of screen)",
                    jpeg_data.len(),
                    adaptive_quality,
                    self.jpeg_quality,
                    (rect_pixels * 100) / total_pixels
                );
                (jpeg_data, "image/jpeg")
            } else {
                // Fallback: PNG (always supported)
                let png_data = Self::encode_png(&region_pixels, rect.width, rect.height)
                    .map_err(|e| format!("PNG encoding failed: {}", e))?;

                debug!(
                    "RDP: PNG encoded - {} bytes ({}% of screen)",
                    png_data.len(),
                    (rect_pixels * 100) / total_pixels
                );
                (png_data, "image/png")
            };

            // ZERO-COPY: Encode base64 into reusable buffer (no allocation)
            // Use STANDARD base64 (client expects +/ characters, not -_)
            self.base64_buffer.clear();
            base64::Engine::encode_string(
                &base64::engine::general_purpose::STANDARD,
                &encoded_data,
                &mut self.base64_buffer,
            );

            debug!("RDP: Base64 encoded - {} bytes", self.base64_buffer.len());

            // Use modern stream-based protocol: img + blob chunks + end
            // This is the standard Guacamole protocol that works with all clients
            let stream_id = self.stream_id;
            let mask = 14u32; // GUAC_COMP_OVER (0x0E), matches Apache guacd
            let layer = 0; // Default layer

            // Send img instruction with stream metadata
            use guacr_protocol::{format_chunked_blobs, format_img};
            let img_instr = format_img(
                stream_id,
                mask,
                layer,
                mimetype,
                rect.x as i32,
                rect.y as i32,
            );

            info!(
                "RDP: Sending img instruction for rect {}x{} at ({}, {}) (stream {})",
                rect.width, rect.height, rect.x, rect.y, stream_id
            );
            self.send_and_record(&img_instr).await?;

            // Send blob chunks and end instruction
            let total_size = self.base64_buffer.len();
            let num_chunks = total_size.div_ceil(6144); // 6KB chunks

            debug!(
                "RDP: Sending blob in {} chunks ({} bytes total)",
                num_chunks, total_size
            );

            let blob_instructions = format_chunked_blobs(stream_id, &self.base64_buffer, None);
            for (chunk_idx, blob_instr) in blob_instructions.iter().enumerate() {
                self.send_and_record(blob_instr).await?;

                // Log first and last chunks
                if (chunk_idx == 0 || chunk_idx == blob_instructions.len() - 1)
                    && blob_instr.contains("blob")
                {
                    debug!(
                        "RDP: Sent blob chunk {}/{} ({} bytes)",
                        chunk_idx + 1,
                        num_chunks,
                        blob_instr.len()
                    );
                }
            }

            // Track this frame for adaptive quality calculation
            self.adaptive_quality.track_frame_sent(total_size);

            info!(
                "RDP: Sent complete image update (stream {}, {} format)",
                stream_id, mimetype
            );

            // Increment stream ID for next image (Guacamole protocol requires unique stream IDs)
            self.stream_id += 1;
        }

        self.framebuffer.clear_dirty();

        // Send sync instruction (reuse buffer to avoid allocation)
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Send sync instruction using shared protocol utilities
        let timestamp_str = timestamp_ms.to_string();
        let sync_instr = format_instruction("sync", &[&timestamp_str]);
        self.send_and_record(&sync_instr).await?;

        // Store pending sync for flow control (client must acknowledge)
        self.sync_control.set_pending_sync(timestamp_ms);
        self.sync_sent_at = Some(std::time::Instant::now());
        trace!(
            "RDP: Sync sent, waiting for client ack (ts: {})",
            timestamp_ms
        );

        Ok(())
    }

    async fn handle_graphics_update(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), String> {
        debug!(
            "RDP: handle_graphics_update - region: {}x{} at ({}, {}), pixels len: {}",
            width,
            height,
            x,
            y,
            pixels.len()
        );

        // IronRDP's DecodedImage is already in RGBA format (PixelFormat::RgbA32)
        // CRITICAL: pixels is a full-screen buffer, not a region buffer
        // Must use update_region_from_fullscreen to read correct rows
        self.framebuffer
            .update_region_from_fullscreen(x, y, width, height, pixels, self.width);

        // Encode and send the dirty rects
        self.encode_and_send_dirty_rects().await
    }

    /// Send instruction to client and record it (if recording is enabled)
    async fn send_and_record(&mut self, instruction: &str) -> Result<(), String> {
        let bytes = Bytes::from(instruction.to_string());

        // Record before sending (server-to-client direction)
        if let Some(ref mut recorder) = self.recorder {
            if let Err(e) = recorder.record_instruction(RecordingDirection::ServerToClient, &bytes)
            {
                warn!("RDP: Failed to record instruction: {}", e);
            }
        }

        self.to_client
            .send(bytes)
            .await
            .map_err(|e| format!("Failed to send: {}", e))
    }

    /// Record client input instruction (if recording is enabled)
    fn record_client_input(&mut self, instruction: &Bytes) {
        if let Some(ref mut recorder) = self.recorder {
            if let Err(e) =
                recorder.record_instruction(RecordingDirection::ClientToServer, instruction)
            {
                warn!("RDP: Failed to record client input: {}", e);
            }
        }
    }

    /// Send display resize via DisplayControl DVC
    async fn send_display_resize(
        &self,
        active_stage: &mut ActiveStage,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        // Get DisplayControl DVC
        let dvc = active_stage
            .get_dvc::<DisplayControlClient>()
            .ok_or_else(|| "DisplayControl DVC not available".to_string())?;

        let channel_id = dvc
            .channel_id()
            .ok_or_else(|| "DisplayControl channel not opened".to_string())?;

        // Get DisplayControl client processor
        let display_control = dvc
            .channel_processor_downcast_ref::<DisplayControlClient>()
            .ok_or_else(|| "Failed to downcast to DisplayControl client".to_string())?;

        // Check if DisplayControl is ready (capabilities received)
        if !display_control.ready() {
            return Err("DisplayControl not ready (capabilities not received)".to_string());
        }

        // Adjust display size to meet RDP requirements
        // Width must be >= 200, <= 8192, and even
        // Height must be >= 200, <= 8192
        let (adjusted_width, adjusted_height) =
            MonitorLayoutEntry::adjust_display_size(width, height);

        if adjusted_width != width || adjusted_height != height {
            info!(
                "RDP: Adjusted display size from {}x{} to {}x{}",
                width, height, adjusted_width, adjusted_height
            );
        }

        // Encode monitor layout message
        let messages = display_control
            .encode_single_primary_monitor(
                channel_id,
                adjusted_width,
                adjusted_height,
                None, // scale_factor
                None, // physical_dims
            )
            .map_err(|e| format!("Failed to encode monitor layout: {}", e))?;

        // Send via ActiveStage
        let _encoded = active_stage
            .encode_dvc_messages(messages)
            .map_err(|e| format!("Failed to encode DVC messages: {}", e))?;

        info!(
            "RDP: Sent DisplayControl resize to server: {}x{}",
            adjusted_width, adjusted_height
        );

        Ok(())
    }
}

// ============================================================================
// TLS Helper Functions
// ============================================================================

/// Dangerous certificate verifier that accepts all certificates
/// (Required for RDP servers with self-signed certificates)
#[derive(Debug)]
struct DangerousNoCertificateVerification;

impl tokio_rustls::rustls::client::danger::ServerCertVerifier
    for DangerousNoCertificateVerification
{
    fn verify_server_cert(
        &self,
        _end_entity: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[tokio_rustls::rustls::pki_types::CertificateDer<'_>],
        _server_name: &tokio_rustls::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: tokio_rustls::rustls::pki_types::UnixTime,
    ) -> Result<tokio_rustls::rustls::client::danger::ServerCertVerified, tokio_rustls::rustls::Error>
    {
        Ok(tokio_rustls::rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<tokio_rustls::rustls::SignatureScheme> {
        vec![
            tokio_rustls::rustls::SignatureScheme::RSA_PKCS1_SHA1,
            tokio_rustls::rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            tokio_rustls::rustls::SignatureScheme::RSA_PKCS1_SHA256,
            tokio_rustls::rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            tokio_rustls::rustls::SignatureScheme::RSA_PKCS1_SHA384,
            tokio_rustls::rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            tokio_rustls::rustls::SignatureScheme::RSA_PKCS1_SHA512,
            tokio_rustls::rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            tokio_rustls::rustls::SignatureScheme::RSA_PSS_SHA256,
            tokio_rustls::rustls::SignatureScheme::RSA_PSS_SHA384,
            tokio_rustls::rustls::SignatureScheme::RSA_PSS_SHA512,
            tokio_rustls::rustls::SignatureScheme::ED25519,
            tokio_rustls::rustls::SignatureScheme::ED448,
        ]
    }
}

/// Extract the server's public key from a TLS certificate
fn extract_tls_server_public_key(cert_der: &[u8]) -> Result<Vec<u8>, String> {
    use x509_cert::der::Decode;

    let cert = x509_cert::Certificate::from_der(cert_der)
        .map_err(|e| format!("Failed to parse certificate: {}", e))?;

    debug!(
        "RDP: Server certificate subject: {}",
        cert.tbs_certificate.subject
    );

    let server_public_key = cert
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| "Subject public key BIT STRING is not aligned".to_string())?
        .to_owned();

    Ok(server_public_key)
}

/// Dummy network client for CredSSP
///
/// We don't actually use network client functionality (it's for fetching auth tokens
/// from external services like KDC proxy), but the API requires one.
///
/// See: https://github.com/Devolutions/IronRDP/pull/1043
struct DummyNetworkClient;

impl ironrdp_async::NetworkClient for DummyNetworkClient {
    async fn send(
        &mut self,
        _request: &ironrdp::connector::sspi::generator::NetworkRequest,
    ) -> ironrdp::connector::ConnectorResult<Vec<u8>> {
        Err(ironrdp::connector::general_err!(
            "Network client not implemented"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rdp_handler_new() {
        let handler = RdpHandler::with_defaults();
        assert_eq!(<RdpHandler as ProtocolHandler>::name(&handler), "rdp");
    }

    #[test]
    fn test_rdp_config_defaults() {
        let config = RdpConfig::default();
        assert_eq!(config.default_port, 3389);
        assert_eq!(config.default_width, 1920);
        assert_eq!(config.default_height, 1080);
        assert_eq!(config.security_mode, "nla");
    }

    #[tokio::test]
    async fn test_rdp_handler_health() {
        let handler = RdpHandler::with_defaults();
        let health = handler.health_check().await.unwrap();
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn test_rdp_handler_stats() {
        let handler = RdpHandler::with_defaults();
        let stats = handler.stats().await.unwrap();
        assert_eq!(stats.total_connections, 0);
    }

    #[test]
    fn test_rdp_settings_from_params() {
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "server.example.com".to_string());
        params.insert("username".to_string(), "user".to_string());
        params.insert("password".to_string(), "pass".to_string());

        let defaults = RdpConfig::default();
        let settings = RdpSettings::from_params(&params, &defaults).unwrap();

        assert_eq!(settings.hostname, "server.example.com");
        assert_eq!(settings.port, 3389);
        assert_eq!(settings.width, 1920);
    }
}
