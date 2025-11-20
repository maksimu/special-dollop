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
- ✅ Control character handling (Ctrl+C, etc.)
- ✅ Scrollback buffer (~150 lines)
- ✅ Mouse events for vim/tmux (X11 mouse sequences)
- ✅ Host key verification (known_hosts)
- ✅ Session recording (Asciicast + Guacamole .ses formats)
- ✅ Recording transports (Files, S3, ZeroMQ, channels)
- ✅ AI threat detection with BAML REST API
- ✅ Automatic session termination on threats

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

### Phase 1: Container Management Protocol (K8s/Docker)
- [ ] Protocol handler for Kubernetes/Docker exec sessions
- [ ] Container list view (spreadsheet-like, similar to database handlers)
- [ ] Container logs streaming
- [ ] Pod/container shell access
- [ ] File copy to/from containers
- [ ] Resource metrics display (CPU, memory)
- [ ] Multi-container view support

### Phase 2: Performance & Completeness

#### Hardware Encoder Implementations (Medium Priority)

**What**: Implement actual hardware encoders (NVENC, QuickSync, VideoToolbox, VAAPI) instead of just framework.

**Why**: Critical for 4K video performance. Currently falls back to JPEG.

**Effort**: 2-3 days per encoder (platform-specific)

**Status**: Framework ready, needs actual implementations

**Important Notes**: 
- Hardware encoders require client-side video playback support
- Guacamole protocol supports `video` instruction (H.264 streaming)
- Vault client needs `VideoPlayer` implementation (currently returns empty array)
- Current JPEG via `img` works fine, but H.264 would be 2-3x more bandwidth efficient

**Build Configuration for Python Wheels**:

When building Python wheels (keeper-pam-webrtc-rs), use:
```toml
guacr = { 
    path = "../pam-guacr/crates/guacr", 
    features = ["ssh", "telnet", "rdp", "vnc", "sftp"],
    optional = true 
}
```

**Do NOT enable hardware encoding features** (`nvenc`, `quicksync`, `videotoolbox`, `vaapi`) because:
- ❌ They don't work in Docker build containers (no GPU access)
- ❌ nvenc, quicksync, vaapi are placeholders that don't actually work yet
- ❌ Build would require CUDA toolkit, Intel Media SDK, libva libraries installed
- ✅ Software JPEG encoding works everywhere and is 5-10x faster than PNG

**Hardware encoder code is kept in pam-guacr for future implementation**, but don't enable those features when building Python wheels.

**Platform-specific builds** (future):
- Linux wheel: Build in Docker without hardware features
- macOS wheel: Could enable `videotoolbox` feature (only one that works)
- Single wheel with all features: Not possible until placeholder encoders become real implementations

#### RDPGFX Channel Integration (Medium Priority)

**What**: Full RDPGFX channel support for hardware-accelerated graphics.

**Why**: Better performance for 4K video streaming.

**Effort**: 1-2 days

**Status**: Framework ready

#### VNC Encoding Types (Low Priority)

**What**: Implement CopyRect, Hextile, ZRLE, Tight encodings.

**Why**: Better bandwidth efficiency for VNC.

**Effort**: 1 day per encoding type

**Status**: CopyRect framework ready

#### Database Query Execution (Low Priority)

**What**: Wire up PostgreSQL, SQL Server, Oracle, MongoDB, Redis query execution (similar to MySQL).

**Why**: Complete database handler support.

**Effort**: 2-3 hours per database (reuse MySQL pattern)

**Status**: MySQL complete, others pending

### Phase 3: UI-Dependent Features

#### Custom Glyph Protocol

**What**: Custom glyph caching for 7-25× bandwidth savings

**Why**: Significant bandwidth optimization for terminal protocols

**Status**: Requires UI updates

## Completed Phases

### ✅ Phase 1: SSH Handler Polish - COMPLETE
- ✅ Private key authentication (all formats supported)
- ✅ Clipboard integration (OSC 52 + bracketed paste)
- ✅ Better logging (trace/debug for hot paths)
- ✅ Control character handling (Ctrl+C, etc.)
- ✅ Scrollback buffer (~150 lines)
- ✅ Mouse events for vim/tmux (X11 mouse sequences)
- ✅ Host key verification (known_hosts)
- ✅ Session recording (Asciicast + Guacamole .ses formats)
- ✅ Recording transports (Files, S3, ZeroMQ, channels)

### ✅ Phase 2: File Transfer & Advanced - COMPLETE
- ✅ Telnet improvements (scrollback, mouse events, threat detection, dirty tracking)
- ✅ SFTP file transfer integration
  - ✅ Handler structure and security (chroot, path validation)
  - ✅ File browser rendering
  - ✅ Guacamole protocol integration (file upload/download)
  - ✅ Mouse click handling for navigation
  - ✅ Complete russh-sftp API integration
- ✅ AI threat detection integration (BAML REST API)
  - ✅ SSH/Telnet handler integration
  - ✅ Configurable threat levels and auto-termination
  - ✅ Tag-based rules (deny/allow regex patterns)

### ✅ Phase 3: Other Protocols - COMPLETE
- ✅ RDP: Complete ironrdp integration with hardware encoding framework
- ✅ VNC: Full VNC protocol implementation
- ✅ Databases: MySQL query execution wired up
- ✅ RBI: chromiumoxide integration with file download support

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
