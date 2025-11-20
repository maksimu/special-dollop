// guacr-sftp: SFTP file transfer protocol handler
//
// Provides secure file transfer over SSH with graphical file browser

mod channel_adapter;
mod file_browser;
mod handler;

pub use channel_adapter::ChannelStreamAdapter;
pub use file_browser::FileBrowser;
pub use handler::{SftpConfig, SftpHandler};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SftpError {
    #[error("SFTP connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("File operation failed: {0}")]
    FileOperationFailed(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SftpError>;
