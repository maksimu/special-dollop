# Terminal Rendering Methods - PNG vs Drawing Instructions

**Date:** 2025-11-14 (Updated)
**Context:** How different approaches render terminal text

## Critical Finding: How Apache guacd Actually Works

**We discovered guacd uses PNG + fonts, NOT colored blocks!**

Examining guacamole-server source code revealed:
```c
// guacd/src/terminal/display.c
PangoLayout* layout = pango_cairo_create_layout(cairo);  // Font rendering
pango_layout_set_text(layout, utf8, bytes);
pango_cairo_show_layout(cairo, layout);                  // Render to Cairo surface
guac_common_surface_draw(display, x, y, surface);       // Send as PNG
```

**guacd rendering pipeline:**
1. Use **Pango** (C font library) to render text with fonts
2. Draw to **Cairo** surface (bitmap)
3. Convert to **PNG**
4. Send via `img/blob/end` instructions
5. Optimize with **dirty region tracking** (only send changed portions)

**This is exactly what we're doing!** (fontdue + image crate instead of Pango + Cairo)

## Three Rendering Approaches

### 1. PNG Images with Fonts (What guacd Does - What We Do)
**How it works:** Render text with font library to bitmap, encode as PNG, send via `img/blob/end`
**Bandwidth:** ~7-13KB per frame full screen, ~200 bytes for single character (with dirty regions)
**Quality:** Real fonts with anti-aliasing, full Unicode support
**Compatibility:** Universal - works with all Guacamole clients
**What Apache guacd uses:** Pango + Cairo → PNG (we use fontdue + image crate → PNG)

**Our implementation:**
- Font: Noto Sans Mono (582KB embedded TTF)
- Rendering: fontdue for glyph rasterization
- Image: image crate for PNG encoding
- Optimization: Dirty region tracking (<30% dirty → region only)

### 2. Drawing Instructions (Colored Blocks - NOT what guacd uses)
**How it works:** Send `rect` and `cfill` instructions to draw colored rectangles
**Bandwidth:** ~1-2KB per frame (just coordinates and colors)
**Quality:** No actual fonts - characters render as solid colored blocks
**Compatibility:** Supported by standard Guacamole client
**NOT what guacd does:** This was our initial POC, not the production approach
**Trade-off:** Low bandwidth but no real text rendering

**When to use:**
- Proof of concept / testing
- Extreme low-bandwidth scenarios (satellite)
- Retro aesthetic

**Why we can't use this with fonts:**
Drawing instructions can only create solid rectangles. To render actual font glyphs with anti-aliasing requires bitmap data (PNG/JPEG).

### 3. Custom Glyph Instruction (Future - Best of Both Worlds)
**How it works:** Extend Guacamole protocol with custom glyph caching instructions
**Bandwidth:** ~100KB initial (glyph definitions) + ~320 bytes per update (glyph IDs only)
**Quality:** Real fonts with anti-aliasing (same as PNG)
**Compatibility:** Requires vault client modifications (custom protocol extension)
**Trade-off:** Implementation complexity for 7-25× bandwidth savings

**Proposed protocol extension:**
```
glyph,<id>,<width>,<height>,<bitmap_base64>;  // Define reusable glyph (sent once)
text,<layer>,<x>,<y>,<fg>,<bg>,<glyph_ids...>;  // Draw text using cached glyphs
```

**Implementation scope:**
- Server (Rust): ~150 lines (glyph cache + instruction formatting)
- Client (TypeScript): ~200 lines (glyph cache + rendering)
- Time estimate: 4-6 hours
- Bandwidth savings: 7-25× for incremental updates

**When to implement:**
After SSH/Telnet are stable and tested in production. This optimization makes sense for high-scale deployments (1000+ concurrent sessions).

## Protocol-Specific Recommendations

### ✅ SSH / Telnet - PNG (Current)
**Status:** Using PNG with size instruction synchronization
**Why PNG:** Proven to work, dimensions now match layer size
**Future:** Should migrate to drawing instructions for 7-14× bandwidth savings

**Code location:**
- `crates/guacr-ssh/src/handler.rs:303-327`
- `crates/guacr-terminal/src/renderer.rs:28-57` (PNG rendering)
- `crates/guacr-terminal/src/renderer.rs:181-270` (Drawing instructions - available but not used)

### ✅ RDP - MUST use PNG
**Why:** RDP streams arbitrary graphics (photos, video, complex UI)
**Cannot use drawing instructions:** Would need thousands of instructions for gradients, images, etc.
**Bandwidth:** ~50-200KB per frame (acceptable for graphics)

**Code location:**
- `crates/guacr-rdp/src/handler.rs`
- `crates/guacr-rdp/src/framebuffer.rs`

### ✅ VNC - MUST use PNG
**Why:** Same as RDP - streams arbitrary graphics
**Cannot use drawing instructions:** Complex graphics require raster format
**Bandwidth:** ~50-200KB per frame (acceptable for graphics)

**Code location:**
- `crates/guacr-vnc/src/handler.rs`

### ⚠️ Database Handlers - PNG (Simple UI)
**Status:** Using PNG via terminal renderer
**Why PNG:** Simple text-based UI, PNG is adequate
**Could optimize:** Drawing instructions would work but not critical

**Code location:**
- `crates/guacr-database/src/sql_terminal.rs:126`

## Why Drawing Instructions Failed (Black Screen)

When we tried switching to drawing instructions (`render_terminal_instructions()`), we saw a black screen. Likely causes:

1. **Volume:** 80×24 terminal = 1,920 cells × ~4 instructions/cell = **~7,680 instructions per frame**
2. **Missing layer initialization:** May need explicit layer clear/reset
3. **Batching:** Might need to send in batches with sync markers
4. **Compositing mode:** Using mode `15` in cfill - may not be correct

## The Current Fix (PNG Approach)

**Root cause of blurriness:** Dimension mismatch between layer size and PNG size
**Solution implemented:**
1. Parse `size` parameter from browser (was hardcoded to 80×24)
2. Send `size` instruction matching PNG dimensions
3. Update `size` instruction when terminal resizes
4. Result: No browser scaling = crisp rendering

**Bug fixed:** Array parameters (like `size: [1864, 1359, 192]`) were being converted to empty strings
**Fix location:** `crates/keeper-webrtc/src/channel/core.rs:276-291`

## Future: Migrate Terminals to Drawing Instructions

**Benefits:**
- 7-14× less bandwidth (~1-2KB vs ~7-13KB per frame)
- Always crisp (vector-based)
- Matches what Apache guacd does
- Better for low-bandwidth connections

**Investigation needed:**
1. Why did we see black screen?
2. Do we need layer clear/initialization?
3. Should we batch instructions?
4. Test with real Guacamole client to verify rendering

**Code ready:** `TerminalRenderer::render_terminal_instructions()` is already implemented
- Location: `crates/guacr-terminal/src/renderer.rs:181-270`
- Returns: `Vec<String>` of Guacamole protocol instructions
- Generates: `rect` + `cfill` pairs for each character cell

## Bandwidth Comparison

For 80×24 terminal with moderate text:

| Method | Bytes/Frame | Frames/Sec | KB/Second | Quality | Notes |
|--------|-------------|------------|-----------|---------|-------|
| PNG + fonts (current) | ~7,000 | 60 | ~410 KB/s | Real fonts, crisp | Production ready |
| Drawing inst. (blocks) | ~1,000 | 60 | ~60 KB/s | Colored rectangles only | No actual fonts |
| Custom glyph cache | ~320 | 60 | ~19 KB/s | Real fonts, crisp | Requires vault changes |

For 100 developers editing 4K video over terminals:
- PNG: **41 MB/s** aggregate bandwidth (acceptable for modern networks)
- Drawing blocks: **6 MB/s** (looks terrible, not recommended)
- Custom glyph: **2 MB/s** (best of both worlds, requires custom protocol)

## ASCII Art RDP (Efficient Low-Bandwidth RDP)

**Concept:** Render RDP as colored text characters using Unicode block elements

**Unicode blocks available:**
```
Full blocks: █ (U+2588)
Half blocks: ▀ ▄ ▌ ▐ (top, bottom, left, right)
Quarter blocks: ▖ ▗ ▘ ▝ ▞ ▚ (various combinations)
Shade blocks: ░ ▒ ▓ (25%, 50%, 75% density)
```

**Efficient implementation:**
1. Downsample RDP framebuffer: 1920×1080 → 240×135 "pixels"
2. Each character cell represents 2×2 pixels using quarter blocks
3. Map pixel colors to nearest ANSI color (16 colors)
4. Send as `cfill` instructions with Unicode characters
5. Result: ~18 KB/frame vs ~200 KB/frame PNG

**Bandwidth calculation:**
```
240×135 cells = 32,400 cells
Each cfill instruction: ~25 bytes
Total: ~810 KB per frame... wait, that's worse!

Better approach:
- Use line-based compression (run-length encoding)
- Typical RDP has lots of solid regions
- Compressed: ~20-50 KB per frame
- 75% bandwidth savings vs PNG
```

**When this makes sense:**
- Satellite links (<512 kbps)
- Emergency/disaster recovery scenarios
- Monitoring dashboards (low-detail acceptable)
- Retro terminals aesthetic

**When to avoid:**
- Photo editing, CAD, design work
- Anything requiring text readability
- Professional development environments

**Implementation effort:** ~400 lines
- Framebuffer downsampling: 80 lines
- Color quantization: 60 lines
- Unicode block selection: 80 lines
- Drawing instruction generation: 100 lines
- Run-length compression: 80 lines

## References

- Apache Guacamole Protocol: https://guacamole.apache.org/doc/gug/guacamole-protocol.html
- Protocol Reference: https://guacamole.apache.org/doc/gug/protocol-reference.html
- Size instruction fix: `BLURRY_IMAGE_FIX.md`
- Current session logs: User reported "first flash is clear then switches to pixelated"
  - Cause: size parameter empty string (array not converted)
  - Fix: Convert number arrays to comma-separated strings
