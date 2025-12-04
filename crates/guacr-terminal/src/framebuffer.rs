use image::{ImageBuffer, Rgba, RgbaImage};
use std::io::Cursor;

type Result<T> = std::result::Result<T, crate::TerminalError>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl FrameRect {
    pub fn intersects(&self, other: &FrameRect) -> bool {
        !(self.x + self.width <= other.x
            || other.x + other.width <= self.x
            || self.y + self.height <= other.y
            || other.y + other.height <= self.y)
    }

    pub fn union(&self, other: &FrameRect) -> FrameRect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);

        FrameRect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        }
    }
}

/// Framebuffer for RDP/VNC screen updates
pub struct FrameBuffer {
    width: u32,
    height: u32,
    data: Vec<u8>, // RGBA pixels
    dirty_rects: Vec<FrameRect>,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
            dirty_rects: Vec::new(),
        }
    }

    /// Update a region of the framebuffer
    pub fn update_region(&mut self, x: u32, y: u32, width: u32, height: u32, pixels: &[u8]) {
        for row in 0..height {
            let dst_offset = ((y + row) * self.width + x) as usize * 4;
            let src_offset = (row * width) as usize * 4;
            let len = (width * 4) as usize;

            if dst_offset + len <= self.data.len() && src_offset + len <= pixels.len() {
                self.data[dst_offset..dst_offset + len]
                    .copy_from_slice(&pixels[src_offset..src_offset + len]);
            }
        }

        self.dirty_rects.push(FrameRect {
            x,
            y,
            width,
            height,
        });
    }

    /// Get dirty rectangles
    pub fn dirty_rects(&self) -> &[FrameRect] {
        &self.dirty_rects
    }

    /// Get all pixels from framebuffer (for hardware encoding)
    pub fn get_all_pixels(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Optimize dirty rectangles by merging overlapping ones
    pub fn optimize_dirty_rects(&mut self) {
        if self.dirty_rects.len() < 2 {
            return;
        }

        self.dirty_rects.sort_by_key(|r| (r.y, r.x));

        let mut merged = Vec::new();
        let mut current = self.dirty_rects[0];

        for rect in &self.dirty_rects[1..] {
            if current.intersects(rect) {
                current = current.union(rect);
            } else {
                merged.push(current);
                current = *rect;
            }
        }
        merged.push(current);

        self.dirty_rects = merged;
    }

    /// Get pixels for a region (for hardware encoding)
    pub fn get_region_pixels(&self, rect: FrameRect) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((rect.width * rect.height * 4) as usize);

        for row in 0..rect.height {
            for col in 0..rect.width {
                let src_x = rect.x + col;
                let src_y = rect.y + row;

                if src_x < self.width && src_y < self.height {
                    let src_offset = (src_y * self.width + src_x) as usize * 4;
                    if src_offset + 4 <= self.data.len() {
                        pixels.extend_from_slice(&self.data[src_offset..src_offset + 4]);
                    } else {
                        // Out of bounds - add transparent pixel
                        pixels.extend_from_slice(&[0, 0, 0, 0]);
                    }
                } else {
                    // Out of bounds - add transparent pixel
                    pixels.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }

        pixels
    }

    /// Encode a region to PNG
    pub fn encode_region(&self, rect: FrameRect) -> Result<Vec<u8>> {
        let mut img: RgbaImage = ImageBuffer::new(rect.width, rect.height);

        for row in 0..rect.height {
            for col in 0..rect.width {
                let src_x = rect.x + col;
                let src_y = rect.y + row;

                if src_x < self.width && src_y < self.height {
                    let src_offset = (src_y * self.width + src_x) as usize * 4;

                    if src_offset + 4 <= self.data.len() {
                        let pixel = Rgba([
                            self.data[src_offset],
                            self.data[src_offset + 1],
                            self.data[src_offset + 2],
                            self.data[src_offset + 3],
                        ]);
                        img.put_pixel(col, row, pixel);
                    }
                }
            }
        }

        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), image::ImageFormat::Png)?;

        Ok(png_data)
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_rects.clear();
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_framebuffer_new() {
        let fb = FrameBuffer::new(1920, 1080);
        assert_eq!(fb.size(), (1920, 1080));
        assert_eq!(fb.dirty_rects().len(), 0);
    }

    #[test]
    fn test_update_region() {
        let mut fb = FrameBuffer::new(100, 100);
        let pixels = vec![255u8; 10 * 10 * 4]; // 10x10 white pixels

        fb.update_region(10, 10, 10, 10, &pixels);

        assert_eq!(fb.dirty_rects().len(), 1);
        assert_eq!(fb.dirty_rects()[0].x, 10);
        assert_eq!(fb.dirty_rects()[0].y, 10);
        assert_eq!(fb.dirty_rects()[0].width, 10);
        assert_eq!(fb.dirty_rects()[0].height, 10);
    }

    #[test]
    fn test_rect_intersects() {
        let rect1 = FrameRect {
            x: 10,
            y: 10,
            width: 20,
            height: 20,
        };
        let rect2 = FrameRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        };
        let rect3 = FrameRect {
            x: 50,
            y: 50,
            width: 10,
            height: 10,
        };

        assert!(rect1.intersects(&rect2));
        assert!(!rect1.intersects(&rect3));
    }

    #[test]
    fn test_rect_union() {
        let rect1 = FrameRect {
            x: 10,
            y: 10,
            width: 20,
            height: 20,
        };
        let rect2 = FrameRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        };

        let union = rect1.union(&rect2);

        assert_eq!(union.x, 10);
        assert_eq!(union.y, 10);
        assert_eq!(union.width, 30);
        assert_eq!(union.height, 30);
    }

    #[test]
    fn test_optimize_dirty_rects() {
        let mut fb = FrameBuffer::new(100, 100);
        let pixels = vec![255u8; 10 * 10 * 4];

        fb.update_region(10, 10, 10, 10, &pixels);
        fb.update_region(15, 15, 10, 10, &pixels); // Overlapping

        assert_eq!(fb.dirty_rects().len(), 2);

        fb.optimize_dirty_rects();

        assert_eq!(fb.dirty_rects().len(), 1); // Should be merged
    }

    #[test]
    fn test_encode_region() {
        let mut fb = FrameBuffer::new(100, 100);
        let pixels = vec![255u8; 20 * 20 * 4];

        fb.update_region(10, 10, 20, 20, &pixels);

        let rect = fb.dirty_rects()[0];
        let png = fb.encode_region(rect);

        assert!(png.is_ok());
        let png_data = png.unwrap();
        assert!(!png_data.is_empty());
        // Check PNG signature
        assert_eq!(&png_data[0..8], b"\x89PNG\r\n\x1a\n");
    }
}
