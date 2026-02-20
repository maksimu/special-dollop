// Database result rendering - shared infrastructure for all database handlers
//
// Provides both terminal-style (CLI) and spreadsheet-style (GUI) rendering
// for SQL query results, with Guacamole protocol output.
//
// The SpreadsheetRenderer provides an interactive data grid with:
// - fontdue-based text rendering (JetBrains Mono, matching TerminalRenderer)
// - JPEG output at quality 85 (matching terminal handlers)
// - Dynamic viewport sizing based on pixel dimensions
// - Row-level selection with keyboard and mouse navigation
// - Configurable per-row actions with keyboard shortcuts
// - Dirty tracking for efficient re-rendering

use crate::{TerminalEmulator, TerminalRenderer};
use fontdue::{Font, FontSettings};
use image::{Rgb, RgbImage};
use std::cell::RefCell;
use std::collections::HashMap;

// Primary font: JetBrains Mono (same as TerminalRenderer)
const FONT_DATA_PRIMARY: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
// Fallback font: DejaVu Sans Mono (same as TerminalRenderer)
const FONT_DATA_FALLBACK: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");

/// Query result structure - shared by all database handlers
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub affected_rows: Option<u64>,
    pub execution_time_ms: Option<u64>,
}

impl QueryResult {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            affected_rows: None,
            execution_time_ms: None,
        }
    }

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

/// Database terminal - SQL CLI experience
///
/// Wraps TerminalEmulator and TerminalRenderer for consistent database UI.
/// Used by all database handlers for terminal-style interaction.
pub struct DatabaseTerminal {
    terminal: TerminalEmulator,
    renderer: TerminalRenderer,
    prompt: String,
    input_buffer: String,
    db_type: String,
}

impl DatabaseTerminal {
    /// Create a new database terminal with specified prompt
    pub fn new(rows: u16, cols: u16, prompt: &str, db_type: &str) -> crate::Result<Self> {
        Ok(Self {
            terminal: TerminalEmulator::new(rows, cols),
            renderer: TerminalRenderer::new()?,
            prompt: prompt.to_string(),
            input_buffer: String::new(),
            db_type: db_type.to_string(),
        })
    }

    /// Write the prompt to terminal
    pub fn write_prompt(&mut self) -> crate::Result<()> {
        self.terminal.process(self.prompt.as_bytes())?;
        Ok(())
    }

    /// Set a new prompt
    pub fn set_prompt(&mut self, prompt: &str) {
        self.prompt = prompt.to_string();
    }

    /// Get the current prompt
    pub fn get_prompt(&self) -> &str {
        &self.prompt
    }

    /// Process raw bytes (for ANSI escape sequences, etc.)
    pub fn process(&mut self, data: &[u8]) -> crate::Result<()> {
        self.terminal.process(data)?;
        Ok(())
    }

    /// Write a line to terminal with newline
    pub fn write_line(&mut self, text: &str) -> crate::Result<()> {
        let line = format!("{}\r\n", text);
        self.terminal.process(line.as_bytes())?;
        Ok(())
    }

    /// Write an error message (formatted with ERROR: prefix)
    pub fn write_error(&mut self, error: &str) -> crate::Result<()> {
        self.write_line(&format!("ERROR: {}", error))
    }

    /// Write a success message
    pub fn write_success(&mut self, message: &str) -> crate::Result<()> {
        self.write_line(&format!("OK: {}", message))
    }

    /// Write query result as formatted table
    pub fn write_result(&mut self, result: &QueryResult) -> crate::Result<()> {
        if result.columns.is_empty() {
            if let Some(affected) = result.affected_rows {
                self.write_line(&format!("Query OK, {} row(s) affected", affected))?;
            } else {
                self.write_line("Query executed successfully")?;
            }
            return Ok(());
        }

        // Calculate column widths
        let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
        for row in &result.rows {
            for (i, val) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(val.len()).min(50); // Cap at 50 chars
                }
            }
        }

        // Write header
        let header = result
            .columns
            .iter()
            .zip(&widths)
            .map(|(col, width)| format!("{:<width$}", truncate_str(col, *width), width = width))
            .collect::<Vec<_>>()
            .join(" | ");
        self.write_line(&header)?;

        // Write separator
        let separator = widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-");
        self.write_line(&separator)?;

        // Write rows (limit to 100 for terminal display)
        let display_rows = result.rows.iter().take(100);
        for row in display_rows {
            let row_str = row
                .iter()
                .zip(&widths)
                .map(|(val, width)| format!("{:<width$}", truncate_str(val, *width), width = width))
                .collect::<Vec<_>>()
                .join(" | ");
            self.write_line(&row_str)?;
        }

        // Write summary
        if result.rows.len() > 100 {
            self.write_line(&format!(
                "\n({} rows total, showing first 100)",
                result.rows.len()
            ))?;
        } else {
            self.write_line(&format!("\n({} row(s))", result.rows.len()))?;
        }

        if let Some(time_ms) = result.execution_time_ms {
            self.write_line(&format!("Time: {}ms", time_ms))?;
        }

        Ok(())
    }

    /// Add a character to the input buffer and display
    pub fn add_char(&mut self, c: char) -> crate::Result<()> {
        self.input_buffer.push(c);
        self.terminal.process(&[c as u8])?;
        Ok(())
    }

    /// Handle backspace
    pub fn backspace(&mut self) -> crate::Result<()> {
        if !self.input_buffer.is_empty() {
            self.input_buffer.pop();
            self.terminal.process(b"\x08 \x08")?;
        }
        Ok(())
    }

    /// Get and clear the input buffer
    pub fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input_buffer)
    }

    /// Get current input without clearing
    pub fn current_input(&self) -> &str {
        &self.input_buffer
    }

    /// Clear input buffer
    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
    }

    /// Render terminal to JPEG
    pub fn render_jpeg(&mut self) -> crate::Result<Vec<u8>> {
        let (rows, cols) = self.terminal.size();
        let jpeg = self
            .renderer
            .render_screen(self.terminal.screen(), rows, cols)?;
        self.terminal.clear_dirty();
        Ok(jpeg)
    }

    /// Check if terminal needs re-render
    pub fn is_dirty(&self) -> bool {
        self.terminal.is_dirty()
    }

    /// Clear dirty flag after rendering
    pub fn clear_dirty(&mut self) {
        self.terminal.clear_dirty();
    }

    /// Format sync instruction to tell client to display buffered instructions
    pub fn format_sync_instruction(&self, timestamp_ms: u64) -> String {
        self.renderer.format_sync_instruction(timestamp_ms)
    }

    /// Get reference to the renderer (for dirty region optimization)
    pub fn renderer(&self) -> &TerminalRenderer {
        &self.renderer
    }

    /// Get reference to the terminal screen (for dirty region tracking)
    pub fn screen(&self) -> &vt100::Screen {
        self.terminal.screen()
    }

    /// Get database type
    pub fn db_type(&self) -> &str {
        &self.db_type
    }

    /// Get terminal size
    pub fn size(&self) -> (u16, u16) {
        self.terminal.size()
    }

    /// Get mutable reference to terminal emulator
    pub fn emulator_mut(&mut self) -> &mut TerminalEmulator {
        &mut self.terminal
    }

    /// Get reference to terminal emulator
    pub fn emulator(&self) -> &TerminalEmulator {
        &self.terminal
    }
}

/// Truncate string to max length with ellipsis
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

// ============================================================================
// Interactive Spreadsheet Grid (WhoDB-inspired)
// ============================================================================

/// Character cell dimensions (matching TerminalRenderer's 9x18 default for databases)
const GRID_CHAR_WIDTH: u32 = 9;
const GRID_CHAR_HEIGHT: u32 = 18;
/// Font size in points (70% of char_height is standard terminal practice)
const GRID_FONT_SIZE: f32 = 12.6;

/// Padding inside cells (left/right)
const CELL_PAD_X: u32 = 4;
/// Padding inside cells (top/bottom)
const CELL_PAD_Y: u32 = 2;
/// Height of each data row in pixels
const ROW_HEIGHT: u32 = GRID_CHAR_HEIGHT + CELL_PAD_Y * 2;
/// Height of the header row in pixels (slightly taller for visual weight)
const HEADER_ROW_HEIGHT: u32 = GRID_CHAR_HEIGHT + CELL_PAD_Y * 2 + 4;
/// Height of the action bar at the bottom
const ACTION_BAR_HEIGHT: u32 = ROW_HEIGHT + 2;
/// Minimum column width in characters
const MIN_COL_WIDTH_CHARS: usize = 4;
/// Maximum column width in characters
const MAX_COL_WIDTH_CHARS: usize = 50;
/// Width of the scrollbar indicator in pixels
const SCROLLBAR_WIDTH: u32 = 6;
/// Grid line thickness in pixels
const GRID_LINE_WIDTH: u32 = 1;

// -- Color palette (dark theme matching terminal look) --
const COLOR_BG: [u8; 3] = [30, 30, 30];
const COLOR_HEADER_BG: [u8; 3] = [50, 50, 60];
const COLOR_HEADER_FG: [u8; 3] = [220, 220, 230];
const COLOR_ROW_EVEN: [u8; 3] = [38, 38, 38];
const COLOR_ROW_ODD: [u8; 3] = [44, 44, 44];
const COLOR_CELL_FG: [u8; 3] = [210, 210, 210];
const COLOR_SELECTED_BG: [u8; 3] = [40, 70, 120];
const COLOR_SELECTED_FG: [u8; 3] = [255, 255, 255];
const COLOR_GRID_LINE: [u8; 3] = [60, 60, 60];
const COLOR_SCROLLBAR_BG: [u8; 3] = [50, 50, 50];
const COLOR_SCROLLBAR_FG: [u8; 3] = [120, 120, 120];
const COLOR_ACTION_BAR_BG: [u8; 3] = [35, 35, 45];
const COLOR_ACTION_SHORTCUT: [u8; 3] = [100, 180, 255];
const COLOR_ACTION_LABEL: [u8; 3] = [180, 180, 190];
const COLOR_STATUS_FG: [u8; 3] = [140, 140, 150];

// -- Guacamole keysym constants --
const KEYSYM_UP: u32 = 0xFF52;
const KEYSYM_DOWN: u32 = 0xFF54;
const KEYSYM_LEFT: u32 = 0xFF51;
const KEYSYM_RIGHT: u32 = 0xFF53;
const KEYSYM_PAGE_UP: u32 = 0xFF55;
const KEYSYM_PAGE_DOWN: u32 = 0xFF56;
const KEYSYM_HOME: u32 = 0xFF50;
const KEYSYM_END: u32 = 0xFF57;
const KEYSYM_RETURN: u32 = 0xFF0D;
const KEYSYM_TAB: u32 = 0xFF09;
const KEYSYM_ESCAPE: u32 = 0xFF1B;

/// Column alignment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
    Center,
}

/// Column definition for the grid
#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    /// Width in characters (auto-calculated if 0)
    pub width_chars: usize,
    pub alignment: Alignment,
}

impl ColumnDef {
    /// Create a left-aligned column with auto-sized width
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            width_chars: 0,
            alignment: Alignment::Left,
        }
    }

    /// Create a column with explicit width and alignment
    pub fn with_width(name: &str, width_chars: usize, alignment: Alignment) -> Self {
        Self {
            name: name.to_string(),
            width_chars,
            alignment,
        }
    }
}

/// Per-row action definition
#[derive(Debug, Clone)]
pub struct Action {
    pub label: String,
    pub shortcut: Option<char>,
    pub id: String,
}

impl Action {
    pub fn new(label: &str, shortcut: Option<char>, id: &str) -> Self {
        Self {
            label: label.to_string(),
            shortcut,
            id: id.to_string(),
        }
    }
}

/// Interaction modes for the grid state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMode {
    /// Normal browsing - arrow keys navigate rows
    Browse,
    /// Action bar focused - Tab cycles through actions
    ActionBar,
}

/// Events produced by input handling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridEvent {
    /// No action needed
    None,
    /// Grid content changed, needs re-render
    Redraw,
    /// User triggered an action on a row
    ActionTriggered { row: usize, action_id: String },
    /// User selected a specific cell (for copy)
    CellSelected {
        row: usize,
        col: usize,
        value: String,
    },
}

/// Interactive spreadsheet grid renderer
///
/// Renders tabular data to JPEG using fontdue (JetBrains Mono), matching
/// the visual quality of TerminalRenderer. Supports keyboard and mouse
/// navigation, row-level selection, and configurable per-row actions.
pub struct SpreadsheetRenderer {
    // Grid data
    columns: Vec<ColumnDef>,
    rows: Vec<Vec<String>>,

    // Viewport
    viewport_start_row: usize,
    viewport_rows: usize,
    viewport_start_col: usize,
    viewport_cols: usize,
    pixel_width: u32,
    pixel_height: u32,

    // Selection
    selected_row: Option<usize>,

    // Actions
    actions: Vec<Action>,
    focused_action: usize,

    // State machine
    mode: GridMode,

    // Rendering state
    dirty: bool,

    // Computed column pixel offsets (x position for each visible column)
    col_pixel_offsets: Vec<u32>,
    // Computed column pixel widths
    col_pixel_widths: Vec<u32>,
    // Total content width in pixels
    total_content_width: u32,

    // Font rendering
    font_primary: Font,
    font_fallback: Font,

    // Glyph cache: avoids re-rasterizing the same character every frame.
    // Keyed by char, stores (Metrics, bitmap) from fontdue. Uses RefCell
    // so render methods can populate the cache through &self references.
    glyph_cache: RefCell<HashMap<char, (fontdue::Metrics, Vec<u8>)>>,

    // Legacy compat: old selected_cell for backward compat
    legacy_selected_cell: Option<(usize, usize)>,
}

impl Default for SpreadsheetRenderer {
    fn default() -> Self {
        Self::new(1024, 768)
    }
}

impl SpreadsheetRenderer {
    /// Create a new SpreadsheetRenderer for the given pixel dimensions
    pub fn new(pixel_width: u32, pixel_height: u32) -> Self {
        let font_primary = Font::from_bytes(FONT_DATA_PRIMARY, FontSettings::default())
            .unwrap_or_else(|_| {
                Font::from_bytes(FONT_DATA_FALLBACK, FontSettings::default())
                    .expect("Failed to load any font")
            });
        let font_fallback = Font::from_bytes(FONT_DATA_FALLBACK, FontSettings::default())
            .unwrap_or_else(|_| {
                Font::from_bytes(FONT_DATA_PRIMARY, FontSettings::default())
                    .expect("Failed to load any font")
            });

        let mut renderer = Self {
            columns: Vec::new(),
            rows: Vec::new(),
            viewport_start_row: 0,
            viewport_rows: 0,
            viewport_start_col: 0,
            viewport_cols: 0,
            pixel_width,
            pixel_height,
            selected_row: None,
            actions: Vec::new(),
            focused_action: 0,
            mode: GridMode::Browse,
            dirty: true,
            col_pixel_offsets: Vec::new(),
            col_pixel_widths: Vec::new(),
            total_content_width: 0,
            font_primary,
            font_fallback,
            glyph_cache: RefCell::new(HashMap::new()),
            legacy_selected_cell: None,
        };
        renderer.recalculate_viewport();
        renderer
    }

    // -- Public data API --

    /// Set the grid data (columns and rows), recalculating layout
    pub fn set_data(&mut self, columns: Vec<ColumnDef>, rows: Vec<Vec<String>>) {
        self.columns = columns;
        self.rows = rows;
        self.viewport_start_row = 0;
        self.viewport_start_col = 0;
        self.selected_row = None;
        self.mode = GridMode::Browse;
        self.focused_action = 0;
        self.legacy_selected_cell = None;
        self.auto_size_columns();
        self.recalculate_viewport();
        self.dirty = true;
    }

    /// Set available actions (shown in the action bar)
    pub fn set_actions(&mut self, actions: Vec<Action>) {
        self.actions = actions;
        self.focused_action = 0;
        self.dirty = true;
    }

    /// Update rows without changing columns (for real-time updates)
    pub fn update_rows(&mut self, rows: Vec<Vec<String>>) {
        self.rows = rows;

        // Re-auto-size columns since data changed
        self.auto_size_columns();
        self.recalculate_viewport();

        // Clamp selection and viewport if rows were removed
        if let Some(sel) = self.selected_row {
            if !self.rows.is_empty() {
                if sel >= self.rows.len() {
                    self.selected_row = Some(self.rows.len() - 1);
                }
            } else {
                self.selected_row = None;
            }
        }
        if !self.rows.is_empty() {
            let max_start = self.rows.len().saturating_sub(self.viewport_rows);
            if self.viewport_start_row > max_start {
                self.viewport_start_row = max_start;
            }
        } else {
            self.viewport_start_row = 0;
        }

        self.dirty = true;
    }

    /// Resize the grid to new pixel dimensions
    pub fn resize(&mut self, pixel_width: u32, pixel_height: u32) {
        if self.pixel_width != pixel_width || self.pixel_height != pixel_height {
            self.pixel_width = pixel_width;
            self.pixel_height = pixel_height;
            self.recalculate_viewport();
            self.dirty = true;
        }
    }

    // -- Input handling --

    /// Handle a keyboard event
    ///
    /// `keysym` is the X11 keysym value from Guacamole `key` instructions.
    /// `pressed` is true on key-down, false on key-up.
    pub fn handle_key(&mut self, keysym: u32, pressed: bool) -> GridEvent {
        if !pressed {
            return GridEvent::None;
        }

        match self.mode {
            GridMode::Browse => self.handle_key_browse(keysym),
            GridMode::ActionBar => self.handle_key_action_bar(keysym),
        }
    }

    /// Handle a mouse click
    ///
    /// `x` and `y` are pixel coordinates. `button_mask` is the Guacamole button mask
    /// (bit 0 = left, bit 1 = middle, bit 2 = right, bit 3 = scroll up, bit 4 = scroll down).
    pub fn handle_mouse(&mut self, x: u32, y: u32, button_mask: u32) -> GridEvent {
        // Scroll up (button mask bit 3)
        if button_mask & 0x08 != 0 {
            return self.handle_scroll(-3);
        }
        // Scroll down (button mask bit 4)
        if button_mask & 0x10 != 0 {
            return self.handle_scroll(3);
        }

        // Left click (button mask bit 0)
        if button_mask & 0x01 == 0 {
            return GridEvent::None;
        }

        let action_bar_y = self.action_bar_top();

        // Click in action bar
        if !self.actions.is_empty() && y >= action_bar_y {
            return self.handle_action_bar_click(x);
        }

        // Click in header row (no action for now)
        if y < HEADER_ROW_HEIGHT {
            return GridEvent::None;
        }

        // Click in data area
        let data_y = y - HEADER_ROW_HEIGHT;
        let clicked_visible_row = (data_y / ROW_HEIGHT) as usize;
        let absolute_row = self.viewport_start_row + clicked_visible_row;

        if absolute_row < self.rows.len() {
            let old_selection = self.selected_row;
            self.selected_row = Some(absolute_row);
            self.mode = GridMode::Browse;

            // Determine which column was clicked (for CellSelected)
            if let Some(col_idx) = self.pixel_x_to_col(x) {
                let abs_col = self.viewport_start_col + col_idx;
                if abs_col < self.columns.len() {
                    let value = self
                        .rows
                        .get(absolute_row)
                        .and_then(|r| r.get(abs_col))
                        .cloned()
                        .unwrap_or_default();
                    // Update legacy compat
                    self.legacy_selected_cell = Some((absolute_row, abs_col));
                    self.dirty = true;
                    return GridEvent::CellSelected {
                        row: absolute_row,
                        col: abs_col,
                        value,
                    };
                }
            }

            if old_selection != self.selected_row {
                self.dirty = true;
                return GridEvent::Redraw;
            }
        }

        GridEvent::None
    }

    /// Handle scroll wheel (positive = down, negative = up)
    pub fn handle_scroll(&mut self, delta: i32) -> GridEvent {
        if self.rows.is_empty() {
            return GridEvent::None;
        }

        let max_start = self.rows.len().saturating_sub(self.viewport_rows);
        let old_start = self.viewport_start_row;

        if delta < 0 {
            self.viewport_start_row = self.viewport_start_row.saturating_sub((-delta) as usize);
        } else {
            self.viewport_start_row = (self.viewport_start_row + delta as usize).min(max_start);
        }

        if self.viewport_start_row != old_start {
            self.dirty = true;
            GridEvent::Redraw
        } else {
            GridEvent::None
        }
    }

    // -- Rendering --

    /// Render the grid to JPEG at the specified quality (1-100)
    pub fn render_jpeg(&self, quality: u8) -> crate::Result<Vec<u8>> {
        if self.pixel_width == 0 || self.pixel_height == 0 {
            return Err(crate::TerminalError::RenderError(
                "Cannot render zero-size grid".to_string(),
            ));
        }

        let mut img = RgbImage::new(self.pixel_width, self.pixel_height);

        // Fill background
        let bg = Rgb(COLOR_BG);
        for pixel in img.pixels_mut() {
            *pixel = bg;
        }

        // Draw header row
        self.draw_header_row(&mut img);

        // Draw data rows
        self.draw_data_rows(&mut img);

        // Draw grid lines
        self.draw_grid_lines(&mut img);

        // Draw scrollbar
        self.draw_scrollbar(&mut img);

        // Draw action bar
        if !self.actions.is_empty() {
            self.draw_action_bar(&mut img);
        }

        // Draw status line (row count / position)
        self.draw_status_line(&mut img);

        // Encode to JPEG
        let mut jpeg_data = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
        encoder.encode_image(&img).map_err(|e| {
            crate::TerminalError::RenderError(format!("JPEG encoding failed: {}", e))
        })?;

        Ok(jpeg_data)
    }

    /// Check if the grid needs re-rendering
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the dirty flag after rendering
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    // -- State accessors --

    /// Get the currently selected row index
    pub fn selected_row(&self) -> Option<usize> {
        self.selected_row
    }

    /// Get the value from the selected row's first column (convenience)
    pub fn get_selected_value(&self) -> Option<String> {
        self.selected_row
            .and_then(|row_idx| self.rows.get(row_idx).and_then(|row| row.first().cloned()))
    }

    /// Get a specific cell value
    pub fn get_cell_value(&self, row: usize, col: usize) -> Option<String> {
        self.rows.get(row).and_then(|r| r.get(col).cloned())
    }

    /// Get the number of data rows
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Get the number of columns
    pub fn col_count(&self) -> usize {
        self.columns.len()
    }

    /// Get the current grid mode
    pub fn mode(&self) -> GridMode {
        self.mode
    }

    /// Get viewport information: (start_row, visible_rows, total_rows)
    pub fn viewport_info(&self) -> (usize, usize, usize) {
        (self.viewport_start_row, self.viewport_rows, self.rows.len())
    }

    // -- Backward compatibility (deprecated wrappers) --

    /// DEPRECATED: Use `handle_mouse` instead.
    ///
    /// Legacy API: handle mouse click with explicit QueryResult reference.
    /// Maps the old cell-based selection to the new row-based model.
    #[deprecated(note = "Use handle_mouse() instead")]
    pub fn handle_click(&mut self, x: u32, y: u32, result: &QueryResult) -> bool {
        // Temporarily set data from result if we have no data
        let had_data = !self.columns.is_empty();
        if !had_data {
            let cols: Vec<ColumnDef> = result.columns.iter().map(|c| ColumnDef::new(c)).collect();
            self.set_data(cols, result.rows.clone());
        }

        let event = self.handle_mouse(x, y, 1); // left click

        match event {
            GridEvent::CellSelected { row, col, .. } => {
                self.legacy_selected_cell = Some((row, col));
                true
            }
            GridEvent::Redraw => {
                if let Some(sel) = self.selected_row {
                    self.legacy_selected_cell = Some((sel, 0));
                }
                true
            }
            _ => false,
        }
    }

    /// DEPRECATED: Use `handle_scroll` instead.
    ///
    /// Legacy API: scroll by delta rows with explicit QueryResult reference.
    #[deprecated(note = "Use handle_scroll() instead")]
    pub fn scroll(&mut self, delta: i32, result: &QueryResult) {
        // Temporarily set data from result if we have no data
        let had_data = !self.columns.is_empty();
        if !had_data {
            let cols: Vec<ColumnDef> = result.columns.iter().map(|c| ColumnDef::new(c)).collect();
            self.set_data(cols, result.rows.clone());
        }

        self.handle_scroll(delta);
    }

    /// DEPRECATED: Use `get_selected_value` or `get_cell_value` instead.
    ///
    /// Legacy API: get selected cell value using the old cell-based selection.
    #[deprecated(note = "Use get_selected_value() or get_cell_value() instead")]
    pub fn get_selected_value_legacy(&self, result: &QueryResult) -> Option<String> {
        if let Some((row, col)) = self.legacy_selected_cell {
            result.rows.get(row)?.get(col).cloned()
        } else {
            None
        }
    }

    /// DEPRECATED: Use `render_jpeg` instead.
    ///
    /// Legacy API: render to PNG. Now renders JPEG internally and wraps the result.
    /// Callers should migrate to `render_jpeg` for better performance.
    #[deprecated(note = "Use render_jpeg() instead for JPEG output")]
    pub fn render_to_png(
        &self,
        result: &QueryResult,
        width: u32,
        height: u32,
    ) -> crate::Result<Vec<u8>> {
        // Create a temporary renderer at the requested dimensions
        let mut temp = SpreadsheetRenderer::new(width, height);
        let cols: Vec<ColumnDef> = result.columns.iter().map(|c| ColumnDef::new(c)).collect();
        temp.set_data(cols, result.rows.clone());

        // For true backward compat, render as PNG using image crate
        let mut img = RgbImage::new(width, height);
        let bg = Rgb(COLOR_BG);
        for pixel in img.pixels_mut() {
            *pixel = bg;
        }
        temp.draw_header_row(&mut img);
        temp.draw_data_rows(&mut img);
        temp.draw_grid_lines(&mut img);
        temp.draw_scrollbar(&mut img);
        temp.draw_status_line(&mut img);

        let mut png_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_data),
            image::ImageFormat::Png,
        )?;
        Ok(png_data)
    }

    // -- Private: layout calculation --

    /// Auto-size column widths based on header names and data content
    fn auto_size_columns(&mut self) {
        for (col_idx, col) in self.columns.iter_mut().enumerate() {
            if col.width_chars == 0 {
                // Start with header width
                let mut max_width = col.name.len();

                // Sample rows for width (sample up to 200 rows for speed)
                let sample_count = self.rows.len().min(200);
                for row in self.rows.iter().take(sample_count) {
                    if let Some(val) = row.get(col_idx) {
                        max_width = max_width.max(val.chars().count());
                    }
                }

                col.width_chars = max_width.clamp(MIN_COL_WIDTH_CHARS, MAX_COL_WIDTH_CHARS);
            } else {
                col.width_chars = col
                    .width_chars
                    .clamp(MIN_COL_WIDTH_CHARS, MAX_COL_WIDTH_CHARS);
            }
        }
    }

    /// Recalculate viewport dimensions and column pixel positions
    fn recalculate_viewport(&mut self) {
        // Calculate available height for data rows
        let action_bar = if self.actions.is_empty() {
            0
        } else {
            ACTION_BAR_HEIGHT
        };
        let status_height = ROW_HEIGHT;
        let available_height = self
            .pixel_height
            .saturating_sub(HEADER_ROW_HEIGHT + action_bar + status_height);
        self.viewport_rows = (available_height / ROW_HEIGHT).max(1) as usize;

        // Calculate column pixel widths and offsets
        self.col_pixel_widths.clear();
        self.col_pixel_offsets.clear();

        let mut x = 0u32;
        for col in &self.columns {
            let width_px = (col.width_chars as u32 * GRID_CHAR_WIDTH) + CELL_PAD_X * 2;
            self.col_pixel_offsets.push(x);
            self.col_pixel_widths.push(width_px);
            x += width_px + GRID_LINE_WIDTH;
        }
        self.total_content_width = x;

        // Calculate visible columns
        let usable_width = self.pixel_width.saturating_sub(SCROLLBAR_WIDTH);
        let mut visible = 0;
        let mut consumed = 0u32;
        for idx in self.viewport_start_col..self.columns.len() {
            let w = self.col_pixel_widths.get(idx).copied().unwrap_or(0);
            if consumed + w > usable_width && visible > 0 {
                break;
            }
            consumed += w + GRID_LINE_WIDTH;
            visible += 1;
        }
        self.viewport_cols = visible.max(1);
    }

    /// Map a pixel x-coordinate to a visible column index (0-based within viewport)
    fn pixel_x_to_col(&self, x: u32) -> Option<usize> {
        let mut col_x = 0u32;
        for vis_idx in 0..self.viewport_cols {
            let abs_col = self.viewport_start_col + vis_idx;
            let w = self.col_pixel_widths.get(abs_col).copied().unwrap_or(0);
            if x >= col_x && x < col_x + w {
                return Some(vis_idx);
            }
            col_x += w + GRID_LINE_WIDTH;
        }
        None
    }

    /// Y position where the action bar starts
    fn action_bar_top(&self) -> u32 {
        self.pixel_height
            .saturating_sub(ACTION_BAR_HEIGHT + ROW_HEIGHT)
    }

    /// Ensure the selected row is visible in the viewport
    fn ensure_selected_visible(&mut self) {
        if let Some(sel) = self.selected_row {
            if sel < self.viewport_start_row {
                self.viewport_start_row = sel;
            } else if sel >= self.viewport_start_row + self.viewport_rows {
                self.viewport_start_row = sel.saturating_sub(self.viewport_rows - 1);
            }
        }
    }

    // -- Private: keyboard handling --

    fn handle_key_browse(&mut self, keysym: u32) -> GridEvent {
        match keysym {
            KEYSYM_UP => {
                if let Some(sel) = self.selected_row {
                    if sel > 0 {
                        self.selected_row = Some(sel - 1);
                        self.ensure_selected_visible();
                        self.dirty = true;
                        return GridEvent::Redraw;
                    }
                } else if !self.rows.is_empty() {
                    self.selected_row = Some(0);
                    self.ensure_selected_visible();
                    self.dirty = true;
                    return GridEvent::Redraw;
                }
                GridEvent::None
            }
            KEYSYM_DOWN => {
                if let Some(sel) = self.selected_row {
                    if sel + 1 < self.rows.len() {
                        self.selected_row = Some(sel + 1);
                        self.ensure_selected_visible();
                        self.dirty = true;
                        return GridEvent::Redraw;
                    }
                } else if !self.rows.is_empty() {
                    self.selected_row = Some(0);
                    self.ensure_selected_visible();
                    self.dirty = true;
                    return GridEvent::Redraw;
                }
                GridEvent::None
            }
            KEYSYM_PAGE_UP => {
                if self.rows.is_empty() {
                    return GridEvent::None;
                }
                let jump = self.viewport_rows;
                let sel = self.selected_row.unwrap_or(0);
                self.selected_row = Some(sel.saturating_sub(jump));
                self.ensure_selected_visible();
                self.dirty = true;
                GridEvent::Redraw
            }
            KEYSYM_PAGE_DOWN => {
                if self.rows.is_empty() {
                    return GridEvent::None;
                }
                let jump = self.viewport_rows;
                let sel = self.selected_row.unwrap_or(0);
                self.selected_row = Some((sel + jump).min(self.rows.len().saturating_sub(1)));
                self.ensure_selected_visible();
                self.dirty = true;
                GridEvent::Redraw
            }
            KEYSYM_HOME => {
                if self.rows.is_empty() {
                    return GridEvent::None;
                }
                self.selected_row = Some(0);
                self.ensure_selected_visible();
                self.dirty = true;
                GridEvent::Redraw
            }
            KEYSYM_END => {
                if self.rows.is_empty() {
                    return GridEvent::None;
                }
                self.selected_row = Some(self.rows.len().saturating_sub(1));
                self.ensure_selected_visible();
                self.dirty = true;
                GridEvent::Redraw
            }
            KEYSYM_LEFT => {
                if self.viewport_start_col > 0 {
                    self.viewport_start_col -= 1;
                    self.recalculate_viewport();
                    self.dirty = true;
                    return GridEvent::Redraw;
                }
                GridEvent::None
            }
            KEYSYM_RIGHT => {
                if self.viewport_start_col + self.viewport_cols < self.columns.len() {
                    self.viewport_start_col += 1;
                    self.recalculate_viewport();
                    self.dirty = true;
                    return GridEvent::Redraw;
                }
                GridEvent::None
            }
            KEYSYM_TAB => {
                if !self.actions.is_empty() {
                    self.mode = GridMode::ActionBar;
                    self.focused_action = 0;
                    self.dirty = true;
                    return GridEvent::Redraw;
                }
                GridEvent::None
            }
            KEYSYM_RETURN => {
                // Enter on selected row triggers first action (if any)
                if let (Some(row), Some(action)) = (self.selected_row, self.actions.first()) {
                    return GridEvent::ActionTriggered {
                        row,
                        action_id: action.id.clone(),
                    };
                }
                GridEvent::None
            }
            _ => {
                // Check action shortcuts
                if let Some(c) = char::from_u32(keysym) {
                    let lower = c.to_ascii_lowercase();
                    for action in &self.actions {
                        if let Some(shortcut) = action.shortcut {
                            if shortcut.to_ascii_lowercase() == lower {
                                if let Some(row) = self.selected_row {
                                    return GridEvent::ActionTriggered {
                                        row,
                                        action_id: action.id.clone(),
                                    };
                                }
                            }
                        }
                    }
                }
                GridEvent::None
            }
        }
    }

    fn handle_key_action_bar(&mut self, keysym: u32) -> GridEvent {
        match keysym {
            KEYSYM_ESCAPE => {
                self.mode = GridMode::Browse;
                self.dirty = true;
                GridEvent::Redraw
            }
            KEYSYM_TAB | KEYSYM_RIGHT => {
                if !self.actions.is_empty() {
                    self.focused_action = (self.focused_action + 1) % self.actions.len();
                    self.dirty = true;
                    GridEvent::Redraw
                } else {
                    GridEvent::None
                }
            }
            KEYSYM_LEFT => {
                if !self.actions.is_empty() {
                    if self.focused_action == 0 {
                        self.focused_action = self.actions.len() - 1;
                    } else {
                        self.focused_action -= 1;
                    }
                    self.dirty = true;
                    GridEvent::Redraw
                } else {
                    GridEvent::None
                }
            }
            KEYSYM_RETURN => {
                if let Some(row) = self.selected_row {
                    if let Some(action) = self.actions.get(self.focused_action) {
                        self.mode = GridMode::Browse;
                        return GridEvent::ActionTriggered {
                            row,
                            action_id: action.id.clone(),
                        };
                    }
                }
                GridEvent::None
            }
            _ => GridEvent::None,
        }
    }

    fn handle_action_bar_click(&mut self, x: u32) -> GridEvent {
        // Determine which action was clicked based on x position
        let mut action_x = CELL_PAD_X;
        for (idx, action) in self.actions.iter().enumerate() {
            let label_width = self.action_label_pixel_width(action);
            if x >= action_x && x < action_x + label_width {
                self.focused_action = idx;
                if let Some(row) = self.selected_row {
                    return GridEvent::ActionTriggered {
                        row,
                        action_id: action.id.clone(),
                    };
                }
                return GridEvent::None;
            }
            action_x += label_width + GRID_CHAR_WIDTH * 2;
        }
        GridEvent::None
    }

    fn action_label_pixel_width(&self, action: &Action) -> u32 {
        // "[S]hell" = 3 for bracket+char+bracket, plus label length
        let shortcut_width = if action.shortcut.is_some() { 3 } else { 0 };
        let text_width = action.label.len() + shortcut_width;
        (text_width as u32) * GRID_CHAR_WIDTH + CELL_PAD_X * 2
    }

    // -- Private: drawing --

    fn draw_header_row(&self, img: &mut RgbImage) {
        let header_bg = Rgb(COLOR_HEADER_BG);
        let header_fg = COLOR_HEADER_FG;

        // Fill header background
        for py in 0..HEADER_ROW_HEIGHT.min(img.height()) {
            for px in 0..img.width() {
                img.put_pixel(px, py, header_bg);
            }
        }

        // Draw column headers
        let mut x = 0u32;
        for vis_idx in 0..self.viewport_cols {
            let abs_col = self.viewport_start_col + vis_idx;
            if abs_col >= self.columns.len() {
                break;
            }
            let col = &self.columns[abs_col];
            let col_w = self.col_pixel_widths.get(abs_col).copied().unwrap_or(0);

            let text_x = x + CELL_PAD_X;
            let text_y = (HEADER_ROW_HEIGHT - GRID_CHAR_HEIGHT) / 2;
            let max_chars = ((col_w - CELL_PAD_X * 2) / GRID_CHAR_WIDTH) as usize;

            self.draw_text_fontdue(img, &col.name, text_x, text_y, header_fg, max_chars);

            x += col_w + GRID_LINE_WIDTH;
        }
    }

    fn draw_data_rows(&self, img: &mut RgbImage) {
        let even_bg = Rgb(COLOR_ROW_EVEN);
        let odd_bg = Rgb(COLOR_ROW_ODD);
        let selected_bg = Rgb(COLOR_SELECTED_BG);
        let cell_fg = COLOR_CELL_FG;
        let selected_fg = COLOR_SELECTED_FG;

        for vis_row in 0..self.viewport_rows {
            let abs_row = self.viewport_start_row + vis_row;
            if abs_row >= self.rows.len() {
                break;
            }

            let row_y = HEADER_ROW_HEIGHT + (vis_row as u32) * ROW_HEIGHT;
            let is_selected = self.selected_row == Some(abs_row);

            // Row background
            let bg = if is_selected {
                selected_bg
            } else if abs_row.is_multiple_of(2) {
                even_bg
            } else {
                odd_bg
            };

            // Fill row background
            for py in row_y..(row_y + ROW_HEIGHT).min(img.height()) {
                for px in 0..self
                    .pixel_width
                    .saturating_sub(SCROLLBAR_WIDTH)
                    .min(img.width())
                {
                    img.put_pixel(px, py, bg);
                }
            }

            // Draw cell values
            let fg = if is_selected { selected_fg } else { cell_fg };
            let row_data = &self.rows[abs_row];

            let mut x = 0u32;
            for vis_col in 0..self.viewport_cols {
                let abs_col = self.viewport_start_col + vis_col;
                if abs_col >= self.columns.len() {
                    break;
                }
                let col_w = self.col_pixel_widths.get(abs_col).copied().unwrap_or(0);
                let max_chars = ((col_w.saturating_sub(CELL_PAD_X * 2)) / GRID_CHAR_WIDTH) as usize;

                let value = row_data.get(abs_col).map(|s| s.as_str()).unwrap_or("");
                let text_x = x + CELL_PAD_X;
                let text_y = row_y + CELL_PAD_Y;

                // Apply alignment
                let alignment = self
                    .columns
                    .get(abs_col)
                    .map(|c| c.alignment)
                    .unwrap_or(Alignment::Left);

                self.draw_text_aligned(img, value, text_x, text_y, fg, max_chars, col_w, alignment);

                x += col_w + GRID_LINE_WIDTH;
            }
        }
    }

    fn draw_grid_lines(&self, img: &mut RgbImage) {
        let grid_color = Rgb(COLOR_GRID_LINE);
        let usable_width = self.pixel_width.saturating_sub(SCROLLBAR_WIDTH);

        // Horizontal lines
        // Line under header
        let header_line_y = HEADER_ROW_HEIGHT;
        if header_line_y < img.height() {
            for px in 0..usable_width.min(img.width()) {
                img.put_pixel(px, header_line_y, grid_color);
            }
        }

        // Lines between data rows
        let visible = self
            .viewport_rows
            .min(self.rows.len().saturating_sub(self.viewport_start_row));
        for vis_row in 1..=visible {
            let y = HEADER_ROW_HEIGHT + (vis_row as u32) * ROW_HEIGHT;
            if y < img.height() {
                for px in 0..usable_width.min(img.width()) {
                    img.put_pixel(px, y, grid_color);
                }
            }
        }

        // Vertical lines between columns
        let mut x = 0u32;
        for vis_col in 0..self.viewport_cols {
            let abs_col = self.viewport_start_col + vis_col;
            let col_w = self.col_pixel_widths.get(abs_col).copied().unwrap_or(0);
            x += col_w;
            if x < usable_width && x < img.width() {
                let max_y = HEADER_ROW_HEIGHT + (visible as u32) * ROW_HEIGHT;
                for py in 0..max_y.min(img.height()) {
                    img.put_pixel(x, py, grid_color);
                }
            }
            x += GRID_LINE_WIDTH;
        }
    }

    fn draw_scrollbar(&self, img: &mut RgbImage) {
        if self.rows.is_empty() {
            return;
        }

        let sb_x = self.pixel_width.saturating_sub(SCROLLBAR_WIDTH);
        let sb_top = HEADER_ROW_HEIGHT;
        let action_bar = if self.actions.is_empty() {
            0
        } else {
            ACTION_BAR_HEIGHT
        };
        let status_h = ROW_HEIGHT;
        let sb_height = self
            .pixel_height
            .saturating_sub(sb_top + action_bar + status_h);

        if sb_height < 4 {
            return;
        }

        // Scrollbar background
        let sb_bg = Rgb(COLOR_SCROLLBAR_BG);
        for py in sb_top..(sb_top + sb_height).min(img.height()) {
            for px in sb_x..self.pixel_width.min(img.width()) {
                img.put_pixel(px, py, sb_bg);
            }
        }

        // Scrollbar thumb
        let total_rows = self.rows.len().max(1);
        let visible_fraction = (self.viewport_rows as f64 / total_rows as f64).min(1.0);
        let thumb_height = ((sb_height as f64) * visible_fraction).max(8.0) as u32;
        let scroll_fraction = if total_rows <= self.viewport_rows {
            0.0
        } else {
            self.viewport_start_row as f64 / (total_rows - self.viewport_rows) as f64
        };
        let thumb_y =
            sb_top + ((sb_height.saturating_sub(thumb_height)) as f64 * scroll_fraction) as u32;

        let sb_fg = Rgb(COLOR_SCROLLBAR_FG);
        for py in thumb_y
            ..(thumb_y + thumb_height)
                .min(sb_top + sb_height)
                .min(img.height())
        {
            for px in (sb_x + 1)..self.pixel_width.saturating_sub(1).min(img.width()) {
                img.put_pixel(px, py, sb_fg);
            }
        }
    }

    fn draw_action_bar(&self, img: &mut RgbImage) {
        let bar_y = self.action_bar_top();
        let bar_bg = Rgb(COLOR_ACTION_BAR_BG);

        // Fill action bar background
        for py in bar_y..(bar_y + ACTION_BAR_HEIGHT).min(img.height()) {
            for px in 0..img.width() {
                img.put_pixel(px, py, bar_bg);
            }
        }

        // Draw action labels
        let mut x = CELL_PAD_X;
        let text_y = bar_y + CELL_PAD_Y + 2;

        for (idx, action) in self.actions.iter().enumerate() {
            let is_focused = self.mode == GridMode::ActionBar && self.focused_action == idx;

            if let Some(shortcut) = action.shortcut {
                // Draw "[X]" with highlight color
                let shortcut_color = if is_focused {
                    COLOR_SELECTED_FG
                } else {
                    COLOR_ACTION_SHORTCUT
                };
                let bracket_str = format!("[{}]", shortcut.to_ascii_uppercase());
                self.draw_text_fontdue(img, &bracket_str, x, text_y, shortcut_color, 3);
                x += 3 * GRID_CHAR_WIDTH;
            }

            // Draw label
            let label_color = if is_focused {
                COLOR_SELECTED_FG
            } else {
                COLOR_ACTION_LABEL
            };
            self.draw_text_fontdue(
                img,
                &action.label,
                x,
                text_y,
                label_color,
                action.label.len(),
            );
            x += (action.label.len() as u32) * GRID_CHAR_WIDTH + GRID_CHAR_WIDTH * 2;
        }
    }

    fn draw_status_line(&self, img: &mut RgbImage) {
        let status_y = self.pixel_height.saturating_sub(ROW_HEIGHT);
        let text_y = status_y + CELL_PAD_Y;

        // Build status text
        let total = self.rows.len();
        let status = if total == 0 {
            "No rows".to_string()
        } else if let Some(sel) = self.selected_row {
            format!(
                "Row {}/{} | Showing {}-{}",
                sel + 1,
                total,
                self.viewport_start_row + 1,
                (self.viewport_start_row + self.viewport_rows).min(total)
            )
        } else {
            format!(
                "{} rows | Showing {}-{}",
                total,
                self.viewport_start_row + 1,
                (self.viewport_start_row + self.viewport_rows).min(total)
            )
        };

        self.draw_text_fontdue(img, &status, CELL_PAD_X, text_y, COLOR_STATUS_FG, 80);
    }

    // -- Private: fontdue text rendering --

    /// Rasterize a character, returning cached (Metrics, bitmap) if available.
    ///
    /// On first call for a given char, rasterizes with the primary font
    /// (falling back to the secondary font if the primary has no glyph),
    /// then stores the result in `glyph_cache` so subsequent frames skip
    /// the rasterization entirely.
    fn rasterize_cached(&self, c: char) -> (fontdue::Metrics, Vec<u8>) {
        // Fast path: already cached
        {
            let cache = self.glyph_cache.borrow();
            if let Some(entry) = cache.get(&c) {
                return entry.clone();
            }
        }

        // Slow path: rasterize, pick best font, then cache
        let (metrics, bitmap) = self.font_primary.rasterize(c, GRID_FONT_SIZE);
        let result = if metrics.width > 0 {
            (metrics, bitmap)
        } else {
            let (metrics_fb, bitmap_fb) = self.font_fallback.rasterize(c, GRID_FONT_SIZE);
            if metrics_fb.width > 0 {
                (metrics_fb, bitmap_fb)
            } else {
                // Neither font has the glyph; store the primary result so we
                // do not re-rasterize on every frame.
                (metrics, bitmap)
            }
        };

        self.glyph_cache.borrow_mut().insert(c, result.clone());
        result
    }

    /// Render text using fontdue at the given pixel position
    ///
    /// Truncates to `max_chars` with ellipsis if needed.
    fn draw_text_fontdue(
        &self,
        img: &mut RgbImage,
        text: &str,
        x: u32,
        y: u32,
        color: [u8; 3],
        max_chars: usize,
    ) {
        if max_chars == 0 {
            return;
        }

        let display_text: String = if text.chars().count() > max_chars {
            if max_chars > 3 {
                let truncated: String = text.chars().take(max_chars - 3).collect();
                format!("{}...", truncated)
            } else {
                text.chars().take(max_chars).collect()
            }
        } else {
            text.to_string()
        };

        let fg = Rgb(color);

        // Baseline at 75% of char height (matching TerminalRenderer)
        let baseline_y = y as f32 + (GRID_CHAR_HEIGHT as f32 * 0.75);

        for (i, c) in display_text.chars().enumerate() {
            let char_x = x + (i as u32) * GRID_CHAR_WIDTH;
            if char_x + GRID_CHAR_WIDTH > img.width() {
                break;
            }

            if c == ' ' {
                continue;
            }

            let (metrics, bitmap) = self.rasterize_cached(c);

            if bitmap.is_empty() || metrics.width == 0 {
                continue;
            }

            // Horizontal: center glyph in cell
            let glyph_x =
                char_x + ((GRID_CHAR_WIDTH as i32 - metrics.width as i32) / 2).max(0) as u32;

            // Vertical: baseline alignment (same as TerminalRenderer)
            let glyph_y =
                (baseline_y - metrics.bounds.height - metrics.bounds.ymin).max(y as f32) as u32;

            // Draw glyph with alpha blending
            for (bi, &alpha) in bitmap.iter().enumerate() {
                if alpha > 0 {
                    let dx = (bi % metrics.width) as u32;
                    let dy = (bi / metrics.width) as u32;
                    let px = glyph_x + dx;
                    let py = glyph_y + dy;

                    if px < img.width() && py < img.height() {
                        let alpha_f = alpha as f32 / 255.0;
                        let current = img.get_pixel(px, py);
                        let blended = Rgb([
                            ((fg.0[0] as f32 * alpha_f) + (current.0[0] as f32 * (1.0 - alpha_f)))
                                as u8,
                            ((fg.0[1] as f32 * alpha_f) + (current.0[1] as f32 * (1.0 - alpha_f)))
                                as u8,
                            ((fg.0[2] as f32 * alpha_f) + (current.0[2] as f32 * (1.0 - alpha_f)))
                                as u8,
                        ]);
                        img.put_pixel(px, py, blended);
                    }
                }
            }
        }
    }

    /// Render text with alignment within a column
    #[allow(clippy::too_many_arguments)]
    fn draw_text_aligned(
        &self,
        img: &mut RgbImage,
        text: &str,
        x: u32,
        y: u32,
        color: [u8; 3],
        max_chars: usize,
        col_width_px: u32,
        alignment: Alignment,
    ) {
        if max_chars == 0 {
            return;
        }

        let char_count = text.chars().count().min(max_chars);
        let text_width_px = char_count as u32 * GRID_CHAR_WIDTH;
        let available_px = col_width_px.saturating_sub(CELL_PAD_X * 2);

        let draw_x = match alignment {
            Alignment::Left => x,
            Alignment::Right => {
                if text_width_px < available_px {
                    x + (available_px - text_width_px)
                } else {
                    x
                }
            }
            Alignment::Center => {
                if text_width_px < available_px {
                    x + (available_px - text_width_px) / 2
                } else {
                    x
                }
            }
        };

        self.draw_text_fontdue(img, text, draw_x, y, color, max_chars);
    }
}

// Allow legacy code that accesses `selected_cell` directly
impl SpreadsheetRenderer {
    /// Legacy access to selected_cell field (for backward compat).
    /// Returns the legacy cell-based selection.
    pub fn selected_cell(&self) -> Option<(usize, usize)> {
        self.legacy_selected_cell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create sample data
    fn sample_query_result() -> QueryResult {
        let mut result = QueryResult::new(vec![
            "id".to_string(),
            "name".to_string(),
            "email".to_string(),
        ]);
        result.add_row(vec![
            "1".to_string(),
            "Alice".to_string(),
            "alice@example.com".to_string(),
        ]);
        result.add_row(vec![
            "2".to_string(),
            "Bob".to_string(),
            "bob@example.com".to_string(),
        ]);
        result.add_row(vec![
            "3".to_string(),
            "Charlie".to_string(),
            "charlie@example.com".to_string(),
        ]);
        result
    }

    fn sample_columns() -> Vec<ColumnDef> {
        vec![
            ColumnDef::new("id"),
            ColumnDef::new("name"),
            ColumnDef::new("email"),
        ]
    }

    fn sample_rows() -> Vec<Vec<String>> {
        vec![
            vec![
                "1".to_string(),
                "Alice".to_string(),
                "alice@example.com".to_string(),
            ],
            vec![
                "2".to_string(),
                "Bob".to_string(),
                "bob@example.com".to_string(),
            ],
            vec![
                "3".to_string(),
                "Charlie".to_string(),
                "charlie@example.com".to_string(),
            ],
        ]
    }

    #[test]
    fn test_query_result() {
        let mut result = QueryResult::new(vec!["id".to_string(), "name".to_string()]);
        result.add_row(vec!["1".to_string(), "Alice".to_string()]);
        result.add_row(vec!["2".to_string(), "Bob".to_string()]);

        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.row_count(), 2);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_database_terminal() {
        let term = DatabaseTerminal::new(24, 80, "mysql> ", "mysql");
        assert!(term.is_ok());

        let term = term.unwrap();
        assert_eq!(term.db_type(), "mysql");
        assert_eq!(term.size(), (24, 80));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("ab", 2), "ab");
    }

    #[test]
    fn test_create_renderer() {
        let renderer = SpreadsheetRenderer::new(1024, 768);
        assert_eq!(renderer.pixel_width, 1024);
        assert_eq!(renderer.pixel_height, 768);
        assert!(renderer.is_dirty());
        assert!(renderer.selected_row().is_none());
        assert_eq!(renderer.row_count(), 0);
        assert_eq!(renderer.col_count(), 0);
    }

    #[test]
    fn test_set_data() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());

        assert_eq!(renderer.col_count(), 3);
        assert_eq!(renderer.row_count(), 3);
        assert!(renderer.is_dirty());
        assert!(renderer.selected_row().is_none());
        assert!(renderer.viewport_rows > 0);
    }

    #[test]
    fn test_row_selection() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());

        // Arrow down selects first row
        let event = renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.selected_row(), Some(0));

        // Arrow down again
        let event = renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.selected_row(), Some(1));

        // Arrow up
        let event = renderer.handle_key(KEYSYM_UP, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.selected_row(), Some(0));

        // Arrow up at top does nothing
        let event = renderer.handle_key(KEYSYM_UP, true);
        assert_eq!(event, GridEvent::None);
        assert_eq!(renderer.selected_row(), Some(0));

        // Key-up events ignored
        let event = renderer.handle_key(KEYSYM_DOWN, false);
        assert_eq!(event, GridEvent::None);
    }

    #[test]
    fn test_page_navigation() {
        let mut renderer = SpreadsheetRenderer::new(1024, 200); // Small height
        let mut rows = Vec::new();
        for i in 0..100 {
            rows.push(vec![format!("{}", i), format!("Row {}", i)]);
        }
        let cols = vec![ColumnDef::new("id"), ColumnDef::new("name")];
        renderer.set_data(cols, rows);

        // Select first row
        renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(renderer.selected_row(), Some(0));

        // Page down
        let event = renderer.handle_key(KEYSYM_PAGE_DOWN, true);
        assert_eq!(event, GridEvent::Redraw);
        assert!(renderer.selected_row().unwrap() > 0);

        // Page up
        let sel_before = renderer.selected_row().unwrap();
        let event = renderer.handle_key(KEYSYM_PAGE_UP, true);
        assert_eq!(event, GridEvent::Redraw);
        assert!(renderer.selected_row().unwrap() < sel_before);

        // End
        let event = renderer.handle_key(KEYSYM_END, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.selected_row(), Some(99));

        // Home
        let event = renderer.handle_key(KEYSYM_HOME, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.selected_row(), Some(0));
    }

    #[test]
    fn test_scroll() {
        let mut renderer = SpreadsheetRenderer::new(1024, 200);
        let mut rows = Vec::new();
        for i in 0..50 {
            rows.push(vec![format!("{}", i)]);
        }
        let cols = vec![ColumnDef::new("id")];
        renderer.set_data(cols, rows);

        let (start, _, _) = renderer.viewport_info();
        assert_eq!(start, 0);

        // Scroll down
        let event = renderer.handle_scroll(5);
        assert_eq!(event, GridEvent::Redraw);
        let (start, _, _) = renderer.viewport_info();
        assert_eq!(start, 5);

        // Scroll up
        let event = renderer.handle_scroll(-3);
        assert_eq!(event, GridEvent::Redraw);
        let (start, _, _) = renderer.viewport_info();
        assert_eq!(start, 2);

        // Scroll up past top
        let event = renderer.handle_scroll(-100);
        assert_eq!(event, GridEvent::Redraw);
        let (start, _, _) = renderer.viewport_info();
        assert_eq!(start, 0);

        // Further up does nothing
        let event = renderer.handle_scroll(-1);
        assert_eq!(event, GridEvent::None);
    }

    #[test]
    fn test_click_selection() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());

        // Click in data area on first row
        let row_y = HEADER_ROW_HEIGHT + 5; // inside first data row
        let event = renderer.handle_mouse(20, row_y, 1);
        match event {
            GridEvent::CellSelected { row, .. } => {
                assert_eq!(row, 0);
            }
            GridEvent::Redraw => {
                assert_eq!(renderer.selected_row(), Some(0));
            }
            _ => panic!("Expected CellSelected or Redraw event"),
        }

        // Click in second row
        let row_y = HEADER_ROW_HEIGHT + ROW_HEIGHT + 5;
        let event = renderer.handle_mouse(20, row_y, 1);
        match event {
            GridEvent::CellSelected { row, .. } => {
                assert_eq!(row, 1);
            }
            GridEvent::Redraw => {
                assert_eq!(renderer.selected_row(), Some(1));
            }
            _ => panic!("Expected CellSelected or Redraw event"),
        }

        // Click in header does nothing
        let event = renderer.handle_mouse(20, 5, 1);
        assert_eq!(event, GridEvent::None);
    }

    #[test]
    fn test_action_trigger() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());
        renderer.set_actions(vec![
            Action::new("Shell", Some('s'), "shell"),
            Action::new("Logs", Some('l'), "logs"),
        ]);

        // Select a row first
        renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(renderer.selected_row(), Some(0));

        // Press 's' shortcut
        let event = renderer.handle_key('s' as u32, true);
        match event {
            GridEvent::ActionTriggered { row, action_id } => {
                assert_eq!(row, 0);
                assert_eq!(action_id, "shell");
            }
            _ => panic!("Expected ActionTriggered"),
        }

        // Press 'l' shortcut
        let event = renderer.handle_key('l' as u32, true);
        match event {
            GridEvent::ActionTriggered { row, action_id } => {
                assert_eq!(row, 0);
                assert_eq!(action_id, "logs");
            }
            _ => panic!("Expected ActionTriggered"),
        }
    }

    #[test]
    fn test_viewport_calculation() {
        let mut renderer = SpreadsheetRenderer::new(800, 400);
        renderer.set_data(sample_columns(), sample_rows());

        let (_, vis_rows, total) = renderer.viewport_info();
        assert!(vis_rows > 0);
        assert_eq!(total, 3);
        assert!(renderer.viewport_cols > 0);
    }

    #[test]
    fn test_auto_column_widths() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        let cols = vec![
            ColumnDef::new("id"),
            ColumnDef::new("a_very_long_column_name"),
        ];
        let rows = vec![vec!["1".to_string(), "short".to_string()]];
        renderer.set_data(cols, rows);

        // "id" column should be at least MIN_COL_WIDTH_CHARS wide
        assert!(renderer.columns[0].width_chars >= MIN_COL_WIDTH_CHARS);

        // The long header should determine width
        assert!(renderer.columns[1].width_chars >= "a_very_long_column_name".len());
    }

    #[test]
    fn test_render_produces_jpeg() {
        let mut renderer = SpreadsheetRenderer::new(800, 600);
        renderer.set_data(sample_columns(), sample_rows());

        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
        let jpeg_data = jpeg.unwrap();
        assert!(!jpeg_data.is_empty());
        // JPEG magic bytes: FF D8
        assert_eq!(jpeg_data[0], 0xFF);
        assert_eq!(jpeg_data[1], 0xD8);
    }

    #[test]
    fn test_resize() {
        let mut renderer = SpreadsheetRenderer::new(800, 600);
        renderer.set_data(sample_columns(), sample_rows());
        renderer.clear_dirty();

        let (_, old_vis, _) = renderer.viewport_info();

        // Resize to larger
        renderer.resize(1920, 1080);
        assert!(renderer.is_dirty());

        let (_, new_vis, _) = renderer.viewport_info();
        assert!(new_vis >= old_vis);
    }

    #[test]
    fn test_empty_data() {
        let mut renderer = SpreadsheetRenderer::new(800, 600);

        // No data set
        assert_eq!(renderer.row_count(), 0);
        assert_eq!(renderer.col_count(), 0);
        assert!(renderer.selected_row().is_none());
        assert!(renderer.get_selected_value().is_none());

        // Arrow keys do nothing
        let event = renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(event, GridEvent::None);

        // Scroll does nothing
        let event = renderer.handle_scroll(5);
        assert_eq!(event, GridEvent::None);

        // Render still works
        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
        let jpeg_data = jpeg.unwrap();
        assert_eq!(jpeg_data[0], 0xFF);
        assert_eq!(jpeg_data[1], 0xD8);

        // Set empty data explicitly
        renderer.set_data(vec![ColumnDef::new("id")], vec![]);
        assert_eq!(renderer.row_count(), 0);
        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
    }

    #[test]
    fn test_update_rows() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());
        renderer.handle_key(KEYSYM_DOWN, true); // select row 0
        renderer.clear_dirty();

        // Update with more rows
        let mut new_rows = sample_rows();
        new_rows.push(vec![
            "4".to_string(),
            "Dave".to_string(),
            "dave@example.com".to_string(),
        ]);
        renderer.update_rows(new_rows);

        assert_eq!(renderer.row_count(), 4);
        assert!(renderer.is_dirty());
        assert_eq!(renderer.selected_row(), Some(0)); // Selection preserved

        // Update with fewer rows (selection clamped)
        renderer.selected_row = Some(3);
        renderer.update_rows(vec![vec![
            "1".to_string(),
            "Alice".to_string(),
            "alice@example.com".to_string(),
        ]]);
        assert_eq!(renderer.row_count(), 1);
        assert_eq!(renderer.selected_row(), Some(0)); // Clamped

        // Update to empty
        renderer.update_rows(vec![]);
        assert_eq!(renderer.row_count(), 0);
        assert!(renderer.selected_row().is_none());
    }

    #[test]
    fn test_action_bar_mode() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());
        renderer.set_actions(vec![
            Action::new("Shell", Some('s'), "shell"),
            Action::new("Logs", Some('l'), "logs"),
            Action::new("Describe", Some('d'), "describe"),
        ]);

        // Select a row
        renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(renderer.mode(), GridMode::Browse);

        // Tab enters action bar mode
        let event = renderer.handle_key(KEYSYM_TAB, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.mode(), GridMode::ActionBar);
        assert_eq!(renderer.focused_action, 0);

        // Tab cycles through actions
        renderer.handle_key(KEYSYM_TAB, true);
        assert_eq!(renderer.focused_action, 1);

        renderer.handle_key(KEYSYM_TAB, true);
        assert_eq!(renderer.focused_action, 2);

        renderer.handle_key(KEYSYM_TAB, true);
        assert_eq!(renderer.focused_action, 0); // Wraps around

        // Left arrow
        renderer.handle_key(KEYSYM_LEFT, true);
        assert_eq!(renderer.focused_action, 2); // Wraps to end

        // Enter triggers focused action
        let event = renderer.handle_key(KEYSYM_RETURN, true);
        match event {
            GridEvent::ActionTriggered { row, action_id } => {
                assert_eq!(row, 0);
                assert_eq!(action_id, "describe");
            }
            _ => panic!("Expected ActionTriggered"),
        }

        // Escape returns to browse mode
        renderer.mode = GridMode::ActionBar;
        let event = renderer.handle_key(KEYSYM_ESCAPE, true);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(renderer.mode(), GridMode::Browse);
    }

    #[test]
    fn test_mouse_scroll() {
        let mut renderer = SpreadsheetRenderer::new(1024, 400);
        let mut rows = Vec::new();
        for i in 0..50 {
            rows.push(vec![format!("{}", i)]);
        }
        renderer.set_data(vec![ColumnDef::new("id")], rows);

        // Mouse scroll down (button mask bit 4)
        let event = renderer.handle_mouse(100, 100, 0x10);
        assert_eq!(event, GridEvent::Redraw);
        let (start, _, _) = renderer.viewport_info();
        assert!(start > 0);

        // Mouse scroll up (button mask bit 3)
        let event = renderer.handle_mouse(100, 100, 0x08);
        assert_eq!(event, GridEvent::Redraw);
    }

    #[test]
    fn test_column_alignment() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        let cols = vec![
            ColumnDef::with_width("id", 10, Alignment::Right),
            ColumnDef::with_width("name", 20, Alignment::Left),
            ColumnDef::with_width("status", 15, Alignment::Center),
        ];
        let rows = vec![vec![
            "1".to_string(),
            "Alice".to_string(),
            "Active".to_string(),
        ]];
        renderer.set_data(cols, rows);

        assert_eq!(renderer.columns[0].alignment, Alignment::Right);
        assert_eq!(renderer.columns[1].alignment, Alignment::Left);
        assert_eq!(renderer.columns[2].alignment, Alignment::Center);

        // Should render without error
        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
    }

    #[test]
    fn test_get_cell_value() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());

        assert_eq!(renderer.get_cell_value(0, 0), Some("1".to_string()));
        assert_eq!(renderer.get_cell_value(1, 1), Some("Bob".to_string()));
        assert_eq!(
            renderer.get_cell_value(2, 2),
            Some("charlie@example.com".to_string())
        );
        assert_eq!(renderer.get_cell_value(10, 0), None);
        assert_eq!(renderer.get_cell_value(0, 10), None);
    }

    #[test]
    fn test_get_selected_value() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        renderer.set_data(sample_columns(), sample_rows());

        // No selection
        assert!(renderer.get_selected_value().is_none());

        // Select row 1
        renderer.handle_key(KEYSYM_DOWN, true);
        renderer.handle_key(KEYSYM_DOWN, true);
        assert_eq!(renderer.selected_row(), Some(1));
        assert_eq!(renderer.get_selected_value(), Some("2".to_string()));
    }

    #[test]
    fn test_dirty_flag() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        assert!(renderer.is_dirty()); // Dirty on creation

        renderer.clear_dirty();
        assert!(!renderer.is_dirty());

        renderer.set_data(sample_columns(), sample_rows());
        assert!(renderer.is_dirty());

        renderer.clear_dirty();
        renderer.handle_key(KEYSYM_DOWN, true);
        assert!(renderer.is_dirty());
    }

    #[test]
    #[allow(deprecated)]
    fn test_legacy_render_to_png() {
        let renderer = SpreadsheetRenderer::new(800, 600);
        let result = sample_query_result();

        let png = renderer.render_to_png(&result, 800, 600);
        assert!(png.is_ok());
        let png_data = png.unwrap();
        assert!(!png_data.is_empty());
        // PNG magic bytes
        assert_eq!(&png_data[0..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    #[allow(deprecated)]
    fn test_legacy_handle_click() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        let result = QueryResult {
            columns: vec!["id".to_string()],
            rows: vec![vec!["1".to_string()]],
            affected_rows: None,
            execution_time_ms: None,
        };

        let changed = renderer.handle_click(50, HEADER_ROW_HEIGHT + 5, &result);
        assert!(changed);
    }

    #[test]
    fn test_large_dataset_render() {
        let mut renderer = SpreadsheetRenderer::new(1024, 768);
        let cols = vec![
            ColumnDef::new("id"),
            ColumnDef::new("name"),
            ColumnDef::new("value"),
        ];
        let mut rows = Vec::new();
        for i in 0..1000 {
            rows.push(vec![
                format!("{}", i),
                format!("Item {}", i),
                format!("{:.2}", i as f64 * 1.5),
            ]);
        }
        renderer.set_data(cols, rows);

        // Rendering should succeed
        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
        let jpeg_data = jpeg.unwrap();
        assert_eq!(jpeg_data[0], 0xFF);
        assert_eq!(jpeg_data[1], 0xD8);

        // Navigate to end
        renderer.handle_key(KEYSYM_END, true);
        assert_eq!(renderer.selected_row(), Some(999));

        // Re-render at new position
        let jpeg = renderer.render_jpeg(85);
        assert!(jpeg.is_ok());
    }
}
