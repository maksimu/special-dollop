//! RDP screenshot test with terminal graphics renderer
//!
//! This connects to an RDP server and renders using Sixel graphics
//! instead of PNG encoding.
//!
//! Usage:
//!   cargo run --package guacr-rdp --example screenshot_terminal
//!
//! Requirements:
//!   - RDP server running on localhost:3389
//!   - Terminal with Sixel support (xterm, mlterm, wezterm, iTerm2, foot, etc.)

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::{RdpConfig, RdpHandler};
use guacr_terminal::GraphicsMode;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RDP Terminal Graphics Test ===\n");

    // Create handler with terminal mode enabled
    let config = RdpConfig {
        terminal_mode: Some(GraphicsMode::Sixel), // Use Sixel graphics
        terminal_cols: 240,
        terminal_rows: 60,
        ..Default::default()
    };
    let handler = RdpHandler::new(config);

    let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
    let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

    let mut params = HashMap::new();
    params.insert("hostname".to_string(), "127.0.0.1".to_string());
    params.insert("port".to_string(), "3389".to_string());
    params.insert("username".to_string(), "linuxuser".to_string());
    params.insert("password".to_string(), "alpine".to_string());
    params.insert("width".to_string(), "1024".to_string());
    params.insert("height".to_string(), "768".to_string());
    params.insert("security".to_string(), "rdp".to_string());
    params.insert("ignore-cert".to_string(), "true".to_string());

    println!("Connecting to RDP server...");

    // Spawn handler
    let _handle = tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Connection error: {}", e);
        }
    });

    // Keep sender alive
    let _keep_alive = from_client_tx;

    println!("Waiting for terminal graphics output...\n");

    // Receive and display terminal graphics
    let mut frame_count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("size") {
                    println!("✓ Screen size received");
                } else if msg_str.starts_with("\x1b") {
                    // Terminal escape sequence (Sixel)
                    frame_count += 1;
                    print!("{}", msg_str); // Print directly to terminal
                    std::io::Write::flush(&mut std::io::stdout()).ok();
                    println!("\n[Frame {} rendered]", frame_count);
                }
            }
            Ok(None) => {
                println!("Channel closed");
                break;
            }
            Err(_) => {
                // Timeout, continue waiting
            }
        }
    }

    println!("\n=== Test Complete ===");
    println!("Frames rendered: {}", frame_count);

    Ok(())
}
