# Guacr + keeper-pam-webrtc-rs Integration Plan

## Executive Summary

**Major Discovery:** Keeper already has a production-ready Rust WebRTC implementation (`keeper-pam-webrtc-rs`) that:
- Already implements Guacamole protocol parsing (zero-copy)
- Has WebRTC tunneling with sub-10ms latency
- Uses SIMD optimization (398-2213ns frame processing)
- Supports `conversationType: "guacd"` specifically for Guacamole
- Built with same architecture: lock-free, buffer pools, zero-copy

**Integration Opportunity:** Instead of building guacr from scratch, integrate WebRTC support into the existing keeper-pam-webrtc-rs codebase, creating a unified high-performance solution.

---

## What keeper-pam-webrtc-rs Provides

### Already Implemented (Production Code)

**1. Guacamole Protocol Parser** (`src/channel/guacd_parser.rs`)
```rust
pub struct GuacdParser;

impl GuacdParser {
    pub fn peek_instruction(buffer: &[u8]) -> Result<PeekedInstruction, PeekError>;
    pub fn parse_instruction(content: &[u8]) -> Result<GuacdInstruction, GuacdParserError>;
}

pub struct GuacdInstruction {
    pub opcode: String,
    pub args: Vec<String>,
}
```

**Features:**
- Zero-copy peeking (borrows from buffer)
- SmallVec optimization (no heap for <4 args)
- Fast path for common opcodes (error, size, disconnect)
- SIMD-optimized frame parsing (398-2213ns per frame)

**2. WebRTC Tube Abstraction** (`src/tube.rs`)
- Full WebRTC peer connection management
- ICE candidate handling
- Data channel creation/management
- RAII cleanup (auto-cleanup on drop)
- Circuit breaker for failure isolation

**3. Conversation Types** (`src/channel/`)
- `tunnel` - Generic TCP tunneling
- `guacd` - Apache Guacamole protocol tunneling (ALREADY SUPPORTS THIS!)
- `socks5` - SOCKS5 proxy

**4. Performance Architecture**
- Lock-free DashMap for tube registry
- Thread-local buffer pools
- SIMD optimizations (always enabled)
- Actor model coordination (zero lock contention)
- io_uring support (Linux)

**5. Python Bindings** (`src/python/`)
- PyO3 bindings for Python integration
- Used by Keeper Gateway and Commander

---

## Integration Architecture

### Option 1: Guacr as WebRTC-First (RECOMMENDED)

Make guacr a **WebRTC-native** remote desktop proxy:

```
Browser (WebRTC)  ←→  Guacr (WebRTC + Rust)  ←→  RDP/VNC/SSH/etc.
     ↑                         ↑                         ↑
  Sub-10ms latency      keeper-pam-webrtc-rs      Protocol handlers
  No text encoding         Tube API                (from guacr design)
  Direct binary           Zero-copy
```

**Architecture:**
```rust
// Merge the two projects
guacr/
├── crates/
│   ├── guacr-daemon/           # Main daemon
│   │   └── Uses keeper-webrtc as core transport
│   │
│   ├── guacr-webrtc/           # Rename/adapt keeper-pam-webrtc-rs
│   │   ├── tube.rs             # Existing tube abstraction
│   │   ├── channel/            # Existing channel handling
│   │   │   └── guacd_parser.rs # Already has Guacamole parser!
│   │   └── webrtc_core.rs      # WebRTC implementation
│   │
│   ├── guacr-protocol/         # Thin wrapper over guacd_parser
│   │   └── Re-export GuacdParser + add codec traits
│   │
│   ├── guacr-handlers/         # Protocol handlers
│   │   └── Same as before (SSH, RDP, VNC, DB, RBI)
│   │
│   └── guacr-grpc/             # Keep for service mesh
│
└── Legacy Guacamole text protocol support (optional fallback)
```

**Benefits:**
- Reuse battle-tested WebRTC implementation
- Sub-10ms latency instead of 20-50ms
- Zero-copy already implemented
- SIMD optimizations already done
- No text encoding overhead
- Direct browser support (WebRTC is native)

**Changes Needed:**
1. Adapt tube API to work as guacr transport
2. Keep protocol handlers from guacr design
3. Add browser WebRTC client (JavaScript)
4. Remove Python bindings (or keep for compatibility)

### Option 2: Dual Transport (FLEXIBLE)

Support BOTH Guacamole text protocol AND WebRTC:

```rust
pub enum Transport {
    Guacamole(TcpStream),        // Text protocol (port 4822)
    WebRTC(Tube),                 // Binary over WebRTC (peer-to-peer)
    GRPC(GrpcStream),             // For service mesh
}

impl GuacrDaemon {
    async fn handle_connection(&self, transport: Transport) -> Result<()> {
        match transport {
            Transport::Guacamole(stream) => {
                // Use text codec (legacy)
                self.handle_guacamole_text(stream).await
            },
            Transport::WebRTC(tube) => {
                // Use keeper-pam-webrtc-rs (modern)
                self.handle_webrtc(tube).await
            },
            Transport::GRPC(stream) => {
                // Service mesh
                self.handle_grpc(stream).await
            }
        }
    }
}
```

**Benefits:**
- Backward compatible (keep text protocol)
- Modern clients use WebRTC
- Gradual migration path
- Best of both worlds

### Option 3: Fork keeper-pam-webrtc-rs into Guacr

**Full integration:**
1. Copy keeper-pam-webrtc-rs codebase
2. Remove Python bindings
3. Add protocol handlers (SSH, RDP, VNC, etc.)
4. Keep WebRTC as primary transport
5. Add Guacamole text protocol as fallback

---

## Detailed Integration Plan

### Phase 1: Proof of Concept (Week 1)

**Validate the integration:**

```rust
// In keeper-pam-webrtc-rs/src/channel/connections.rs
// The guacd conversation type already exists!

// Add protocol handlers to the existing channel handling
match conversation_type {
    ConversationType::Tunnel => {
        // Existing: Generic TCP tunnel
        handle_tunnel(channel, settings).await
    },
    ConversationType::Guacd => {
        // Existing: Guacamole protocol parsing
        // ENHANCE: Add actual protocol handlers here
        handle_guacd_with_handlers(channel, settings, handler_registry).await
    },
    ConversationType::Socks5 => {
        // Existing: SOCKS5 proxy
        handle_socks5(channel, settings).await
    }
}

async fn handle_guacd_with_handlers(
    channel: Arc<Channel>,
    settings: Settings,
    handlers: Arc<ProtocolHandlerRegistry>,
) -> Result<()> {
    // 1. Parse Guacamole handshake (already have parser!)
    let handshake = parse_guacd_handshake(&channel).await?;

    // 2. Get protocol handler (SSH, RDP, VNC, etc.)
    let handler = handlers.get(&handshake.protocol)?;

    // 3. Connect via handler
    handler.connect_webrtc(handshake.params, channel).await?;

    Ok(())
}
```

### Phase 2: Merge Codebases (Week 2-3)

**Directory structure:**
```
guacr/
├── Cargo.toml                    # Workspace
├── crates/
│   ├── guacr-daemon/             # Main binary
│   │   ├── Listens for WebRTC signaling
│   │   ├── Uses tube registry
│   │   └── Routes to protocol handlers
│   │
│   ├── guacr-webrtc/             # From keeper-pam-webrtc-rs
│   │   ├── tube.rs               # Tube abstraction
│   │   ├── tube_registry.rs      # Registry actor
│   │   ├── channel/              # Channel handling
│   │   │   ├── guacd_parser.rs   # Guacamole parser (keep!)
│   │   │   ├── connections.rs    # Connection logic
│   │   │   └── frame_handling.rs # SIMD frame parsing
│   │   ├── webrtc_core.rs        # WebRTC implementation
│   │   └── metrics/              # Metrics collection
│   │
│   ├── guacr-handlers/           # Protocol handler trait
│   ├── guacr-ssh/                # SSH handler
│   ├── guacr-rdp/                # RDP handler
│   ├── guacr-vnc/                # VNC handler
│   ├── guacr-database/           # Database handlers
│   └── guacr-rbi/                # Browser isolation
│
└── client/                       # JavaScript WebRTC client
    └── guacr-client.js           # Browser-based client
```

### Phase 3: Protocol Handlers (Week 4-8)

**Implement handlers to work with WebRTC channel:**

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    fn name(&self) -> &str;

    // Modified signature - takes WebRTC channel instead of queues
    async fn connect_webrtc(
        &self,
        params: HashMap<String, String>,
        channel: Arc<Channel>,  // From keeper-pam-webrtc-rs
    ) -> Result<()>;
}

// SSH handler using WebRTC channel
impl ProtocolHandler for SshHandler {
    async fn connect_webrtc(
        &self,
        params: HashMap<String, String>,
        channel: Arc<Channel>,
    ) -> Result<()> {
        // 1. Connect to SSH server
        let mut ssh_session = connect_ssh(&params).await?;

        // 2. Bidirectional forwarding (zero-copy via WebRTC)
        loop {
            tokio::select! {
                // SSH → WebRTC (use existing channel.send)
                data = ssh_session.read() => {
                    let terminal_output = terminal.process(data?)?;
                    let png = terminal.render_dirty()?;

                    // Send as Guacamole instruction over WebRTC
                    let instruction = format_guacd_img(png);
                    channel.send(instruction).await?;
                },

                // WebRTC → SSH (use existing channel.recv)
                msg = channel.recv() => {
                    let instruction = GuacdParser::parse_instruction(&msg?)?;

                    match instruction.opcode.as_str() {
                        "key" => ssh_session.send_key(&instruction.args).await?,
                        "size" => terminal.resize(&instruction.args)?,
                        _ => {}
                    }
                }
            }
        }
    }
}
```

---

## Why This Integration Makes Sense

### 1. Avoid Duplication

**Without integration:**
- Build Guacamole text protocol parser (duplicates guacd_parser.rs)
- Build WebRTC support from scratch
- Two separate codebases to maintain

**With integration:**
- Reuse existing production WebRTC code
- Reuse existing Guacamole parser
- Single unified codebase
- Add protocol handlers on top

### 2. Better Performance

**keeper-pam-webrtc-rs already has:**
- SIMD optimizations (398-2213ns frame processing)
- Lock-free architecture (DashMap, thread-local pools)
- Zero-copy frame handling
- Production-tested under load

**You'd be reimplementing all of this** if you build separate guacr.

### 3. WebRTC Advantages

**Guacamole text protocol over TCP:**
- 20-50ms latency
- Server must proxy all traffic
- Text encoding overhead
- No NAT traversal

**WebRTC data channels:**
- 2-10ms latency (peer-to-peer)
- Optional server relay (TURN)
- Binary protocol (no encoding)
- Built-in NAT traversal (STUN/TURN)
- Browser native

### 4. Architectural Alignment

**Both projects share:**
- Rust with Tokio async
- Zero-copy philosophy
- Lock-free data structures
- SIMD acceleration
- Buffer pooling
- Production focus (Keeper Security)

---

## Recommended Approach

### Use keeper-pam-webrtc-rs as the Foundation

**Week 1-2: Integration Planning**
1. Review keeper-pam-webrtc-rs codebase thoroughly
2. Identify integration points
3. Design ProtocolHandler trait to work with Channel API
4. Plan protocol handler implementation

**Week 3-4: Core Integration**
1. Add ProtocolHandlerRegistry to keeper-pam-webrtc-rs
2. Enhance `ConversationType::Guacd` handler
3. Implement SSH handler as proof-of-concept
4. Test end-to-end: Browser (WebRTC) → keeper-webrtc → SSH → Remote host

**Week 5-8: Additional Handlers**
1. Implement RDP, VNC handlers
2. Implement database handlers
3. Implement RBI handler
4. All using WebRTC as transport

**Week 9-12: Browser Client**
1. Build JavaScript WebRTC client
2. Replace guacamole-client web interface
3. Direct WebRTC to keeper-pam-webrtc-rs
4. Sub-10ms latency remote desktop

**Week 13+: Production**
1. Observability integration
2. Service mesh (gRPC between instances)
3. Kubernetes deployment
4. Migration from old guacd

### Architecture Comparison

**Original Plan (Build from Scratch):**
```
Browser → guacamole-client → guacr (TCP 4822, text protocol) → RDP/VNC/SSH
          (WebSocket)         (Rust, zero-copy)

Timeline: 28 weeks
Performance: 10x vs C guacd (20-50ms latency)
```

**New Plan (Integrate WebRTC):**
```
Browser (WebRTC) → keeper-pam-webrtc-rs + handlers → RDP/VNC/SSH
    ↑               (Rust, zero-copy, SIMD)
    Direct P2P, <10ms latency

Timeline: 12-16 weeks (reusing existing WebRTC)
Performance: 20-50x vs C guacd (<10ms latency)
```

---

## Integration Strategy

### Rename/Restructure keeper-pam-webrtc-rs

**Current focus:** Python bindings for PAM/Gateway
**New focus:** General-purpose remote desktop proxy

```
keeper-pam-webrtc-rs/
├── Cargo.toml
├── crates/
│   ├── keeper-webrtc-core/      # Rename from root src/
│   │   ├── tube.rs
│   │   ├── tube_registry.rs
│   │   ├── channel/
│   │   │   ├── guacd_parser.rs  # Keep!
│   │   │   ├── connections.rs
│   │   │   └── protocol.rs
│   │   └── webrtc_core.rs
│   │
│   ├── keeper-webrtc-python/    # Python bindings (optional)
│   │   └── src/python/
│   │
│   ├── guacr-handlers/          # New: Protocol handler system
│   │   ├── trait.rs
│   │   └── registry.rs
│   │
│   ├── guacr-ssh/               # New: SSH handler
│   ├── guacr-rdp/               # New: RDP handler
│   ├── guacr-vnc/               # New: VNC handler
│   ├── guacr-database/          # New: Database handlers
│   └── guacr-rbi/               # New: Browser isolation
│
├── client/                      # New: Browser WebRTC client
│   └── guacr-webrtc-client.js
│
└── docs/
    └── Integration docs
```

### Key Code Changes

**1. Enhance Channel API for Protocol Handlers**

```rust
// In keeper-webrtc-core/src/channel/connections.rs

pub async fn handle_guacd_conversation(
    channel: Arc<Channel>,
    settings: Settings,
    handler_registry: Arc<ProtocolHandlerRegistry>,  // NEW
) -> Result<()> {
    // 1. Read Guacamole handshake using existing parser
    let handshake = read_guacd_handshake(&channel).await?;

    // 2. Select protocol handler based on handshake
    let handler = handler_registry.get(&handshake.protocol)
        .ok_or_else(|| anyhow!("Protocol not supported: {}", handshake.protocol))?;

    // 3. Create instruction channels
    let (to_client, from_handler) = create_channels();
    let (to_handler, from_client) = create_channels();

    // 4. Spawn protocol handler task
    tokio::spawn(async move {
        handler.connect_webrtc(handshake.params, to_client, from_client).await
    });

    // 5. Forward between WebRTC channel and handler
    bidirectional_forward(channel, from_handler, to_handler).await?;

    Ok(())
}
```

**2. Protocol Handler API**

```rust
// In guacr-handlers/src/trait.rs

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    fn name(&self) -> &str;

    async fn connect_webrtc(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<Bytes>,      // Send to client via WebRTC
        guac_rx: mpsc::Receiver<Bytes>,    // Recv from client via WebRTC
    ) -> Result<()>;
}
```

**3. Browser Client**

```javascript
// client/guacr-webrtc-client.js

class GuacrWebRTCClient {
    constructor() {
        this.pc = new RTCPeerConnection({
            iceServers: [{ urls: 'stun:stun.l.google.com:19302' }]
        });
        this.dataChannel = null;
    }

    async connect(serverUrl, protocol, params) {
        // 1. Create data channel
        this.dataChannel = this.pc.createDataChannel('guacamole', {
            ordered: true,
        });

        // 2. Create offer
        const offer = await this.pc.createOffer();
        await this.pc.setLocalDescription(offer);

        // 3. Send offer to server, get answer
        const response = await fetch(`${serverUrl}/webrtc/offer`, {
            method: 'POST',
            body: JSON.stringify({
                offer: btoa(offer.sdp),
                protocol: protocol,
                params: params
            })
        });
        const { answer } = await response.json();

        // 4. Set answer
        await this.pc.setRemoteDescription({
            type: 'answer',
            sdp: atob(answer)
        });

        // 5. Set up data channel handlers
        this.dataChannel.onmessage = (event) => {
            const instruction = this.parseGuacamoleInstruction(event.data);
            this.handleInstruction(instruction);
        };
    }

    sendKey(keysym, pressed) {
        const instruction = this.formatGuacamoleInstruction('key', [
            keysym.toString(),
            pressed ? '1' : '0'
        ]);
        this.dataChannel.send(instruction);
    }

    sendMouse(x, y, buttonMask) {
        const instruction = this.formatGuacamoleInstruction('mouse', [
            x.toString(),
            y.toString(),
            buttonMask.toString()
        ]);
        this.dataChannel.send(instruction);
    }
}
```

---

## Performance Analysis

### Current keeper-pam-webrtc-rs Performance

From their benchmarks:
- **Frame parsing**: 398-2213ns per frame (SIMD optimized)
- **Lock-free**: DashMap reads (no contention)
- **Memory**: Thread-local buffer pools
- **Latency**: Sub-10ms peer-to-peer

### With Protocol Handlers Added

**Expected:**
- **SSH connections**: 5-10ms input latency (vs 20-50ms with text protocol)
- **RDP/VNC**: 60+ FPS with <15ms latency
- **Throughput**: 1-2 Gbps per connection (binary, no encoding)
- **Connections**: 10,000+ per instance (same as planned)

### Comparison

| Metric | Guacr (Text) | keeper-webrtc + handlers | Improvement |
|--------|--------------|--------------------------|-------------|
| Latency | 20-50ms | 5-15ms | 60-75% reduction |
| Protocol overhead | Text encoding | Binary (WebRTC) | 90% reduction |
| NAT traversal | No | Yes (STUN/TURN) | Built-in |
| Browser support | Via proxy | Native WebRTC | Direct |
| Implementation time | 28 weeks | 12-16 weeks | 43-57% faster |

---

## Recommendation

**DO NOT build guacr from scratch. Instead:**

1. **Extend keeper-pam-webrtc-rs** with protocol handlers
2. **Reuse existing:**
   - WebRTC implementation (production-tested)
   - Guacamole protocol parser (already done!)
   - Zero-copy architecture (already optimized)
   - SIMD optimizations (already implemented)

3. **Add only:**
   - Protocol handler trait and registry
   - SSH, RDP, VNC, Database, RBI handlers
   - Browser WebRTC client
   - Enhanced observability

4. **Benefits:**
   - 50% faster timeline (12-16 weeks vs 28 weeks)
   - Better performance (5-15ms vs 20-50ms latency)
   - Less code to maintain
   - Production-proven foundation
   - Native browser support (no proxy needed)

**Next Steps:**
1. Deep dive into keeper-pam-webrtc-rs architecture
2. Design protocol handler integration points
3. Implement SSH handler as proof-of-concept
4. Build browser WebRTC client
5. Add remaining protocol handlers

Should I create a detailed integration plan showing exactly how to merge the two projects?
