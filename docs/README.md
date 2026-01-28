# Documentation Index

This directory contains workspace-level documentation that applies to the entire `keeper-pam-connections` project.

## 📚 Documentation Structure

### Workspace-Level Docs (this directory)

**User-Facing Documentation:**
- **[ARCHITECTURE_EXPLANATION.md](ARCHITECTURE_EXPLANATION.md)** - Python bindings architecture
- **[TESTING_STRATEGY.md](TESTING_STRATEGY.md)** - Workspace testing strategy, CI vs manual tests

### Project-Specific Docs

**WebRTC** ([webrtc/](webrtc/)):
- **[PYTHON_API_CONTRACT.md](webrtc/PYTHON_API_CONTRACT.md)** - Python API reference, base64 SDP encoding rules
- **[ACTOR_DASHMAP_RAII.md](webrtc/ACTOR_DASHMAP_RAII.md)** - Registry concurrency architecture
- **[FAILURE_ISOLATION_ARCHITECTURE.md](webrtc/FAILURE_ISOLATION_ARCHITECTURE.md)** - WebRTC isolation, circuit breakers
- **[HOT_PATH_OPTIMIZATION_SUMMARY.md](webrtc/HOT_PATH_OPTIMIZATION_SUMMARY.md)** - Performance optimizations, SIMD details
- **[PERFORMANCE_BENCHMARKS.md](webrtc/PERFORMANCE_BENCHMARKS.md)** - Benchmark results and methodology

**guacr Protocol Handlers** ([guacr/](guacr/)):
- **[README.md](guacr/README.md)** - guacr documentation hub
- **[CLAUDE.md](guacr/CLAUDE.md)** - Development guidance for protocol handlers
- **[concepts/](guacr/concepts/)** - Core concepts (Guacamole protocol, zero-copy, threat detection)
- **[crates/](guacr/crates/)** - Per-crate documentation (SSH, RDP, VNC, etc.)
- **[guides/](guacr/guides/)** - How-to guides (integration, testing, performance)
- **[reference/](guacr/reference/)** - Reference documentation (original guacd analysis)

### Crate-Specific Docs

**keeper-pam-webrtc-rs** ([crates/keeper-pam-webrtc-rs/docs/](../crates/keeper-pam-webrtc-rs/docs/)):
- **ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md** - ICE restart implementation details
- **UDP_SOCKS5_IMPLEMENTATION.md** - SOCKS5 UDP ASSOCIATE protocol details
- **WEBSOCKET_INTEGRATION.md** - WebSocket integration patterns
- **ENHANCED_METRICS_SUMMARY.md** - Metrics collection implementation

## 🎯 Quick Links by Topic

### For Python Users
1. Start here: [Python API Contract](webrtc/PYTHON_API_CONTRACT.md)
2. Understanding architecture: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
3. Testing your code: [Testing Strategy](TESTING_STRATEGY.md)

### For Rust Developers - WebRTC
1. Architecture overview: [Actor + DashMap + RAII](webrtc/ACTOR_DASHMAP_RAII.md)
2. Performance details: [Hot Path Optimizations](webrtc/HOT_PATH_OPTIMIZATION_SUMMARY.md)
3. Failure handling: [Failure Isolation](webrtc/FAILURE_ISOLATION_ARCHITECTURE.md)
4. Benchmarks: [Performance Benchmarks](webrtc/PERFORMANCE_BENCHMARKS.md)

### For Rust Developers - Protocol Handlers
1. Start here: [guacr README](guacr/README.md)
2. Development guide: [guacr CLAUDE.md](guacr/CLAUDE.md)
3. SSH handler: [guacr-ssh](guacr/crates/guacr-ssh.md)
4. RDP handler: [guacr-rdp](guacr/crates/guacr-rdp.md)
5. VNC handler: [guacr-vnc](guacr/crates/guacr-vnc.md)

### For Contributors
1. Workspace architecture: [Architecture Explanation](ARCHITECTURE_EXPLANATION.md)
2. Testing approach: [Testing Strategy](TESTING_STRATEGY.md)
3. Build system: [../CLAUDE.md](../CLAUDE.md)

### For Protocol Implementers
1. ICE restart: [crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md](../crates/keeper-pam-webrtc-rs/docs/ICE_RESTART_TRICKLE_INTEGRATION_GUIDE.md)
2. SOCKS5 UDP: [crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md](../crates/keeper-pam-webrtc-rs/docs/UDP_SOCKS5_IMPLEMENTATION.md)
3. WebSocket: [crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md](../crates/keeper-pam-webrtc-rs/docs/WEBSOCKET_INTEGRATION.md)

## 📖 Documentation Philosophy

### Workspace-Level Docs (`docs/`)
Documentation that applies to the **entire project**:
- Monorepo architecture and structure
- Workspace-wide testing strategy
- Cross-project integration patterns

### Project-Level Docs (`docs/webrtc/`, `docs/guacr/`)
Documentation specific to one major component:
- **WebRTC**: Python API, concurrency, performance, benchmarks
- **guacr**: Protocol handlers, Guacamole protocol, implementation guides

### Crate-Level Docs (`crates/<crate-name>/docs/`)
Documentation specific to **one crate's implementation**:
- Protocol-specific details (ICE, SOCKS5, WebSocket)
- Crate-internal architecture
- Implementation-specific metrics

## 🔄 Adding New Documentation

### Workspace-Level (`docs/`)
- Affects multiple projects (WebRTC + guacr)
- Workspace-wide patterns and decisions
- Cross-project architecture

### Project-Level (`docs/webrtc/` or `docs/guacr/`)
- Specific to WebRTC or guacr
- Project-wide architecture and design
- User-facing documentation for that project

### Crate-Level (`crates/<crate-name>/docs/`)
- Specific to one crate's implementation
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
