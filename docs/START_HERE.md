# keeper-pam-webrtc-rs - Quick Start

**Branch:** `feat/graphical-handlers`
**Last Updated:** 2025-11-17

## What This Is

Rust-based WebRTC tunneling library with **native protocol handlers** for secure remote access:
- **Core:** WebRTC tubes for secure tunneling (Python bindings via PyO3)
- **Handlers:** 13+ protocols in pure Rust (SSH, Telnet, RDP, VNC, 7 databases, SFTP, RBI, K8s/Docker)
- **Integration:** Replaces Apache Guacamole's C daemon with memory-safe Rust

## Current Status

✅ **SSH Handler Production Ready**
- Password & SSH key authentication (all formats: OpenSSH, PEM, PKCS#8)
- JPEG rendering (5-10x faster than PNG)
- 16ms debounce (60fps smoothness)
- 1024x768 size cap (prevents WebRTC backpressure)
- Bidirectional clipboard (OSC 52 + bracketed paste)
- Dirty region optimization with scroll detection
- All input responsive (no lag)

### Architecture - JPEG + Font Rendering

**Approach:** JPEG encoding with fontdue (5-10x faster than PNG)
- ✅ Dirty region tracking (like guacd)
- ✅ Scroll optimization (copy + render only new lines)
- ✅ Size cap (1024x768 to prevent backpressure)
- ✅ 16ms debounce (60fps updates)
- ✅ Font rendering (Noto Sans Mono)
- ✅ JPEG quality 95 (fast with minimal loss)

### What Works
- Handler invocation and lifecycle
- SSH authentication (password + SSH keys with passphrase)
- Terminal emulation (vt100 crate)
- Keyboard input (x11_keysym_to_bytes)
- Clipboard paste (bracketed paste mode)
- Clipboard copy (OSC 52 detection)
- Terminal resize (window_change)
- Session recording (asciicast)
- Disconnect detection
- Network-driven rendering
- Fast JPEG encoding

### Recently Fixed

1. **Performance lag fixed** - Changed PNG to JPEG (5-10x faster)
2. **Size cap added** - 1024x768 max to prevent WebRTC backpressure (~60KB → ~25KB images)
3. **Debounce optimized** - Reduced from 100ms to 16ms for 60fps smoothness
4. **SSH key auth** - All formats supported (OpenSSH, PEM, PKCS#8) with passphrases
5. **Clipboard integration** - Bidirectional copy/paste with OSC 52 + bracketed paste
6. **Logging cleanup** - Trace/debug for hot paths, info/error for user events

## Quick Commands

```bash
# Build with handlers
cargo build --release --features handlers

# Run tests
cargo test --no-default-features  # Rust only (235 tests)
python3 -m pytest tests/          # Python integration

# Build Python wheel (for Gateway)
maturin build --release -m crates/keeper-webrtc/Cargo.toml --features handlers

# Build Linux wheel (in Docker)
docker run --rm -v $(pwd):/io konstin2/maturin build --release \
  -m crates/keeper-webrtc/Cargo.toml --features handlers

# Linting
cargo clippy --features handlers -- -D warnings
```

## Architecture

```
Browser (TypeScript) - Guacamole protocol rendering
    ↕ WebRTC data channels
Python Gateway - Session management
    ↕ PyO3 bindings
Rust Backend (keeper-webrtc + guacr-*) - Protocol handlers
    ↕ Native protocol (SSH/RDP/etc)
Remote Servers
```

### Key Design Patterns
1. **Actor Model** - Registry coordination without locks
2. **Lock-Free Hot Paths** - DashMap, atomic counters (398-2213ns frame processing)
3. **RAII Resource Management** - Automatic cleanup via Drop
4. **Failure Isolation** - Per-tube WebRTC isolation

## Project Structure

### Core Crates
- `crates/keeper-webrtc/` - WebRTC core, channel management, Python bindings
- `crates/guacr-handlers/` - Handler registry and traits
- `crates/guacr-terminal/` - Terminal emulation (vt100, PNG rendering, fonts)

### Protocol Handlers
- `crates/guacr-ssh/` - SSH (russh)
- `crates/guacr-telnet/` - Telnet
- `crates/guacr-rdp/` - RDP (framebuffer)
- `crates/guacr-vnc/` - VNC
- `crates/guacr-sftp/` - SFTP file browser
- `crates/guacr-database/` - MySQL, PostgreSQL, SQL Server, Oracle, MongoDB, Redis, spreadsheet UI
- `crates/guacr-rbi/` - Remote Browser Isolation

### Documentation
- `CLAUDE.md` - Developer guidelines and architecture overview
- `docs/` - Detailed architecture and design docs
- `docs/guacr/` - Protocol handler design docs

## TODO (Prioritized)

### Phase 1: SSH Handler Polish
- [x] ~~Private key authentication~~ (DONE: All formats supported)
- [x] ~~Clipboard integration~~ (DONE: OSC 52 + bracketed paste)
- [x] ~~Better logging~~ (DONE: Trace/debug for hot paths)
- [ ] Send control + chars through like ctl+c  
- [ ] Scrollback buffer (~150 lines)
- [ ] Mouse events for vim/tmux (~80 lines)
- [ ] Host key verification (~80 lines)

### Phase 2: File Transfer & Advanced
- [ ] SFTP file transfer integration (~500 lines)
- [ ] Custom glyph protocol (7-25× bandwidth savings)
- [ ] AI threat detection integration
- [ ] Session termination on threat detection

### Phase 3: Other Protocols
- [ ] RDP: Complete ironrdp integration
- [ ] VNC: Add VNC client calls
- [ ] Databases: Wire up actual query execution
- [ ] Telnet: Similar improvements to SSH
- [ ] RBI: Remote Browser Isolation

### Phase 4: New Protocol - Container Management (K8s/Docker)
- [ ] Protocol handler for Kubernetes/Docker exec sessions
- [ ] Container list view (spreadsheet-like, similar to database handlers)
- [ ] Container logs streaming
- [ ] Pod/container shell access
- [ ] File copy to/from containers
- [ ] Resource metrics display (CPU, memory)
- [ ] Multi-container view support

## Key Files

**Essential:**
- `crates/keeper-webrtc/src/channel/core.rs` - Channel management
- `crates/keeper-webrtc/src/channel/handler_connections.rs` - Handler invocation
- `crates/keeper-webrtc/src/channel/protocol.rs` - Protocol routing
- `crates/guacr-terminal/src/renderer.rs` - Terminal rendering with fonts
- `crates/guacr-ssh/src/handler.rs` - SSH handler implementation

**Documentation:**
- `docs/development/RENDERING_METHODS.md` - How guacd uses PNG+fonts (NOT colored blocks!)
- `docs/development/GUACD_FEATURE_COMPARISON.md` - What we have vs guacd (clipboard, SFTP, etc.)
- `docs/development/CUSTOM_GLYPH_PROTOCOL.md` - Custom glyph caching for 7-25× bandwidth savings (future)
- `docs/development/PROTOCOL_CONVERSION.md` - Guacamole protocol details
- `docs/ACTOR_DASHMAP_RAII.md` - Registry architecture
- `docs/HOT_PATH_OPTIMIZATION_SUMMARY.md` - Performance details

## Performance Targets

- Frame processing: 398-2213ns (2.5M-700K frames/sec)
- UTF-8 character counting: 371-630ns per instruction
- Memory per tube: ~52KB
- Concurrent tube creates: 100 (configurable)

## Getting Help

- Read `CLAUDE.md` for detailed architecture and patterns
- Check `docs/` for specific subsystem deep-dives
- Run tests with `--nocapture` for debug output
- Enable debug logging: `RUST_LOG=debug cargo test`

---

**Remember:** Read `CLAUDE.md` for the complete architecture overview and development guidelines.
