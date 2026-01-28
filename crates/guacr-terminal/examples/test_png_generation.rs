// Quick test to verify JPEG rendering is working correctly
use guacr_terminal::{TerminalEmulator, TerminalRenderer};
use std::fs;

fn main() -> anyhow::Result<()> {
    // Create a terminal
    let mut terminal = TerminalEmulator::new(24, 80);

    // Process some test data (simulating SSH login prompt)
    terminal.process(b"Linux server v5.15.0\nlogin: ")?;

    // Render to JPEG (5-10x faster than PNG)
    let renderer = TerminalRenderer::new()?;
    let jpeg = renderer.render_screen(terminal.screen(), 24, 80)?;

    println!("JPEG size: {} bytes", jpeg.len());

    // Check terminal contents
    let contents = terminal.screen().contents();
    println!("Terminal contents:\n{}", contents);
    println!("Terminal has {} chars", contents.len());

    // Save JPEG to disk for inspection
    fs::write("/tmp/test_terminal.jpg", &jpeg)?;
    println!("Saved JPEG to /tmp/test_terminal.jpg");

    // Check if JPEG has non-black pixels
    let has_colors = check_jpeg_colors(&jpeg)?;
    println!("JPEG has non-black pixels: {}", has_colors);

    Ok(())
}

fn check_jpeg_colors(jpeg_data: &[u8]) -> anyhow::Result<bool> {
    use image::ImageReader;
    use std::io::Cursor;

    let img = ImageReader::new(Cursor::new(jpeg_data))
        .with_guessed_format()?
        .decode()?;

    let rgb = img.to_rgb8();

    let mut black_pixels = 0;
    let mut colored_pixels = 0;

    for pixel in rgb.pixels() {
        if pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0 {
            black_pixels += 1;
        } else {
            colored_pixels += 1;
            // Print first few colored pixels
            if colored_pixels <= 5 {
                println!(
                    "  Colored pixel: RGB({}, {}, {})",
                    pixel[0], pixel[1], pixel[2]
                );
            }
        }
    }

    println!(
        "Black pixels: {}, Colored pixels: {}",
        black_pixels, colored_pixels
    );

    Ok(colored_pixels > 0)
}
