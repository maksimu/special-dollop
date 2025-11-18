use crate::Result;
use vt100::Parser;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Terminal emulator using VT100 parser
///
/// Wraps the vt100 crate and provides dirty tracking for efficient rendering
pub struct TerminalEmulator {
    parser: Parser,
    rows: u16,
    cols: u16,
    dirty: bool,
}

impl TerminalEmulator {
    /// Create a new terminal emulator
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows, cols, 0),
            rows,
            cols,
            dirty: false,
        }
    }

    /// Process terminal output data
    pub fn process(&mut self, data: &[u8]) -> Result<()> {
        self.parser.process(data);
        self.dirty = true;
        Ok(())
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
}
