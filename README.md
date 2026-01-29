# keeper-pam-connections

Keeper PAM connection management monorepo - unified Python package for secure connection management.

## Overview

This workspace provides a unified Python package (`keeper-pam-connections`) that aggregates multiple Rust-based connection management libraries. The architecture allows for easy extension with new protocol-specific crates while maintaining a single, cohesive Python API.

## Architecture

```
keeper-pam-connections/
├── crates/
│   ├── keeper-pam-webrtc-rs/     # Core WebRTC tunneling library
│   ├── python-bindings/           # Unified Python package (aggregates all crates)
│   ├── guacr/                     # Protocol handlers (aggregator)
│   ├── guacr-handlers/            # Handler trait definitions
│   ├── guacr-protocol/            # Guacamole protocol codec
│   ├── guacr-terminal/            # Terminal emulation
│   ├── guacr-ssh/                 # SSH protocol handler
│   ├── guacr-telnet/              # Telnet protocol handler
│   ├── guacr-rdp/                 # RDP protocol handler
│   ├── guacr-vnc/                 # VNC protocol handler
│   ├── guacr-database/            # Database protocol handlers
│   ├── guacr-sftp/                # SFTP file transfer
│   ├── guacr-rbi/                 # Remote Browser Isolation
│   └── guacr-threat-detection/    # AI threat detection
```

## Distribution Channels

This monorepo supports multiple distribution channels for different use cases:

### For Python Users
```bash
pip install keeper-pam-connections
```

```python
import keeper_pam_connections

# Access WebRTC + protocol handlers
registry = keeper_pam_connections.PyTubeRegistry()
```

### For Rust Users - Protocol Handlers Only
```toml
[dependencies]
guacr = { version = "1.1", features = ["ssh", "rdp", "vnc"] }
```

### For Rust Users - WebRTC Tunneling
```toml
[dependencies]
keeper-pam-webrtc-rs = "2.0"
guacr = "1.1"  # Optional, for built-in protocol handlers
```

## Crates

### keeper-pam-webrtc-rs
Core WebRTC-based secure tunneling library providing:
- Secure WebRTC data channel management
- Multi-protocol support (TCP tunnel, Guacamole, SOCKS5)
- High-performance frame processing (700K-2.5M frames/sec)
- Enterprise-grade stability and failure isolation

**Documentation**:
- [Crate README](crates/keeper-pam-webrtc-rs/README.md.original)
- [Architecture Docs](docs/) (workspace-level)
- [Protocol Docs](crates/keeper-pam-webrtc-rs/docs/) (crate-specific)

### guacr (Protocol Handlers)
Protocol handlers for remote desktop access:
- **SSH** - Secure Shell (production-ready)
- **Telnet** - Telnet protocol
- **RDP** - Remote Desktop Protocol (production-ready with IronRDP)
- **VNC** - Virtual Network Computing
- **Database** - MySQL, PostgreSQL, MongoDB, Redis, Oracle, SQL Server, MariaDB
- **SFTP** - SSH File Transfer Protocol
- **RBI** - Remote Browser Isolation (Chrome/CDP)

**Documentation**:
- [guacr README](docs/guacr/README.md)
- [Protocol Documentation](docs/guacr/)

### python-bindings
Unified Python package that exposes all functionality through a single module.
Combines WebRTC tunneling with protocol handlers for a complete solution.

## Installation

### From PyPI
```bash
pip install keeper-pam-connections
```

### From Source
```bash
# Build all crates
cargo build --release

# Build Python package
cd crates/python-bindings
maturin develop --release
```

## Development

### Build All Crates
```bash
cargo build --release
```

### Run All Tests
```bash
# Rust tests
cargo test --release

# Python tests
cd crates/python-bindings
python -m pytest tests/ -v
```

### Add New Protocol Crate

1. Create new crate in `crates/`:
```bash
cargo new --lib crates/my-protocol-rs
```

2. Add to workspace in root `Cargo.toml`:
```toml
[workspace]
members = [
    "crates/keeper-pam-webrtc-rs",
    "crates/python-bindings",
    "crates/my-protocol-rs"  # Add here
]
```

3. Implement Python registration function in your crate:
```rust
// crates/my-protocol-rs/src/python/mod.rs
pub fn register_my_protocol_module(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<MyProtocolClass>()?;
    // ... register your functionality
    Ok(())
}
```

4. Add dependency and registration in `crates/python-bindings/src/lib.rs`:
```rust
#[pymodule]
fn keeper_pam_connections(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    keeper_pam_webrtc_rs::python::register_webrtc_module(py, m)?;
    my_protocol_rs::python::register_my_protocol_module(py, m)?;  // Add here
    Ok(())
}
```

5. Rebuild:
```bash
cd crates/python-bindings
maturin develop --release
```

Now your new functionality is available:
```python
import keeper_pam_connections
# Use both WebRTC and your new protocol
```

## Project Structure

- **Root**: Workspace configuration, shared dependencies
- **crates/keeper-pam-webrtc-rs/**: Core WebRTC library (pure Rust)
- **crates/python-bindings/**: Unified Python package (cdylib)
- **.github/workflows/**: CI/CD pipelines
- **build_*.sh**: Build scripts for various platforms

## Key Features

- **Unified Python API**: Single import for all functionality
- **Extensible Architecture**: Easy to add new protocol crates
- **High Performance**: Sub-microsecond frame processing
- **Enterprise Stability**: Lock-free architecture, RAII resource management
- **Cross-Platform**: Linux, macOS, Windows, Alpine support
- **Memory Safe**: Rust-powered with comprehensive bounds checking

## Documentation

### 📚 Quick Start
- **Python Users**: [Python API Contract](crates/python-bindings/docs/PYTHON_API_CONTRACT.md)
- **Architecture Overview**: [Architecture Explanation](docs/ARCHITECTURE_EXPLANATION.md)
- **Testing Guide**: [Testing Strategy](docs/TESTING_STRATEGY.md)

### 🏗️ Architecture & Design
- [Actor + DashMap + RAII](crates/keeper-pam-webrtc-rs/docs/ACTOR_DASHMAP_RAII.md) - Registry concurrency architecture
- [Failure Isolation](crates/keeper-pam-webrtc-rs/docs/FAILURE_ISOLATION_ARCHITECTURE.md) - WebRTC isolation, circuit breakers
- [Hot Path Optimizations](crates/keeper-pam-webrtc-rs/docs/HOT_PATH_OPTIMIZATION_SUMMARY.md) - Performance details
- [Performance Benchmarks](crates/keeper-pam-webrtc-rs/docs/PERFORMANCE_BENCHMARKS.md) - Measured results

### 🔧 Implementation Details
- [Testing Strategy](docs/TESTING_STRATEGY.md) - Workspace-wide testing approach
- [Protocol-Specific Docs](crates/keeper-pam-webrtc-rs/docs/) - ICE, SOCKS5, WebSocket

**Full documentation index**: [docs/README.md](docs/README.md)

## License

MIT - See [LICENSE](LICENSE) for details.

## Authors

Keeper Security Engineering <engineering@keeper.io>
