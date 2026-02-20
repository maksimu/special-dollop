# Crates Overview

This document provides a high-level overview of all crates in the `keeper-pam-connections` monorepo.

## Core Crates

### keeper-pam-webrtc-rs

**Location:** `crates/keeper-pam-webrtc-rs/`
**Type:** Pure Rust library (rlib)
**Purpose:** WebRTC tunneling and secure connection management

Provides the core WebRTC abstraction ("Tube API") for secure tunneling with support for:
- Three conversation types: `tunnel` (raw TCP), `guacd` (Guacamole protocol), `socks5` (SOCKS5 proxy)
- Lock-free hot paths with SIMD optimization (398-2213ns frame processing)
- Actor-based registry with automatic backpressure
- RAII resource management for automatic cleanup
- Failure isolation via circuit breakers

**Key Features:**
- Sub-microsecond frame processing (700K-2.5M frames/sec)
- Trickle ICE for network recovery
- Multi-channel support with fragmentation
- Python bindings via PyO3 (abi3-py37+)

**Documentation:** `crates/keeper-pam-webrtc-rs/docs/` and `crates/keeper-pam-webrtc-rs/CLAUDE.md`

### python-bindings

**Location:** `crates/python-bindings/`
**Type:** Python extension module (cdylib)
**Purpose:** Unified Python package (`keeper_pam_connections`)

Aggregates all Rust crates into a single Python module:
- Calls `register_webrtc_module()` from keeper-pam-webrtc-rs
- Provides single import: `import keeper_pam_connections`
- All SDP must be base64-encoded across Python/Rust boundary

**Key APIs:**
- `PyTubeRegistry.create_tube()` - Create WebRTC connections
- `PyTubeRegistry.close_tube()` - Close connections
- Signal callbacks for WebRTC events

**Documentation:** `docs/ARCHITECTURE_EXPLANATION.md`, `crates/python-bindings/docs/PYTHON_API_CONTRACT.md`

## Protocol Handler Crates (guacr)

### guacr

**Location:** `crates/guacr/`
**Type:** Aggregator crate
**Purpose:** Feature-gated re-exports of all protocol handlers

Main entry point for protocol handlers with feature flags:
- Default features: `ssh`, `telnet`
- Optional features: `rdp`, `vnc`, `database`, `sftp`, `rbi`, `threat-detection`
- `all` feature enables everything

**Usage:**
```toml
guacr = { version = "2.0", features = ["ssh", "rdp"] }
```

**Documentation:** `crates/guacr/docs/`

### guacr-handlers

**Location:** `crates/guacr-handlers/`
**Type:** Foundation library
**Purpose:** Protocol handler trait and registry

Provides:
- `ProtocolHandler` trait definition
- Handler registry for dispatch
- Event callback system
- Signal event types

All protocol handlers implement the `ProtocolHandler` trait.

**Documentation:** `crates/guacr-handlers/README.md`

### guacr-protocol

**Location:** `crates/guacr-protocol/`
**Type:** Protocol codec
**Purpose:** Guacamole protocol encoding/decoding

Implements the text-based Guacamole protocol:
- Format: `LENGTH.ELEMENT,LENGTH.ELEMENT,...;`
- Example: `5.mouse,1.0,2.10,3.120;` = mouse at (10, 120)
- Used by all protocol handlers

**Documentation:** `crates/guacr/docs/concepts/GUACAMOLE_PROTOCOL.md`

### guacr-terminal

**Location:** `crates/guacr-terminal/`
**Type:** Shared library
**Purpose:** Terminal emulation infrastructure

Shared by SSH and Telnet handlers:
- VT100 parser
- PNG rendering engine (16ms debounce for 60fps)
- JPEG compression support
- Font rendering with UTF-8 support
- Scrollback buffer management

**Documentation:** `crates/guacr/docs/concepts/TERMINAL_GRAPHICS.md`

### guacr-ssh

**Location:** `crates/guacr-ssh/`
**Type:** Protocol handler
**Status:** Production-ready
**Purpose:** SSH protocol support

Features:
- Password and SSH key authentication (all formats)
- Bidirectional clipboard (OSC 52 + bracketed paste)
- JPEG rendering with 16ms debounce (60fps)
- Dirty region optimization with scroll detection
- Default: 1024x768 @ 96 DPI (resizes to browser)
- Optional threat detection integration

**Dependencies:** `russh` 0.45, `guacr-terminal`

**Documentation:** `crates/guacr/docs/crates/guacr-ssh.md`

### guacr-telnet

**Location:** `crates/guacr-telnet/`
**Type:** Protocol handler
**Status:** Complete
**Purpose:** Telnet protocol support

Features:
- Custom Telnet protocol implementation
- Full VT100 terminal emulation
- Scrollback support
- Mouse event handling
- Threat detection integration

**Dependencies:** `guacr-terminal`

**Documentation:** `crates/guacr/docs/`

### guacr-rdp

**Location:** `crates/guacr-rdp/`
**Type:** Protocol handler
**Status:** Production-ready
**Purpose:** RDP protocol support via IronRDP

Features:
- Pure Rust RDP client (IronRDP)
- 60 FPS rendering with dirty rectangle tracking
- 90-99% bandwidth reduction via scroll detection
- Dynamic resize support (reconnect method)
- Smart rendering: <30% = partial update, >30% = full update
- WebP compression for efficiency
- HiDPI support (96/144/192 DPI scaling)
- Optional SFTP file transfer integration

**Dependencies:** Custom IronRDP fork (feature/handle-update-pdu branch)

**Documentation:** `crates/guacr-rdp/README.md`, `crates/guacr-rdp/IRONRDP_STATUS.md`, `crates/guacr/docs/crates/guacr-rdp.md`

### guacr-vnc

**Location:** `crates/guacr-vnc/`
**Type:** Protocol handler
**Status:** Complete
**Purpose:** VNC protocol support

Features:
- RFB 3.8 protocol support
- Full VNC feature support
- Async-based implementation

**Documentation:** `crates/guacr/docs/crates/guacr-vnc.md`

### guacr-database

**Location:** `crates/guacr-database/`
**Type:** Protocol handler
**Status:** Working
**Purpose:** Database protocol support

Supported databases:
- MySQL
- PostgreSQL
- SQL Server (via tiberius)
- Oracle
- MongoDB
- Redis
- MariaDB

Features:
- Query execution interface
- Spreadsheet-like UI for results
- Terminal-based rendering for query results

**Dependencies:** `sqlx`, `tiberius`, `guacr-terminal`

**Documentation:** `crates/guacr/docs/`

### guacr-sftp

**Location:** `crates/guacr-sftp/`
**Type:** Protocol handler
**Status:** Complete
**Purpose:** SFTP file transfer

Features:
- SSH File Transfer Protocol implementation
- File browser UI
- Upload/download support
- Security-hardened
- Optional integration with RDP handler

**Documentation:** `crates/guacr/docs/`

### guacr-rbi

**Location:** `crates/guacr-rbi/`
**Type:** Protocol handler
**Status:** Complete (Chrome/CDP only)
**Purpose:** Remote Browser Isolation

Features:
- Chromium integration via Chrome DevTools Protocol (CDP)
- Headless Chrome support
- File download handling
- Sandbox isolation for browser threats
- Session isolation with dedicated profiles

**Note:** CEF implementation removed in v2.0 (see commit 7b64164)

**Requirements:** Chrome/Chromium installed at runtime

**Documentation:** `crates/guacr-rbi/README.md`

### guacr-threat-detection

**Location:** `crates/guacr-threat-detection/`
**Type:** Optional feature
**Purpose:** AI-powered security analysis

Features:
- Live threat detection during sessions
- Three modes: reactive, proactive, stateful
- Integration with SSH/Telnet handlers
- Terminal state tracking

**Documentation:** `crates/guacr/docs/concepts/THREAT_DETECTION.md`

## Crate Dependencies

```
keeper-pam-webrtc-rs
  ├─> guacr (with all protocol handlers)
  └─> pyo3 (Python bindings)

python-bindings
  └─> keeper-pam-webrtc-rs

guacr (aggregator)
  ├─> guacr-ssh
  ├─> guacr-telnet
  ├─> guacr-rdp
  ├─> guacr-vnc
  ├─> guacr-database
  ├─> guacr-sftp
  ├─> guacr-rbi
  └─> guacr-threat-detection

guacr-ssh
  ├─> guacr-handlers
  ├─> guacr-protocol
  ├─> guacr-terminal
  └─> russh

guacr-telnet
  ├─> guacr-handlers
  ├─> guacr-protocol
  └─> guacr-terminal

guacr-rdp
  ├─> guacr-handlers
  ├─> guacr-protocol
  └─> ironrdp (custom fork)

guacr-database
  ├─> guacr-handlers
  ├─> guacr-protocol
  ├─> guacr-terminal
  └─> sqlx / tiberius

guacr-rbi
  ├─> guacr-handlers
  ├─> guacr-protocol
  └─> chromiumoxide
```

## Build Targets

**Rust Library:**
```bash
cargo build --release -p keeper-pam-webrtc-rs
cargo build --workspace --release
```

**Python Package:**
```bash
cd crates/python-bindings
maturin build --release
```

**Feature Selection (guacr):**
```bash
# Minimal (SSH + Telnet only)
cargo build -p guacr

# With RDP and VNC
cargo build -p guacr --features "rdp,vnc"

# Everything
cargo build -p guacr --features "all"
```

## Testing

**Rust Tests:**
```bash
# All workspace tests
cargo test --workspace --lib

# Specific crate
cargo test -p keeper-pam-webrtc-rs
cargo test -p guacr-ssh
```

**Python Tests:**
```bash
cd crates/python-bindings/tests
python -m pytest -v
```

## Documentation Locations

- **Workspace docs:** `/docs/` (this directory)
- **WebRTC docs:** `/crates/keeper-pam-webrtc-rs/docs/` and `CLAUDE.md`
- **guacr docs:** `/crates/guacr/docs/` and `CLAUDE.md`
- **Python docs:** `/docs/ARCHITECTURE_EXPLANATION.md`

## Publishing

Each guacr crate can be published independently to crates.io:
```bash
cd crates/guacr-ssh
cargo publish
```

Or use the GitHub workflow: `.github/workflows/publish-crates.yml`

## Performance Characteristics

**keeper-pam-webrtc-rs:**
- Frame processing: 398-2213ns (700K-2.5M frames/sec)
- Max concurrent tubes: 100 (configurable)
- Queue depth read: ~1ns (atomic)

**Protocol Handlers:**
- SSH/Telnet: 60 FPS terminal rendering
- RDP: 60 FPS with 90-99% bandwidth reduction
- VNC: Full framebuffer support
- All: <100ms input latency

See `crates/keeper-pam-webrtc-rs/docs/PERFORMANCE_BENCHMARKS.md` for detailed benchmarks.
