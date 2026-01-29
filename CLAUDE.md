# CLAUDE.md - keeper-pam-connections Workspace

This file provides comprehensive guidance to Claude Code (claude.ai/code) when working with the **keeper-pam-connections** workspace.

## Project Overview

**keeper-pam-connections** is a **Cargo workspace monorepo** providing a unified Python package for secure connection management, built on WebRTC with Rust.

- **Architecture**: Monorepo with multiple crates, unified Python bindings
- **Primary Language**: Rust (core libraries) with Python bindings (abi3-py37+)
- **Python Package**: `keeper-pam-connections` (renamed from `keeper-pam-webrtc-rs`)
- **Target Users**: Internal Keeper Security products (Gateway, Commander)
- **Main Use Case**: 100s of developers editing 4K videos over these connections

### Two Major Subsystems

**1. WebRTC Core** (`keeper-pam-webrtc-rs`)
- Secure WebRTC-based tunneling via "Tube API" abstraction
- Three conversation types: `tunnel` (TCP), `guacd` (Guacamole protocol), `socks5` (SOCKS5 proxy)
- Lock-free hot paths with SIMD optimization (398-2213ns frame processing)
- Actor-based registry with automatic backpressure and RAII resource management

**2. Protocol Handlers** (`guacr` family of crates)
- Ground-up rewrite of Apache Guacamole's guacd daemon in Rust
- Translates RDP, VNC, SSH, Telnet, database protocols into Guacamole protocol
- 10,000+ concurrent connections per instance (vs 500-1,000 in C guacd)
- Zero-copy I/O, SIMD acceleration, lock-free concurrency
- **Status**: SSH and RDP handlers production-ready

## Workspace Structure

```
keeper-pam-connections/
├── Cargo.toml                          # Workspace root
├── docs/                               # Workspace-level documentation
├── crates/
│   ├── keeper-pam-webrtc-rs/          # Core WebRTC library (rlib)
│   ├── python-bindings/                # Unified Python package (cdylib)
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

### Crate-Specific Commands

**WebRTC Development:**
```bash
cargo build --release -p keeper-pam-webrtc-rs
cargo test -p keeper-pam-webrtc-rs
RUST_LOG=debug cargo test test_tube_p2p_data_transfer_end_to_end -- --nocapture
```

**Protocol Handler Development:**
```bash
cargo build --release -p guacr-ssh
cargo test -p guacr-protocol
cargo bench  # Run performance benchmarks
```

**Python Development:**
```bash
cd crates/python-bindings
maturin develop
cd tests && python -m pytest -v
```

## Architecture

### WebRTC Core: Three-Layer System

```
┌─────────────────────────────────────────────────────────┐
│ Python Layer (PyTubeRegistry, PyO3 bindings)           │
│ - Python API with base64-encoded SDP                    │
│ - Signal callbacks for WebRTC events                    │
└─────────────────────────────────────────────────────────┘
                         │
┌─────────────────────────────────────────────────────────┐
│ Registry Layer (Actor + DashMap + RAII)                │
│ - RegistryHandle: Public API with lock-free reads       │
│ - RegistryActor: Single-threaded coordination           │
│ - Tube: RAII resource owner with automatic cleanup      │
│ - Backpressure: Max 100 concurrent creates (default)    │
└─────────────────────────────────────────────────────────┘
                         │
┌─────────────────────────────────────────────────────────┐
│ WebRTC/Channel Layer (Isolated APIs + Hot Paths)       │
│ - IsolatedWebRTCAPI: Per-tube WebRTC isolation          │
│ - TubeCircuitBreaker: Failure isolation                 │
│ - Channel: Connection/data handling with SIMD           │
│ - Hot paths: 398-2213ns frame processing (SIMD)         │
└─────────────────────────────────────────────────────────┘
```

**Key WebRTC Principles:**

1. **Actor Model for Coordination** (`src/tube_registry.rs`)
   - Single-threaded RegistryActor handles tube creation/deletion
   - No locks needed for coordination (message-passing)
   - Automatic backpressure (rejects when overloaded)
   - Max 100 concurrent creates (configurable via `WEBRTC_MAX_CONCURRENT_CREATES`)

2. **Lock-Free Hot Paths** (`src/channel/`)
   - DashMap for concurrent tube reads (zero locks)
   - Thread-local buffer pools (zero contention)
   - Atomic queue depth counters (1ns vs 50-100ns Mutex)
   - SIMD-optimized frame parsing (398-2213ns per frame)

3. **RAII Resource Management** (`src/tube.rs`)
   - Tube owns all resources (signal_sender, metrics_handle, keepalive_task)
   - `Tube::drop()` automatically cleans up everything
   - Impossible to leak resources (compiler-enforced)

4. **Failure Isolation** (`src/webrtc_core.rs`)
   - IsolatedWebRTCAPI per tube (no cross-contamination)
   - TubeCircuitBreaker prevents cascading failures
   - Shared tokio runtime with automatic panic isolation
   - One tube failure NEVER affects others

### Protocol Handlers: Plugin Architecture

**Core Components:**

1. **guacr-daemon**: Multi-listener architecture (TCP 4822, WebSocket 4823, gRPC 50051)
2. **guacr-protocol**: Guacamole protocol codec (`LENGTH.ELEMENT,...;` format)
3. **guacr-handlers**: `ProtocolHandler` trait for plugin system
4. **Protocol Crates**: Each protocol in separate crate (SSH, RDP, VNC, etc.)

**Production-Ready Handlers:**
- **SSH** (guacr-ssh): Password/key auth, JPEG rendering (60fps), clipboard (OSC 52)
- **RDP** (guacr-rdp): IronRDP, 60fps, scroll detection (90-99% bandwidth reduction)
- **VNC** (guacr-vnc): RFB 3.8 protocol support
- **Telnet**, **Database**, **SFTP**, **RBI**: Complete implementations

**Handler Pattern:**
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

### Unified Python Bindings

**Key Point**: Python bindings are NOT in `keeper-pam-webrtc-rs` anymore!

- **`crates/keeper-pam-webrtc-rs/`** = Pure Rust library (rlib)
  - Provides `register_webrtc_module()` function
  - Contains PyO3 struct definitions (Rust code)
  - **NOT** compiled as Python extension

- **`crates/python-bindings/`** = Unified Python package (cdylib)
  - Aggregates all crates' Python bindings
  - Builds the actual `keeper_pam_connections` module
  - This is what users install and import

```python
# Old (deprecated)
import keeper_pam_webrtc_rs

# New (current)
import keeper_pam_connections
```

## Critical Implementation Details

### Python API Contract: Base64 SDP Encoding

**CRITICAL**: ALL SDP exchanged over the Python/Rust boundary MUST be base64-encoded.

```python
# ✅ CORRECT: Use what Rust returns directly
server_info = registry.create_tube(...)
offer_base64 = server_info['offer']  # Already base64
client_info = registry.create_tube(..., offer=offer_base64)

# ❌ WRONG: Don't decode/encode
offer_raw = base64.b64decode(offer_base64)  # NO!
client_info = registry.create_tube(..., offer=offer_raw)  # FAILS!
```

See `crates/python-bindings/docs/PYTHON_API_CONTRACT.md` for complete API reference.

### Trickle ICE Requirement

**CRITICAL**: ICE restart functionality **requires** `trickle_ice=True`.

```python
# ✅ CORRECT: ICE restart enabled
tube_info = registry.create_tube(
    trickle_ice=True,  # Required for ICE restart
    # ...
)
```

### Logging System (Unified as of Oct 2025)

```rust
// Hot path logging (1-2ns overhead when disabled)
if unlikely!(crate::logger::is_verbose_logging()) {
    debug!("Frame processing: {} bytes", len);
    trace!("Ultra-verbose details: {}", data);
}

// Warnings/errors ALWAYS log (no gating)
warn!("Connection error: {}", err);
error!("Critical failure: {}", err);

// Branch prediction for hot paths
if likely!(conn_no == 1) {
    // Main data path - 90%+ of traffic
}
```

**Key Points:**
- Backend: `tracing-subscriber` (bridges to Python logging)
- API: Pure `log` crate for all logging calls
- Hot path guard: `unlikely!(crate::logger::is_verbose_logging())`
- Single atomic flag: `VERBOSE_LOGGING` (controlled by Python)

### Zero-Copy Strategy

Critical performance technique throughout:
1. **bytes::Bytes** - Reference-counted immutable buffers (cheap clones, no copies)
2. **Buffer pools** - Pre-allocated buffers reused across connections
3. **io_uring** - Kernel bypass for zero-copy from NIC to user space (Linux)
4. **SIMD** - AVX2/NEON for 8x pixel operation speed (format conversion, comparison)

**Buffer Pool Usage:**
```rust
// Acquire from pool
let mut buf = buffer_pool.acquire();

// Use buffer
encoder.encode_into(&mut buf, data)?;

// Convert to Bytes (zero-copy via Arc)
let bytes = buf.freeze();

// Buffer automatically returned to pool when dropped
```

**SIMD Pattern:**
```rust
// Use platform-specific SIMD with fallback
if is_x86_feature_detected!("avx2") {
    unsafe { convert_bgra_to_rgba_avx2(input, output) };
} else {
    convert_bgra_to_rgba_scalar(input, output);
}
```

### Lock-Free Concurrency

**NEVER use Mutex in hot paths:**
```rust
// ❌ WRONG: Mutex locks in hot paths
let depth = self.queue.lock().unwrap().len();  // 50-100ns

// ✅ CORRECT: Atomic counters
let depth = self.queue_size.load(Ordering::Acquire);  // ~1ns

// ✅ CORRECT: ArrayQueue for instruction passing
let queue = Arc::new(ArrayQueue::<Instruction>::new(1024));
queue.push(instruction)?;
if let Some(instruction) = queue.pop() {
    process(instruction);
}
```

**Guidelines:**
- Use `crossbeam::queue::ArrayQueue` instead of `Mutex<Vec<T>>`
- Use `parking_lot::RwLock` when locks are required (faster than std)
- Use atomic operations for stats/counters
- Use arena allocators (bumpalo) for short-lived objects

### TCP_NODELAY and Flush Behavior

```rust
// ❌ WRONG: Without TCP_NODELAY, Nagle's algorithm batches data
stream.set_nodelay(false)?;  // Nagle enabled = buffering
backend.write_all(payload).await?;
// User types "h", Nagle batches it, appears on next keystroke

// ✅ CORRECT: TCP_NODELAY ensures immediate send (no flush needed)
stream.set_nodelay(true)?;  // Disable Nagle's algorithm
backend.write_all(payload).await?;  // Data sent immediately
// flush() is redundant - TCP with TCP_NODELAY is already unbuffered
// Explicit flush hurts Windows performance (16ms timer granularity overhead)
```

**Guidelines:**
- ✅ **Always set `TCP_NODELAY` for interactive protocols** (SSH, RDP, Guacd)
- ✅ **Never call `flush()` on TCP streams with `TCP_NODELAY`** - redundant and hurts Windows

## Code Organization

### WebRTC Core Files (`crates/keeper-pam-webrtc-rs/src/`)

**Core Files (Read These First):**
- **`lib.rs`**: Entry point, module declarations
- **`tube_registry.rs`**: Registry actor, tube management, backpressure
- **`tube.rs`**: Tube struct, RAII Drop implementation, lifecycle
- **`webrtc_core.rs`**: IsolatedWebRTCAPI, peer connection management
- **`python/tube_registry_binding.rs`**: PyO3 Python bindings

**Channel System (Hot Paths):**
- **`channel/core.rs`**: Channel creation and initialization
- **`channel/frame_handling.rs`**: SIMD frame parsing, routing (HOT PATH)
- **`channel/connections.rs`**: TCP reads, backpressure, batching (HOT PATH)
- **`channel/protocol.rs`**: Guacamole protocol messages
- **`channel/server.rs`**: SOCKS5 server, TCP tunnel mode
- **`channel/socks5.rs`**: SOCKS5 client, TCP CONNECT handling
- **`channel/udp.rs`**: SOCKS5 UDP ASSOCIATE implementation

**Supporting Infrastructure:**
- **`buffer_pool.rs`**: Thread-local buffer pools (lock-free)
- **`webrtc_data_channel.rs`**: Event-driven backpressure system
- **`webrtc_circuit_breaker.rs`**: Circuit breaker for failure isolation
- **`metrics/`**: Metrics collection and monitoring
- **`logger.rs`**: Unified logging system with verbose flag

### Protocol Handler Crates

```
crates/
├── guacr/              Protocol handlers aggregator
├── guacr-protocol/     Guacamole protocol codec (Decoder/Encoder)
├── guacr-handlers/     ProtocolHandler trait + registry
├── guacr-ssh/          SSH handler (russh) - PRODUCTION READY
├── guacr-rdp/          RDP handler (ironrdp) - PRODUCTION READY
├── guacr-vnc/          VNC handler (async-vnc)
├── guacr-terminal/     Shared terminal emulation (vt100 parser, PNG render)
├── guacr-database/     MySQL/PostgreSQL/SQL Server (sqlx/tiberius)
├── guacr-rbi/          Remote Browser Isolation (headless Chrome)
└── guacr-threat-detection/  AI-powered security analysis
```

## Development Patterns

### When Making Changes to Hot Paths

1. **Profile first** - Use `--features profiling` and measure impact
2. **Check logging** - Ensure logs are guarded with `unlikely!(is_verbose_logging())`
3. **Avoid locks** - Use atomics or thread-local storage
4. **Preserve SIMD** - Don't break existing SIMD optimizations
5. **Test performance** - Run benchmarks from `crates/keeper-pam-webrtc-rs/docs/PERFORMANCE_BENCHMARKS.md`

### When Modifying the Registry

1. **Use the actor** - Don't bypass RegistryActor for coordination
2. **Preserve RAII** - Resources must be owned by Tube for automatic cleanup
3. **Test backpressure** - Verify system rejects gracefully when overloaded
4. **Check for leaks** - Ensure no resource leaks (RAII makes this automatic)

### When Implementing Protocol Handlers

1. Implement `ProtocolHandler` trait in `guacr-handlers`
2. Use lock-free queues for instruction passing (ArrayQueue, not mpsc)
3. Acquire buffers from pool (not Vec::new)
4. Use zero-copy techniques (bytes::Bytes)
5. For terminal protocols (SSH/Telnet): use vt100 parser and render to PNG/JPEG
6. For graphical protocols (RDP/VNC): use dirty rectangle optimization
7. Handle Guacamole instructions: `key`, `mouse`, `clipboard`, `size`
8. Generate Guacamole instructions: `img`, `sync`, `audio`

### When Adding Python APIs

1. **Base64 encode SDP** - ALL SDP must be base64 over FFI boundary
2. **Document return values** - Specify all returned fields clearly
3. **Handle errors** - Convert Rust errors to Python exceptions properly
4. **Test with pytest** - Add tests to `crates/python-bindings/tests/` directory

### Test Organization

- **Rust unit tests**: `crates/*/src/tests/` (fast, deterministic)
- **Python CI tests**: `crates/python-bindings/tests/test_*.py` (fast, deterministic, <5 min)
- **Manual stress tests**: `crates/python-bindings/tests/manual_stress_tests.py` (NOT for CI)

**Important Test Patterns:**
- **CI tests**: Fast, deterministic, <5 minutes runtime
- **Manual stress tests**: Resource-intensive, run locally before major commits
- **Never run stress tests in CI** - they intentionally stress the system

## Performance

### Frame Processing (Production Verified)

| Frame Type | Parse Time | Encode Time | Throughput |
|---|---|---|---|
| Small (0-64B) | 398-477ns | 479ns | 2.5M frames/sec |
| Medium (1.5KB) | 430-490ns | 490ns | 2.3M frames/sec |
| Large (8KB) | 513-580ns | 580ns | 1.9M frames/sec |
| Max UDP (64KB) | 1428-2213ns | 2213ns | 700K frames/sec |

### UTF-8 Character Processing

- ASCII: 371ns per instruction (baseline)
- European languages: 623-630ns per instruction
- CJK languages: 490-599ns per instruction
- Mixed UTF-8: 603ns per instruction

### System Capacity

- Max concurrent tube creates: 100 (configurable via `WEBRTC_MAX_CONCURRENT_CREATES`)
- ICE keepalive interval: 60 seconds (prevents NAT timeout)
- Queue depth monitoring: ~1ns (atomic read)
- Memory per tube: ~52KB (~4% increase from isolation)

### Protocol Handler Requirements

- <100ms latency for input events
- 30+ FPS for graphical protocols (SSH/RDP achieve 60fps)
- 10,000+ concurrent connections per instance
- <10MB memory per connection
- Zero-copy whenever possible

## Cross-Platform Performance Considerations

This codebase runs on **Windows, Linux (x86_64/ARM64), and macOS**. Always consider platform differences when adding I/O operations, timers, or async logic.

### Critical Platform Differences

#### 1. Timer Granularity and tokio::time Operations

| Platform | Timer Resolution | tokio::time Overhead | Impact |
|----------|-----------------|---------------------|---------|
| **Linux** | ~1-2μs (epoll) | ~500ns-2μs | ✅ Fast timeouts work well |
| **macOS** | ~1-2μs (kqueue) | ~500ns-2μs | ✅ Fast timeouts work well |
| **Windows** | **~15-16ms** (IOCP) | **~16ms+** | ❌ Avoid frequent timeouts! |

**Real-world example:**
```rust
// ❌ BAD: This appears to be "50ms timeout" but Windows makes it ~65ms
let flush_result = tokio::time::timeout(
    Duration::from_millis(50),  // Requested: 50ms
    backend.flush(),
).await;  // Actual on Windows: ~65ms (15ms overage + setup cost)

// At 60 events/sec: 60 × 65ms = 3.9 seconds/min of timer overhead on Windows!
```

**Guidelines:**
- ⚠️ **Avoid `tokio::time::timeout()` in hot paths** - Windows timer granularity is 15-16ms
- ✅ Use OS-level timeouts (TCP socket timeouts) instead when possible
- ✅ If using `timeout()`, make durations ≥ 500ms to avoid proportionally high overhead on Windows
- ✅ Consider removing timeouts entirely if the underlying operation has its own timeout

#### 2. TCP/Socket Behavior Differences

| Feature | Linux/macOS | Windows | Notes |
|---------|------------|---------|-------|
| `TCP_NODELAY` | Immediate send, flush() = no-op | Immediate send, but flush() may trigger IOCP notification | Always set `TCP_NODELAY` for interactive protocols |
| `flush()` on TcpStream | ~100-500ns (instant) | ~50-200μs (IOCP queue) | **10-400× slower on Windows!** |
| Delayed-ACK behavior | 40ms default | Different algorithm | Affects small packet performance |
| Socket buffer sizes | Larger defaults | Smaller defaults | May need explicit `SO_SNDBUF`/`SO_RCVBUF` tuning |

#### 3. Async I/O Architecture Differences

| Platform | I/O Model | Per-Operation Cost | Characteristics |
|----------|-----------|-------------------|----------------|
| **Linux** | epoll (readiness-based) | ~100-500ns | "Tell me when ready" - instant notification |
| **macOS** | kqueue (readiness-based) | ~100-500ns | Similar to epoll |
| **Windows** | IOCP (completion-based) | **~50-200μs** | "Tell me when done" - must wait for completion |

**From tokio research:**
> "IOCP is completion-based instead of readiness-based, requiring more adaptation
> to bridge the two paradigms. Because Mio provides a readiness-based API similar
> to Linux epoll, many parts of the API can be one-to-one mappings on Linux, but
> require additional work on Windows."

**Guidelines:**
- ⚠️ Expect async operations to be **10-100× slower on Windows** for individual operations
- ✅ Batch operations when possible to amortize IOCP overhead
- ✅ Use event-driven patterns (WebRTC native events) instead of polling
- ✅ Profile on Windows early - "works fast on Linux" ≠ "works fast on Windows"

#### 4. DNS Resolution

| Platform | Implementation | Typical Latency |
|----------|---------------|----------------|
| Linux | `getaddrinfo()` with fast caching | ~10-50ms |
| macOS | mDNSResponder with aggressive caching | ~5-30ms |
| Windows | DNS Client service | **~50-200ms** (2-4× slower!) |

**Guidelines:**
- ✅ Use longer timeouts for HTTP clients that do DNS resolution (10s+ recommended)
- ✅ Consider DNS caching for frequently accessed hosts
- ✅ Test on Windows with real internet (not localhost)

#### 5. Console/Terminal I/O

| Platform | Console Write Speed | UTF-8 Handling |
|----------|-------------------|----------------|
| Linux | ~10-50μs | Native |
| macOS | ~10-50μs | Native |
| Windows | **~500-2000μs** | UTF-16 conversion required (100× slower!) |

**Guidelines:**
- ⚠️ **Never use verbose logging on Windows with console output** - 100× slower
- ✅ Use file-based logging or disable verbose mode by default
- ✅ Windows console writes can block for milliseconds - use async logging

#### 6. Thread-Local Storage (TLS)

| Platform | TLS Access Cost | Notes |
|----------|----------------|-------|
| Linux | ~1-2ns (`__thread`) | Compiled to direct memory access |
| macOS | ~1-2ns (`__thread`) | Similar to Linux |
| Windows | **~5-10ns** (Win32 TLS slots) | Requires Win32 API call |

**Guidelines:**
- ✅ Thread-local buffer pools are still fast on Windows (5-10ns is acceptable)
- ✅ Avoid excessive TLS lookups in tight loops (cache the reference)

### Pre-Commit Checklist for Cross-Platform Code

When adding **any** of these operations, test on Windows:

- [ ] `tokio::time::timeout()` - Does this need to be in a hot path?
- [ ] `tokio::time::sleep()` - Is timer granularity acceptable?
- [ ] `backend.flush()` - Is this actually necessary? (Hint: Not for TCP with `TCP_NODELAY`)
- [ ] HTTP clients with DNS - Is timeout long enough for Windows DNS?
- [ ] Console/debug output - Will this spam stderr on Windows?
- [ ] Frequent file I/O - Is buffering enabled?

### Testing Guidelines

**Minimum testing before merging hot-path changes:**

1. **Profile on all platforms**:
   ```bash
   # Linux/macOS - baseline
   cargo build --release --features profiling

   # Windows - expect 2-10× slower for I/O operations
   cargo build --release --features profiling
   ```

2. **Load testing on Windows**:
   - SSH interactive session (60 keystrokes/sec)
   - RDP video streaming (1000+ frames/sec)
   - Guacamole clipboard transfers (burst traffic)

3. **Monitor for Windows-specific symptoms**:
   - High CPU usage (timer spam)
   - Increased latency (IOCP overhead)
   - Circuit breaker opens (timeouts too aggressive)
   - Console log spam (verbose mode)

### Summary: Cross-Platform Performance Philosophy

**DO:**
- ✅ Set `TCP_NODELAY` for interactive protocols
- ✅ Use OS-level timeouts (socket options) instead of `tokio::time`
- ✅ Batch operations to amortize Windows IOCP overhead
- ✅ Test on Windows early and often
- ✅ Profile hot paths on all platforms

**DON'T:**
- ❌ Use `tokio::time::timeout` in hot paths (<1s)
- ❌ Call `flush()` on TCP streams with `TCP_NODELAY`
- ❌ Assume "fast on Linux" means "fast everywhere"
- ❌ Use verbose console logging on Windows
- ❌ Use aggressive timeouts without testing on Windows DNS/network
- ❌ Make .md summary files for everything. And don't try to use another format for this like .txt
- ❌ Add or use EMOJIS in any code or documentation
- ❌ Make speed/performance claims in comments without testing them

## Configuration and Environment Variables

```bash
# Registry configuration
export WEBRTC_MAX_CONCURRENT_CREATES=100  # Max concurrent tube creates

# NAT keepalive (defined in code, not configurable)
# ice_keepalive_interval: 60 seconds (changed from 300s to fix NAT timeout)

# Logging
RUST_LOG=debug  # Enable debug logging for troubleshooting
RUST_LOG=trace  # Ultra-verbose (hot paths gated by is_verbose_logging())

# Protocol handler configuration (for guacr)
GUACR_CONFIG_PATH=config/guacr.toml
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
```

## Common Issues and Solutions

### Issue: "Failed to decode from base64"
**Cause**: Passing raw SDP instead of base64-encoded SDP
**Solution**: Use the SDP strings returned from Rust directly without decoding

### Issue: "Timeout acquiring REGISTRY write lock"
**Cause**: This was fixed in the actor model refactor
**Solution**: Upgrade to the latest version with RegistryActor

### Issue: "STALE TUBE DETECTED"
**Cause**: This was fixed by RAII pattern
**Solution**: Tubes now automatically cleanup via `Drop` implementation

### Issue: Keyboard lag (1 keystroke delay)
**Cause**: Missing TCP_NODELAY or incorrect flush usage
**Solution**: Always set `TCP_NODELAY`, never call `flush()` with it enabled

### Issue: High CPU usage with many connections
**Cause**: Event-driven backpressure should prevent this
**Solution**: Check if you're polling instead of using WebRTC native events

## Git Workflow Rules

**CRITICAL**: Never commit code automatically. Always let the user review changes and commit manually.

- ❌ **NEVER** run `git commit` commands
- ❌ **NEVER** run `git add` commands
- ❌ **NEVER** run `git push` commands
- ❌ **NEVER** add `Co-Authored-By:` or any attribution to commit messages
- ✅ Make code changes using edit tools
- ✅ Let user review with `git diff`
- ✅ Let user decide when to commit
- ✅ Let user write commit messages (exactly as provided, with no additions)

### Commit Message Format

Follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) format:

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only changes
- `style`: Code style changes (formatting, missing semi-colons, etc.)
- `refactor`: Code change that neither fixes a bug nor adds a feature
- `perf`: Performance improvement
- `test`: Adding or updating tests
- `build`: Changes to build system or dependencies
- `ci`: Changes to CI configuration
- `chore`: Other changes that don't modify src or test files
- `revert`: Reverts a previous commit

**Examples:**
```
feat(webrtc): add ICE restart support
fix(guacr-ssh): resolve clipboard sync issue
docs: reorganize to crate-ownership model
refactor(registry): simplify actor coordination logic
```

## Design Principles

1. **Unified Python Package**: Single import, all functionality
2. **Extensible Architecture**: Easy to add new protocol crates
3. **Workspace-Wide Quality**: All checks run on all crates
4. **Clean Separation**: Core logic separate from Python bindings
5. **Always Fast**: All optimizations enabled by default (no feature flags needed)
6. **Security First**: Memory-safe Rust, comprehensive bounds checking
7. **Enterprise Stability**: Lock-free architecture, RAII cleanup, failure isolation
8. **Zero Configuration**: All optimizations enabled by default
9. **Production Ready**: Comprehensive error handling, graceful degradation

## Understanding the "Always Fast" Philosophy

This project follows an **"always fast"** design:
- All performance optimizations are **built-in and always enabled**
- No feature flags needed for optimal performance
- Just `cargo build --release` gives maximum performance
- Zero complexity for users - they get the best performance automatically

This was intentional to avoid the complexity of feature flag combinations and ensure consistent, predictable performance in production.

## Documentation

### Workspace-Level Docs (`docs/`)
- **[README.md](docs/README.md)** - Full documentation index
- **[ARCHITECTURE_EXPLANATION.md](docs/ARCHITECTURE_EXPLANATION.md)** - Python bindings architecture
- **[TESTING_STRATEGY.md](docs/TESTING_STRATEGY.md)** - Workspace testing strategy
- **[CRATES_OVERVIEW.md](docs/CRATES_OVERVIEW.md)** - All crates overview

### Crate-Specific Docs
- **[python-bindings](crates/python-bindings/docs/)** - Python API contract, base64 SDP rules
- **[keeper-pam-webrtc-rs](crates/keeper-pam-webrtc-rs/docs/)** - WebRTC core (registry, isolation, performance)
- **[guacr](crates/guacr/docs/)** - Protocol handlers overview, guides, reference
- **[guacr-protocol](crates/guacr-protocol/docs/)** - Guacamole protocol codec
- **[guacr-terminal](crates/guacr-terminal/docs/)** - Terminal emulation
- **[guacr-ssh](crates/guacr-ssh/docs/)** - SSH handler (production-ready)
- **[guacr-rdp](crates/guacr-rdp/docs/)** - RDP handler (production-ready)
- **[guacr-vnc](crates/guacr-vnc/docs/)** - VNC handler
- **[guacr-threat-detection](crates/guacr-threat-detection/docs/)** - AI threat detection

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

3. **Implement registration function** (if Python bindings needed):
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

## When Making a Pull Request

1. **Run all tests**: `cargo test --workspace && python3 -m pytest crates/python-bindings/tests/`
2. **Check clippy**: `cargo clippy --workspace -- -D warnings` must pass clean
3. **Run stress tests locally**: `python3 crates/python-bindings/tests/manual_stress_tests.py --light`
4. **Update documentation**: If adding features, update relevant docs
5. **Performance check**: If touching hot paths, benchmark before/after
6. **Verify no zombies**: Check that RAII cleanup still works correctly

## Common Tasks

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

### Update Python imports in tests
```bash
# Already done during migration, but if needed:
cd crates/python-bindings/tests
find . -name "*.py" -exec sed -i '' 's/keeper_pam_webrtc_rs/keeper_pam_connections/g' {} +
```

## Summary

This is a **workspace monorepo** with:
- ✅ Multiple Rust crates (extensible)
- ✅ Unified Python package (`keeper_pam_connections`)
- ✅ Workspace-wide quality checks
- ✅ Clean architecture with separation of concerns
- ✅ Production-ready WebRTC core and protocol handlers
- ✅ Sub-microsecond hot paths with SIMD optimization
- ✅ Cross-platform support (Windows, Linux, macOS)

---

**Last Updated**: January 29, 2026
**Maintainers**: Keeper Security Engineering <engineering@keeper.io>
