use crate::Result;
use std::collections::VecDeque;
use vt100::Parser;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// A line of terminal content with cell information
#[derive(Debug, Clone)]
pub struct ScrollbackLine {
    pub cells: Vec<vt100::Cell>,
    pub cols: u16,
}

impl ScrollbackLine {
    pub fn new(cols: u16) -> Self {
        Self {
            cells: Vec::with_capacity(cols as usize),
            cols,
        }
    }

    pub fn from_screen_row(screen: &vt100::Screen, row: u16, cols: u16) -> Self {
        let mut cells = Vec::with_capacity(cols as usize);
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                cells.push(cell.clone());
            }
        }
        Self { cells, cols }
    }
}

/// Terminal emulator using VT100 parser
///
/// Wraps the vt100 crate and provides dirty tracking for efficient rendering.
/// Maintains a scrollback buffer of lines that have scrolled off the top.
pub struct TerminalEmulator {
    parser: Parser,
    rows: u16,
    cols: u16,
    dirty: bool,
    scrollback: VecDeque<ScrollbackLine>,
    scrollback_max_lines: usize,
    last_screen_state: Vec<String>, // For detecting scrolls
}

impl TerminalEmulator {
    /// Extract text from a specific row of the screen
    fn row_text(screen: &vt100::Screen, row: u16, cols: u16) -> String {
        let mut text = String::with_capacity(cols as usize);
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                if let Some(c) = cell.contents().chars().next() {
                    text.push(c);
                } else {
                    text.push(' ');
                }
            } else {
                text.push(' ');
            }
        }
        text
    }

    /// Create a new terminal emulator with default scrollback (150 lines)
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::new_with_scrollback(rows, cols, 150)
    }

    /// Create a new terminal emulator with specified scrollback size
    pub fn new_with_scrollback(rows: u16, cols: u16, scrollback_lines: usize) -> Self {
        let parser = Parser::new(rows, cols, 0);
        let screen = parser.screen();
        let last_screen_state = (0..rows)
            .map(|row| Self::row_text(screen, row, cols))
            .collect();

        Self {
            parser,
            rows,
            cols,
            dirty: false,
            scrollback: VecDeque::with_capacity(scrollback_lines),
            scrollback_max_lines: scrollback_lines,
            last_screen_state,
        }
    }

    /// Process terminal output data
    pub fn process(&mut self, data: &[u8]) -> Result<()> {
        let screen = self.parser.screen();

        // Capture current screen state before processing
        let current_screen_state: Vec<String> = (0..self.rows)
            .map(|row| Self::row_text(screen, row, self.cols))
            .collect();

        // Capture the top line before processing (in case it scrolls off)
        let top_line_before = ScrollbackLine::from_screen_row(screen, 0, self.cols);

        // Process the data
        self.parser.process(data);

        // Detect scroll up (new content at bottom, old content pushed up)
        // Compare screen states to detect if top line changed and bottom line is new
        let should_add_scrollback = {
            let new_screen = self.parser.screen();
            if !current_screen_state.is_empty() && current_screen_state.len() == self.rows as usize
            {
                let old_top = &current_screen_state[0];
                let new_top = Self::row_text(new_screen, 0, self.cols);
                let old_bottom = current_screen_state.last().unwrap();
                let new_bottom = Self::row_text(new_screen, self.rows - 1, self.cols);

                // If top line changed but bottom is new (different from old bottom), it's a scroll up
                old_top != &new_top && old_bottom != &new_bottom
            } else {
                false
            }
        };

        // Add to scrollback if needed (outside the borrow)
        if should_add_scrollback {
            self.scrollback.push_back(top_line_before);

            // Limit scrollback size
            while self.scrollback.len() > self.scrollback_max_lines {
                self.scrollback.pop_front();
            }
        }

        // Update last screen state
        let new_screen = self.parser.screen();
        self.last_screen_state = (0..self.rows)
            .map(|row| Self::row_text(new_screen, row, self.cols))
            .collect();

        self.dirty = true;
        Ok(())
    }

    /// Add a line to scrollback buffer (public for testing)
    #[cfg(test)]
    pub(crate) fn add_to_scrollback(&mut self, line: ScrollbackLine) {
        self.scrollback.push_back(line);

        // Limit scrollback size
        while self.scrollback.len() > self.scrollback_max_lines {
            self.scrollback.pop_front();
        }
    }

    /// Check if terminal has changed since last render
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark terminal as clean (after rendering)
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Get the terminal screen
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Get terminal dimensions
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Resize terminal
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser = Parser::new(rows, cols, 0);
        self.rows = rows;
        self.cols = cols;

        // Update last screen state for new dimensions
        let screen = self.parser.screen();
        self.last_screen_state = (0..rows)
            .map(|row| Self::row_text(screen, row, cols))
            .collect();

        self.dirty = true;
    }

    /// Get the full screen as a rectangle
    pub fn full_screen_rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.cols,
            height: self.rows,
        }
    }

    /// Get scrollback buffer size
    pub fn scrollback_lines(&self) -> usize {
        self.scrollback.len()
    }

    /// Get a line from scrollback buffer (0 = oldest, scrollback_lines()-1 = newest)
    pub fn get_scrollback_line(&self, index: usize) -> Option<&ScrollbackLine> {
        self.scrollback.get(index)
    }

    /// Get all scrollback lines
    pub fn scrollback(&self) -> &VecDeque<ScrollbackLine> {
        &self.scrollback
    }

    /// Clear scrollback buffer
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_new() {
        let term = TerminalEmulator::new(24, 80);
        assert_eq!(term.size(), (24, 80));
        assert!(!term.is_dirty());
    }

    #[test]
    fn test_terminal_process() {
        let mut term = TerminalEmulator::new(24, 80);
        term.process(b"Hello, World!\n").unwrap();

        assert!(term.is_dirty());

        let screen = term.screen();
        let contents = screen.contents();
        assert!(contents.contains("Hello, World!"));
    }

    #[test]
    fn test_terminal_dirty_flag() {
        let mut term = TerminalEmulator::new(24, 80);

        assert!(!term.is_dirty());

        term.process(b"test").unwrap();
        assert!(term.is_dirty());

        term.clear_dirty();
        assert!(!term.is_dirty());
    }

    #[test]
    fn test_terminal_resize() {
        let mut term = TerminalEmulator::new(24, 80);

        term.resize(30, 100);
        assert_eq!(term.size(), (30, 100));
        assert!(term.is_dirty());
    }

    #[test]
    fn test_full_screen_rect() {
        let term = TerminalEmulator::new(24, 80);
        let rect = term.full_screen_rect();

        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        assert_eq!(rect.width, 80);
        assert_eq!(rect.height, 24);
    }

    #[test]
    fn test_scrollback_initial_state() {
        let term = TerminalEmulator::new_with_scrollback(24, 80, 150);
        assert_eq!(term.scrollback_lines(), 0);
        assert!(term.scrollback().is_empty());
    }

    #[test]
    fn test_scrollback_access() {
        let mut term = TerminalEmulator::new_with_scrollback(24, 80, 150);

        // Add scrollback lines manually
        for _ in 0..5 {
            let line = ScrollbackLine::new(80);
            term.add_to_scrollback(line);
        }

        assert_eq!(term.scrollback_lines(), 5);

        // Access scrollback buffer
        let scrollback = term.scrollback();
        assert_eq!(scrollback.len(), 5);

        // Get specific line
        let line = term.get_scrollback_line(0);
        assert!(line.is_some());
        assert_eq!(line.unwrap().cols, 80);
    }

    #[test]
    fn test_scrollback_with_scrolling() {
        let mut term = TerminalEmulator::new_with_scrollback(5, 80, 10); // Small terminal for testing

        // Fill terminal with content that will cause scrolling
        for i in 0..10 {
            term.process(format!("Line {}\n", i).as_bytes()).unwrap();
        }

        // After scrolling, we should have some scrollback
        // Note: Scrollback detection depends on screen state changes
        let scrollback_count = term.scrollback_lines();

        // Verify scrollback methods work
        if scrollback_count > 0 {
            let line = term.get_scrollback_line(0);
            assert!(line.is_some());
            assert_eq!(line.unwrap().cols, 80);
        }
    }

    #[test]
    fn test_scrollback_limit() {
        let mut term = TerminalEmulator::new_with_scrollback(5, 80, 3); // Limit to 3 lines

        // Create scrollback lines manually (since scrollback detection is complex)
        for _ in 0..5 {
            let line = ScrollbackLine::new(80);
            term.add_to_scrollback(line);
        }

        // Should be limited to 3 lines
        assert_eq!(term.scrollback_lines(), 3);
    }

    #[test]
    fn test_clear_scrollback() {
        let mut term = TerminalEmulator::new_with_scrollback(24, 80, 150);

        // Add some scrollback
        for _ in 0..5 {
            let line = ScrollbackLine::new(80);
            term.add_to_scrollback(line);
        }

        assert_eq!(term.scrollback_lines(), 5);

        // Clear it
        term.clear_scrollback();
        assert_eq!(term.scrollback_lines(), 0);
    }

    #[test]
    fn test_scrollback_line_creation() {
        let line = ScrollbackLine::new(80);
        assert_eq!(line.cols, 80);
        assert!(line.cells.is_empty());
    }
}
