// Dirty region tracking for efficient terminal rendering
// Only send changed portions of the screen (like Apache guacd)

/// Rectangle representing a dirty region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub min_row: u16,
    pub max_row: u16,
    pub min_col: u16,
    pub max_col: u16,
}

impl Default for DirtyRect {
    fn default() -> Self {
        Self {
            min_row: u16::MAX,
            max_row: 0,
            min_col: u16::MAX,
            max_col: 0,
        }
    }
}

impl DirtyRect {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.min_row > self.max_row || self.min_col > self.max_col
    }

    pub fn expand_to(&mut self, row: u16, col: u16) {
        self.min_row = self.min_row.min(row);
        self.max_row = self.max_row.max(row);
        self.min_col = self.min_col.min(col);
        self.max_col = self.max_col.max(col);
    }

    pub fn width(&self) -> u16 {
        if self.is_empty() {
            0
        } else {
            self.max_col - self.min_col + 1
        }
    }

    pub fn height(&self) -> u16 {
        if self.is_empty() {
            0
        } else {
            self.max_row - self.min_row + 1
        }
    }

    pub fn cell_count(&self) -> usize {
        (self.width() as usize) * (self.height() as usize)
    }

    /// Detect if this dirty region represents a scroll operation
    ///
    /// Returns Some((scroll_direction, scroll_lines)) if scrolling detected:
    /// - scroll_direction: 1 for scroll up (new content at bottom), -1 for scroll down
    /// - scroll_lines: number of lines scrolled
    ///
    /// Scroll detection heuristic:
    /// - Dirty region is full width (spans all columns)
    /// - Dirty region is at top (scroll down) or bottom (scroll up) of screen
    /// - Dirty region is relatively small (< 50% of screen)
    pub fn is_scroll(&self, terminal_rows: u16, terminal_cols: u16) -> Option<(i8, u16)> {
        if self.is_empty() {
            return None;
        }

        // Must be full width
        if self.min_col != 0 || self.max_col != terminal_cols - 1 {
            return None;
        }

        let dirty_height = self.height();
        let dirty_pct = (dirty_height * 100) / terminal_rows;

        // Must be < 50% of screen (not a full refresh)
        if dirty_pct >= 50 {
            return None;
        }

        // Check if at bottom (scroll up - most common)
        if self.max_row == terminal_rows - 1 && self.min_row > 0 {
            return Some((1, dirty_height)); // Scroll up
        }

        // Check if at top (scroll down - less common)
        if self.min_row == 0 && self.max_row < terminal_rows - 1 {
            return Some((-1, dirty_height)); // Scroll down
        }

        None
    }
}

/// Track which cells changed between screen updates
pub struct DirtyTracker {
    last_screen_hash: Vec<u64>,
    rows: u16,
    cols: u16,
}

impl DirtyTracker {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            last_screen_hash: vec![0; (rows as usize) * (cols as usize)],
            rows,
            cols,
        }
    }

    /// Compare current screen with last snapshot and return dirty region
    pub fn find_dirty_region(&mut self, screen: &vt100::Screen) -> Option<DirtyRect> {
        let mut dirty_rect = DirtyRect::new();
        let mut found_dirty = false;

        for row in 0..self.rows {
            for col in 0..self.cols {
                let idx = (row as usize) * (self.cols as usize) + (col as usize);

                // Hash cell content + attributes
                let cell_hash = if let Some(cell) = screen.cell(row, col) {
                    hash_cell(cell)
                } else {
                    0
                };

                // Check if changed
                if cell_hash != self.last_screen_hash[idx] {
                    dirty_rect.expand_to(row, col);
                    found_dirty = true;
                    self.last_screen_hash[idx] = cell_hash;
                }
            }
        }

        if found_dirty {
            Some(dirty_rect)
        } else {
            None
        }
    }

    /// Reset tracking (e.g., after screen resize)
    pub fn reset(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.last_screen_hash = vec![0; (rows as usize) * (cols as usize)];
    }
}

/// Simple hash of cell content and attributes
fn hash_cell(cell: &vt100::Cell) -> u64 {
    let mut hash = 0u64;

    // Hash character
    if let Some(c) = cell.contents().chars().next() {
        hash = hash.wrapping_mul(31).wrapping_add(c as u64);
    }

    // Hash colors (pack into single u64)
    let fg = color_to_u32(cell.fgcolor());
    let bg = color_to_u32(cell.bgcolor());
    hash = hash.wrapping_mul(31).wrapping_add(fg as u64);
    hash = hash.wrapping_mul(31).wrapping_add(bg as u64);

    // Hash attributes
    if cell.bold() {
        hash = hash.wrapping_mul(31).wrapping_add(1);
    }
    if cell.italic() {
        hash = hash.wrapping_mul(31).wrapping_add(2);
    }
    if cell.underline() {
        hash = hash.wrapping_mul(31).wrapping_add(3);
    }
    if cell.inverse() {
        hash = hash.wrapping_mul(31).wrapping_add(4);
    }

    hash
}

fn color_to_u32(color: vt100::Color) -> u32 {
    match color {
        vt100::Color::Default => 0,
        vt100::Color::Idx(idx) => idx as u32 + 1,
        vt100::Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_rect_empty() {
        let rect = DirtyRect::new();
        assert!(rect.is_empty());
    }

    #[test]
    fn test_dirty_rect_expand() {
        let mut rect = DirtyRect::new();
        rect.expand_to(5, 10);
        rect.expand_to(8, 15);

        assert_eq!(rect.min_row, 5);
        assert_eq!(rect.max_row, 8);
        assert_eq!(rect.min_col, 10);
        assert_eq!(rect.max_col, 15);
        assert_eq!(rect.width(), 6);
        assert_eq!(rect.height(), 4);
        assert_eq!(rect.cell_count(), 24);
    }

    #[test]
    fn test_dirty_tracker() {
        let mut tracker = DirtyTracker::new(24, 80);

        // Create screen via Parser (Screen::new is private)
        let parser = vt100::Parser::new(24, 80, 0);
        let screen = parser.screen();

        // First check - no dirty
        let dirty = tracker.find_dirty_region(screen);
        assert!(dirty.is_none());

        // Simulate typing (screen would change but we can't easily mutate vt100::Screen in test)
        // In real usage, terminal.process(data) modifies the screen
    }
}
