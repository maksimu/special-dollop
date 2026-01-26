//! RDP Scroll Detection
//!
//! Detects scroll operations in RDP framebuffer to optimize bandwidth using copy instructions.
//! Unlike terminal scroll detection (which uses cell hashing), RDP scroll detection uses
//! pixel-level comparison of framebuffer rows.
//!
//! ## Algorithm
//!
//! 1. Compare current framebuffer with previous framebuffer
//! 2. Detect if most rows shifted up/down by N pixels
//! 3. If scroll detected, use `copy` instruction instead of re-encoding
//! 4. Only encode the new content (scrolled-in region)
//!
//! ## Performance
//!
//! - Scroll up/down: 90%+ bandwidth savings (copy is tiny, only encode new line)
//! - Non-scroll: No overhead (fast row hash comparison)

use std::collections::HashMap;

/// Scroll direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,   // Content moved up (new content at bottom)
    Down, // Content moved down (new content at top)
}

/// Detected scroll operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollOperation {
    pub direction: ScrollDirection,
    pub pixels: u32, // Number of pixels scrolled
}

/// Scroll detector for RDP framebuffer
///
/// Uses row hashing to quickly detect if content shifted vertically.
/// Much faster than full pixel comparison.
pub struct ScrollDetector {
    /// Hash of each row in the previous frame
    row_hashes: Vec<u64>,
    /// Width of framebuffer in pixels
    width: u32,
    /// Height of framebuffer in pixels
    height: u32,
    /// Bytes per pixel (4 for BGRA32)
    bytes_per_pixel: u32,
}

impl ScrollDetector {
    /// Create a new scroll detector
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            row_hashes: vec![0; height as usize],
            width,
            height,
            bytes_per_pixel: 4, // BGRA32
        }
    }

    /// Detect scroll operation by comparing current framebuffer with previous
    ///
    /// Returns Some(ScrollOperation) if scrolling detected, None otherwise.
    ///
    /// ## Algorithm
    ///
    /// 1. Hash each row of current framebuffer
    /// 2. Compare with previous row hashes to find matches
    /// 3. If most rows shifted by same amount, it's a scroll
    /// 4. Update row hashes for next frame
    ///
    /// ## Heuristics
    ///
    /// - Must have >70% row matches (not a full screen change)
    /// - Scroll distance must be consistent (same shift for most rows)
    /// - Scroll distance must be reasonable (1-50% of screen height)
    pub fn detect_scroll(&mut self, framebuffer: &[u8]) -> Option<ScrollOperation> {
        if framebuffer.len() != (self.width * self.height * self.bytes_per_pixel) as usize {
            return None;
        }

        // Hash each row of current framebuffer
        let mut current_hashes = Vec::with_capacity(self.height as usize);
        for row in 0..self.height {
            let hash = self.hash_row(framebuffer, row);
            current_hashes.push(hash);
        }

        // Find where each current row came from in previous frame
        // Map: current_row -> previous_row
        let mut matches: HashMap<u32, u32> = HashMap::new();

        for current_row in 0..self.height {
            let current_hash = current_hashes[current_row as usize];

            // Look for matching row in previous frame (within reasonable scroll distance)
            let max_scroll = (self.height / 2).min(500); // Max 50% of screen or 500 pixels

            for prev_row in 0..self.height {
                // Only consider matches within reasonable scroll distance
                let distance = (current_row as i32 - prev_row as i32).unsigned_abs();
                if distance > max_scroll {
                    continue;
                }

                if current_hash == self.row_hashes[prev_row as usize] && current_hash != 0 {
                    matches.insert(current_row, prev_row);
                    break;
                }
            }
        }

        // Analyze matches to detect scroll
        let scroll_op = self.analyze_matches(&matches);

        // Update row hashes for next frame
        self.row_hashes = current_hashes;

        scroll_op
    }

    /// Analyze row matches to detect scroll operation
    ///
    /// Returns Some(ScrollOperation) if consistent scroll detected.
    fn analyze_matches(&self, matches: &HashMap<u32, u32>) -> Option<ScrollOperation> {
        if matches.is_empty() {
            return None;
        }

        // Calculate shift for each match (current_row - prev_row)
        // Negative shift = scroll up (content moved up, row 0 now has what was in row 20)
        // Positive shift = scroll down (content moved down, row 20 now has what was in row 0)
        let mut shifts: HashMap<i32, u32> = HashMap::new(); // shift -> count

        for (current_row, prev_row) in matches.iter() {
            let shift = *current_row as i32 - *prev_row as i32;
            *shifts.entry(shift).or_insert(0) += 1;
        }

        // Find most common shift
        let (most_common_shift, shift_count) = shifts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(shift, count)| (*shift, *count))?;

        // Check if enough rows have the same shift (>70% of matched rows)
        let match_percentage = (shift_count * 100) / matches.len() as u32;
        if match_percentage < 70 {
            return None; // Not a consistent scroll
        }

        // Check if enough total rows matched (>50% of screen)
        let total_match_percentage = (matches.len() * 100) / self.height as usize;
        if total_match_percentage < 50 {
            return None; // Too much changed, not a scroll
        }

        // Check if shift is reasonable (not zero, not too large)
        if most_common_shift == 0 {
            return None; // No shift
        }

        let shift_abs = most_common_shift.unsigned_abs();
        let max_reasonable_shift = (self.height / 2).min(500);
        if shift_abs > max_reasonable_shift {
            return None; // Shift too large
        }

        // Determine direction
        // Negative shift = current row is above prev row = content moved up = scroll up
        // Positive shift = current row is below prev row = content moved down = scroll down
        let direction = if most_common_shift < 0 {
            ScrollDirection::Up
        } else {
            ScrollDirection::Down
        };

        Some(ScrollOperation {
            direction,
            pixels: shift_abs,
        })
    }

    /// Hash a single row of the framebuffer
    ///
    /// Uses sampling to avoid hashing every pixel (too slow).
    /// Samples every 8th pixel for better uniqueness while staying fast.
    fn hash_row(&self, framebuffer: &[u8], row: u32) -> u64 {
        let stride = self.width * self.bytes_per_pixel;
        let row_offset = (row * stride) as usize;

        if row_offset >= framebuffer.len() {
            return 0;
        }

        let row_end = (row_offset + stride as usize).min(framebuffer.len());
        let row_data = &framebuffer[row_offset..row_end];

        // Sample every 8th pixel (4 bytes per pixel, so every 32 bytes)
        // This gives better uniqueness than every 16th pixel
        const SAMPLE_STRIDE: usize = 32;

        let mut hash: u64 = 0;
        let mut i = 0;
        let mut sample_count = 0;
        while i < row_data.len() {
            // Hash 4 bytes at once (BGRA)
            if i + 3 < row_data.len() {
                let pixel = u32::from_le_bytes([
                    row_data[i],
                    row_data[i + 1],
                    row_data[i + 2],
                    row_data[i + 3],
                ]);
                // Use a better hash function with position weighting
                hash = hash.wrapping_mul(31).wrapping_add(pixel as u64);
                hash = hash.wrapping_mul(17).wrapping_add(sample_count);
                sample_count += 1;
            }
            i += SAMPLE_STRIDE;
        }

        // Include row length to catch size changes
        hash = hash.wrapping_mul(31).wrapping_add(row_data.len() as u64);

        hash
    }

    /// Reset detector (e.g., after screen resize)
    pub fn reset(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.row_hashes = vec![0; height as usize];
    }

    /// Get dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test framebuffer filled with a pattern
    fn create_test_framebuffer(width: u32, height: u32, pattern: u8) -> Vec<u8> {
        let size = (width * height * 4) as usize;
        let mut buffer = vec![0u8; size];

        // Fill with pattern (VERY different for each row to avoid false matches)
        for row in 0..height {
            let row_offset = (row * width * 4) as usize;
            // Use row number prominently in the pattern to make rows VERY distinct
            let row_id = row.to_le_bytes();
            for pixel in 0..width {
                let pixel_offset = row_offset + (pixel * 4) as usize;
                // Encode row number in every pixel so rows are unmistakably unique
                buffer[pixel_offset] = row_id[0].wrapping_add(pattern); // B
                buffer[pixel_offset + 1] = row_id[1].wrapping_add(pattern); // G
                buffer[pixel_offset + 2] = row_id[2].wrapping_add(pattern); // R
                buffer[pixel_offset + 3] = row_id[3].wrapping_add(pattern); // A
            }
        }

        buffer
    }

    /// Create a scrolled version of a framebuffer
    fn scroll_framebuffer(
        original: &[u8],
        width: u32,
        height: u32,
        scroll_pixels: u32,
        direction: ScrollDirection,
    ) -> Vec<u8> {
        let stride = (width * 4) as usize;
        let mut scrolled = vec![0u8; original.len()];

        match direction {
            ScrollDirection::Up => {
                // Content moves up: copy rows [scroll_pixels..height] to [0..height-scroll_pixels]
                for row in scroll_pixels..height {
                    let src_offset = (row * width * 4) as usize;
                    let dst_offset = ((row - scroll_pixels) * width * 4) as usize;
                    scrolled[dst_offset..dst_offset + stride]
                        .copy_from_slice(&original[src_offset..src_offset + stride]);
                }
                // New content at bottom (fill with different pattern)
                for row in (height - scroll_pixels)..height {
                    let offset = (row * width * 4) as usize;
                    for i in 0..stride {
                        scrolled[offset + i] = 0xFF; // New content
                    }
                }
            }
            ScrollDirection::Down => {
                // Content moves down: copy rows [0..height-scroll_pixels] to [scroll_pixels..height]
                for row in 0..(height - scroll_pixels) {
                    let src_offset = (row * width * 4) as usize;
                    let dst_offset = ((row + scroll_pixels) * width * 4) as usize;
                    scrolled[dst_offset..dst_offset + stride]
                        .copy_from_slice(&original[src_offset..src_offset + stride]);
                }
                // New content at top (fill with different pattern)
                for row in 0..scroll_pixels {
                    let offset = (row * width * 4) as usize;
                    for i in 0..stride {
                        scrolled[offset + i] = 0xFF; // New content
                    }
                }
            }
        }

        scrolled
    }

    #[test]
    fn test_scroll_up_detection() {
        let width = 800;
        let height = 600;
        let mut detector = ScrollDetector::new(width, height);

        // Create initial framebuffer
        let fb1 = create_test_framebuffer(width, height, 0x10);

        // Initialize detector with first frame
        detector.detect_scroll(&fb1);

        // Create scrolled framebuffer (scroll up by 20 pixels)
        let fb2 = scroll_framebuffer(&fb1, width, height, 20, ScrollDirection::Up);

        // Detect scroll
        let scroll = detector.detect_scroll(&fb2);
        assert!(scroll.is_some());

        let scroll = scroll.unwrap();
        assert_eq!(scroll.direction, ScrollDirection::Up);
        assert_eq!(scroll.pixels, 20);
    }

    #[test]
    fn test_scroll_down_detection() {
        let width = 800;
        let height = 600;
        let mut detector = ScrollDetector::new(width, height);

        // Create initial framebuffer
        let fb1 = create_test_framebuffer(width, height, 0x20);

        // Initialize detector
        detector.detect_scroll(&fb1);

        // Create scrolled framebuffer (scroll down by 15 pixels)
        let fb2 = scroll_framebuffer(&fb1, width, height, 15, ScrollDirection::Down);

        // Detect scroll
        let scroll = detector.detect_scroll(&fb2);
        assert!(scroll.is_some());

        let scroll = scroll.unwrap();
        assert_eq!(scroll.direction, ScrollDirection::Down);
        assert_eq!(scroll.pixels, 15);
    }

    #[test]
    fn test_no_scroll_detection() {
        let width = 800;
        let height = 600;
        let mut detector = ScrollDetector::new(width, height);

        // Create initial framebuffer
        let fb1 = create_test_framebuffer(width, height, 0x30);

        // Initialize detector
        detector.detect_scroll(&fb1);

        // Create completely different framebuffer (not a scroll)
        let fb2 = create_test_framebuffer(width, height, 0x40);

        // Should not detect scroll
        let scroll = detector.detect_scroll(&fb2);
        assert!(scroll.is_none());
    }

    #[test]
    fn test_reset() {
        let mut detector = ScrollDetector::new(800, 600);

        detector.reset(1024, 768);

        assert_eq!(detector.dimensions(), (1024, 768));
        assert_eq!(detector.row_hashes.len(), 768);
    }

    #[test]
    fn test_large_scroll() {
        let width = 800;
        let height = 600;
        let mut detector = ScrollDetector::new(width, height);

        // Create initial framebuffer
        let fb1 = create_test_framebuffer(width, height, 0x50);

        // Initialize detector
        detector.detect_scroll(&fb1);

        // Create framebuffer with very large scroll (>50% of screen)
        let fb2 = scroll_framebuffer(&fb1, width, height, 400, ScrollDirection::Up);

        // Should not detect (too large)
        let scroll = detector.detect_scroll(&fb2);
        assert!(scroll.is_none());
    }
}
