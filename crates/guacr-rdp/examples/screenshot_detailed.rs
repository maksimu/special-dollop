//! Detailed RDP screenshot test with pixel analysis
//!
//! This test captures multiple frames and analyzes color distribution
//! to verify that the RDP connection is rendering correctly.
//!
//! Usage:
//!   cargo run --package guacr-rdp --example screenshot_detailed
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
    println!("=== Detailed RDP Screenshot Test ===");
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

    // Spawn handler
    let handler_task = tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client_tx, from_client_rx).await {
            eprintln!("Handler error: {}", e);
        }
    });

    let mut img_count = 0;
    let mut blob_data_map: HashMap<String, Vec<u8>> = HashMap::new();
    let mut all_images: Vec<Vec<u8>> = Vec::new();

    // Collect messages for 15 seconds to get multiple frames
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    while tokio::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
            Ok(Some(msg)) => {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains("ready") {
                    println!("✓ Connection ready");
                } else if msg_str.contains("size") {
                    println!("✓ Size negotiated");
                } else if msg_str.contains("img,") {
                    img_count += 1;

                    // Parse stream ID
                    if let Some(img_start) = msg_str.find("img,") {
                        let after_img = &msg_str[img_start + 4..];
                        if let Some(comma_pos) = after_img.find(',') {
                            let stream_part = &after_img[..comma_pos];
                            if let Some(dot_pos) = stream_part.find('.') {
                                let stream_id = &stream_part[dot_pos + 1..];
                                if img_count % 5 == 0 {
                                    println!("  Frame {} (stream {})", img_count, stream_id);
                                }
                            }
                        }
                    }
                } else if msg_str.contains("blob,") {
                    // Parse and decode blob
                    if let Some(blob_start) = msg_str.find("blob,") {
                        let after_blob = &msg_str[blob_start + 5..];

                        if let Some(comma_pos) = after_blob.find(',') {
                            let stream_part = &after_blob[..comma_pos];
                            if let Some(dot_pos) = stream_part.find('.') {
                                let stream_id = &stream_part[dot_pos + 1..];

                                let after_comma = &after_blob[comma_pos + 1..];
                                if let Some(dot_pos) = after_comma.find('.') {
                                    let after_dot = &after_comma[dot_pos + 1..];
                                    if let Some(semicolon) = after_dot.find(';') {
                                        let base64_data = &after_dot[..semicolon];

                                        use base64::Engine;
                                        if let Ok(png_bytes) =
                                            base64::engine::general_purpose::STANDARD
                                                .decode(base64_data.trim())
                                        {
                                            blob_data_map
                                                .insert(stream_id.to_string(), png_bytes.clone());
                                            all_images.push(png_bytes);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }

    handler_task.abort();

    println!("\n=== Summary ===");
    println!("Total frames: {}", img_count);
    println!("Decoded images: {}", all_images.len());

    if all_images.is_empty() {
        println!("\n❌ No images captured!");
        return Ok(());
    }

    // Analyze first and last images
    println!("\n=== First Frame Analysis ===");
    analyze_image(&all_images[0], "/tmp/rdp_first.png")?;

    if all_images.len() > 1 {
        println!("\n=== Last Frame Analysis ===");
        analyze_image(&all_images[all_images.len() - 1], "/tmp/rdp_last.png")?;
    }

    // Find the image with most color variety
    let mut max_colors = 0;
    let mut best_idx = 0;
    for (idx, png_bytes) in all_images.iter().enumerate() {
        if let Ok(img) = image::load_from_memory(png_bytes) {
            let rgba = img.to_rgba8();
            let pixels = rgba.as_raw();
            let mut colors = std::collections::HashSet::new();
            for chunk in pixels.chunks(4).take(1000) {
                colors.insert((chunk[0], chunk[1], chunk[2]));
            }
            if colors.len() > max_colors {
                max_colors = colors.len();
                best_idx = idx;
            }
        }
    }

    if max_colors > 2 {
        println!("\n=== Most Colorful Frame (#{}) ===", best_idx + 1);
        analyze_image(&all_images[best_idx], "/tmp/rdp_best.png")?;
    }

    Ok(())
}

fn analyze_image(png_bytes: &[u8], output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Check PNG signature
    if png_bytes.len() < 8 {
        println!("❌ Image too small");
        return Ok(());
    }

    let signature = &png_bytes[0..8];
    let expected = [137, 80, 78, 71, 13, 10, 26, 10];
    if signature != expected {
        println!("❌ Invalid PNG signature");
        return Ok(());
    }

    // Save to file
    std::fs::write(output_path, png_bytes)?;
    println!("✓ Saved to: {}", output_path);

    // Analyze pixels
    if let Ok(img) = image::load_from_memory(png_bytes) {
        let rgba = img.to_rgba8();
        let pixels = rgba.as_raw();

        println!("  Dimensions: {}x{}", img.width(), img.height());
        println!("  Size: {} KB", png_bytes.len() / 1024);

        // Count unique colors
        let mut colors = std::collections::HashSet::new();
        for chunk in pixels.chunks(4) {
            colors.insert((chunk[0], chunk[1], chunk[2]));
        }
        println!("  Unique colors: {}", colors.len());

        // Calculate color distribution
        let mut black_count = 0;
        let mut white_count = 0;
        let mut gray_count = 0;
        let mut color_count = 0;

        for chunk in pixels.chunks(4) {
            let (r, g, b) = (chunk[0], chunk[1], chunk[2]);
            if r == 0 && g == 0 && b == 0 {
                black_count += 1;
            } else if r == 255 && g == 255 && b == 255 {
                white_count += 1;
            } else if r == g && g == b {
                gray_count += 1;
            } else {
                color_count += 1;
            }
        }

        let total = pixels.len() / 4;
        println!("  Color distribution:");
        println!(
            "    Black: {:.1}%",
            (black_count as f64 / total as f64) * 100.0
        );
        println!(
            "    White: {:.1}%",
            (white_count as f64 / total as f64) * 100.0
        );
        println!(
            "    Gray: {:.1}%",
            (gray_count as f64 / total as f64) * 100.0
        );
        println!(
            "    Color: {:.1}%",
            (color_count as f64 / total as f64) * 100.0
        );

        // Sample some non-black pixels
        let mut sample_colors: Vec<(u8, u8, u8)> = Vec::new();
        for chunk in pixels.chunks(4) {
            let (r, g, b) = (chunk[0], chunk[1], chunk[2]);
            if (r != 0 || g != 0 || b != 0) && sample_colors.len() < 10 {
                sample_colors.push((r, g, b));
            }
        }

        if !sample_colors.is_empty() {
            println!("  Sample non-black colors:");
            for (_i, (r, g, b)) in sample_colors.iter().enumerate().take(5) {
                println!("    RGB({}, {}, {})", r, g, b);
            }
        }
    }

    Ok(())
}
