use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use guacr_protocol::{BinaryEncoder, GuacamoleParser};
use guacr_terminal::{BufferPool, HardwareEncoder, HardwareEncoderImpl};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

// Re-export supporting modules
use crate::channel_handler::RdpChannelHandler;
use crate::clipboard::{
    RdpClipboard, CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE,
};
use crate::framebuffer::FrameBuffer;
use crate::input_handler::RdpInputHandler;

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
            to_client,
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
                sender_clone.send(msg);
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
    pub domain: Option<String>,
    #[allow(dead_code)] // TODO: Used for connection brokers
    pub load_balance_info: Option<String>,
    #[allow(dead_code)] // TODO: Used for cert validation
    pub ignore_cert: bool,
    #[allow(dead_code)] // TODO: Used for cert validation
    pub cert_fingerprint: Option<String>,
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
            "RDP Settings: {}@{}:{}, {}x{}, security: {}, clipboard: {} bytes",
            username, hostname, port, width, height, security_mode, clipboard_buffer_size
        );

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
            domain,
            load_balance_info,
            ignore_cert,
            cert_fingerprint,
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
    binary_encoder: BinaryEncoder,
    #[allow(dead_code)] // TODO: Used for clipboard integration
    clipboard: RdpClipboard,
    input_handler: RdpInputHandler,
    channel_handler: RdpChannelHandler,
    hardware_encoder: Option<HardwareEncoderImpl>,
    video_stream_id: Option<u32>,
    stream_id: u32,
    #[allow(dead_code)] // TODO: Used for clipboard policy
    disable_copy: bool,
    #[allow(dead_code)] // TODO: Used for clipboard policy
    disable_paste: bool,
    to_client: mpsc::Sender<Bytes>,
    width: u32,
    height: u32,
    #[allow(dead_code)] // TODO: Used to track encoding mode
    use_hardware_encoding: bool,
    #[cfg(feature = "sftp")]
    sftp_session: Option<russh_sftp::client::SftpSession>,
}

impl IronRdpSession {
    fn new(
        width: u32,
        height: u32,
        clipboard_buffer_size: usize,
        disable_copy: bool,
        disable_paste: bool,
        to_client: mpsc::Sender<Bytes>,
    ) -> Self {
        let frame_size = (width * height * 4) as usize;
        let buffer_pool = Arc::new(BufferPool::new(8, frame_size));

        let use_hardware_encoding = width >= 3840 || height >= 2160;
        let hardware_encoder = if use_hardware_encoding {
            match HardwareEncoderImpl::new(width, height, 20) {
                Ok(encoder) => {
                    info!("RDP: Hardware encoder initialized: {}", encoder.name());
                    Some(encoder)
                }
                Err(e) => {
                    warn!("RDP: Hardware encoder unavailable: {}, using JPEG", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            framebuffer: FrameBuffer::new(width, height),
            buffer_pool,
            binary_encoder: BinaryEncoder::new(),
            clipboard: RdpClipboard::new(clipboard_buffer_size),
            input_handler: RdpInputHandler::new(),
            channel_handler: RdpChannelHandler::new(
                clipboard_buffer_size,
                disable_copy,
                disable_paste,
            ),
            hardware_encoder,
            video_stream_id: None,
            stream_id: 1,
            disable_copy,
            disable_paste,
            to_client,
            width,
            height,
            use_hardware_encoding,
            #[cfg(feature = "sftp")]
            sftp_session: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_and_run(
        mut self,
        hostname: &str,
        port: u16,
        _username: &str,
        _password: &str,
        _domain: Option<&str>,
        _security_mode: &str,
        from_client: mpsc::Receiver<Bytes>,
        #[cfg(feature = "sftp")] settings: Option<&RdpSettings>,
        #[cfg(not(feature = "sftp"))] _settings: Option<&RdpSettings>,
    ) -> Result<(), String> {
        info!("RDP: Connecting to {}:{}", hostname, port);

        let stream = TcpStream::connect((hostname, port))
            .await
            .map_err(|e| format!("TCP connection failed: {}", e))?;

        info!("RDP: TCP connection established");

        let ready_instr = format!("5.ready,{}.{};", 8, "rdp-ready");
        self.to_client
            .send(Bytes::from(ready_instr))
            .await
            .map_err(|e| format!("Failed to send ready: {}", e))?;

        let size_instr = self.binary_encoder.encode_size(0, self.width, self.height);
        self.to_client
            .send(size_instr)
            .await
            .map_err(|e| format!("Failed to send size: {}", e))?;

        info!("RDP: Session ready, starting event loop");

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

        self.run_event_loop(stream, from_client).await?;

        Ok(())
    }

    async fn run_event_loop<S>(
        &mut self,
        mut stream: S,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut read_buf = vec![0u8; 4096];

        loop {
            tokio::select! {
                result = stream.read(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                            info!("RDP: Connection closed by server");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = self.process_rdp_pdu(&read_buf[..n]).await {
                                error!("RDP: Error processing PDU: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            error!("RDP: Read error: {}", e);
                            break;
                        }
                    }
                }

                Some(msg) = from_client.recv() => {
                    if let Err(e) = self.handle_client_input(&mut stream, &msg).await {
                        warn!("RDP: Error handling client input: {}", e);
                    }
                }

                else => {
                    debug!("RDP: Event loop ended");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn process_rdp_pdu(&mut self, data: &[u8]) -> Result<(), String> {
        debug!("RDP: Processing {} bytes of RDP data", data.len());

        if data.is_empty() {
            return Ok(());
        }

        let first_byte = data[0];
        let is_fast_path = (first_byte & 0x80) != 0;

        if is_fast_path {
            return self.process_fast_path_bitmap(data).await;
        }

        if data.len() >= 4 && data[0] == 0x03 && data[1] == 0x00 {
            let tpkt_length = u16::from_be_bytes([data[2], data[3]]) as usize;
            if data.len() >= tpkt_length {
                return self.process_tpkt_channel(&data[4..tpkt_length]).await;
            }
        }

        if data.len() > 100 {
            debug!("RDP: Using heuristic bitmap detection (temporary)");
            let test_pixels = vec![255u8; 100 * 100 * 3];
            self.handle_graphics_update(0, 0, 100, 100, &test_pixels)
                .await?;
        }

        Ok(())
    }

    async fn process_fast_path_bitmap(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() < 5 {
            return Err("Fast-Path bitmap data too short".to_string());
        }

        let update_code = data[1];
        if update_code != 0 {
            debug!("RDP: Fast-Path update code: {} (not bitmap)", update_code);
            return Ok(());
        }

        let rect_count = u16::from_le_bytes([data[3], data[4]]) as usize;
        debug!(
            "RDP: Fast-Path bitmap update with {} rectangles",
            rect_count
        );

        let mut offset = 5;
        for _i in 0..rect_count {
            if offset + 18 > data.len() {
                break;
            }

            let x = u16::from_le_bytes([data[offset], data[offset + 1]]) as u32;
            let y = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as u32;
            let width = u16::from_le_bytes([data[offset + 8], data[offset + 9]]) as u32;
            let height = u16::from_le_bytes([data[offset + 10], data[offset + 11]]) as u32;
            let bpp = u16::from_le_bytes([data[offset + 12], data[offset + 13]]);

            offset += 18;

            let bytes_per_pixel = (bpp / 8) as usize;
            let bitmap_size = (width * height) as usize * bytes_per_pixel;

            if offset + bitmap_size > data.len() {
                warn!("RDP: Bitmap data incomplete, skipping rectangle");
                break;
            }

            let bitmap_data = &data[offset..offset + bitmap_size];

            self.handle_graphics_update(x, y, width, height, bitmap_data)
                .await?;

            offset += bitmap_size;
        }

        Ok(())
    }

    async fn process_tpkt_channel(&mut self, data: &[u8]) -> Result<(), String> {
        debug!("RDP: TPKT channel data received, {} bytes", data.len());
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
                self.handle_keyboard_input(stream, &instr.args).await?;
            }
            "mouse" => {
                self.handle_mouse_input(stream, &instr.args).await?;
            }
            "clipboard" => {
                self.handle_clipboard_input(stream, msg).await?;
            }
            "size" => {
                self.handle_resize(stream, &instr.args).await?;
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
                        warn!("RDP: SFTP file operation failed: {}", e);
                    }
                } else {
                    warn!("RDP: File transfer requested but SFTP not enabled");
                }
            }
            _ => {
                debug!("RDP: Unhandled instruction: {}", instr.opcode);
            }
        }

        Ok(())
    }

    async fn handle_keyboard_input<S>(
        &mut self,
        _stream: &mut S,
        args: &[&str],
    ) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        if args.len() < 2 {
            return Err("Invalid key instruction".to_string());
        }

        let keysym: u32 = args[0].parse().map_err(|_| "Invalid keysym".to_string())?;
        let pressed = args[1] == "1";

        let key_event = self.input_handler.handle_keyboard(keysym, pressed)?;

        debug!(
            "RDP: Sending keyboard event - scancode: 0x{:02x}, pressed: {}",
            key_event.scancode, key_event.pressed
        );

        Ok(())
    }

    async fn handle_mouse_input<S>(&mut self, _stream: &mut S, args: &[&str]) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        if args.len() < 3 {
            return Err("Invalid mouse instruction".to_string());
        }

        let mask: u8 = args[0]
            .parse()
            .map_err(|_| "Invalid mouse mask".to_string())?;
        let x: i32 = args[1]
            .parse()
            .map_err(|_| "Invalid x coordinate".to_string())?;
        let y: i32 = args[2]
            .parse()
            .map_err(|_| "Invalid y coordinate".to_string())?;

        let pointer_event = self.input_handler.handle_mouse(mask, x, y)?;

        debug!(
            "RDP: Sending mouse event - x: {}, y: {}, buttons: L={} M={} R={}",
            pointer_event.x,
            pointer_event.y,
            pointer_event.left_button,
            pointer_event.middle_button,
            pointer_event.right_button
        );

        Ok(())
    }

    async fn handle_clipboard_input<S>(
        &mut self,
        _stream: &mut S,
        msg: &Bytes,
    ) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        match self.channel_handler.handle_client_clipboard(msg) {
            Ok(Some(cliprdr_data)) => {
                info!(
                    "RDP: Clipboard data ready for server: format={:?}, {} bytes",
                    cliprdr_data.format,
                    cliprdr_data.data.len()
                );
            }
            Ok(None) => {}
            Err(e) => {
                warn!("RDP: Clipboard error: {}", e);
            }
        }

        Ok(())
    }

    async fn handle_resize<S>(&mut self, _stream: &mut S, args: &[&str]) -> Result<(), String>
    where
        S: AsyncWrite + Unpin,
    {
        if args.len() < 2 {
            return Err("Invalid size instruction".to_string());
        }

        let width: u32 = args[0].parse().map_err(|_| "Invalid width".to_string())?;
        let height: u32 = args[1].parse().map_err(|_| "Invalid height".to_string())?;

        info!("RDP: Resize requested: {}x{}", width, height);

        let _disp_msg = self.channel_handler.prepare_disp_resize(width, height);

        self.width = width;
        self.height = height;
        self.framebuffer = FrameBuffer::new(width, height);

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
        crate::simd::convert_bgr_to_rgba_simd(pixels, &mut rgba_vec, width, height);

        self.framebuffer
            .update_region(x, y, width, height, &rgba_vec);

        self.framebuffer.optimize_dirty_rects();
        let dirty_rects: Vec<_> = self.framebuffer.dirty_rects().to_vec();
        if dirty_rects.is_empty() {
            return Ok(());
        }

        if let Some(ref mut encoder) = self.hardware_encoder {
            let full_pixels = self.framebuffer.get_all_pixels();

            if let Ok(h264_data) = encoder.encode_frame(&full_pixels, self.width, self.height) {
                let video_stream_id = if let Some(id) = self.video_stream_id {
                    id
                } else {
                    let id = self.stream_id;
                    self.stream_id += 1;
                    self.video_stream_id = Some(id);

                    let video_msg = self.binary_encoder.encode_video(id, 0, "video/h264");
                    self.to_client
                        .send(video_msg)
                        .await
                        .map_err(|e| format!("Failed to send video instruction: {}", e))?;

                    id
                };

                let blob_msg = self.binary_encoder.encode_blob(video_stream_id, h264_data);
                self.to_client
                    .send(blob_msg)
                    .await
                    .map_err(|e| format!("Failed to send video blob: {}", e))?;

                self.framebuffer.clear_dirty();
                return Ok(());
            } else {
                debug!("RDP: Hardware encoding failed, falling back to JPEG");
            }
        }

        for rect in &dirty_rects {
            let png_data = self
                .framebuffer
                .encode_region(*rect)
                .map_err(|e| format!("Encoding failed: {}", e))?;

            let png_bytes = Bytes::from(png_data);

            let msg = self.binary_encoder.encode_image(
                self.stream_id,
                0,
                rect.x as i32,
                rect.y as i32,
                rect.width as u16,
                rect.height as u16,
                0,
                png_bytes,
            );

            self.to_client
                .send(msg)
                .await
                .map_err(|e| format!("Failed to send image: {}", e))?;
        }

        self.framebuffer.clear_dirty();

        let sync_msg = self.binary_encoder.encode_sync(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
        self.to_client
            .send(sync_msg)
            .await
            .map_err(|e| format!("Failed to send sync: {}", e))?;

        Ok(())
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
