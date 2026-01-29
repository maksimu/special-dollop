# Guacamole Protocol Conversion Flow

## Overview

The system converts between **Guacamole protocol instructions** (text-based) and **native protocol commands** (binary/text). This document explains how that conversion works in both directions.

## Guacamole Protocol Format

The Guacamole protocol uses a simple text-based format:

```
LENGTH.OPCODE,LENGTH.ARG1,LENGTH.ARG2,...,LENGTH.ARGN;
```

Example:
```
3.key,5.65507,1.1;    <- Key press: keysym=65507 (Shift), pressed=1
4.size,4.1920,4.1080; <- Screen resize: 1920x1080
3.img,1.0,4.image/png,... <- Image data
```

### Key Components:
- **LENGTH**: Character count (not byte count for UTF-8!)
- **DOT (.)**: Separates length from content
- **COMMA (,)**: Separates arguments
- **SEMICOLON (;)**: Terminates instruction

## Architecture: Two-Layer System

```
┌─────────────────────────────────────────────────────────┐
│                  Browser/Client                         │
│              (Guacamole JavaScript)                     │
└─────────────────────────────────────────────────────────┘
                         │
                         │ Guacamole Protocol (Text)
                         │ "3.key,5.65507,1.1;"
                         ↓
┌─────────────────────────────────────────────────────────┐
│            keeper-webrtc Channel Layer                  │
│         (Protocol Parser & Frame Handler)               │
│                                                          │
│  ┌────────────────────────────────────────────────┐   │
│  │ guacd_parser.rs - Parse Guacamole instructions │   │
│  │ • peek_instruction() - Zero-copy parsing        │   │
│  │ • SIMD-optimized UTF-8 handling                 │   │
│  │ • 398-2213ns per frame                          │   │
│  └────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                         │
                         │ Bytes (Raw Guacamole)
                         │
                         ↓
┌─────────────────────────────────────────────────────────┐
│            guacr Protocol Handlers                      │
│         (SSH, Telnet, RDP, VNC, etc.)                  │
│                                                          │
│  ┌────────────────────────────────────────────────┐   │
│  │ Parse Guacamole → Native Protocol              │   │
│  │ • "key" → SSH/Telnet bytes                     │   │
│  │ • "size" → Window resize                        │   │
│  │                                                  │   │
│  │ Generate Guacamole ← Native Protocol           │   │
│  │ • Terminal output → "img" instruction           │   │
│  │ • PNG frames @ 10fps (dirty detection)         │   │
│  └────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                         │
                         │ Native Protocol (Binary)
                         │ SSH: Raw bytes
                         │ RDP: RDP packets
                         ↓
┌─────────────────────────────────────────────────────────┐
│              Remote Server (SSH/RDP/etc)                │
└─────────────────────────────────────────────────────────┘
```

## Inbound Flow: Client → Server

### Example: User Types 'h' in SSH Session

**Step 1: Browser generates Guacamole instruction**
```javascript
// Guacamole JavaScript client
connection.sendKeyEvent(104, 1); // 'h' key pressed

// Generates:
"3.key,3.104,1.1;"
//  ^^^ ^^^ ^^^ ^
//  |   |   |   pressed (1 = down, 0 = up)
//  |   |   keysym (104 = 'h')
//  |   opcode
//  length
```

**Step 2: WebRTC sends to keeper-webrtc**
```
Frame {
    connection_no: 1,
    timestamp_ms: 1699999999,
    payload: b"3.key,3.104,1.1;"
}
```

**Step 3: guacd_parser.rs parses instruction (SIMD-optimized)**

File: `crates/keeper-webrtc/src/channel/guacd_parser.rs`

```rust
pub fn peek_instruction(buf: &[u8]) -> Result<PeekedInstruction, PeekError> {
    // Zero-copy parsing with SIMD UTF-8 validation
    // Performance: 398-477ns for small frames

    // Parse: "3.key,3.104,1.1;"
    // Result:
    PeekedInstruction {
        opcode: "key",           // Borrowed slice, no allocation
        args: ["104", "1"],      // SmallVec, stack-allocated
        total_length_in_buffer: 16,
        is_error_opcode: false,
    }
}
```

**Step 4: Forward to protocol handler**

File: `crates/keeper-webrtc/src/channel/handler_connections.rs`

```rust
// TODO (not yet wired): Forward frame.payload to handler
if let Some(sender) = channel.handler_senders.get(&conn_no) {
    sender.send(frame.payload).await?;
}
```

**Step 5: SSH handler converts Guacamole → SSH bytes**

File: `crates/guacr-ssh/src/handler.rs:222-249`

```rust
// Receive from WebRTC
Some(msg) = from_client.recv() => {
    let msg_str = String::from_utf8_lossy(&msg);

    // Parse instruction
    if msg_str.contains(".key,") {
        // Manual parsing (simplified)
        // Extract: keysym=104, pressed=1

        // Convert X11 keysym → terminal bytes
        let bytes = x11_keysym_to_bytes(104, true);
        // bytes = b"h" (single ASCII char)

        // Send to SSH server
        channel.data(&bytes[..]).await?;
    }
}
```

**Step 6: SSH client sends to remote server**
```
SSH packet → Remote server receives 'h'
```

### Keysym Conversion (X11 → Terminal)

File: `crates/guacr-terminal/src/keysym.rs`

```rust
pub fn x11_keysym_to_bytes(keysym: u32, pressed: bool) -> Vec<u8> {
    if !pressed {
        return Vec::new(); // Only process key down
    }

    match keysym {
        // ASCII printable (0x0020-0x007E)
        0x0020..=0x007E => vec![keysym as u8],  // Direct mapping

        // Special keys → VT100 escape sequences
        0xFF0D => vec![b'\r'],                   // Return
        0xFF08 => vec![0x7F],                    // Backspace
        0xFF09 => vec![b'\t'],                   // Tab
        0xFF1B => vec![0x1B],                    // Escape

        // Arrow keys
        0xFF51 => vec![0x1B, b'[', b'D'],        // Left
        0xFF52 => vec![0x1B, b'[', b'A'],        // Up
        0xFF53 => vec![0x1B, b'[', b'C'],        // Right
        0xFF54 => vec![0x1B, b'[', b'B'],        // Down

        // Function keys (F1-F12)
        0xFFBE => vec![0x1B, b'O', b'P'],        // F1
        // ... etc

        _ => Vec::new(),                         // Unsupported
    }
}
```

## Outbound Flow: Server → Client

### Example: SSH Server Sends "hello\n"

**Step 1: SSH handler receives data**

File: `crates/guacr-ssh/src/handler.rs:180-208`

```rust
// Receive from SSH channel
Some(msg) = ssh_channel.wait() => {
    match msg {
        russh::ChannelMsg::Data { data } => {
            // data = b"hello\n"

            // Update terminal emulator
            terminal.process(&data)?;
            terminal.mark_dirty();

            // Rate-limited screen rendering (10fps)
            if terminal.is_dirty() && last_render.elapsed() > Duration::from_millis(100) {
                // Render terminal to PNG
                let png = renderer.render_screen(
                    terminal.screen(),
                    rows,
                    cols
                )?;

                // Format as Guacamole "img" instruction
                let img_instruction = renderer.format_img_instruction(&png);

                // Send to client
                to_client.send(Bytes::from(img_instruction)).await?;
            }
        }
    }
}
```

**Step 2: Terminal emulation (VT100)**

File: `crates/guacr-terminal/src/emulator.rs`

```rust
pub struct TerminalEmulator {
    parser: vt100::Parser,  // VT100/ANSI escape sequence parser
    dirty: bool,
}

impl TerminalEmulator {
    pub fn process(&mut self, data: &[u8]) -> Result<()> {
        self.parser.process(data);  // Parse ANSI codes
        self.dirty = true;
        Ok(())
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()  // Get rendered screen state
    }
}
```

**Step 3: Render to PNG**

File: `crates/guacr-terminal/src/renderer.rs:28-57`

```rust
pub fn render_screen(&self, screen: &vt100::Screen, rows: u16, cols: u16) -> Result<Vec<u8>> {
    let width = cols as u32 * 9;   // 9 pixels per char
    let height = rows as u32 * 16;  // 16 pixels per row

    let mut img = RgbImage::new(width, height);

    // Render each cell
    for row in 0..rows {
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                self.render_cell(&mut img, cell, row, col, has_cursor)?;
            }
        }
    }

    // Encode to PNG
    let mut png_data = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)?;

    Ok(png_data)
}
```

**Step 4: Format as Guacamole instruction**

File: `crates/guacr-terminal/src/renderer.rs:135-154`

```rust
pub fn format_img_instruction(&self, png_data: &[u8]) -> String {
    // Base64 encode PNG data
    let encoded = BASE64_STANDARD.encode(png_data);

    // Format: "img,layer,mimetype,x,y,data;"
    format!(
        "3.img,1.0,9.image/png,1.0,1.0,{}.{};",
        encoded.len(),
        encoded
    )

    // Example output:
    // "3.img,1.0,9.image/png,1.0,1.0,4532.iVBORw0KGgoAAAANS...CYII=;"
    //  ^^^^ ^^^  ^^^^^^^^^  ^^^  ^^^  ^^^^  ^^^^^^^^^^^^^^^^^^^
    //  |    |    |           |    |    |     base64 PNG data
    //  |    |    |           |    |    length
    //  |    |    |           |    y coordinate
    //  |    |    |           x coordinate
    //  |    |    MIME type
    //  |    layer
    //  opcode
}
```

**Step 5: Send to WebRTC**

```rust
// In handler outbound task (handler_connections.rs:100-136)
while let Some(msg) = handler_rx.recv().await {
    // Wrap in Frame
    let frame = Frame {
        connection_no: 1,
        timestamp_ms: now_ms(),
        payload: msg,  // "3.img,1.0,9.image/png,..."
    };

    // Encode and send to WebRTC
    dc.send(encode_frame(frame)).await?;
}
```

**Step 6: Browser renders PNG**

```javascript
// Guacamole JavaScript client
tunnel.oninstruction = function(opcode, args) {
    if (opcode === "img") {
        let layer = args[0];    // "0"
        let mimetype = args[1]; // "image/png"
        let x = args[2];        // "0"
        let y = args[3];        // "0"
        let data = args[4];     // base64 PNG

        // Decode and render
        let img = new Image();
        img.src = "data:image/png;base64," + data;
        layer.drawImage(img, x, y);
    }
};
```

## Performance Optimizations

### 1. SIMD-Optimized Parsing (guacd_parser.rs)

```rust
// SSE2 SIMD for UTF-8 validation (x86_64)
unsafe {
    let ascii_mask = _mm_set1_epi8(0x80u8 as i8);

    while pos + 16 <= slice.len() {
        let chunk = _mm_loadu_si128(slice.as_ptr().add(pos) as *const __m128i);
        let has_non_ascii = _mm_movemask_epi8(_mm_and_si128(chunk, ascii_mask));

        if has_non_ascii != 0 {
            break; // Found non-ASCII
        }
        pos += 16; // Process 16 bytes at once
    }
}
```

**Result:** 398-477ns per small frame vs 1-2μs without SIMD

### 2. Zero-Copy Parsing

```rust
pub struct PeekedInstruction<'a> {
    pub opcode: &'a str,              // Borrowed, no allocation
    pub args: SmallVec<[&'a str; 4]>, // Stack-allocated for ≤4 args
    // ...
}
```

**Result:** No heap allocations for common instructions

### 3. Dirty Detection & Rate Limiting

```rust
// Only render when screen changes
if terminal.is_dirty() && last_render.elapsed() > Duration::from_millis(100) {
    render_and_send();
    terminal.clear_dirty();
}
```

**Result:** 10fps max (100ms between frames) instead of 1000s/sec

### 4. Character Cell Rendering

Instead of full-screen text rendering:
- Each character = 9x16 pixel block
- Simple color fills for background
- Cursor rendered as inverted cell

**Result:** Fast rendering, ~5-10ms for 80x24 terminal

## Protocol Handler Interface

All handlers implement this trait:

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    fn name(&self) -> &str;

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,      // Send Guacamole instructions
        from_client: mpsc::Receiver<Bytes>,  // Receive Guacamole instructions
    ) -> Result<()>;

    async fn health_check(&self) -> Result<HealthStatus>;
    async fn stats(&self) -> Result<HandlerStats>;
}
```

## Supported Instructions

### Inbound (Client → Server)

| Instruction | Format | Handler Action |
|-------------|--------|----------------|
| `key` | `3.key,5.KEYSYM,1.PRESSED;` | Convert keysym → terminal bytes |
| `size` | `4.size,4.COLS,4.ROWS;` | Resize terminal/window |
| `mouse` | `5.mouse,1.X,1.Y;` | Forward mouse event (RDP/VNC) |
| `clipboard` | `9.clipboard,N.DATA;` | Set clipboard content |

### Outbound (Server → Client)

| Instruction | Format | Description |
|-------------|--------|-------------|
| `img` | `3.img,LAYER,MIME,X,Y,LEN.DATA;` | Display image (PNG) |
| `size` | `4.size,4.COLS,4.ROWS;` | Request resize |
| `sync` | `4.sync,10.TIMESTAMP;` | Keepalive |
| `error` | `5.error,N.MESSAGE,N.CODE;` | Error notification |

## Example: Complete SSH Keystroke Flow

```
User Types: 'h'
    ↓
Browser: Generate instruction
    "3.key,3.104,1.1;"
    ↓
WebRTC DataChannel
    Frame { conn_no: 1, payload: b"3.key,3.104,1.1;" }
    ↓
keeper-webrtc: Parse (398ns)
    PeekedInstruction { opcode: "key", args: ["104", "1"] }
    ↓
[TODO: Forward to handler]
    handler_senders[1].send(payload)
    ↓
SSH Handler: Convert
    x11_keysym_to_bytes(104, true) → b"h"
    ↓
russh: Send SSH packet
    SSH_MSG_CHANNEL_DATA { data: b"h" }
    ↓
Remote SSH Server
    bash receives 'h'
    ↓
Remote SSH Server
    bash echoes "h\n"
    ↓
russh: Receive SSH packet
    SSH_MSG_CHANNEL_DATA { data: b"h\n" }
    ↓
SSH Handler: Process
    terminal.process(b"h\n")
    ↓
Terminal Emulator
    vt100::Parser updates screen buffer
    ↓
SSH Handler: Render (dirty check)
    Every 100ms if dirty:
    - render_screen() → PNG (9x16 * 80x24 = ~5-10ms)
    - format_img_instruction() → Guacamole
    ↓
keeper-webrtc: Send
    Frame { conn_no: 1, payload: b"3.img,1.0,9.image/png,..." }
    ↓
WebRTC DataChannel
    ↓
Browser: Render
    Decode base64 PNG → draw on canvas
    ↓
User sees 'h' on screen
```

**Total latency:** ~150-250ms (network + rendering)

## Files Reference

### Parsing & Protocol
- `crates/keeper-webrtc/src/channel/guacd_parser.rs` - SIMD-optimized Guacamole parser
- `crates/keeper-webrtc/src/channel/protocol.rs` - Protocol message handling
- `crates/keeper-webrtc/src/channel/handler_connections.rs` - Handler invocation

### SSH Handler
- `crates/guacr-ssh/src/handler.rs` - SSH protocol handler
- `crates/guacr-terminal/src/keysym.rs` - X11 keysym → terminal conversion
- `crates/guacr-terminal/src/emulator.rs` - VT100 terminal emulator
- `crates/guacr-terminal/src/renderer.rs` - Screen → PNG rendering

### Integration
- `crates/guacr-handlers/src/integration.rs` - Handler registry integration
- `crates/guacr-handlers/src/handler.rs` - ProtocolHandler trait

## Testing Protocol Conversion

```rust
#[test]
fn test_key_instruction_parsing() {
    let input = b"3.key,3.104,1.1;";
    let result = GuacdParser::peek_instruction(input).unwrap();

    assert_eq!(result.opcode, "key");
    assert_eq!(result.args[0], "104");
    assert_eq!(result.args[1], "1");
}

#[test]
fn test_keysym_conversion() {
    let bytes = x11_keysym_to_bytes(104, true); // 'h'
    assert_eq!(bytes, vec![b'h']);

    let bytes = x11_keysym_to_bytes(0xFF51, true); // Left arrow
    assert_eq!(bytes, vec![0x1B, b'[', b'D']); // ESC [ D
}
```

## Summary

The protocol conversion is a two-layer system:

1. **keeper-webrtc layer**: Fast, zero-copy Guacamole protocol parsing with SIMD optimization
2. **guacr-handlers layer**: Protocol-specific conversion (Guacamole ↔ SSH/RDP/VNC/etc.)

The design prioritizes:
- **Performance**: SIMD parsing, zero-copy, dirty detection
- **Correctness**: Proper UTF-8 handling, VT100 compatibility
- **Modularity**: Clean handler interface for adding protocols

The missing piece is wiring the inbound path (see CONTINUE_HERE.md) to complete the bidirectional flow.
