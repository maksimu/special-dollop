use crate::config::ColorScheme;
use crate::Result;
use fontdue::{Font, FontSettings};
use guacr_protocol::{
    format_blob, format_cfill, format_end, format_instruction, format_rect, format_transfer,
};
use image::{Rgb, RgbImage};
use std::io::Cursor;

// Primary font: Noto Sans Mono (SIL Open Font License)
// Good coverage for Latin, Cyrillic, Greek, and common symbols
const FONT_DATA_PRIMARY: &[u8] = include_bytes!("../fonts/NotoSansMono-Regular.ttf");

// Fallback font: DejaVu Sans Mono (Bitstream Vera license - permissive)
// Broader Unicode coverage including mathematical/technical symbols
// This matches guacd's behavior which requires dejavu-sans-mono-fonts
const FONT_DATA_FALLBACK: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");

/// Terminal renderer - converts terminal screen to JPEG images or Guacamole instructions
///
/// Uses fontdue for actual text rendering with font fallback support.
/// Primary font is Noto Sans Mono; falls back to DejaVu Sans Mono for missing glyphs.
/// This matches guacd's behavior which requires dejavu-sans-mono-fonts package.
/// Character cell dimensions and font size are calculated dynamically based on screen size.
/// JPEG encoding (quality 95) is used for fast rendering with minimal visual loss.
pub struct TerminalRenderer {
    char_width: u32,
    char_height: u32,
    font_size: f32,
    /// Primary font (Noto Sans Mono)
    font_primary: Font,
    /// Fallback font (DejaVu Sans Mono) for broader Unicode coverage
    font_fallback: Font,
    /// Color scheme for terminal rendering
    color_scheme: ColorScheme,
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
        Self::new_with_dimensions_and_scheme(
            char_width,
            char_height,
            font_size,
            ColorScheme::default(),
        )
    }

    /// Create a new renderer with specific dimensions and color scheme
    ///
    /// # Arguments
    ///
    /// * `char_width` - Width of each character cell in pixels
    /// * `char_height` - Height of each character cell in pixels
    /// * `font_size` - Font size in points (typically 70-75% of char_height for good fit)
    /// * `color_scheme` - Color scheme for terminal rendering
    pub fn new_with_dimensions_and_scheme(
        char_width: u32,
        char_height: u32,
        font_size: f32,
        color_scheme: ColorScheme,
    ) -> Result<Self> {
        let font_primary =
            Font::from_bytes(FONT_DATA_PRIMARY, FontSettings::default()).map_err(|e| {
                crate::TerminalError::FontError(format!(
                    "Failed to load primary font (Noto Sans Mono): {}",
                    e
                ))
            })?;

        let font_fallback =
            Font::from_bytes(FONT_DATA_FALLBACK, FontSettings::default()).map_err(|e| {
                crate::TerminalError::FontError(format!(
                    "Failed to load fallback font (DejaVu Sans Mono): {}",
                    e
                ))
            })?;

        Ok(Self {
            char_width,
            char_height,
            font_size,
            font_primary,
            font_fallback,
            color_scheme,
        })
    }

    /// Check if a character has a renderable glyph in the given font
    ///
    /// Returns true if the font can render this character (has non-zero width glyph)
    fn has_glyph(font: &Font, c: char, font_size: f32) -> bool {
        let (metrics, _) = font.rasterize(c, font_size);
        metrics.width > 0
    }

    /// Get the best font for rendering a character
    ///
    /// Tries primary font first, falls back to DejaVu Sans Mono for missing glyphs.
    /// This matches guacd's behavior which uses Pango with system font fallback.
    fn get_font_for_char(&self, c: char) -> &Font {
        if Self::has_glyph(&self.font_primary, c, self.font_size) {
            &self.font_primary
        } else if Self::has_glyph(&self.font_fallback, c, self.font_size) {
            &self.font_fallback
        } else {
            // Neither font has the glyph - return primary (will show placeholder)
            &self.font_primary
        }
    }

    /// Get the current color scheme
    pub fn color_scheme(&self) -> &ColorScheme {
        &self.color_scheme
    }

    /// Set the color scheme
    pub fn set_color_scheme(&mut self, scheme: ColorScheme) {
        self.color_scheme = scheme;
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
        // CRITICAL: Prevent rendering zero-size images (causes black screen)
        if width_px == 0 || height_px == 0 || rows == 0 || cols == 0 {
            return Err(crate::TerminalError::RenderError(format!(
                "Invalid render dimensions: {}x{} px ({}x{} chars)",
                width_px, height_px, cols, rows
            )));
        }

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

                // Check for Unicode box drawing characters (U+2500-U+257F)
                // Render these manually for consistency and crispness
                if Self::is_box_drawing_char(c) {
                    self.render_box_drawing_char(img, c, x, y, fg)?;
                    return Ok(());
                }

                // Check for Unicode block drawing characters (U+2580-U+259F)
                // These are often missing from fonts, so render them manually
                if let Some(block_region) = Self::get_block_character_region(c) {
                    // Render block character as a filled rectangle
                    let (x_start, y_start, x_end, y_end) = block_region;
                    let x0 = x + (self.char_width as f32 * x_start) as u32;
                    let y0 = y + (self.char_height as f32 * y_start) as u32;
                    let x1 = x + (self.char_width as f32 * x_end) as u32;
                    let y1 = y + (self.char_height as f32 * y_end) as u32;

                    for py in y0..y1.min(img.height()) {
                        for px in x0..x1.min(img.width()) {
                            img.put_pixel(px, py, fg);
                        }
                    }
                    return Ok(());
                }

                // Get the best font for this character (primary or fallback)
                // This matches guacd's behavior which uses Pango with system font fallback
                let font = self.get_font_for_char(c);

                // Rasterize glyph at dynamic font size
                let (metrics, bitmap) = font.rasterize(c, self.font_size);

                // If neither font has this glyph, render a placeholder rectangle
                // This ensures the user sees SOMETHING instead of invisible characters
                if bitmap.is_empty() && metrics.width == 0 {
                    // Draw a small centered rectangle as a fallback "missing glyph" indicator
                    let margin_x = self.char_width / 4;
                    let margin_y = self.char_height / 4;
                    for py in (y + margin_y)..(y + self.char_height - margin_y).min(img.height()) {
                        for px in (x + margin_x)..(x + self.char_width - margin_x).min(img.width())
                        {
                            img.put_pixel(px, py, fg);
                        }
                    }
                    return Ok(());
                }

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

    /// Check if character is a box drawing character (U+2500-U+257F)
    fn is_box_drawing_char(c: char) -> bool {
        matches!(c, '\u{2500}'..='\u{257F}')
    }

    /// Render box drawing character manually for consistency
    /// Box drawing characters (U+2500-U+257F) are lines, corners, and intersections
    fn render_box_drawing_char(
        &self,
        img: &mut RgbImage,
        c: char,
        x: u32,
        y: u32,
        color: Rgb<u8>,
    ) -> Result<()> {
        // Line thickness (1-2 pixels for crisp rendering)
        let thick = 1;

        // Calculate midpoints and edges
        let mid_x = x + self.char_width / 2;
        let mid_y = y + self.char_height / 2;
        let left = x;
        let right = x + self.char_width;
        let top = y;
        let bottom = y + self.char_height;

        // Helper to draw horizontal line
        let draw_h_line = |img: &mut RgbImage, y: u32, x1: u32, x2: u32| {
            for py in y.saturating_sub(thick / 2)..=(y + thick / 2).min(img.height() - 1) {
                for px in x1..x2.min(img.width()) {
                    img.put_pixel(px, py, color);
                }
            }
        };

        // Helper to draw vertical line
        let draw_v_line = |img: &mut RgbImage, x: u32, y1: u32, y2: u32| {
            for px in x.saturating_sub(thick / 2)..=(x + thick / 2).min(img.width() - 1) {
                for py in y1..y2.min(img.height()) {
                    img.put_pixel(px, py, color);
                }
            }
        };

        match c {
            // Horizontal lines
            '\u{2500}' | '\u{2501}' => draw_h_line(img, mid_y, left, right), // ─ ━
            // Vertical lines
            '\u{2502}' | '\u{2503}' => draw_v_line(img, mid_x, top, bottom), // │ ┃

            // Corners
            '\u{250C}' | '\u{250D}' | '\u{250E}' | '\u{250F}' => {
                // ┌ ┍ ┎ ┏
                draw_h_line(img, mid_y, mid_x, right);
                draw_v_line(img, mid_x, mid_y, bottom);
            }
            '\u{2510}' | '\u{2511}' | '\u{2512}' | '\u{2513}' => {
                // ┐ ┑ ┒ ┓
                draw_h_line(img, mid_y, left, mid_x);
                draw_v_line(img, mid_x, mid_y, bottom);
            }
            '\u{2514}' | '\u{2515}' | '\u{2516}' | '\u{2517}' => {
                // └ ┕ ┖ ┗
                draw_h_line(img, mid_y, mid_x, right);
                draw_v_line(img, mid_x, top, mid_y);
            }
            '\u{2518}' | '\u{2519}' | '\u{251A}' | '\u{251B}' => {
                // ┘ ┙ ┚ ┛
                draw_h_line(img, mid_y, left, mid_x);
                draw_v_line(img, mid_x, top, mid_y);
            }

            // T-junctions
            '\u{251C}' | '\u{251D}' | '\u{251E}' | '\u{251F}' | '\u{2520}' | '\u{2521}'
            | '\u{2522}' | '\u{2523}' => {
                // ├ ┝ ┞ ┟ ┠ ┡ ┢ ┣
                draw_h_line(img, mid_y, mid_x, right);
                draw_v_line(img, mid_x, top, bottom);
            }
            '\u{2524}' | '\u{2525}' | '\u{2526}' | '\u{2527}' | '\u{2528}' | '\u{2529}'
            | '\u{252A}' | '\u{252B}' => {
                // ┤ ┥ ┦ ┧ ┨ ┩ ┪ ┫
                draw_h_line(img, mid_y, left, mid_x);
                draw_v_line(img, mid_x, top, bottom);
            }
            '\u{252C}' | '\u{252D}' | '\u{252E}' | '\u{252F}' | '\u{2530}' | '\u{2531}'
            | '\u{2532}' | '\u{2533}' => {
                // ┬ ┭ ┮ ┯ ┰ ┱ ┲ ┳
                draw_h_line(img, mid_y, left, right);
                draw_v_line(img, mid_x, mid_y, bottom);
            }
            '\u{2534}' | '\u{2535}' | '\u{2536}' | '\u{2537}' | '\u{2538}' | '\u{2539}'
            | '\u{253A}' | '\u{253B}' => {
                // ┴ ┵ ┶ ┷ ┸ ┹ ┺ ┻
                draw_h_line(img, mid_y, left, right);
                draw_v_line(img, mid_x, top, mid_y);
            }

            // Cross
            '\u{253C}' | '\u{253D}' | '\u{253E}' | '\u{253F}' | '\u{2540}' | '\u{2541}'
            | '\u{2542}' | '\u{2543}' | '\u{2544}' | '\u{2545}' | '\u{2546}' | '\u{2547}'
            | '\u{2548}' | '\u{2549}' | '\u{254A}' | '\u{254B}' => {
                // ┼ and variants
                draw_h_line(img, mid_y, left, right);
                draw_v_line(img, mid_x, top, bottom);
            }

            // For other box drawing chars, fall back to font rendering
            _ => {
                // Try font rendering for less common box drawing chars
                let font = self.get_font_for_char(c);
                let (metrics, bitmap) = font.rasterize(c, self.font_size);
                if !bitmap.is_empty() {
                    // Render using font
                    const BASELINE_RATIO: f32 = 0.75;
                    let glyph_x =
                        x + ((self.char_width as i32 - metrics.width as i32) / 2).max(0) as u32;
                    let glyph_y = y + (self.char_height as f32 * BASELINE_RATIO) as u32;

                    for (i, &alpha) in bitmap.iter().enumerate() {
                        if alpha > 0 {
                            let gx = i % metrics.width;
                            let gy = i / metrics.width;
                            let px = glyph_x + gx as u32;
                            let py = glyph_y
                                .saturating_sub(metrics.height as u32 - metrics.ymin as u32)
                                + gy as u32;

                            if px < img.width() && py < img.height() {
                                let alpha_f = alpha as f32 / 255.0;
                                let current = img.get_pixel(px, py);
                                let blended = Rgb([
                                    ((color.0[0] as f32 * alpha_f)
                                        + (current.0[0] as f32 * (1.0 - alpha_f)))
                                        as u8,
                                    ((color.0[1] as f32 * alpha_f)
                                        + (current.0[1] as f32 * (1.0 - alpha_f)))
                                        as u8,
                                    ((color.0[2] as f32 * alpha_f)
                                        + (current.0[2] as f32 * (1.0 - alpha_f)))
                                        as u8,
                                ]);
                                img.put_pixel(px, py, blended);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns the fill region (x_start, y_start, x_end, y_end) as fractions of cell size
    /// for Unicode block drawing characters (U+2580-U+259F).
    /// Returns None if the character is not a block element.
    fn get_block_character_region(c: char) -> Option<(f32, f32, f32, f32)> {
        match c {
            // Block Elements (U+2580-U+259F)
            '\u{2580}' => Some((0.0, 0.0, 1.0, 0.5)), // ▀ Upper half block
            '\u{2581}' => Some((0.0, 0.875, 1.0, 1.0)), // ▁ Lower one eighth block
            '\u{2582}' => Some((0.0, 0.75, 1.0, 1.0)), // ▂ Lower one quarter block
            '\u{2583}' => Some((0.0, 0.625, 1.0, 1.0)), // ▃ Lower three eighths block
            '\u{2584}' => Some((0.0, 0.5, 1.0, 1.0)), // ▄ Lower half block
            '\u{2585}' => Some((0.0, 0.375, 1.0, 1.0)), // ▅ Lower five eighths block
            '\u{2586}' => Some((0.0, 0.25, 1.0, 1.0)), // ▆ Lower three quarters block
            '\u{2587}' => Some((0.0, 0.125, 1.0, 1.0)), // ▇ Lower seven eighths block
            '\u{2588}' => Some((0.0, 0.0, 1.0, 1.0)), // █ Full block
            '\u{2589}' => Some((0.0, 0.0, 0.875, 1.0)), // ▉ Left seven eighths block
            '\u{258A}' => Some((0.0, 0.0, 0.75, 1.0)), // ▊ Left three quarters block
            '\u{258B}' => Some((0.0, 0.0, 0.625, 1.0)), // ▋ Left five eighths block
            '\u{258C}' => Some((0.0, 0.0, 0.5, 1.0)), // ▌ Left half block
            '\u{258D}' => Some((0.0, 0.0, 0.375, 1.0)), // ▍ Left three eighths block
            '\u{258E}' => Some((0.0, 0.0, 0.25, 1.0)), // ▎ Left one quarter block
            '\u{258F}' => Some((0.0, 0.0, 0.125, 1.0)), // ▏ Left one eighth block
            '\u{2590}' => Some((0.5, 0.0, 1.0, 1.0)), // ▐ Right half block
            '\u{2591}' => Some((0.0, 0.0, 1.0, 1.0)), // ░ Light shade (render as full for now)
            '\u{2592}' => Some((0.0, 0.0, 1.0, 1.0)), // ▒ Medium shade (render as full for now)
            '\u{2593}' => Some((0.0, 0.0, 1.0, 1.0)), // ▓ Dark shade (render as full for now)
            '\u{2594}' => Some((0.0, 0.0, 1.0, 0.125)), // ▔ Upper one eighth block
            '\u{2595}' => Some((0.875, 0.0, 1.0, 1.0)), // ▕ Right one eighth block
            '\u{2596}' => Some((0.0, 0.5, 0.5, 1.0)), // ▖ Quadrant lower left
            '\u{2597}' => Some((0.5, 0.5, 1.0, 1.0)), // ▗ Quadrant lower right
            '\u{2598}' => Some((0.0, 0.0, 0.5, 0.5)), // ▘ Quadrant upper left
            '\u{2599}' => Some((0.0, 0.0, 1.0, 1.0)), // ▙ Quadrant upper left and lower left and lower right (complex)
            '\u{259A}' => Some((0.0, 0.0, 1.0, 1.0)), // ▚ Quadrant upper left and lower right (complex)
            '\u{259B}' => Some((0.0, 0.0, 1.0, 1.0)), // ▛ Quadrant upper left and upper right and lower left (complex)
            '\u{259C}' => Some((0.0, 0.0, 1.0, 1.0)), // ▜ Quadrant upper left and upper right and lower right (complex)
            '\u{259D}' => Some((0.5, 0.0, 1.0, 0.5)), // ▝ Quadrant upper right
            '\u{259E}' => Some((0.0, 0.0, 1.0, 1.0)), // ▞ Quadrant upper right and lower left (complex)
            '\u{259F}' => Some((0.0, 0.0, 1.0, 1.0)), // ▟ Quadrant upper right and lower left and lower right (complex)
            _ => None,
        }
    }

    fn vt100_color_to_rgb(&self, color: vt100::Color, is_foreground: bool) -> Rgb<u8> {
        match color {
            vt100::Color::Default => {
                // Use color scheme for default colors
                if is_foreground {
                    Rgb(self.color_scheme.foreground)
                } else {
                    Rgb(self.color_scheme.background)
                }
            }
            vt100::Color::Idx(n) => {
                // Standard 16-color palette
                // For index 0 (black) and 7 (white), use color scheme if they match
                // the default foreground/background to maintain theme consistency
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
        // Default color scheme is GRAY_BLACK (gray foreground, black background)
        let fg_color = renderer.vt100_color_to_rgb(vt100::Color::Default, true);
        assert_eq!(fg_color.0, [229, 229, 229]); // Gray foreground

        let bg_color = renderer.vt100_color_to_rgb(vt100::Color::Default, false);
        assert_eq!(bg_color.0, [0, 0, 0]); // Black background
    }

    #[test]
    fn test_color_scheme_application() {
        use crate::config::ColorScheme;

        // Test with green-black scheme
        let renderer = TerminalRenderer::new_with_dimensions_and_scheme(
            19,
            38,
            28.0,
            ColorScheme::GREEN_BLACK,
        )
        .unwrap();

        let fg_color = renderer.vt100_color_to_rgb(vt100::Color::Default, true);
        assert_eq!(fg_color.0, [0, 255, 0]); // Green foreground

        let bg_color = renderer.vt100_color_to_rgb(vt100::Color::Default, false);
        assert_eq!(bg_color.0, [0, 0, 0]); // Black background
    }
}
