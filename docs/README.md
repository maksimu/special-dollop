# Documentation Index

This directory contains workspace-level documentation that applies to the entire `keeper-pam-connections` project.

## 📚 Documentation Structure

### Workspace-Level Docs (this directory)

**User-Facing Documentation:**
- **[ARCHITECTURE_EXPLANATION.md](ARCHITECTURE_EXPLANATION.md)** - Python bindings architecture
- **[TESTING_STRATEGY.md](TESTING_STRATEGY.md)** - Workspace testing strategy, CI vs manual tests

### Crate-Specific Docs

**keeper-pam-webrtc-rs** ([crates/keeper-pam-webrtc-rs/docs/](../crates/keeper-pam-webrtc-rs/docs/)):
- **[PYTHON_API_CONTRACT.md](../crates/keeper-pam-webrtc-rs/docs/PYTHON_API_CONTRACT.md)** - Python API reference, base64 SDP encoding rules
- **[ACTOR_DASHMAP_RAII.md](../crates/keeper-pam-webrtc-rs/docs/ACTOR_DASHMAP_RAII.md)** - Registry concurrency architecture
- **[FAILURE_ISOLATION_ARCHITECTURE.md](../crates/keeper-pam-webrtc-rs/docs/FAILURE_ISOLATION_ARCHITECTURE.md)** - WebRTC isolation, circuit breakers
- **[HOT_PATH_OPTIMIZATION_SUMMARY.md](../crates/keeper-pam-webrtc-rs/docs/HOT_PATH_OPTIMIZATION_SUMMARY.md)** - Performance optimizations, SIMD details
- **[PERFORMANCE_BENCHMARKS.md](../crates/keeper-pam-webrtc-rs/docs/PERFORMANCE_BENCHMARKS.md)** - Benchmark results and methodology
- **[ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md](../crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md)** - ICE restart implementation details
- **[UDP_SOCKS5_IMPLEMENTATION.md](../crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md)** - SOCKS5 UDP ASSOCIATE protocol details
- **[WEBSOCKET_INTEGRATION.md](../crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md)** - WebSocket integration patterns
- **[ENHANCED_METRICS_SUMMARY.md](../crates/keeper-pam-webrtc-rs/docs/ENHANCED_METRICS_SUMMARY.md)** - Metrics collection implementation

**python-bindings** ([crates/python-bindings/docs/](../crates/python-bindings/docs/)):
- **[PYTHON_API_CONTRACT.md](../crates/python-bindings/docs/PYTHON_API_CONTRACT.md)** - Python API reference, base64 SDP encoding rules

**guacr Protocol Handlers**:
- **[guacr aggregator](../crates/guacr/docs/)** - Overview docs, guides, concepts, and reference
  - **[README.md](../crates/guacr/docs/README.md)** - guacr documentation hub
  - **[CLAUDE.md](../crates/guacr/docs/CLAUDE.md)** - Development guidance
  - **[concepts/](../crates/guacr/docs/concepts/)** - Zero-copy patterns
  - **[guides/](../crates/guacr/docs/guides/)** - Integration, testing, performance
  - **[reference/](../crates/guacr/docs/reference/)** - Original guacd analysis
- **[guacr-protocol](../crates/guacr-protocol/docs/)** - Guacamole protocol codec
- **[guacr-terminal](../crates/guacr-terminal/docs/)** - Terminal emulation and rendering
- **[guacr-ssh](../crates/guacr-ssh/docs/)** - SSH protocol handler (production-ready)
- **[guacr-rdp](../crates/guacr-rdp/docs/)** - RDP protocol handler (production-ready)
- **[guacr-vnc](../crates/guacr-vnc/docs/)** - VNC protocol handler
- **[guacr-threat-detection](../crates/guacr-threat-detection/docs/)** - AI threat detection

## 🎯 Quick Links by Topic

### For Python Users
1. Start here: [Python API Contract](../crates/python-bindings/docs/PYTHON_API_CONTRACT.md)
2. Understanding architecture: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
3. Testing your code: [Testing Strategy](TESTING_STRATEGY.md)

### For Rust Developers - WebRTC
1. Architecture overview: [Actor + DashMap + RAII](../crates/keeper-pam-webrtc-rs/docs/ACTOR_DASHMAP_RAII.md)
2. Performance details: [Hot Path Optimizations](../crates/keeper-pam-webrtc-rs/docs/HOT_PATH_OPTIMIZATION_SUMMARY.md)
3. Failure handling: [Failure Isolation](../crates/keeper-pam-webrtc-rs/docs/FAILURE_ISOLATION_ARCHITECTURE.md)
4. Benchmarks: [Performance Benchmarks](../crates/keeper-pam-webrtc-rs/docs/PERFORMANCE_BENCHMARKS.md)

### For Rust Developers - Protocol Handlers
1. Start here: [guacr README](../crates/guacr/docs/README.md)
2. Development guide: [guacr CLAUDE.md](../crates/guacr/docs/CLAUDE.md)
3. Guacamole protocol: [guacr-protocol docs](../crates/guacr-protocol/docs/)
4. Terminal emulation: [guacr-terminal docs](../crates/guacr-terminal/docs/)
5. SSH handler: [guacr-ssh docs](../crates/guacr-ssh/docs/)
6. RDP handler: [guacr-rdp docs](../crates/guacr-rdp/docs/)
7. VNC handler: [guacr-vnc docs](../crates/guacr-vnc/docs/)

### For Contributors
1. Workspace architecture: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
2. Testing approach: [Testing Strategy](TESTING_STRATEGY.md)
3. Build system: [../CLAUDE.md](../CLAUDE.md)

### For Protocol Implementers
1. ICE restart: [ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md](../crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md)
2. SOCKS5 UDP: [UDP_SOCKS5_IMPLEMENTATION.md](../crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md)
3. WebSocket: [WEBSOCKET_INTEGRATION.md](../crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md)

## 📖 Documentation Philosophy

### Workspace-Level Docs (`docs/`)
Documentation that applies to the **entire project**:
- Monorepo architecture and structure
- Workspace-wide testing strategy
- Cross-project integration patterns
- High-level crate overview

### Crate-Level Docs (`crates/<crate-name>/docs/`)
Documentation specific to **one crate's implementation**:
- **keeper-pam-webrtc-rs**: Python API, concurrency, performance, benchmarks, protocol details (ICE, SOCKS5, WebSocket)
- **guacr**: Protocol handlers, Guacamole protocol, implementation guides
- Crate-internal architecture
- Implementation-specific metrics and benchmarks

## 🔄 Adding New Documentation

### Workspace-Level (`docs/`)
- Affects multiple crates in the workspace
- Workspace-wide patterns and decisions
- Cross-crate architecture and integration
- Overview and navigation documents

### Crate-Level (`crates/<crate-name>/docs/`)
- Specific to one crate's implementation
- Crate-wide architecture and design
- User-facing documentation for that crate
- Internal implementation details
- Crate-specific benchmarks

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
