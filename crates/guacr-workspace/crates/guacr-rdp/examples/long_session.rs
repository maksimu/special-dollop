//! Long RDP session to allow desktop to start
//!
//! Usage: cargo run --release --package guacr-rdp --example long_session

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::RdpHandler;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Long RDP Session Test ===");
    println!("This will maintain connection for 30 seconds to allow desktop to start\n");

    let handler = RdpHandler::with_defaults();
    let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
    let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

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
    let handle = tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    println!("Connecting...");

    let mut img_count = 0;
    let mut last_save_time = std::time::Instant::now();

    // Run for 30 seconds
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("img") {
                    img_count += 1;

                    // Save a screenshot every 5 seconds
                    if last_save_time.elapsed() > Duration::from_secs(5) {
                        println!(
                            "  Image #{} received ({}s elapsed)",
                            img_count,
                            deadline.elapsed().as_secs()
                        );
                        last_save_time = std::time::Instant::now();
                    }
                }
            }
            Ok(None) => {
                println!("Connection closed");
                break;
            }
            Err(_) => {
                // Timeout, send a sync to keep connection alive
                let sync_msg = format!(
                    "4.sync,{};",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                );
                let _ = from_client_tx.send(Bytes::from(sync_msg)).await;
            }
        }
    }

    println!("\n=== Session Complete ===");
    println!("Total images: {}", img_count);
    println!("Duration: 30 seconds");

    // Close connection
    drop(from_client_tx);
    let _ = timeout(Duration::from_secs(2), handle).await;

    Ok(())
}
