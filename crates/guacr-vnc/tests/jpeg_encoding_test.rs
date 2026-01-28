//! Unit test for JPEG encoding functionality
//!
//! Tests the JPEG encoder without requiring a VNC server

use guacr_terminal::FrameBuffer;
use image::ImageEncoder;

#[test]
fn test_jpeg_encoding_basic() {
    // Create a simple 100x100 test image
    let width = 100u32;
    let height = 100u32;

    // Create test pattern: red square
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for i in 0..(width * height) as usize {
        pixels[i * 4] = 255; // R
        pixels[i * 4 + 1] = 0; // G
        pixels[i * 4 + 2] = 0; // B
        pixels[i * 4 + 3] = 255; // A
    }

    // Encode as JPEG using image crate (same as VNC handler)
    let img =
        image::RgbaImage::from_raw(width, height, pixels.clone()).expect("Failed to create image");

    let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

    let mut jpeg_data = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, 85);
    encoder
        .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
        .expect("JPEG encoding failed");

    // Verify JPEG was created
    assert!(!jpeg_data.is_empty(), "JPEG data should not be empty");

    // Verify JPEG header (FF D8 FF)
    assert_eq!(jpeg_data[0], 0xFF, "JPEG should start with 0xFF");
    assert_eq!(jpeg_data[1], 0xD8, "JPEG should have 0xD8 marker");
    assert_eq!(jpeg_data[2], 0xFF, "JPEG should have 0xFF marker");

    println!("✅ JPEG encoding works!");
    println!(
        "   Input: {}x{} RGBA = {} bytes",
        width,
        height,
        pixels.len()
    );
    println!("   Output: JPEG = {} bytes", jpeg_data.len());
    println!(
        "   Compression: {:.1}x smaller",
        pixels.len() as f32 / jpeg_data.len() as f32
    );
}

#[test]
fn test_jpeg_vs_png_size() {
    // Create a 200x200 test image with gradient
    let width = 200u32;
    let height = 200u32;

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            pixels[i * 4] = (x * 255 / width) as u8; // R gradient
            pixels[i * 4 + 1] = (y * 255 / height) as u8; // G gradient
            pixels[i * 4 + 2] = 128; // B constant
            pixels[i * 4 + 3] = 255; // A opaque
        }
    }

    // Encode as JPEG
    let img =
        image::RgbaImage::from_raw(width, height, pixels.clone()).expect("Failed to create image");
    let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

    let mut jpeg_data = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, 85);
    encoder
        .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
        .expect("JPEG encoding failed");

    // Encode as PNG using framebuffer
    let mut fb = FrameBuffer::new(width, height);
    fb.update_region(0, 0, width, height, &pixels);

    let rect = guacr_terminal::FrameRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let png_data = fb.encode_region(rect).expect("PNG encoding failed");

    // Compare sizes
    println!("✅ JPEG vs PNG comparison:");
    println!("   JPEG: {} bytes", jpeg_data.len());
    println!("   PNG:  {} bytes", png_data.len());
    println!(
        "   JPEG is {:.1}x smaller",
        png_data.len() as f32 / jpeg_data.len() as f32
    );

    // JPEG should be significantly smaller (at least 2x)
    assert!(
        jpeg_data.len() < png_data.len() / 2,
        "JPEG should be at least 2x smaller than PNG"
    );
}

#[test]
fn test_jpeg_quality_levels() {
    let width = 100u32;
    let height = 100u32;

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for i in 0..(width * height) as usize {
        pixels[i * 4] = ((i * 7) % 256) as u8; // R
        pixels[i * 4 + 1] = ((i * 13) % 256) as u8; // G
        pixels[i * 4 + 2] = ((i * 19) % 256) as u8; // B
        pixels[i * 4 + 3] = 255; // A
    }

    println!("✅ JPEG quality comparison:");

    for quality in [50, 70, 85, 95] {
        let img = image::RgbaImage::from_raw(width, height, pixels.clone())
            .expect("Failed to create image");
        let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

        let mut jpeg_data = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder
            .write_image(&rgb_img, width, height, image::ExtendedColorType::Rgb8)
            .expect("JPEG encoding failed");

        println!("   Quality {}: {} bytes", quality, jpeg_data.len());
    }
}
