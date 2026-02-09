//! Test JPEG encoding in RDP handler

use guacr_rdp::RdpConfig;

#[test]
fn test_rdp_config_jpeg_defaults() {
    let config = RdpConfig::default();

    // JPEG should be enabled by default
    assert!(config.use_jpeg, "JPEG should be enabled by default");
    assert_eq!(
        config.jpeg_quality, 92,
        "JPEG quality should be 92 by default"
    );

    println!("✅ RDP JPEG defaults:");
    println!("   use_jpeg: {}", config.use_jpeg);
    println!(
        "   jpeg_quality: {} (high quality, minimal artifacts)",
        config.jpeg_quality
    );
}

#[test]
fn test_jpeg_encoding_function() {
    // Create a simple 100x100 test image
    let width = 100u32;
    let height = 100u32;

    // Create test pattern: gradient
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            pixels[i * 4] = (x * 255 / width) as u8; // R
            pixels[i * 4 + 1] = (y * 255 / height) as u8; // G
            pixels[i * 4 + 2] = 128; // B
            pixels[i * 4 + 3] = 255; // A
        }
    }

    // Test JPEG encoding at different quality levels
    for quality in [50, 70, 85, 95] {
        let img = image::RgbaImage::from_raw(width, height, pixels.clone())
            .expect("Failed to create image");
        let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

        let mut jpeg_data = Vec::new();
        use image::ImageEncoder;
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder
            .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
            .expect("JPEG encoding failed");

        // Verify JPEG was created
        assert!(!jpeg_data.is_empty(), "JPEG data should not be empty");
        assert_eq!(jpeg_data[0], 0xFF, "JPEG should start with 0xFF");
        assert_eq!(jpeg_data[1], 0xD8, "JPEG should have 0xD8 marker");

        println!("✅ JPEG quality {}: {} bytes", quality, jpeg_data.len());
    }
}

#[test]
fn test_jpeg_bandwidth_savings() {
    let width = 200u32;
    let height = 200u32;

    // Create gradient test pattern
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            pixels[i * 4] = (x * 255 / width) as u8;
            pixels[i * 4 + 1] = (y * 255 / height) as u8;
            pixels[i * 4 + 2] = 128;
            pixels[i * 4 + 3] = 255;
        }
    }

    // Encode as JPEG (quality 85 for comparison)
    let img =
        image::RgbaImage::from_raw(width, height, pixels.clone()).expect("Failed to create image");
    let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

    let mut jpeg_data = Vec::new();
    use image::ImageEncoder;
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, 85);
    encoder
        .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
        .expect("JPEG encoding failed");

    // Encode as PNG using framebuffer
    use guacr_terminal::FrameBuffer;
    let mut fb = FrameBuffer::new(width, height);
    fb.update_region(0, 0, width, height, &pixels);

    let rect = guacr_terminal::FrameRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let png_data = fb.encode_region(rect).expect("PNG encoding failed");

    // Calculate bandwidth savings
    let savings = png_data.len() as f32 / jpeg_data.len() as f32;

    println!("✅ RDP bandwidth savings:");
    println!("   PNG:  {} bytes", png_data.len());
    println!("   JPEG: {} bytes (quality 85)", jpeg_data.len());
    println!("   Savings: {:.1}x smaller with JPEG", savings);

    // JPEG should be significantly smaller
    assert!(
        jpeg_data.len() < png_data.len() / 2,
        "JPEG should be at least 2x smaller than PNG"
    );
}
