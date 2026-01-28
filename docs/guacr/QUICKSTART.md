# Quick Start Guide

**pam-guacr** is a Rust-based protocol handler library for remote desktop access, designed to work with WebRTC-based secure tunneling systems.

## What This Is

Protocol handlers that translate various remote access protocols into the Guacamole protocol for browser-based access:
- **SSH, Telnet** - Terminal protocols with JPEG rendering (60fps)
- **RDP, VNC** - Graphical protocols with hardware encoding support
- **Database** - MySQL, PostgreSQL, SQL Server, Oracle, MongoDB, Redis
- **SFTP** - File transfer with browser-based UI
- **RBI** - Remote Browser Isolation with Chromium

## Installation

```bash
# Clone repository
git clone https://github.com/your-org/pam-guacr
cd pam-guacr

# Build all crates
cargo build --release --workspace

# Run tests
cargo test --workspace
```

## Quick Commands

```bash
# Build specific protocol handler
cargo build -p guacr-ssh
cargo build -p guacr-rdp

# Run integration tests (requires Docker)
make docker-up
make test-ssh
make test-rdp

# Linting
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all
```

## Current Status

### Production Ready
- ✅ **SSH** - Password/key auth, JPEG rendering, clipboard, recording, threat detection
- ✅ **RDP** - 60fps, 90-99% bandwidth reduction, dynamic resize
- ✅ **VNC** - RFB 3.8 protocol, full feature support

### Complete
- ✅ **Telnet** - Scrollback, mouse events, threat detection
- ✅ **Database** - MySQL query execution, spreadsheet UI
- ✅ **SFTP** - File browser, upload/download
- ✅ **RBI** - Chromium integration, file downloads

## Basic Usage

```rust
use guacr::{ProtocolHandlerRegistry, ProtocolHandler};
use std::collections::HashMap;
use bytes::Bytes;
use tokio::sync::mpsc;

// Create registry
let registry = ProtocolHandlerRegistry::new();

// Register handlers
registry.register(guacr::ssh::SshHandler::with_defaults());
registry.register(guacr::rdp::RdpHandler::with_defaults());

// Get handler
let handler = registry.get("ssh").expect("SSH handler not found");

// Setup channels
let (to_client, mut from_handler) = mpsc::channel::<Bytes>(128);
let (to_handler, from_client) = mpsc::channel::<Bytes>(128);

// Connection parameters
let mut params = HashMap::new();
params.insert("hostname".to_string(), "example.com".to_string());
params.insert("port".to_string(), "22".to_string());
params.insert("username".to_string(), "user".to_string());
params.insert("password".to_string(), "pass".to_string());

// Connect
handler.connect(params, to_client, from_client).await?;
```

## Architecture

```
Browser (TypeScript) - Guacamole protocol rendering
    ↕ WebRTC data channels
Python Gateway - Session management
    ↕ PyO3 bindings
Rust Backend (guacr-*) - Protocol handlers
    ↕ Native protocol (SSH/RDP/VNC/etc)
Remote Servers
```

### Key Design Patterns
1. **Zero-Copy I/O** - `bytes::Bytes` for reference-counted buffers
2. **Lock-Free Concurrency** - `ArrayQueue` instead of `Mutex<Vec<T>>`
3. **Async/Await** - Tokio runtime throughout
4. **RAII Resource Management** - Automatic cleanup via Drop

## Project Structure

```
crates/
├── guacr-handlers/     # Handler registry and traits
├── guacr-protocol/     # Guacamole protocol codec
├── guacr-terminal/     # Terminal emulation (shared)
├── guacr-ssh/          # SSH handler
├── guacr-telnet/       # Telnet handler
├── guacr-rdp/          # RDP handler
├── guacr-vnc/          # VNC handler
├── guacr-database/     # Database handlers
├── guacr-sftp/         # SFTP handler
├── guacr-rbi/          # Remote Browser Isolation
└── guacr-threat-detection/ # AI threat detection
```

## Performance Targets

- **Latency**: <100ms for input events
- **Frame Rate**: 60 FPS for graphical protocols
- **Connections**: 10,000+ concurrent per instance
- **Memory**: <10MB per connection
- **Bandwidth**: 90-99% reduction via optimizations

## Next Steps

1. **Read Architecture** - See [ARCHITECTURE.md](ARCHITECTURE.md) for system design
2. **Protocol Details** - See [docs/crates/](docs/crates/) for per-protocol documentation
3. **Testing** - See [docs/guides/TESTING.md](docs/guides/TESTING.md) for testing procedures
4. **Integration** - See [docs/guides/INTEGRATION.md](docs/guides/INTEGRATION.md) for integration patterns

## Key Features

### Session Recording
- Zero-copy, lock-free recording
- Multiple transports: files, S3, channels
- Asciicast and Guacamole .ses formats
- See [docs/concepts/RECORDING.md](docs/concepts/RECORDING.md)

### AI Threat Detection
- Live analysis of keyboard input and terminal output
- Three modes: reactive, proactive, stateful
- BAML REST API integration
- See [docs/concepts/THREAT_DETECTION.md](docs/concepts/THREAT_DETECTION.md)

### Zero-Copy Architecture
- Buffer pooling for zero allocations
- SIMD acceleration for pixel operations
- io_uring support for kernel bypass
- See [docs/concepts/ZERO_COPY.md](docs/concepts/ZERO_COPY.md)

## Getting Help

- **Documentation**: Browse [docs/](docs/) directory
- **Issues**: Check existing issues or create new one
- **Contributing**: See [CONTRIBUTING.md](CONTRIBUTING.md)
- **Testing**: See [docs/guides/TESTING.md](docs/guides/TESTING.md)

## Common Issues

### Build Errors

**RDP handler fails to build:**
```bash
# Install FreeRDP dependencies (if using FFI)
# Ubuntu/Debian
apt install freerdp2-dev libwinpr2-dev

# macOS
brew install freerdp
```

**Tests fail to connect:**
```bash
# Start Docker test containers
make docker-up

# Check containers are running
docker ps
```

### Performance Issues

**High CPU usage:**
- Enable hardware encoding (if available)
- Increase buffer pool size
- Use binary protocol instead of text

**High memory usage:**
- Reduce buffer pool size
- Lower channel capacity
- Check for connection leaks

See [docs/guides/PERFORMANCE.md](docs/guides/PERFORMANCE.md) for detailed tuning.
