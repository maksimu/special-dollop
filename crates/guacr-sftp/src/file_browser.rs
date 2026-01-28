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

    #[test]
    fn test_file_browser_multiple_entries() {
        let entries = vec![
            FileEntry {
                name: "file1.txt".to_string(),
                size: 1024,
                is_directory: false,
                permissions: "rw-r--r--".to_string(),
                modified: "2024-01-01".to_string(),
            },
            FileEntry {
                name: "dir1".to_string(),
                size: 0,
                is_directory: true,
                permissions: "drwxr-xr-x".to_string(),
                modified: "2024-01-02".to_string(),
            },
            FileEntry {
                name: "file2.txt".to_string(),
                size: 2048,
                is_directory: false,
                permissions: "rw-r--r--".to_string(),
                modified: "2024-01-03".to_string(),
            },
        ];

        let mut browser = FileBrowser::new("/".to_string(), entries);

        // Click second entry (directory)
        browser.handle_click(54); // y = 30 + 24 = 54 (second row)
        assert_eq!(browser.selected_index, Some(1));
        assert_eq!(browser.get_selected().unwrap().name, "dir1");
        assert!(browser.get_selected().unwrap().is_directory);

        // Click third entry
        browser.handle_click(78); // y = 30 + 48 = 78 (third row)
        assert_eq!(browser.selected_index, Some(2));
        assert_eq!(browser.get_selected().unwrap().name, "file2.txt");
    }

    #[test]
    fn test_file_browser_click_path_bar() {
        let entries = vec![FileEntry {
            name: "file1.txt".to_string(),
            size: 1024,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01".to_string(),
        }];

        let mut browser = FileBrowser::new("/".to_string(), entries);
        browser.handle_click(50); // Select first file
        assert_eq!(browser.selected_index, Some(0));

        // Click in path bar (y < 30) should not change selection
        browser.handle_click(20);
        assert_eq!(browser.selected_index, Some(0)); // Unchanged
    }

    #[test]
    fn test_file_browser_click_out_of_bounds() {
        let entries = vec![FileEntry {
            name: "file1.txt".to_string(),
            size: 1024,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01".to_string(),
        }];

        let mut browser = FileBrowser::new("/".to_string(), entries);

        // Click beyond available entries
        browser.handle_click(1000); // Way beyond
        assert_eq!(browser.selected_index, None);
    }

    #[test]
    fn test_file_entry_structure() {
        let entry = FileEntry {
            name: "test.txt".to_string(),
            size: 12345,
            is_directory: false,
            permissions: "rw-r--r--".to_string(),
            modified: "2024-01-01 12:00".to_string(),
        };

        assert_eq!(entry.name, "test.txt");
        assert_eq!(entry.size, 12345);
        assert!(!entry.is_directory);
        assert_eq!(entry.permissions, "rw-r--r--");
    }

    #[test]
    fn test_file_entry_directory() {
        let entry = FileEntry {
            name: "mydir".to_string(),
            size: 0,
            is_directory: true,
            permissions: "drwxr-xr-x".to_string(),
            modified: "2024-01-01".to_string(),
        };

        assert!(entry.is_directory);
        assert_eq!(entry.size, 0);
        assert!(entry.permissions.starts_with('d'));
    }
}
