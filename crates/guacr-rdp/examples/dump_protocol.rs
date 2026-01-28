//! Dump raw Guacamole protocol data to see the actual format
//!
//! Usage: cargo run --release --package guacr-rdp --example dump_protocol

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::RdpHandler;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RDP Protocol Dump ===\n");

    let handler = RdpHandler::with_defaults();
    let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
    let (_from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

    let mut params = HashMap::new();
    params.insert("hostname".to_string(), "127.0.0.1".to_string());
    params.insert("port".to_string(), "3389".to_string());
    params.insert("username".to_string(), "linuxuser".to_string());
    params.insert("password".to_string(), "alpine".to_string());
    params.insert("width".to_string(), "1024".to_string());
    params.insert("height".to_string(), "768".to_string());
    params.insert("security".to_string(), "any".to_string());
    params.insert("ignore-cert".to_string(), "true".to_string());

    tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    let mut msg_count = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline && msg_count < 10 {
        match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                msg_count += 1;
                let msg_str = String::from_utf8_lossy(&msg);

                println!("=== Message {} ({} bytes) ===", msg_count, msg.len());

                // Show first 200 chars
                let preview = if msg_str.len() > 200 {
                    format!("{}...", &msg_str[..200])
                } else {
                    msg_str.to_string()
                };
                println!("{}", preview);

                // If it's a blob, save it
                if msg_str.contains("blob") {
                    let path = format!("/tmp/rdp_msg_{}.txt", msg_count);
                    std::fs::write(&path, &msg)?;
                    println!("→ Saved full message to: {}", path);
                }
                println!();
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    println!("Total messages: {}", msg_count);
    Ok(())
}
