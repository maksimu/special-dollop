// Graphics protocol support for terminal-based RDP rendering
//
// Supports Sixel graphics - the most widely compatible terminal image protocol

use bytes::Bytes;

/// Graphics rendering mode for terminal output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsMode {
    /// Sixel graphics - bitmap graphics in terminal
    /// Supported by: xterm, mlterm, mintty, iTerm2, WezTerm, foot, Contour
    /// Most widely compatible image protocol
    Sixel,
}

impl GraphicsMode {
    /// Detect if terminal supports Sixel
    pub fn detect() -> Self {
        // Always return Sixel - it's the only mode we support
        Self::Sixel
    }

    /// Get effective resolution for Sixel mode
    pub fn effective_resolution(&self, _term_cols: u16, _term_rows: u16) -> (u32, u32) {
        // Sixel can display arbitrary resolution
        // Return reasonable default
        (640, 480)
    }
}

/// Graphics renderer for terminal output
#[allow(dead_code)]
pub struct GraphicsRenderer {
    mode: GraphicsMode,
    term_cols: u16,
    term_rows: u16,
}

impl GraphicsRenderer {
    /// Create a new graphics renderer
    pub fn new(mode: GraphicsMode, term_cols: u16, term_rows: u16) -> Self {
        Self {
            mode,
            term_cols,
            term_rows,
        }
    }

    /// Create renderer with auto-detected mode
    pub fn auto_detect(term_cols: u16, term_rows: u16) -> Self {
        Self::new(GraphicsMode::detect(), term_cols, term_rows)
    }

    /// Render framebuffer to terminal output using Sixel
    pub fn render(&self, framebuffer: &[u8], width: u32, height: u32) -> Bytes {
        self.render_sixel(framebuffer, width, height)
    }

    /// Render using Sixel graphics protocol
    fn render_sixel(&self, framebuffer: &[u8], width: u32, height: u32) -> Bytes {
        // For now, use a simplified sixel implementation
        // Full sixel encoding is complex - this creates a basic working version

        // Downsample to reasonable size for sixel (it's slow)
        let target_width = width.min(640);
        let target_height = height.min(480);

        let downsampled = if width != target_width || height != target_height {
            downsample_image(framebuffer, width, height, target_width, target_height)
        } else {
            framebuffer.to_vec()
        };

        // Start sixel sequence
        let mut output = String::from("\x1bPq"); // DCS + 'q' (sixel mode)

        // Raster attributes: "width;height
        output.push_str(&format!("\"1;1;{};{}", target_width, target_height));

        // For simplicity, use a limited color palette (16 colors)
        // Define color palette
        for i in 0..16 {
            let r = ((i & 1) * 50 + (i & 8) * 50) as u8;
            let g = (((i & 2) >> 1) * 50 + ((i & 8) >> 1) * 50) as u8;
            let b = (((i & 4) >> 2) * 50 + ((i & 8) >> 2) * 50) as u8;

            // Convert RGB to percentage (0-100)
            let r_pct = (r as u32 * 100) / 255;
            let g_pct = (g as u32 * 100) / 255;
            let b_pct = (b as u32 * 100) / 255;

            output.push_str(&format!("#{};2;{};{};{}", i, r_pct, g_pct, b_pct));
        }

        // Encode image data
        // Sixel encodes 6 vertical pixels per character
        for y in (0..target_height).step_by(6) {
            for color_idx in 0..16 {
                output.push_str(&format!("#{}", color_idx));

                let mut run_length = 0;
                let mut last_sixel = '?';

                for x in 0..target_width {
                    // Collect 6 vertical pixels
                    let mut sixel_byte = 0u8;

                    for dy in 0..6 {
                        let py = y + dy;
                        if py < target_height {
                            let idx = ((py * target_width + x) * 4) as usize;
                            if idx + 2 < downsampled.len() {
                                let r = downsampled[idx];
                                let g = downsampled[idx + 1];
                                let b = downsampled[idx + 2];

                                // Simple color quantization to 16 colors
                                let pixel_color = quantize_to_16_colors(r, g, b);

                                if pixel_color == color_idx {
                                    sixel_byte |= 1 << dy;
                                }
                            }
                        }
                    }

                    // Encode sixel byte
                    let sixel_char = (sixel_byte + 63) as char; // Add 63 to get printable char

                    if sixel_char == last_sixel && run_length < 255 {
                        run_length += 1;
                    } else {
                        if run_length > 3 {
                            output.push_str(&format!("!{}{}", run_length, last_sixel));
                        } else {
                            for _ in 0..run_length {
                                output.push(last_sixel);
                            }
                        }
                        last_sixel = sixel_char;
                        run_length = 1;
                    }
                }

                // Flush remaining run
                if run_length > 3 {
                    output.push_str(&format!("!{}{}", run_length, last_sixel));
                } else {
                    for _ in 0..run_length {
                        output.push(last_sixel);
                    }
                }

                output.push('$'); // Carriage return
            }
            output.push('-'); // Line feed
        }

        // End sixel sequence
        output.push_str("\x1b\\"); // ST (String Terminator)

        Bytes::from(output)
    }
}

// Helper functions for Sixel rendering

/// Downsample image to target resolution
fn downsample_image(
    framebuffer: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    let mut output = vec![0u8; (dst_width * dst_height * 4) as usize];

    let x_ratio = src_width as f32 / dst_width as f32;
    let y_ratio = src_height as f32 / dst_height as f32;

    for y in 0..dst_height {
        for x in 0..dst_width {
            let src_x = (x as f32 * x_ratio) as u32;
            let src_y = (y as f32 * y_ratio) as u32;

            let src_idx = ((src_y * src_width + src_x) * 4) as usize;
            let dst_idx = ((y * dst_width + x) * 4) as usize;

            if src_idx + 3 < framebuffer.len() && dst_idx + 3 < output.len() {
                output[dst_idx..dst_idx + 4].copy_from_slice(&framebuffer[src_idx..src_idx + 4]);
            }
        }
    }

    output
}

/// Quantize RGB color to 16-color palette
///
/// Uses standard 4-bit color encoding:
/// - Bit 0: Red
/// - Bit 1: Green  
/// - Bit 2: Blue
/// - Bit 3: Bright/Intensity
fn quantize_to_16_colors(r: u8, g: u8, b: u8) -> u8 {
    // Determine base color bits
    let r_bit = if r > 128 { 1 } else { 0 };
    let g_bit = if g > 128 { 1 } else { 0 };
    let b_bit = if b > 128 { 1 } else { 0 };

    // Brightness: if any channel is bright (>128), use bright variant
    // This ensures pure red (255,0,0) maps to bright red (9), not dark red (1)
    let bright = if r > 128 || g > 128 || b > 128 { 1 } else { 0 };

    // Standard 4-bit color: bright(bit3) | blue(bit2) | green(bit1) | red(bit0)
    (bright << 3) | (b_bit << 2) | (g_bit << 1) | r_bit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graphics_mode_detection() {
        let mode = GraphicsMode::detect();
        assert_eq!(mode, GraphicsMode::Sixel);
    }

    #[test]
    fn test_effective_resolution() {
        let mode = GraphicsMode::Sixel;
        let (width, height) = mode.effective_resolution(80, 24);
        assert_eq!(width, 640);
        assert_eq!(height, 480);
    }

    #[test]
    fn test_quantize_to_16_colors() {
        // Black
        assert_eq!(quantize_to_16_colors(0, 0, 0), 0);
        // White
        assert_eq!(quantize_to_16_colors(255, 255, 255), 15);
        // Red
        assert_eq!(quantize_to_16_colors(255, 0, 0), 9);
    }
}
