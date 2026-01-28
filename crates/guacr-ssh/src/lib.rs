// guacr-ssh: SSH protocol handler for remote desktop access via WebRTC
//
// # Example
//
// ```no_run
// use guacr_ssh::SshHandler;
// use guacr_handlers::ProtocolHandler;
// use std::collections::HashMap;
// use tokio::sync::mpsc;
//
// #[tokio::main]
// async fn main() {
//     let handler = SshHandler::with_defaults();
//
//     let mut params = HashMap::new();
//     params.insert("hostname".to_string(), "example.com".to_string());
//     params.insert("username".to_string(), "user".to_string());
//     params.insert("password".to_string(), "pass".to_string());
//
//     let (tx, rx) = mpsc::channel(10);
//     let (client_tx, client_rx) = mpsc::channel(10);
//
//     handler.connect(params, tx, client_rx).await.unwrap();
// }
// ```

mod handler;

pub use handler::SshHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SshError {
    #[error("SSH connection failed: {0}")]
    ConnectionFailed(String),

    #[error("SSH authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Terminal error: {0}")]
    TerminalError(#[from] guacr_terminal::TerminalError),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("SSH error: {0}")]
    SshError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SshError>;
