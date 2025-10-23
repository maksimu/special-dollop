# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`keeper-pam-webrtc-rs` is a **Rust-based WebRTC tube abstraction with Python bindings** for secure tunneling. It's designed for Keeper Security's mission-critical applications with a focus on **security, stability, and performance**.

- **Primary Language**: Rust (library) with Python bindings (abi3-py37+)
- **Core Purpose**: Secure WebRTC-based tunneling via "Tube API" abstraction
- **Target Users**: Internal Keeper Security products (Gateway, Commander)
- **Conversation Types**: `tunnel` (TCP), `guacd` (Apache Guacamole), `socks5` (SOCKS5 proxy)

## Build and Test Commands

### Standard Development Workflow

```bash
# Standard build (all optimizations enabled by default)
cargo build --release

# Quick development checks
cargo check
cargo clippy -- -D warnings

# Run comprehensive test suite
cargo test --release

# Run Python tests (requires Python bindings)
python3 -m pytest tests/test_*.py -v

# Run specific test by name
cargo test test_name -- --nocapture
```

### Debug Builds and Logging

```bash
# Production with debug logging enabled
cargo build --release --features production_debug

# Maximum performance (disable hot path logging)
cargo build --release --features disable_hot_path_logging

# Development with profiling instrumentation
cargo build --features profiling
```

### Running Tests

```bash
# All Rust tests
cargo test

# Rust tests without default features (no Python)
cargo test --no-default-features

# Run tests with logging output
RUST_LOG=debug cargo test test_tube_p2p_data_transfer_end_to_end -- --nocapture

# Run Python integration tests (after building)
python3 -m pytest tests/

# Manual stress tests (NOT for CI - run locally only)
python3 tests/manual_stress_tests.py --light
```

### Important Test Patterns

- **CI tests** (`tests/test_*.py`): Fast, deterministic, <5 minutes runtime
- **Manual stress tests** (`tests/manual_stress_tests.py`): Resource-intensive, run locally before major commits
- **Never run stress tests in CI** - they intentionally stress the system and have higher error tolerances

## High-Level Architecture

### Three-Layer System Design

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

### Key Architectural Principles

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

3. **RAII Resource Management** (`src/tube.rs:1740-1845`)
   - Tube owns all resources (signal_sender, metrics_handle, keepalive_task)
   - `Tube::drop()` automatically cleans up everything
   - Impossible to leak resources (compiler-enforced)
   - No manual cleanup needed

4. **Failure Isolation** (`src/webrtc_core.rs`)
   - IsolatedWebRTCAPI per tube (no cross-contamination)
   - TubeCircuitBreaker prevents cascading failures
   - Shared tokio runtime with automatic panic isolation
   - One tube failure NEVER affects others

5. **Always-Optimized Hot Paths** (`src/channel/frame_handling.rs`, `src/channel/connections.rs`)
   - SIMD optimizations always enabled (auto-detected)
   - Event-driven backpressure (zero polling)
   - UTF-8 character counting for Guacamole protocol compliance
   - Sub-microsecond frame processing (production-verified)

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

See `docs/PYTHON_API_CONTRACT.md` for complete API reference.

### Trickle ICE Requirement for ICE Restart

**CRITICAL**: ICE restart functionality **requires** `trickle_ice=True`.

```python
# ✅ CORRECT: ICE restart enabled
tube_info = registry.create_tube(
    trickle_ice=True,  # Required for ICE restart
    # ...
)

# ❌ WRONG: ICE restart disabled
tube_info = registry.create_tube(
    trickle_ice=False,  # No ICE restart!
    # ...
)
```

### Logging System (Unified as of Oct 2025)

The codebase uses a **unified logging approach** with minimal overhead:

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

**Key Points**:
- Backend: `tracing-subscriber` (bridges to Python logging)
- API: Pure `log` crate for all logging calls
- Hot path guard: `unlikely!(crate::logger::is_verbose_logging())`
- Single atomic flag: `VERBOSE_LOGGING` (controlled by Python)

### Performance-Critical Patterns

**NEVER do these in hot paths:**

```rust
// ❌ WRONG: Mutex locks in hot paths
let depth = self.queue.lock().unwrap().len();  // 50-100ns

// ✅ CORRECT: Atomic counters
let depth = self.queue_size.load(Ordering::Acquire);  // ~1ns
```

```rust
// ❌ WRONG: Always-evaluated logging
debug!("Processing frame: {:?}", expensive_debug_format());

// ✅ CORRECT: Guarded logging
if unlikely!(crate::logger::is_verbose_logging()) {
    debug!("Processing frame: {:?}", expensive_debug_format());
}
```

```rust
// ❌ WRONG: Without TCP_NODELAY, Nagle's algorithm batches data
stream.set_nodelay(false)?;  // Nagle enabled = buffering
backend.write_all(payload).await?;
// User types "h", Nagle batches it, appears on next keystroke

// ✅ CORRECT: TCP_NODELAY ensures immediate send (no flush needed)
stream.set_nodelay(true)?;  // Disable Nagle's algorithm (see connections.rs:87)
// ... later in write path ...
backend.write_all(payload).await?;  // Data sent immediately
// flush() is redundant - TCP with TCP_NODELAY is already unbuffered
// Explicit flush hurts Windows performance (16ms timer granularity overhead)
```

## Code Organization

### Core Files (Read These First)

- **`src/lib.rs`**: Entry point, module declarations
- **`src/tube_registry.rs`**: Registry actor, tube management, backpressure
- **`src/tube.rs`**: Tube struct, RAII Drop implementation, lifecycle
- **`src/webrtc_core.rs`**: IsolatedWebRTCAPI, peer connection management
- **`src/python/tube_registry_binding.rs`**: PyO3 Python bindings

### Channel System (Hot Paths)

- **`src/channel/core.rs`**: Channel creation and initialization
- **`src/channel/frame_handling.rs`**: SIMD frame parsing, routing (HOT PATH)
- **`src/channel/connections.rs`**: TCP reads, backpressure, batching (HOT PATH)
- **`src/channel/protocol.rs`**: Guacamole protocol messages
- **`src/channel/server.rs`**: SOCKS5 server, TCP tunnel mode
- **`src/channel/socks5.rs`**: SOCKS5 client, TCP CONNECT handling
- **`src/channel/udp.rs`**: SOCKS5 UDP ASSOCIATE implementation

### Supporting Infrastructure

- **`src/buffer_pool.rs`**: Thread-local buffer pools (lock-free)
- **`src/webrtc_data_channel.rs`**: Event-driven backpressure system
- **`src/webrtc_circuit_breaker.rs`**: Circuit breaker for failure isolation
- **`src/metrics/`**: Metrics collection and monitoring
- **`src/logger.rs`**: Unified logging system with verbose flag

### Test Organization

- **`src/tests/`**: Rust unit tests (fast, deterministic)
- **`tests/test_*.py`**: Python CI tests (fast, deterministic)
- **`tests/manual_stress_tests.py`**: Manual stress tests (NOT for CI)

## Important Development Patterns

### When Making Changes to Hot Paths

1. **Profile first** - Use `--features profiling` and measure impact
2. **Check logging** - Ensure logs are guarded with `unlikely!(is_verbose_logging())`
3. **Avoid locks** - Use atomics or thread-local storage
4. **Preserve SIMD** - Don't break existing SIMD optimizations
5. **Test performance** - Run benchmarks from `docs/PERFORMANCE_BENCHMARKS.md`

### When Modifying the Registry

1. **Use the actor** - Don't bypass RegistryActor for coordination
2. **Preserve RAII** - Resources must be owned by Tube for automatic cleanup
3. **Test backpressure** - Verify system rejects gracefully when overloaded
4. **Check for leaks** - Ensure no resource leaks (RAII makes this automatic)

### When Changing WebRTC Layer

1. **Maintain isolation** - Each tube has its own IsolatedWebRTCAPI
2. **Use circuit breaker** - Wrap risky operations with circuit breaker
3. **Test failure isolation** - One tube failure must not affect others
4. **Check TURN cleanup** - Verify TURN clients don't corrupt each other

### When Adding Python APIs

1. **Base64 encode SDP** - ALL SDP must be base64 over FFI boundary
2. **Document return values** - Specify all returned fields clearly
3. **Handle errors** - Convert Rust errors to Python exceptions properly
4. **Test with pytest** - Add tests to `tests/` directory

## Configuration and Environment Variables

```bash
# Registry configuration
export WEBRTC_MAX_CONCURRENT_CREATES=100  # Max concurrent tube creates

# NAT keepalive (defined in code, not configurable)
# ice_keepalive_interval: 60 seconds (changed from 300s to fix NAT timeout)

# Runtime checks in code
RUST_LOG=debug  # Enable debug logging for troubleshooting
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
**Cause**: Missing `flush()` in backend write path
**Solution**: Always call `backend.flush().await?` after `write_all()` for interactive data

### Issue: High CPU usage with many connections
**Cause**: Event-driven backpressure should prevent this
**Solution**: Check if you're polling instead of using WebRTC native events

## Architecture Documentation

For deep dives into specific subsystems, see:

- **`docs/ACTOR_DASHMAP_RAII.md`**: Registry concurrency architecture, RAII patterns
- **`docs/FAILURE_ISOLATION_ARCHITECTURE.md`**: WebRTC isolation, circuit breakers
- **`docs/HOT_PATH_OPTIMIZATION_SUMMARY.md`**: Performance optimizations, SIMD details
- **`docs/PYTHON_API_CONTRACT.md`**: Python API reference, base64 encoding rules
- **`docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md`**: ICE restart, network recovery
- **`docs/TESTING_STRATEGY.md`**: CI vs manual testing, quality standards
- **`docs/UDP_SOCKS5_IMPLEMENTATION.md`**: SOCKS5 protocol, UDP ASSOCIATE
- **`docs/WEBSOCKET_INTEGRATION.md`**: WebSocket integration patterns

## Key Metrics and Performance Targets

**Frame Processing (Production Verified)**:
- Small frames (0-64B): 398-477ns parse/encode → 2.5M frames/sec
- Medium frames (1.5KB): 430-490ns parse/encode → 2.3M frames/sec
- Large frames (8KB): 513-580ns parse/encode → 1.9M frames/sec
- Max UDP (64KB): 1428-2213ns parse/encode → 700K frames/sec

**UTF-8 Character Processing**:
- ASCII: 371ns per instruction (baseline)
- European languages: 623-630ns per instruction
- CJK languages: 490-599ns per instruction
- Mixed UTF-8: 603ns per instruction

**System Capacity**:
- Max concurrent tube creates: 100 (configurable)
- ICE keepalive interval: 60 seconds (prevents NAT timeout)
- Queue depth monitoring: ~1ns (atomic read)
- Memory per tube: ~52KB (~4% increase from isolation)

## Design Principles Summary

1. **Security First**: Memory-safe Rust, comprehensive bounds checking, no unsafe code in hot paths (except vetted SIMD)
2. **Enterprise Stability**: Lock-free architecture, RAII cleanup, failure isolation between tubes
3. **Optimized Performance**: SIMD frame parsing, event-driven backpressure, sub-microsecond hot paths
4. **Zero Configuration**: All optimizations enabled by default, no feature flags needed for performance
5. **Production Ready**: Comprehensive error handling, graceful degradation, extensive testing

## When Making a Pull Request

1. **Run all tests**: `cargo test && python3 -m pytest tests/`
2. **Check clippy**: `cargo clippy -- -D warnings` must pass clean
3. **Run stress tests locally**: `python3 tests/manual_stress_tests.py --light`
4. **Update documentation**: If adding features, update relevant docs in `docs/`
5. **Performance check**: If touching hot paths, benchmark before/after
6. **Verify no zombies**: Check that RAII cleanup still works correctly

## Understanding the "Always Fast" Philosophy

This project follows an **"always fast"** design:
- All performance optimizations are **built-in and always enabled**
- No feature flags needed for optimal performance
- Just `cargo build --release` gives maximum performance
- Zero complexity for users - they get the best performance automatically

This was intentional to avoid the complexity of feature flag combinations and ensure consistent, predictable performance in production, because the main use case is 100s of developers editing 4k videos over these connections.

## Cross-Platform Performance Considerations

This codebase runs on **Windows, Linux (x86_64/ARM64), and macOS**. Always consider platform differences when adding I/O operations, timers, or async logic.

### 🚨 Critical Platform Differences

#### **1. Timer Granularity and tokio::time Operations**

| Platform | Timer Resolution | tokio::time Overhead | Impact |
|----------|-----------------|---------------------|---------|
| **Linux** | ~1-2μs (epoll) | ~500ns-2μs | ✅ Fast timeouts work well |
| **macOS** | ~1-2μs (kqueue) | ~500ns-2μs | ✅ Fast timeouts work well |
| **Windows** | **~15-16ms** (IOCP) | **~16ms+** | ❌ Avoid frequent timeouts! |

**Real-world example from commit d13974d**:
```rust
// ❌ BAD: This appears to be "50ms timeout" but Windows makes it ~65ms
let flush_result = tokio::time::timeout(
    Duration::from_millis(50),  // Requested: 50ms
    backend.flush(),
).await;  // Actual on Windows: ~65ms (15ms overage + setup cost)

// At 60 events/sec: 60 × 65ms = 3.9 seconds/min of timer overhead on Windows!
```

**Guidelines**:
- ⚠️ **Avoid `tokio::time::timeout()` in hot paths** - Windows timer granularity is 15-16ms
- ✅ Use OS-level timeouts (TCP socket timeouts) instead when possible
- ✅ If using `timeout()`, make durations ≥ 500ms to avoid proportionally high overhead on Windows
- ✅ Consider removing timeouts entirely if the underlying operation has its own timeout

#### **2. TCP/Socket Behavior Differences**

| Feature | Linux/macOS | Windows | Notes |
|---------|------------|---------|-------|
| `TCP_NODELAY` | Immediate send, flush() = no-op | Immediate send, but flush() may trigger IOCP notification | Always set `TCP_NODELAY` for interactive protocols |
| `flush()` on TcpStream | ~100-500ns (instant) | ~50-200μs (IOCP queue) | **10-400× slower on Windows!** |
| Delayed-ACK behavior | 40ms default | Different algorithm | Affects small packet performance |
| Socket buffer sizes | Larger defaults | Smaller defaults | May need explicit `SO_SNDBUF`/`SO_RCVBUF` tuning |

**Guidelines**:
- ✅ **Always set `TCP_NODELAY` for interactive protocols** (SSH, RDP, Guacd)
- ✅ **Never call `flush()` on TCP streams with `TCP_NODELAY`** - it's redundant and hurts Windows
- ✅ Test with realistic Windows network conditions (not just localhost)

#### **3. Async I/O Architecture Differences**

| Platform | I/O Model | Per-Operation Cost | Characteristics |
|----------|-----------|-------------------|----------------|
| **Linux** | epoll (readiness-based) | ~100-500ns | "Tell me when ready" - instant notification |
| **macOS** | kqueue (readiness-based) | ~100-500ns | Similar to epoll |
| **Windows** | IOCP (completion-based) | **~50-200μs** | "Tell me when done" - must wait for completion |

**From tokio research** (verified via WebSearch):
> "IOCP is completion-based instead of readiness-based, requiring more adaptation
> to bridge the two paradigms. Because Mio provides a readiness-based API similar
> to Linux epoll, many parts of the API can be one-to-one mappings on Linux, but
> require additional work on Windows."

**Guidelines**:
- ⚠️ Expect async operations to be **10-100× slower on Windows** for individual operations
- ✅ Batch operations when possible to amortize IOCP overhead
- ✅ Use event-driven patterns (WebRTC native events) instead of polling
- ✅ Profile on Windows early - "works fast on Linux" ≠ "works fast on Windows"

#### **4. DNS Resolution**

| Platform | Implementation | Typical Latency |
|----------|---------------|----------------|
| Linux | `getaddrinfo()` with fast caching | ~10-50ms |
| macOS | mDNSResponder with aggressive caching | ~5-30ms |
| Windows | DNS Client service | **~50-200ms** (2-4× slower!) |

**Impact on commit d13974d** (router timeout reduction):
```rust
// Changed from 30s → 5s timeout
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(5))  // Was 30s
    .build()?;

// Windows: DNS (200ms) + TLS handshake (100ms) + network (100ms) = 400ms baseline
// With 5s timeout, less margin for error → more circuit breaker opens on Windows
```

**Guidelines**:
- ✅ Use longer timeouts for HTTP clients that do DNS resolution (10s+ recommended)
- ✅ Consider DNS caching for frequently accessed hosts
- ✅ Test on Windows with real internet (not localhost)

#### **5. Console/Terminal I/O**

| Platform | Console Write Speed | UTF-8 Handling |
|----------|-------------------|----------------|
| Linux | ~10-50μs | Native |
| macOS | ~10-50μs | Native |
| Windows | **~500-2000μs** | UTF-16 conversion required (100× slower!) |

**Guidelines**:
- ⚠️ **Never use verbose logging on Windows with console output** - 100× slower
- ✅ Use file-based logging or disable verbose mode by default
- ✅ Windows console writes can block for milliseconds - use async logging

#### **6. Thread-Local Storage (TLS)**

| Platform | TLS Access Cost | Notes |
|----------|----------------|-------|
| Linux | ~1-2ns (`__thread`) | Compiled to direct memory access |
| macOS | ~1-2ns (`__thread`) | Similar to Linux |
| Windows | **~5-10ns** (Win32 TLS slots) | Requires Win32 API call |

**Guidelines**:
- ✅ Thread-local buffer pools are still fast on Windows (5-10ns is acceptable)
- ✅ Avoid excessive TLS lookups in tight loops (cache the reference)

### 📝 Pre-Commit Checklist for Cross-Platform Code

When adding **any** of these operations, test on Windows:

- [ ] `tokio::time::timeout()` - Does this need to be in a hot path?
- [ ] `tokio::time::sleep()` - Is timer granularity acceptable?
- [ ] `backend.flush()` - Is this actually necessary? (Hint: Not for TCP with `TCP_NODELAY`)
- [ ] HTTP clients with DNS - Is timeout long enough for Windows DNS?
- [ ] Console/debug output - Will this spam stderr on Windows?
- [ ] Frequent file I/O - Is buffering enabled?

### 🧪 Testing Guidelines

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

### 📚 Related Issues from History

**Commit d13974d (Oct 2025)**: Added `tokio::time::timeout` to flush operations
- ❌ Result: 10-20× performance degradation on Windows (16ms timer granularity)
- ✅ Fix: Remove timeout wrapper entirely (this commit)

**Commit 72a37b2 (Oct 2025)**: Added `TCP_NODELAY`
- ✅ Result: Improved interactive latency on all platforms
- 💡 Learning: This **already** fixed the keyboard lag - flush() became redundant

**Commit 7a1c007 (Oct 2025)**: Added explicit `flush()` call
- ⚠️ Result: Minor overhead on Linux (~5μs), major overhead on Windows (~100μs)
- 💡 Learning: flush() + TCP_NODELAY is redundant

### 🎯 Summary: Cross-Platform Performance Philosophy

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

**Remember**: The main use case is **100s of developers editing 4K videos over these connections**. A 10× slowdown on Windows is unacceptable.