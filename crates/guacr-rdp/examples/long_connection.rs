//! Long-running RDP connection to capture desktop after it fully loads
//!
//! Usage: RUST_LOG=guacr_rdp=debug cargo run --release --package guacr-rdp --example long_connection

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::RdpHandler;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    println!("=== Long RDP Connection Test ===");
    println!("Connecting to localhost:3389...");
    println!("Will run for 30 seconds to allow desktop to fully load\n");

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

    // Spawn handler
    tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    let mut img_count = 0;
    let mut blob_count = 0;
    let mut last_report = std::time::Instant::now();

    // Run for 30 seconds
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    println!("Monitoring connection...\n");

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("size") {
                    println!("✓ Screen size received");
                } else if msg_str.contains("img") {
                    img_count += 1;
                } else if msg_str.contains("blob") {
                    blob_count += 1;
                }

                // Report every 5 seconds
                if last_report.elapsed() > Duration::from_secs(5) {
                    let elapsed = (tokio::time::Instant::now()
                        - (deadline - Duration::from_secs(30)))
                    .as_secs();
                    println!(
                        "  [{}s] Images: {}, Blobs: {}",
                        elapsed, img_count, blob_count
                    );
                    last_report = std::time::Instant::now();
                }
            }
            Ok(None) => {
                println!("Connection closed");
                break;
            }
            Err(_) => {
                // Timeout, continue
            }
        }
    }

    println!("\n=== Summary ===");
    println!("Total images: {}", img_count);
    println!("Total blobs: {}", blob_count);
    println!("Connection duration: 30 seconds");

    Ok(())
}
