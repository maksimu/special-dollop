// guacr-telnet: Telnet protocol handler for remote desktop access via WebRTC
//
// # Example
//
// ```no_run
// use guacr_telnet::TelnetHandler;
// use guacr_handlers::ProtocolHandler;
// use std::collections::HashMap;
// use tokio::sync::mpsc;
//
// #[tokio::main]
// async fn main() {
//     let handler = TelnetHandler::with_defaults();
//
//     let mut params = HashMap::new();
//     params.insert("hostname".to_string(), "example.com".to_string());
//
//     let (tx, rx) = mpsc::channel(10);
//     let (client_tx, client_rx) = mpsc::channel(10);
//
//     handler.connect(params, tx, client_rx).await.unwrap();
// }
// ```

mod handler;

pub use handler::TelnetHandler;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TelnetError {
    #[error("Telnet connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Terminal error: {0}")]
    TerminalError(#[from] guacr_terminal::TerminalError),

    #[error("Handler error: {0}")]
    HandlerError(#[from] guacr_handlers::HandlerError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TelnetError>;
