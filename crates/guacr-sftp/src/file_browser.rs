// File browser UI renderer for SFTP

use image::{Rgba, RgbaImage};
use std::io::Cursor;

pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
    pub permissions: String,
    pub modified: String,
}

/// File browser view (spreadsheet-like for files)
pub struct FileBrowser {
    #[allow(dead_code)]
    current_path: String,
    entries: Vec<FileEntry>,
    selected_index: Option<usize>,
    scroll_offset: usize,
}

impl FileBrowser {
    pub fn new(path: String, entries: Vec<FileEntry>) -> Self {
        Self {
            current_path: path,
            entries,
            selected_index: None,
            scroll_offset: 0,
        }
    }

    pub fn render_to_png(&self, width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
        let mut img = RgbaImage::new(width, height);

        // Background - white
        for pixel in img.pixels_mut() {
            *pixel = Rgba([255, 255, 255, 255]);
        }

        // Draw path bar
        self.draw_path_bar(&mut img)?;

        // Draw file list (like spreadsheet)
        self.draw_file_list(&mut img)?;

        // Encode to PNG
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), image::ImageFormat::Png)?;

        Ok(png_data)
    }

    fn draw_path_bar(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let bg = Rgba([240, 240, 240, 255]);

        // Draw path bar background
        for y in 0..30 {
            for x in 0..img.width() {
                img.put_pixel(x, y, bg);
            }
        }

        // TODO: Draw path text
        Ok(())
    }

    fn draw_file_list(&self, img: &mut RgbaImage) -> Result<(), image::ImageError> {
        let row_height = 24u32;
        let visible_entries = self.entries.iter().skip(self.scroll_offset).take(20);

        for (idx, _entry) in visible_entries.enumerate() {
            let y = 30 + (idx as u32 * row_height);
            let bg = if Some(idx + self.scroll_offset) == self.selected_index {
                Rgba([200, 220, 255, 255]) // Selected - blue
            } else if idx % 2 == 0 {
                Rgba([255, 255, 255, 255]) // White
            } else {
                Rgba([248, 248, 248, 255]) // Light gray
            };

            // Draw row background
            for py in 0..row_height {
                for x in 0..img.width() {
                    img.put_pixel(x, y + py, bg);
                }
            }

            // TODO: Draw file icon, name, size, date
        }

        Ok(())
    }

    pub fn handle_click(&mut self, y: u32) {
        if y < 30 {
            // Click in path bar
            return;
        }

        let row = ((y - 30) / 24) as usize;
        let index = row + self.scroll_offset;

        if index < self.entries.len() {
            self.selected_index = Some(index);
        }
    }

    pub fn get_selected(&self) -> Option<&FileEntry> {
        self.selected_index.and_then(|i| self.entries.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_browser_new() {
        let entries = vec![FileEntry {
            name: "file1.txt".to_string(),
            size: 1024,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01".to_string(),
        }];

        let browser = FileBrowser::new("/home/user".to_string(), entries);
        assert_eq!(browser.current_path, "/home/user");
        assert_eq!(browser.entries.len(), 1);
    }

    #[test]
    fn test_file_browser_render() {
        let browser = FileBrowser::new("/".to_string(), vec![]);
        let png = browser.render_to_png(800, 600);

        assert!(png.is_ok());
        let png_data = png.unwrap();
        assert!(!png_data.is_empty());
    }

    #[test]
    fn test_file_browser_selection() {
        let entries = vec![FileEntry {
            name: "file1.txt".to_string(),
            size: 1024,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01".to_string(),
        }];

        let mut browser = FileBrowser::new("/".to_string(), entries);
        browser.handle_click(50); // Click first file

        assert_eq!(browser.selected_index, Some(0));
        assert!(browser.get_selected().is_some());
    }
}
