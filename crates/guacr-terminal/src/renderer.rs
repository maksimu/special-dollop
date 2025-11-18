use crate::Result;
use fontdue::{Font, FontSettings};
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

        // Get cursor position
        let cursor_pos = screen.cursor_position();

        // Render cells in the dirty region
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                if let Some(cell) = screen.cell(row, col) {
                    let local_row = row - min_row;
                    let local_col = col - min_col;
                    let has_cursor = cursor_pos.0 == row && cursor_pos.1 == col;
                    self.render_cell_at(&mut img, cell, local_row, local_col, has_cursor)?;
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

    fn render_cell_at(
        &self,
        img: &mut RgbImage,
        cell: &vt100::Cell,
        row: u16,
        col: u16,
        has_cursor: bool,
    ) -> Result<()> {
        self.render_cell(img, cell, row, col, has_cursor)
    }

    /// Render terminal screen to JPEG with exact pixel dimensions
    ///
    /// This allows rendering at non-character-aligned sizes to match browser layer exactly
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

        // Get cursor position
        let cursor_pos = screen.cursor_position();

        // Render each cell
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    let has_cursor = cursor_pos.0 == row && cursor_pos.1 == col;
                    self.render_cell(&mut img, cell, row, col, has_cursor)?;
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
        row: u16,
        col: u16,
        has_cursor: bool,
    ) -> Result<()> {
        let x = col as u32 * self.char_width;
        let y = row as u32 * self.char_height;

        // Get cell background color
        let bg = self.vt100_color_to_rgb(cell.bgcolor(), false);

        // Fill background
        for dy in 0..self.char_height {
            for dx in 0..self.char_width {
                let px = x + dx;
                let py = y + dy;
                if px < img.width() && py < img.height() {
                    img.put_pixel(px, py, bg);
                }
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
            vt100::Color::Idx(idx) => self.ansi_color_to_rgb(idx),
            vt100::Color::Rgb(r, g, b) => Rgb([r, g, b]),
        }
    }

    fn ansi_color_to_rgb(&self, idx: u8) -> Rgb<u8> {
        // Standard ANSI color palette
        match idx {
            0 => Rgb([0, 0, 0]),        // Black
            1 => Rgb([205, 0, 0]),      // Red
            2 => Rgb([0, 205, 0]),      // Green
            3 => Rgb([205, 205, 0]),    // Yellow
            4 => Rgb([0, 0, 238]),      // Blue
            5 => Rgb([205, 0, 205]),    // Magenta
            6 => Rgb([0, 205, 205]),    // Cyan
            7 => Rgb([229, 229, 229]),  // White
            8 => Rgb([127, 127, 127]),  // Bright black (gray)
            9 => Rgb([255, 0, 0]),      // Bright red
            10 => Rgb([0, 255, 0]),     // Bright green
            11 => Rgb([255, 255, 0]),   // Bright yellow
            12 => Rgb([92, 92, 255]),   // Bright blue
            13 => Rgb([255, 0, 255]),   // Bright magenta
            14 => Rgb([0, 255, 255]),   // Bright cyan
            15 => Rgb([255, 255, 255]), // Bright white
            _ => Rgb([229, 229, 229]),  // Default to white
        }
    }
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

impl TerminalRenderer {
    /// Generate Guacamole drawing instructions for terminal rendering
    ///
    /// Returns Guacamole protocol instructions (rect, cfill, cursor) to render the terminal screen.
    /// Note: This method uses drawing instructions instead of JPEG images.
    /// For this implementation, JPEG is preferred (5-10x faster than PNG).
    ///
    /// Instructions generated:
    /// 1. size - Set display dimensions
    /// 2. rect + cfill - Draw each character cell with background/foreground
    /// 3. cursor - Draw cursor position
    pub fn render_terminal_instructions(
        &self,
        screen: &vt100::Screen,
        rows: u16,
        cols: u16,
    ) -> Vec<String> {
        let mut instructions = Vec::new();

        // Get cursor position
        let cursor_pos = screen.cursor_position();

        // Render each cell as drawing instruction
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    let x = col as i32 * self.char_width as i32;
                    let y = row as i32 * self.char_height as i32;

                    // Get colors
                    let bg = self.vt100_color_to_rgb(cell.bgcolor(), false);
                    let fg = self.vt100_color_to_rgb(cell.fgcolor(), true);

                    // Draw background rectangle
                    let x_str = x.to_string();
                    let y_str = y.to_string();
                    let w_str = self.char_width.to_string();
                    let h_str = self.char_height.to_string();

                    // rect instruction: draws path
                    let rect_instr = format!(
                        "4.rect,1.0,{}.{},{}.{},{}.{},{}.{};",
                        x_str.len(),
                        x_str,
                        y_str.len(),
                        y_str,
                        w_str.len(),
                        w_str,
                        h_str.len(),
                        h_str
                    );
                    instructions.push(rect_instr);

                    // cfill instruction: fill path with background color
                    let r_str = bg.0[0].to_string();
                    let g_str = bg.0[1].to_string();
                    let b_str = bg.0[2].to_string();
                    let cfill_instr = format!(
                        "5.cfill,2.15,{}.{},{}.{},{}.{},3.255;",
                        r_str.len(),
                        r_str,
                        g_str.len(),
                        g_str,
                        b_str.len(),
                        b_str
                    );
                    instructions.push(cfill_instr);

                    // If cell has visible character, draw foreground
                    if let Some(c) = cell.contents().chars().next() {
                        if c != ' ' && c != '\0' {
                            // Draw smaller rect for character (leave margins)
                            let char_x = x + 1;
                            let char_y = y + 2;
                            let char_w = (self.char_width - 2) as i32;
                            let char_h = (self.char_height - 4) as i32;

                            let cx_str = char_x.to_string();
                            let cy_str = char_y.to_string();
                            let cw_str = char_w.to_string();
                            let ch_str = char_h.to_string();

                            let char_rect = format!(
                                "4.rect,1.0,{}.{},{}.{},{}.{},{}.{};",
                                cx_str.len(),
                                cx_str,
                                cy_str.len(),
                                cy_str,
                                cw_str.len(),
                                cw_str,
                                ch_str.len(),
                                ch_str
                            );
                            instructions.push(char_rect);

                            // Fill with foreground color
                            let fr_str = fg.0[0].to_string();
                            let fg_str = fg.0[1].to_string();
                            let fb_str = fg.0[2].to_string();
                            let char_cfill = format!(
                                "5.cfill,2.15,{}.{},{}.{},{}.{},3.255;",
                                fr_str.len(),
                                fr_str,
                                fg_str.len(),
                                fg_str,
                                fb_str.len(),
                                fb_str
                            );
                            instructions.push(char_cfill);
                        }
                    }

                    // Draw cursor if at this position
                    if cursor_pos.0 == row && cursor_pos.1 == col {
                        // Cursor as white filled rectangle
                        let cursor_rect = format!(
                            "4.rect,1.0,{}.{},{}.{},{}.{},{}.{};",
                            x_str.len(),
                            x_str,
                            y_str.len(),
                            y_str,
                            w_str.len(),
                            w_str,
                            h_str.len(),
                            h_str
                        );
                        instructions.push(cursor_rect);

                        let cursor_cfill = "5.cfill,2.15,3.255,3.255,3.255,3.255;".to_string();
                        instructions.push(cursor_cfill);
                    }
                }
            }
        }

        instructions
    }

    /// Generate Guacamole protocol instructions to send a JPEG image
    ///
    /// This is what guacd actually uses - JPEG images for terminal rendering.
    /// JPEG encoding is fast and efficient for terminal screenshots.
    ///
    /// Returns a vector of instruction strings that should be sent in sequence:
    /// 1. img instruction (allocates stream)
    /// 2. blob instruction(s) (sends base64-encoded JPEG data in chunks)
    /// 3. end instruction (closes stream)
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

        // 2. blob instruction(s): send base64-encoded JPEG data in chunks
        // Format: blob,<stream_len>.<stream>,<base64_len>.<base64_data>;
        let stream_str = stream_id.to_string();
        for chunk in image_data.chunks(BLOB_CHUNK_SIZE) {
            let base64_chunk = base64::engine::general_purpose::STANDARD.encode(chunk);
            let blob_instr = format!(
                "4.blob,{}.{},{}.{};",
                stream_str.len(),
                stream_str,
                base64_chunk.len(),
                base64_chunk
            );
            instructions.push(blob_instr);
        }

        // 3. end instruction: close the stream
        // Format: end,<stream_len>.<stream>;
        let end_instr = format!("3.end,{}.{};", stream_str.len(), stream_str);
        instructions.push(end_instr);

        instructions
    }

    /// Generate size instruction to set display dimensions
    /// CRITICAL: Must be sent before ANY image/drawing operations!
    /// Format: size,<layer>,<width>,<height>;
    pub fn format_size_instruction(layer: i32, width_px: u32, height_px: u32) -> String {
        let layer_str = layer.to_string();
        let width_str = width_px.to_string();
        let height_str = height_px.to_string();
        format!(
            "4.size,{}.{},{}.{},{}.{};",
            layer_str.len(),
            layer_str,
            width_str.len(),
            width_str,
            height_str.len(),
            height_str
        )
    }

    /// Generate sync instruction for frame boundary
    /// Should be sent after all drawing operations for a frame
    pub fn format_sync_instruction(&self, timestamp_ms: u64) -> String {
        let ts_str = timestamp_ms.to_string();
        format!("4.sync,{}.{};", ts_str.len(), ts_str)
    }

    /// Generate instructions to clear a rectangular region with black fill
    ///
    /// This is essential before drawing JPEG content to prevent old content
    /// from showing through (the copy instruction doesn't clear the source region).
    ///
    /// Returns vector of 2 instructions: [rect, cfill]
    ///
    /// # Arguments
    ///
    /// * `row`, `col` - Top-left position in character cells
    /// * `width_chars`, `height_chars` - Size in character cells
    /// * `char_width`, `char_height` - Character cell dimensions in pixels
    /// * `layer` - Layer to operate on (typically 0 for default layer)
    pub fn format_clear_region_instructions(
        row: u16,
        col: u16,
        width_chars: u16,
        height_chars: u16,
        char_width: u32,
        char_height: u32,
        layer: i32,
    ) -> Vec<String> {
        let x = col as u32 * char_width;
        let y = row as u32 * char_height;
        let width_px = width_chars as u32 * char_width;
        let height_px = height_chars as u32 * char_height;

        let layer_str = layer.to_string();
        let x_str = x.to_string();
        let y_str = y.to_string();
        let width_str = width_px.to_string();
        let height_str = height_px.to_string();

        vec![
            // rect instruction: define rectangular path
            format!(
                "4.rect,{}.{},{}.{},{}.{},{}.{},{}.{};",
                layer_str.len(),
                layer_str,
                x_str.len(),
                x_str,
                y_str.len(),
                y_str,
                width_str.len(),
                width_str,
                height_str.len(),
                height_str
            ),
            // cfill instruction: fill with black (RGB 0,0,0)
            "5.cfill,2.15,1.0,1.0,1.0,3.255;".to_string(),
        ]
    }

    /// Generate copy instruction for efficient scrolling
    ///
    /// Copies a rectangular region from one position to another on the same layer.
    /// This is how guacd implements efficient terminal scrolling - it moves existing
    /// content rather than retransmitting it.
    ///
    /// Format: copy,<srclayer>,<srcx>,<srcy>,<srcwidth>,<srcheight>,<mask>,<dstlayer>,<dstx>,<dsty>;
    ///
    /// # Arguments
    ///
    /// * `src_row`, `src_col` - Source position in character cells
    /// * `width_chars`, `height_chars` - Size of region in character cells
    /// * `dst_row`, `dst_col` - Destination position in character cells
    /// * `char_width`, `char_height` - Character cell dimensions in pixels
    /// * `layer` - Layer to operate on (typically 0 for default layer)
    #[allow(clippy::too_many_arguments)]
    pub fn format_copy_instruction(
        src_row: u16,
        src_col: u16,
        width_chars: u16,
        height_chars: u16,
        dst_row: u16,
        dst_col: u16,
        char_width: u32,
        char_height: u32,
        layer: i32,
    ) -> String {
        // Convert character positions to pixel coordinates
        let src_x = src_col as u32 * char_width;
        let src_y = src_row as u32 * char_height;
        let width_px = width_chars as u32 * char_width;
        let height_px = height_chars as u32 * char_height;
        let dst_x = dst_col as u32 * char_width;
        let dst_y = dst_row as u32 * char_height;

        let layer_str = layer.to_string();
        let src_x_str = src_x.to_string();
        let src_y_str = src_y.to_string();
        let width_str = width_px.to_string();
        let height_str = height_px.to_string();
        let mask_str = "15"; // RGBA channels (0x0F)
        let dst_x_str = dst_x.to_string();
        let dst_y_str = dst_y.to_string();

        format!(
            "4.copy,{}.{},{}.{},{}.{},{}.{},{}.{},{},{}.{},{}.{},{}.{};",
            layer_str.len(),
            layer_str,
            src_x_str.len(),
            src_x_str,
            src_y_str.len(),
            src_y_str,
            width_str.len(),
            width_str,
            height_str.len(),
            height_str,
            mask_str,
            layer_str.len(),
            layer_str,
            dst_x_str.len(),
            dst_x_str,
            dst_y_str.len(),
            dst_y_str
        )
    }

    /// Generate args instruction for Guacamole handshake
    /// Lists the parameter names the protocol accepts
    pub fn format_args_instruction(param_names: &[&str]) -> String {
        let mut result = String::from("4.args");
        for name in param_names {
            result.push(',');
            result.push_str(&name.len().to_string());
            result.push('.');
            result.push_str(name);
        }
        result.push(';');
        result
    }

    /// Generate ready instruction for Guacamole handshake
    /// Signals that the connection is ready to accept data
    pub fn format_ready_instruction(connection_id: &str) -> String {
        let cid_str = connection_id;
        format!("5.ready,{}.{};", cid_str.len(), cid_str)
    }

    /// Generate argv/blob/end sequence to send a parameter value
    ///
    /// Guacd sends these after handshake to acknowledge received parameters.
    /// Returns vec of 3 instructions: [argv, blob, end]
    pub fn format_argv_instructions(
        stream_id: u32,
        param_name: &str,
        param_value: &str,
    ) -> Vec<String> {
        use base64::Engine;

        let mut instructions = Vec::new();
        let stream_str = stream_id.to_string();

        // 1. argv instruction: allocate stream for parameter
        // Format: argv,<stream>,<mimetype>,<param_name>;
        let argv_instr = format!(
            "4.argv,{}.{},10.text/plain,{}.{};",
            stream_str.len(),
            stream_str,
            param_name.len(),
            param_name
        );
        instructions.push(argv_instr);

        // 2. blob instruction: send base64-encoded value
        let base64_value = base64::engine::general_purpose::STANDARD.encode(param_value.as_bytes());
        let blob_instr = format!(
            "4.blob,{}.{},{}.{};",
            stream_str.len(),
            stream_str,
            base64_value.len(),
            base64_value
        );
        instructions.push(blob_instr);

        // 3. end instruction: close stream
        let end_instr = format!("3.end,{}.{};", stream_str.len(), stream_str);
        instructions.push(end_instr);

        instructions
    }

    /// Legacy method for backward compatibility - now uses streaming
    #[deprecated(note = "Use format_img_instructions instead for proper Guacamole protocol")]
    pub fn format_img_instruction(&self, image_data: &[u8]) -> String {
        // This was sending the entire image in one instruction, which is wrong
        // Keeping for now but should migrate to format_img_instructions
        use base64::Engine;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(image_data);

        let opcode = "img";
        let args = vec!["0", "image/jpeg", &base64_data];

        let mut result = String::new();
        result.push_str(&opcode.len().to_string());
        result.push('.');
        result.push_str(opcode);

        for arg in args {
            result.push(',');
            result.push_str(&arg.len().to_string());
            result.push('.');
            result.push_str(arg);
        }

        result.push(';');
        result
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
    fn test_render_empty_screen() {
        let renderer = TerminalRenderer::new().unwrap();
        let parser = vt100::Parser::new(24, 80, 0);
        let screen = parser.screen();

        let jpeg = renderer.render_screen(screen, 24, 80);
        assert!(jpeg.is_ok());

        let jpeg_data = jpeg.unwrap();
        assert!(!jpeg_data.is_empty());
        // Check JPEG magic bytes (FF D8 FF)
        assert_eq!(&jpeg_data[0..3], &[0xFF, 0xD8, 0xFF]);
    }

    #[test]
    fn test_render_with_text() {
        let renderer = TerminalRenderer::new().unwrap();
        let mut parser = vt100::Parser::new(24, 80, 0);

        parser.process(b"Hello, Terminal!\n");
        let screen = parser.screen();

        let jpeg = renderer.render_screen(screen, 24, 80);
        assert!(jpeg.is_ok());

        let jpeg_data = jpeg.unwrap();
        assert!(!jpeg_data.is_empty());
        assert!(jpeg_data.len() > 100); // Should have substantial data
    }

    #[test]
    fn test_format_img_instruction() {
        let renderer = TerminalRenderer::new().unwrap();
        let jpeg_data = vec![0xFF, 0xD8, 0xFF]; // JPEG header

        #[allow(deprecated)]
        let instruction = renderer.format_img_instruction(&jpeg_data);

        assert!(instruction.starts_with("3.img,"));
        assert!(instruction.ends_with(';'));
        assert!(instruction.contains("image/jpeg"));
    }
}
