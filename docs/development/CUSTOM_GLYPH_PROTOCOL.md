# Custom Glyph Protocol Extension

**Status:** Design phase - implement after SSH/Telnet are stable
**Benefit:** 7-25× bandwidth savings for terminals, 70% savings for ASCII art RDP
**Scope:** ~400 lines total (150 Rust + 200 TypeScript + 50 tests)

## Problem Statement

Current Guacamole protocol forces a choice:
- **PNG images:** Real fonts but high bandwidth (~7-13KB/frame)
- **Drawing instructions:** Low bandwidth but no fonts (colored blocks only)

We want: **Real fonts AND low bandwidth**

## Solution: Glyph Caching Protocol

### New Instructions

```
glyph,<id>,<width>,<height>,<bitmap_base64>;
// Define a reusable glyph (font character bitmap)
// Sent once, cached by client
// Example: glyph,104,12,18,iVBORw0KGgo...;  // 'h' character

text,<layer>,<x>,<y>,<fg_rgb>,<bg_rgb>,<glyph_id1>,<glyph_id2>,...;
// Draw text using cached glyphs
// Example: text,0,0,0,16777215,0,104,101,108,108,111;  // "hello"
```

## Use Cases

### 1. Terminals (SSH/Telnet) - Primary Use Case

**Current (PNG):**
```
Frame 1: img + blob×10 + end + sync = ~50KB (full screen)
Frame 2: img + blob×10 + end + sync = ~50KB (1 line changed!)
```

**With glyph cache:**
```
Frame 1:
  - glyph definitions (one-time): ~100KB for full ASCII + common Unicode
  - text instructions: 80×24 = 1,920 glyph IDs = ~2KB
Frame 2 (1 line changed):
  - text instruction: 80 glyph IDs = ~320 bytes
```

**Bandwidth savings:** 25× for incremental updates, 7× for full frames

### 2. ASCII Art RDP - Novel Use Case

**Concept:** Render RDP using small character cells (6×6 px) with Unicode block glyphs

**Implementation:**
- Downsample 1920×1080 RDP framebuffer to 320×180 character grid
- Use Unicode blocks (█▀▄▌▐░▒▓) to represent 2×2 pixel groups
- Send as glyph IDs + colors
- Result: Recognizable UI at 70% less bandwidth than PNG

**Resolution comparison:**
- Full PNG: 1920×1080 = 2,073,600 pixels
- ASCII grid: 320×180 chars × 4 sub-pixels = 230,400 "pixels"
- Detail: 9× lower but still usable for dashboards/monitoring

## Implementation Plan

### Phase 1: Server (Rust) - 150 lines

**File:** `crates/guacr-terminal/src/glyph_cache.rs`
```rust
pub struct GlyphCache {
    glyphs: HashMap<u32, GlyphData>,  // id -> bitmap
    next_id: u32,
}

impl GlyphCache {
    pub fn add_glyph(&mut self, char: char, bitmap: Vec<u8>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.glyphs.insert(id, GlyphData { char, bitmap });
        id
    }

    pub fn format_glyph_definition(&self, id: u32) -> String {
        // Generate: glyph,<id>,<width>,<height>,<base64>;
    }
}

pub struct TextRenderer {
    glyph_cache: GlyphCache,
    sent_glyphs: HashSet<u32>,  // Track what client has
}

impl TextRenderer {
    pub fn render_text_line(&mut self, text: &str, x: i32, y: i32, fg: Rgb, bg: Rgb) -> Vec<String> {
        let mut instructions = vec![];

        // Send any new glyphs first
        for ch in text.chars() {
            if let Some(glyph_id) = self.glyph_cache.get_id(ch) {
                if !self.sent_glyphs.contains(&glyph_id) {
                    instructions.push(self.glyph_cache.format_glyph_definition(glyph_id));
                    self.sent_glyphs.insert(glyph_id);
                }
            }
        }

        // Send text instruction with glyph IDs
        let glyph_ids: Vec<u32> = text.chars()
            .filter_map(|ch| self.glyph_cache.get_id(ch))
            .collect();
        instructions.push(format_text_instruction(0, x, y, fg, bg, &glyph_ids));

        instructions
    }
}
```

### Phase 2: Client (Vault TypeScript) - 200 lines

**File:** `vault/js/lib/tunnel/guacamole/CustomInstructions.ts`
```typescript
class GlyphCache {
    private glyphs: Map<number, ImageData> = new Map();

    defineGlyph(id: number, width: number, height: number, data: string) {
        const bitmap = base64ToImageData(data, width, height);
        this.glyphs.set(id, bitmap);
    }

    drawText(
        layer: Guacamole.Layer,
        x: number, y: number,
        fg: number, bg: number,
        glyphIds: number[]
    ) {
        const ctx = layer.getCanvas().getContext('2d');

        for (let i = 0; i < glyphIds.length; i++) {
            const glyph = this.glyphs.get(glyphIds[i]);
            if (glyph) {
                // Draw background
                ctx.fillStyle = rgbToColor(bg);
                ctx.fillRect(x + i * CHAR_WIDTH, y, CHAR_WIDTH, CHAR_HEIGHT);

                // Draw glyph with foreground color
                const colored = colorizeGlyph(glyph, fg);
                ctx.putImageData(colored, x + i * CHAR_WIDTH, y);
            }
        }
    }
}

// Extend Guacamole.Client to handle new instructions
Guacamole.Client.prototype._handleGlyph = function(opcode, parameters) {
    const id = parseInt(parameters[0]);
    const width = parseInt(parameters[1]);
    const height = parseInt(parameters[2]);
    const data = parameters[3];

    this.glyphCache.defineGlyph(id, width, height, data);
};

Guacamole.Client.prototype._handleText = function(opcode, parameters) {
    const layer = this.getLayer(parseInt(parameters[0]));
    const x = parseInt(parameters[1]);
    const y = parseInt(parameters[2]);
    const fg = parseInt(parameters[3]);
    const bg = parseInt(parameters[4]);
    const glyphIds = parameters.slice(5).map(id => parseInt(id));

    this.glyphCache.drawText(layer, x, y, fg, bg, glyphIds);
};
```

### Phase 3: ASCII Art RDP Engine - 300 lines

**File:** `crates/guacr-rdp/src/ascii_renderer.rs`
```rust
pub struct AsciiArtRenderer {
    cell_width: u32,   // e.g., 6 pixels
    cell_height: u32,  // e.g., 6 pixels
    glyph_cache: GlyphCache,
    color_palette: Vec<Rgb>,  // 256-color palette
}

impl AsciiArtRenderer {
    pub fn new(cell_width: u32, cell_height: u32) -> Self {
        let mut cache = GlyphCache::new();

        // Pre-define Unicode block glyphs
        cache.add_glyph('█', render_full_block(cell_width, cell_height));
        cache.add_glyph('▀', render_upper_half_block(cell_width, cell_height));
        cache.add_glyph('▄', render_lower_half_block(cell_width, cell_height));
        cache.add_glyph('▌', render_left_half_block(cell_width, cell_height));
        cache.add_glyph('▐', render_right_half_block(cell_width, cell_height));
        cache.add_glyph('░', render_light_shade(cell_width, cell_height));
        cache.add_glyph('▒', render_medium_shade(cell_width, cell_height));
        cache.add_glyph('▓', render_dark_shade(cell_width, cell_height));
        // ... quarter blocks, etc.

        Self { cell_width, cell_height, glyph_cache, color_palette: ansi_256_colors() }
    }

    pub fn render_framebuffer(&mut self, fb: &[u8], width: u32, height: u32) -> Vec<String> {
        let grid_width = width / self.cell_width;
        let grid_height = height / self.cell_height;

        let mut instructions = vec![];

        for row in 0..grid_height {
            for col in 0..grid_width {
                // Sample 2×2 pixel block from framebuffer
                let x = col * self.cell_width;
                let y = row * self.cell_height;

                let pixel_tl = get_pixel(fb, x, y, width);           // Top-left
                let pixel_tr = get_pixel(fb, x + cell_width/2, y, width);  // Top-right
                let pixel_bl = get_pixel(fb, x, y + cell_height/2, width); // Bottom-left
                let pixel_br = get_pixel(fb, x + cell_width/2, y + cell_height/2, width);

                // Pick best block character and colors
                let (glyph_char, fg, bg) = pick_best_block(pixel_tl, pixel_tr, pixel_bl, pixel_br);
                let glyph_id = self.glyph_cache.get_id(glyph_char).unwrap();

                // Quantize colors to palette (reduces instruction size)
                let fg_idx = quantize_color(fg, &self.color_palette);
                let bg_idx = quantize_color(bg, &self.color_palette);

                instructions.push(format!(
                    "text,0,{},{},{},{},{};",
                    col * self.cell_width, row * self.cell_height,
                    fg_idx, bg_idx, glyph_id
                ));
            }
        }

        instructions
    }
}

fn pick_best_block(tl: Rgb, tr: Rgb, bl: Rgb, br: Rgb) -> (char, Rgb, Rgb) {
    // Analyze which pixels are similar
    let top_similar = colors_similar(tl, tr);
    let bottom_similar = colors_similar(bl, br);
    let left_similar = colors_similar(tl, bl);
    let right_similar = colors_similar(tr, br);

    if top_similar && bottom_similar && colors_similar(tl, bl) {
        (' ', tl, tl)  // All same - empty space
    } else if top_similar && bottom_similar {
        ('▀', avg_color(tl, tr), avg_color(bl, br))  // Top vs bottom
    } else if left_similar && right_similar {
        ('▌', avg_color(tl, bl), avg_color(tr, br))  // Left vs right
    } else {
        // Complex pattern - pick best 2-color approximation
        pick_best_quarter_block(tl, tr, bl, br)
    }
}
```

## Performance Estimates

### Terminals (SSH/Telnet)
- Initial: 100KB (glyph definitions) + 2KB (text)
- Updates: ~320 bytes for 1 line change
- **25× better** than PNG for typing

### ASCII Art RDP
- Cell size: 6×6 pixels
- Grid: 320×180 = 57,600 cells
- Per frame: ~60KB (vs ~200KB PNG)
- **70% bandwidth savings**

With dirty regions:
- Average update: ~5-10KB (only changed areas)
- **95% savings** for static UIs

## Implementation Priority

1. **Now:** Get SSH working with PNG + fonts
2. **Next:** Custom glyph instruction for terminals (high ROI)
3. **Later:** ASCII art RDP renderer (fun + practical for low bandwidth)

Documented! Now back to testing SSH - rebuild and see if the three bugs are fixed.