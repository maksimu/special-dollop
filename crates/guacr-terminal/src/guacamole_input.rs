use crate::TerminalEmulator;
/// Guacamole protocol input event handling for terminal-based protocols
///
/// This module provides shared functionality for parsing and handling
/// Guacamole input instructions (key, mouse, clipboard) for SSH, Telnet, etc.
use base64::Engine;

/// Mouse selection state tracking
#[derive(Debug, Clone)]
pub struct MouseSelection {
    pub mouse_down: bool,
    pub start: Option<(u16, u16)>, // (row, col)
    pub end: Option<(u16, u16)>,
}

impl MouseSelection {
    pub fn new() -> Self {
        Self {
            mouse_down: false,
            start: None,
            end: None,
        }
    }

    pub fn reset(&mut self) {
        self.mouse_down = false;
        self.start = None;
        self.end = None;
    }
}

impl Default for MouseSelection {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed key event from Guacamole protocol
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub keysym: u32,
    pub pressed: bool,
}

/// Parsed mouse event from Guacamole protocol
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub x_px: u32,
    pub y_px: u32,
    pub button_mask: u32,
}

/// Parse a Guacamole key instruction
///
/// Format: "3.key,LENGTH.KEYSYM,LENGTH.PRESSED;"
/// Example: "3.key,2.99,1.1;" (key 'c' pressed)
///
/// Returns: Some(KeyEvent) if valid, None otherwise
pub fn parse_key_instruction(msg: &str) -> Option<KeyEvent> {
    if !msg.contains(".key,") {
        return None;
    }

    let args_part = msg.split_once(".key,")?.1;
    let (first_arg, rest) = args_part.split_once(',')?;
    let (_, keysym_str) = first_arg.split_once('.')?;
    let mut keysym = keysym_str.parse::<u32>().ok()?;

    let (_, pressed_val) = rest.split_once('.')?;
    let pressed = pressed_val.starts_with('1');

    // Fix keysyms - match Vault's behavior
    // Right Ctrl (65508/0xFFE4) → Left Ctrl (65507/0xFFE3)
    // Some browsers don't send Right Ctrl correctly
    if keysym == 65508 {
        keysym = 65507;
    }

    Some(KeyEvent { keysym, pressed })
}

/// Parse a Guacamole mouse instruction
///
/// Format: "5.mouse,LENGTH.X,LENGTH.Y,LENGTH.BUTTON;"
/// Example: "5.mouse,3.915,3.328,1.0;" (at pixel 915,328, no button)
///
/// Returns: Some(MouseEvent) if valid, None otherwise
pub fn parse_mouse_instruction(msg: &str) -> Option<MouseEvent> {
    if !msg.contains(".mouse,") {
        return None;
    }

    let args_part = msg.split_once(".mouse,")?.1;
    let parts: Vec<&str> = args_part.split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    // Parse x: "3.915" -> extract "915"
    let (_, x_str) = parts[0].split_once('.')?;
    // Parse y: "3.328" -> extract "328"
    let (_, y_str) = parts[1].split_once('.')?;
    // Parse button: "1.0;" -> extract "0" (button mask)
    let button_part = parts[2].trim_end_matches(';');
    let (_, button_str) = button_part.split_once('.')?;

    let x_px = x_str.parse::<u32>().ok()?;
    let y_px = y_str.parse::<u32>().ok()?;
    let button_mask = button_str.parse::<u32>().ok()?;

    Some(MouseEvent {
        x_px,
        y_px,
        button_mask,
    })
}

/// Parse clipboard blob data from Guacamole instruction
///
/// Format: "4.blob,LENGTH.STREAM_ID,LENGTH.BASE64DATA;"
/// Example: "4.blob,1.0,24.UnVzdCBoYW5kbGVyIGxldmVs;"
///
/// Returns: Some(String) with decoded text, None if invalid/empty
pub fn parse_clipboard_blob(msg: &str) -> Option<String> {
    if !msg.contains(".blob,") {
        return None;
    }

    let args_part = msg.split_once(".blob,")?.1;
    let parts: Vec<&str> = args_part.split(',').collect();
    if parts.len() < 2 {
        return None;
    }

    // parts[0] = "1.0" (stream ID)
    // parts[1] = "44.base64data;" (length.data)
    let (_, data_part) = parts[1].split_once('.')?;
    let data_str = data_part.trim_end_matches(';');

    // Decode base64 clipboard data
    use base64::Engine;
    let clipboard_data = base64::engine::general_purpose::STANDARD
        .decode(data_str)
        .ok()?;
    let clipboard_text = String::from_utf8(clipboard_data).ok()?;

    // Skip empty clipboard syncs (initial connection)
    if clipboard_text.is_empty() {
        None
    } else {
        Some(clipboard_text)
    }
}

/// Extract selected text from terminal emulator
///
/// Converts mouse selection coordinates (row, col) to actual text content.
/// Supports both single-line and multi-line selections.
///
/// # Arguments
/// * `terminal` - Terminal emulator instance
/// * `start` - Start position (row, col)
/// * `end` - End position (row, col)
/// * `cols` - Number of columns in terminal
///
/// # Returns
/// String containing the selected text
pub fn extract_selection_text(
    terminal: &TerminalEmulator,
    start: (u16, u16),
    end: (u16, u16),
    cols: u16,
) -> String {
    let screen = terminal.screen();
    let (start_row, start_col) = start;
    let (end_row, end_col) = end;

    // Normalize selection (ensure start < end)
    let (start_row, start_col, end_row, end_col) =
        if start_row > end_row || (start_row == end_row && start_col > end_col) {
            (end_row, end_col, start_row, start_col)
        } else {
            (start_row, start_col, end_row, end_col)
        };

    let mut text = String::new();

    if start_row == end_row {
        // Single line selection
        for col in start_col..=end_col {
            if let Some(cell) = screen.cell(start_row, col) {
                text.push_str(&cell.contents());
            }
        }
    } else {
        // Multi-line selection
        // First line: from start_col to end
        for col in start_col..cols {
            if let Some(cell) = screen.cell(start_row, col) {
                text.push_str(&cell.contents());
            }
        }
        text.push('\n');

        // Middle lines: full lines
        for row in (start_row + 1)..end_row {
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    text.push_str(&cell.contents());
                }
            }
            text.push('\n');
        }

        // Last line: from start to end_col
        for col in 0..=end_col {
            if let Some(cell) = screen.cell(end_row, col) {
                text.push_str(&cell.contents());
            }
        }
    }

    text
}

/// Handle mouse event for text selection
///
/// Processes mouse events and updates selection state. Returns Some(selected_text)
/// when the selection is complete (mouse up).
///
/// # Arguments
/// * `event` - Parsed mouse event
/// * `selection` - Mouse selection state (updated in-place)
/// * `terminal` - Terminal emulator instance
/// * `char_width` - Width of character cell in pixels
/// * `char_height` - Height of character cell in pixels
/// * `cols` - Number of columns in terminal
/// * `rows` - Number of rows in terminal
///
/// # Returns
/// Some(String) with selected text when selection completes, None otherwise
pub fn handle_mouse_selection(
    event: MouseEvent,
    selection: &mut MouseSelection,
    terminal: &TerminalEmulator,
    char_width: u32,
    char_height: u32,
    cols: u16,
    rows: u16,
) -> Option<String> {
    // Convert pixel coords to terminal cell coords
    let col = (event.x_px / char_width).min(cols as u32 - 1) as u16;
    let row = (event.y_px / char_height).min(rows as u32 - 1) as u16;

    // Button mask: 1=left, 2=middle, 4=right
    let left_button = (event.button_mask & 1) != 0;

    if left_button && !selection.mouse_down {
        // Mouse down - start selection
        selection.mouse_down = true;
        selection.start = Some((row, col));
        selection.end = Some((row, col));
        None
    } else if left_button && selection.mouse_down {
        // Drag - update selection end
        selection.end = Some((row, col));
        None
    } else if !left_button && selection.mouse_down {
        // Mouse up - finalize selection and return text
        selection.mouse_down = false;
        let end_pos = Some((row, col));

        let result = if let (Some(start), Some(end)) = (selection.start, end_pos) {
            let text = extract_selection_text(terminal, start, end, cols);
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        } else {
            None
        };

        selection.reset();
        result
    } else {
        None
    }
}

/// Format clipboard data as Guacamole protocol instructions
///
/// Creates a sequence of instructions to send clipboard data to client:
/// 1. clipboard instruction (allocate stream)
/// 2. blob instruction (send base64 data)
/// 3. end instruction (close stream)
///
/// # Arguments
/// * `clipboard_text` - Text to send to clipboard
/// * `stream_id` - Stream ID to use (e.g., 10)
///
/// # Returns
/// Vec of instruction strings ready to send
pub fn format_clipboard_instructions(clipboard_text: &str, stream_id: u32) -> Vec<String> {
    let clipboard_data =
        base64::engine::general_purpose::STANDARD.encode(clipboard_text.as_bytes());

    vec![
        // Allocate clipboard stream
        format!(
            "9.clipboard,{}.{},10.text/plain;",
            stream_id.to_string().len(),
            stream_id
        ),
        // Send blob
        format!(
            "4.blob,{}.{},{}.{};",
            stream_id.to_string().len(),
            stream_id,
            clipboard_data.len(),
            clipboard_data
        ),
        // End stream
        format!("3.end,{}.{};", stream_id.to_string().len(), stream_id),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_instruction() {
        let event = parse_key_instruction("3.key,2.99,1.1;").unwrap();
        assert_eq!(event.keysym, 99); // 'c'
        assert!(event.pressed);

        let event = parse_key_instruction("3.key,2.99,1.0;").unwrap();
        assert_eq!(event.keysym, 99);
        assert!(!event.pressed);

        // Test Right Ctrl -> Left Ctrl fix
        let event = parse_key_instruction("3.key,5.65508,1.1;").unwrap();
        assert_eq!(event.keysym, 65507); // Fixed to Left Ctrl
    }

    #[test]
    fn test_parse_mouse_instruction() {
        let event = parse_mouse_instruction("5.mouse,3.915,3.328,1.0;").unwrap();
        assert_eq!(event.x_px, 915);
        assert_eq!(event.y_px, 328);
        assert_eq!(event.button_mask, 0);

        let event = parse_mouse_instruction("5.mouse,3.100,2.50,1.1;").unwrap();
        assert_eq!(event.x_px, 100);
        assert_eq!(event.y_px, 50);
        assert_eq!(event.button_mask, 1); // Left button
    }

    #[test]
    fn test_parse_clipboard_blob() {
        // "Rust handler level" in base64
        let text = parse_clipboard_blob("4.blob,1.0,24.UnVzdCBoYW5kbGVyIGxldmVs;").unwrap();
        assert_eq!(text, "Rust handler level");

        // Empty clipboard should return None
        let result = parse_clipboard_blob("4.blob,1.0,0.;");
        assert!(result.is_none());
    }
}
