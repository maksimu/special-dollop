use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use guacr_protocol::{BinaryEncoder, GuacamoleParser};
use guacr_rdp::FrameBuffer;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
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
}

impl Default for VncConfig {
    fn default() -> Self {
        Self {
            default_port: 5900,
            default_width: 1920,
            default_height: 1080,
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
        let mut client = VncClient::new(settings.width, settings.height, to_client);

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

        let width = params
            .get("width")
            .and_then(|w| w.parse().ok())
            .unwrap_or(defaults.default_width);

        let height = params
            .get("height")
            .and_then(|h| h.parse().ok())
            .unwrap_or(defaults.default_height);

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

        info!("VNC Settings: {}:{}, {}x{}", hostname, port, width, height);

        Ok(Self {
            hostname,
            port,
            password,
            width,
            height,
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
    #[cfg(feature = "sftp")]
    sftp_session: Option<russh_sftp::client::SftpSession>,
    to_client: mpsc::Sender<Bytes>,
}

impl VncClient {
    fn new(width: u32, height: u32, to_client: mpsc::Sender<Bytes>) -> Self {
        Self {
            framebuffer: FrameBuffer::new(width, height),
            binary_encoder: BinaryEncoder::new(),
            stream_id: 1,
            width,
            height,
            #[cfg(feature = "sftp")]
            sftp_session: None,
            to_client,
        }
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
        info!("VNC: Connecting to {}:{}", hostname, port);

        let mut stream = TcpStream::connect((hostname, port))
            .await
            .map_err(|e| format!("TCP connection failed: {}", e))?;

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

        let ready_instr = format!("5.ready,{}.{};", 9, "vnc-ready");
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

        loop {
            tokio::select! {
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
                    if let (Ok(mask), Ok(x), Ok(y)) = (
                        instr.args[0].parse::<u8>(),
                        instr.args[1].parse::<i32>(),
                        instr.args[2].parse::<i32>(),
                    ) {
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

        for dirty_rect in &dirty_rects {
            let png_data = self
                .framebuffer
                .encode_region(*dirty_rect)
                .map_err(|e| format!("Encoding failed: {}", e))?;

            let png_bytes = Bytes::from(png_data);

            let msg = self.binary_encoder.encode_image(
                self.stream_id,
                0,
                dirty_rect.x as i32,
                dirty_rect.y as i32,
                dirty_rect.width as u16,
                dirty_rect.height as u16,
                0,
                png_bytes,
            );

            self.to_client
                .send(msg)
                .await
                .map_err(|e| format!("Failed to send image: {}", e))?;
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
