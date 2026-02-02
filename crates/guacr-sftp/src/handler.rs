use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler,
    EventCallback,
    HandlerError,
    HandlerStats,
    HealthStatus,
    // Host key verification
    HostKeyConfig,
    HostKeyResult,
    HostKeyVerifier,
    ProtocolHandler,
    // Security
    SftpSecuritySettings,
};
use guacr_protocol::{format_chunked_blobs, TextProtocolEncoder};
use log::{debug, error, info, warn};
use russh::client;
use russh_keys::key;
use russh_keys::PublicKeyBase64;
use russh_sftp::client::SftpSession;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::channel_adapter::ChannelStreamAdapter;
use crate::file_browser::{FileBrowser, FileEntry};

/// SFTP handler - secure file transfer over SSH
///
/// Provides graphical file browser interface over SFTP with upload/download support.
pub struct SftpHandler {
    config: SftpConfig,
}

#[derive(Debug, Clone)]
pub struct SftpConfig {
    pub default_port: u16,
    pub chroot_to_home: bool, // Security: Restrict to home directory
    pub allow_uploads: bool,
    pub allow_downloads: bool,
    pub allow_delete: bool,
    pub max_file_size: u64, // Maximum file size for transfer (default: 100MB)
}

impl Default for SftpConfig {
    fn default() -> Self {
        Self {
            default_port: 22,
            chroot_to_home: true, // Security: Restrict access
            allow_uploads: true,
            allow_downloads: true,
            allow_delete: false,              // Security: No delete by default
            max_file_size: 100 * 1024 * 1024, // 100MB default
        }
    }
}

impl SftpHandler {
    pub fn new(config: SftpConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(SftpConfig::default())
    }

    /// Validate path is within chroot (home directory)
    fn validate_path(&self, path: &Path, home: &Path) -> Result<PathBuf, HandlerError> {
        if !self.config.chroot_to_home {
            return Ok(path.to_path_buf());
        }

        let canonical = path.canonicalize().map_err(|e| {
            HandlerError::SecurityViolation(format!("Path resolution failed: {}", e))
        })?;

        let home_canonical = home.canonicalize().map_err(|e| {
            HandlerError::SecurityViolation(format!("Home resolution failed: {}", e))
        })?;

        if canonical.starts_with(&home_canonical) {
            Ok(canonical)
        } else {
            Err(HandlerError::SecurityViolation(format!(
                "Path traversal detected: {} outside {}",
                canonical.display(),
                home_canonical.display()
            )))
        }
    }

    /// Test helper: Validate path (public for integration tests)
    #[doc(hidden)]
    pub fn test_validate_path(&self, path: &Path, home: &Path) -> Result<PathBuf, HandlerError> {
        self.validate_path(path, home)
    }
}

#[async_trait]
impl ProtocolHandler for SftpHandler {
    fn name(&self) -> &str {
        "sftp"
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
        info!("SFTP handler starting");

        // Parse security settings from params
        let security = SftpSecuritySettings::from_params(&params);
        info!(
            "SFTP: Security - read_only={}, download={}, upload={}, root={:?}",
            security.base.read_only,
            security.is_download_allowed(),
            security.is_upload_allowed(),
            security.root_directory
        );

        // Parse host key verification configuration
        let host_key_config = HostKeyConfig::from_params(&params);
        if host_key_config.ignore_host_key {
            warn!("SFTP: Host key verification DISABLED (ignore-host-key=true) - INSECURE");
        } else if host_key_config.known_hosts_path.is_some() {
            info!(
                "SFTP: Host key verification via known_hosts: {}",
                host_key_config
                    .known_hosts_path
                    .as_deref()
                    .unwrap_or("(none)")
            );
        } else if host_key_config.host_key_fingerprint.is_some() {
            info!("SFTP: Host key verification via pinned fingerprint");
        }

        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let username = params
            .get("username")
            .ok_or_else(|| HandlerError::MissingParameter("username".to_string()))?;

        let password = params.get("password");
        let private_key = params.get("private_key");
        let private_key_passphrase = params.get("private_key_passphrase");

        info!("SFTP connecting to {}@{}:{}", username, hostname, port);

        // Connect via SSH (reuse SSH connection logic)
        let config = Arc::new(client::Config::default());

        // Create SFTP client handler with host key verification
        let sftp_client_handler = SftpClientHandler::new(hostname.clone(), port, host_key_config);

        let mut session = client::connect(config, (hostname.as_str(), port), sftp_client_handler)
            .await
            .map_err(|e| {
                let error_str = e.to_string();
                if error_str.contains("host key") || error_str.contains("fingerprint") {
                    error!("SFTP: Host key verification failed: {}", e);
                    HandlerError::AuthenticationFailed(format!(
                        "Host key verification failed: {}",
                        e
                    ))
                } else {
                    HandlerError::ConnectionFailed(format!("SSH connection failed: {}", e))
                }
            })?;

        // Authenticate
        let authenticated = if let Some(pwd) = password {
            session
                .authenticate_password(username, pwd)
                .await
                .map_err(|e| {
                    HandlerError::AuthenticationFailed(format!("Password auth failed: {}", e))
                })?
        } else if let Some(key_data) = private_key {
            let passphrase = private_key_passphrase.map(|s| s.as_str());
            let key = russh_keys::decode_secret_key(key_data, passphrase).map_err(|e| {
                HandlerError::AuthenticationFailed(format!("Key decode failed: {}", e))
            })?;
            session
                .authenticate_publickey(username, Arc::new(key))
                .await
                .map_err(|e| {
                    HandlerError::AuthenticationFailed(format!("Key auth failed: {}", e))
                })?
        } else {
            return Err(HandlerError::MissingParameter(
                "password or private_key required".to_string(),
            ));
        };

        if !authenticated {
            return Err(HandlerError::AuthenticationFailed(
                "Authentication failed".to_string(),
            ));
        }

        info!("SFTP: SSH authentication successful");

        // Open SFTP channel
        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| HandlerError::ProtocolError(format!("Channel open failed: {}", e)))?;

        channel.request_subsystem(true, "sftp").await.map_err(|e| {
            HandlerError::ProtocolError(format!("SFTP subsystem request failed: {}", e))
        })?;

        info!("SFTP: Creating SFTP session from SSH channel");

        // Create SFTP session from the channel
        // russh-sftp 2.1 API: SftpSession::new takes a stream (AsyncRead + AsyncWrite)
        // russh channels use a message-based API, so we create an adapter
        let (channel_stream, _forwarder_handle) = ChannelStreamAdapter::new(channel);

        let sftp = SftpSession::new(channel_stream).await.map_err(|e| {
            HandlerError::ProtocolError(format!("SFTP session creation failed: {}", e))
        })?;

        info!("SFTP: Session established successfully");

        // Get actual home directory from SFTP server
        // Try canonicalize with "~" or use default
        let home = match sftp.canonicalize("~".to_string()).await {
            Ok(path_str) => {
                let path = PathBuf::from(path_str);
                info!("SFTP: Detected home directory: {}", path.display());
                path
            }
            Err(_) => {
                // Fallback to default home
                let default_home = PathBuf::from("/home").join(username);
                warn!(
                    "SFTP: Could not detect home directory, using default: {}",
                    default_home.display()
                );
                default_home
            }
        };

        let mut current_path = home.clone();

        // Get initial directory listing
        let mut file_entries = vec![
            FileEntry {
                name: ".".to_string(),
                size: 0,
                is_directory: true,
                permissions: "drwxr-xr-x".to_string(),
                modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
            },
            FileEntry {
                name: "..".to_string(),
                size: 0,
                is_directory: true,
                permissions: "drwxr-xr-x".to_string(),
                modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
            },
        ];

        // Read directory entries from SFTP server
        match sftp
            .read_dir(current_path.to_string_lossy().to_string())
            .await
        {
            Ok(read_dir) => {
                // ReadDir implements Iterator (synchronous), not Stream
                // Collect all entries
                for dir_entry in read_dir {
                    let metadata = dir_entry.metadata();
                    let file_name = dir_entry.file_name().to_string();

                    // Skip hidden files starting with '.' (except . and ..)
                    if file_name.starts_with('.') && file_name != "." && file_name != ".." {
                        continue;
                    }

                    let is_dir = metadata.is_dir();
                    let size = metadata.len();

                    // Format permissions (simplified)
                    let perms = if is_dir {
                        "drwxr-xr-x".to_string()
                    } else {
                        "-rw-r--r--".to_string()
                    };

                    // Format modified time
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .and_then(|d| {
                                    chrono::DateTime::<chrono::Utc>::from_timestamp(
                                        d.as_secs() as i64,
                                        0,
                                    )
                                })
                                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        })
                        .unwrap_or("Unknown".to_string());

                    file_entries.push(FileEntry {
                        name: file_name,
                        size,
                        is_directory: is_dir,
                        permissions: perms,
                        modified,
                    });
                }
            }
            Err(e) => {
                warn!(
                    "SFTP: Failed to read directory {}: {}",
                    current_path.display(),
                    e
                );
            }
        }

        let mut file_browser =
            FileBrowser::new(current_path.to_string_lossy().to_string(), file_entries);

        // Render initial file browser
        let mut protocol_encoder = TextProtocolEncoder::new();
        let mut stream_id = 1u32;
        let browser_image = file_browser
            .render_to_png(1920, 1080)
            .map_err(|e| HandlerError::ProtocolError(format!("Render failed: {}", e)))?;

        // Base64 encode and send via modern zero-allocation protocol
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &browser_image);

        let img_instr = protocol_encoder.format_img_instruction(stream_id, 0, 0, 0, "image/png");
        to_client
            .send(img_instr.freeze())
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
        for instr in blob_instructions {
            to_client
                .send(Bytes::from(instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        }
        stream_id += 1;

        // Main event loop
        loop {
            tokio::select! {
                // Client messages
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        info!("SFTP: Client disconnected");
                        break;
                    };

                    let msg_str = String::from_utf8_lossy(&msg);

                    // Handle mouse clicks for navigation
                    if msg_str.contains(".mouse,") {
                        // Parse mouse instruction: mouse,<layer>,<x>,<y>,<button_mask>;
                        if let Some(mouse_part) = msg_str.split_once(".mouse,") {
                            let parts: Vec<&str> = mouse_part.1.split(',').collect();
                            if parts.len() >= 4 {
                                if let (Ok(_x), Ok(y), Ok(button_mask)) = (
                                    parts[1].parse::<u32>(),
                                    parts[2].parse::<u32>(),
                                    parts[3].trim_end_matches(';').parse::<u32>(),
                                ) {
                                    // Single click: navigate to directory
                                    if button_mask == 1 {
                                        file_browser.handle_click(y);

                                        if let Some(entry) = file_browser.get_selected() {
                                            if entry.is_directory {
                                                // Navigate into directory
                                                let new_path = current_path.join(&entry.name);
                                                let validated_path = self.validate_path(&new_path, &home)?;

                                                // Read directory entries from SFTP server
                                                let mut file_entries = vec![
                                                    FileEntry {
                                                        name: ".".to_string(),
                                                        size: 0,
                                                        is_directory: true,
                                                        permissions: "drwxr-xr-x".to_string(),
                                                        modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                                                    },
                                                    FileEntry {
                                                        name: "..".to_string(),
                                                        size: 0,
                                                        is_directory: true,
                                                        permissions: "drwxr-xr-x".to_string(),
                                                        modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                                                    },
                                                ];

                                                match sftp.read_dir(validated_path.to_string_lossy().to_string()).await {
                                                    Ok(read_dir) => {
                                                        // ReadDir implements Iterator (synchronous)
                                                        for dir_entry in read_dir {
                                                            let metadata = dir_entry.metadata();
                                                            let file_name = dir_entry.file_name().to_string();

                                                            // Skip hidden files
                                                            if file_name.starts_with('.') && file_name != "." && file_name != ".." {
                                                                continue;
                                                            }

                                                            let is_dir = metadata.is_dir();
                                                            let size = metadata.len();
                                                            let perms = if is_dir {
                                                                "drwxr-xr-x".to_string()
                                                            } else {
                                                                "-rw-r--r--".to_string()
                                                            };
                                                            let modified = metadata.modified()
                                                                .ok()
                                                                .and_then(|t| {
                                                                    t.duration_since(std::time::UNIX_EPOCH)
                                                                        .ok()
                                                                        .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0))
                                                                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                                                })
                                                                .unwrap_or("Unknown".to_string());

                                                            file_entries.push(FileEntry {
                                                                name: file_name,
                                                                size,
                                                                is_directory: is_dir,
                                                                permissions: perms,
                                                                modified,
                                                            });
                                                        }

                                                        current_path = validated_path;
                                                        file_browser = FileBrowser::new(
                                                            current_path.to_string_lossy().to_string(),
                                                            file_entries,
                                                        );

                                                        // Re-render browser
                                                        let browser_image = file_browser
                                                            .render_to_png(1920, 1080)
                                                            .map_err(|e| HandlerError::ProtocolError(format!("Render failed: {}", e)))?;

                                                        // Base64 encode and send via modern zero-allocation protocol
                                                        let base64_data = base64::Engine::encode(
                                                            &base64::engine::general_purpose::STANDARD,
                                                            &browser_image,
                                                        );

                                                        let img_instr = protocol_encoder.format_img_instruction(stream_id, 0, 0, 0, "image/png");
                                                        to_client
                                                            .send(img_instr.freeze())
                                                            .await
                                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                                        let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                                        for instr in blob_instructions {
                                                            to_client
                                                                .send(Bytes::from(instr))
                                                                .await
                                                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                                        }
                                                        stream_id += 1;
                                                    }
                                                    Err(e) => {
                                                        warn!("SFTP: Failed to read directory {}: {}", validated_path.display(), e);
                                                    }
                                                }
                                            } else if self.config.allow_downloads && security.is_download_allowed() {
                                                // Download file
                                                let file_path = current_path.join(&entry.name);
                                                let validated_path = self.validate_path(&file_path, &home)?;

                                                // Check file size limit
                                                if entry.size > self.config.max_file_size {
                                                    warn!("SFTP: File {} exceeds size limit ({} > {})",
                                                        entry.name, entry.size, self.config.max_file_size);
                                                    continue;
                                                }

                                                // Open file for reading
                                                // Use convenience method read() which reads entire file
                                                match sftp.read(validated_path.to_string_lossy().to_string()).await {
                                                    Ok(file_data) => {
                                                        // Send file via Guacamole file protocol
                                                        // Format: file,<stream>,<mimetype>,<filename>,<data>;
                                                        let mimetype = "application/octet-stream";
                                                        let filename = entry.name.clone();
                                                        let base64_data = base64::engine::general_purpose::STANDARD.encode(&file_data);

                                                        let file_instr = format!(
                                                            "4.file,{}.{},{}.{},{}.{},{}.{};",
                                                            stream_id.to_string().len(),
                                                            stream_id,
                                                            mimetype.len(),
                                                            mimetype,
                                                            filename.len(),
                                                            filename,
                                                            base64_data.len(),
                                                            base64_data
                                                        );

                                                        to_client.send(Bytes::from(file_instr)).await
                                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                                        info!("SFTP: Sent file {} ({} bytes)", filename, file_data.len());
                                                        stream_id += 1;
                                                    }
                                                    Err(e) => {
                                                        warn!("SFTP: Failed to read file {}: {}", validated_path.display(), e);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Handle file upload
                    else if msg_str.contains(".file,") && self.config.allow_uploads && security.is_upload_allowed() {
                        // Parse file instruction: file,<stream>,<mimetype>,<filename>,<data>;
                        if let Some(file_part) = msg_str.split_once(".file,") {
                            let parts: Vec<&str> = file_part.1.split(',').collect();
                            if parts.len() >= 4 {
                                let filename = parts[2];
                                let base64_data = parts[3].trim_end_matches(';');

                                match base64::engine::general_purpose::STANDARD.decode(base64_data) {
                                    Ok(file_data) => {
                                        if file_data.len() as u64 > self.config.max_file_size {
                                            warn!("SFTP: Upload rejected - file too large: {} bytes", file_data.len());
                                            continue;
                                        }

                                        let upload_path = current_path.join(filename);
                                        let validated_path = self.validate_path(&upload_path, &home)?;

                                        // Write file data using convenience method
                                        match sftp.write(validated_path.to_string_lossy().to_string(), &file_data).await {
                                            Ok(_) => {
                                                info!("SFTP: Uploaded file {} ({} bytes)", filename, file_data.len());

                                                // Refresh directory listing
                                                let mut file_entries = vec![
                                                    FileEntry {
                                                        name: ".".to_string(),
                                                        size: 0,
                                                        is_directory: true,
                                                        permissions: "drwxr-xr-x".to_string(),
                                                        modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                                                    },
                                                    FileEntry {
                                                        name: "..".to_string(),
                                                        size: 0,
                                                        is_directory: true,
                                                        permissions: "drwxr-xr-x".to_string(),
                                                        modified: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                                                    },
                                                ];

                                                // Re-read directory to show new file
                                                if let Ok(read_dir) = sftp.read_dir(current_path.to_string_lossy().to_string()).await {
                                                    // ReadDir implements Iterator (synchronous)
                                                    for dir_entry in read_dir {
                                                        let metadata = dir_entry.metadata();
                                                        let file_name = dir_entry.file_name().to_string();

                                                        if file_name.starts_with('.') && file_name != "." && file_name != ".." {
                                                            continue;
                                                        }

                                                        let is_dir = metadata.is_dir();
                                                        let size = metadata.len();
                                                        let perms = if is_dir {
                                                            "drwxr-xr-x".to_string()
                                                        } else {
                                                            "-rw-r--r--".to_string()
                                                        };
                                                        let modified = metadata.modified()
                                                            .ok()
                                                            .and_then(|t| {
                                                                t.duration_since(std::time::UNIX_EPOCH)
                                                                    .ok()
                                                                    .and_then(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0))
                                                                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                                            })
                                                            .unwrap_or("Unknown".to_string());

                                                        file_entries.push(FileEntry {
                                                            name: file_name,
                                                            size,
                                                            is_directory: is_dir,
                                                            permissions: perms,
                                                            modified,
                                                        });
                                                    }
                                                }

                                                        file_browser = FileBrowser::new(
                                                            current_path.to_string_lossy().to_string(),
                                                            file_entries,
                                                        );

                                                        // Re-render browser
                                                        let browser_image = file_browser
                                                            .render_to_png(1920, 1080)
                                                            .map_err(|e| HandlerError::ProtocolError(format!("Render failed: {}", e)))?;

                                                        // Base64 encode and send via modern zero-allocation protocol
                                                        let base64_data = base64::Engine::encode(
                                                            &base64::engine::general_purpose::STANDARD,
                                                            &browser_image,
                                                        );

                                                        let img_instr = protocol_encoder.format_img_instruction(stream_id, 0, 0, 0, "image/png");
                                                        to_client
                                                            .send(img_instr.freeze())
                                                            .await
                                                            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

                                                        let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
                                                        for instr in blob_instructions {
                                                            to_client
                                                                .send(Bytes::from(instr))
                                                                .await
                                                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                                                        }
                                                        stream_id += 1;
                                                    }
                                                    Err(e) => {
                                                        warn!("SFTP: Failed to write file {}: {}", validated_path.display(), e);
                                                    }
                                                }
                                    }
                                    Err(e) => {
                                        warn!("SFTP: Failed to decode file data: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    // Handle key presses (for navigation, etc.)
                    else if msg_str.contains(".key,") {
                        // TODO: Handle keyboard shortcuts (e.g., Backspace for up directory)
                        debug!("SFTP: Key press received");
                    }
                }
            }
        }

        info!("SFTP handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// SFTP client handler with host key verification support
struct SftpClientHandler {
    verifier: HostKeyVerifier,
    hostname: String,
    port: u16,
}

impl SftpClientHandler {
    fn new(hostname: String, port: u16, config: HostKeyConfig) -> Self {
        Self {
            verifier: HostKeyVerifier::new(config),
            hostname,
            port,
        }
    }
}

#[async_trait]
impl client::Handler for SftpClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Get key type and raw bytes from the public key
        let key_type = server_public_key.name();
        let key_bytes = server_public_key.public_key_bytes();

        // Verify the host key
        let result = self
            .verifier
            .verify(&self.hostname, self.port, key_type, &key_bytes);

        match &result {
            HostKeyResult::Verified => {
                info!(
                    "SFTP: Host key verified for {}:{}",
                    self.hostname, self.port
                );
            }
            HostKeyResult::Skipped => {
                warn!(
                    "SFTP: Host key verification skipped for {}:{} (INSECURE)",
                    self.hostname, self.port
                );
            }
            HostKeyResult::NotConfigured => {
                debug!(
                    "SFTP: No host key verification configured for {}:{}",
                    self.hostname, self.port
                );
            }
            HostKeyResult::UnknownHost => {
                warn!(
                    "SFTP: Unknown host {}:{} - not in known_hosts",
                    self.hostname, self.port
                );
            }
            HostKeyResult::Mismatch { expected, actual } => {
                error!(
                    "SFTP: HOST KEY MISMATCH for {}:{}\nExpected: {}\nActual: {}",
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
                error!("SFTP: {}", msg);
            }
            Ok(false) // Reject the connection
        }
    }
}

// Event-based handler implementation for zero-copy integration
#[async_trait]
impl EventBasedHandler for SftpHandler {
    fn name(&self) -> &str {
        "sftp"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sftp_handler_new() {
        let handler = SftpHandler::with_defaults();
        assert_eq!(
            <_ as guacr_handlers::ProtocolHandler>::name(&handler),
            "sftp"
        );
    }

    #[test]
    fn test_sftp_security_defaults() {
        let config = SftpConfig::default();
        assert!(config.chroot_to_home); // Security
        assert!(!config.allow_delete); // Security: No delete by default
    }

    #[test]
    fn test_sftp_port() {
        let config = SftpConfig::default();
        assert_eq!(config.default_port, 22); // Same as SSH
    }
}
