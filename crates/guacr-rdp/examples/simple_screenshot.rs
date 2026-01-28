//! Simple screenshot test - saves raw Guacamole protocol data
//!
//! Usage: cargo run --release --package guacr-rdp --example simple_screenshot

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::RdpHandler;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RDP Screenshot Test ===");
    println!("Connecting to localhost:3389...\n");

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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("size") {
                    println!("✓ Screen size received");
                } else if msg_str.contains("img") && msg_str.contains("blob") {
                    img_count += 1;
                    println!("✓ Image #{} received ({} bytes)", img_count, msg.len());

                    // Save first image
                    if img_count == 1 {
                        let output_path = "/tmp/rdp_guacamole_data.txt";
                        std::fs::write(output_path, &msg)?;
                        println!("  → Saved raw data to: {}", output_path);

                        // Try to extract and decode PNG
                        if let Some(blob_idx) = msg_str.find("4.blob,") {
                            // Format: 4.blob,<stream>,<base64data>;
                            let after_blob = &msg_str[blob_idx + 7..];
                            if let Some(comma_idx) = after_blob.find(',') {
                                let after_comma = &after_blob[comma_idx + 1..];
                                if let Some(semicolon_idx) = after_comma.find(';') {
                                    let base64_data = &after_comma[..semicolon_idx];

                                    match base64::Engine::decode(
                                        &base64::engine::general_purpose::STANDARD,
                                        base64_data.as_bytes(),
                                    ) {
                                        Ok(png_bytes) => {
                                            let png_path = "/tmp/rdp_screenshot.png";
                                            std::fs::write(png_path, &png_bytes)?;
                                            println!(
                                                "  → Decoded PNG ({} bytes) to: {}",
                                                png_bytes.len(),
                                                png_path
                                            );

                                            // Analyze pixels
                                            if let Ok(img) = image::load_from_memory(&png_bytes) {
                                                let rgba = img.to_rgba8();
                                                let pixels = rgba.as_raw();

                                                let all_black = pixels
                                                    .chunks(4)
                                                    .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);

                                                println!("\n=== RESULT ===");
                                                println!(
                                                    "Image size: {}x{}",
                                                    img.width(),
                                                    img.height()
                                                );
                                                if all_black {
                                                    println!("Status: ❌ IMAGE IS ALL BLACK");
                                                } else {
                                                    println!("Status: ✅ IMAGE HAS CONTENT");

                                                    // Count non-black pixels
                                                    let non_black = pixels
                                                        .chunks(4)
                                                        .filter(|p| {
                                                            p[0] != 0 || p[1] != 0 || p[2] != 0
                                                        })
                                                        .count();
                                                    let total = pixels.len() / 4;
                                                    println!(
                                                        "Non-black pixels: {}/{} ({:.1}%)",
                                                        non_black,
                                                        total,
                                                        (non_black as f64 / total as f64) * 100.0
                                                    );

                                                    // Sample pixels
                                                    println!("\nSample pixels:");
                                                    for i in 0..10.min(total) {
                                                        let idx = i * 4;
                                                        println!(
                                                            "  [{:3}]: RGB({:3}, {:3}, {:3})",
                                                            i,
                                                            pixels[idx],
                                                            pixels[idx + 1],
                                                            pixels[idx + 2]
                                                        );
                                                    }
                                                }
                                                println!("==============\n");
                                                break;
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
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    println!("\nTotal images received: {}", img_count);
    Ok(())
}
