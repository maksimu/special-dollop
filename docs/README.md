# Guacr Integration with keeper-pam-webrtc-rs

This directory contains the design documentation and integration plan for adding Guacr (remote desktop protocol handlers) to the keeper-pam-webrtc-rs project.

## What is Guacr?

Guacr is a set of protocol handlers (SSH, RDP, VNC, MySQL, PostgreSQL, SQL Server, RBI/HTTP) that work on top of the keeper-pam-webrtc-rs WebRTC infrastructure to provide browser-based remote desktop access.

**Instead of building a separate guacd daemon, we integrate protocol handlers into this existing WebRTC codebase.**

## Why Integrate Here?

1. **WebRTC transport already exists** - Production-proven, optimized
2. **Guacamole parser already exists** - `src/channel/guacd_parser.rs`
3. **Zero-copy architecture already done** - SIMD, buffer pools, lock-free
4. **Sub-10ms latency** - Better than traditional TCP-based approach
5. **Same philosophy** - Security, stability, performance

## Integration Strategy

### Current State
```
keeper-pam-webrtc-rs
├── WebRTC tube abstraction (production)
├── Guacamole parser (production)
├── Conversation types: tunnel, guacd, socks5
└── Python bindings for Gateway/Commander
```

### Target State
```
keeper-pam-webrtc-rs  (or rename to keeper-remote-rs)
├── WebRTC tube abstraction (existing)
├── Guacamole parser (existing)
├── Protocol handlers (NEW: SSH, RDP, VNC, DB, RBI)
├── Python bindings (existing, keep)
└── Browser WebRTC client (NEW)
```

## Documentation Structure

### Design Documents (docs/context/)

**Core Design:**
- **GUACR_ARCHITECTURE_PLAN.md** (78KB) - Complete technical architecture
- **GUACR_WEBRTC_INTEGRATION.md** (19KB) - How to integrate with this repo
- **GUACR_ZERO_COPY_OPTIMIZATION.md** (33KB) - Performance techniques
- **GUACR_PROTOCOL_HANDLERS_GUIDE.md** (30KB) - Handler implementation patterns
- **GUACR_QUICKSTART_GUIDE.md** (22KB) - Getting started
- **GUACR_EXECUTIVE_SUMMARY.md** (13KB) - Business case and ROI

**Total:** 176KB of comprehensive design documentation

### Reference Documents (docs/reference/original-guacd/)

Analysis of the original C guacd implementation:
- Architecture analysis (31KB)
- Database plugins analysis (66KB)
- RBI implementation analysis (70KB)
- File references (29KB)

**Total:** 186KB of reference documentation

### Supporting Files

- **README.md** - This file
- **PROJECT_SUMMARY.md** - Executive summary
- **CONTRIBUTING.md** - Development guidelines (NO EMOJIS policy)
- **CLAUDE.md** - Guidance for Claude Code instances
- **Cargo.toml** - Workspace configuration for guacr crates
- **config/guacr.example.toml** - Configuration reference
- **docker/** - Docker build and compose files
- **k8s/** - Kubernetes deployment manifests

## How to Start Implementation

### Step 1: Review Existing Code

Read these files in this repo to understand what's already built:
- `src/channel/guacd_parser.rs` - Guacamole protocol parser (ALREADY DONE!)
- `src/channel/connections.rs` - Connection handling patterns
- `src/tube.rs` - Tube abstraction and lifecycle
- `CLAUDE.md` - Architecture overview of this codebase

### Step 2: Read Design Docs

Start here:
1. `docs/guacr/docs/context/GUACR_WEBRTC_INTEGRATION.md` - Integration strategy
2. `docs/guacr/docs/context/GUACR_ARCHITECTURE_PLAN.md` - Full architecture
3. `docs/guacr/docs/context/GUACR_PROTOCOL_HANDLERS_GUIDE.md` - How to build handlers

### Step 3: Implement Protocol Handlers

Follow the plan in GUACR_WEBRTC_INTEGRATION.md:
- Restructure this repo into workspace
- Add guacr-handlers crate (trait + registry)
- Implement SSH handler first (proof of concept)
- Add remaining handlers (RDP, VNC, DB, RBI)

## Key Integration Points

### Where Guacamole Support Already Exists

**`src/channel/connections.rs`**:
```rust
ConversationType::Guacd => {
    handle_guacd_conversation(channel, settings).await
}
```

**THIS is where protocol handlers will plug in.**

### What Needs to be Added

1. **Protocol Handler System** - New crates for SSH, RDP, VNC, etc.
2. **Handler Registry** - Route Guacamole protocol to correct handler
3. **Browser Client** - WebRTC client to replace guacamole-client

### What Already Works

1. **WebRTC infrastructure** - Signaling, ICE, data channels
2. **Guacamole parser** - Zero-copy, SIMD-optimized
3. **Buffer management** - Thread-local pools
4. **Metrics** - Already instrumented
5. **Error handling** - Circuit breakers, failure isolation

## Timeline

**Phase 1 (Weeks 1-2):** Restructure repo, add handler trait
**Phase 2 (Weeks 3-4):** SSH handler proof-of-concept
**Phase 3 (Weeks 5-8):** All protocol handlers
**Phase 4 (Weeks 9-12):** Browser client, production ready
**Phase 5 (Weeks 13-16):** Advanced features, migration

**Total:** 12-16 weeks (vs 28 weeks building from scratch)

## Performance Targets

With WebRTC transport + protocol handlers:
- **Latency:** 5-15ms (vs 20-50ms with TCP)
- **Throughput:** 1-2 Gbps per connection
- **Connections:** 10,000+ per instance
- **CPU:** 10-20% per connection
- **Memory:** 5-10 MB per connection

## Next Steps

1. Review `docs/context/GUACR_WEBRTC_INTEGRATION.md` for detailed integration plan
2. Restructure this repo into workspace
3. Start with SSH handler implementation
4. Build on existing WebRTC foundation

---

**The code goes HERE. The design is READY. Let's build it.**
