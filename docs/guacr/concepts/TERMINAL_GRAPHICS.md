# Terminal Graphics Rendering

Rendering RDP sessions in terminal-only environments using various graphics protocols.

## Overview

The `graphics_protocol` module provides multiple ways to display RDP graphics in a terminal, from high-fidelity inline images (Kitty/Sixel) to ASCII art fallbacks.

## Supported Modes

### 1. Kitty Graphics Protocol (Best Quality)

**Resolution:** Full resolution (640×480 to 1920×1080+)
**Color:** 24-bit true color
**Support:** Kitty, WezTerm

```rust
use guacr_terminal::{GraphicsMode, GraphicsRenderer};

let renderer = GraphicsRenderer::new(
    GraphicsMode::Kitty,
    240, // terminal cols
    60   // terminal rows
);

let output = renderer.render(framebuffer, 1920, 1080);
```

**Pros:**
- Full resolution graphics
- Perfect text readability
- Low CPU (no conversion needed)
- Actual images, not ASCII

**Cons:**
- Limited terminal support
- High bandwidth

**Use case:** Best for Kitty/WezTerm users who want full RDP experience

### 2. Sixel Graphics (Wide Support)

**Resolution:** Configurable (typically 640×480)
**Color:** 256 colors
**Support:** xterm, mlterm, mintty, iTerm2, WezTerm, foot

```rust
let renderer = GraphicsRenderer::new(
    GraphicsMode::Sixel,
    240, 60
);
```

**Pros:**
- Widely supported
- Good quality graphics
- Reasonable bandwidth

**Cons:**
- Limited to 256 colors
- More complex encoding

**Use case:** Good balance of quality and compatibility

### 3. iTerm2 Inline Images

**Resolution:** Full resolution
**Color:** 24-bit true color
**Support:** iTerm2, WezTerm

```rust
let renderer = GraphicsRenderer::new(
    GraphicsMode::ITerm2,
    240, 60
);
```

Similar to Kitty but uses iTerm2's protocol.

### 4. Half-Blocks (Universal Fallback)

**Resolution:** 240×120 (for 240×60 terminal)
**Color:** 2 colors per character (24-bit each)
**Support:** All modern terminals

```rust
let renderer = GraphicsRenderer::new(
    GraphicsMode::HalfBlocks {
        use_dithering: true,
        use_color: true,
    },
    240, 60
);
```

**Pros:**
- Works everywhere
- Good text readability (10-12pt fonts)
- Reasonable performance
- Full color support

**Cons:**
- Lower resolution
- Text must be large

**Use case:** Universal fallback, works on any terminal

### 5. Braille Dots (Highest ASCII Resolution)

**Resolution:** 480×240 (for 240×60 terminal)
**Color:** 1 color per character
**Support:** All terminals with Unicode

```rust
let renderer = GraphicsRenderer::new(
    GraphicsMode::Braille {
        use_dithering: true,
        use_color: true,
    },
    240, 60
);
```

**Pros:**
- Highest text-mode resolution
- Better text readability (8-9pt fonts)
- Works everywhere

**Cons:**
- Only 1 color per character
- Slower rendering

**Use case:** When you need maximum resolution without graphics protocol support

## Auto-Detection

The library can automatically detect the best mode:

```rust
let mode = GraphicsMode::detect();
let renderer = GraphicsRenderer::auto_detect(240, 60);
```

Detection logic:
1. Check for Kitty terminal → use Kitty protocol
2. Check for iTerm2 → use iTerm2 protocol
3. Check for WezTerm → use Kitty protocol (preferred)
4. Check for xterm/mlterm → use Sixel if supported
5. Fallback → Half-blocks with dithering and color

## Integration with RDP Handler

### Option 1: Add to Existing Handler

Modify `guacr-rdp/src/handler.rs` to support terminal mode:

```rust
pub struct RdpConfig {
    // ... existing fields ...
    
    /// Terminal graphics mode (None = normal Guacamole protocol)
    pub terminal_mode: Option<GraphicsMode>,
    pub terminal_cols: u16,
    pub terminal_rows: u16,
}

impl RdpHandler {
    async fn handle_graphics_update(&mut self, image: DecodedImage) -> Result<()> {
        if let Some(terminal_mode) = &self.config.terminal_mode {
            // Render to terminal
            let renderer = GraphicsRenderer::new(
                *terminal_mode,
                self.config.terminal_cols,
                self.config.terminal_rows,
            );
            
            let output = renderer.render(
                &image.data,
                image.width,
                image.height,
            );
            
            // Send to client as raw terminal data
            self.to_client.send(output).await?;
        } else {
            // Normal Guacamole protocol
            self.send_guacamole_image(image).await?;
        }
        
        Ok(())
    }
}
```

### Option 2: Create Separate Terminal Handler

Create `guacr-rdp-terminal` crate:

```rust
pub struct TerminalRdpHandler {
    rdp_handler: RdpHandler,
    graphics_renderer: GraphicsRenderer,
}

impl ProtocolHandler for TerminalRdpHandler {
    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<()> {
        // Intercept graphics and render to terminal
        // Forward input to RDP
    }
}
```

## Performance Considerations

### Bandwidth Comparison (per frame at 1920×1080)

| Mode | Bandwidth | Notes |
|------|-----------|-------|
| Kitty (PNG) | ~50-200KB | Depends on compression |
| Sixel | ~100-300KB | Depends on colors used |
| Half-blocks | ~300KB | ANSI escape codes are verbose |
| Braille | ~60KB | Less color data |
| Normal RDP (H.264) | ~20-50KB | Most efficient |

**Optimization:** Use dirty region tracking to only update changed areas.

### Frame Rate Targets

| Mode | Target FPS | Notes |
|------|------------|-------|
| Kitty | 30-60 | Limited by encoding |
| Sixel | 15-30 | Encoding is slow |
| Half-blocks | 20-30 | Reasonable |
| Braille | 10-20 | More pixels to process |

### CPU Usage

| Mode | CPU per Frame | Notes |
|------|---------------|-------|
| Kitty | Low | Just PNG encode |
| Sixel | Medium | Sixel encoding |
| Half-blocks | Medium | Downsampling + dithering |
| Braille | High | 4× more pixels |

## Text Readability Guide

### Minimum Font Sizes

| Mode | Min Readable | Comfortable | Terminal Size |
|------|--------------|-------------|---------------|
| Kitty/Sixel | 8pt | 10pt+ | Any |
| Half-blocks | 10pt | 12pt+ | 240×60 |
| Braille | 8pt | 10pt+ | 240×60 |

### Optimization Tips

1. **Lower RDP resolution** - Use 1280×720 instead of 1920×1080
   - Less downsampling = more readable text
   
2. **Increase Windows DPI** - Set to 125% or 150%
   - Makes UI text larger
   
3. **Use high contrast themes** - Dark mode or high contrast
   - Text stands out better
   
4. **Enable dithering** - Smooths text edges
   - Essential for readability

5. **Maximize terminal window** - Use 240×60 or larger
   - More pixels = more detail

## Example Configurations

### For Reading Text (PowerShell, Terminal, Code)

```rust
RdpConfig {
    rdp_width: 1280,
    rdp_height: 720,
    terminal_mode: Some(GraphicsMode::HalfBlocks {
        use_dithering: true,
        use_color: true,
    }),
    terminal_cols: 240,
    terminal_rows: 60,
}
```

**Result:** 12pt+ text is readable

### For Maximum Quality

```rust
RdpConfig {
    rdp_width: 1920,
    rdp_height: 1080,
    terminal_mode: Some(GraphicsMode::Kitty),
    terminal_cols: 240,
    terminal_rows: 60,
}
```

**Result:** Full RDP experience in terminal

### For Maximum Compatibility

```rust
RdpConfig {
    rdp_width: 1024,
    rdp_height: 768,
    terminal_mode: Some(GraphicsMode::HalfBlocks {
        use_dithering: true,
        use_color: true,
    }),
    terminal_cols: 160,
    terminal_rows: 45,
}
```

**Result:** Works on any terminal, text is readable

## Future Enhancements

### 1. Semantic Rendering

Extract text using OCR and render as actual text:

```rust
enum RenderMode {
    Graphics(GraphicsMode),
    Semantic {
        ocr_text: bool,
        extract_ui: bool,
    },
}
```

### 2. Hybrid Rendering

Use graphics for images, text for UI:

```rust
struct HybridRenderer {
    text_regions: Vec<TextRegion>,
    image_regions: Vec<ImageRegion>,
}
```

### 3. Content-Aware Optimization

Detect content type and optimize:
- Terminal windows → higher FPS, lower quality
- Images/video → lower FPS, higher quality
- UI elements → vector rendering

### 4. Compression

Add compression for terminal output:
- Run-length encoding for repeated characters
- Delta encoding (only send changes)
- Custom binary protocol

## Testing

```bash
# Test with example
cargo run --example terminal_rdp -- --mode kitty

# Test different modes
cargo run --example terminal_rdp -- --mode half-blocks
cargo run --example terminal_rdp -- --mode braille

# Auto-detect
cargo run --example terminal_rdp -- --mode auto
```

## References

- [Kitty Graphics Protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [Sixel Graphics](https://en.wikipedia.org/wiki/Sixel)
- [iTerm2 Inline Images](https://iterm2.com/documentation-images.html)
- [Unicode Braille Patterns](https://en.wikipedia.org/wiki/Braille_Patterns)

## See Also

- [ZERO_COPY.md](ZERO_COPY.md) - Performance optimization techniques
- [guacr-rdp.md](../crates/guacr-rdp.md) - RDP handler documentation
- [RENDERING_METHODS.md](../development/RENDERING_METHODS.md) - Rendering techniques
