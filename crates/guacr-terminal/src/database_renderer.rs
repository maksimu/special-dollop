// Database result rendering - shared infrastructure for all database handlers
//
// Provides both terminal-style (CLI) and spreadsheet-style (GUI) rendering
// for SQL query results, with Guacamole protocol output.

use crate::{TerminalEmulator, TerminalRenderer};
use image::{Rgb, RgbImage};
use std::io::Cursor;

/// Query result structure - shared by all database handlers
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub affected_rows: Option<u64>,
    pub execution_time_ms: Option<u64>,
}

impl QueryResult {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            affected_rows: None,
            execution_time_ms: None,
        }
    }

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Database terminal - SQL CLI experience
///
/// Wraps TerminalEmulator and TerminalRenderer for consistent database UI.
/// Used by all database handlers for terminal-style interaction.
pub struct DatabaseTerminal {
    terminal: TerminalEmulator,
    renderer: TerminalRenderer,
    prompt: String,
    input_buffer: String,
    db_type: String,
}

impl DatabaseTerminal {
    /// Create a new database terminal with specified prompt
    pub fn new(rows: u16, cols: u16, prompt: &str, db_type: &str) -> crate::Result<Self> {
        Ok(Self {
            terminal: TerminalEmulator::new(rows, cols),
            renderer: TerminalRenderer::new()?,
            prompt: prompt.to_string(),
            input_buffer: String::new(),
            db_type: db_type.to_string(),
        })
    }

    /// Write the prompt to terminal
    pub fn write_prompt(&mut self) -> crate::Result<()> {
        self.terminal.process(self.prompt.as_bytes())?;
        Ok(())
    }

    /// Set a new prompt
    pub fn set_prompt(&mut self, prompt: &str) {
        self.prompt = prompt.to_string();
    }

    /// Get the current prompt
    pub fn get_prompt(&self) -> &str {
        &self.prompt
    }

    /// Process raw bytes (for ANSI escape sequences, etc.)
    pub fn process(&mut self, data: &[u8]) -> crate::Result<()> {
        self.terminal.process(data)?;
        Ok(())
    }

    /// Write a line to terminal with newline
    pub fn write_line(&mut self, text: &str) -> crate::Result<()> {
        let line = format!("{}\r\n", text);
        self.terminal.process(line.as_bytes())?;
        Ok(())
    }

    /// Write an error message (formatted with ERROR: prefix)
    pub fn write_error(&mut self, error: &str) -> crate::Result<()> {
        self.write_line(&format!("ERROR: {}", error))
    }

    /// Write a success message
    pub fn write_success(&mut self, message: &str) -> crate::Result<()> {
        self.write_line(&format!("OK: {}", message))
    }

    /// Write query result as formatted table
    pub fn write_result(&mut self, result: &QueryResult) -> crate::Result<()> {
        if result.columns.is_empty() {
            if let Some(affected) = result.affected_rows {
                self.write_line(&format!("Query OK, {} row(s) affected", affected))?;
            } else {
                self.write_line("Query executed successfully")?;
            }
            return Ok(());
        }

        // Calculate column widths
        let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
        for row in &result.rows {
            for (i, val) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(val.len()).min(50); // Cap at 50 chars
                }
            }
        }

        // Write header
        let header = result
            .columns
            .iter()
            .zip(&widths)
            .map(|(col, width)| format!("{:<width$}", truncate_str(col, *width), width = width))
            .collect::<Vec<_>>()
            .join(" | ");
        self.write_line(&header)?;

        // Write separator
        let separator = widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-");
        self.write_line(&separator)?;

        // Write rows (limit to 100 for terminal display)
        let display_rows = result.rows.iter().take(100);
        for row in display_rows {
            let row_str = row
                .iter()
                .zip(&widths)
                .map(|(val, width)| format!("{:<width$}", truncate_str(val, *width), width = width))
                .collect::<Vec<_>>()
                .join(" | ");
            self.write_line(&row_str)?;
        }

        // Write summary
        if result.rows.len() > 100 {
            self.write_line(&format!(
                "\n({} rows total, showing first 100)",
                result.rows.len()
            ))?;
        } else {
            self.write_line(&format!("\n({} row(s))", result.rows.len()))?;
        }

        if let Some(time_ms) = result.execution_time_ms {
            self.write_line(&format!("Time: {}ms", time_ms))?;
        }

        Ok(())
    }

    /// Add a character to the input buffer and display
    pub fn add_char(&mut self, c: char) -> crate::Result<()> {
        self.input_buffer.push(c);
        self.terminal.process(&[c as u8])?;
        Ok(())
    }

    /// Handle backspace
    pub fn backspace(&mut self) -> crate::Result<()> {
        if !self.input_buffer.is_empty() {
            self.input_buffer.pop();
            self.terminal.process(b"\x08 \x08")?;
        }
        Ok(())
    }

    /// Get and clear the input buffer
    pub fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input_buffer)
    }

    /// Get current input without clearing
    pub fn current_input(&self) -> &str {
        &self.input_buffer
    }

    /// Clear input buffer
    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
    }

    /// Render terminal to JPEG
    pub fn render_jpeg(&mut self) -> crate::Result<Vec<u8>> {
        let (rows, cols) = self.terminal.size();
        let jpeg = self
            .renderer
            .render_screen(self.terminal.screen(), rows, cols)?;
        self.terminal.clear_dirty();
        Ok(jpeg)
    }

    /// Check if terminal needs re-render
    pub fn is_dirty(&self) -> bool {
        self.terminal.is_dirty()
    }

    /// Format Guacamole img instructions for the rendered image
    #[allow(deprecated)]
    pub fn format_img_instructions(&self, jpeg_data: &[u8], stream_id: u32) -> Vec<String> {
        self.renderer
            .format_img_instructions(jpeg_data, stream_id, 0, 0, 0)
    }

    /// Format sync instruction to tell client to display buffered instructions
    pub fn format_sync_instruction(&self, timestamp_ms: u64) -> String {
        self.renderer.format_sync_instruction(timestamp_ms)
    }

    /// Get database type
    pub fn db_type(&self) -> &str {
        &self.db_type
    }

    /// Get terminal size
    pub fn size(&self) -> (u16, u16) {
        self.terminal.size()
    }

    /// Get mutable reference to terminal emulator
    pub fn emulator_mut(&mut self) -> &mut TerminalEmulator {
        &mut self.terminal
    }

    /// Get reference to terminal emulator
    pub fn emulator(&self) -> &TerminalEmulator {
        &self.terminal
    }
}

/// Truncate string to max length with ellipsis
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

// ============================================================================
// Spreadsheet-style rendering (for GUI mode)
// ============================================================================

const CELL_WIDTH: u32 = 120;
const CELL_HEIGHT: u32 = 24;
const HEADER_HEIGHT: u32 = 30;
const CHAR_WIDTH: u32 = 8;

/// Spreadsheet view for visual query results
pub struct SpreadsheetRenderer {
    scroll_row: usize,
    scroll_col: usize,
    selected_cell: Option<(usize, usize)>,
    viewport_rows: usize,
    viewport_cols: usize,
}

impl Default for SpreadsheetRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpreadsheetRenderer {
    pub fn new() -> Self {
        Self {
            scroll_row: 0,
            scroll_col: 0,
            selected_cell: None,
            viewport_rows: 20,
            viewport_cols: 8,
        }
    }

    /// Render query result to PNG
    pub fn render_to_png(
        &self,
        result: &QueryResult,
        width: u32,
        height: u32,
    ) -> crate::Result<Vec<u8>> {
        let mut img = RgbImage::new(width, height);

        // Background
        for pixel in img.pixels_mut() {
            *pixel = Rgb([255, 255, 255]);
        }

        // Draw headers
        self.draw_headers(&mut img, result)?;

        // Draw data rows
        self.draw_rows(&mut img, result)?;

        // Draw grid
        self.draw_grid(&mut img, result)?;

        // Draw selection
        if let Some((row, col)) = self.selected_cell {
            self.draw_selection(&mut img, row, col)?;
        }

        // Encode to PNG
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), image::ImageFormat::Png)?;

        Ok(png_data)
    }

    fn draw_headers(&self, img: &mut RgbImage, result: &QueryResult) -> crate::Result<()> {
        let header_bg = Rgb([220, 220, 220]);
        let text_color = Rgb([0, 0, 0]);

        for (col_idx, column) in result
            .columns
            .iter()
            .skip(self.scroll_col)
            .enumerate()
            .take(self.viewport_cols)
        {
            let x = col_idx as u32 * CELL_WIDTH;

            // Draw header background
            for py in 0..HEADER_HEIGHT {
                for px in 0..CELL_WIDTH {
                    let pixel_x = x + px;
                    if pixel_x < img.width() && py < img.height() {
                        img.put_pixel(pixel_x, py, header_bg);
                    }
                }
            }

            // Draw column name
            self.draw_text(img, column, x + 4, 8, text_color, CELL_WIDTH - 8)?;
        }

        Ok(())
    }

    fn draw_rows(&self, img: &mut RgbImage, result: &QueryResult) -> crate::Result<()> {
        let cell_bg = Rgb([255, 255, 255]);
        let alt_bg = Rgb([248, 248, 248]);
        let text_color = Rgb([0, 0, 0]);

        let visible_rows = result
            .rows
            .iter()
            .skip(self.scroll_row)
            .take(self.viewport_rows);

        for (row_idx, row) in visible_rows.enumerate() {
            let y = HEADER_HEIGHT + (row_idx as u32 * CELL_HEIGHT);
            let bg = if row_idx % 2 == 0 { cell_bg } else { alt_bg };

            for (col_idx, value) in row
                .iter()
                .skip(self.scroll_col)
                .enumerate()
                .take(self.viewport_cols)
            {
                let x = col_idx as u32 * CELL_WIDTH;

                // Draw cell background
                for py in 0..CELL_HEIGHT {
                    for px in 0..CELL_WIDTH {
                        let pixel_x = x + px;
                        let pixel_y = y + py;
                        if pixel_x < img.width() && pixel_y < img.height() {
                            img.put_pixel(pixel_x, pixel_y, bg);
                        }
                    }
                }

                // Draw cell value
                self.draw_text(img, value, x + 4, y + 5, text_color, CELL_WIDTH - 8)?;
            }
        }

        Ok(())
    }

    fn draw_grid(&self, img: &mut RgbImage, result: &QueryResult) -> crate::Result<()> {
        let grid_color = Rgb([200, 200, 200]);
        let visible_cols = result
            .columns
            .len()
            .saturating_sub(self.scroll_col)
            .min(self.viewport_cols);
        let visible_rows = result
            .rows
            .len()
            .saturating_sub(self.scroll_row)
            .min(self.viewport_rows);

        // Vertical lines
        for col_idx in 0..=visible_cols {
            let x = col_idx as u32 * CELL_WIDTH;
            if x < img.width() {
                for y in 0..img.height() {
                    img.put_pixel(x, y, grid_color);
                }
            }
        }

        // Horizontal lines
        for row_idx in 0..=visible_rows {
            let y = HEADER_HEIGHT + (row_idx as u32 * CELL_HEIGHT);
            if y < img.height() {
                for x in 0..img.width() {
                    img.put_pixel(x, y, grid_color);
                }
            }
        }

        Ok(())
    }

    fn draw_selection(&self, img: &mut RgbImage, row: usize, col: usize) -> crate::Result<()> {
        let highlight = Rgb([100, 150, 255]);

        if row >= self.scroll_row
            && row < self.scroll_row + self.viewport_rows
            && col >= self.scroll_col
            && col < self.scroll_col + self.viewport_cols
        {
            let visible_row = row - self.scroll_row;
            let visible_col = col - self.scroll_col;
            let x = visible_col as u32 * CELL_WIDTH;
            let y = HEADER_HEIGHT + (visible_row as u32 * CELL_HEIGHT);

            // Draw selection border (3px)
            for border in 0..3 {
                // Top and bottom
                for px in 0..CELL_WIDTH {
                    let pixel_x = x + px;
                    if pixel_x < img.width() {
                        if y + border < img.height() {
                            img.put_pixel(pixel_x, y + border, highlight);
                        }
                        if y + CELL_HEIGHT - border - 1 < img.height() {
                            img.put_pixel(pixel_x, y + CELL_HEIGHT - border - 1, highlight);
                        }
                    }
                }
                // Left and right
                for py in 0..CELL_HEIGHT {
                    let pixel_y = y + py;
                    if pixel_y < img.height() {
                        if x + border < img.width() {
                            img.put_pixel(x + border, pixel_y, highlight);
                        }
                        if x + CELL_WIDTH - border - 1 < img.width() {
                            img.put_pixel(x + CELL_WIDTH - border - 1, pixel_y, highlight);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn draw_text(
        &self,
        img: &mut RgbImage,
        text: &str,
        x: u32,
        y: u32,
        color: Rgb<u8>,
        max_width: u32,
    ) -> crate::Result<()> {
        let max_chars = (max_width / CHAR_WIDTH) as usize;
        let display_text = if text.len() > max_chars && max_chars > 3 {
            format!("{}...", &text[..max_chars - 3])
        } else {
            text.chars().take(max_chars).collect()
        };

        // Simple bitmap font rendering (5x7 characters)
        for (i, c) in display_text.chars().enumerate() {
            let char_x = x + (i as u32 * CHAR_WIDTH);
            if char_x + CHAR_WIDTH > img.width() {
                break;
            }
            self.draw_char(img, c, char_x, y, color);
        }

        Ok(())
    }

    fn draw_char(&self, img: &mut RgbImage, c: char, x: u32, y: u32, color: Rgb<u8>) {
        // Simple 5x7 bitmap font patterns for common characters
        let pattern = get_char_pattern(c);

        for (row, bits) in pattern.iter().enumerate() {
            for col in 0..5 {
                if (bits >> (4 - col)) & 1 == 1 {
                    let px = x + col;
                    let py = y + row as u32;
                    if px < img.width() && py < img.height() {
                        img.put_pixel(px, py, color);
                    }
                }
            }
        }
    }

    /// Handle mouse click - returns true if selection changed
    pub fn handle_click(&mut self, x: u32, y: u32, result: &QueryResult) -> bool {
        if y < HEADER_HEIGHT {
            return false; // Header click (could sort)
        }

        let col = (x / CELL_WIDTH) as usize + self.scroll_col;
        let row = ((y - HEADER_HEIGHT) / CELL_HEIGHT) as usize + self.scroll_row;

        if col < result.columns.len() && row < result.rows.len() {
            self.selected_cell = Some((row, col));
            true
        } else {
            false
        }
    }

    /// Scroll by delta rows
    pub fn scroll(&mut self, delta: i32, result: &QueryResult) {
        let new_offset = (self.scroll_row as i32 + delta).max(0) as usize;
        let max_offset = result.rows.len().saturating_sub(self.viewport_rows);
        self.scroll_row = new_offset.min(max_offset);
    }

    /// Get selected cell value
    pub fn get_selected_value(&self, result: &QueryResult) -> Option<String> {
        if let Some((row, col)) = self.selected_cell {
            result.rows.get(row)?.get(col).cloned()
        } else {
            None
        }
    }
}

/// Simple 5x7 bitmap font patterns
fn get_char_pattern(c: char) -> [u8; 7] {
    match c {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b01110, 0b00001,
        ],
        'R' => [
            0b11110, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001, 0b10001,
        ],
        'S' => [
            0b01110, 0b10001, 0b10000, 0b01110, 0b00001, 0b10001, 0b01110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b01010, 0b10001,
        ],
        'Y' => [
            0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        'a'..='z' => get_char_pattern((c as u8 - 32) as char), // Reuse uppercase
        '0' => [
            0b01110, 0b10011, 0b10101, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
        ],
        '3' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        ' ' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b01000,
        ],
        ':' => [
            0b00000, 0b00100, 0b00000, 0b00000, 0b00000, 0b00100, 0b00000,
        ],
        ';' => [
            0b00000, 0b00100, 0b00000, 0b00000, 0b00100, 0b00100, 0b01000,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        '[' => [
            0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110,
        ],
        ']' => [
            0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        '|' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '+' => [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
        '=' => [
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ],
        '*' => [
            0b00000, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0b00000,
        ],
        '>' => [
            0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000,
        ],
        '<' => [
            0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010,
        ],
        '!' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
        '?' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100,
        ],
        '@' => [
            0b01110, 0b10001, 0b10111, 0b10101, 0b10111, 0b10000, 0b01110,
        ],
        '#' => [
            0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010,
        ],
        '$' => [
            0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100,
        ],
        '%' => [
            0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011,
        ],
        '&' => [
            0b01100, 0b10010, 0b01100, 0b01000, 0b10101, 0b10010, 0b01101,
        ],
        '\'' => [
            0b00100, 0b00100, 0b01000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        '"' => [
            0b01010, 0b01010, 0b10100, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        _ => [
            0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
        ], // Box for unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_result() {
        let mut result = QueryResult::new(vec!["id".to_string(), "name".to_string()]);
        result.add_row(vec!["1".to_string(), "Alice".to_string()]);
        result.add_row(vec!["2".to_string(), "Bob".to_string()]);

        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.row_count(), 2);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_database_terminal() {
        let term = DatabaseTerminal::new(24, 80, "mysql> ", "mysql");
        assert!(term.is_ok());

        let term = term.unwrap();
        assert_eq!(term.db_type(), "mysql");
        assert_eq!(term.size(), (24, 80));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("ab", 2), "ab");
    }

    #[test]
    fn test_spreadsheet_renderer() {
        let renderer = SpreadsheetRenderer::new();
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![vec!["1".to_string(), "Alice".to_string()]],
            affected_rows: None,
            execution_time_ms: None,
        };

        let png = renderer.render_to_png(&result, 800, 600);
        assert!(png.is_ok());
        let png_data = png.unwrap();
        assert!(!png_data.is_empty());
        assert_eq!(&png_data[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn test_spreadsheet_click() {
        let mut renderer = SpreadsheetRenderer::new();
        let result = QueryResult {
            columns: vec!["id".to_string()],
            rows: vec![vec!["1".to_string()]],
            affected_rows: None,
            execution_time_ms: None,
        };

        let changed = renderer.handle_click(50, 50, &result);
        assert!(changed);
        assert_eq!(renderer.selected_cell, Some((0, 0)));
    }
}
