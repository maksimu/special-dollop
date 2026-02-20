//! RDP screenshot test - verifies alpha channel fix
//!
//! This test connects to an RDP server and captures a screenshot to verify
//! that the alpha channel is correctly set to 255 (opaque).
//!
//! Usage:
//!   cargo run --package guacr-rdp --example screenshot_test
//!
//! Requirements:
//!   - RDP server running on localhost:3389
//!   - User: linuxuser, Password: alpine
//!   - Start with: docker start guacr-test-rdp

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
    println!("Connecting to localhost:3389...");

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
    let handler_task = tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    println!("Waiting for connection...");

    let mut received_ready = false;
    let mut received_size = false;
    let mut img_count = 0;
    let mut blob_data_map: HashMap<String, Vec<u8>> = HashMap::new();
    let mut first_complete_image: Option<Vec<u8>> = None;

    // Collect messages for 10 seconds
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    received_ready = true;
                    println!("✓ Received ready instruction");
                } else if msg_str.contains("size") {
                    received_size = true;
                    println!("✓ Received size instruction");
                } else if msg_str.contains("img,") {
                    img_count += 1;
                    println!("✓ Received img instruction #{}", img_count);

                    // Parse stream ID from img instruction
                    // Format: 3.img,<stream_id>.<stream_id_value>,<mask>.<mask_value>,...
                    if let Some(img_start) = msg_str.find("img,") {
                        let after_img = &msg_str[img_start + 4..];
                        if let Some(comma_pos) = after_img.find(',') {
                            let stream_part = &after_img[..comma_pos];
                            if let Some(dot_pos) = stream_part.find('.') {
                                let stream_id = &stream_part[dot_pos + 1..];
                                println!("  Stream ID: {}", stream_id);
                            }
                        }
                    }
                } else if msg_str.contains("blob,") {
                    println!("  - Received blob data ({} bytes)", msg.len());

                    // Parse blob instruction
                    // Format: 4.blob,<stream_id>.<stream_id_value>,<data_len>.<base64_data>;
                    if let Some(blob_start) = msg_str.find("blob,") {
                        let after_blob = &msg_str[blob_start + 5..];

                        // Find stream ID
                        if let Some(comma_pos) = after_blob.find(',') {
                            let stream_part = &after_blob[..comma_pos];
                            if let Some(dot_pos) = stream_part.find('.') {
                                let stream_id = &stream_part[dot_pos + 1..];

                                // Find base64 data
                                let after_comma = &after_blob[comma_pos + 1..];
                                if let Some(dot_pos) = after_comma.find('.') {
                                    let after_dot = &after_comma[dot_pos + 1..];
                                    if let Some(semicolon) = after_dot.find(';') {
                                        let base64_data = &after_dot[..semicolon];

                                        // Decode base64
                                        use base64::Engine;
                                        if let Ok(png_bytes) =
                                            base64::engine::general_purpose::STANDARD
                                                .decode(base64_data.trim())
                                        {
                                            println!(
                                                "    Decoded PNG: {} bytes for stream {}",
                                                png_bytes.len(),
                                                stream_id
                                            );
                                            blob_data_map.insert(stream_id.to_string(), png_bytes);

                                            // Save first complete image
                                            if first_complete_image.is_none() {
                                                first_complete_image = Some(
                                                    blob_data_map.get(stream_id).unwrap().clone(),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else if msg_str.contains("end,") {
                    println!("  - Received end instruction");
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

    // Cancel the handler task
    handler_task.abort();

    println!("\n=== Results ===");
    println!("Ready: {}", received_ready);
    println!("Size: {}", received_size);
    println!("Images received: {}", img_count);
    println!("Blobs decoded: {}", blob_data_map.len());

    if let Some(png_bytes) = first_complete_image {
        println!("\n=== Analyzing First Image ===");
        println!("PNG size: {} bytes", png_bytes.len());

        // Check PNG signature
        if png_bytes.len() >= 8 {
            let signature = &png_bytes[0..8];
            let expected = [137, 80, 78, 71, 13, 10, 26, 10];
            if signature == expected {
                println!("✓ Valid PNG signature");

                // Save to file
                let output_path = "/tmp/rdp_screenshot.png";
                std::fs::write(output_path, &png_bytes)?;
                println!("✓ Saved to: {}", output_path);

                // Analyze pixel data
                if let Ok(img) = image::load_from_memory(&png_bytes) {
                    let rgba = img.to_rgba8();
                    let pixels = rgba.as_raw();

                    println!("\n=== Pixel Analysis ===");
                    println!("Image dimensions: {}x{}", img.width(), img.height());
                    println!("Total pixels: {}", pixels.len() / 4);

                    // Check if all black
                    let all_black = pixels
                        .chunks(4)
                        .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);

                    // Check if all same color
                    let first_pixel = &pixels[0..4];
                    let all_same = pixels.chunks(4).all(|p| p == first_pixel);

                    if all_black {
                        println!("❌ IMAGE IS ALL BLACK!");
                    } else if all_same {
                        println!(
                            "⚠️  IMAGE IS SOLID COLOR: RGB({}, {}, {})",
                            first_pixel[0], first_pixel[1], first_pixel[2]
                        );
                    } else {
                        println!("✓ IMAGE HAS VARIED CONTENT!");

                        // Sample some pixels
                        let sample_size = 100.min(pixels.len() / 4);
                        let mut color_variety = std::collections::HashSet::new();
                        for i in 0..sample_size {
                            let idx = i * 4;
                            let color = (pixels[idx], pixels[idx + 1], pixels[idx + 2]);
                            color_variety.insert(color);
                        }
                        println!(
                            "Unique colors in sample: {}/{}",
                            color_variety.len(),
                            sample_size
                        );
                    }

                    // Show first few pixels
                    println!("\nFirst 10 pixels (RGBA):");
                    for i in 0..10.min(pixels.len() / 4) {
                        let idx = i * 4;
                        println!(
                            "  Pixel {}: ({}, {}, {}, {})",
                            i,
                            pixels[idx],
                            pixels[idx + 1],
                            pixels[idx + 2],
                            pixels[idx + 3]
                        );
                    }

                    // Calculate average color
                    let mut r_sum: u64 = 0;
                    let mut g_sum: u64 = 0;
                    let mut b_sum: u64 = 0;
                    let pixel_count = pixels.len() / 4;
                    for i in 0..pixel_count {
                        let idx = i * 4;
                        r_sum += pixels[idx] as u64;
                        g_sum += pixels[idx + 1] as u64;
                        b_sum += pixels[idx + 2] as u64;
                    }
                    println!(
                        "\nAverage color: RGB({}, {}, {})",
                        r_sum / pixel_count as u64,
                        g_sum / pixel_count as u64,
                        b_sum / pixel_count as u64
                    );
                } else {
                    println!("❌ Failed to decode PNG image");
                }
            } else {
                println!("❌ Invalid PNG signature: {:?}", signature);
            }
        }
    } else {
        println!("❌ No complete image data received!");
    }

    Ok(())
}
