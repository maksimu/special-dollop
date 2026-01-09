# Testing Guide

## Warning Assessment

### Low Priority (Can Ignore for Now)
- **Unused imports**: `AsyncReadExt`, `AsyncWriteExt`, `OpenFlags` - These are kept for future use or were needed during development
- **Unused variables**: `session_id`, `threat_detector` in some contexts - May be needed for future features
- **Unused fields**: `asciicast_transports`, `guacamole_transports` - Part of recording infrastructure, may be used later

### Medium Priority (Clean Up When Convenient)
- **Unused function**: `x11_keysym_to_bytes_legacy` - Legacy code, can remove if not needed
- **Mutable variables**: Some variables don't need to be mutable - Easy fix with `cargo fix`

### Action Items
```bash
# Auto-fix simple warnings
cargo fix --workspace --allow-dirty

# Or manually clean up:
# 1. Remove unused imports
# 2. Prefix unused variables with `_` (e.g., `_session_id`)
# 3. Remove truly unused code
```

**Recommendation**: Warnings are non-critical. Focus on testing first, then clean up warnings in a separate pass.

## Test Strategy

### 1. Unit Tests (High Priority)

#### ChannelStreamAdapter Tests
**Location**: `crates/guacr-sftp/src/channel_adapter.rs`

**Critical Tests Needed**:
- ✅ Read buffering (partial reads)
- ✅ EOF handling
- ✅ Write channel forwarding
- ✅ Concurrent read/write
- ✅ Error propagation

#### SFTP Handler Tests
**Location**: `crates/guacr-sftp/src/handler.rs`

**Critical Tests Needed**:
- ✅ Path validation (chroot enforcement)
- ✅ File size limit enforcement
- ✅ Directory traversal prevention
- ✅ Home directory detection
- ✅ FileEntry conversion from DirEntry

#### Telnet Handler Tests
**Location**: `crates/guacr-telnet/src/handler.rs`

**Critical Tests Needed**:
- ✅ Scrollback buffer (add/retrieve lines)
- ✅ Mouse event X11 sequence generation
- ✅ ModifierState tracking
- ✅ Dirty region tracking integration
- ✅ Threat detection integration

#### Recording Validation Tests
**Location**: `crates/guacr-terminal/tests/recording_validation.rs`

**Tests Implemented** (12 tests, all passing):
- ✅ Asciicast v2 format compliance (JSON validation)
- ✅ Unicode handling (Chinese, Russian, Arabic, emoji)
- ✅ Short session recording (single-line sessions)
- ✅ Guacamole .ses format (protocol instructions)
- ✅ Dual format recording (both .cast and .ses)
- ✅ Terminal resize events
- ✅ Input recording (keyboard events)
- ✅ Multiple output events (sequential)
- ✅ Large output (1000+ lines)
- ✅ Timing accuracy (monotonic timestamps)
- ✅ Binary data handling (non-UTF8)
- ✅ Empty recording (zero-event sessions)

**Cross-Platform**: Uses `tempfile` crate for portable temporary file handling (Windows, Linux, macOS, BSD)

### 2. Integration Tests (Medium Priority)

#### SFTP Integration Tests
**Location**: `crates/guacr-sftp/tests/integration_test.rs` (NEW)

**Tests Needed**:
- ✅ End-to-end SFTP connection
- ✅ Directory listing
- ✅ File upload/download
- ✅ Error handling (network failures, permission errors)
- ✅ Concurrent file operations

**Requirements**:
- Mock SFTP server or use testcontainers
- Test with actual russh-sftp API

#### Telnet Integration Tests
**Location**: `crates/guacr-telnet/tests/integration_test.rs` (NEW)

**Tests Needed**:
- ✅ End-to-end Telnet connection
- ✅ Scrollback buffer behavior
- ✅ Mouse event forwarding
- ✅ Threat detection integration
- ✅ Rendering optimizations

### 3. Property-Based Tests (Nice to Have)

Use `proptest` or `quickcheck` for:
- Path validation edge cases
- File size limit boundaries
- Directory traversal attempts
- X11 mouse sequence generation

### 4. Manual Testing Checklist

#### SFTP Handler
- [ ] Connect to real SFTP server
- [ ] Navigate directories (up/down)
- [ ] Upload small file (< 1MB)
- [ ] Upload large file (> 10MB)
- [ ] Download file
- [ ] Test path traversal attempts (should fail)
- [ ] Test file size limit (should reject)
- [ ] Test chroot enforcement (can't access parent directories)

#### Telnet Handler
- [ ] Connect to real Telnet server
- [ ] Test scrollback (scroll up, verify history)
- [ ] Test mouse events (click in vim/tmux)
- [ ] Test control characters (Ctrl+C, Ctrl+D)
- [ ] Test threat detection (malicious command should terminate)
- [ ] Verify rendering performance (60fps)

## Running Tests

### Quick Commands (Makefile)

```bash
# Unit tests only (fast, no Docker)
make test

# Full integration tests (requires Docker)
make test-full

# Protocol-specific
make test-ssh
make test-rdp
make test-vnc
make test-db

# Docker management
make docker-up      # Start test servers
make docker-down    # Stop test servers
make docker-logs    # View logs
```

### Manual Commands

```bash
# All tests
cargo test --workspace

# Specific crate
cargo test -p guacr-sftp
cargo test -p guacr-telnet
cargo test -p guacr-terminal

# Integration tests (requires Docker servers)
cargo test -p guacr-ssh --test integration_test -- --include-ignored

# With output
cargo test -- --nocapture

# Coverage (requires cargo-tarpaulin)
cargo install cargo-tarpaulin
cargo tarpaulin --workspace
```

### Integration Tests with Docker

Start test servers:
```bash
make docker-up
```

Test servers run on non-standard ports:
- SSH: localhost:2222 (test_user/test_password)
- RDP: localhost:3389 (test_user/test_password)
- VNC: localhost:5901 (password: test_password)
- Telnet: localhost:2323 (test_user/test_password)
- MySQL: localhost:13306 (testuser/testpassword)
- PostgreSQL: localhost:15432 (testuser/testpassword)
- MongoDB: localhost:17017 (testuser/testpassword)
- Redis: localhost:16379 (password: testpassword)

Protocol-specific tests:
```bash
make test-ssh              # SSH integration tests
make test-sftp             # SFTP integration tests (uses SSH server)
make test-rdp              # RDP integration tests
make test-vnc              # VNC integration tests
make test-telnet           # Telnet integration tests
make test-db               # Database integration tests
make test-rbi              # RBI integration tests (requires Chrome)
make test-threat-detection # Threat detection tests (mock BAML API)
make test-performance      # Performance/load tests
make test-security         # Security tests
```

Manual testing:
```bash
make dev-ssh    # Shows connection info
ssh -p 2222 test_user@localhost
```

### Cross-Platform Testing

All tests use cross-platform abstractions:
- **Temporary files**: `tempfile` crate (works on Windows, Linux, macOS, BSD)
- **Path handling**: `std::path::Path` with platform-agnostic separators
- **File I/O**: Standard library with platform-specific implementations

**Tested on**:
- macOS (primary development)
- Linux (CI/production)
- Windows (via cross-compilation)
- FreeBSD (community tested)

## Test Coverage Goals

- **Unit Tests**: >80% coverage for new code
- **Integration Tests**: Cover all critical paths
- **Error Paths**: Test all error conditions
- **Edge Cases**: Boundary conditions, empty inputs, etc.

## Continuous Integration

Tests should run in CI:
- On every PR
- Before merging to main
- On release builds

## Performance Testing

For performance-critical code:
```bash
cargo bench --workspace
```

Key metrics:
- ChannelStreamAdapter throughput
- SFTP file transfer speed
- Telnet rendering latency
- Memory usage under load
