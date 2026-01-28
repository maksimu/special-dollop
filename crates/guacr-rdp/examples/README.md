# Terminal RDP Examples

Examples demonstrating terminal-based RDP rendering using Sixel graphics.

## What's Implemented

### ✅ Sixel Graphics
- Widely compatible bitmap graphics protocol
- Supported by: xterm, mlterm, mintty, iTerm2, WezTerm, foot, Contour, konsole
- 256-color palette with run-length encoding
- Configurable resolution (default 640×480)

## Running the Examples

### Test Pattern Demo

```bash
# Display a test pattern using Sixel graphics
cargo run -p guacr-rdp --example terminal_rdp
```

**Supported terminals:**
- [iTerm2](https://iterm2.com/) (macOS) - Excellent support
- [WezTerm](https://wezfurlong.org/wezterm/) - Full support
- [xterm](https://invisible-island.net/xterm/) - Full support (use `xterm -ti vt340`)
- [mlterm](https://mlterm.sourceforge.net/) - Full support
- [mintty](https://mintty.github.io/) (Windows) - Full support
- [foot](https://codeberg.org/dnkl/foot) (Wayland) - Full support
- [Contour](https://github.com/contour-terminal/contour) - Full support

### Connect to Real RDP Server

```bash
cargo run -p guacr-rdp --example rdp_terminal_mode -- \
    --host <rdp-server-ip> \
    --username <user> \
    --password <pass>
```

## What You'll See

```
Using Sixel graphics mode

Terminal size: 80×24
Effective resolution: 640×480 pixels

Rendering test pattern...

[Full resolution bitmap image displayed inline using Sixel protocol]
```

## Expected Output

The example generates a test pattern:
- **Gradient** - Color gradient from cyan to yellow
- **White rectangle** - Simulates text/UI elements

This demonstrates:
- Color rendering
- Downsampling quality
- Text-like pattern visibility

## Terminal Compatibility

| Terminal | Sixel Support | Notes |
|----------|---------------|-------|
| iTerm2 (macOS) | ✅ Excellent | Best quality on macOS |
| WezTerm | ✅ Full | Cross-platform |
| xterm | ✅ Full | Use `xterm -ti vt340` |
| mlterm | ✅ Full | Lightweight |
| mintty (Windows) | ✅ Full | Git Bash, Cygwin |
| foot (Wayland) | ✅ Full | Modern Wayland |
| Contour | ✅ Full | GPU-accelerated |
| Alacritty | ❌ | No Sixel support |
| Terminal.app | ❌ | No Sixel support |
| konsole | ✅ Full | KDE terminal |

## Next Steps

To use with actual RDP:

1. **Integrate with RDP handler** - Hook into graphics update handler
2. **Add configuration** - Add terminal mode to `RdpConfig`
3. **Test with real RDP** - Connect to Windows/Linux RDP server
4. **Optimize performance** - Add dirty region tracking

See the RDP handler integration in `crates/guacr-rdp/src/handler.rs` for implementation details.

## Troubleshooting

### "No image displayed" in Sixel mode

**Problem:** You see escape codes instead of images

**Solution:** 
- Make sure you're running in a Sixel-compatible terminal (iTerm2, WezTerm, xterm, mlterm)
- For xterm, use: `xterm -ti vt340`
- Check terminal documentation for Sixel support

### "Low resolution" with Sixel

**Problem:** Image looks pixelated

**Solution:**
- This is a test pattern at 640×480
- Real RDP would use higher resolution
- Sixel supports arbitrary resolution - limited only by terminal size

## Performance Notes

**Frame generation time:**
- Sixel: ~15-25ms (color quantization + encoding)

**Output size:**
- Sixel: ~50-150KB per frame (run-length encoded, 256 colors)

**For real RDP:**
- Use dirty region tracking (only update changed areas)
- Target 15-30 FPS for terminal rendering
- Consider lower RDP resolution (1280×720 vs 1920×1080)

## References

- [Sixel Graphics](https://en.wikipedia.org/wiki/Sixel) - Original DEC VT340 protocol
- [Sixel Support in Terminals](https://www.arewesixelyet.com/) - Terminal compatibility
- [Terminal Graphics Docs](../../../docs/concepts/TERMINAL_GRAPHICS.md) - Implementation details
