//! Analyze screenshot pixel data in detail
//!
//! Usage: cargo run --release --package guacr-rdp --example analyze_screenshot

use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "/tmp/rdp_screenshot_1.png";
    println!("=== Analyzing Screenshot ===");
    println!("File: {}\n", path);

    let img = image::open(path)?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    println!("Dimensions: {}x{}", width, height);
    println!("Total pixels: {}\n", width * height);

    // Analyze pixel distribution
    let pixels = rgba.as_raw();
    let mut color_counts: HashMap<(u8, u8, u8, u8), usize> = HashMap::new();

    for chunk in pixels.chunks(4) {
        let color = (chunk[0], chunk[1], chunk[2], chunk[3]);
        *color_counts.entry(color).or_insert(0) += 1;
    }

    println!("Unique colors: {}", color_counts.len());

    // Sort by frequency
    let mut colors: Vec<_> = color_counts.iter().collect();
    colors.sort_by(|a, b| b.1.cmp(a.1));

    println!("\nTop 10 most common colors:");
    for (i, ((r, g, b, a), count)) in colors.iter().take(10).enumerate() {
        let percent = (**count as f64 / (width * height) as f64) * 100.0;
        println!(
            "  {}. RGBA({:3}, {:3}, {:3}, {:3}) - {:7} pixels ({:.2}%)",
            i + 1,
            r,
            g,
            b,
            a,
            count,
            percent
        );
    }

    // Check brightness distribution
    let mut brightness_sum = 0u64;
    let mut black_pixels = 0;
    let mut very_dark = 0;
    let mut dark_gray = 0;
    let mut medium = 0;
    let mut bright = 0;

    for chunk in pixels.chunks(4) {
        let brightness = (chunk[0] as u32 + chunk[1] as u32 + chunk[2] as u32) / 3;
        brightness_sum += brightness as u64;

        match brightness {
            0 => black_pixels += 1,
            1..=30 => very_dark += 1,
            31..=100 => dark_gray += 1,
            101..=200 => medium += 1,
            _ => bright += 1,
        }
    }

    let total_pixels = (width * height) as u64;
    let avg_brightness = brightness_sum as f64 / total_pixels as f64;

    println!("\nBrightness analysis:");
    println!(
        "  Average brightness: {:.2} (0=black, 255=white)",
        avg_brightness
    );
    println!(
        "  Pure black (0): {} pixels ({:.2}%)",
        black_pixels,
        (black_pixels as f64 / total_pixels as f64) * 100.0
    );
    println!(
        "  Very dark (1-30): {} pixels ({:.2}%)",
        very_dark,
        (very_dark as f64 / total_pixels as f64) * 100.0
    );
    println!(
        "  Dark gray (31-100): {} pixels ({:.2}%)",
        dark_gray,
        (dark_gray as f64 / total_pixels as f64) * 100.0
    );
    println!(
        "  Medium (101-200): {} pixels ({:.2}%)",
        medium,
        (medium as f64 / total_pixels as f64) * 100.0
    );
    println!(
        "  Bright (201-255): {} pixels ({:.2}%)",
        bright,
        (bright as f64 / total_pixels as f64) * 100.0
    );

    // Sample specific locations
    println!("\nSample pixels:");
    let get_pixel = |x: u32, y: u32| {
        let idx = ((y * width + x) * 4) as usize;
        (
            pixels[idx],
            pixels[idx + 1],
            pixels[idx + 2],
            pixels[idx + 3],
        )
    };

    println!("  Top-left (0,0): RGBA{:?}", get_pixel(0, 0));
    println!(
        "  Top-right ({},0): RGBA{:?}",
        width - 1,
        get_pixel(width - 1, 0)
    );
    println!(
        "  Center ({},{}): RGBA{:?}",
        width / 2,
        height / 2,
        get_pixel(width / 2, height / 2)
    );
    println!(
        "  Bottom-left (0,{}): RGBA{:?}",
        height - 1,
        get_pixel(0, height - 1)
    );
    println!(
        "  Bottom-right ({},{}): RGBA{:?}",
        width - 1,
        height - 1,
        get_pixel(width - 1, height - 1)
    );

    Ok(())
}
