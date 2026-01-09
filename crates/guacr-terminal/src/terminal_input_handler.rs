//! Unified terminal input handler for all terminal-based protocols
//!
//! Provides shared functionality for:
//! - Mouse selection (click and drag to select text)
//! - Clipboard operations (copy/paste)
//! - Terminal resizing
//! - Scrollback navigation
//!
//! Used by SSH, Telnet, and Database handlers for consistent UX.

use crate::{
    extract_selection_text, format_clear_selection_instructions, format_clipboard_instructions,
    format_selection_overlay_instructions, handle_mouse_selection, parse_clipboard_blob,
    parse_mouse_instruction, DirtyTracker, MouseEvent, MouseSelection, SelectionResult,
    TerminalEmulator,
};

/// Unified terminal input handler
///
/// Manages mouse selection, clipboard, and resize operations for all terminal types.
pub struct TerminalInputHandler {
    /// Mouse selection state
    mouse_selection: MouseSelection,

    /// Clipboard data received from client
    clipboard_data: String,

    /// Current terminal dimensions
    rows: u16,
    cols: u16,

    /// Dirty region tracker for scroll optimization
    dirty: DirtyTracker,

    /// Scrollback configuration
    scrollback_size: usize,
}

impl TerminalInputHandler {
    /// Create a new input handler with default scrollback
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::new_with_scrollback(rows, cols, 1000)
    }

    /// Create a new input handler with custom scrollback size
    pub fn new_with_scrollback(rows: u16, cols: u16, scrollback_size: usize) -> Self {
        Self {
            mouse_selection: MouseSelection::new(),
            clipboard_data: String::new(),
            rows,
            cols,
            dirty: DirtyTracker::new(rows, cols),
            scrollback_size,
        }
    }

    /// Get current terminal dimensions
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    /// Get scrollback size
    pub fn scrollback_size(&self) -> usize {
        self.scrollback_size
    }

    /// Handle clipboard paste from client
    ///
    /// Returns the clipboard text as bytes ready to be sent to the terminal/application.
    pub fn handle_clipboard_paste(&mut self, clipboard_text: &str) -> Vec<u8> {
        self.clipboard_data = clipboard_text.to_string();
        clipboard_text.as_bytes().to_vec()
    }

    /// Parse clipboard instruction and return text if present
    pub fn parse_clipboard_instruction(&self, instruction: &str) -> Option<String> {
        parse_clipboard_blob(instruction)
    }

    /// Handle mouse event for selection
    ///
    /// Returns SelectionResult indicating what action to take.
    ///
    /// Note: Requires character dimensions (9x18 pixels per char for databases)
    pub fn handle_mouse_event(
        &mut self,
        mouse_event: MouseEvent,
        terminal: &TerminalEmulator,
        char_width: u32,
        char_height: u32,
    ) -> SelectionResult {
        handle_mouse_selection(
            mouse_event,
            &mut self.mouse_selection,
            terminal,
            char_width,
            char_height,
            self.cols,
            self.rows,
            false, // shift_pressed - TODO: track modifier state
        )
    }

    /// Parse mouse instruction from Guacamole protocol
    pub fn parse_mouse_instruction(&self, instruction: &str) -> Option<MouseEvent> {
        parse_mouse_instruction(instruction)
    }

    /// Handle terminal resize
    ///
    /// Updates dimensions and resizes the terminal emulator.
    pub fn handle_resize(
        &mut self,
        new_rows: u16,
        new_cols: u16,
        terminal: &mut TerminalEmulator,
    ) -> crate::Result<()> {
        if new_rows != self.rows || new_cols != self.cols {
            self.rows = new_rows;
            self.cols = new_cols;
            terminal.resize(new_rows, new_cols);
            self.dirty = DirtyTracker::new(new_rows, new_cols);

            // Clear selection on resize
            self.mouse_selection = MouseSelection::new();
        }
        Ok(())
    }

    /// Get clipboard instructions for selected text
    ///
    /// Returns Guacamole clipboard instructions to send to client.
    pub fn get_clipboard_instructions(
        &self,
        terminal: &TerminalEmulator,
        stream_id: u32,
    ) -> Vec<String> {
        // Check if we have a selection
        if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
            self.mouse_selection.selection_start_row,
            self.mouse_selection.selection_start_column,
            self.mouse_selection.selection_end_row,
            self.mouse_selection.selection_end_column,
        ) {
            // Extract text using the shared function
            let text = extract_selection_text(
                terminal,
                (start_row, start_col),
                (end_row, end_col),
                terminal.screen().size().1,
            );

            if !text.is_empty() {
                return format_clipboard_instructions(&text, stream_id);
            }
        }

        vec![]
    }

    /// Get selection overlay instructions
    ///
    /// Returns Guacamole instructions to draw selection highlight.
    ///
    /// Note: Requires character dimensions (9x18 pixels per char for databases)
    pub fn get_selection_overlay_instructions(
        &self,
        char_width: u32,
        char_height: u32,
    ) -> Vec<String> {
        if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
            self.mouse_selection.selection_start_row,
            self.mouse_selection.selection_start_column,
            self.mouse_selection.selection_end_row,
            self.mouse_selection.selection_end_column,
        ) {
            format_selection_overlay_instructions(
                (start_row, start_col),
                (end_row, end_col),
                char_width,
                char_height,
                self.cols,
            )
        } else {
            vec![]
        }
    }

    /// Clear selection overlay instructions
    pub fn get_clear_selection_instructions(&self) -> Vec<String> {
        format_clear_selection_instructions()
    }

    /// Check if selection is active
    pub fn has_selection(&self) -> bool {
        self.mouse_selection.start.is_some() && self.mouse_selection.end.is_some()
    }

    /// Clear current selection
    pub fn clear_selection(&mut self) {
        self.mouse_selection = MouseSelection::new();
    }

    /// Get dirty tracker for scroll optimization
    pub fn dirty_tracker(&self) -> &DirtyTracker {
        &self.dirty
    }

    /// Get mutable dirty tracker
    pub fn dirty_tracker_mut(&mut self) -> &mut DirtyTracker {
        &mut self.dirty
    }

    /// Reset dirty tracker
    pub fn reset_dirty(&mut self) {
        self.dirty = DirtyTracker::new(self.rows, self.cols);
    }

    /// Get stored clipboard data
    pub fn clipboard_data(&self) -> &str {
        &self.clipboard_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_handler() {
        let handler = TerminalInputHandler::new(24, 80);
        assert_eq!(handler.size(), (24, 80));
        assert_eq!(handler.scrollback_size(), 1000);
        assert!(!handler.has_selection());
    }

    #[test]
    fn test_new_with_scrollback() {
        let handler = TerminalInputHandler::new_with_scrollback(24, 80, 5000);
        assert_eq!(handler.scrollback_size(), 5000);
    }

    #[test]
    fn test_clipboard_paste() {
        let mut handler = TerminalInputHandler::new(24, 80);
        let text = "SELECT * FROM users";
        let bytes = handler.handle_clipboard_paste(text);
        assert_eq!(bytes, text.as_bytes());
        assert_eq!(handler.clipboard_data(), text);
    }

    #[test]
    fn test_resize() {
        let mut handler = TerminalInputHandler::new(24, 80);
        let mut terminal = TerminalEmulator::new(24, 80);

        handler.handle_resize(30, 100, &mut terminal).unwrap();
        assert_eq!(handler.size(), (30, 100));
        assert_eq!(terminal.size(), (30, 100));
    }

    #[test]
    fn test_clear_selection() {
        let mut handler = TerminalInputHandler::new(24, 80);
        // Selection would be set by handle_mouse_event in real usage
        handler.clear_selection();
        assert!(!handler.has_selection());
    }

    #[test]
    fn test_clipboard_instructions_empty() {
        let handler = TerminalInputHandler::new(24, 80);
        let terminal = TerminalEmulator::new(24, 80);
        let instrs = handler.get_clipboard_instructions(&terminal, 1);
        assert!(instrs.is_empty()); // No selection
    }
}
