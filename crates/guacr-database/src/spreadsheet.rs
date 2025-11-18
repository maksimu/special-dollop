// Spreadsheet-style renderer for SQL query results
//
// Better than whoDB because:
// - Pure Rust (no Go subprocess)
// - Renders directly to PNG (no browser needed)
// - Lightweight (10-20MB vs 250MB+ for whoDB)
// - Integrated with guacr (no external dependencies)
// - Customizable for our exact needs

use image::{Rgba, RgbaImage};
use std::io::Cursor;

const CELL_WIDTH: u32 = 120;
const CELL_HEIGHT: u32 = 24;
const HEADER_HEIGHT: u32 = 30;
const SCROLLBAR_WIDTH: u32 = 20;

pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Spreadsheet-style view for SQL query results
pub struct SpreadsheetView {
    result: QueryResult,
    scroll_offset: usize,
    selected_cell: Option<(usize, usize)>,
    viewport_rows: usize,
    viewport_cols: usize,
}

impl SpreadsheetView {
    pub fn new(result: QueryResult) -> Self {
        Self {
            result,
            scroll_offset: 0,
            selected_cell: None,
            viewport_rows: 20, // Show 20 rows at a time
            viewport_cols: 10, // Show up to 10 columns
        }
    }

    /// Render spreadsheet to PNG image
    pub fn render_to_png(&self, width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
        let mut img = RgbaImage::new(width, height);

        // Background - white
        for pixel in img.pixels_mut() {
            *pixel = Rgba([255, 255, 255, 255]);
        }

        // Draw column headers
        self.draw_headers(&mut img)?;

        // Draw data rows
        self.draw_data_rows(&mut img)?;

        // Draw grid lines
        self.draw_grid(&mut img)?;

        // Draw selection highlight
        if let Some((row, col)) = self.selected_cell {
            self.draw_selection(&mut img, row, col)?;
        }

        // Draw scrollbar if needed
        if self.result.rows.len() > self.viewport_rows {
            self.draw_scrollbar(&mut img)?;
        }

        // Encode to PNG
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), image::ImageFormat::Png)?;

        Ok(png_data)
    }

    fn draw_headers(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let header_bg = Rgba([200, 200, 200, 255]); // Light gray
        let text_color = Rgba([0, 0, 0, 255]); // Black

        for (col_idx, column) in self
            .result
            .columns
            .iter()
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

            // Draw column name (simplified - just draw blocks for now)
            // TODO: Add actual text rendering with font
            self.draw_text_simplified(img, column, x + 5, 5, text_color)?;
        }

        Ok(())
    }

    fn draw_data_rows(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let cell_bg = Rgba([255, 255, 255, 255]); // White
        let alt_bg = Rgba([245, 245, 245, 255]); // Very light gray
        let text_color = Rgba([0, 0, 0, 255]); // Black

        let visible_rows = self
            .result
            .rows
            .iter()
            .skip(self.scroll_offset)
            .take(self.viewport_rows);

        for (row_idx, row) in visible_rows.enumerate() {
            let y = HEADER_HEIGHT + (row_idx as u32 * CELL_HEIGHT);
            let bg = if row_idx % 2 == 0 { cell_bg } else { alt_bg };

            for (col_idx, value) in row.iter().enumerate().take(self.viewport_cols) {
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
                self.draw_text_simplified(img, value, x + 5, y + 5, text_color)?;
            }
        }

        Ok(())
    }

    fn draw_grid(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let grid_color = Rgba([200, 200, 200, 255]);

        // Vertical lines
        for col_idx in 0..=self.viewport_cols.min(self.result.columns.len()) {
            let x = col_idx as u32 * CELL_WIDTH;
            for y in 0..img.height() {
                if x < img.width() {
                    img.put_pixel(x, y, grid_color);
                }
            }
        }

        // Horizontal lines
        let max_rows = self
            .viewport_rows
            .min(self.result.rows.len() - self.scroll_offset);
        for row_idx in 0..=max_rows {
            let y = HEADER_HEIGHT + (row_idx as u32 * CELL_HEIGHT);
            for x in 0..img.width() {
                if y < img.height() {
                    img.put_pixel(x, y, grid_color);
                }
            }
        }

        Ok(())
    }

    fn draw_selection(
        &self,
        img: &mut RgbaImage,
        row: usize,
        col: usize,
    ) -> Result<(), image::ImageError> {
        let highlight = Rgba([100, 150, 255, 100]); // Semi-transparent blue

        if row >= self.scroll_offset
            && row < self.scroll_offset + self.viewport_rows
            && col < self.viewport_cols
        {
            let visible_row = row - self.scroll_offset;
            let x = col as u32 * CELL_WIDTH;
            let y = HEADER_HEIGHT + (visible_row as u32 * CELL_HEIGHT);

            // Draw selection overlay
            for py in 0..CELL_HEIGHT {
                for px in 0..CELL_WIDTH {
                    let pixel_x = x + px;
                    let pixel_y = y + py;
                    if pixel_x < img.width() && pixel_y < img.height() {
                        // Alpha blend with existing pixel
                        let existing = img.get_pixel(pixel_x, pixel_y);
                        let blended = alpha_blend(existing, &highlight);
                        img.put_pixel(pixel_x, pixel_y, blended);
                    }
                }
            }
        }

        Ok(())
    }

    fn draw_scrollbar(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let scrollbar_bg = Rgba([220, 220, 220, 255]);
        let scrollbar_thumb = Rgba([150, 150, 150, 255]);

        let x = img.width() - SCROLLBAR_WIDTH;
        let scrollbar_height = img.height() - HEADER_HEIGHT;

        // Draw scrollbar background
        for py in HEADER_HEIGHT..img.height() {
            for px in 0..SCROLLBAR_WIDTH {
                let pixel_x = x + px;
                if pixel_x < img.width() {
                    img.put_pixel(pixel_x, py, scrollbar_bg);
                }
            }
        }

        // Calculate thumb position and size
        let total_rows = self.result.rows.len();
        let thumb_size =
            (self.viewport_rows as f32 / total_rows as f32 * scrollbar_height as f32) as u32;
        let thumb_pos =
            (self.scroll_offset as f32 / total_rows as f32 * scrollbar_height as f32) as u32;

        // Draw thumb
        for py in thumb_pos..(thumb_pos + thumb_size).min(img.height()) {
            for px in 2..(SCROLLBAR_WIDTH - 2) {
                let pixel_x = x + px;
                let pixel_y = HEADER_HEIGHT + py;
                if pixel_x < img.width() && pixel_y < img.height() {
                    img.put_pixel(pixel_x, pixel_y, scrollbar_thumb);
                }
            }
        }

        Ok(())
    }

    fn draw_text_simplified(
        &self,
        img: &mut RgbaImage,
        text: &str,
        x: u32,
        y: u32,
        color: Rgba<u8>,
    ) -> Result<(), image::ImageError> {
        // Simplified text rendering - draw colored blocks to represent text
        // TODO: Add proper font rendering later
        for (i, _c) in text.chars().take(10).enumerate() {
            let char_x = x + (i as u32 * 8);
            for py in 0..12 {
                for px in 0..6 {
                    let pixel_x = char_x + px;
                    let pixel_y = y + py;
                    if pixel_x < img.width() && pixel_y < img.height() {
                        img.put_pixel(pixel_x, pixel_y, color);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn handle_click(&mut self, x: u32, y: u32) {
        if y < HEADER_HEIGHT {
            // Click in header - could sort by column
            return;
        }

        let col = (x / CELL_WIDTH) as usize;
        let row = ((y - HEADER_HEIGHT) / CELL_HEIGHT) as usize + self.scroll_offset;

        if col < self.result.columns.len() && row < self.result.rows.len() {
            self.selected_cell = Some((row, col));
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        let new_offset = (self.scroll_offset as i32 + delta).max(0) as usize;
        let max_offset = self.result.rows.len().saturating_sub(self.viewport_rows);
        self.scroll_offset = new_offset.min(max_offset);
    }

    pub fn get_selected_value(&self) -> Option<String> {
        if let Some((row, col)) = self.selected_cell {
            self.result.rows.get(row)?.get(col).cloned()
        } else {
            None
        }
    }

    pub fn export_csv(&self) -> String {
        let mut csv = String::new();

        // Header
        csv.push_str(&self.result.columns.join(","));
        csv.push('\n');

        // Rows
        for row in &self.result.rows {
            csv.push_str(&row.join(","));
            csv.push('\n');
        }

        csv
    }
}

fn alpha_blend(base: &Rgba<u8>, overlay: &Rgba<u8>) -> Rgba<u8> {
    let alpha = overlay[3] as f32 / 255.0;
    let inv_alpha = 1.0 - alpha;

    Rgba([
        (base[0] as f32 * inv_alpha + overlay[0] as f32 * alpha) as u8,
        (base[1] as f32 * inv_alpha + overlay[1] as f32 * alpha) as u8,
        (base[2] as f32 * inv_alpha + overlay[2] as f32 * alpha) as u8,
        255,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spreadsheet_new() {
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![
                vec!["1".to_string(), "Alice".to_string()],
                vec!["2".to_string(), "Bob".to_string()],
            ],
        };

        let view = SpreadsheetView::new(result);
        assert_eq!(view.result.columns.len(), 2);
        assert_eq!(view.result.rows.len(), 2);
    }

    #[test]
    fn test_render_to_png() {
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![vec!["1".to_string(), "Alice".to_string()]],
        };

        let view = SpreadsheetView::new(result);
        let png = view.render_to_png(800, 600);

        assert!(png.is_ok());
        let png_data = png.unwrap();
        assert!(!png_data.is_empty());
        assert_eq!(&png_data[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn test_handle_click() {
        let result = QueryResult {
            columns: vec!["id".to_string()],
            rows: vec![vec!["1".to_string()]],
        };

        let mut view = SpreadsheetView::new(result);
        view.handle_click(50, 50); // Click in first data cell

        assert_eq!(view.selected_cell, Some((0, 0)));
    }

    #[test]
    fn test_scroll() {
        let result = QueryResult {
            columns: vec!["id".to_string()],
            rows: vec![
                vec!["1".to_string()],
                vec!["2".to_string()],
                vec!["3".to_string()],
            ],
        };

        let mut view = SpreadsheetView::new(result);
        view.viewport_rows = 2;

        view.scroll(1);
        assert_eq!(view.scroll_offset, 1);

        view.scroll(-1);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_export_csv() {
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![
                vec!["1".to_string(), "Alice".to_string()],
                vec!["2".to_string(), "Bob".to_string()],
            ],
        };

        let view = SpreadsheetView::new(result);
        let csv = view.export_csv();

        assert!(csv.contains("id,name"));
        assert!(csv.contains("1,Alice"));
        assert!(csv.contains("2,Bob"));
    }

    #[test]
    fn test_get_selected_value() {
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![vec!["1".to_string(), "Alice".to_string()]],
        };

        let mut view = SpreadsheetView::new(result);
        view.selected_cell = Some((0, 1));

        let value = view.get_selected_value();
        assert_eq!(value, Some("Alice".to_string()));
    }
}
