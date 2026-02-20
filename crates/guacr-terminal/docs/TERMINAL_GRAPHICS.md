# Terminal Graphics Rendering - Commander CLI

Terminal-based rendering of remote sessions (Sixel, Kitty, half-blocks) belongs
in the **Commander CLI client**, not in this Rust codebase.

All protocols -- RDP, VNC, SSH, Telnet, and Database -- send JPEG images via
Guacamole `img` + `blob` + `end` instructions. The Commander CLI needs to decode
these and render them to the user's terminal.

## Use case

Customers SSH into a headless server (no desktop environment, no browser) and
need to visually interact with remote sessions. The Commander CLI is their only
display. This covers:

- **RDP/VNC**: Graphical desktops rendered as JPEG dirty rects by the server
- **SSH/Telnet**: Terminal output rendered to JPEG by the server's terminal
  emulator (fontdue + JetBrains Mono). Also supports a `pipe` streaming path
  that bypasses image encoding entirely (see "SSH/Telnet pipe streaming" below)
- **Database**: Query results and interactive CLI rendered to JPEG by the
  server's database terminal renderer

For graphical protocols (RDP/VNC), this is a monitoring use case -- low FPS is
fine. For terminal protocols, the pipe streaming path gives full fidelity when
available; image rendering is the fallback.

## Why client-side (not server-side)

- The server already sends JPEG images via standard Guacamole `img` + `blob` + `end`
  instructions. No server changes needed.
- JPEG is bandwidth-efficient for transport over WebRTC. Sixel/Kitty data is much
  larger than JPEG and would be terrible for bandwidth.
- The client knows its own terminal capabilities (Sixel, Kitty, truecolor) --
  the server would need protocol negotiation to learn this.
- One rendering implementation serves all protocols (SSH, RDP, VNC, database, etc.).
- No server-side mode switching or protocol changes needed. Any Guacamole client
  (browser OR terminal) works with the same server.

## Where to implement

**Repository:** `~/Documents/commander`
**Location:** `keepercommander/commands/pam_launch/guac_cli/`

## Existing Commander code (by Ivan Dimov, KC-975)

The full Guacamole CLI client already exists:

- `decoder.py` (~350 lines) - Guacamole protocol parser (`LENGTH.ELEMENT,...;` format),
  X11 keysym mappings, instruction encode/decode
- `renderer.py` (~360 lines) - ANSI text renderer with virtual screen buffer. Handles
  `cursor`, `text`, `rect`, `cfill`, `copy`, `size`, `move`, `sync`. Refreshes display
  on sync instruction. Uses ANSI escape codes for positioning and 16-color output.
- `instructions.py` (~570 lines) - Instruction handler router. Key feature: STDOUT
  pipe/blob/end pattern for plaintext SSH/TTY streams. Server sends `pipe` with name
  "STDOUT", then `blob` with base64-encoded terminal output, client decodes and writes
  to `sys.stdout.buffer` directly.
- `input.py` (~330 lines) - Keyboard input handler. Converts stdin keystrokes to X11
  keysyms. Platform-specific readers (Unix termios, macOS, Windows msvcrt). Escape
  sequence detection for arrow keys, function keys, etc.
- `stdin_handler.py` (~640 lines) - Enhanced stdin handler using pipe/blob/end pattern.
  Multi-platform (Unix, macOS, Windows). Comprehensive VT100/xterm escape sequence
  mapping including modifiers (Shift+Arrow, Ctrl+Arrow).
- `guacamole/client.py` (~615 lines) - Full Guacamole client state machine. Connection
  lifecycle, stream management, sync/ack handling, keep-alive (5s interval).
- `guacamole/parser.py` (~313 lines) - Protocol parser with instruction routing.
- `guacamole/tunnel.py` - WebRTC/WebSocket tunnel management.

## What renderer.py currently does with images

```python
# renderer.py line 106-108
elif opcode == GuacOp.PNG.value or opcode == GuacOp.JPEG.value:
    # Image operations - log but don't render in text mode
    logging.debug(f"Ignoring image operation: {opcode}")
```

This is the gap. All protocol content (RDP, VNC, SSH, Telnet, Database) arrives
as `img` + `blob` (JPEG chunks) + `end`. For terminal display, these need to be
decoded and rendered to the user's terminal.

## How the image stream works (Guacamole protocol)

The server sends images as a stream:

1. `img` instruction: `3.img,{stream},{mask},{layer},{mimetype},{x},{y};`
   - Opens an image stream with position and MIME type
2. `blob` instructions: `4.blob,{stream},{base64_data};`
   - Multiple blob chunks, each ~6KB of base64 (~4.5KB raw)
   - All blobs for one image share the same stream ID
3. `end` instruction: `3.end,{stream};`
   - Closes the stream, image is complete

The client must reassemble all blob chunks for a given stream ID, base64-decode
them, and concatenate to get the full JPEG/PNG. The `x,y` from the `img` instruction
tells where to place the image on the display.

Stream IDs increment with each image. Multiple images may be in-flight.

### What each protocol sends

| Protocol | Image content | Typical size | Frequency |
|----------|--------------|--------------|-----------|
| RDP | Desktop dirty rects (partial screen updates) | 10-500KB JPEG | High (mouse movement, window redraws) |
| VNC | Framebuffer dirty rects | 10-500KB JPEG | High (similar to RDP) |
| SSH | Full terminal screen or dirty region (fontdue-rendered text) | 5-50KB JPEG | Medium (on keystroke/output) |
| Telnet | Full terminal screen or dirty region (fontdue-rendered text) | 5-50KB JPEG | Medium (on data received) |
| Database | Query results / interactive CLI (fontdue-rendered text) | 5-30KB JPEG | Low (on query execution / input) |

RDP and VNC send native graphical content -- the image IS the desktop. SSH,
Telnet, and Database send server-side rendered terminal text -- the Rust server
runs a terminal emulator (vt100), renders it to JPEG with fontdue, and sends
that JPEG. The client-side rendering is identical for all protocols: decode JPEG,
display in terminal.

## Rendering tiers

Three tiers, auto-detected, in order of preference:

### Tier 1: Kitty graphics protocol (best quality)

- **Color:** 24-bit, native JPEG passthrough
- **Terminals:** Kitty, WezTerm
- **Detection:** `$KITTY_WINDOW_ID` env var, or graphics protocol query
- **How it works:** Sends base64-encoded image chunks directly in escape sequences
  with placement IDs. Supports replace-by-id for dirty rect updates.
- **Key advantage:** Can send JPEG data directly from the blob stream without
  re-encoding. Just wrap the decoded JPEG bytes in Kitty escape sequences.
- **Dirty rects:** Built-in via placement IDs -- replace a specific screen region.

### Tier 2: Sixel graphics (wide support)

- **Color:** 256-color palette (median-cut or octree quantization)
- **Terminals:** xterm, foot, mintty, Contour, iTerm2, WezTerm, konsole
- **Detection:** DA1 (Device Attributes) query response
- **How it works:** DCS escape sequence with palette definition and bitmap data
  encoded as printable characters (6 vertical pixels per character). RLE compression.
- **Dirty rects:** Position cursor at changed region, emit partial Sixel.
- **Note:** 24-bit Sixel (truecolor) exists in some terminals but is non-standard.

### Tier 3: Half-blocks with truecolor (universal fallback)

- **Color:** 24-bit truecolor (2 colors per character cell: fg + bg)
- **Terminals:** Everything modern -- Windows Terminal, Terminal.app, PuTTY, etc.
- **Detection:** Fallback when Kitty and Sixel are not available.
- **How it works:** Uses Unicode upper-half-block character. Each character cell
  represents 2 vertical pixels using foreground and background colors:
  ```
  \x1b[38;2;R1;G1;B1;48;2;R2;G2;B2m\u2580
  ```
  Where R1,G1,B1 is the top pixel color (fg) and R2,G2,B2 is the bottom pixel (bg).
- **Resolution:** 2x vertical resolution vs plain text (each cell = 2 pixels tall).
  A 240-column terminal gives 240x120 effective pixel resolution.
- **Dirty rects:** ANSI cursor positioning to changed cell region, redraw only those cells.
- **This is the most important tier** -- it works on virtually every modern terminal
  and is the only option for many customer environments.

### Tier comparison

| Tier | Protocol | Color | Terminal Support | Bandwidth |
|------|----------|-------|-----------------|-----------|
| 1 | Kitty | 24-bit, native JPEG | Kitty, WezTerm | Low (JPEG passthrough) |
| 2 | Sixel | 256-color palette | xterm, foot, mintty, iTerm2, WezTerm | Medium |
| 3 | Half-block | 24-bit truecolor | Everything modern | High (ANSI escapes are verbose) |

## Terminal capability detection

```python
def detect_terminal_tier():
    # Tier 1: Kitty
    if os.environ.get('KITTY_WINDOW_ID'):
        return 'kitty'

    # Tier 1: WezTerm (supports Kitty protocol)
    if 'wezterm' in os.environ.get('TERM_PROGRAM', '').lower():
        return 'kitty'

    # Tier 2: Sixel -- query via DA1 (Device Attributes)
    # Send: ESC [ c
    # Response containing ";4;" indicates Sixel support
    # This requires reading terminal response, which is tricky in raw mode.
    # Alternative: check $TERM against known Sixel-capable terminals.
    term = os.environ.get('TERM', '')
    term_program = os.environ.get('TERM_PROGRAM', '').lower()
    if term_program in ('iterm2', 'mintty', 'contour'):
        return 'sixel'
    if 'xterm' in term and not term_program == 'apple_terminal':
        return 'sixel'

    # Tier 3: Half-blocks (universal)
    return 'halfblock'
```

## Image downsampling

For half-blocks and Sixel, the JPEG image (potentially 1920x1080) must be
downsampled to terminal cell resolution.

**Use area-average downsampling, NOT nearest-neighbor.** Nearest-neighbor drops
pixels and makes text on the remote desktop unreadable. Area-average blends
pixel neighborhoods and preserves text legibility.

With Pillow:
```python
from PIL import Image
img = img.resize((target_w, target_h), Image.LANCZOS)
```

For half-blocks at 240 columns: target is 240 x (rows * 2) pixels.
For Sixel: target is configurable, 640x480 is a reasonable default.
For Kitty: no downsampling needed, terminal handles it.

## Frame rate and performance

- **Target: 2-5 FPS** for monitoring use case. This saves bandwidth and avoids
  overwhelming the terminal.
- Coalesce dirty rects within a frame period -- if multiple `img` instructions
  arrive within one frame period, batch them.
- The server already does dirty rect optimization (only sends changed regions).
  The client should track positions from `img` instructions to only update
  affected terminal cells.

## SSH/Telnet pipe streaming (bypasses image rendering)

For terminal protocols (SSH, Telnet), there is an alternative path that bypasses
image encoding entirely. The server streams raw ANSI terminal output directly
to the client:

1. Server opens STDOUT pipe: `pipe,100,application/octet-stream,STDOUT`
2. Server sends raw terminal data: `blob,100,{base64_encoded_ansi_data}`
3. Client decodes base64 and writes directly to `sys.stdout.buffer`
4. Client sends ack after each blob: `ack,100,OK,0`

This gives perfect terminal fidelity -- colors, cursor movement, all ANSI
escape sequences pass through directly. No image decoding needed.

Enable with connection parameter: `enable-pipe=true`

The pipe infrastructure is in `guacr-handlers/src/pipe.rs` (Rust server-side)
and `instructions.py` lines 496-546 (Python client-side).

**When pipe is NOT available**, SSH and Telnet fall back to the same image
rendering path as RDP/VNC/Database: the server renders terminal text to JPEG
and sends it via `img` + `blob` + `end`. The client must then decode and render
those images using the tiers described above.

**Database protocols** have no pipe equivalent -- they always use the image
rendering path since the server renders query results into a visual terminal UI.

## Implementation plan for Commander

### Phase 1: Image stream reassembly (all protocols)

Add to `renderer.py` or new `image_renderer.py`:
- Track active image streams (stream_id -> {mimetype, x, y, chunks[]})
- On `img` instruction: open new stream, store metadata
- On `blob` instruction: if stream is an image stream, append chunk
- On `end` instruction: concatenate chunks, base64-decode, decode JPEG
- This handles all protocols uniformly -- RDP, VNC, SSH, Telnet, Database all
  send the same `img`/`blob`/`end` instruction sequence

### Phase 2: Half-block renderer (universal, do this first)

This gets 80% of the value since it works everywhere:
- Decode JPEG to pixel array (Pillow)
- Downsample to terminal cell grid using area-average (LANCZOS)
- For each pair of vertical pixels, emit half-block with truecolor fg/bg
- Use ANSI cursor positioning to place at correct (x, y) from img instruction
- Dirty rect: only redraw cells that changed
- Works identically for all protocols since input is always JPEG

### Phase 3: Kitty renderer

- Detect Kitty/WezTerm via env vars
- Wrap decoded JPEG bytes in Kitty escape sequences
- Use placement IDs for dirty rect replacement
- No re-encoding needed -- JPEG bytes pass through

### Phase 4: Sixel renderer

- Detect via terminal identification
- Decode JPEG, quantize to 256-color palette (Pillow.quantize())
- Encode as Sixel data with RLE compression
- Position via cursor movement before Sixel output

### Phase 5: Frame rate throttling

- Track time since last frame render
- If multiple img+end sequences arrive within frame period, skip intermediate frames
- Configurable FPS: 2-5 for graphical protocols (RDP/VNC), can go higher for
  terminal protocols (SSH/Telnet/Database) since their images are smaller

### Phase 6: Pipe streaming preference for terminal protocols

- When connecting to SSH or Telnet, prefer the pipe streaming path (`enable-pipe=true`)
- Pipe gives perfect terminal fidelity with zero image overhead
- Fall back to image rendering (Phases 1-5) when pipe is not available
- Database connections always use image rendering (no pipe equivalent)

## References

- [Kitty Graphics Protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [Sixel Graphics](https://en.wikipedia.org/wiki/Sixel)
- [iTerm2 Inline Images](https://iterm2.com/documentation-images.html)
- [Are We Sixel Yet?](https://www.arewesixelyet.com/) - Terminal Sixel support tracker
- Commander PR: KC-975 (commit 4aa0e4ad by idimov-keeper)
- KCM patches: 5001-KCM-505, 5002-KCM-505 (pipe stream support in guacd)
