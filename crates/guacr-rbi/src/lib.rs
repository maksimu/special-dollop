// guacr-rbi: Remote Browser Isolation handler
//
// Provides isolated browser sessions via headless Chrome/Chromium

mod handler;

pub use handler::RbiHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RbiError {
    #[error("Browser launch failed: {0}")]
    BrowserLaunchFailed(String),

    #[error("Navigation failed: {0}")]
    NavigationFailed(String),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, RbiError>;
