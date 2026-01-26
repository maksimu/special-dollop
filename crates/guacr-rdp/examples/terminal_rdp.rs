// Example: RDP rendering to terminal using Sixel graphics
//
// This demonstrates how to use the graphics_protocol module to render
// RDP sessions in a terminal using Sixel graphics.
//
// Usage:
//   cargo run --example terminal_rdp

use guacr_terminal::{GraphicsMode, GraphicsRenderer};

fn main() {
    let mode = GraphicsMode::Sixel;

    println!("Using Sixel graphics mode");
    println!();

    // Get terminal size (or use defaults)
    let (term_cols, term_rows) = terminal_size().unwrap_or((80, 24));
    println!("Terminal size: {}×{}", term_cols, term_rows);

    let (eff_width, eff_height) = mode.effective_resolution(term_cols, term_rows);
    println!("Effective resolution: {}×{} pixels", eff_width, eff_height);
    println!();

    // Create renderer
    let renderer = GraphicsRenderer::new(mode, term_cols, term_rows);

    // Create a test pattern (gradient)
    let width = 640u32;
    let height = 480u32;
    let mut framebuffer = vec![0u8; (width * height * 4) as usize];

    // Generate test pattern
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;

            // Gradient pattern
            framebuffer[idx] = ((x * 255) / width) as u8; // R
            framebuffer[idx + 1] = ((y * 255) / height) as u8; // G
            framebuffer[idx + 2] = 128; // B
            framebuffer[idx + 3] = 255; // A
        }
    }

    // Add some text-like patterns (white rectangles)
    for y in 100..120 {
        for x in 50..200 {
            let idx = ((y * width + x) * 4) as usize;
            framebuffer[idx] = 255;
            framebuffer[idx + 1] = 255;
            framebuffer[idx + 2] = 255;
        }
    }

    // Render to terminal
    println!("Rendering test pattern...");
    println!();

    let output = renderer.render(&framebuffer, width, height);
    print!("{}", String::from_utf8_lossy(&output));

    println!();
    println!("Done!");
}

/// Get terminal size using termion or similar
fn terminal_size() -> Option<(u16, u16)> {
    // Try to get terminal size from environment or ioctl
    // For now, just return None to use defaults

    // You could use termion crate:
    // use termion::terminal_size;
    // terminal_size().ok()

    None
}
