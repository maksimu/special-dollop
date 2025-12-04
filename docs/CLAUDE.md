# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Guacr is a ground-up rewrite of Apache Guacamole's guacd daemon in Rust. It is a remote desktop protocol proxy that translates RDP, VNC, SSH, Telnet, database protocols, and containerized environments into the Guacamole protocol for browser-based access. The rewrite targets through zero-copy I/O, SIMD acceleration, and lock-free concurrency.

**Status:** SSH handler production-ready. Other protocols in various stages.

**Current Implementation Status:**
- ✅ **SSH:** Production-ready with auth, clipboard, JPEG rendering (16ms debounce, 60fps)
- ✅ **Telnet:** Complete with scrollback, mouse events, threat detection
- ✅ **RDP:** Complete ironrdp integration with hardware encoding framework
- ✅ **VNC:** Complete VNC protocol implementation (RFB 3.8)
- ✅ **Database:** MySQL query execution wired up, spreadsheet UI complete
- ✅ **SFTP:** Complete file transfer implementation with russh-sftp API
- ✅ **RBI:** Complete chromiumoxide integration with file download support
- 📋 **Container (K8s/Docker):** Design document created (see CONTAINER_MANAGEMENT_PROTOCOL.md)

**Key Goals:**
- 10,000+ concurrent connections per instance (vs 500-1,000 in C guacd)
- Memory safety via Rust (no buffer overflows, use-after-free, or data races)
- Fast JPEG rendering (5-10x faster than PNG) with 60fps smoothness
- Cloud-native with Kubernetes and Docker management support

## Architecture Overview

### Core Daemon (guacr-daemon)
- Tokio async runtime with io_uring backend
- Multi-listener architecture: TCP (4822), WebSocket (4823), gRPC (50051), Metrics (9090)
- Lock-free instruction queues (crossbeam ArrayQueue) for zero contention
- Buffer pooling with pre-allocated pools to eliminate allocations
- Connection multiplexer spawning per-connection Tokio tasks

### Protocol Handlers (Plugin Architecture)
Each protocol is a separate crate implementing the `ProtocolHandler` trait:
- **guacr-ssh**: SSH using russh crate (PRODUCTION READY)
  - Password & SSH key authentication (all formats)
  - JPEG rendering with 16ms debounce (60fps)
  - Bidirectional clipboard (OSC 52 + bracketed paste)
  - Dirty region optimization with scroll detection
  - Default handshake size (1024x768 @ 96 DPI, then resizes to browser)
  - Proper backpressure with async EventCallback (4096 channel capacity)
- **guacr-telnet**: Telnet with custom implementation
- **guacr-rdp**: RDP using ironrdp crate (pure Rust)
- **guacr-vnc**: VNC using async-vnc crate
- **guacr-database**: MySQL/PostgreSQL/SQL Server/Oracle/MongoDB/Redis using sqlx/tiberius
  - Spreadsheet-like UI for query results
- **guacr-sftp**: SFTP file browser and transfer
- **guacr-rbi**: Remote Browser Isolation using headless Chrome + CDP
- **guacr-container**: Kubernetes/Docker management (DESIGN COMPLETE)
  - Pod/container list view (spreadsheet-like)
  - Shell access via kubectl exec / docker exec
  - Log streaming with tail/follow
  - Resource metrics display

Handlers communicate via async channels and share terminal rendering infrastructure.

### Guacamole Protocol (guacr-protocol)
Text-based instruction protocol: `LENGTH.ELEMENT,LENGTH.ELEMENT,...;`

Example: `5.mouse,1.0,2.10,3.120;` means mouse instruction with args (0, 10, 120)

Key instructions:
- `select` - Client selects protocol
- `args` - Server requests parameters
- `connect` - Client provides connection params
- `ready` - Server confirms ready
- `img` - Image data (PNG/JPEG)
- `key`/`mouse` - Input events
- `clipboard` - Clipboard data
- `sync` - Frame synchronization

### Zero-Copy Strategy
Critical performance technique throughout:
1. **bytes::Bytes** - Reference-counted immutable buffers (cheap clones, no copies)
2. **Buffer pools** - Pre-allocated buffers reused across connections
3. **io_uring** - Kernel bypass for zero-copy from NIC to user space
4. **Shared memory** - Zero-copy IPC for browser isolation (RBI handler)
5. **SIMD** - AVX2/NEON for 8x pixel operation speed (format conversion, comparison)

### Lock-Free Concurrency
Avoid mutex contention:
- Use `crossbeam::queue::ArrayQueue` instead of `Mutex<Vec<T>>`
- Use `parking_lot::RwLock` when locks are required (faster than std)
- Atomic operations for stats/counters
- Arena allocators (bumpalo) for short-lived objects

## Commands

### Building
```bash
cargo build --release                 # Full release build
cargo build -p guacr-daemon          # Build specific crate
cargo build --workspace              # Build all crates
```

### Testing
```bash
cargo test                           # All tests
cargo test -p guacr-protocol         # Specific crate tests
cargo test -- --nocapture           # Tests with output
RUST_LOG=debug cargo test           # Tests with logging
cargo test --test integration        # Integration tests only
cargo bench                          # Run benchmarks
```

### Development
```bash
cargo fmt                            # Format code
cargo clippy -- -D warnings          # Lint with warnings as errors
cargo audit                          # Security audit
cargo watch -x 'run --bin guacr-daemon'  # Auto-reload on changes
```

### Running
```bash
# Development
cargo run -- --config config/guacr.toml
RUST_LOG=debug cargo run -- --config config/guacr.toml

# Release
cargo run --release -- --config config/guacr.toml
./target/release/guacr-daemon --config config/guacr.toml
```

### Profiling
```bash
cargo flamegraph --bin guacr-daemon  # CPU profiling
heaptrack ./target/release/guacr-daemon  # Memory profiling
```

### Docker
```bash
docker build -f docker/Dockerfile -t guacr .
docker-compose -f docker/docker-compose.yml up
```

### Kubernetes
```bash
kubectl apply -f k8s/deployment.yaml
kubectl scale deployment guacr --replicas=5
kubectl logs -f deployment/guacr
```

## Critical Design Patterns

### Protocol Handler Implementation
When implementing a new protocol handler:

1. Implement `ProtocolHandler` trait in `guacr-handlers`
2. Use lock-free queues for instruction passing (not mpsc)
3. Acquire buffers from pool (not Vec::new)
4. Use zero-copy techniques (bytes::Bytes)
5. For terminal protocols (SSH/Telnet): use vt100 parser and render to PNG
6. For graphical protocols (RDP/VNC): use dirty rectangle optimization
7. Handle Guacamole instructions: `key`, `mouse`, `clipboard`, `size`
8. Generate Guacamole instructions: `img`, `sync`, `audio`

Example structure:
```rust
#[async_trait]
impl ProtocolHandler for SshHandler {
    fn name(&self) -> &str { "ssh" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        // 1. Connect to remote host
        // 2. Set up bidirectional forwarding
        // 3. Convert terminal output to Guacamole img instructions
        // 4. Convert Guacamole key/mouse to protocol input
    }
}
```

### Buffer Pool Usage
Always use buffer pools to avoid allocations:

```rust
// Acquire from pool
let mut buf = buffer_pool.acquire();

// Use buffer
encoder.encode_into(&mut buf, data)?;

// Convert to Bytes (zero-copy via Arc)
let bytes = buf.freeze();

// Buffer automatically returned to pool when dropped
```

### SIMD Pixel Operations
For pixel format conversion or comparison:

```rust
// Use platform-specific SIMD with fallback
if is_x86_feature_detected!("avx2") {
    unsafe { convert_bgra_to_rgba_avx2(input, output) };
} else {
    convert_bgra_to_rgba_scalar(input, output);
}
```

### Lock-Free Queue Pattern
For high-throughput instruction passing:

```rust
// Use ArrayQueue, not Mutex<Vec> or mpsc
let queue = Arc::new(ArrayQueue::<Instruction>::new(1024));

// Producer
queue.push(instruction)?;

// Consumer
if let Some(instruction) = queue.pop() {
    process(instruction);
}
```

## Important Constraints

### NO EMOJIS
**Critical:** Do not use emojis in any code, documentation, comments, or commit messages. This is enforced policy (see CONTRIBUTING.md).

### Performance Requirements
- <100ms latency for input events
- 30+ FPS for graphical protocols
- 10,000+ concurrent connections per instance
- <10MB memory per connection
- Zero-copy whenever possible (use bytes::Bytes)

### Memory Safety
- Review all `unsafe` code carefully
- Document safety invariants
- Prefer safe abstractions when possible
- Run `cargo audit` before committing

### Testing Requirements
- >80% code coverage
- Test error paths, not just happy paths
- Benchmark performance-critical code with `cargo bench`
- Integration tests for each protocol handler

## Configuration

Configuration in `config/guacr.toml` (TOML format):
- `[server]` - Bind address, max connections, timeouts
- `[protocols.*]` - Per-protocol settings (enabled, max connections, defaults)
- `[logging]` - Level, format (json/pretty), output
- `[metrics]` - Prometheus port, OpenTelemetry endpoint
- `[performance]` - Enable io_uring, SIMD, GPU, buffer pool size
- `[recording]` - Session recording (local/S3/Azure)
- `[security]` - TLS, rate limiting, clipboard restrictions

Environment variables:
- `RUST_LOG` - Logging level (trace/debug/info/warn/error)
- `GUACR_CONFIG_PATH` - Config file path
- `OTEL_EXPORTER_OTLP_ENDPOINT` - OpenTelemetry endpoint

## Documentation Structure

### Core Documentation (`docs/`)

**Essential Reference**:
- `CLAUDE.md` - This file - guidance for Claude Code
- `START_HERE.md` - Quick start guide and current implementation status
- `CONTRIBUTING.md` - Contribution guidelines
- `README.md` - Documentation index

**Protocol Implementation**:
- `RDP_VNC_IMPLEMENTATION.md` - Complete RDP/VNC guide with SFTP integration
- `RBI_IMPLEMENTATION.md` - Remote Browser Isolation implementation
- `SFTP_CHANNEL_ADAPTER.md` - SFTP file transfer implementation
- `THREAT_DETECTION.md` - Security threat detection system

**Technical Guides**:
- `INTEGRATION_GUIDE.md` - System integration patterns
- `TESTING_GUIDE.md` - Testing procedures
- `PERFORMANCE_OPTIMIZATIONS.md` - Performance techniques
- `ZERO_COPY_INTEGRATION.md` - Zero-copy optimization guide
- `WEBRTC_INTEGRATION_CHANGES.md` - WebRTC integration details
- `WEBRTC_MULTI_CHANNEL_OPTIMIZATION.md` - Multi-channel optimization
- `RECORDING_TRANSPORTS.md` - Session recording system

**Protocol Specifications**:
- `BINARY_PROTOCOL_SPEC.md` - Binary protocol format
- `GUACAMOLE_PROTOCOL_COVERAGE.md` - Guacamole protocol support
- `CONTAINER_MANAGEMENT_PROTOCOL.md` - Kubernetes/Docker protocol design

**Security**:
- `ADDITIONAL_PROTOCOLS_SECURITY.md` - Security considerations
- `KEEPER_PAM_WEBRTC_RS_INTEGRATION.md` - Keeper integration

### Reference Documentation (`docs/docs/`)

**Context** (`docs/docs/context/`):
- `GUACR_ARCHITECTURE_PLAN.md` - Complete technical design
- `GUACR_ZERO_COPY_OPTIMIZATION.md` - Performance techniques
- `GUACR_PROTOCOL_HANDLERS_GUIDE.md` - Implementation patterns
- `GUACR_QUICKSTART_GUIDE.md` - Getting started
- `GUACR_EXECUTIVE_SUMMARY.md` - Business case
- `GUACR_WEBRTC_INTEGRATION.md` - WebRTC integration

**Original guacd Reference** (`docs/docs/reference/original-guacd/`):
- Reference when implementing protocol handlers
- Shows how original C implementation works
- Includes database plugins and RBI analysis

### Development Documentation (`docs/development/`)

- `CUSTOM_GLYPH_PROTOCOL.md` - Custom glyph protocol design
- `GUACD_FEATURE_COMPARISON.md` - Feature comparison with guacd
- `PROTOCOL_CONVERSION.md` - Protocol conversion details
- `RENDERING_METHODS.md` - Rendering techniques

## Development Phases

**Current Status:** Phase 4 in progress

**Phase 4 (PLANNED):** Container Management
- [ ] Kubernetes/Docker protocol handler (see CONTAINER_MANAGEMENT_PROTOCOL.md)
- [ ] Pod/container list view (spreadsheet-like)
- [ ] Shell access (kubectl exec / docker exec)
- [ ] Log streaming and metrics

**Phase 5 (FUTURE):** Advanced Features
- [ ] Custom glyph protocol (7-25x bandwidth savings)
- [ ] Complete hardware encoder implementations (NVENC, QuickSync, etc.)
- [ ] RDPGFX channel integration
- [ ] Audio channel support
- [ ] Multi-session support

## Key Files to Reference

When implementing protocol handlers:
- `docs/context/GUACR_PROTOCOL_HANDLERS_GUIDE.md` - Implementation patterns
- `docs/reference/original-guacd/GUACD_ARCHITECTURE_DEEP_DIVE.md` - How original works

When optimizing performance:
- `docs/context/GUACR_ZERO_COPY_OPTIMIZATION.md` - Zero-copy techniques

When understanding architecture:
- `docs/context/GUACR_ARCHITECTURE_PLAN.md` - Complete design

## Common Pitfalls

1. **Don't use mpsc channels for high-throughput instruction passing** - Use lock-free ArrayQueue
2. **Don't allocate buffers per frame** - Use buffer pools
3. **Don't copy pixel data** - Use bytes::Bytes for reference counting
4. **Don't use Mutex<Vec<T>> for queues** - Use ArrayQueue
5. **Don't use std::sync::RwLock** - Use parking_lot::RwLock (faster)
6. **Don't skip SIMD optimizations** - Use AVX2/NEON with scalar fallback
7. **Don't use regular I/O** - Use io_uring when available (Linux)
8. **Don't forget to mark instruction/buffer returns** - Use RAII patterns

## Workspace Structure

```
crates/
├── guacr-daemon/       Main binary, server setup, connection multiplexer
├── guacr-protocol/     Guacamole protocol codec (Decoder/Encoder)
├── guacr-handlers/     ProtocolHandler trait + registry
├── guacr-ssh/          SSH handler (russh)
├── guacr-telnet/       Telnet handler
├── guacr-rdp/          RDP handler (ironrdp)
├── guacr-vnc/          VNC handler (async-vnc)
├── guacr-database/     MySQL/PostgreSQL/SQL Server (sqlx/tiberius)
├── guacr-rbi/          Remote Browser Isolation (headless Chrome)
├── guacr-terminal/     Shared terminal emulation (vt100 parser, PNG render)
├── guacr-grpc/         gRPC service for distributed deployment
└── guacr-recorder/     Session recording (local/S3/Azure)
```

Each crate should be independently testable and documented.

## Monitoring & Observability

Prometheus metrics (port 9090):
- `guacr_active_connections{protocol="rdp"}` - Active connection count
- `guacr_connection_duration_seconds` - Connection duration histogram
- `guacr_frame_encoding_duration_seconds` - Encoding latency
- `guacr_zero_copy_bytes_total` - Bytes transferred via zero-copy
- `guacr_buffer_pool_hits_total` - Buffer pool efficiency

OpenTelemetry traces:
- Use `#[instrument]` macro on async functions
- Spans automatically include trace context
- Export to Jaeger via OTLP

Structured logging:
- Use `tracing` crate (not `log`)
- JSON format for production
- Include relevant context (client_id, protocol, etc.)
