use crate::Result;
use fontdue::{Font, FontSettings};
use guacr_protocol::{
    format_blob, format_cfill, format_end, format_instruction, format_rect, format_transfer,
};
use image::{Rgb, RgbImage};
use std::io::Cursor;

// Embedded monospace font (Noto Sans Mono - SIL Open Font License)
const FONT_DATA: &[u8] = include_bytes!("../fonts/NotoSansMono-Regular.ttf");

/// Terminal renderer - converts terminal screen to JPEG images or Guacamole instructions
///
/// Uses fontdue for actual text rendering instead of colored blocks.
/// Character cell dimensions and font size are calculated dynamically based on screen size.
/// JPEG encoding (quality 95) is used for fast rendering with minimal visual loss.
pub struct TerminalRenderer {
    char_width: u32,
    char_height: u32,
    font_size: f32,
    font: Font,
}

impl TerminalRenderer {
    /// Create a new renderer with default dimensions
    ///
    /// Uses 19x38 pixel cells with 28pt font (legacy defaults for backward compatibility)
    pub fn new() -> Result<Self> {
        Self::new_with_dimensions(19, 38, 28.0)
    }

    /// Create a new renderer with specific character cell dimensions
    ///
    /// This allows dynamic sizing based on screen resolution and terminal dimensions.
    ///
    /// # Arguments
    ///
    /// * `char_width` - Width of each character cell in pixels
    /// * `char_height` - Height of each character cell in pixels
    /// * `font_size` - Font size in points (typically 70-75% of char_height for good fit)
    ///
    /// # Example
    ///
    /// ```
    /// use guacr_terminal::TerminalRenderer;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // For 1920x1080 screen with 80x24 terminal:
    /// let char_width = 1920 / 80;  // = 24px
    /// let char_height = 1080 / 24; // = 45px
    /// let font_size = char_height as f32 * 0.70; // = 31.5pt
    /// let renderer = TerminalRenderer::new_with_dimensions(char_width, char_height, font_size)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new_with_dimensions(char_width: u32, char_height: u32, font_size: f32) -> Result<Self> {
        let font = Font::from_bytes(FONT_DATA, FontSettings::default()).map_err(|e| {
            crate::TerminalError::FontError(format!("Failed to load embedded font: {}", e))
        })?;

        Ok(Self {
            char_width,
            char_height,
            font_size,
            font,
        })
    }

    /// Render terminal screen to JPEG
    ///
    /// Renders at exact pixel dimensions, padding if necessary to match browser's layer size
    pub fn render_screen(&self, screen: &vt100::Screen, rows: u16, cols: u16) -> Result<Vec<u8>> {
        self.render_screen_with_size(
            screen,
            rows,
            cols,
            cols as u32 * self.char_width,
            rows as u32 * self.char_height,
        )
    }

    /// Render only a specific region of the terminal (dirty region optimization)
    ///
    /// This is the guacd optimization - only render changed portions of the screen
    pub fn render_region(
        &self,
        screen: &vt100::Screen,
        min_row: u16,
        max_row: u16,
        min_col: u16,
        max_col: u16,
    ) -> Result<(Vec<u8>, u32, u32, u32, u32)> {
        let width = (max_col - min_col + 1) as u32;
        let height = (max_row - min_row + 1) as u32;
        let width_px = width * self.char_width;
        let height_px = height * self.char_height;

        let mut img = RgbImage::new(width_px, height_px);

        // Background (black)
        for pixel in img.pixels_mut() {
            *pixel = Rgb([0, 0, 0]);
        }

        // Render each cell in the region
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                let x = (col - min_col) as u32;
                let y = (row - min_row) as u32;
                let x_px = x * self.char_width;
                let y_px = y * self.char_height;

                if let Some(cell) = screen.cell(row, col) {
                    let has_cursor = screen.cursor_position() == (row, col);
                    self.render_cell(&mut img, cell, x_px, y_px, has_cursor)?;
                }
            }
        }

        // Encode to JPEG (quality 95 for fast encoding with minimal visual loss)
        // JPEG is 5-10x faster than PNG for terminal rendering
        let mut jpeg_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpeg_data), image::ImageFormat::Jpeg)?;

        // Return JPEG + position info (x, y in pixels)
        let x_px = min_col as u32 * self.char_width;
        let y_px = min_row as u32 * self.char_height;
        Ok((jpeg_data, x_px, y_px, width_px, height_px))
    }

    /// Render terminal screen to JPEG with exact pixel dimensions
    ///
    /// This version allows specifying exact output dimensions, useful for
    /// matching browser layer size exactly.
    pub fn render_screen_with_size(
        &self,
        screen: &vt100::Screen,
        rows: u16,
        cols: u16,
        width_px: u32,
        height_px: u32,
    ) -> Result<Vec<u8>> {
        let mut img = RgbImage::new(width_px, height_px);

        // Background (black)
        for pixel in img.pixels_mut() {
            *pixel = Rgb([0, 0, 0]);
        }

        // Render each cell
        for row in 0..rows {
            for col in 0..cols {
                let x_px = col as u32 * self.char_width;
                let y_px = row as u32 * self.char_height;

                // Skip if outside image bounds
                if x_px >= width_px || y_px >= height_px {
                    continue;
                }

                if let Some(cell) = screen.cell(row, col) {
                    let has_cursor = screen.cursor_position() == (row, col);
                    self.render_cell(&mut img, cell, x_px, y_px, has_cursor)?;
                }
            }
        }

        // Encode to JPEG (quality 95 for fast encoding with minimal visual loss)
        // JPEG is 5-10x faster than PNG for terminal rendering
        let mut jpeg_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpeg_data), image::ImageFormat::Jpeg)?;

        Ok(jpeg_data)
    }

    fn render_cell(
        &self,
        img: &mut RgbImage,
        cell: &vt100::Cell,
        x: u32,
        y: u32,
        has_cursor: bool,
    ) -> Result<()> {
        // Background color
        let bg = self.vt100_color_to_rgb(cell.bgcolor(), false);

        // Fill cell background
        for py in y..(y + self.char_height).min(img.height()) {
            for px in x..(x + self.char_width).min(img.width()) {
                img.put_pixel(px, py, bg);
            }
        }

        // Render character using fontdue
        if let Some(c) = cell.contents().chars().next() {
            if c != ' ' && c != '\0' {
                let fg = self.vt100_color_to_rgb(cell.fgcolor(), true);

                // Rasterize glyph at dynamic font size
                let (metrics, bitmap) = self.font.rasterize(c, self.font_size);

                // CRITICAL: Use consistent baseline alignment, not centering
                // This ensures all characters appear at the same size/position
                // Baseline is positioned at 75% down the cell height (standard terminal practice)
                const BASELINE_RATIO: f32 = 0.75; // Baseline at 75% from top

                // Horizontal: center the glyph
                let glyph_x =
                    x + ((self.char_width as i32 - metrics.width as i32) / 2).max(0) as u32;

                // Vertical: align to baseline using fontdue's PositiveYDown coordinate system
                // fontdue metrics: bounds.ymin = bottom edge offset from baseline (negative for descenders)
                //                  bounds.height = total glyph height
                // For PositiveYDown: glyph_top = baseline - height - ymin
                //
                // Example: 'A' with ymin=0, height=20 at baseline=28.5
                //   glyph_top = 28.5 - 20 - 0 = 8.5 (correct: top at 8.5, bottom at 28.5)
                // Example: 'g' with ymin=-6, height=22 at baseline=28.5
                //   glyph_top = 28.5 - 22 - (-6) = 12.5 (correct: top at 12.5, extends to 34.5)
                let baseline_y = y as f32 + (self.char_height as f32 * BASELINE_RATIO);
                let glyph_y =
                    (baseline_y - metrics.bounds.height - metrics.bounds.ymin).max(y as f32) as u32;

                // Draw glyph bitmap (grayscale alpha blending)
                for (i, &alpha) in bitmap.iter().enumerate() {
                    if alpha > 0 {
                        let dx = (i % metrics.width) as u32;
                        let dy = (i / metrics.width) as u32;
                        let px = glyph_x + dx;
                        let py = glyph_y + dy;

                        if px < img.width() && py < img.height() {
                            // Alpha blend foreground over background
                            let alpha_f = alpha as f32 / 255.0;
                            let current = img.get_pixel(px, py);
                            let blended = Rgb([
                                ((fg.0[0] as f32 * alpha_f)
                                    + (current.0[0] as f32 * (1.0 - alpha_f)))
                                    as u8,
                                ((fg.0[1] as f32 * alpha_f)
                                    + (current.0[1] as f32 * (1.0 - alpha_f)))
                                    as u8,
                                ((fg.0[2] as f32 * alpha_f)
                                    + (current.0[2] as f32 * (1.0 - alpha_f)))
                                    as u8,
                            ]);
                            img.put_pixel(px, py, blended);
                        }
                    }
                }
            }
        }

        // Render cursor if present
        if has_cursor {
            self.draw_cursor(img, x, y)?;
        }

        Ok(())
    }

    fn draw_cursor(&self, img: &mut RgbImage, x: u32, y: u32) -> Result<()> {
        // Draw cursor as underline (bottom 3 pixels of cell)
        let cursor_color = Rgb([255, 255, 255]);
        let cursor_height = 3;

        for dy in 0..cursor_height {
            for dx in 0..self.char_width {
                let px = x + dx;
                let py = y + self.char_height - dy - 1;
                if px < img.width() && py < img.height() {
                    img.put_pixel(px, py, cursor_color);
                }
            }
        }

        Ok(())
    }

    fn vt100_color_to_rgb(&self, color: vt100::Color, is_foreground: bool) -> Rgb<u8> {
        match color {
            vt100::Color::Default => {
                if is_foreground {
                    Rgb([229, 229, 229]) // Default fg = light gray (like most terminals)
                } else {
                    Rgb([0, 0, 0]) // Default bg = black
                }
            }
            vt100::Color::Idx(n) => {
                // Standard 16-color palette
                match n {
                    0 => Rgb([0, 0, 0]),        // Black
                    1 => Rgb([205, 0, 0]),      // Red
                    2 => Rgb([0, 205, 0]),      // Green
                    3 => Rgb([205, 205, 0]),    // Yellow
                    4 => Rgb([0, 0, 238]),      // Blue
                    5 => Rgb([205, 0, 205]),    // Magenta
                    6 => Rgb([0, 205, 205]),    // Cyan
                    7 => Rgb([229, 229, 229]),  // White
                    8 => Rgb([127, 127, 127]),  // Bright Black
                    9 => Rgb([255, 0, 0]),      // Bright Red
                    10 => Rgb([0, 255, 0]),     // Bright Green
                    11 => Rgb([255, 255, 0]),   // Bright Yellow
                    12 => Rgb([92, 92, 255]),   // Bright Blue
                    13 => Rgb([255, 0, 255]),   // Bright Magenta
                    14 => Rgb([0, 255, 255]),   // Bright Cyan
                    15 => Rgb([255, 255, 255]), // Bright White
                    _ => Rgb([0, 0, 0]),        // Fallback
                }
            }
            vt100::Color::Rgb(r, g, b) => Rgb([r, g, b]),
        }
    }

    /// Generate Guacamole protocol instructions for drawing operations
    ///
    /// Note: This method uses drawing instructions instead of JPEG images.
    /// For this implementation, JPEG is preferred (5-10x faster than PNG).
    pub fn format_drawing_instructions(
        &self,
        screen: &vt100::Screen,
        rows: u16,
        cols: u16,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Clear screen
        instructions.push(format_rect(
            0,
            0,
            0,
            cols as u32 * self.char_width,
            rows as u32 * self.char_height,
        ));
        instructions.push(format_cfill(0, 0, 0, 0, 255)); // Black background

        // Render each cell as colored rectangle (simplified)
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    let x = col as u32 * self.char_width;
                    let y = row as u32 * self.char_height;

                    let bg = self.vt100_color_to_rgb(cell.bgcolor(), false);

                    instructions.push(format_rect(0, x, y, self.char_width, self.char_height));
                    instructions.push(format_cfill(0, bg.0[0], bg.0[1], bg.0[2], 255));
                }
            }
        }

        instructions
    }

    /// Generate Guacamole protocol instructions to send a JPEG image
    ///
    /// This is what guacd actually uses - JPEG images for terminal rendering.
    /// JPEG encoding is fast and efficient for terminal screenshots.
    pub fn format_img_instructions(
        &self,
        image_data: &[u8],
        stream_id: u32,
        layer: i32,
        x: i32,
        y: i32,
    ) -> Vec<String> {
        use base64::Engine;

        const BLOB_CHUNK_SIZE: usize = 6144; // 6KB chunks (8KB base64-encoded)
        let mut instructions = Vec::new();

        // 1. img instruction: allocate stream for image
        // Format: img,<stream>,<mask>,<layer>,<mimetype>,<x>,<y>;
        // Every element needs LENGTH.VALUE format
        let mask = 0x07; // RGB channels (no alpha) - matches RgbImage format
        let stream_str = stream_id.to_string();
        let mask_str = mask.to_string();
        let layer_str = layer.to_string();
        let x_str = x.to_string();
        let y_str = y.to_string();

        let img_instr = format!(
            "3.img,{}.{},{}.{},{}.{},10.image/jpeg,{}.{},{}.{};",
            stream_str.len(),
            stream_str,
            mask_str.len(),
            mask_str,
            layer_str.len(),
            layer_str,
            x_str.len(),
            x_str,
            y_str.len(),
            y_str
        );
        instructions.push(img_instr);

        // 2. blob instructions: send base64-encoded JPEG data in chunks
        let base64_data = base64::engine::general_purpose::STANDARD.encode(image_data);

        for chunk in base64_data.as_bytes().chunks(BLOB_CHUNK_SIZE) {
            let chunk_str = String::from_utf8_lossy(chunk);
            let blob_instr = format_blob(stream_id, &chunk_str);
            instructions.push(blob_instr);
        }

        // 3. end instruction: close stream
        instructions.push(format_end(stream_id));

        instructions
    }

    /// Format ready instruction
    pub fn format_ready_instruction(protocol: &str) -> String {
        format_instruction("ready", &[protocol])
    }

    /// Format size instruction
    pub fn format_size_instruction(layer: i32, width: u32, height: u32) -> String {
        format_instruction(
            "size",
            &[&layer.to_string(), &width.to_string(), &height.to_string()],
        )
    }

    /// Format sync instruction
    ///
    /// Format: `4.sync,{timestamp};`
    ///
    /// # Arguments
    /// - `timestamp_ms`: Timestamp in milliseconds
    pub fn format_sync_instruction(&self, timestamp_ms: u64) -> String {
        let timestamp_str = timestamp_ms.to_string();
        format_instruction("sync", &[&timestamp_str])
    }

    /// Format copy instruction (for scroll optimization)
    #[allow(clippy::too_many_arguments)]
    pub fn format_copy_instruction(
        src_row: u16,
        src_col: u16,
        width: u16,
        height: u16,
        dst_row: u16,
        dst_col: u16,
        char_width: u32,
        char_height: u32,
        layer: i32,
    ) -> String {
        // Copy instruction format: copy,<src_layer>,<src_x>,<src_y>,<width>,<height>,<dst_layer>,<dst_x>,<dst_y>;
        let src_x = src_col as u32 * char_width;
        let src_y = src_row as u32 * char_height;
        let width_px = width as u32 * char_width;
        let height_px = height as u32 * char_height;
        let dst_x = dst_col as u32 * char_width;
        let dst_y = dst_row as u32 * char_height;

        format_transfer(
            layer, src_x, src_y, width_px, height_px, 12, // SRC function
            layer, dst_x, dst_y,
        )
    }

    /// Format clear region instructions (for scroll optimization)
    pub fn format_clear_region_instructions(
        row: u16,
        col: u16,
        width: u16,
        height: u16,
        char_width: u32,
        char_height: u32,
    ) -> Vec<String> {
        let x = col as u32 * char_width;
        let y = row as u32 * char_height;
        let width_px = width as u32 * char_width;
        let height_px = height as u32 * char_height;

        vec![
            format_rect(0, x, y, width_px, height_px),
            format_cfill(0, 0, 0, 0, 255), // Black background
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renderer_new() {
        let renderer = TerminalRenderer::new();
        assert!(renderer.is_ok());
    }

    #[test]
    fn test_renderer_with_dimensions() {
        let renderer = TerminalRenderer::new_with_dimensions(24, 45, 31.5);
        assert!(renderer.is_ok());
    }

    #[test]
    fn test_vt100_color_to_rgb() {
        let renderer = TerminalRenderer::new().unwrap();
        let color = renderer.vt100_color_to_rgb(vt100::Color::Default, true);
        assert_eq!(color.0, [229, 229, 229]); // Light gray
    }
}
