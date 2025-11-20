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

```bash
# All tests
cargo test --workspace

# Specific crate
cargo test -p guacr-sftp
cargo test -p guacr-telnet

# With output
cargo test -- --nocapture

# Integration tests only
cargo test --test '*'

# Coverage (requires cargo-tarpaulin)
cargo install cargo-tarpaulin
cargo tarpaulin --workspace
```

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
