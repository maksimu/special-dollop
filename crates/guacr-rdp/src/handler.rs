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
    InstructionSender,
    KeepAliveManager,
    MultiFormatRecorder,
    ProtocolHandler,
    // Recording
    RecordingConfig,
    RecordingDirection,
    DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
use guacr_protocol::GuacamoleParser;
use guacr_terminal::BufferPool;
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

type TokioTlsStream = tokio_rustls::client::TlsStream<TcpStream>;

// Re-export supporting modules
use crate::channel_handler::RdpChannelHandler;

// Import shared types from guacr-terminal
use guacr_terminal::{
    FrameBuffer, RdpClipboard, RdpInputHandler, ScrollDetector, ScrollDirection,
    CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE,
};

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
    pub security_mode: String,
    /// Clipboard buffer size in bytes (256KB - 50MB)
    pub clipboard_buffer_size: usize,
    /// Whether clipboard copy is disabled
    pub disable_copy: bool,
    /// Whether clipboard paste is disabled
    pub disable_paste: bool,
}

impl Default for RdpConfig {
    fn default() -> Self {
        Self {
            default_port: 3389,
            default_width: 1920,
            default_height: 1080,
            security_mode: "nla".to_string(), // Network Level Authentication
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,
            disable_copy: false,
            disable_paste: false,
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
            settings.clipboard_buffer_size,
            settings.disable_copy,
            settings.disable_paste,
            settings.read_only,
            settings.security.connection_timeout_secs,
            settings.recording_config.clone(),
            to_client,
            &params,
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
                #[cfg(feature = "sftp")]
                Some(&settings),
                #[cfg(not(feature = "sftp"))]
                None,
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
        let (to_client, mut handler_rx) = mpsc::channel::<Bytes>(128);

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
    pub security_mode: String,
    pub clipboard_buffer_size: usize,
    pub disable_copy: bool,
    pub disable_paste: bool,
    /// Read-only mode - blocks keyboard/mouse input
    pub read_only: bool,
    /// Security settings (includes connection timeout)
    pub security: HandlerSecuritySettings,
    pub domain: Option<String>,
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

        let width = params
            .get("width")
            .and_then(|w| w.parse().ok())
            .unwrap_or(defaults.default_width);

        let height = params
            .get("height")
            .and_then(|h| h.parse().ok())
            .unwrap_or(defaults.default_height);

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

        // Parse security settings (includes connection timeout)
        let security = HandlerSecuritySettings::from_params(params);

        // Parse recording configuration
        let recording_config = RecordingConfig::from_params(params);

        let domain = params.get("domain").cloned();
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
            security_mode,
            clipboard_buffer_size,
            disable_copy,
            disable_paste,
            read_only,
            security,
            domain,
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
    channel_handler: RdpChannelHandler,
    /// Scroll detector for bandwidth optimization (90-99% savings on scrolling)
    scroll_detector: ScrollDetector,
    stream_id: u32,
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
    #[allow(dead_code)] // TODO: Used to track encoding mode
    use_hardware_encoding: bool,
    #[cfg(feature = "sftp")]
    sftp_session: Option<russh_sftp::client::SftpSession>,
}

impl IronRdpSession {
    #[allow(clippy::too_many_arguments)]
    fn new(
        width: u32,
        height: u32,
        clipboard_buffer_size: usize,
        disable_copy: bool,
        disable_paste: bool,
        read_only: bool,
        connection_timeout_secs: u64,
        recording_config: RecordingConfig,
        to_client: mpsc::Sender<Bytes>,
        params: &HashMap<String, String>,
    ) -> Self {
        let frame_size = (width * height * 4) as usize;
        let buffer_pool = Arc::new(BufferPool::new(8, frame_size));

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

        Self {
            framebuffer: FrameBuffer::new(width, height),
            buffer_pool,
            clipboard: RdpClipboard::new(clipboard_buffer_size),
            input_handler: RdpInputHandler::new(),
            scroll_detector: ScrollDetector::new(width, height),
            channel_handler: RdpChannelHandler::new(
                clipboard_buffer_size,
                disable_copy,
                disable_paste,
            ),
            stream_id: 1,
            disable_copy,
            disable_paste,
            read_only,
            connection_timeout_secs,
            recording_config,
            recorder,
            to_client,
            width,
            height,
            use_hardware_encoding: false, // Hardware encoding disabled for now
            #[cfg(feature = "sftp")]
            sftp_session: None,
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
        #[cfg(feature = "sftp")] settings: Option<&RdpSettings>,
        #[cfg(not(feature = "sftp"))] _settings: Option<&RdpSettings>,
    ) -> Result<(), String> {
        info!(
            "RDP: Connecting to {}:{} (timeout: {}s)",
            hostname, port, self.connection_timeout_secs
        );

        // Build IronRDP connector config
        let config = self.build_ironrdp_config(username, password, domain);

        // Establish TCP connection with timeout (matches guacd behavior)
        let stream = connect_tcp_with_timeout((hostname, port), self.connection_timeout_secs)
            .await
            .map_err(|e| format!("{}", e))?;

        info!("RDP: TCP connection established");

        // Perform RDP handshake and authentication
        let (connection_result, framed) = self
            .perform_rdp_handshake(stream, config, hostname.to_string())
            .await
            .map_err(|e| format!("RDP handshake failed: {}", e))?;

        info!(
            "RDP: Connection established - {}x{}",
            connection_result.desktop_size.width, connection_result.desktop_size.height
        );

        // Update session dimensions if they differ from requested
        self.width = u32::from(connection_result.desktop_size.width);
        self.height = u32::from(connection_result.desktop_size.height);
        self.framebuffer = FrameBuffer::new(self.width, self.height);
        self.scroll_detector.reset(self.width, self.height);

        // Send ready and size instructions to client (and record them)
        let ready_value = "rdp-ready";
        let ready_instr = format!("5.ready,{}.{};", ready_value.len(), ready_value);
        self.send_and_record(&ready_instr).await?;

        // Format: size,<layer>,<width>,<height>;
        let layer = 0;
        let layer_str = layer.to_string();
        let width_str = self.width.to_string();
        let height_str = self.height.to_string();
        let size_instr = format!(
            "4.size,{}.{},{}.{},{}.{};",
            layer_str.len(),
            layer_str,
            width_str.len(),
            width_str,
            height_str.len(),
            height_str
        );
        self.send_and_record(&size_instr).await?;

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

    fn build_ironrdp_config(
        &self,
        username: &str,
        password: &str,
        domain: Option<&str>,
    ) -> connector::Config {
        connector::Config {
            credentials: Credentials::UsernamePassword {
                username: username.to_string(),
                password: password.to_string(),
            },
            domain: domain.map(|s| s.to_string()),
            enable_tls: true,
            enable_credssp: true,
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
            enable_server_pointer: true,
            request_data: None,
            autologon: false,
            enable_audio_playback: false,
            pointer_software_rendering: true,
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

        Ok((connection_result, upgraded_framed))
    }

    async fn tls_upgrade(
        &self,
        stream: TcpStream,
        server_name: &str,
    ) -> Result<(TokioTlsStream, Vec<u8>), String> {
        use tokio_rustls::rustls;

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
                            debug!("RDP: Received frame - action: {:?}, {} bytes", action, payload.len());

                            let outputs = active_stage
                                .process(&mut image, action, &payload)
                                .map_err(|e| format!("Failed to process RDP frame: {}", e))?;

                            for output in outputs {
                                match output {
                                    ActiveStageOutput::ResponseFrame(frame) => {
                                        framed
                                            .write_all(&frame)
                                            .await
                                            .map_err(|e| format!("Failed to write response: {}", e))?;
                                    }
                                    ActiveStageOutput::GraphicsUpdate(_rect) => {
                                        // Send graphics update to client
                                        self.send_graphics_update(&image).await?;
                                    }
                                    ActiveStageOutput::Terminate(reason) => {
                                        info!("RDP: Session terminated: {:?}", reason);
                                        return Ok(());
                                    }
                                    _ => {
                                        debug!("RDP: Unhandled output: {:?}", output);
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
                Some(msg) = from_client.recv() => {
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

    async fn send_graphics_update(&mut self, image: &DecodedImage) -> Result<(), String> {
        // Convert DecodedImage to our framebuffer format and send to client
        let image_data = image.data();

        // Check for scroll operation first (most bandwidth-efficient)
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

            return Ok(());
        }

        // No scroll detected - update framebuffer and use normal dirty region rendering
        self.framebuffer
            .update_region(0, 0, self.width, self.height, image_data);

        // Encode and send the update
        self.handle_graphics_update(0, 0, self.width, self.height, image_data)
            .await
    }

    async fn handle_client_input_ironrdp(
        &mut self,
        framed: &mut ironrdp_tokio::Framed<ironrdp_tokio::MovableTokioStream<TokioTlsStream>>,
        msg: &Bytes,
        active_stage: &mut ActiveStage,
        image: &mut DecodedImage,
    ) -> Result<(), String> {
        use ironrdp_tokio::FramedWrite;

        let instr = GuacamoleParser::parse_instruction(msg)
            .map_err(|e| format!("Failed to parse instruction: {}", e))?;

        // Record client input (if recording is enabled and includes keys/mouse)
        self.record_client_input(msg);

        match instr.opcode {
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

                    let key_event = self.input_handler.handle_keyboard(keysym, pressed)?;

                    // Convert to IronRDP FastPathInputEvent
                    let flags = if pressed {
                        KeyboardFlags::empty()
                    } else {
                        KeyboardFlags::RELEASE
                    };

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
                    let mask: u8 = instr.args[0].parse().map_err(|_| "Invalid mouse mask")?;
                    let x: i32 = instr.args[1].parse().map_err(|_| "Invalid x")?;
                    let y: i32 = instr.args[2].parse().map_err(|_| "Invalid y")?;

                    // Security: Check read-only mode for mouse clicks
                    if self.read_only && !is_mouse_event_allowed_readonly(mask as u32) {
                        trace!("RDP: Mouse click blocked (read-only mode)");
                        return Ok(());
                    }

                    let pointer_event = self.input_handler.handle_mouse(mask, x, y)?;

                    // Convert Guacamole mouse mask to RDP PointerFlags
                    // Guacamole mask: bit 0=left, bit 1=middle, bit 2=right, bit 3=scroll up, bit 4=scroll down
                    let mut flags = PointerFlags::MOVE;
                    let mut wheel_units: i16 = 0;

                    if pointer_event.left_button {
                        flags |= PointerFlags::LEFT_BUTTON | PointerFlags::DOWN;
                    }
                    if pointer_event.middle_button {
                        flags |= PointerFlags::MIDDLE_BUTTON_OR_WHEEL | PointerFlags::DOWN;
                    }
                    if pointer_event.right_button {
                        flags |= PointerFlags::RIGHT_BUTTON | PointerFlags::DOWN;
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

                    // Send response frames and handle graphics updates
                    for output in outputs {
                        match output {
                            ActiveStageOutput::ResponseFrame(frame) => {
                                framed.write_all(&frame).await.map_err(|e| {
                                    format!("Failed to write mouse response: {}", e)
                                })?;
                            }
                            ActiveStageOutput::GraphicsUpdate(_rect) => {
                                // Pointer moved, send graphics update
                                self.send_graphics_update(image).await?;
                            }
                            _ => {}
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
                if instr.args.len() >= 2 {
                    let width: u32 = instr.args[0].parse().map_err(|_| "Invalid width")?;
                    let height: u32 = instr.args[1].parse().map_err(|_| "Invalid height")?;
                    info!("RDP: Resize requested: {}x{}", width, height);
                    // TODO: Send display control resize via DISP channel
                    let _disp_msg = self.channel_handler.prepare_disp_resize(width, height);
                }
            }
            _ => {
                debug!("RDP: Unhandled instruction: {}", instr.opcode);
            }
        }

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
        let mut rgba_vec = vec![0u8; (width * height * 4) as usize];
        guacr_terminal::convert_bgr_to_rgba_simd(pixels, &mut rgba_vec, width, height);

        self.framebuffer
            .update_region(x, y, width, height, &rgba_vec);

        self.framebuffer.optimize_dirty_rects();
        let dirty_rects: Vec<_> = self.framebuffer.dirty_rects().to_vec();
        if dirty_rects.is_empty() {
            return Ok(());
        }

        // Calculate dirty region coverage
        let total_pixels = (self.width * self.height) as usize;
        let dirty_pixels: usize = dirty_rects
            .iter()
            .map(|r| (r.width * r.height) as usize)
            .sum();
        let coverage_pct = (dirty_pixels * 100) / total_pixels;

        // Smart rendering: <30% = partial, >30% = full screen
        // This matches the FreeRDP optimization pattern
        if coverage_pct < 30 {
            trace!(
                "RDP: Rendering {} dirty regions ({}% of screen)",
                dirty_rects.len(),
                coverage_pct
            );
        } else {
            trace!(
                "RDP: Rendering full screen ({}% dirty, {} regions)",
                coverage_pct,
                dirty_rects.len()
            );
        }

        // Use PNG encoding for all graphics updates
        for rect in &dirty_rects {
            let png_data = self
                .framebuffer
                .encode_region(*rect)
                .map_err(|e| format!("Encoding failed: {}", e))?;

            // Encode PNG as base64 for text protocol
            let png_base64 =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png_data);

            // Format img instruction: img,<stream>,<mask>,<layer>,<mimetype>,<x>,<y>;
            // Then send blob instruction: blob,<stream>,<base64_data>;
            // Every element needs LENGTH.VALUE format
            let stream_str = self.stream_id.to_string();
            let mask = 7; // RGB channels (0x07)
            let layer = 0; // Default layer
            let mask_str = mask.to_string();
            let layer_str = layer.to_string();
            let x_str = rect.x.to_string();
            let y_str = rect.y.to_string();
            let mimetype = "image/png";

            let img_instr = format!(
                "3.img,{}.{},{}.{},{}.{},{}.{},{}.{},{}.{};",
                stream_str.len(),
                stream_str,
                mask_str.len(),
                mask_str,
                layer_str.len(),
                layer_str,
                mimetype.len(),
                mimetype,
                x_str.len(),
                x_str,
                y_str.len(),
                y_str
            );

            let blob_instr = format!(
                "4.blob,{}.{},{}.{};",
                stream_str.len(),
                stream_str,
                png_base64.len(),
                png_base64
            );

            // Send and record instructions
            self.send_and_record(&img_instr).await?;
            self.send_and_record(&blob_instr).await?;

            // Send end instruction to close the stream
            let end_instr = guacr_protocol::format_end(self.stream_id);
            self.send_and_record(&end_instr).await?;
        }

        self.framebuffer.clear_dirty();

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let ts_str = timestamp_ms.to_string();
        let sync_instr = format!("4.sync,{}.{};", ts_str.len(), ts_str);

        self.send_and_record(&sync_instr).await?;

        Ok(())
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
