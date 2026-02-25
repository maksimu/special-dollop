# Ratatui Rendering Pipeline

## Context

The existing terminal handlers (SSH, Telnet, Database) render through a vt100 parser -> fontdue
pixel renderer -> JPEG pipeline. The database handler has real limitations: 100-row cap, no
scrollable results, plain ASCII tables.

Ratatui with `TestBackend` eliminates the ANSI round-trip entirely: it produces a structured cell
buffer (symbol + fg/bg/modifier per cell) that the existing fontdue renderer can consume directly.
The `TestBackend` is used intentionally here - we don't want ratatui to drive a real terminal, we
want its layout/widget engine to produce a cell buffer that we render to pixels using fontdue.

This plan covers two deliverables:
1. Ratatui rendering infrastructure in `guacr-terminal`
2. Richer database handler UI (replaces `DatabaseTerminal` + `QueryExecutor` rendering)

---

## Part 1: Ratatui rendering infrastructure (`guacr-terminal`)

### `crates/guacr-terminal/Cargo.toml`

Add under `[dependencies]`:
```toml
ratatui = { version = "0.29", default-features = false }
# default-features = false: no crossterm/termion, just TestBackend + all widgets
```

### New file: `crates/guacr-terminal/src/ratatui_renderer.rs`

```rust
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use crate::{Result, TerminalError, TerminalRenderer};

pub struct RatatuiRenderer {
    pub terminal: Terminal<TestBackend>,
    pub font_renderer: TerminalRenderer,
}

impl RatatuiRenderer {
    pub fn new(cols: u16, rows: u16, char_width: u32, char_height: u32) -> Result<Self> {
        let backend = TestBackend::new(cols, rows);
        let terminal = Terminal::new(backend)
            .map_err(|e| TerminalError::RenderError(e.to_string()))?;
        let font_size = char_height as f32 * 0.70;
        let font_renderer = TerminalRenderer::new_with_dimensions(char_width, char_height, font_size)?;
        Ok(Self { terminal, font_renderer })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.terminal
            .resize(Rect::new(0, 0, cols, rows))
            .map_err(|e| TerminalError::RenderError(e.to_string()))
    }

    pub fn render_to_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        let buffer = self.terminal.backend().buffer();
        self.font_renderer.render_ratatui_buffer(buffer, quality)
    }
}
```

### Modified `crates/guacr-terminal/src/renderer.rs`

Add these methods to `impl TerminalRenderer`:

```rust
/// Render a ratatui buffer to JPEG using fontdue
///
/// Mirrors render_screen_with_size_and_quality() but reads from
/// a ratatui::buffer::Buffer instead of a vt100::Screen.
pub fn render_ratatui_buffer(
    &self,
    buffer: &ratatui::buffer::Buffer,
    quality: u8,
) -> Result<Vec<u8>> {
    let area = buffer.area;
    let width_px = area.width as u32 * self.char_width;
    let height_px = area.height as u32 * self.char_height;

    if width_px == 0 || height_px == 0 {
        return Err(crate::TerminalError::RenderError(format!(
            "Invalid render dimensions: {}x{} px ({}x{} chars)",
            width_px, height_px, area.width, area.height
        )));
    }

    let mut img = image::RgbImage::new(width_px, height_px);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgb([0, 0, 0]);
    }

    for row in 0..area.height {
        for col in 0..area.width {
            let idx = row as usize * area.width as usize + col as usize;
            let cell = &buffer.content[idx];
            let x_px = col as u32 * self.char_width;
            let y_px = row as u32 * self.char_height;
            self.render_ratatui_cell(&mut img, cell, x_px, y_px)?;
        }
    }

    let mut jpeg_data = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality);
    encoder.encode_image(&img)?;
    Ok(jpeg_data)
}

/// Render a single ratatui cell to the image buffer
fn render_ratatui_cell(
    &self,
    img: &mut image::RgbImage,
    cell: &ratatui::buffer::Cell,
    x: u32,
    y: u32,
) -> Result<()> {
    let bg = self.ratatui_color_to_rgb(cell.bg, false);
    for py in y..(y + self.char_height).min(img.height()) {
        for px in x..(x + self.char_width).min(img.width()) {
            img.put_pixel(px, py, bg);
        }
    }

    let symbol = cell.symbol();
    if let Some(c) = symbol.chars().next() {
        if c != ' ' && c != '\0' {
            let fg = self.ratatui_color_to_rgb(cell.fg, true);

            if Self::is_box_drawing_char(c) {
                self.render_box_drawing_char(img, c, x, y, fg)?;
                return Ok(());
            }

            if let Some(block_region) = Self::get_block_character_region(c) {
                let (x_start, y_start, x_end, y_end) = block_region;
                let x0 = x + (self.char_width as f32 * x_start) as u32;
                let y0 = y + (self.char_height as f32 * y_start) as u32;
                let x1 = x + (self.char_width as f32 * x_end) as u32;
                let y1 = y + (self.char_height as f32 * y_end) as u32;
                for py in y0..y1.min(img.height()) {
                    for px in x0..x1.min(img.width()) {
                        img.put_pixel(px, py, fg);
                    }
                }
                return Ok(());
            }

            let font = self.get_font_for_char(c);
            let (metrics, bitmap) = font.rasterize(c, self.font_size);

            if bitmap.is_empty() && metrics.width == 0 {
                let margin_x = self.char_width / 4;
                let margin_y = self.char_height / 4;
                for py in (y + margin_y)..(y + self.char_height - margin_y).min(img.height()) {
                    for px in (x + margin_x)..(x + self.char_width - margin_x).min(img.width()) {
                        img.put_pixel(px, py, fg);
                    }
                }
                return Ok(());
            }

            const BASELINE_RATIO: f32 = 0.75;
            let glyph_x =
                x + ((self.char_width as i32 - metrics.width as i32) / 2).max(0) as u32;
            let baseline_y = y as f32 + (self.char_height as f32 * BASELINE_RATIO);
            let glyph_y =
                (baseline_y - metrics.bounds.height - metrics.bounds.ymin).max(y as f32) as u32;

            for (i, &alpha) in bitmap.iter().enumerate() {
                if alpha > 0 {
                    let dx = (i % metrics.width) as u32;
                    let dy = (i / metrics.width) as u32;
                    let px = glyph_x + dx;
                    let py = glyph_y + dy;
                    if px < img.width() && py < img.height() {
                        let alpha_f = alpha as f32 / 255.0;
                        let current = img.get_pixel(px, py);
                        let blended = image::Rgb([
                            ((fg.0[0] as f32 * alpha_f) + (current.0[0] as f32 * (1.0 - alpha_f))) as u8,
                            ((fg.0[1] as f32 * alpha_f) + (current.0[1] as f32 * (1.0 - alpha_f))) as u8,
                            ((fg.0[2] as f32 * alpha_f) + (current.0[2] as f32 * (1.0 - alpha_f))) as u8,
                        ]);
                        img.put_pixel(px, py, blended);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Map ratatui Color to RGB, using color scheme for Reset
fn ratatui_color_to_rgb(&self, color: ratatui::style::Color, is_foreground: bool) -> image::Rgb<u8> {
    use ratatui::style::Color;
    match color {
        Color::Reset => {
            if is_foreground {
                image::Rgb(self.color_scheme.foreground)
            } else {
                image::Rgb(self.color_scheme.background)
            }
        }
        Color::Black       => image::Rgb([0, 0, 0]),
        Color::Red         => image::Rgb([205, 0, 0]),
        Color::Green       => image::Rgb([0, 205, 0]),
        Color::Yellow      => image::Rgb([205, 205, 0]),
        Color::Blue        => image::Rgb([0, 0, 238]),
        Color::Magenta     => image::Rgb([205, 0, 205]),
        Color::Cyan        => image::Rgb([0, 205, 205]),
        Color::Gray        => image::Rgb([229, 229, 229]),
        Color::DarkGray    => image::Rgb([127, 127, 127]),
        Color::LightRed    => image::Rgb([255, 0, 0]),
        Color::LightGreen  => image::Rgb([0, 255, 0]),
        Color::LightYellow => image::Rgb([255, 255, 0]),
        Color::LightBlue   => image::Rgb([92, 92, 255]),
        Color::LightMagenta => image::Rgb([255, 0, 255]),
        Color::LightCyan   => image::Rgb([0, 255, 255]),
        Color::White       => image::Rgb([255, 255, 255]),
        Color::Rgb(r, g, b) => image::Rgb([r, g, b]),
        Color::Indexed(i) => Self::xterm_256_color(i),
    }
}

/// xterm 256-color lookup
fn xterm_256_color(index: u8) -> image::Rgb<u8> {
    match index {
        // 0-15: standard ANSI (same as named variants above)
        0  => image::Rgb([0, 0, 0]),
        1  => image::Rgb([205, 0, 0]),
        2  => image::Rgb([0, 205, 0]),
        3  => image::Rgb([205, 205, 0]),
        4  => image::Rgb([0, 0, 238]),
        5  => image::Rgb([205, 0, 205]),
        6  => image::Rgb([0, 205, 205]),
        7  => image::Rgb([229, 229, 229]),
        8  => image::Rgb([127, 127, 127]),
        9  => image::Rgb([255, 0, 0]),
        10 => image::Rgb([0, 255, 0]),
        11 => image::Rgb([255, 255, 0]),
        12 => image::Rgb([92, 92, 255]),
        13 => image::Rgb([255, 0, 255]),
        14 => image::Rgb([0, 255, 255]),
        15 => image::Rgb([255, 255, 255]),
        // 16-231: 6x6x6 color cube
        16..=231 => {
            let i = index - 16;
            let b = i % 6;
            let g = (i / 6) % 6;
            let r = i / 36;
            let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            image::Rgb([scale(r), scale(g), scale(b)])
        }
        // 232-255: 24-step grayscale
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            image::Rgb([v, v, v])
        }
    }
}
```

### Modified `crates/guacr-terminal/src/lib.rs`

Add after the existing module declarations (around line 39):
```rust
pub mod ratatui_renderer;
pub use ratatui_renderer::RatatuiRenderer;
```

---

## Part 2: Database handler Ratatui UI

### `crates/guacr-database/Cargo.toml`

Add under `[dependencies]` (ratatui is needed because `ratatui_db_ui.rs` uses the widget types
directly):
```toml
ratatui = { version = "0.29", default-features = false }
```

### New file: `crates/guacr-database/src/ratatui_db_ui.rs`

```rust
use std::collections::VecDeque;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
               ScrollbarState, Table, TableState},
};
use guacr_terminal::QueryResult;

pub struct DatabaseRatatuiApp {
    // Query results
    pub results: Vec<Vec<String>>,
    pub columns: Vec<String>,
    pub table_state: TableState,
    pub scroll_state: ScrollbarState,

    // Input line
    pub input_buffer: String,
    pub cursor_pos: usize,

    // History
    pub history: VecDeque<String>,
    pub history_idx: Option<usize>,
    pub temp_buffer: String,
    pub history_max_size: usize,

    // Status / prompt
    pub status_msg: String,
    pub query_time_ms: Option<u64>,
    pub prompt: String,
    pub in_continuation: bool,
    pub continuation_prompt: String,
}

impl DatabaseRatatuiApp {
    pub fn new(prompt: &str, continuation_prompt: &str) -> Self {
        Self {
            results: Vec::new(),
            columns: Vec::new(),
            table_state: TableState::default(),
            scroll_state: ScrollbarState::default(),
            input_buffer: String::new(),
            cursor_pos: 0,
            history: VecDeque::new(),
            history_idx: None,
            temp_buffer: String::new(),
            history_max_size: 250,
            status_msg: String::new(),
            query_time_ms: None,
            prompt: prompt.to_string(),
            in_continuation: false,
            continuation_prompt: continuation_prompt.to_string(),
        }
    }

    pub fn set_prompt(&mut self, prompt: &str) {
        self.prompt = prompt.to_string();
    }

    pub fn set_results(&mut self, result: &QueryResult) {
        self.columns = result.columns.clone();
        self.results = result.rows.clone();
        self.table_state = TableState::default();
        let row_count = self.results.len();
        self.scroll_state = ScrollbarState::new(row_count);
        let summary = if result.rows.is_empty() {
            if let Some(affected) = result.affected_rows {
                format!("Query OK, {} row(s) affected", affected)
            } else {
                "Query executed successfully".to_string()
            }
        } else {
            format!("{} row(s)", result.rows.len())
        };
        self.status_msg = if let Some(ms) = result.execution_time_ms {
            format!("{} ({}ms)", summary, ms)
        } else {
            summary
        };
        self.query_time_ms = result.execution_time_ms;
    }

    pub fn set_error(&mut self, error: &str) {
        self.status_msg = format!("ERROR: {}", error);
    }

    pub fn set_status(&mut self, msg: String, time_ms: Option<u64>) {
        self.status_msg = msg;
        self.query_time_ms = time_ms;
    }

    pub fn insert_clipboard_text(&mut self, text: &str) {
        self.input_buffer.insert_str(self.cursor_pos, text);
        self.cursor_pos += text.len();
    }

    /// Render the full UI into the given frame
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Split vertically: results on top (flexible), input at bottom (3 rows)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        self.render_results(frame, chunks[0]);
        self.render_input(frame, chunks[1]);
    }

    fn render_results(&mut self, frame: &mut Frame, area: Rect) {
        if self.columns.is_empty() {
            // Show status message when no results
            let para = Paragraph::new(self.status_msg.clone())
                .block(Block::default().borders(Borders::ALL).title("Results"))
                .style(Style::default().fg(Color::Gray));
            frame.render_widget(para, area);
            return;
        }

        // Build header + rows
        let header_cells: Vec<Cell> = self.columns.iter()
            .map(|c| Cell::from(c.clone()).style(
                Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD)
            ))
            .collect();
        let header = Row::new(header_cells).height(1);

        let rows: Vec<Row> = self.results.iter()
            .map(|row| {
                let cells: Vec<Cell> = row.iter()
                    .map(|val| Cell::from(val.clone()))
                    .collect();
                Row::new(cells).height(1)
            })
            .collect();

        // Auto-width: equal columns
        let col_count = self.columns.len().max(1);
        let constraints: Vec<Constraint> = (0..col_count)
            .map(|_| Constraint::Ratio(1, col_count as u32))
            .collect();

        let title = if self.status_msg.is_empty() {
            "Results".to_string()
        } else {
            format!("Results - {}", self.status_msg)
        };

        let table = Table::new(rows, constraints)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .style(Style::default().fg(Color::White));

        // Split area to leave room for scrollbar
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        let scroll_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            width: 1,
            ..area
        };

        frame.render_stateful_widget(table, table_area, &mut self.table_state);

        let row_count = self.results.len();
        self.scroll_state = self.scroll_state.content_length(row_count);
        if let Some(sel) = self.table_state.selected() {
            self.scroll_state = self.scroll_state.position(sel);
        }
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            scroll_area,
            &mut self.scroll_state,
        );
    }

    fn render_input(&mut self, frame: &mut Frame, area: Rect) {
        let prompt_str = if self.in_continuation {
            &self.continuation_prompt
        } else {
            &self.prompt
        };

        // Build input line with cursor indicator
        let before_cursor = &self.input_buffer[..self.cursor_pos];
        let after_cursor = &self.input_buffer[self.cursor_pos..];

        let line = Line::from(vec![
            Span::styled(prompt_str.clone(), Style::default().fg(Color::Green)),
            Span::raw(before_cursor.to_string()),
            Span::styled("_", Style::default().add_modifier(Modifier::RAPID_BLINK).fg(Color::White)),
            Span::raw(after_cursor.to_string()),
        ]);

        let para = Paragraph::new(line)
            .block(Block::default().borders(Borders::ALL).title("Query"));
        frame.render_widget(para, area);
    }

    /// Handle a key event. Returns Some(query) when Enter is pressed with non-empty input.
    pub fn handle_key(&mut self, keysym: u32, pressed: bool) -> Option<String> {
        if !pressed {
            return None;
        }

        match keysym {
            // Enter
            0xFF0D => {
                let query = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                self.cursor_pos = 0;
                if !query.is_empty() {
                    self.add_to_history(&query);
                    return Some(query);
                }
                None
            }
            // Backspace
            0xFF08 => {
                self.delete_char_before_cursor();
                None
            }
            // Escape
            0xFF1B => {
                self.input_buffer.clear();
                self.cursor_pos = 0;
                None
            }
            // Up arrow - history previous
            0xFF52 => {
                self.history_previous();
                None
            }
            // Down arrow - history next
            0xFF54 => {
                self.history_next();
                None
            }
            // Left arrow
            0xFF51 => {
                if self.cursor_pos > 0 { self.cursor_pos -= 1; }
                None
            }
            // Right arrow
            0xFF53 => {
                if self.cursor_pos < self.input_buffer.len() { self.cursor_pos += 1; }
                None
            }
            // Home / Ctrl+A
            0xFF50 | 0x0001 => {
                self.cursor_pos = 0;
                None
            }
            // End / Ctrl+E
            0xFF57 | 0x0005 => {
                self.cursor_pos = self.input_buffer.len();
                None
            }
            // Delete
            0xFFFF => {
                self.delete_char_at_cursor();
                None
            }
            // Ctrl+K - kill to end
            0x000B => {
                self.input_buffer.truncate(self.cursor_pos);
                None
            }
            // Ctrl+U - kill line
            0x0015 => {
                self.input_buffer.clear();
                self.cursor_pos = 0;
                None
            }
            // Ctrl+W - kill word
            0x0017 => {
                self.kill_word();
                None
            }
            // Ctrl+C
            0x0003 => {
                self.input_buffer.clear();
                self.cursor_pos = 0;
                None
            }
            // Table navigation: Page Up/Down, Home, End when results are showing
            0xFF55 => { // Page Up - scroll table up
                if !self.results.is_empty() {
                    let sel = self.table_state.selected().unwrap_or(0);
                    let new_sel = sel.saturating_sub(10);
                    self.table_state.select(Some(new_sel));
                }
                None
            }
            0xFF56 => { // Page Down - scroll table down
                if !self.results.is_empty() {
                    let sel = self.table_state.selected().unwrap_or(0);
                    let new_sel = (sel + 10).min(self.results.len().saturating_sub(1));
                    self.table_state.select(Some(new_sel));
                }
                None
            }
            // Regular printable character
            _ => {
                if let Some(c) = char::from_u32(keysym) {
                    if c.is_ascii() && !c.is_control() {
                        self.input_buffer.insert(self.cursor_pos, c);
                        self.cursor_pos += 1;
                    }
                }
                None
            }
        }
    }

    // --- History ---

    pub fn add_to_history(&mut self, command: &str) {
        if command.trim().is_empty() { return; }
        if self.history.back() == Some(&command.to_string()) { return; }
        self.history.push_back(command.to_string());
        if self.history.len() > self.history_max_size {
            self.history.pop_front();
        }
        self.history_idx = None;
        self.temp_buffer.clear();
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() { return; }
        if self.history_idx.is_none() {
            self.temp_buffer = self.input_buffer.clone();
            self.history_idx = Some(self.history.len() - 1);
        } else if let Some(idx) = self.history_idx {
            if idx > 0 { self.history_idx = Some(idx - 1); } else { return; }
        }
        if let Some(idx) = self.history_idx {
            self.input_buffer = self.history[idx].clone();
            self.cursor_pos = self.input_buffer.len();
        }
    }

    fn history_next(&mut self) {
        if let Some(idx) = self.history_idx {
            if idx < self.history.len() - 1 {
                self.history_idx = Some(idx + 1);
                self.input_buffer = self.history[idx + 1].clone();
            } else {
                self.history_idx = None;
                self.input_buffer = self.temp_buffer.clone();
                self.temp_buffer.clear();
            }
            self.cursor_pos = self.input_buffer.len();
        }
    }

    // --- Editing ---

    fn delete_char_before_cursor(&mut self) {
        if self.cursor_pos > 0 {
            self.input_buffer.remove(self.cursor_pos - 1);
            self.cursor_pos -= 1;
        }
    }

    fn delete_char_at_cursor(&mut self) {
        if self.cursor_pos < self.input_buffer.len() {
            self.input_buffer.remove(self.cursor_pos);
        }
    }

    fn kill_word(&mut self) {
        if self.cursor_pos > 0 {
            let chars: Vec<char> = self.input_buffer.chars().collect();
            let mut pos = self.cursor_pos - 1;
            while pos > 0 && chars[pos].is_whitespace() { pos -= 1; }
            while pos > 0 && !chars[pos - 1].is_whitespace() { pos -= 1; }
            self.input_buffer.drain(pos..self.cursor_pos);
            self.cursor_pos = pos;
        }
    }
}
```

### Modified `crates/guacr-database/src/lib.rs`

Add to the module list (after `mod recording;`):
```rust
mod ratatui_db_ui;
pub use ratatui_db_ui::DatabaseRatatuiApp;
```

### Modified `crates/guacr-database/src/query_executor.rs`

Replace the existing `QueryExecutor` struct and its rendering path. Key changes:

**Imports to add:**
```rust
use crate::ratatui_db_ui::DatabaseRatatuiApp;
use guacr_terminal::RatatuiRenderer;
```

**New struct:**
```rust
pub struct QueryExecutor {
    pub ratatui: RatatuiRenderer,
    pub app: DatabaseRatatuiApp,
    stream_id: u32,
    db_type: String,

    // Multi-line continuation (not moved to app since it affects prompt selection)
    in_continuation: bool,
    continuation_prompt: String,
    current_database: Option<String>,
    prompt_template: String,

    // Mouse/clipboard/resize handling (unchanged)
    input_handler: TerminalInputHandler,

    // Protocol encoder (unchanged)
    protocol_encoder: TextProtocolEncoder,
}
```

The char dimensions for databases: `char_width=9, char_height=18` (matching the existing
`const CHAR_WIDTH: u32 = 9; const CHAR_HEIGHT: u32 = 18;` in `handle_mouse_input`).

**`new_with_size` becomes:**
```rust
pub fn new_with_size(prompt: &str, db_type: &str, rows: u16, cols: u16) -> Result<Self> {
    let continuation_prompt = match db_type {
        "mysql" | "mariadb" => "    -> ".to_string(),
        "postgresql" => "    -> ".to_string(),
        "mongodb" => "... ".to_string(),
        "redis" => "... ".to_string(),
        _ => "    -> ".to_string(),
    };

    const CHAR_WIDTH: u32 = 9;
    const CHAR_HEIGHT: u32 = 18;

    Ok(Self {
        ratatui: RatatuiRenderer::new(cols, rows, CHAR_WIDTH, CHAR_HEIGHT)?,
        app: DatabaseRatatuiApp::new(prompt, &continuation_prompt),
        stream_id: 1,
        db_type: db_type.to_string(),
        in_continuation: false,
        continuation_prompt,
        current_database: None,
        prompt_template: prompt.to_string(),
        input_handler: TerminalInputHandler::new_with_scrollback(rows, cols, 1000),
        protocol_encoder: TextProtocolEncoder::new(),
    })
}
```

**`size()` needs a change** - there's no more `self.terminal.size()`. Store rows/cols or get from
backend:
```rust
pub fn size(&self) -> (u16, u16) {
    let area = self.ratatui.terminal.backend().buffer().area;
    (area.height, area.width)
}
```

**`write_result` becomes:**
```rust
pub fn write_result(&mut self, result: &QueryResult) -> Result<()> {
    self.app.set_results(result);
    Ok(())
}

pub fn write_error(&mut self, error: &str) -> Result<()> {
    self.app.set_error(error);
    Ok(())
}
```

**`is_dirty` becomes always-true** (ratatui always renders cleanly):
```rust
pub fn is_dirty(&self) -> bool {
    true
}
```

**`render_screen` becomes:**
```rust
pub async fn render_screen(&mut self) -> Result<(bool, Vec<Bytes>)> {
    // Sync app's in_continuation state
    self.app.in_continuation = self.in_continuation;

    // Draw ratatui frame
    let app = &mut self.app;
    self.ratatui.terminal.draw(|f| app.render(f))
        .map_err(|e| DatabaseError::QueryError(format!("Render error: {}", e)))?;

    // Render buffer to JPEG
    let jpeg = self.ratatui.render_to_jpeg(85)
        .map_err(|e| DatabaseError::QueryError(format!("JPEG error: {}", e)))?;

    let base64_data =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg);

    let img_instr = self.protocol_encoder.format_img_instruction(
        self.stream_id,
        0, 0, 0,
        "image/jpeg",
    );
    let blob_instructions = format_chunked_blobs(self.stream_id, &base64_data, None);

    let mut instructions = Vec::with_capacity(1 + blob_instructions.len() + 1);
    instructions.push(img_instr.freeze());
    instructions.extend(blob_instructions.into_iter().map(Bytes::from));

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let sync_instr = self.ratatui.font_renderer.format_sync_instruction(timestamp_ms);
    instructions.push(Bytes::from(sync_instr));

    Ok((true, instructions))
}
```

**`process_input` key handling** - delegate to `app.handle_key`:
```rust
// In the "key" branch of process_input:
let pending_query = self.app.handle_key(keysym, pressed);
if pending_query.is_some() || /* key affected input */ true {
    let (_, instructions) = self.render_screen().await?;
    return Ok((true, instructions, pending_query));
}
```

The current character-at-a-time render suppression (`input_dirty` flag + debounce) should be
replaced by always rendering on every key. The ratatui path is always a full-screen render, but
since the terminal session is not a hot path this is fine.

**`handle_clipboard_input` patch:**
```rust
async fn handle_clipboard_input(&mut self, instruction: &str)
    -> Result<(bool, Vec<Bytes>, Option<String>)>
{
    if let Some(text) = self.input_handler.parse_clipboard_instruction(instruction) {
        self.app.insert_clipboard_text(&text);
        let (_, instructions) = self.render_screen().await?;
        Ok((true, instructions, None))
    } else {
        Ok((false, vec![], None))
    }
}
```

**`handle_resize_input` patch:**
```rust
async fn handle_resize_input(&mut self, args: &[&str])
    -> Result<(bool, Vec<Bytes>, Option<String>)>
{
    if args.len() >= 2 {
        let width: u32 = args[0].parse().unwrap_or(1024);
        let height: u32 = args[1].parse().unwrap_or(768);
        const CHAR_WIDTH: u32 = 9;
        const CHAR_HEIGHT: u32 = 18;
        let cols = (width / CHAR_WIDTH).max(80) as u16;
        let rows = (height / CHAR_HEIGHT).max(24) as u16;

        // Resize input handler's terminal emulator (for mouse selection geometry)
        let (old_rows, old_cols) = self.size();
        if rows != old_rows || cols != old_cols {
            // input_handler.handle_resize needs a TerminalEmulator - keep a minimal one
            // for mouse handling, OR remove this if mouse selection in database is not needed
            self.ratatui.resize(cols, rows)
                .map_err(|e| DatabaseError::QueryError(format!("Resize error: {}", e)))?;
        }

        let (_, instructions) = self.render_screen().await?;
        Ok((true, instructions, None))
    } else {
        Ok((false, vec![], None))
    }
}
```

Note: `handle_mouse_input` calls `self.input_handler.handle_mouse_event(..., self.terminal.emulator(), ...)`.
With ratatui, there is no terminal emulator. Mouse selection in the database handler either needs
to be re-implemented against the ratatui buffer or removed. The simplest approach is to remove
mouse selection from the database handler in this refactor and handle it separately later.

**Remove these methods** (now handled by `DatabaseRatatuiApp`):
- `redraw_input_line`
- `delete_char_before_cursor`
- `delete_char_at_cursor`
- `move_cursor_left`, `move_cursor_right`, `move_cursor_home`, `move_cursor_end`
- `kill_to_end`, `kill_line`, `kill_word`
- `history_previous`, `history_next`, `add_to_history`

**Keep these methods unchanged:**
- `send_display_init` (static, no instance state used)
- `set_current_database` (still calls `update_prompt` which calls `app.set_prompt`)
- `update_prompt` (change `self.terminal.set_prompt` to `self.app.set_prompt`)
- `set_continuation` (change `self.in_continuation = ...` stays, also sets `self.app.in_continuation`)
- `current_input` (change to `&self.app.input_buffer`)
- `db_type` (unchanged)

**Update `set_current_database`:**
```rust
pub fn set_current_database(&mut self, database: Option<String>) {
    self.current_database = database;
    self.update_prompt();
}

fn update_prompt(&mut self) {
    let prompt = if let Some(ref db) = self.current_database {
        match self.db_type.as_str() {
            "mysql" | "mariadb" => format!("mysql [{}]> ", db),
            "postgresql" => format!("{}=# ", db),
            _ => self.prompt_template.clone(),
        }
    } else {
        self.prompt_template.clone()
    };
    self.app.set_prompt(&prompt);
}
```

**Remove from `lib.rs`** the `pub use guacr_terminal::DatabaseTerminal` re-export if nothing
else uses it. (Check by searching for `DatabaseTerminal` in guacr-database handler files first.)

---

## Files changed

| File | Change |
|------|--------|
| `crates/guacr-terminal/Cargo.toml` | Add `ratatui = { version = "0.29", default-features = false }` |
| `crates/guacr-terminal/src/renderer.rs` | Add `render_ratatui_buffer`, `render_ratatui_cell`, `ratatui_color_to_rgb`, `xterm_256_color` |
| `crates/guacr-terminal/src/ratatui_renderer.rs` | NEW - `RatatuiRenderer` struct |
| `crates/guacr-terminal/src/lib.rs` | Add `pub mod ratatui_renderer; pub use ratatui_renderer::RatatuiRenderer;` |
| `crates/guacr-database/Cargo.toml` | Add `ratatui = { version = "0.29", default-features = false }` |
| `crates/guacr-database/src/ratatui_db_ui.rs` | NEW - `DatabaseRatatuiApp` |
| `crates/guacr-database/src/lib.rs` | Add `mod ratatui_db_ui; pub use ratatui_db_ui::DatabaseRatatuiApp;` |
| `crates/guacr-database/src/query_executor.rs` | Replace `DatabaseTerminal`/`DirtyTracker` rendering with ratatui path |

---

## Verification

```bash
# Build check
cargo build -p guacr-terminal -p guacr-database

# Existing tests still pass
cargo test -p guacr-terminal
cargo test -p guacr-database

# Manual: connect to a MySQL/Postgres handler, verify:
# - Results table shows all rows (no 100-row cap)
# - Arrow keys scroll the results table
# - Input line edits correctly
# - History navigation works (Up/Down arrows)
# - Page Up/Down scrolls results viewport
```

---

## Implementation notes from reading the code

- `QueryExecutor` currently stores `stream_id: u32` starting at 1 (correct per the Guacamole
  stream index note in MEMORY.md).
- `TerminalInputHandler::new_with_scrollback(rows, cols, 1000)` is still needed for clipboard
  parsing (`parse_clipboard_instruction`) even in the ratatui path.
- `TextProtocolEncoder` is in `guacr_protocol` - keep it for `format_img_instruction` and
  `format_chunked_blobs`.
- `DatabaseTerminal` in `guacr_terminal` still exists and is re-exported from `guacr_database`.
  If other callers use `DatabaseTerminal`, don't remove it - just stop using it in `QueryExecutor`.
- The `send_display_init` static method sends a `size` instruction using pixel dimensions
  `(width, height)`, not char dimensions. This is still correct - the caller (e.g., mysql.rs)
  passes `width=1024, height=768` or whatever the actual browser dimensions are.
- Mouse selection (`handle_mouse_input`) currently needs `self.terminal.emulator()` which won't
  exist after removing `DatabaseTerminal`. Two options:
  1. Keep a minimal `TerminalEmulator` as a dummy (wasteful)
  2. Remove database mouse selection in this PR and file a follow-on task
  Option 2 is cleaner for this migration.
- The `handle_resize_input` currently calls `self.dirty_tracker = DirtyTracker::new(rows, cols)`
  which can be removed - no dirty tracker in the ratatui path.
- `update_prompt` currently calls `self.terminal.set_prompt(&prompt)`. Becomes
  `self.app.set_prompt(&prompt)`.

---

## Natural follow-ons

### TN3270 / TN5250

`guacr-tn3270/src/screen.rs` has `CellAttribute { foreground: Color3270, background: Color3270, ... }`.
This maps directly to ratatui `Cell` style. Populate a `ratatui::buffer::Buffer` directly from
the 3270 `ScreenBuffer`, then call `TerminalRenderer::render_ratatui_buffer()`. No ANSI round-trip.

### SFTP file browser

`guacr-sftp/src/file_browser.rs` currently renders a white PNG via nested pixel loops with no
fonts. A ratatui `Table` widget gives proper fonts, column widths, scroll, selection at ~100 lines.

### ResourceBrowserHandler — Kubernetes / Docker / vSphere list mode

`ResourceBrowserHandler` in `guacr-handlers/src/resource_browser.rs` uses `SpreadsheetRenderer`
for its list view. `SpreadsheetRenderer` is a ~1,600-line hand-built pixel renderer that does
the same job as `ratatui::widgets::Table`. It can be retired once `render_ratatui_buffer` lands.

The replacement is a `RatatuiRenderer` inside `ResourceBrowserHandler`. `render()` produces the
3-zone layout (namespace/filter bar + Table + action/status bar). Colored status badges
(Running=green, Error=red, Pending=yellow) are a one-liner with `Style::fg`. The filter input
and namespace selector are Ratatui `Paragraph` widgets — currently impossible with
`SpreadsheetRenderer` without significant pixel work.

The `ActionResult::Terminal` path (exec shell, log streaming) stays on `TerminalEmulator` + vt100
JPEG as today — Ratatui only replaces the list-mode rendering.

See `crates/guacr/docs/CONTAINER_MANAGEMENT_PROTOCOL.md` for the target layout diagram and
color scheme.

### SSH/Telnet chrome wrapper (tab bar)

A ratatui `Tabs` widget at the top of the frame can composite over the terminal JPEG. The
tab bar renders to a `Buffer`, gets converted to JPEG via `render_ratatui_buffer`, and gets
composited with the terminal JPEG at the frame level.
