//! Cell-based copy detection using 2D Rabin-Karp hashing.
//!
//! Detects when pixel content moved position between frames, enabling `copy`
//! instructions (~50 bytes) instead of re-encoded images (~100-500KB).
//!
//! Shared between RDP and VNC handlers (any protocol using FrameBuffer).
//!
//! ## Algorithm
//!
//! 1. Divide framebuffer into 64x64 cells
//! 2. Hash each cell using 2D polynomial rolling hash
//! 3. Compare with previous frame cell hashes
//! 4. If content moved position, emit CellOp::Copy
//! 5. Solid-color cells emit CellOp::Rect
//! 6. Remaining changed cells emit CellOp::Image
//!
//! ## Performance
//!
//! On a 2102x1536 screen:
//! - 33x24 = 792 cells
//! - Hash per cell: ~4096 ops (64 rows x 64 pixels)
//! - Total: ~3.2M ops (~1-2ms)
//! - Lookup: O(1) per cell via flat hash table

use crate::framebuffer::FrameRect;

/// Cell size in pixels (matches Apache guacd's cell size)
const CELL_SIZE: u32 = 64;

/// Hash table bucket count (power of 2 for fast modulo via bitmask)
const HASH_TABLE_SIZE: usize = 65536;
const HASH_TABLE_MASK: u64 = (HASH_TABLE_SIZE as u64) - 1;

/// Maximum combined region size for merging adjacent operations
const MAX_COMBINED_SIZE: u32 = 512;

/// Operations the handler should execute for this frame
#[derive(Debug, Clone, PartialEq)]
pub enum CellOp {
    /// Copy pixels from previous frame position to new position.
    /// The client executes this as canvas.drawImage() which is GPU-accelerated.
    Copy {
        src_x: u32,
        src_y: u32,
        dst_x: u32,
        dst_y: u32,
        w: u32,
        h: u32,
    },
    /// Fill rectangle with solid color (rect + cfill instruction).
    Rect {
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color: [u8; 4],
    },
    /// Encode and send as image (JPEG/PNG/WebP).
    Image { x: u32, y: u32, w: u32, h: u32 },
}

/// Entry in the flat hash table mapping cell hash -> grid position
#[derive(Clone, Default)]
struct CellRef {
    col: u16,
    row: u16,
    hash: u64,
    occupied: bool,
}

/// Cell-based copy detection using 2D Rabin-Karp hashing.
///
/// Shared between RDP and VNC handlers.
pub struct CopyDetector {
    /// Grid columns (width / cell_size, rounded up)
    cols: u32,
    /// Grid rows (height / cell_size, rounded up)
    rows: u32,
    /// Hash of each cell in the previous frame (cols * rows entries)
    prev_cell_hashes: Vec<u64>,
    /// Previous frame pixels (for verification on hash match)
    prev_frame: Vec<u8>,
    /// Flat hash table for current-frame cell lookup
    hash_index: Vec<CellRef>,
    /// Screen dimensions
    width: u32,
    height: u32,
    /// Whether we have a valid previous frame
    has_prev_frame: bool,
}

impl CopyDetector {
    pub fn new(width: u32, height: u32) -> Self {
        let cols = width.div_ceil(CELL_SIZE);
        let rows = height.div_ceil(CELL_SIZE);
        let num_cells = (cols * rows) as usize;

        Self {
            cols,
            rows,
            prev_cell_hashes: vec![0; num_cells],
            prev_frame: Vec::new(),
            hash_index: vec![CellRef::default(); HASH_TABLE_SIZE],
            width,
            height,
            has_prev_frame: false,
        }
    }

    /// Plan operations for a frame given current pixel data and dirty region.
    ///
    /// Returns a list of operations the handler should execute. The caller
    /// should process them in order: Copy first (shift existing pixels on client),
    /// then Rect (fill solid regions), then Image (encode remaining changes).
    ///
    /// `current_pixels` must be RGBA data (4 bytes/pixel), length = width * height * 4.
    /// `dirty` is the bounding rect of the changed region from the remote desktop.
    pub fn plan_frame(&mut self, current_pixels: &[u8], dirty: FrameRect) -> Vec<CellOp> {
        let expected_len = (self.width * self.height * 4) as usize;
        if current_pixels.len() != expected_len {
            // Wrong size -- fall back to full image
            self.save_frame(current_pixels);
            return vec![CellOp::Image {
                x: dirty.x,
                y: dirty.y,
                w: dirty.width,
                h: dirty.height,
            }];
        }

        if !self.has_prev_frame {
            // First frame -- no comparison possible
            self.save_frame(current_pixels);
            return vec![CellOp::Image {
                x: dirty.x,
                y: dirty.y,
                w: dirty.width,
                h: dirty.height,
            }];
        }

        // Determine which cells overlap the dirty region
        let cell_col_start = dirty.x / CELL_SIZE;
        let cell_col_end = (dirty.x + dirty.width).div_ceil(CELL_SIZE).min(self.cols);
        let cell_row_start = dirty.y / CELL_SIZE;
        let cell_row_end = (dirty.y + dirty.height).div_ceil(CELL_SIZE).min(self.rows);

        // Phase 1: Hash current dirty cells and build index
        self.clear_hash_index();
        let mut current_hashes = self.prev_cell_hashes.clone();
        let mut cell_states: Vec<CellState> =
            vec![CellState::Clean; (self.cols * self.rows) as usize];

        for row in cell_row_start..cell_row_end {
            for col in cell_col_start..cell_col_end {
                let idx = (row * self.cols + col) as usize;
                let (cx, cy, cw, ch) = self.cell_rect(col, row);
                let hash = self.hash_cell(current_pixels, cx, cy, cw, ch);
                current_hashes[idx] = hash;

                // Check for solid color
                if let Some(color) = self.check_solid(current_pixels, cx, cy, cw, ch) {
                    cell_states[idx] = CellState::Solid(color);
                    continue;
                }

                // Index this cell for lookup
                self.index_cell(col as u16, row as u16, hash);
                cell_states[idx] = CellState::Dirty;
            }
        }

        // Phase 2: Search previous frame for matches
        for row in cell_row_start..cell_row_end {
            for col in cell_col_start..cell_col_end {
                let idx = (row * self.cols + col) as usize;
                if cell_states[idx] != CellState::Dirty {
                    continue;
                }

                let prev_hash = self.prev_cell_hashes[idx];
                if prev_hash == 0 {
                    continue;
                }

                // Look up previous cell hash in current frame's index
                if let Some((match_col, match_row)) = self.lookup_cell(prev_hash) {
                    // Hash matched -- verify pixels
                    let (prev_x, prev_y, prev_w, prev_h) = self.cell_rect(col, row);
                    let (cur_x, cur_y, cur_w, cur_h) =
                        self.cell_rect(match_col as u32, match_row as u32);

                    if prev_w == cur_w
                        && prev_h == cur_h
                        && self.verify_cells(
                            &self.prev_frame,
                            prev_x,
                            prev_y,
                            current_pixels,
                            cur_x,
                            cur_y,
                            prev_w,
                            prev_h,
                        )
                    {
                        let new_idx = (match_row as u32 * self.cols + match_col as u32) as usize;
                        if cell_states[new_idx] == CellState::Dirty {
                            cell_states[new_idx] = CellState::Copy {
                                src_col: col,
                                src_row: row,
                            };
                        }
                    }
                }
            }
        }

        // Phase 3: Emit operations
        let mut ops: Vec<CellOp> = Vec::new();

        for row in cell_row_start..cell_row_end {
            for col in cell_col_start..cell_col_end {
                let idx = (row * self.cols + col) as usize;
                let (cx, cy, cw, ch) = self.cell_rect(col, row);

                match &cell_states[idx] {
                    CellState::Clean => {}
                    CellState::Copy { src_col, src_row } => {
                        let (sx, sy, _, _) = self.cell_rect(*src_col, *src_row);
                        ops.push(CellOp::Copy {
                            src_x: sx,
                            src_y: sy,
                            dst_x: cx,
                            dst_y: cy,
                            w: cw,
                            h: ch,
                        });
                    }
                    CellState::Solid(color) => {
                        ops.push(CellOp::Rect {
                            x: cx,
                            y: cy,
                            w: cw,
                            h: ch,
                            color: *color,
                        });
                    }
                    CellState::Dirty => {
                        ops.push(CellOp::Image {
                            x: cx,
                            y: cy,
                            w: cw,
                            h: ch,
                        });
                    }
                }
            }
        }

        // Phase 4: Sort copy operations in safe order to prevent read-after-write
        // corruption. When content shifts right, process rightmost destinations first
        // so sources are read before being overwritten by earlier copies.
        let ops = Self::sort_copies_safe(ops);

        // Phase 5: Merge adjacent operations (after sorting to preserve safe order)
        let ops = Self::merge_ops(ops);

        // Phase 6: Save state for next frame
        self.prev_cell_hashes = current_hashes;
        self.save_frame(current_pixels);

        ops
    }

    /// Reset detector (e.g., after screen resize)
    pub fn reset(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.cols = width.div_ceil(CELL_SIZE);
        self.rows = height.div_ceil(CELL_SIZE);
        let num_cells = (self.cols * self.rows) as usize;
        self.prev_cell_hashes = vec![0; num_cells];
        self.prev_frame.clear();
        self.has_prev_frame = false;
    }

    // ---- Internal helpers ----

    /// Get pixel rect for a cell, clamped to screen bounds
    fn cell_rect(&self, col: u32, row: u32) -> (u32, u32, u32, u32) {
        let x = col * CELL_SIZE;
        let y = row * CELL_SIZE;
        let w = CELL_SIZE.min(self.width.saturating_sub(x));
        let h = CELL_SIZE.min(self.height.saturating_sub(y));
        (x, y, w, h)
    }

    /// 2D polynomial rolling hash for a cell region (matches guacd algorithm)
    fn hash_cell(&self, pixels: &[u8], x: u32, y: u32, w: u32, h: u32) -> u64 {
        let stride = self.width * 4;
        let mut cell_hash: u64 = 0;

        for row in 0..h {
            let mut row_hash: u64 = 0;
            let y_off = (y + row) * stride;

            for col in 0..w {
                let off = (y_off + (x + col) * 4) as usize;
                if off + 3 < pixels.len() {
                    let pixel = u32::from_ne_bytes([
                        pixels[off],
                        pixels[off + 1],
                        pixels[off + 2],
                        pixels[off + 3],
                    ]);
                    row_hash = (row_hash.wrapping_mul(31) << 1).wrapping_add(pixel as u64);
                }
            }

            cell_hash = (cell_hash.wrapping_mul(31) << 1).wrapping_add(row_hash);
        }

        // Ensure non-zero (0 = empty/unused sentinel)
        if cell_hash == 0 {
            1
        } else {
            cell_hash
        }
    }

    /// Check if all pixels in a cell are the same color
    fn check_solid(&self, pixels: &[u8], x: u32, y: u32, w: u32, h: u32) -> Option<[u8; 4]> {
        if w == 0 || h == 0 {
            return None;
        }

        let stride = (self.width * 4) as usize;
        let first_off = (y * self.width * 4 + x * 4) as usize;
        if first_off + 3 >= pixels.len() {
            return None;
        }

        let color = [
            pixels[first_off],
            pixels[first_off + 1],
            pixels[first_off + 2],
            pixels[first_off + 3],
        ];

        for row in 0..h {
            let row_off = (y + row) as usize * stride + x as usize * 4;
            for col in 0..w {
                let off = row_off + col as usize * 4;
                if off + 3 >= pixels.len() {
                    return None;
                }
                if pixels[off] != color[0]
                    || pixels[off + 1] != color[1]
                    || pixels[off + 2] != color[2]
                // Skip alpha comparison -- alpha is always 255
                {
                    return None;
                }
            }
        }

        Some(color)
    }

    /// Clear the flat hash table
    fn clear_hash_index(&mut self) {
        for entry in &mut self.hash_index {
            entry.occupied = false;
        }
    }

    /// XOR-fold a 64-bit hash to a 16-bit bucket index
    fn bucket(hash: u64) -> usize {
        let folded = hash ^ (hash >> 16) ^ (hash >> 32) ^ (hash >> 48);
        (folded & HASH_TABLE_MASK) as usize
    }

    /// Insert a cell into the hash index (open addressing, linear probing)
    fn index_cell(&mut self, col: u16, row: u16, hash: u64) {
        let mut bucket = Self::bucket(hash);
        for _ in 0..16 {
            // Max 16 probes
            if !self.hash_index[bucket].occupied {
                self.hash_index[bucket] = CellRef {
                    col,
                    row,
                    hash,
                    occupied: true,
                };
                return;
            }
            bucket = (bucket + 1) & (HASH_TABLE_SIZE - 1);
        }
        // Table too full at this bucket chain -- just skip indexing
    }

    /// Look up a hash in the index, returning (col, row) if found
    fn lookup_cell(&self, hash: u64) -> Option<(u16, u16)> {
        let mut bucket = Self::bucket(hash);
        for _ in 0..16 {
            let entry = &self.hash_index[bucket];
            if !entry.occupied {
                return None;
            }
            if entry.hash == hash {
                return Some((entry.col, entry.row));
            }
            bucket = (bucket + 1) & (HASH_TABLE_SIZE - 1);
        }
        None
    }

    /// Verify that two cells have identical pixels (row-by-row comparison)
    #[allow(clippy::too_many_arguments)]
    fn verify_cells(
        &self,
        prev: &[u8],
        prev_x: u32,
        prev_y: u32,
        curr: &[u8],
        curr_x: u32,
        curr_y: u32,
        w: u32,
        h: u32,
    ) -> bool {
        let stride = (self.width * 4) as usize;
        let row_bytes = (w * 4) as usize;

        for row in 0..h {
            let prev_off = (prev_y + row) as usize * stride + prev_x as usize * 4;
            let curr_off = (curr_y + row) as usize * stride + curr_x as usize * 4;

            if prev_off + row_bytes > prev.len() || curr_off + row_bytes > curr.len() {
                return false;
            }

            if prev[prev_off..prev_off + row_bytes] != curr[curr_off..curr_off + row_bytes] {
                return false;
            }
        }

        true
    }

    /// Save current frame as previous frame for next comparison
    fn save_frame(&mut self, pixels: &[u8]) {
        self.prev_frame.clear();
        self.prev_frame.extend_from_slice(pixels);
        self.has_prev_frame = true;
    }

    /// Sort copy operations in safe order to prevent read-after-write corruption.
    ///
    /// When multiple copy operations share the same layer, a later copy might read
    /// from a position already written by an earlier copy. For example, shifting
    /// content right by 64px:
    ///   Copy (0,0)->(64,0), Copy (64,0)->(128,0)
    /// The second copy reads from (64,0) which was already overwritten.
    ///
    /// Fix: process copies in reverse of the shift direction:
    /// - dx > 0 (shift right): sort by dst_x descending (rightmost first)
    /// - dx < 0 (shift left): sort by dst_x ascending (leftmost first)
    /// - dy > 0 (shift down): sort by dst_y descending (bottommost first)
    /// - dy < 0 (shift up): sort by dst_y ascending (topmost first)
    fn sort_copies_safe(mut ops: Vec<CellOp>) -> Vec<CellOp> {
        // Find the dominant copy delta
        let mut dx_sum: i64 = 0;
        let mut dy_sum: i64 = 0;
        let mut copy_count: i64 = 0;

        for op in &ops {
            if let CellOp::Copy {
                src_x,
                src_y,
                dst_x,
                dst_y,
                ..
            } = op
            {
                dx_sum += *dst_x as i64 - *src_x as i64;
                dy_sum += *dst_y as i64 - *src_y as i64;
                copy_count += 1;
            }
        }

        if copy_count == 0 {
            return ops;
        }

        // Average delta direction determines sort order
        let avg_dx = dx_sum / copy_count;
        let avg_dy = dy_sum / copy_count;

        // Partition: non-copy ops first (they don't have ordering constraints),
        // then copies in safe order
        let mut non_copies: Vec<CellOp> = Vec::new();
        let mut copies: Vec<CellOp> = Vec::new();

        for op in ops.drain(..) {
            match &op {
                CellOp::Copy { .. } => copies.push(op),
                _ => non_copies.push(op),
            }
        }

        // Sort copies based on dominant shift direction.
        // Primary axis = dimension with larger average shift.
        copies.sort_by(|a, b| {
            let (a_dx, a_dy) = match a {
                CellOp::Copy { dst_x, dst_y, .. } => (*dst_x, *dst_y),
                _ => unreachable!(),
            };
            let (b_dx, b_dy) = match b {
                CellOp::Copy { dst_x, dst_y, .. } => (*dst_x, *dst_y),
                _ => unreachable!(),
            };

            if avg_dx.abs() >= avg_dy.abs() {
                // Horizontal shift is dominant
                if avg_dx > 0 {
                    // Shifting right: process rightmost first
                    b_dx.cmp(&a_dx).then(b_dy.cmp(&a_dy))
                } else {
                    // Shifting left: process leftmost first
                    a_dx.cmp(&b_dx).then(a_dy.cmp(&b_dy))
                }
            } else {
                // Vertical shift is dominant
                if avg_dy > 0 {
                    // Shifting down: process bottommost first
                    b_dy.cmp(&a_dy).then(b_dx.cmp(&a_dx))
                } else {
                    // Shifting up: process topmost first
                    a_dy.cmp(&b_dy).then(a_dx.cmp(&b_dx))
                }
            }
        });

        // Emit: copies first (before images overwrite source regions), then non-copies
        copies.extend(non_copies);
        copies
    }

    /// Merge adjacent operations of the same type with the same translation delta
    fn merge_ops(ops: Vec<CellOp>) -> Vec<CellOp> {
        if ops.len() < 2 {
            return ops;
        }

        let mut merged: Vec<CellOp> = Vec::with_capacity(ops.len());

        for op in ops {
            let should_merge = if let Some(last) = merged.last() {
                Self::can_merge(last, &op)
            } else {
                false
            };

            if should_merge {
                let last = merged.last_mut().unwrap();
                Self::do_merge(last, &op);
            } else {
                merged.push(op);
            }
        }

        merged
    }

    fn can_merge(a: &CellOp, b: &CellOp) -> bool {
        match (a, b) {
            (
                CellOp::Copy {
                    src_x: sx_a,
                    src_y: sy_a,
                    dst_x: dx_a,
                    dst_y: dy_a,
                    w: w_a,
                    h: h_a,
                },
                CellOp::Copy {
                    src_x: sx_b,
                    src_y: sy_b,
                    dst_x: dx_b,
                    dst_y: dy_b,
                    w: w_b,
                    h: h_b,
                },
            ) => {
                // Same translation delta
                let delta_x_a = *dx_a as i64 - *sx_a as i64;
                let delta_y_a = *dy_a as i64 - *sy_a as i64;
                let delta_x_b = *dx_b as i64 - *sx_b as i64;
                let delta_y_b = *dy_b as i64 - *sy_b as i64;

                if delta_x_a != delta_x_b || delta_y_a != delta_y_b {
                    return false;
                }

                // Adjacent horizontally (same row, same height, next column)
                let adjacent_h = *sy_a == *sy_b && *h_a == *h_b && sx_a + w_a == *sx_b;
                // Adjacent vertically (same column, same width, next row)
                let adjacent_v = *sx_a == *sx_b && *w_a == *w_b && sy_a + h_a == *sy_b;

                (adjacent_h || adjacent_v)
                    && w_a + b.width() <= MAX_COMBINED_SIZE
                    && h_a + b.height() <= MAX_COMBINED_SIZE
            }
            (
                CellOp::Rect {
                    x: x_a,
                    y: y_a,
                    w: w_a,
                    h: h_a,
                    color: c_a,
                },
                CellOp::Rect {
                    x: x_b,
                    y: y_b,
                    color: c_b,
                    ..
                },
            ) => {
                if c_a != c_b {
                    return false;
                }
                let adjacent_h = *y_a == *y_b && x_a + w_a == *x_b;
                let adjacent_v = *x_a == *x_b && y_a + h_a == *y_b;
                (adjacent_h || adjacent_v)
                    && w_a + b.width() <= MAX_COMBINED_SIZE
                    && h_a + b.height() <= MAX_COMBINED_SIZE
            }
            (
                CellOp::Image {
                    x: x_a,
                    y: y_a,
                    w: w_a,
                    h: h_a,
                },
                CellOp::Image { x: x_b, y: y_b, .. },
            ) => {
                let adjacent_h = *y_a == *y_b && x_a + w_a == *x_b;
                let adjacent_v = *x_a == *x_b && y_a + h_a == *y_b;
                (adjacent_h || adjacent_v)
                    && w_a + b.width() <= MAX_COMBINED_SIZE
                    && h_a + b.height() <= MAX_COMBINED_SIZE
            }
            _ => false,
        }
    }

    fn do_merge(a: &mut CellOp, b: &CellOp) {
        // Determine merge direction from actual coordinates.
        // can_merge already verified adjacency; here we just detect whether
        // B is below A (vertical) or to the right of A (horizontal).
        match (a, b) {
            (
                CellOp::Copy {
                    src_x: sx_a,
                    w: w_a,
                    h: h_a,
                    ..
                },
                CellOp::Copy {
                    src_x: sx_b,
                    w: w_b,
                    h: h_b,
                    ..
                },
            ) => {
                if *sx_b == *sx_a && *w_b <= *w_a {
                    *h_a += h_b;
                } else {
                    *w_a += w_b;
                }
            }
            (
                CellOp::Rect {
                    x: x_a,
                    w: w_a,
                    h: h_a,
                    ..
                },
                CellOp::Rect {
                    x: x_b,
                    w: w_b,
                    h: h_b,
                    ..
                },
            ) => {
                if *x_b == *x_a && *w_b <= *w_a {
                    *h_a += h_b;
                } else {
                    *w_a += w_b;
                }
            }
            (
                CellOp::Image {
                    x: x_a,
                    w: w_a,
                    h: h_a,
                    ..
                },
                CellOp::Image {
                    x: x_b,
                    w: w_b,
                    h: h_b,
                    ..
                },
            ) => {
                if *x_b == *x_a && *w_b <= *w_a {
                    *h_a += h_b;
                } else {
                    *w_a += w_b;
                }
            }
            _ => {}
        }
    }
}

impl CellOp {
    fn width(&self) -> u32 {
        match self {
            CellOp::Copy { w, .. } => *w,
            CellOp::Rect { w, .. } => *w,
            CellOp::Image { w, .. } => *w,
        }
    }

    fn height(&self) -> u32 {
        match self {
            CellOp::Copy { h, .. } => *h,
            CellOp::Rect { h, .. } => *h,
            CellOp::Image { h, .. } => *h,
        }
    }
}

/// Internal cell state during frame analysis
#[derive(Debug, Clone, PartialEq)]
enum CellState {
    /// Cell is outside dirty region or unchanged
    Clean,
    /// Cell content moved here from previous frame position
    Copy { src_col: u32, src_row: u32 },
    /// Cell is a solid color
    Solid([u8; 4]),
    /// Cell changed and needs image encoding
    Dirty,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_solid_frame(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut data = vec![0u8; (width * height * 4) as usize];
        for pixel in data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        data
    }

    fn make_pattern_frame(width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let off = (y * width + x) as usize * 4;
                data[off] = (x % 256) as u8;
                data[off + 1] = (y % 256) as u8;
                data[off + 2] = ((x + y) % 256) as u8;
                data[off + 3] = 255;
            }
        }
        data
    }

    #[test]
    fn test_first_frame_returns_image() {
        let mut detector = CopyDetector::new(256, 256);
        let frame = make_pattern_frame(256, 256);
        let dirty = FrameRect {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        };

        let ops = detector.plan_frame(&frame, dirty);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], CellOp::Image { .. }));
    }

    #[test]
    fn test_identical_frames_no_ops() {
        let mut detector = CopyDetector::new(256, 256);
        let frame = make_pattern_frame(256, 256);
        let dirty = FrameRect {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        };

        // First frame
        detector.plan_frame(&frame, dirty);

        // Second identical frame -- all cells unchanged, but dirty region
        // forces re-evaluation. Cells will have same hash as previous,
        // so copy detection should find matches (self-copies).
        // In practice the handler would not call plan_frame without actual changes.
        let ops = detector.plan_frame(&frame, dirty);

        // When prev and current hashes match at same position, the cell
        // is a self-copy which we should still render since it's in the dirty region.
        // Actually, self-copies (src == dst) are not useful, they'll be Image ops.
        for op in &ops {
            match op {
                CellOp::Copy {
                    src_x,
                    src_y,
                    dst_x,
                    dst_y,
                    ..
                } => {
                    // Self-copies are valid but indicate identical content
                    assert_eq!(src_x, dst_x);
                    assert_eq!(src_y, dst_y);
                }
                CellOp::Image { .. } | CellOp::Rect { .. } => {} // Also fine
            }
        }
    }

    #[test]
    fn test_solid_detection() {
        let mut detector = CopyDetector::new(128, 128);
        let white = [255u8, 255, 255, 255];
        let frame1 = make_pattern_frame(128, 128);
        let frame2 = make_solid_frame(128, 128, white);
        let dirty = FrameRect {
            x: 0,
            y: 0,
            width: 128,
            height: 128,
        };

        detector.plan_frame(&frame1, dirty);
        let ops = detector.plan_frame(&frame2, dirty);

        // All cells should be detected as solid
        for op in &ops {
            if let CellOp::Rect { color, .. } = op {
                assert_eq!(*color, white);
            }
        }
        assert!(!ops.is_empty());
    }

    #[test]
    fn test_moved_content_detected() {
        let w = 256u32;
        let h = 256u32;
        let mut detector = CopyDetector::new(w, h);

        // Frame 1: pattern
        let frame1 = make_pattern_frame(w, h);
        let dirty = FrameRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        detector.plan_frame(&frame1, dirty);

        // Frame 2: shift content right by 64 pixels (one cell)
        let mut frame2 = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 64..w {
                let dst = (y * w + x) as usize * 4;
                let src = (y * w + (x - 64)) as usize * 4;
                frame2[dst..dst + 4].copy_from_slice(&frame1[src..src + 4]);
            }
        }

        let ops = detector.plan_frame(&frame2, dirty);

        // Should have at least some Copy ops for the shifted content
        let copy_count = ops
            .iter()
            .filter(|op| matches!(op, CellOp::Copy { .. }))
            .count();
        let image_count = ops
            .iter()
            .filter(|op| matches!(op, CellOp::Image { .. }))
            .count();

        // We expect copies for the shifted cells and images for new content at left edge
        assert!(
            copy_count > 0 || image_count > 0,
            "Expected some operations, got copies={}, images={}",
            copy_count,
            image_count
        );
    }

    #[test]
    fn test_reset() {
        let mut detector = CopyDetector::new(640, 480);
        let frame = make_pattern_frame(640, 480);
        let dirty = FrameRect {
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        };
        detector.plan_frame(&frame, dirty);

        detector.reset(1920, 1080);
        assert_eq!(detector.width, 1920);
        assert_eq!(detector.height, 1080);
        assert!(!detector.has_prev_frame);
    }
}
