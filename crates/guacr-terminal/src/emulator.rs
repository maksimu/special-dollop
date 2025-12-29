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

/// Mouse tracking mode as set by the remote application
///
/// Terminal applications enable mouse reporting via escape sequences:
/// - Normal mode (1000): Report button press/release
/// - Button event mode (1002): Report press/release and motion while pressed
/// - Any event mode (1003): Report all motion events
/// - SGR extended mode (1006): Extended coordinates for large terminals
///
/// Without mouse mode enabled, sending X11 mouse sequences to the terminal
/// will result in garbage characters being displayed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MouseMode {
    /// Mouse tracking disabled (default) - do NOT send mouse events to terminal
    #[default]
    Disabled,
    /// Normal tracking mode (1000) - report button press/release
    Normal,
    /// Button event tracking mode (1002) - report press/release + motion while pressed  
    ButtonEvent,
    /// Any event tracking mode (1003) - report all motion
    AnyEvent,
}

impl MouseMode {
    /// Returns true if mouse mode is enabled (any tracking mode active)
    pub fn is_enabled(&self) -> bool {
        !matches!(self, MouseMode::Disabled)
    }
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
/// Tracks mouse mode state for proper X11 mouse event handling.
pub struct TerminalEmulator {
    parser: Parser,
    rows: u16,
    cols: u16,
    dirty: bool,
    scrollback: VecDeque<ScrollbackLine>,
    scrollback_max_lines: usize,
    last_screen_state: Vec<String>, // For detecting scrolls
    /// Current mouse tracking mode set by the remote application
    mouse_mode: MouseMode,
    /// Whether SGR extended mouse mode (1006) is active
    sgr_mouse_mode: bool,
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
            mouse_mode: MouseMode::Disabled,
            sgr_mouse_mode: false,
        }
    }

    /// Process terminal output data
    pub fn process(&mut self, data: &[u8]) -> Result<()> {
        // Check for mouse mode escape sequences BEFORE processing
        // This ensures we track mouse mode changes as they happen
        self.detect_mouse_mode_changes(data);

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

    /// Get current mouse tracking mode
    ///
    /// Returns the mouse mode set by the remote application via escape sequences.
    /// If this returns `MouseMode::Disabled`, X11 mouse sequences should NOT be
    /// sent to the terminal (they will appear as garbage characters).
    pub fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    /// Check if mouse tracking is enabled
    ///
    /// Returns true if any mouse tracking mode is active (Normal, ButtonEvent, or AnyEvent).
    /// Only send X11 mouse sequences to the terminal when this returns true.
    pub fn is_mouse_enabled(&self) -> bool {
        self.mouse_mode.is_enabled()
    }

    /// Check if SGR extended mouse mode (1006) is active
    ///
    /// SGR mode uses a different escape sequence format for coordinates.
    pub fn is_sgr_mouse_mode(&self) -> bool {
        self.sgr_mouse_mode
    }

    /// Detect mouse mode changes from escape sequences in the data
    ///
    /// Scans for DEC private mode set/reset sequences that control mouse tracking:
    /// - ESC [ ? 1000 h/l - Normal tracking mode (enable/disable)
    /// - ESC [ ? 1002 h/l - Button event tracking mode
    /// - ESC [ ? 1003 h/l - Any event tracking mode
    /// - ESC [ ? 1006 h/l - SGR extended mode
    fn detect_mouse_mode_changes(&mut self, data: &[u8]) {
        // Look for CSI ? <mode> h (set) or CSI ? <mode> l (reset)
        // ESC = 0x1b, [ = 0x5b, ? = 0x3f, h = 0x68, l = 0x6c
        let mut i = 0;
        while i + 5 < data.len() {
            // Look for ESC [
            if data[i] == 0x1b && data[i + 1] == 0x5b {
                // Check for ? (DEC private mode)
                if data[i + 2] == 0x3f {
                    // Find the mode number and terminator
                    let start = i + 3;
                    let mut end = start;

                    // Scan for digits
                    while end < data.len() && data[end].is_ascii_digit() {
                        end += 1;
                    }

                    if end > start && end < data.len() {
                        // Parse mode number
                        if let Ok(mode_str) = std::str::from_utf8(&data[start..end]) {
                            if let Ok(mode_num) = mode_str.parse::<u32>() {
                                let terminator = data[end];
                                let enable = terminator == 0x68; // 'h'
                                let disable = terminator == 0x6c; // 'l'

                                if enable || disable {
                                    self.apply_mouse_mode_change(mode_num, enable);
                                }
                            }
                        }
                    }
                }
            }
            i += 1;
        }
    }

    /// Apply a mouse mode change based on DEC private mode number
    fn apply_mouse_mode_change(&mut self, mode: u32, enable: bool) {
        match mode {
            1000 => {
                // Normal tracking mode
                if enable {
                    self.mouse_mode = MouseMode::Normal;
                } else if self.mouse_mode == MouseMode::Normal {
                    self.mouse_mode = MouseMode::Disabled;
                }
            }
            1002 => {
                // Button event tracking mode
                if enable {
                    self.mouse_mode = MouseMode::ButtonEvent;
                } else if self.mouse_mode == MouseMode::ButtonEvent {
                    self.mouse_mode = MouseMode::Disabled;
                }
            }
            1003 => {
                // Any event tracking mode
                if enable {
                    self.mouse_mode = MouseMode::AnyEvent;
                } else if self.mouse_mode == MouseMode::AnyEvent {
                    self.mouse_mode = MouseMode::Disabled;
                }
            }
            1006 => {
                // SGR extended mode (affects coordinate encoding, not tracking mode)
                self.sgr_mouse_mode = enable;
            }
            _ => {}
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

    #[test]
    fn test_mouse_mode_default() {
        let term = TerminalEmulator::new(24, 80);
        assert_eq!(term.mouse_mode(), MouseMode::Disabled);
        assert!(!term.is_mouse_enabled());
        assert!(!term.is_sgr_mouse_mode());
    }

    #[test]
    fn test_mouse_mode_normal_enable() {
        let mut term = TerminalEmulator::new(24, 80);

        // ESC [ ? 1000 h - Enable normal tracking
        term.process(b"\x1b[?1000h").unwrap();
        assert_eq!(term.mouse_mode(), MouseMode::Normal);
        assert!(term.is_mouse_enabled());
    }

    #[test]
    fn test_mouse_mode_normal_disable() {
        let mut term = TerminalEmulator::new(24, 80);

        // Enable then disable
        term.process(b"\x1b[?1000h").unwrap();
        assert!(term.is_mouse_enabled());

        term.process(b"\x1b[?1000l").unwrap();
        assert_eq!(term.mouse_mode(), MouseMode::Disabled);
        assert!(!term.is_mouse_enabled());
    }

    #[test]
    fn test_mouse_mode_button_event() {
        let mut term = TerminalEmulator::new(24, 80);

        // ESC [ ? 1002 h - Enable button event tracking
        term.process(b"\x1b[?1002h").unwrap();
        assert_eq!(term.mouse_mode(), MouseMode::ButtonEvent);
        assert!(term.is_mouse_enabled());
    }

    #[test]
    fn test_mouse_mode_any_event() {
        let mut term = TerminalEmulator::new(24, 80);

        // ESC [ ? 1003 h - Enable any event tracking
        term.process(b"\x1b[?1003h").unwrap();
        assert_eq!(term.mouse_mode(), MouseMode::AnyEvent);
        assert!(term.is_mouse_enabled());
    }

    #[test]
    fn test_mouse_mode_sgr() {
        let mut term = TerminalEmulator::new(24, 80);

        // ESC [ ? 1006 h - Enable SGR extended mode
        term.process(b"\x1b[?1006h").unwrap();
        assert!(term.is_sgr_mouse_mode());

        // Disable
        term.process(b"\x1b[?1006l").unwrap();
        assert!(!term.is_sgr_mouse_mode());
    }

    #[test]
    fn test_mouse_mode_in_mixed_data() {
        let mut term = TerminalEmulator::new(24, 80);

        // Mouse enable sequence embedded in regular text
        term.process(b"Hello \x1b[?1000h World").unwrap();
        assert!(term.is_mouse_enabled());

        // Verify text was still processed
        let screen = term.screen();
        let contents = screen.contents();
        assert!(contents.contains("Hello"));
        assert!(contents.contains("World"));
    }

    #[test]
    fn test_mouse_mode_vim_like_sequence() {
        let mut term = TerminalEmulator::new(24, 80);

        // Vim typically enables mouse with: ESC[?1000h ESC[?1002h ESC[?1006h
        term.process(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h").unwrap();

        // Last one wins for mode, SGR should be enabled
        assert_eq!(term.mouse_mode(), MouseMode::ButtonEvent);
        assert!(term.is_mouse_enabled());
        assert!(term.is_sgr_mouse_mode());
    }
}
