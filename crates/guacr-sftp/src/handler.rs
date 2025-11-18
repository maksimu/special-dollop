use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::file_browser::{FileBrowser, FileEntry};

/// SFTP handler - secure file transfer over SSH
///
/// Provides graphical file browser interface over SFTP
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
}

impl Default for SftpConfig {
    fn default() -> Self {
        Self {
            default_port: 22,
            chroot_to_home: true, // Security: Restrict access
            allow_uploads: true,
            allow_downloads: true,
            allow_delete: false, // Security: Readonly by default for deletes
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
}

#[async_trait]
impl ProtocolHandler for SftpHandler {
    fn name(&self) -> &str {
        "sftp"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        _to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("SFTP handler starting");

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

        info!("SFTP connecting to {}@{}:{}", username, hostname, port);

        // TODO: Full SFTP implementation
        // Security features:
        // - Reuse SSH authentication (same security as SSH)
        // - chroot to home directory (prevent path traversal)
        // - Restrict operations (no delete by default)
        // - Audit all file transfers
        // - Virus scanning on downloads (optional)
        //
        // use russh::client::connect;
        // use russh_sftp::client::SftpSession;
        //
        // let ssh_session = connect(...).await?;
        // ssh_session.authenticate_password(username, password).await?;
        //
        // let channel = ssh_session.channel_open_session().await?;
        // channel.request_subsystem(true, "sftp").await?;
        // let sftp = SftpSession::new(channel).await?;
        //
        // Security: chroot if configured
        // if self.config.chroot_to_home {
        //     let home = sftp.realpath(".").await?;
        //     // Validate all paths stay within home
        // }
        //
        // List directory
        // let entries = sftp.read_dir(path).await?;
        // let file_list: Vec<FileEntry> = entries.map(|e| FileEntry {
        //     name: e.filename,
        //     size: e.size,
        //     is_directory: e.is_dir(),
        //     ...
        // }).collect();
        //
        // Render file browser
        // let browser = FileBrowser::new(current_path, file_list);
        // let png = browser.render_to_png(1920, 1080)?;
        // send_image(png).await?;
        //
        // Handle clicks:
        // - Double-click directory: navigate
        // - Click file + download button: transfer
        // - Drag & drop: upload (if allowed)

        // Stub
        let entries = vec![FileEntry {
            name: "example.txt".to_string(),
            size: 1024,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01".to_string(),
        }];

        let browser = FileBrowser::new("/home".to_string(), entries);
        let png = browser
            .render_to_png(1920, 1080)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // TODO: Send file browser image
        drop(png);

        while from_client.recv().await.is_some() {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sftp_handler_new() {
        let handler = SftpHandler::with_defaults();
        assert_eq!(handler.name(), "sftp");
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
