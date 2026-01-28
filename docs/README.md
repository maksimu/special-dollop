# Documentation Index

This directory contains workspace-level documentation that applies to the entire `keeper-pam-connections` project.

## 📚 Documentation Structure

### Workspace-Level Docs (this directory)

**User-Facing Documentation:**
- **[PYTHON_API_CONTRACT.md](PYTHON_API_CONTRACT.md)** - Python API reference, base64 SDP encoding rules
- **[TESTING_STRATEGY.md](TESTING_STRATEGY.md)** - Workspace testing strategy, CI vs manual tests
- **[ARCHITECTURE_EXPLANATION.md](ARCHITECTURE_EXPLANATION.md)** - Python bindings architecture

**Architecture & Design:**
- **[ACTOR_DASHMAP_RAII.md](ACTOR_DASHMAP_RAII.md)** - Registry concurrency architecture
- **[FAILURE_ISOLATION_ARCHITECTURE.md](FAILURE_ISOLATION_ARCHITECTURE.md)** - WebRTC isolation, circuit breakers
- **[HOT_PATH_OPTIMIZATION_SUMMARY.md](HOT_PATH_OPTIMIZATION_SUMMARY.md)** - Performance optimizations, SIMD details
- **[PERFORMANCE_BENCHMARKS.md](PERFORMANCE_BENCHMARKS.md)** - Benchmark results and methodology

### Crate-Specific Docs

**keeper-pam-webrtc-rs** ([crates/keeper-pam-webrtc-rs/docs/](../crates/keeper-pam-webrtc-rs/docs/)):
- **ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md** - ICE restart implementation details
- **UDP_SOCKS5_IMPLEMENTATION.md** - SOCKS5 UDP ASSOCIATE protocol details
- **WEBSOCKET_INTEGRATION.md** - WebSocket integration patterns
- **ENHANCED_METRICS_SUMMARY.md** - Metrics collection implementation

## 🎯 Quick Links by Topic

### For Python Users
1. Start here: [Python API Contract](PYTHON_API_CONTRACT.md)
2. Understanding architecture: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
3. Testing your code: [Testing Strategy](TESTING_STRATEGY.md)

### For Rust Developers
1. Architecture overview: [Actor + DashMap + RAII](ACTOR_DASHMAP_RAII.md)
2. Performance details: [Hot Path Optimizations](HOT_PATH_OPTIMIZATION_SUMMARY.md)
3. Failure handling: [Failure Isolation](FAILURE_ISOLATION_ARCHITECTURE.md)
4. Benchmarks: [Performance Benchmarks](PERFORMANCE_BENCHMARKS.md)

### For Contributors
1. Architecture overview: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
2. Testing approach: [Testing Strategy](TESTING_STRATEGY.md)
3. Build system: [../CLAUDE.md](../CLAUDE.md)

### For Protocol Implementers
1. ICE restart: [crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md](../crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md)
2. SOCKS5 UDP: [crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md](../crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md)
3. WebSocket: [crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md](../crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md)

## 📖 Documentation Philosophy

### Workspace-Level Docs
Documentation that applies to the **entire project** or **multiple crates**:
- Python API (affects all users)
- Architecture decisions (affects all crates)
- Performance characteristics (system-wide)
- Testing strategy (workspace-wide)

### Crate-Level Docs
Documentation specific to **one crate's implementation**:
- Protocol-specific details (ICE, SOCKS5, WebSocket)
- Crate-internal architecture
- Implementation-specific metrics

## 🔄 Adding New Documentation

### When to Add Workspace-Level Docs
- Affects Python users
- Describes cross-crate architecture
- Documents workspace-wide patterns
- Explains system-level performance

**Location**: `docs/` (this directory)

### When to Add Crate-Level Docs
- Specific to one crate's implementation
- Protocol-specific details
- Internal implementation notes
- Crate-specific benchmarks

**Location**: `crates/<crate-name>/docs/`

## 📝 Documentation Standards

All documentation should:
- ✅ Be written in Markdown
- ✅ Include a table of contents for long docs
- ✅ Use code examples where applicable
- ✅ Include "Why" explanations, not just "What"
- ✅ Reference related docs with relative links
- ✅ Be kept up-to-date with code changes

## 🔍 Finding Documentation

### By File Type
```bash
# All workspace-level docs
ls docs/

# All crate-specific docs
find crates -name "docs" -type d

# All markdown files
find . -name "*.md" | grep -v target | grep -v .git
```

### By Topic
Use this README's Quick Links section above, or search:
```bash
# Search all docs for a topic
grep -r "your-topic" docs/ crates/*/docs/
```

## 📚 External Resources

- **Cargo Workspace Documentation**: https://doc.rust-lang.org/cargo/reference/workspaces.html
- **PyO3 Guide**: https://pyo3.rs/
- **WebRTC Specification**: https://www.w3.org/TR/webrtc/
- **Keeper Security**: https://www.keepersecurity.com/

---

**Last Updated**: January 28, 2026  
**Maintainers**: Keeper Security Engineering <engineering@keeper.io>
