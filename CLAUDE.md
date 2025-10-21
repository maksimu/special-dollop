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
// ❌ WRONG: Removed flush causes keyboard lag
backend.write_all(payload).await?;
// User types "h", nothing appears until next keystroke

// ✅ CORRECT: Flush for interactive latency
backend.write_all(payload).await?;
backend.flush().await?;  // Instant response
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