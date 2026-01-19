use crate::{points_enclose_text, ColumnSide, SelectionPoint, TerminalEmulator};
/// Guacamole protocol input event handling for terminal-based protocols
///
/// This module provides shared functionality for parsing and handling
/// Guacamole input instructions (key, mouse, clipboard) for SSH, Telnet, etc.
use base64::Engine;
use std::time::{Duration, Instant};

/// Mouse selection state tracking with side-aware selection points
#[derive(Debug, Clone)]
pub struct MouseSelection {
    pub mouse_down: bool,
    pub start: Option<SelectionPoint>,
    pub end: Option<SelectionPoint>,
    pub last_rendered: Option<((u16, u16), (u16, u16))>, // Track last rendered selection for updates

    // Multi-click tracking
    pub last_click_time: Option<Instant>,
    pub click_count: u8,
    pub click_timeout: Duration,

    // Normalized selection boundaries (for rendering)
    pub selection_start_row: Option<u16>,
    pub selection_start_column: Option<u16>,
    pub selection_end_row: Option<u16>,
    pub selection_end_column: Option<u16>,
}

impl MouseSelection {
    pub fn new() -> Self {
        Self {
            mouse_down: false,
            start: None,
            end: None,
            last_rendered: None,
            last_click_time: None,
            click_count: 0,
            click_timeout: Duration::from_millis(300), // 300ms double-click threshold
            selection_start_row: None,
            selection_start_column: None,
            selection_end_row: None,
            selection_end_column: None,
        }
    }

    pub fn reset(&mut self) {
        self.mouse_down = false;
        self.start = None;
        self.end = None;
        self.last_rendered = None;
        self.selection_start_row = None;
        self.selection_start_column = None;
        self.selection_end_row = None;
        self.selection_end_column = None;
        // Don't reset click tracking - we need it for multi-click detection
    }

    /// Register a click and return the click count (1=single, 2=double, 3=triple)
    pub fn register_click(&mut self) -> u8 {
        let now = Instant::now();

        if let Some(last_time) = self.last_click_time {
            if now.duration_since(last_time) < self.click_timeout {
                self.click_count += 1;
            } else {
                self.click_count = 1;
            }
        } else {
            self.click_count = 1;
        }

        self.last_click_time = Some(now);
        self.click_count
    }
}

impl Default for MouseSelection {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of mouse selection handling
#[derive(Debug, Clone)]
pub enum SelectionResult {
    /// Selection in progress - contains instructions to draw overlay
    InProgress(Vec<String>),
    /// Selection complete - contains selected text and instructions to clear overlay
    Complete {
        text: String,
        clear_instructions: Vec<String>,
    },
    /// No selection action (hovering, etc.)
    None,
}

/// Update the normalized selection boundaries based on selection points
fn update_selection_boundaries(selection: &mut MouseSelection, _terminal: &TerminalEmulator) {
    if let (Some(start), Some(end)) = (&selection.start, &selection.end) {
        // Normalize so start comes before end
        let (start, end) = if start.is_after(end) {
            (end, start)
        } else {
            (start, end)
        };

        // Check if points enclose text
        if points_enclose_text(start, end) {
            let new_start_column = start.round_up();
            let new_end_column = end.round_down();

            selection.selection_start_row = Some(start.row);
            selection.selection_start_column = Some(new_start_column);
            selection.selection_end_row = Some(end.row);
            selection.selection_end_column = Some(new_end_column);
        } else {
            // No text enclosed - clear selection
            selection.selection_start_row = None;
            selection.selection_start_column = None;
            selection.selection_end_row = None;
            selection.selection_end_column = None;
        }
    }
}

/// Handle shift+click to extend existing selection
fn handle_shift_click_extend(
    selection: &mut MouseSelection,
    point: SelectionPoint,
    terminal: &TerminalEmulator,
    char_width: u32,
    char_height: u32,
    cols: u16,
) -> SelectionResult {
    if let Some(start) = &selection.start {
        // Determine which end to extend from
        if point.is_after(start) {
            // Extend from start
            selection.end = Some(point);
        } else {
            // Extend from end (swap start/end)
            selection.end = selection.start.clone();
            selection.start = Some(point);
        }

        // Update boundaries
        update_selection_boundaries(selection, terminal);

        // Generate overlay
        if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
            selection.selection_start_row,
            selection.selection_start_column,
            selection.selection_end_row,
            selection.selection_end_column,
        ) {
            let overlay_instructions = format_selection_overlay_instructions(
                (start_row, start_col),
                (end_row, end_col),
                char_width,
                char_height,
                cols,
            );
            selection.last_rendered = Some(((start_row, start_col), (end_row, end_col)));
            SelectionResult::InProgress(overlay_instructions)
        } else {
            SelectionResult::None
        }
    } else {
        // No existing selection - start new one
        selection.start = Some(point.clone());
        selection.end = Some(point);
        update_selection_boundaries(selection, terminal);
        SelectionResult::None
    }
}

/// Handle double-click word selection
fn handle_double_click_word_selection(
    selection: &mut MouseSelection,
    point: SelectionPoint,
    terminal: &TerminalEmulator,
    char_width: u32,
    char_height: u32,
    cols: u16,
) -> SelectionResult {
    // Find word boundaries
    let (word_start, word_end) = find_word_boundaries(terminal, point.row, point.column);

    // Create selection points at word boundaries
    let start_point = SelectionPoint::new(point.row, word_start, ColumnSide::Left, terminal);
    let end_point = SelectionPoint::new(point.row, word_end, ColumnSide::Right, terminal);

    selection.start = Some(start_point);
    selection.end = Some(end_point);

    // Update boundaries
    update_selection_boundaries(selection, terminal);

    // Generate overlay
    if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
        selection.selection_start_row,
        selection.selection_start_column,
        selection.selection_end_row,
        selection.selection_end_column,
    ) {
        let overlay_instructions = format_selection_overlay_instructions(
            (start_row, start_col),
            (end_row, end_col),
            char_width,
            char_height,
            cols,
        );
        selection.last_rendered = Some(((start_row, start_col), (end_row, end_col)));
        SelectionResult::InProgress(overlay_instructions)
    } else {
        SelectionResult::None
    }
}

/// Handle triple-click line selection
fn handle_triple_click_line_selection(
    selection: &mut MouseSelection,
    point: SelectionPoint,
    terminal: &TerminalEmulator,
    char_width: u32,
    char_height: u32,
    cols: u16,
) -> SelectionResult {
    // Select entire line
    let start_point = SelectionPoint::new(point.row, 0, ColumnSide::Left, terminal);
    let end_point = SelectionPoint::new(point.row, cols - 1, ColumnSide::Right, terminal);

    selection.start = Some(start_point);
    selection.end = Some(end_point);

    // Update boundaries
    update_selection_boundaries(selection, terminal);

    // Generate overlay
    if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
        selection.selection_start_row,
        selection.selection_start_column,
        selection.selection_end_row,
        selection.selection_end_column,
    ) {
        let overlay_instructions = format_selection_overlay_instructions(
            (start_row, start_col),
            (end_row, end_col),
            char_width,
            char_height,
            cols,
        );
        selection.last_rendered = Some(((start_row, start_col), (end_row, end_col)));
        SelectionResult::InProgress(overlay_instructions)
    } else {
        SelectionResult::None
    }
}

/// Find word boundaries at a given position
///
/// A "word" is defined as a sequence of alphanumeric characters or underscores.
/// This matches the behavior of most terminal emulators.
fn find_word_boundaries(terminal: &TerminalEmulator, row: u16, col: u16) -> (u16, u16) {
    let screen = terminal.screen();

    // Helper to check if a character is part of a word
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-' || c == '.';

    // Get the character at the clicked position
    let clicked_cell = screen.cell(row, col);
    if clicked_cell.is_none() {
        return (col, col);
    }

    let clicked_contents = clicked_cell.unwrap().contents();
    if clicked_contents.is_empty() {
        return (col, col);
    }

    let clicked_char = clicked_contents.chars().next().unwrap();
    if !is_word_char(clicked_char) {
        // Not a word character - select just this character
        return (col, col);
    }

    // Find start of word (scan left)
    let mut word_start = col;
    while word_start > 0 {
        if let Some(cell) = screen.cell(row, word_start - 1) {
            let contents = cell.contents();
            if !contents.is_empty() {
                let c = contents.chars().next().unwrap();
                if is_word_char(c) {
                    word_start -= 1;
                    continue;
                }
            }
        }
        break;
    }

    // Find end of word (scan right)
    let cols = screen.size().1;
    let mut word_end = col;
    while word_end < cols - 1 {
        if let Some(cell) = screen.cell(row, word_end + 1) {
            let contents = cell.contents();
            if !contents.is_empty() {
                let c = contents.chars().next().unwrap();
                if is_word_char(c) {
                    word_end += 1;
                    continue;
                }
            }
        }
        break;
    }

    (word_start, word_end)
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
        let mut line = String::new();
        for col in start_col..cols {
            if let Some(cell) = screen.cell(start_row, col) {
                line.push_str(&cell.contents());
            }
        }
        // Trim trailing whitespace from line (like guacd does)
        text.push_str(line.trim_end());
        text.push('\n');

        // Middle lines: full lines
        for row in (start_row + 1)..end_row {
            let mut line = String::new();
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    line.push_str(&cell.contents());
                }
            }
            // Trim trailing whitespace from line (like guacd does)
            text.push_str(line.trim_end());
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

/// Handle mouse event for text selection with visual feedback
///
/// Processes mouse events and updates selection state. Returns SelectionResult
/// with overlay instructions for visual feedback or selected text when complete.
///
/// Supports:
/// - Single click + drag: Character-by-character selection
/// - Double click: Word selection
/// - Triple click: Line selection
/// - Shift + click: Extend selection
///
/// # Arguments
/// * `event` - Parsed mouse event
/// * `selection` - Mouse selection state (updated in-place)
/// * `terminal` - Terminal emulator instance
/// * `char_width` - Width of character cell in pixels
/// * `char_height` - Height of character cell in pixels
/// * `cols` - Number of columns in terminal
/// * `rows` - Number of rows in terminal
/// * `shift_pressed` - Whether shift key is currently pressed
///
/// # Returns
/// SelectionResult indicating current selection state
#[allow(clippy::too_many_arguments)]
pub fn handle_mouse_selection(
    event: MouseEvent,
    selection: &mut MouseSelection,
    terminal: &TerminalEmulator,
    char_width: u32,
    char_height: u32,
    cols: u16,
    rows: u16,
    shift_pressed: bool,
) -> SelectionResult {
    // Create selection point from pixel coordinates
    let point = SelectionPoint::from_pixel_coords(
        event.x_px,
        event.y_px,
        char_width,
        char_height,
        cols,
        rows,
        terminal,
    );

    // Button mask: 1=left, 2=middle, 4=right
    let left_button = (event.button_mask & 1) != 0;

    if left_button && !selection.mouse_down {
        // Mouse down - start or extend selection
        selection.mouse_down = true;

        if shift_pressed && selection.start.is_some() {
            // Shift+click: Extend existing selection
            return handle_shift_click_extend(
                selection,
                point,
                terminal,
                char_width,
                char_height,
                cols,
            );
        }

        // Register click for multi-click detection
        let click_count = selection.register_click();

        match click_count {
            1 => {
                // Single click - start new selection
                selection.start = Some(point.clone());
                selection.end = Some(point.clone());

                // Update normalized boundaries
                update_selection_boundaries(selection, terminal);

                // Generate initial overlay
                if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
                    selection.selection_start_row,
                    selection.selection_start_column,
                    selection.selection_end_row,
                    selection.selection_end_column,
                ) {
                    let overlay_instructions = format_selection_overlay_instructions(
                        (start_row, start_col),
                        (end_row, end_col),
                        char_width,
                        char_height,
                        cols,
                    );
                    selection.last_rendered = Some(((start_row, start_col), (end_row, end_col)));
                    SelectionResult::InProgress(overlay_instructions)
                } else {
                    SelectionResult::None
                }
            }
            2 => {
                // Double click - select word
                handle_double_click_word_selection(
                    selection,
                    point,
                    terminal,
                    char_width,
                    char_height,
                    cols,
                )
            }
            _ => {
                // Triple click (or more) - select line
                handle_triple_click_line_selection(
                    selection,
                    point,
                    terminal,
                    char_width,
                    char_height,
                    cols,
                )
            }
        }
    } else if left_button && selection.mouse_down {
        // Drag - update selection end
        selection.end = Some(point);

        // Update normalized boundaries
        update_selection_boundaries(selection, terminal);

        // Only send update if selection changed
        if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
            selection.selection_start_row,
            selection.selection_start_column,
            selection.selection_end_row,
            selection.selection_end_column,
        ) {
            let current_selection = ((start_row, start_col), (end_row, end_col));
            if selection.last_rendered != Some(current_selection) {
                let overlay_instructions = format_selection_overlay_instructions(
                    (start_row, start_col),
                    (end_row, end_col),
                    char_width,
                    char_height,
                    cols,
                );
                selection.last_rendered = Some(current_selection);
                SelectionResult::InProgress(overlay_instructions)
            } else {
                SelectionResult::None
            }
        } else {
            SelectionResult::None
        }
    } else if !left_button && selection.mouse_down {
        // Mouse up - finalize selection and return text
        selection.mouse_down = false;

        let result = if let (Some(start_row), Some(start_col), Some(end_row), Some(end_col)) = (
            selection.selection_start_row,
            selection.selection_start_column,
            selection.selection_end_row,
            selection.selection_end_column,
        ) {
            let text =
                extract_selection_text(terminal, (start_row, start_col), (end_row, end_col), cols);
            if text.is_empty() {
                SelectionResult::None
            } else {
                let clear_instructions = format_clear_selection_instructions();
                SelectionResult::Complete {
                    text,
                    clear_instructions,
                }
            }
        } else {
            SelectionResult::None
        };

        selection.reset();
        result
    } else {
        SelectionResult::None
    }
}

/// Generate Guacamole instructions to draw selection overlay
///
/// Creates blue semi-transparent rectangles to highlight selected text.
/// Uses layer ID 1 for selection overlay (layer 0 is the terminal display).
///
/// # Arguments
/// * `start` - Start position (row, col)
/// * `end` - End position (row, col)
/// * `char_width` - Width of character cell in pixels
/// * `char_height` - Height of character cell in pixels
/// * `cols` - Number of columns in terminal
///
/// # Returns
/// Vec of Guacamole instructions to draw selection overlay
pub fn format_selection_overlay_instructions(
    start: (u16, u16),
    end: (u16, u16),
    char_width: u32,
    char_height: u32,
    cols: u16,
) -> Vec<String> {
    let (start_row, start_col) = start;
    let (end_row, end_col) = end;

    // Normalize selection (ensure start < end)
    let (start_row, start_col, end_row, end_col) =
        if start_row > end_row || (start_row == end_row && start_col > end_col) {
            (end_row, end_col, start_row, start_col)
        } else {
            (start_row, start_col, end_row, end_col)
        };

    let mut instructions = Vec::new();
    let layer = "1"; // Selection overlay layer

    if start_row == end_row {
        // Single row selection - one rectangle
        let x = start_col as u32 * char_width;
        let y = start_row as u32 * char_height;
        let width = (end_col - start_col + 1) as u32 * char_width;
        let height = char_height;

        instructions.push(format!(
            "4.rect,{}.{},{}.{},{}.{},{}.{},{}.{};",
            layer.len(),
            layer,
            x.to_string().len(),
            x,
            y.to_string().len(),
            y,
            width.to_string().len(),
            width,
            height.to_string().len(),
            height
        ));
    } else {
        // Multi-row selection - three rectangles

        // First row: from start_col to end of row
        let x1 = start_col as u32 * char_width;
        let y1 = start_row as u32 * char_height;
        let width1 = (cols - start_col) as u32 * char_width;
        let height1 = char_height;

        instructions.push(format!(
            "4.rect,{}.{},{}.{},{}.{},{}.{},{}.{};",
            layer.len(),
            layer,
            x1.to_string().len(),
            x1,
            y1.to_string().len(),
            y1,
            width1.to_string().len(),
            width1,
            height1.to_string().len(),
            height1
        ));

        // Middle rows: full width (if any)
        if end_row > start_row + 1 {
            let x2 = 0;
            let y2 = (start_row + 1) as u32 * char_height;
            let width2 = cols as u32 * char_width;
            let height2 = (end_row - start_row - 1) as u32 * char_height;

            instructions.push(format!(
                "4.rect,{}.{},{}.{},{}.{},{}.{},{}.{};",
                layer.len(),
                layer,
                x2.to_string().len(),
                x2,
                y2.to_string().len(),
                y2,
                width2.to_string().len(),
                width2,
                height2.to_string().len(),
                height2
            ));
        }

        // Last row: from start to end_col
        let x3 = 0;
        let y3 = end_row as u32 * char_height;
        let width3 = (end_col + 1) as u32 * char_width;
        let height3 = char_height;

        instructions.push(format!(
            "4.rect,{}.{},{}.{},{}.{},{}.{},{}.{};",
            layer.len(),
            layer,
            x3.to_string().len(),
            x3,
            y3.to_string().len(),
            y3,
            width3.to_string().len(),
            width3,
            height3.to_string().len(),
            height3
        ));
    }

    // Fill with blue semi-transparent color (matching guacd visibility)
    // Blue: R=0, G=128 (0x80), B=255 (0xFF), A=200 (0xC8 = 78% opacity)
    // Increased from 160 (62.7%) to 200 (78%) for much better visibility
    // This matches guacd's selection overlay which is quite visible
    instructions.push(format!(
        "5.cfill,{}.{},1.0,3.128,3.255,3.200;",
        layer.len(),
        layer
    ));

    instructions
}

/// Generate Guacamole instructions to clear selection overlay
///
/// Uses the `dispose` instruction to properly destroy the overlay layer.
/// This is the correct way to clear a layer in the Guacamole protocol.
///
/// # Returns
/// Vec of Guacamole instructions to clear selection overlay
pub fn format_clear_selection_instructions() -> Vec<String> {
    // Use dispose instruction to properly clear the overlay layer
    // Layer 1 is the selection overlay layer
    vec![guacr_protocol::format_dispose(1)]
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
