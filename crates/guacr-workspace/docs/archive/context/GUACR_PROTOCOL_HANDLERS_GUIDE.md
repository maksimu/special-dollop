# Guacr Protocol Handlers - Implementation Guide

## Overview

This document provides detailed implementation guidance for building protocol handlers in guacr. Each handler converts a specific remote access protocol (SSH, RDP, VNC, etc.) into the Guacamole protocol for browser-based access.

---

## Handler Architecture

### The ProtocolHandler Trait

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol identifier (e.g., "ssh", "rdp", "vnc")
    fn name(&self) -> &str;

    /// Initialize the handler with configuration
    fn initialize(&self, config: HashMap<String, String>) -> Result<()> {
        Ok(())
    }

    /// Connect to remote host and handle the session
    ///
    /// # Arguments
    /// * `params` - Connection parameters from Guacamole handshake
    /// * `guac_tx` - Channel to send Guacamole instructions to client
    /// * `guac_rx` - Channel to receive Guacamole instructions from client
    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()>;

    /// Graceful disconnect
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    /// Health check for this handler
    async fn health_check(&self) -> Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    /// Update handler configuration at runtime
    async fn update_config(&self, config: &HashMap<String, String>) -> Result<()> {
        Ok(())
    }

    /// Get handler statistics
    async fn stats(&self) -> Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Unhealthy { reason: String },
}

#[derive(Debug, Default)]
pub struct HandlerStats {
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}
```

### Connection Flow

```
Client Browser                  Guacr                    Remote Host
     |                            |                            |
     |----(1) WebSocket Connect-->|                            |
     |                            |                            |
     |<---(2) Handshake Request---|                            |
     |                            |                            |
     |----(3) Select Protocol---->|                            |
     |        (e.g., "ssh")       |                            |
     |                            |                            |
     |<---(4) Args Request--------|                            |
     |                            |                            |
     |----(5) Connection Params-->|                            |
     |    (host, port, user, etc) |                            |
     |                            |                            |
     |                            |----(6) TCP Connect-------->|
     |                            |                            |
     |                            |<---(7) Auth Challenge------|
     |                            |                            |
     |<---(8) Ready Notification--|                            |
     |                            |                            |
     |<===(9) Bidirectional Guac Instructions================>|
     |          (mouse, key, img, clipboard, etc.)            |
     |                                                         |
```

---

## Implementation Guide by Protocol

### 1. Terminal-Based Protocols (SSH, Telnet)

**Key Components:**
- **Connection**: Establish TCP or SSH connection
- **Terminal Emulation**: Parse ANSI/VT100 escape codes
- **Screen Rendering**: Convert terminal state to PNG images
- **Input Handling**: Convert Guacamole key events to terminal input

#### Terminal State Machine

```rust
pub struct TerminalEmulator {
    parser: vt100::Parser,
    screen_buffer: ScreenBuffer,
    dirty_regions: Vec<Rect>,
}

impl TerminalEmulator {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            screen_buffer: ScreenBuffer::new(rows, cols),
            dirty_regions: Vec::new(),
        }
    }

    /// Process terminal output data
    pub fn process(&mut self, data: &[u8]) -> Result<()> {
        self.parser.process(data);
        self.update_dirty_regions();
        Ok(())
    }

    /// Get dirty rectangles that need to be redrawn
    pub fn dirty_rects(&self) -> &[Rect] {
        &self.dirty_regions
    }

    /// Render terminal to PNG for a specific region
    pub fn render_region(&self, rect: Rect) -> Result<Vec<u8>> {
        let screen = self.parser.screen();

        // Render text with specified font
        let font = load_terminal_font()?;
        let mut img = RgbaImage::new(
            rect.width() * CHAR_WIDTH,
            rect.height() * CHAR_HEIGHT,
        );

        for row in rect.top..rect.bottom {
            for col in rect.left..rect.right {
                let cell = screen.cell(row, col);
                render_cell(&mut img, cell, row, col, &font)?;
            }
        }

        // Encode to PNG
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)?;

        Ok(png_data)
    }

    /// Clear dirty regions after rendering
    pub fn clear_dirty(&mut self) {
        self.dirty_regions.clear();
    }
}

fn render_cell(
    img: &mut RgbaImage,
    cell: &vt100::Cell,
    row: u16,
    col: u16,
    font: &Font,
) -> Result<()> {
    let x = col as u32 * CHAR_WIDTH;
    let y = row as u32 * CHAR_HEIGHT;

    // Background color
    let bg_color = cell.bgcolor();
    draw_filled_rect(img, x, y, CHAR_WIDTH, CHAR_HEIGHT, bg_color);

    // Character
    if let Some(c) = cell.contents().chars().next() {
        let fg_color = cell.fgcolor();
        draw_char(img, c, x, y, fg_color, font)?;
    }

    // Cursor
    if cell.has_cursor() {
        draw_cursor(img, x, y)?;
    }

    Ok(())
}
```

#### SSH Handler Pattern

```rust
pub struct SshHandler {
    config: Arc<SshConfig>,
}

impl SshHandler {
    async fn handle_ssh_connection(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        // 1. Extract connection parameters
        let host = params.get("hostname").ok_or(Error::MissingParam("hostname"))?;
        let port = params.get("port").and_then(|p| p.parse().ok()).unwrap_or(22);
        let username = params.get("username").ok_or(Error::MissingParam("username"))?;

        // 2. Connect to SSH server
        let config = russh::client::Config::default();
        let mut session = russh::client::connect(Arc::new(config), (host.as_str(), port)).await?;

        // 3. Authenticate
        self.authenticate(&mut session, username, &params).await?;

        // 4. Request PTY
        let mut channel = session.channel_open_session().await?;
        let (cols, rows) = self.get_terminal_size(&params);
        channel.request_pty("xterm-256color", cols, rows, 0, 0).await?;
        channel.request_shell().await?;

        // 5. Set up terminal emulator
        let mut terminal = TerminalEmulator::new(rows, cols);

        // 6. Bidirectional event loop
        let (mut ssh_read, mut ssh_write) = channel.split();

        loop {
            tokio::select! {
                // SSH output → Terminal → Guacamole
                result = ssh_read.read(&mut buffer) => {
                    match result {
                        Ok(n) if n > 0 => {
                            // Process terminal output
                            terminal.process(&buffer[..n])?;

                            // Render dirty regions
                            for rect in terminal.dirty_rects() {
                                let png_data = terminal.render_region(*rect)?;

                                guac_tx.send(GuacInstruction {
                                    opcode: "img".to_string(),
                                    args: vec![
                                        "0".to_string(),  // layer
                                        "image/png".to_string(),
                                        base64::encode(&png_data),
                                    ],
                                }).await?;
                            }

                            terminal.clear_dirty();
                        },
                        Ok(_) => break,  // EOF
                        Err(e) => return Err(e.into()),
                    }
                },

                // Guacamole input → SSH
                Some(instruction) = guac_rx.recv() => {
                    match instruction.opcode.as_str() {
                        "key" => {
                            let input = self.handle_key_event(&instruction)?;
                            ssh_write.write_all(&input).await?;
                        },
                        "size" => {
                            let (cols, rows) = parse_size(&instruction)?;
                            terminal.resize(rows, cols);
                            ssh_write.window_change(cols, rows, 0, 0).await?;
                        },
                        "clipboard" => {
                            // Paste clipboard content
                            let text = &instruction.args[1];
                            ssh_write.write_all(text.as_bytes()).await?;
                        },
                        _ => {}
                    }
                },
            }
        }

        Ok(())
    }

    fn handle_key_event(&self, instruction: &GuacInstruction) -> Result<Vec<u8>> {
        // Parse Guacamole key instruction
        // args: [keysym, pressed]
        let keysym: u32 = instruction.args[0].parse()?;
        let pressed: bool = instruction.args[1] == "1";

        if !pressed {
            return Ok(Vec::new());  // Only handle key press
        }

        // Convert X11 keysym to terminal input
        Ok(match keysym {
            // ASCII characters (0x0020 - 0x007E)
            0x0020..=0x007E => vec![keysym as u8],

            // Return/Enter
            0xFF0D => vec![b'\r'],

            // Backspace
            0xFF08 => vec![0x7F],

            // Tab
            0xFF09 => vec![b'\t'],

            // Escape
            0xFF1B => vec![0x1B],

            // Arrow keys
            0xFF51 => vec![0x1B, b'[', b'D'],  // Left
            0xFF52 => vec![0x1B, b'[', b'A'],  // Up
            0xFF53 => vec![0x1B, b'[', b'C'],  // Right
            0xFF54 => vec![0x1B, b'[', b'B'],  // Down

            // Function keys
            0xFFBE => vec![0x1B, b'O', b'P'],  // F1
            0xFFBF => vec![0x1B, b'O', b'Q'],  // F2
            0xFFC0 => vec![0x1B, b'O', b'R'],  // F3
            0xFFC1 => vec![0x1B, b'O', b'S'],  // F4

            // Control characters
            _ if keysym >= 0x0041 && keysym <= 0x005A => {
                // Ctrl+A through Ctrl+Z
                vec![(keysym - 0x0040) as u8]
            },

            _ => Vec::new(),  // Unsupported key
        })
    }
}
```

---

### 2. Graphical Protocols (RDP, VNC)

**Key Components:**
- **Connection**: Establish protocol-specific connection
- **Frame Buffer**: Receive bitmap/pixel updates
- **Image Encoding**: Encode updates as PNG/JPEG
- **Optimization**: Dirty rectangle tracking, compression
- **Input**: Mouse and keyboard events

#### Frame Buffer Management

```rust
pub struct FrameBuffer {
    width: u32,
    height: u32,
    data: Vec<u8>,  // RGBA pixels
    dirty_rects: Vec<Rect>,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
            dirty_rects: Vec::new(),
        }
    }

    /// Update a region of the framebuffer
    pub fn update_region(&mut self, x: u32, y: u32, width: u32, height: u32, pixels: &[u8]) {
        for row in 0..height {
            let dst_offset = ((y + row) * self.width + x) as usize * 4;
            let src_offset = (row * width) as usize * 4;
            let len = (width * 4) as usize;

            self.data[dst_offset..dst_offset + len]
                .copy_from_slice(&pixels[src_offset..src_offset + len]);
        }

        self.dirty_rects.push(Rect { x, y, width, height });
    }

    /// Merge overlapping dirty rectangles
    pub fn optimize_dirty_rects(&mut self) {
        if self.dirty_rects.len() < 2 {
            return;
        }

        // Sort by position
        self.dirty_rects.sort_by_key(|r| (r.y, r.x));

        // Merge adjacent/overlapping rectangles
        let mut merged = Vec::new();
        let mut current = self.dirty_rects[0];

        for rect in &self.dirty_rects[1..] {
            if current.intersects(rect) || current.is_adjacent(rect) {
                current = current.union(rect);
            } else {
                merged.push(current);
                current = *rect;
            }
        }
        merged.push(current);

        self.dirty_rects = merged;
    }

    /// Encode dirty region to PNG
    pub fn encode_region(&self, rect: Rect) -> Result<Vec<u8>> {
        let mut img = RgbaImage::new(rect.width, rect.height);

        for row in 0..rect.height {
            for col in 0..rect.width {
                let src_offset = (((rect.y + row) * self.width + rect.x + col) * 4) as usize;
                let pixel = Rgba([
                    self.data[src_offset],
                    self.data[src_offset + 1],
                    self.data[src_offset + 2],
                    self.data[src_offset + 3],
                ]);
                img.put_pixel(col, row, pixel);
            }
        }

        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)?;

        Ok(png_data)
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_rects.clear();
    }
}
```

#### RDP Handler Pattern

```rust
pub struct RdpHandler {
    config: Arc<RdpConfig>,
}

impl RdpHandler {
    async fn handle_rdp_connection(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        // 1. Parse connection parameters
        let host = params.get("hostname").ok_or(Error::MissingParam("hostname"))?;
        let port = params.get("port").and_then(|p| p.parse().ok()).unwrap_or(3389);
        let username = params.get("username").ok_or(Error::MissingParam("username"))?;
        let password = params.get("password").ok_or(Error::MissingParam("password"))?;
        let domain = params.get("domain").unwrap_or(&String::new()).clone();

        // 2. Configure RDP client
        let connector = ironrdp::ClientConnector::new()
            .with_server_addr((host.clone(), port))
            .with_credentials(username.clone(), password.clone())
            .with_domain(domain)
            .with_desktop_size(ironrdp::DesktopSize {
                width: 1920,
                height: 1080,
            });

        // 3. Connect
        let stream = TcpStream::connect((host.as_str(), port)).await?;
        let ironrdp::ConnectorResult {
            mut connection,
            ..
        } = connector.connect(stream).await?;

        // 4. Initialize framebuffer
        let mut framebuffer = FrameBuffer::new(1920, 1080);

        // 5. Event loop
        loop {
            tokio::select! {
                // RDP events
                event = connection.next_event() => {
                    match event? {
                        RdpEvent::Bitmap(bitmap) => {
                            // Update framebuffer
                            framebuffer.update_region(
                                bitmap.dest_left,
                                bitmap.dest_top,
                                bitmap.width,
                                bitmap.height,
                                &bitmap.data,
                            );

                            // Optimize dirty rectangles
                            framebuffer.optimize_dirty_rects();

                            // Send updates
                            for rect in framebuffer.dirty_rects.iter() {
                                let png_data = framebuffer.encode_region(*rect)?;

                                guac_tx.send(GuacInstruction {
                                    opcode: "img".to_string(),
                                    args: vec![
                                        "0".to_string(),
                                        "image/png".to_string(),
                                        base64::encode(&png_data),
                                    ],
                                }).await?;

                                // Position the image
                                guac_tx.send(GuacInstruction {
                                    opcode: "move".to_string(),
                                    args: vec![
                                        "0".to_string(),
                                        rect.x.to_string(),
                                        rect.y.to_string(),
                                    ],
                                }).await?;
                            }

                            framebuffer.clear_dirty();

                            // Send sync
                            guac_tx.send(GuacInstruction {
                                opcode: "sync".to_string(),
                                args: vec![timestamp_ms().to_string()],
                            }).await?;
                        },

                        RdpEvent::Pointer(cursor) => {
                            // Send cursor update
                            guac_tx.send(GuacInstruction {
                                opcode: "cursor".to_string(),
                                args: vec![
                                    cursor.x.to_string(),
                                    cursor.y.to_string(),
                                    "0".to_string(),  // layer
                                ],
                            }).await?;
                        },

                        RdpEvent::Audio(audio) => {
                            // Send audio data
                            guac_tx.send(GuacInstruction {
                                opcode: "audio".to_string(),
                                args: vec![
                                    "audio/L16".to_string(),
                                    base64::encode(&audio.data),
                                ],
                            }).await?;
                        },

                        _ => {}
                    }
                },

                // Guacamole input
                Some(instruction) = guac_rx.recv() => {
                    match instruction.opcode.as_str() {
                        "mouse" => {
                            let x: u16 = instruction.args[0].parse()?;
                            let y: u16 = instruction.args[1].parse()?;
                            let button_mask: u8 = instruction.args[2].parse()?;

                            connection.send_mouse_event(x, y, button_mask).await?;
                        },

                        "key" => {
                            let keysym: u32 = instruction.args[0].parse()?;
                            let pressed: bool = instruction.args[1] == "1";

                            // Convert X11 keysym to Windows scancode
                            let scancode = x11_to_scancode(keysym);
                            connection.send_key_event(scancode, pressed).await?;
                        },

                        "clipboard" => {
                            let clipboard_data = &instruction.args[1];
                            connection.send_clipboard(clipboard_data.as_bytes()).await?;
                        },

                        _ => {}
                    }
                },
            }
        }
    }
}

fn x11_to_scancode(keysym: u32) -> u16 {
    // X11 keysym to Windows scancode mapping
    match keysym {
        0x0061 => 0x1E,  // A
        0x0062 => 0x30,  // B
        0x0063 => 0x2E,  // C
        // ... full mapping table
        _ => 0,
    }
}
```

---

### 3. Database Protocols (MySQL, PostgreSQL, SQL Server)

**Key Components:**
- **SQL Client**: Connect to database
- **Query Execution**: Run SQL queries
- **Result Formatting**: Format results as tables
- **Virtual Terminal**: Emulate SQL terminal UI

#### SQL Terminal Emulator

```rust
pub struct SqlTerminal {
    rows: u16,
    cols: u16,
    buffer: Vec<String>,
    cursor_row: u16,
    cursor_col: u16,
    prompt: String,
}

impl SqlTerminal {
    pub fn new(rows: u16, cols: u16, prompt: &str) -> Self {
        Self {
            rows,
            cols,
            buffer: vec![String::new(); rows as usize],
            cursor_row: 0,
            cursor_col: 0,
            prompt: prompt.to_string(),
        }
    }

    pub fn write_prompt(&mut self) {
        self.write_str(&self.prompt);
    }

    pub fn write_str(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    pub fn write_char(&mut self, c: char) {
        if c == '\n' {
            self.newline();
        } else {
            if self.cursor_col >= self.cols {
                self.newline();
            }

            self.buffer[self.cursor_row as usize].push(c);
            self.cursor_col += 1;
        }
    }

    pub fn newline(&mut self) {
        self.cursor_row += 1;
        self.cursor_col = 0;

        if self.cursor_row >= self.rows {
            // Scroll
            self.buffer.remove(0);
            self.buffer.push(String::new());
            self.cursor_row = self.rows - 1;
        }
    }

    pub fn write_table(&mut self, result: &QueryResult) -> Result<()> {
        if result.columns.is_empty() {
            return Ok(());
        }

        // Calculate column widths
        let mut widths: Vec<usize> = result.columns.iter()
            .map(|c| c.len())
            .collect();

        for row in &result.values {
            for (i, val) in row.iter().enumerate() {
                widths[i] = widths[i].max(val.len());
            }
        }

        // Header separator
        let separator = widths.iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("+");

        // Write header
        self.write_str(&format!("+{}+\n", separator));

        let header = result.columns.iter()
            .zip(&widths)
            .map(|(col, width)| format!(" {:width$} ", col, width = width))
            .collect::<Vec<_>>()
            .join("|");

        self.write_str(&format!("|{}|\n", header));
        self.write_str(&format!("+{}+\n", separator));

        // Write rows
        for row in &result.values {
            let row_str = row.iter()
                .zip(&widths)
                .map(|(val, width)| format!(" {:width$} ", val, width = width))
                .collect::<Vec<_>>()
                .join("|");

            self.write_str(&format!("|{}|\n", row_str));
        }

        self.write_str(&format!("+{}+\n", separator));
        self.write_str(&format!("{} rows\n\n", result.values.len()));

        Ok(())
    }

    pub fn render(&self) -> Result<Vec<u8>> {
        let font = load_terminal_font()?;
        let mut img = RgbaImage::new(
            self.cols as u32 * CHAR_WIDTH,
            self.rows as u32 * CHAR_HEIGHT,
        );

        // Background
        for pixel in img.pixels_mut() {
            *pixel = Rgba([0, 0, 0, 255]);
        }

        // Render each line
        for (row, line) in self.buffer.iter().enumerate() {
            for (col, c) in line.chars().enumerate() {
                draw_char(
                    &mut img,
                    c,
                    col as u32 * CHAR_WIDTH,
                    row as u32 * CHAR_HEIGHT,
                    Rgba([255, 255, 255, 255]),
                    &font,
                )?;
            }
        }

        // Cursor
        draw_cursor(
            &mut img,
            self.cursor_col as u32 * CHAR_WIDTH,
            self.cursor_row as u32 * CHAR_HEIGHT,
        )?;

        // Encode
        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)?;

        Ok(png_data)
    }
}
```

---

## Performance Optimization Strategies

### 1. Dirty Rectangle Optimization

Only send changed regions instead of full screen:

```rust
impl DirtyRectOptimizer {
    pub fn optimize(rects: Vec<Rect>) -> Vec<Rect> {
        if rects.is_empty() {
            return Vec::new();
        }

        // Merge small adjacent rectangles
        let merged = Self::merge_adjacent(rects);

        // If many small rects, send full screen instead
        if merged.len() > 10 {
            vec![Rect { x: 0, y: 0, width: SCREEN_WIDTH, height: SCREEN_HEIGHT }]
        } else {
            merged
        }
    }
}
```

### 2. Image Compression

Use appropriate compression for content type:

```rust
pub fn encode_region_optimized(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    // Analyze content
    let is_text = analyze_content(pixels);

    if is_text {
        // Text: use PNG with max compression
        encode_png(pixels, width, height, CompressionLevel::High)
    } else {
        // Graphics: use JPEG for better compression
        encode_jpeg(pixels, width, height, 85)
    }
}
```

### 3. Frame Rate Control

Limit frame rate to reduce bandwidth:

```rust
pub struct FrameRateController {
    target_fps: u32,
    last_frame: Instant,
}

impl FrameRateController {
    pub async fn wait_for_next_frame(&mut self) {
        let frame_duration = Duration::from_millis(1000 / self.target_fps as u64);
        let elapsed = self.last_frame.elapsed();

        if elapsed < frame_duration {
            tokio::time::sleep(frame_duration - elapsed).await;
        }

        self.last_frame = Instant::now();
    }
}
```

---

## Testing Handlers

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ssh_key_conversion() {
        let handler = SshHandler::new();

        // Test Enter key
        let instruction = GuacInstruction {
            opcode: "key".to_string(),
            args: vec!["65293".to_string(), "1".to_string()],  // Return key
        };

        let result = handler.handle_key_event(&instruction).unwrap();
        assert_eq!(result, vec![b'\r']);
    }

    #[tokio::test]
    async fn test_terminal_rendering() {
        let mut terminal = TerminalEmulator::new(24, 80);
        terminal.process(b"Hello, World!\n").unwrap();

        let png = terminal.render_region(Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        }).unwrap();

        assert!(!png.is_empty());
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n");  // PNG magic
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_ssh_full_connection() {
    // Start mock SSH server
    let mock_server = MockSshServer::start().await;

    // Create handler
    let handler = SshHandler::new();

    // Create channels
    let (guac_tx, mut guac_rx) = mpsc::channel(100);
    let (handler_tx, handler_rx) = mpsc::channel(100);

    // Connect
    let params = hashmap! {
        "hostname".to_string() => mock_server.addr(),
        "port".to_string() => mock_server.port().to_string(),
        "username".to_string() => "test".to_string(),
        "password".to_string() => "test".to_string(),
    };

    tokio::spawn(async move {
        handler.connect(params, handler_tx, handler_rx).await.unwrap();
    });

    // Should receive ready instruction
    let ready = tokio::time::timeout(Duration::from_secs(5), guac_rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(ready.opcode, "img");  // First screen render
}
```

---

## Common Pitfalls & Solutions

### Problem: High CPU usage during idle connections

**Solution:** Implement smart polling - only capture when changes detected

```rust
let mut idle_counter = 0;
let mut poll_interval = Duration::from_millis(100);

loop {
    if no_changes_detected() {
        idle_counter += 1;
        if idle_counter > 10 {
            // Reduce polling frequency
            poll_interval = Duration::from_millis(500);
        }
    } else {
        idle_counter = 0;
        poll_interval = Duration::from_millis(100);
    }

    tokio::time::sleep(poll_interval).await;
}
```

### Problem: Memory leaks from unclosed connections

**Solution:** Use RAII and proper cancellation

```rust
pub struct ConnectionGuard {
    session_id: String,
    registry: Arc<SessionRegistry>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        // Always cleanup on drop
        self.registry.remove(&self.session_id);
    }
}
```

### Problem: Slow screen updates

**Solution:** Batch multiple dirty rects into single instruction

```rust
// Bad: send each rect separately
for rect in dirty_rects {
    send_img(rect).await?;
}

// Good: batch into single message
let combined = combine_rects(dirty_rects);
send_img(combined).await?;
```

---

## Checklist for New Handler Implementation

- [ ] Implement `ProtocolHandler` trait
- [ ] Handle all Guacamole input opcodes (mouse, key, clipboard, size)
- [ ] Generate appropriate Guacamole output (img, sync, audio, etc.)
- [ ] Implement authentication (password, key, certificate)
- [ ] Error handling and reconnection logic
- [ ] Resource cleanup (Drop trait, cancellation)
- [ ] Metrics and tracing
- [ ] Unit tests for key functions
- [ ] Integration test with mock server
- [ ] Load test (100+ concurrent connections)
- [ ] Documentation (connection params, features, limitations)

---

## Resources

- [Guacamole Protocol Reference](https://guacamole.apache.org/doc/gug/guacamole-protocol.html)
- [Tokio Documentation](https://tokio.rs/tokio/tutorial)
- [russh Documentation](https://docs.rs/russh/)
- [ironrdp Documentation](https://docs.rs/ironrdp/)
- [sqlx Documentation](https://docs.rs/sqlx/)