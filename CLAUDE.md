# CLAUDE.md - Workspace Root

This file provides guidance to Claude Code (claude.ai/code) when working with the **keeper-pam-connections** workspace.

## Project Overview

**keeper-pam-connections** is a **Cargo workspace monorepo** providing a unified Python package for secure connection management, built on WebRTC with Rust.

- **Architecture**: Monorepo with multiple crates, unified Python bindings
- **Primary Language**: Rust (core libraries) with Python bindings (abi3-py37+)
- **Python Package**: `keeper-pam-connections` (renamed from `keeper-pam-webrtc-rs`)
- **Target Users**: Internal Keeper Security products (Gateway, Commander)

## Workspace Structure

```
keeper-pam-connections/
├── Cargo.toml                          # Workspace root
├── docs/                               # Workspace-level documentation
│   ├── guacr/                          # guacr protocol handler docs
│   └── ...                             # WebRTC docs
├── crates/
│   ├── keeper-pam-webrtc-rs/          # Core WebRTC library (rlib)
│   │   ├── CLAUDE.md                   # Crate-specific guidance
│   │   ├── src/                        # Core Rust code
│   │   └── docs/                       # Protocol-specific docs
│   ├── python-bindings/                # Unified Python package (cdylib)
│   │   ├── src/lib.rs                  # Aggregates all bindings
│   │   ├── python/keeper_pam_connections/  # Python module
│   │   └── tests/                      # All Python tests
│   ├── guacr/                          # Protocol handlers (aggregator)
│   ├── guacr-handlers/                 # Handler trait definitions
│   ├── guacr-protocol/                 # Guacamole protocol codec
│   ├── guacr-terminal/                 # Terminal emulation
│   ├── guacr-ssh/                      # SSH protocol handler
│   ├── guacr-telnet/                   # Telnet protocol handler
│   ├── guacr-rdp/                      # RDP protocol handler (IronRDP)
│   ├── guacr-vnc/                      # VNC protocol handler
│   ├── guacr-database/                 # Database protocol handlers
│   ├── guacr-sftp/                     # SFTP file transfer
│   ├── guacr-rbi/                      # Remote Browser Isolation
│   └── guacr-threat-detection/         # AI threat detection
└── README.md                           # Workspace overview
```

## Quick Start

### Build Everything
```bash
# Build all Rust crates
cargo build --workspace --release

# Build Python package
cd crates/python-bindings
maturin build --release

# Or use workspace script
./build_and_test.sh
```

### Run All Tests
```bash
# Workspace-wide checks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --lib

# Python tests
cd crates/python-bindings/tests
python -m pytest -v
```

## Important Concepts

### 1. Unified Python Bindings Architecture

**Key Point**: Python bindings are NOT in `keeper-pam-webrtc-rs` anymore!

- **`crates/keeper-pam-webrtc-rs/`** = Pure Rust library (rlib)
  - Provides `register_webrtc_module()` function
  - Contains PyO3 struct definitions (Rust code)
  - **NOT** compiled as Python extension

- **`crates/python-bindings/`** = Unified Python package (cdylib)
  - Aggregates all crates' Python bindings
  - Builds the actual `keeper_pam_connections` module
  - This is what users install and import

See [ARCHITECTURE_EXPLANATION.md](ARCHITECTURE_EXPLANATION.md) for full details.

### 2. Python Package Rename

**Breaking Change**: Package renamed for unified architecture

```python
# Old (deprecated)
import keeper_pam_webrtc_rs

# New (current)
import keeper_pam_connections
```

All functionality is identical, only the import name changed.

### 3. guacr Protocol Handlers (Merged from pam-guacr)

**Important**: The guacr crates were merged from the separate pam-guacr repository
and are now part of this monorepo while remaining independently publishable.

**Protocol Handlers Available**:
- **SSH** - Production-ready with auth, clipboard, JPEG rendering
- **Telnet** - Complete with scrollback, mouse events
- **RDP** - Production-ready with IronRDP (60fps, dynamic resize)
- **VNC** - Complete VNC protocol implementation (RFB 3.8)
- **Database** - MySQL, PostgreSQL, MongoDB, Redis, Oracle, SQL Server, MariaDB
- **SFTP** - Complete file transfer implementation
- **RBI** - Remote Browser Isolation with Chrome/CDP

**Independent Publishing**:
Each guacr crate can be published independently to crates.io:
```bash
# Publish individual crate
cd crates/guacr-ssh
cargo publish

# Or use GitHub workflow (manual trigger)
# .github/workflows/publish-crates.yml
```

**For External Users**:
```toml
[dependencies]
guacr = { version = "1.1", features = ["ssh", "rdp"] }
```

**For Internal Use (Monorepo)**:
```toml
[dependencies]
guacr = { path = "../guacr", features = ["ssh", "rdp"] }
```

See [docs/guacr/](docs/guacr/) for protocol handler documentation.

### 4. Workspace-Wide Testing

All quality checks run across **all crates**:
- `cargo fmt --all` - Formats all crates
- `cargo clippy --workspace` - Lints all crates
- `cargo test --workspace` - Tests all crates

See [TESTING_CHECKLIST.md](TESTING_CHECKLIST.md) for verification.

## Build Commands by Use Case

### Rust Development (Core Library)
```bash
# Build just the core library
cargo build --release -p keeper-pam-webrtc-rs

# Test just the core library
cargo test -p keeper-pam-webrtc-rs

# Check a specific crate
cargo check -p keeper-pam-webrtc-rs
```

### Python Development
```bash
# Build and install Python package
cd crates/python-bindings
maturin develop

# Run Python tests
cd tests
python -m pytest -v

# Build release wheel
maturin build --release
```

### Workspace-Wide Operations
```bash
# Format all code
cargo fmt --all

# Lint everything
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Test everything
cargo test --workspace
cd crates/python-bindings/tests && python -m pytest -v

# Or use the comprehensive script
./build_and_test.sh
```

## Documentation

### Workspace-Level Docs (`docs/`)
- **[PYTHON_API_CONTRACT.md](docs/PYTHON_API_CONTRACT.md)** - Python API reference
- **[ACTOR_DASHMAP_RAII.md](docs/ACTOR_DASHMAP_RAII.md)** - Registry architecture
- **[FAILURE_ISOLATION_ARCHITECTURE.md](docs/FAILURE_ISOLATION_ARCHITECTURE.md)** - WebRTC isolation
- **[HOT_PATH_OPTIMIZATION_SUMMARY.md](docs/HOT_PATH_OPTIMIZATION_SUMMARY.md)** - Performance details
- **[PERFORMANCE_BENCHMARKS.md](docs/PERFORMANCE_BENCHMARKS.md)** - Benchmark results
- **[TESTING_STRATEGY.md](docs/TESTING_STRATEGY.md)** - Testing philosophy

### Migration & Setup
- **[ARCHITECTURE_EXPLANATION.md](docs/ARCHITECTURE_EXPLANATION.md)** - Python bindings architecture
- **[TESTING_STRATEGY.md](docs/TESTING_STRATEGY.md)** - Workspace testing approach

### Crate-Specific Docs
- **[crates/keeper-pam-webrtc-rs/CLAUDE.md](crates/keeper-pam-webrtc-rs/CLAUDE.md)** - Core library guidance
- **[crates/keeper-pam-webrtc-rs/docs/](crates/keeper-pam-webrtc-rs/docs/)** - Protocol-specific docs

**Full index**: [docs/README.md](docs/README.md)

## Adding New Crates

To extend the workspace with a new protocol:

1. **Create the crate**:
```bash
cargo new --lib crates/my-protocol-rs
```

2. **Add to workspace** (`Cargo.toml`):
```toml
[workspace]
members = [
    "crates/keeper-pam-webrtc-rs",
    "crates/python-bindings",
    "crates/my-protocol-rs"  # Add here
]
```

3. **Implement registration function**:
```rust
// crates/my-protocol-rs/src/python/mod.rs
pub fn register_my_protocol_module(
    _py: Python<'_>, 
    parent: &Bound<'_, PyModule>
) -> PyResult<()> {
    parent.add_class::<MyClass>()?;
    Ok(())
}
```

4. **Register in unified bindings**:
```rust
// crates/python-bindings/src/lib.rs
#[pymodule]
fn keeper_pam_connections(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    keeper_pam_webrtc_rs::python::register_webrtc_module(py, m)?;
    my_protocol_rs::python::register_my_protocol_module(py, m)?;  // Add here
    Ok(())
}
```

All functionality now available through `import keeper_pam_connections`!

## Git Workflow Rules

**CRITICAL**: Never commit code automatically. Always let the user review changes and commit manually.

- ❌ **NEVER** run `git commit` commands
- ❌ **NEVER** run `git add` commands
- ❌ **NEVER** run `git push` commands
- ✅ Make code changes using edit tools
- ✅ Let user review with `git diff`
- ✅ Let user decide when to commit
- ✅ Let user write commit messages

## Key Design Principles

1. **Unified Python Package**: Single import, all functionality
2. **Extensible Architecture**: Easy to add new protocol crates
3. **Workspace-Wide Quality**: All checks run on all crates
4. **Clean Separation**: Core logic separate from Python bindings
5. **Always Fast**: All optimizations enabled by default

## Common Tasks

### Update Python imports in tests
```bash
# Already done during migration, but if needed:
cd crates/python-bindings/tests
find . -name "*.py" -exec sed -i '' 's/keeper_pam_webrtc_rs/keeper_pam_connections/g' {} +
```

### Verify workspace structure
```bash
# Check workspace members
cargo metadata --no-deps --format-version 1 | jq '.workspace_members'

# List all crates
ls -d crates/*/
```

### Run CI checks locally
```bash
./build_and_test.sh
```

## Performance Targets

**Frame Processing** (Production Verified):
- Small frames (0-64B): 398-477ns → 2.5M frames/sec
- Medium frames (1.5KB): 430-490ns → 2.3M frames/sec
- Large frames (8KB): 513-580ns → 1.9M frames/sec
- Max UDP (64KB): 1428-2213ns → 700K frames/sec

See [docs/PERFORMANCE_BENCHMARKS.md](docs/PERFORMANCE_BENCHMARKS.md) for details.

## Cross-Platform Considerations

- **Windows**: 16ms timer granularity (avoid `tokio::time::timeout` in hot paths)
- **TCP_NODELAY**: Always enabled for interactive protocols
- **No explicit flush()**: Redundant with TCP_NODELAY, hurts Windows performance

See [crates/keeper-pam-webrtc-rs/CLAUDE.md](crates/keeper-pam-webrtc-rs/CLAUDE.md) for detailed platform notes.

## Summary

This is a **workspace monorepo** with:
- ✅ Multiple Rust crates (extensible)
- ✅ Unified Python package (`keeper-pam-connections`)
- ✅ Workspace-wide quality checks
- ✅ Clean architecture with separation of concerns

For crate-specific details, see the CLAUDE.md in each crate directory.

---

**Last Updated**: January 28, 2026  
**Maintainers**: Keeper Security Engineering <engineering@keeper.io>
