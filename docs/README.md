# pam-guacr Documentation

Protocol handlers for remote desktop access (SSH, RDP, VNC, Telnet, Database protocols, SFTP, RBI) in Rust.

## Getting Started

- **[Quick Start](QUICKSTART.md)** - Get up and running in 5 minutes
- **[Architecture](ARCHITECTURE.md)** - System design and core concepts
- **[Contributing](CONTRIBUTING.md)** - How to contribute to the project
- **[Testing](guides/TESTING.md)** - Testing procedures and guidelines

## Protocol Handlers

Each protocol handler has its own documentation:

### Terminal Protocols
- **[guacr-ssh](crates/guacr-ssh.md)** - SSH handler (production-ready)
  - Password/key auth, JPEG rendering, clipboard, recording, threat detection
- **[guacr-telnet](crates/guacr-telnet.md)** - Telnet handler
  - Scrollback, mouse events, threat detection

### Graphical Protocols
- **[guacr-rdp](crates/guacr-rdp.md)** - RDP handler (production-ready)
  - 60fps, 90-99% bandwidth reduction, dynamic resize
- **[guacr-vnc](crates/guacr-vnc.md)** - VNC handler (production-ready)
  - RFB 3.8 protocol, full feature support

### Data Access
- **[guacr-database](crates/guacr-database.md)** - Database handlers
  - MySQL, PostgreSQL, SQL Server, Oracle, MongoDB, Redis
- **[guacr-sftp](crates/guacr-sftp.md)** - SFTP file transfer
  - File browser, upload/download, security

### Advanced
- **[guacr-rbi](crates/guacr-rbi.md)** - Remote Browser Isolation
  - Chromium integration, file downloads
- **[guacr-threat-detection](crates/guacr-threat-detection.md)** - AI threat detection
  - Live analysis, three modes (reactive, proactive, stateful)

## Core Concepts

Deep dives into key architectural concepts:

- **[Recording](concepts/RECORDING.md)** - Zero-copy session recording
  - Architecture, transports, task lifecycle, guarantees
- **[Threat Detection](concepts/THREAT_DETECTION.md)** - AI-powered security
  - Live analysis, BAML integration, three modes
- **[Zero-Copy](concepts/ZERO_COPY.md)** - Performance optimization
  - Buffer pooling, SIMD, io_uring
- **[Guacamole Protocol](concepts/GUACAMOLE_PROTOCOL.md)** - Protocol coverage
  - Instruction set, encoding, binary protocol

## Guides

How-to guides for common tasks:

- **[Integration](guides/INTEGRATION.md)** - Integration patterns
  - keeper-pam-webrtc-rs, Python bindings, WebRTC
- **[Testing](guides/TESTING.md)** - Testing procedures
  - Unit tests, integration tests, Docker setup
- **[Deployment](guides/DEPLOYMENT.md)** - Docker/Kubernetes deployment
  - Container images, orchestration, scaling
- **[Performance](guides/PERFORMANCE.md)** - Performance tuning
  - Optimization techniques, benchmarking, profiling

## Reference

### Original guacd Implementation
- **[reference/original-guacd/](reference/original-guacd/)** - Original C implementation reference
  - Architecture deep dive, key files, database plugins, RBI implementation

### Specifications
- **[Binary Protocol Spec](BINARY_PROTOCOL_SPEC.md)** - Binary protocol format
- **[Container Management Protocol](CONTAINER_MANAGEMENT_PROTOCOL.md)** - K8s/Docker protocol design

## Project Status

### Production Ready
- ✅ SSH - Complete with all features
- ✅ RDP - 60fps with optimizations
- ✅ VNC - Full RFB 3.8 support

### Complete
- ✅ Telnet - All features implemented
- ✅ Database - MySQL working, others pending
- ✅ SFTP - File transfer complete
- ✅ RBI - Chromium integration complete

### Planned
- 📋 Container Management - K8s/Docker protocol (design complete)
- 📋 Hardware Encoders - NVENC, QuickSync, etc. (framework ready)

## Architecture Overview

```
Browser (TypeScript)
    ↕ WebRTC data channels
Python Gateway
    ↕ PyO3 bindings
Rust Backend (guacr-*)
    ↕ Native protocols
Remote Servers
```

### Key Design Principles
1. **Zero-Copy I/O** - `bytes::Bytes` for reference-counted buffers
2. **Lock-Free Concurrency** - `ArrayQueue` instead of `Mutex<Vec<T>>`
3. **Async/Await** - Tokio runtime throughout
4. **Memory Safety** - Rust type system prevents common bugs

### Performance Targets
- **Latency**: <100ms for input events
- **Frame Rate**: 60 FPS for graphical protocols
- **Connections**: 10,000+ concurrent per instance
- **Memory**: <10MB per connection

## Documentation Structure

```
docs/
├── README.md                    # This file
├── QUICKSTART.md                # Getting started
├── ARCHITECTURE.md              # System design
├── CONTRIBUTING.md              # Contribution guidelines
│
├── crates/                      # Per-crate documentation
│   ├── guacr-ssh.md
│   ├── guacr-rdp.md
│   ├── guacr-vnc.md
│   ├── guacr-telnet.md
│   ├── guacr-database.md
│   ├── guacr-sftp.md
│   ├── guacr-rbi.md
│   ├── guacr-threat-detection.md
│   └── guacr-terminal.md
│
├── concepts/                    # Core concepts
│   ├── RECORDING.md
│   ├── THREAT_DETECTION.md
│   ├── ZERO_COPY.md
│   └── GUACAMOLE_PROTOCOL.md
│
├── guides/                      # How-to guides
│   ├── INTEGRATION.md
│   ├── TESTING.md
│   ├── DEPLOYMENT.md
│   └── PERFORMANCE.md
│
└── reference/                   # Reference documentation
    └── original-guacd/          # Original C implementation
```

## Quick Links

### For Users
- [Quick Start](QUICKSTART.md) - Get started quickly
- [SSH Handler](crates/guacr-ssh.md) - Most commonly used
- [Testing Guide](guides/TESTING.md) - Run tests

### For Developers
- [Architecture](ARCHITECTURE.md) - Understand the design
- [Contributing](CONTRIBUTING.md) - Contribute code
- [Zero-Copy](concepts/ZERO_COPY.md) - Performance techniques

### For Integrators
- [Integration Guide](guides/INTEGRATION.md) - Integrate with your system
- [Recording](concepts/RECORDING.md) - Session recording
- [Threat Detection](concepts/THREAT_DETECTION.md) - Security features

## Getting Help

- **Documentation**: Browse this directory
- **Issues**: Check GitHub issues or create new one
- **Testing**: See [guides/TESTING.md](guides/TESTING.md)
- **Contributing**: See [CONTRIBUTING.md](CONTRIBUTING.md)

## License

MIT
