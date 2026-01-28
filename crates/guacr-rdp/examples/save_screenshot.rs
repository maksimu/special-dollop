//! Save actual RDP screenshot to file for visual inspection
//!
//! Usage: cargo run --release --package guacr-rdp --example save_screenshot

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

    println!("=== RDP Screenshot Capture ===");
    println!("Connecting to localhost:3389...\n");

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
    tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    println!("Waiting for images...");

    let mut img_count = 0;
    let mut saved_count = 0;
    let max_images = 3; // Save 3 images

    // Wait for connection ready
    println!("Waiting for connection...");
    let mut ready = false;
    while !ready {
        match timeout(Duration::from_secs(5), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);
                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                    ready = true;
                }
            }
            _ => break,
        }
    }

    // Wait 10 seconds for desktop to fully load and render
    println!("Waiting 10 seconds for desktop to initialize and render...");
    tokio::time::sleep(Duration::from_secs(10)).await;
    println!("Capturing screenshots...");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline && saved_count < max_images {
        match timeout(Duration::from_secs(3), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("size") {
                    println!("✓ Screen size received");
                } else if msg_str.contains("img") {
                    img_count += 1;

                    // Look for blob in the NEXT message
                    if let Ok(Some(blob_msg)) =
                        timeout(Duration::from_millis(500), to_client_rx.recv()).await
                    {
                        let blob_str = String::from_utf8_lossy(&blob_msg);

                        if blob_str.starts_with("4.blob,") {
                            // Format: 4.blob,<stream>,<length>.<base64data>;
                            let parts: Vec<&str> = blob_str.splitn(3, ',').collect();
                            if parts.len() >= 3 {
                                let data_part = parts[2];
                                if let Some(semicolon_idx) = data_part.find(';') {
                                    let length_and_data = &data_part[..semicolon_idx];
                                    // Skip the "length." prefix
                                    let base64_data =
                                        if let Some(dot_idx) = length_and_data.find('.') {
                                            &length_and_data[dot_idx + 1..]
                                        } else {
                                            length_and_data
                                        };

                                    match base64::Engine::decode(
                                        &base64::engine::general_purpose::STANDARD,
                                        base64_data.as_bytes(),
                                    ) {
                                        Ok(png_bytes) => {
                                            saved_count += 1;
                                            let png_path =
                                                format!("/tmp/rdp_screenshot_{}.png", saved_count);
                                            std::fs::write(&png_path, &png_bytes)?;

                                            println!(
                                                "✓ Image #{} saved ({} bytes) → {}",
                                                img_count,
                                                png_bytes.len(),
                                                png_path
                                            );

                                            // Quick analysis
                                            if let Ok(img) = image::load_from_memory(&png_bytes) {
                                                let rgba = img.to_rgba8();
                                                let pixels = rgba.as_raw();

                                                let all_black = pixels
                                                    .chunks(4)
                                                    .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
                                                let non_black = pixels
                                                    .chunks(4)
                                                    .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
                                                    .count();
                                                let total = pixels.len() / 4;

                                                if all_black {
                                                    println!("  ⚠️  All pixels are black!");
                                                } else {
                                                    println!(
                                                        "  ✓ {}x{} - {:.1}% non-black pixels",
                                                        img.width(),
                                                        img.height(),
                                                        (non_black as f64 / total as f64) * 100.0
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            println!("  ❌ Failed to decode base64: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(None) => {
                println!("Channel closed");
                break;
            }
            Err(_) => {
                // Timeout, continue
            }
        }
    }

    println!("\n=== Summary ===");
    println!("Total images received: {}", img_count);
    println!("Images saved: {}", saved_count);

    if saved_count > 0 {
        println!("\nScreenshots saved to:");
        for i in 1..=saved_count {
            println!("  /tmp/rdp_screenshot_{}.png", i);
        }
        println!("\nView with:");
        println!("  open /tmp/rdp_screenshot_1.png");
        println!("  # or");
        println!("  qlmanage -p /tmp/rdp_screenshot_1.png");
    }

    // Keep connection alive a bit longer to ensure images are fully received
    println!("\nWaiting 2 more seconds for final images...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Keep from_client_tx alive until the end
    drop(from_client_tx);

    Ok(())
}
