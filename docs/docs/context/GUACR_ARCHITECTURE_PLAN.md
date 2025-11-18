# Guacr - Modern Rust Rewrite of Guacd

## Executive Summary

**guacr** is a ground-up rewrite of the Apache Guacamole daemon (guacd) in Rust, designed for modern cloud-native deployments. It maintains the core concept of proxying remote desktop protocols through the Guacamole protocol but embraces modern architectural patterns, async I/O, and cloud-native observability.

**Key Goals:**
- **Modern**: Async/await with Tokio, structured logging, observability, io_uring
- **Safe**: Memory safety, type safety, no segfaults via Rust
- **Performant**: Zero-copy I/O, SIMD acceleration, GPU encoding, lock-free queues, 10,000+ connections
- **Cloud-native**: Container-first, dynamic config, health checks, metrics
- **Complete**: All protocol handlers (SSH, Telnet, RDP, VNC, Database, RBI/HTTP)
- **Optimized**: Buffer pooling, shared memory IPC, memory mapping, kernel bypass

---

## 1. Core Daemon Architecture

### 1.1 Runtime & Concurrency

```
┌─────────────────────────────────────────────────────────────┐
│                      Guacr Daemon                            │
│                                                              │
│  ┌────────────────────────────────────────────────────┐    │
│  │           Tokio Async Runtime (Multi-threaded)      │    │
│  └────────────────────────────────────────────────────┘    │
│                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐     │
│  │  Listener    │  │  Listener    │  │  Listener    │     │
│  │  TCP :4822   │  │  WebSocket   │  │  gRPC :4823  │     │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘     │
│         │                  │                  │              │
│         └──────────────────┴──────────────────┘             │
│                            │                                 │
│                   ┌────────▼────────┐                       │
│                   │  Connection      │                       │
│                   │  Multiplexer     │                       │
│                   └────────┬────────┘                       │
│                            │                                 │
│         ┌──────────────────┼──────────────────┐            │
│         │                  │                  │             │
│    ┌────▼─────┐      ┌────▼─────┐      ┌────▼─────┐      │
│    │ Client 1 │      │ Client 2 │      │ Client N │      │
│    │ (Task)   │      │ (Task)   │      │ (Task)   │      │
│    └────┬─────┘      └────┬─────┘      └────┬─────┘      │
│         │                  │                  │             │
│    ┌────▼─────┐      ┌────▼─────┐      ┌────▼─────┐      │
│    │ Protocol │      │ Protocol │      │ Protocol │      │
│    │ Handler  │      │ Handler  │      │ Handler  │      │
│    │  (SSH)   │      │  (RDP)   │      │  (VNC)   │      │
│    └──────────┘      └──────────┘      └──────────┘      │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

**Technology Stack:**
- **Runtime**: `tokio` with `io_uring` backend (async multi-threaded runtime)
- **Zero-Copy**: `bytes::Bytes` for reference-counted buffers, buffer pooling
- **Lock-Free**: `crossbeam` queues for instruction passing without mutex contention
- **SIMD**: Platform-specific acceleration (AVX2, NEON) for pixel operations
- **Async traits**: `async-trait` for protocol handler interface
- **Channels**: Lock-free `ArrayQueue` for high-performance messaging
- **Task spawning**: `tokio::task::spawn` for per-connection tasks
- **Cancellation**: `tokio::select!` and `CancellationToken` for graceful shutdown

### 1.2 Connection Lifecycle

```rust
// Zero-copy connection handling with optimizations
async fn handle_connection(
    stream: TcpStream,
    handlers: Arc<ProtocolHandlerRegistry>,
    config: Arc<Config>,
    buffer_pool: Arc<BufferPool>,
) -> Result<()> {
    // 1. Parse Guacamole protocol handshake
    let mut guac_conn = GuacConnection::new(stream).await?;
    let handshake = guac_conn.read_handshake().await?;

    // 2. Select protocol handler based on handshake
    let handler = handlers.get(&handshake.protocol)?;

    // 3. Create lock-free instruction queues (zero contention)
    let to_handler = Arc::new(LockFreeInstructionQueue::new(1024));
    let from_handler = Arc::new(LockFreeInstructionQueue::new(1024));

    // 4. Create connection arena for fast allocation
    let arena = ConnectionArena::new();

    // 5. Spawn protocol handler task with zero-copy support
    let handler_task = tokio::spawn(async move {
        handler.connect_zero_copy(
            handshake.params,
            from_handler.clone(),
            to_handler.clone(),
            buffer_pool.clone(),
        ).await
    });

    // 6. Bidirectional zero-copy forwarding using io_uring
    let forward_task = tokio::spawn(async move {
        bidirectional_forward_zero_copy(
            guac_conn,
            from_handler,
            to_handler,
            buffer_pool,
        ).await
    });

    // 7. Wait for completion or cancellation
    tokio::select! {
        _ = handler_task => {},
        _ = forward_task => {},
        _ = shutdown_signal() => {},
    }

    // Arena and buffers automatically returned to pool
    Ok(())
}
```

### 1.3 Configuration Management

**Modern TOML-based configuration** (replacing guacd.conf):

```toml
# guacr.toml
[server]
bind_address = "0.0.0.0:4822"
websocket_enabled = true
websocket_port = 4823
max_connections = 10000
connection_timeout = "30s"

[logging]
level = "info"
format = "json"  # or "pretty" for development
output = "stdout"  # or file path

[metrics]
enabled = true
prometheus_port = 9090
opentelemetry_endpoint = "http://localhost:4317"

[protocols.ssh]
enabled = true
max_connections = 1000
default_port = 22
keepalive_interval = "30s"

[protocols.rdp]
enabled = true
max_connections = 500
default_port = 3389
security_mode = "nla"  # or "rdp", "tls", "any"

[protocols.vnc]
enabled = true
max_connections = 500
default_port = 5900

[protocols.telnet]
enabled = true
max_connections = 100

[protocols.database]
enabled = true
mysql = true
postgresql = true
sqlserver = true

[protocols.rbi]
enabled = true
max_browser_instances = 50
chromium_path = "/usr/lib/chromium"

[distribution]
mode = "standalone"  # or "distributed"
grpc_endpoint = "0.0.0.0:50051"

[security]
tls_enabled = false
tls_cert = "/path/to/cert.pem"
tls_key = "/path/to/key.pem"
max_clipboard_size = "10MB"
```

**Configuration reloading:**
```rust
// Hot reload without restart
async fn watch_config(config: Arc<RwLock<Config>>) {
    let mut watcher = notify::watcher(/* ... */);
    loop {
        match watcher.recv().await {
            ConfigChange::Protocols(proto) => {
                // Reload protocol-specific config
            },
            ConfigChange::Security(sec) => {
                // Reload security settings
            },
            _ => {}
        }
    }
}
```

### 1.4 Logging & Observability

**Structured logging with `tracing`:**

```rust
use tracing::{info, warn, error, instrument, Span};

#[instrument(skip(stream), fields(client_id = %client_id))]
async fn handle_connection(stream: TcpStream, client_id: Uuid) {
    info!("new connection established");

    let protocol = match read_handshake(&stream).await {
        Ok(p) => {
            info!(protocol = %p, "protocol selected");
            p
        },
        Err(e) => {
            error!(error = %e, "handshake failed");
            return;
        }
    };

    // All logs automatically include client_id and span context
}
```

**OpenTelemetry integration:**
```rust
// Distributed tracing for microservices
use opentelemetry::global;
use tracing_opentelemetry::OpenTelemetryLayer;

let tracer = opentelemetry_otlp::new_pipeline()
    .tracing()
    .with_exporter(opentelemetry_otlp::new_exporter().tonic())
    .install_batch(runtime)?;

tracing_subscriber::registry()
    .with(OpenTelemetryLayer::new(tracer))
    .with(tracing_subscriber::fmt::layer())
    .init();
```

**Metrics with Prometheus:**
```rust
use prometheus::{IntCounter, IntGauge, Histogram};

lazy_static! {
    static ref ACTIVE_CONNECTIONS: IntGauge =
        register_int_gauge!("guacr_active_connections", "Active connections").unwrap();

    static ref TOTAL_CONNECTIONS: IntCounter =
        register_int_counter!("guacr_total_connections", "Total connections").unwrap();

    static ref CONNECTION_DURATION: Histogram =
        register_histogram!("guacr_connection_duration_seconds", "Connection duration").unwrap();
}
```

---

## 1.5 Zero-Copy Architecture

### Buffer Pooling System

Pre-allocated buffer pools eliminate allocation overhead:

```rust
pub struct BufferPool {
    // Lock-free queue for buffer reuse
    pool: crossbeam::queue::ArrayQueue<BytesMut>,
    buffer_size: usize,

    // Metrics
    allocations_avoided: AtomicU64,
    pool_hits: AtomicU64,
}

impl BufferPool {
    pub fn new(pool_size: usize, buffer_size: usize) -> Self {
        let pool = ArrayQueue::new(pool_size);

        // Pre-allocate all buffers
        for _ in 0..pool_size {
            pool.push(BytesMut::with_capacity(buffer_size)).ok();
        }

        Self { pool, buffer_size, /* ... */ }
    }

    pub fn acquire(&self) -> BytesMut {
        match self.pool.pop() {
            Some(buf) => {
                self.pool_hits.fetch_add(1, Ordering::Relaxed);
                buf
            }
            None => {
                // Pool exhausted, allocate new (rare)
                BytesMut::with_capacity(self.buffer_size)
            }
        }
    }

    pub fn release(&self, mut buf: BytesMut) {
        buf.clear();  // Reset but keep capacity
        self.pool.push(buf).ok();  // Return to pool
    }
}
```

### io_uring Integration

Kernel bypass for true zero-copy I/O:

```rust
use tokio_uring::net::TcpStream;

pub async fn send_zero_copy(stream: &TcpStream, data: Vec<u8>) -> Result<()> {
    // io_uring takes ownership, returns after completion
    let (result, _buf) = stream.write(data).await;
    result?;
    Ok(())
}

pub struct IoUringConnection {
    stream: TcpStream,
    registered_buffers: Vec<Vec<u8>>,
}

impl IoUringConnection {
    pub async fn send_registered(&self, buffer_index: usize) -> Result<()> {
        // Use pre-registered buffer - true zero-copy
        self.stream.write_fixed(buffer_index, 4096).await?;
        Ok(())
    }
}
```

### SIMD Pixel Operations

Hardware-accelerated pixel format conversion:

```rust
use std::arch::x86_64::*;

pub struct SimdPixelConverter;

impl SimdPixelConverter {
    #[target_feature(enable = "avx2")]
    pub unsafe fn bgra_to_rgba_avx2(input: &[u8], output: &mut [u8]) {
        let shuffle_mask = _mm256_setr_epi8(
            2, 1, 0, 3,  // Swap R and B
            6, 5, 4, 7,
            10, 9, 8, 11,
            14, 13, 12, 15,
            // ... repeat for 8 pixels
        );

        for i in (0..input.len()).step_by(32) {
            let pixels = _mm256_loadu_si256(input.as_ptr().add(i) as *const __m256i);
            let converted = _mm256_shuffle_epi8(pixels, shuffle_mask);
            _mm256_storeu_si256(output.as_mut_ptr().add(i) as *mut __m256i, converted);
        }
    }

    pub fn convert(input: &[u8], output: &mut [u8]) {
        if is_x86_feature_detected!("avx2") {
            unsafe { Self::bgra_to_rgba_avx2(input, output) };
        } else {
            // Fallback to scalar
            Self::bgra_to_rgba_scalar(input, output);
        }
    }
}
```

### Shared Memory for IPC

Zero-copy communication with browser processes:

```rust
use shared_memory::Shmem;

pub struct SharedFrameBuffer {
    shm: Shmem,
    header: *mut FrameBufferHeader,
    data: *mut u8,
}

#[repr(C)]
struct FrameBufferHeader {
    width: u32,
    height: u32,
    sequence: AtomicU64,
    ready: AtomicU64,
}

impl SharedFrameBuffer {
    pub fn write_frame(&mut self, pixels: &[u8]) {
        unsafe {
            // Wait for reader
            while (*self.header).ready.load(Ordering::Acquire) == 1 {
                std::hint::spin_loop();
            }

            // Copy frame data
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.data, pixels.len());

            // Mark ready
            (*self.header).sequence.fetch_add(1, Ordering::Release);
            (*self.header).ready.store(1, Ordering::Release);
        }
    }
}
```

### Lock-Free Instruction Queues

Eliminate mutex contention:

```rust
use crossbeam::queue::ArrayQueue;

pub struct LockFreeInstructionQueue {
    queue: Arc<ArrayQueue<Instruction>>,
}

impl LockFreeInstructionQueue {
    pub fn push(&self, instruction: Instruction) -> Result<(), Instruction> {
        self.queue.push(instruction)
    }

    pub fn pop(&self) -> Option<Instruction> {
        self.queue.pop()
    }
}

// Use in handlers instead of mpsc channels for higher throughput
```

### Performance Gains

Expected improvements over traditional implementation:

```
Metric                  Traditional    Zero-Copy    Improvement
─────────────────────────────────────────────────────────────
Memory copies/frame     4-6            0-1          83-90%
Allocations/frame       3-5            0 (pooled)   100%
CPU per connection      40-60%         10-20%       67-75%
Throughput             100-200 Mbps   500-1000 Mbps 400-900%
Max connections        500-1000       5000-10000   900%
Frame encoding latency  10ms           2ms          80%
Network send latency    5ms            1ms          80%
```

**See GUACR_ZERO_COPY_OPTIMIZATION.md for complete implementation details.**

---

## 2. Guacamole Protocol Implementation

### 2.1 Protocol Parser (Pure Rust)

The Guacamole protocol is a text-based instruction protocol. Example:
```
5.mouse,1.0,2.10,3.120;
4.size,3.800,3.600,2.96;
```

**Rust implementation:**

```rust
// guacr-protocol crate
pub mod instruction {
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    #[derive(Debug, Clone)]
    pub struct Instruction {
        pub opcode: String,
        pub args: Vec<String>,
    }

    pub struct GuacCodec;

    impl Decoder for GuacCodec {
        type Item = Instruction;
        type Error = GuacError;

        fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
            // Parse: LENGTH.ELEMENT,LENGTH.ELEMENT,...;
            if let Some(semicolon_pos) = src.iter().position(|&b| b == b';') {
                let line = src.split_to(semicolon_pos + 1);
                let instruction = parse_instruction(&line)?;
                Ok(Some(instruction))
            } else {
                Ok(None)
            }
        }
    }

    impl Encoder<Instruction> for GuacCodec {
        type Error = GuacError;

        fn encode(&mut self, item: Instruction, dst: &mut BytesMut) -> Result<(), Self::Error> {
            // Encode instruction into Guacamole protocol format
            dst.extend_from_slice(format_instruction(&item).as_bytes());
            Ok(())
        }
    }
}

// Usage with Tokio
use tokio_util::codec::Framed;

let stream = TcpStream::connect("...").await?;
let mut framed = Framed::new(stream, GuacCodec);

// Read instruction
if let Some(instruction) = framed.next().await {
    match instruction?.opcode.as_str() {
        "mouse" => handle_mouse(&instruction.args),
        "key" => handle_key(&instruction.args),
        "size" => handle_size(&instruction.args),
        _ => {}
    }
}

// Write instruction
framed.send(Instruction {
    opcode: "sync".to_string(),
    args: vec!["12345".to_string()],
}).await?;
```

### 2.2 Connection Protocol

**Handshake sequence:**

```rust
pub struct GuacHandshake {
    pub protocol: String,  // "ssh", "rdp", "vnc", etc.
    pub params: HashMap<String, String>,
}

impl GuacConnection {
    pub async fn read_handshake(&mut self) -> Result<GuacHandshake> {
        // 1. Client sends: "select: protocol"
        let select = self.read_instruction().await?;
        assert_eq!(select.opcode, "select");
        let protocol = select.args[0].clone();

        // 2. Server responds with arg list
        self.send_instruction(Instruction {
            opcode: "args".to_string(),
            args: get_protocol_args(&protocol),
        }).await?;

        // 3. Client sends connection parameters
        let connect = self.read_instruction().await?;
        assert_eq!(connect.opcode, "connect");
        let params = parse_params(&connect.args)?;

        // 4. Server sends ready
        self.send_instruction(Instruction {
            opcode: "ready".to_string(),
            args: vec![generate_connection_id()],
        }).await?;

        Ok(GuacHandshake { protocol, params })
    }
}
```

### 2.3 Instruction Set

**Core instructions to implement:**

| Opcode | Direction | Purpose |
|--------|-----------|---------|
| `select` | C→S | Select protocol |
| `args` | S→C | Request parameters |
| `connect` | C→S | Connection params |
| `ready` | S→C | Connection ready |
| `sync` | S→C | Synchronization frame |
| `mouse` | C→S | Mouse movement/click |
| `key` | C→S | Keyboard input |
| `clipboard` | C↔S | Clipboard data |
| `size` | C→S | Screen size change |
| `img` | S→C | Image data (PNG) |
| `copy` | S→C | Copy image layer |
| `dispose` | S→C | Dispose layer |
| `audio` | S→C | Audio stream |
| `video` | S→C | Video stream |
| `file` | C↔S | File transfer |
| `disconnect` | C↔S | Close connection |

---

## 3. Protocol Handlers (Hybrid Approach)

### 3.1 Handler Trait Interface

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol name (ssh, rdp, vnc, etc.)
    fn name(&self) -> &str;

    /// Connect to remote host
    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()>;

    /// Disconnect and cleanup
    async fn disconnect(&self) -> Result<()>;

    /// Health check
    async fn health_check(&self) -> Result<HealthStatus>;
}

pub struct ProtocolHandlerRegistry {
    handlers: HashMap<String, Arc<dyn ProtocolHandler>>,
}

impl ProtocolHandlerRegistry {
    pub fn register<H: ProtocolHandler + 'static>(&mut self, handler: H) {
        self.handlers.insert(handler.name().to_string(), Arc::new(handler));
    }

    pub fn get(&self, protocol: &str) -> Option<Arc<dyn ProtocolHandler>> {
        self.handlers.get(protocol).cloned()
    }
}
```

### 3.2 SSH Handler (Pure Rust)

**Using `russh` crate (modern async SSH library):**

```rust
pub struct SshHandler {
    config: SshConfig,
}

#[async_trait]
impl ProtocolHandler for SshHandler {
    fn name(&self) -> &str { "ssh" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        // 1. Connect to SSH server
        let hostname = params.get("hostname").ok_or(Error::MissingParam)?;
        let port = params.get("port").unwrap_or(&"22".to_string()).parse()?;
        let username = params.get("username").ok_or(Error::MissingParam)?;

        let mut session = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            (hostname.as_str(), port),
        ).await?;

        // 2. Authenticate
        let auth_method = params.get("auth-method").unwrap_or(&"password".to_string());
        match auth_method.as_str() {
            "password" => {
                let password = params.get("password").ok_or(Error::MissingParam)?;
                session.authenticate_password(username, password).await?;
            },
            "private-key" => {
                let key = load_private_key(&params)?;
                session.authenticate_publickey(username, Arc::new(key)).await?;
            },
            _ => return Err(Error::UnsupportedAuthMethod),
        }

        // 3. Open PTY channel
        let mut channel = session.channel_open_session().await?;
        channel.request_pty(
            "xterm-256color",
            80,  // cols
            24,  // rows
            0, 0,  // width/height in pixels
        ).await?;
        channel.request_shell().await?;

        // 4. Bidirectional forwarding
        let (mut ssh_rx, mut ssh_tx) = channel.split();

        // SSH → Guac (output)
        let output_task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 4096];
            loop {
                match ssh_rx.read(&mut buffer).await {
                    Ok(n) if n > 0 => {
                        // Convert terminal output to Guacamole display instructions
                        let instructions = terminal_to_guac(&buffer[..n])?;
                        for instr in instructions {
                            guac_tx.send(instr).await?;
                        }
                    },
                    Ok(_) => break,  // EOF
                    Err(e) => {
                        error!("SSH read error: {}", e);
                        break;
                    }
                }
            }
            Ok::<_, Error>(())
        });

        // Guac → SSH (input)
        let input_task = tokio::spawn(async move {
            while let Some(instruction) = guac_rx.recv().await {
                match instruction.opcode.as_str() {
                    "key" => {
                        let key_data = parse_key_instruction(&instruction)?;
                        ssh_tx.write_all(&key_data).await?;
                    },
                    "mouse" => {
                        // Mouse not applicable for SSH, but could support mouse reporting
                    },
                    "clipboard" => {
                        let clipboard_data = instruction.args[1].as_bytes();
                        ssh_tx.write_all(clipboard_data).await?;
                    },
                    "size" => {
                        let (cols, rows) = parse_size(&instruction)?;
                        ssh_tx.window_change(cols, rows, 0, 0).await?;
                    },
                    _ => {}
                }
            }
            Ok::<_, Error>(())
        });

        // Wait for tasks to complete
        tokio::try_join!(output_task, input_task)?;

        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // Cleanup handled by task cancellation
        Ok(())
    }

    async fn health_check(&self) -> Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }
}

// Terminal emulation - convert ANSI/VT100 to Guacamole display
fn terminal_to_guac(data: &[u8]) -> Result<Vec<GuacInstruction>> {
    use vt100::Parser;

    let mut parser = Parser::new(24, 80, 0);
    parser.process(data);

    let screen = parser.screen();
    let mut instructions = Vec::new();

    // Convert terminal screen to Guacamole layers and images
    // This is simplified - real implementation needs:
    // - Layer management
    // - Dirty rectangle optimization
    // - Text rendering to PNG
    // - Cursor handling

    instructions.push(GuacInstruction {
        opcode: "img".to_string(),
        args: vec![
            "0".to_string(),  // layer
            "image/png".to_string(),
            render_screen_to_png(screen)?,
        ],
    });

    Ok(instructions)
}
```

### 3.3 Telnet Handler (Pure Rust)

**Using `tokio-telnet` or custom implementation:**

```rust
pub struct TelnetHandler;

#[async_trait]
impl ProtocolHandler for TelnetHandler {
    fn name(&self) -> &str { "telnet" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        let hostname = params.get("hostname").ok_or(Error::MissingParam)?;
        let port = params.get("port").unwrap_or(&"23".to_string()).parse()?;

        let stream = TcpStream::connect((hostname.as_str(), port)).await?;
        let mut telnet = TelnetCodec::new(stream);

        // Similar bidirectional forwarding as SSH
        // Handle telnet negotiations (IAC commands)
        // Convert terminal output to Guacamole display

        Ok(())
    }

    // ... rest of implementation
}
```

### 3.4 RDP Handler (Rust crate)

**Using `ironrdp` crate (pure Rust RDP implementation):**

```rust
pub struct RdpHandler {
    config: RdpConfig,
}

#[async_trait]
impl ProtocolHandler for RdpHandler {
    fn name(&self) -> &str { "rdp" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        use ironrdp::{ConnectorResult, DesktopSize, ClientConnector};

        // 1. Build RDP connector
        let hostname = params.get("hostname").ok_or(Error::MissingParam)?;
        let port = params.get("port").unwrap_or(&"3389".to_string()).parse()?;
        let username = params.get("username").ok_or(Error::MissingParam)?;
        let password = params.get("password").ok_or(Error::MissingParam)?;

        let connector = ClientConnector::new()
            .with_server_addr((hostname.clone(), port))
            .with_credentials(username.clone(), password.clone())
            .with_desktop_size(DesktopSize { width: 1920, height: 1080 });

        // 2. Connect
        let stream = TcpStream::connect((hostname.as_str(), port)).await?;
        let ConnectorResult {
            mut connection,
            io_channel_id,
            user_channel_id,
            ..
        } = connector.connect(stream).await?;

        // 3. Event loop
        loop {
            tokio::select! {
                // RDP events (display updates, audio, etc.)
                event = connection.next_event() => {
                    match event? {
                        RdpEvent::Bitmap(bitmap) => {
                            // Convert RDP bitmap to Guacamole img instruction
                            let png_data = bitmap_to_png(&bitmap)?;
                            guac_tx.send(GuacInstruction {
                                opcode: "img".to_string(),
                                args: vec![
                                    "0".to_string(),
                                    "image/png".to_string(),
                                    base64::encode(&png_data),
                                ],
                            }).await?;
                        },
                        RdpEvent::Pointer(pointer) => {
                            // Send pointer update
                        },
                        RdpEvent::Audio(audio) => {
                            // Forward audio stream
                        },
                        _ => {}
                    }
                },

                // Guacamole instructions (input events)
                Some(instruction) = guac_rx.recv() => {
                    match instruction.opcode.as_str() {
                        "mouse" => {
                            let (x, y, button) = parse_mouse(&instruction)?;
                            connection.send_mouse_event(x, y, button).await?;
                        },
                        "key" => {
                            let (keysym, pressed) = parse_key(&instruction)?;
                            connection.send_key_event(keysym, pressed).await?;
                        },
                        "clipboard" => {
                            let data = &instruction.args[1];
                            connection.send_clipboard(data).await?;
                        },
                        _ => {}
                    }
                },
            }
        }
    }

    // ... rest of implementation
}
```

**Alternative: FFI to FreeRDP** (if `ironrdp` lacks features):

```rust
// Use freerdp-rs bindings or create custom FFI
pub struct RdpHandlerFFI {
    // Hold reference to C FreeRDP context
    context: *mut freerdp_sys::freerdp,
}

// Safety: Careful synchronization required
unsafe impl Send for RdpHandlerFFI {}
unsafe impl Sync for RdpHandlerFFI {}
```

### 3.5 VNC Handler (Rust crate)

**Using `vnc-rs` or `async-vnc` crate:**

```rust
pub struct VncHandler;

#[async_trait]
impl ProtocolHandler for VncHandler {
    fn name(&self) -> &str { "vnc" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        use async_vnc::{VncConnector, VncEvent};

        let hostname = params.get("hostname").ok_or(Error::MissingParam)?;
        let port = params.get("port").unwrap_or(&"5900".to_string()).parse()?;
        let password = params.get("password");

        let stream = TcpStream::connect((hostname.as_str(), port)).await?;
        let mut vnc = VncConnector::new(stream)
            .set_auth_method(async_vnc::AuthMethod::Password)
            .add_encoding(async_vnc::Encoding::Tight)
            .add_encoding(async_vnc::Encoding::CopyRect)
            .add_encoding(async_vnc::Encoding::Raw)
            .connect()
            .await?;

        if let Some(pwd) = password {
            vnc.authenticate(pwd.as_bytes()).await?;
        }

        // Event loop similar to RDP
        loop {
            tokio::select! {
                event = vnc.next_event() => {
                    match event? {
                        VncEvent::FramebufferUpdate(rects) => {
                            for rect in rects {
                                // Convert VNC rectangle to Guacamole instruction
                                let png = encode_rect_to_png(&rect)?;
                                guac_tx.send(GuacInstruction {
                                    opcode: "img".to_string(),
                                    args: vec![
                                        "0".to_string(),
                                        "image/png".to_string(),
                                        base64::encode(&png),
                                    ],
                                }).await?;
                            }
                        },
                        VncEvent::Bell => {
                            // Send bell/beep
                        },
                        VncEvent::ServerCutText(text) => {
                            // Send clipboard update
                            guac_tx.send(GuacInstruction {
                                opcode: "clipboard".to_string(),
                                args: vec!["text/plain".to_string(), text],
                            }).await?;
                        },
                        _ => {}
                    }
                },
                Some(instruction) = guac_rx.recv() => {
                    match instruction.opcode.as_str() {
                        "mouse" => {
                            let (x, y, button_mask) = parse_mouse(&instruction)?;
                            vnc.send_pointer_event(button_mask, x, y).await?;
                        },
                        "key" => {
                            let (keysym, down) = parse_key(&instruction)?;
                            vnc.send_key_event(down, keysym).await?;
                        },
                        "clipboard" => {
                            vnc.send_cut_text(&instruction.args[1]).await?;
                        },
                        _ => {}
                    }
                }
            }
        }
    }

    // ... rest
}
```

### 3.6 Database Handlers (Pure Rust)

**MySQL Handler:**

```rust
pub struct MySqlHandler;

#[async_trait]
impl ProtocolHandler for MySqlHandler {
    fn name(&self) -> &str { "mysql" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        use sqlx::mysql::{MySqlPool, MySqlConnectOptions};

        // 1. Build connection string
        let hostname = params.get("hostname").ok_or(Error::MissingParam)?;
        let port = params.get("port").unwrap_or(&"3306".to_string()).parse()?;
        let username = params.get("username").ok_or(Error::MissingParam)?;
        let password = params.get("password").ok_or(Error::MissingParam)?;
        let database = params.get("database");

        let options = MySqlConnectOptions::new()
            .host(hostname)
            .port(port)
            .username(username)
            .password(password);

        let options = if let Some(db) = database {
            options.database(db)
        } else {
            options
        };

        // 2. Connect
        let pool = MySqlPool::connect_with(options).await?;

        // 3. Create virtual terminal for SQL interaction
        let mut terminal = VirtualTerminal::new(80, 24);
        terminal.write_line("Connected to MySQL server.")?;
        terminal.write_prompt("mysql> ")?;

        // Send initial screen
        send_terminal_screen(&guac_tx, &terminal).await?;

        // 4. Command processing loop
        let mut input_buffer = String::new();

        while let Some(instruction) = guac_rx.recv().await {
            match instruction.opcode.as_str() {
                "key" => {
                    let key = parse_key_instruction(&instruction)?;

                    match key {
                        Key::Char(c) => {
                            input_buffer.push(c);
                            terminal.write_char(c)?;
                        },
                        Key::Enter => {
                            terminal.newline()?;

                            // Execute SQL query
                            let query = input_buffer.trim();
                            if !query.is_empty() {
                                match execute_query(&pool, query).await {
                                    Ok(result) => {
                                        // Format result as table
                                        terminal.write_table(&result)?;
                                    },
                                    Err(e) => {
                                        terminal.write_error(&format!("Error: {}", e))?;
                                    }
                                }
                            }

                            input_buffer.clear();
                            terminal.write_prompt("mysql> ")?;
                        },
                        Key::Backspace => {
                            if !input_buffer.is_empty() {
                                input_buffer.pop();
                                terminal.backspace()?;
                            }
                        },
                        Key::Tab => {
                            // SQL auto-completion
                            let completions = autocomplete_sql(&input_buffer, &pool).await?;
                            if completions.len() == 1 {
                                let completion = &completions[0];
                                input_buffer.push_str(&completion[input_buffer.len()..]);
                                terminal.write_str(&completion[input_buffer.len()..])?;
                            } else if completions.len() > 1 {
                                terminal.newline()?;
                                terminal.write_completions(&completions)?;
                                terminal.write_prompt("mysql> ")?;
                                terminal.write_str(&input_buffer)?;
                            }
                        },
                        _ => {}
                    }

                    // Send updated screen
                    send_terminal_screen(&guac_tx, &terminal).await?;
                },
                _ => {}
            }
        }

        Ok(())
    }

    // ... rest
}

async fn execute_query(pool: &MySqlPool, query: &str) -> Result<QueryResult> {
    use sqlx::Row;

    // Determine query type
    let query_type = query.split_whitespace().next().unwrap_or("").to_uppercase();

    match query_type.as_str() {
        "SELECT" | "SHOW" | "DESCRIBE" | "EXPLAIN" => {
            let rows = sqlx::query(query).fetch_all(pool).await?;

            if rows.is_empty() {
                return Ok(QueryResult::empty());
            }

            // Extract column names
            let columns: Vec<String> = rows[0].columns().iter()
                .map(|col| col.name().to_string())
                .collect();

            // Extract values
            let values: Vec<Vec<String>> = rows.iter()
                .map(|row| {
                    (0..columns.len())
                        .map(|i| row.try_get::<String, _>(i).unwrap_or_default())
                        .collect()
                })
                .collect();

            Ok(QueryResult { columns, values })
        },
        "INSERT" | "UPDATE" | "DELETE" => {
            let result = sqlx::query(query).execute(pool).await?;
            Ok(QueryResult::affected_rows(result.rows_affected()))
        },
        _ => {
            sqlx::query(query).execute(pool).await?;
            Ok(QueryResult::success())
        }
    }
}
```

**PostgreSQL Handler** (similar structure):

```rust
pub struct PostgreSqlHandler;

#[async_trait]
impl ProtocolHandler for PostgreSqlHandler {
    fn name(&self) -> &str { "postgresql" }

    async fn connect(/* ... */) -> Result<()> {
        use sqlx::postgres::PgPool;
        // Similar to MySQL but with PostgreSQL-specific features
        // - \d commands for schema inspection
        // - COPY support
        // - LISTEN/NOTIFY for pub/sub
    }
}
```

**SQL Server Handler:**

```rust
pub struct SqlServerHandler;

#[async_trait]
impl ProtocolHandler for SqlServerHandler {
    fn name(&self) -> &str { "sqlserver" }

    async fn connect(/* ... */) -> Result<()> {
        use tiberius::{Client, Config, AuthMethod};
        // Use tiberius crate for SQL Server
        // Handle Windows authentication if needed
    }
}
```

### 3.7 RBI/HTTP Handler (Browser Isolation)

This is the most complex handler. Two approaches:

**Approach 1: Headless Chromium with CDP (Chrome DevTools Protocol)**

```rust
pub struct RbiHandler {
    chromium_path: PathBuf,
}

#[async_trait]
impl ProtocolHandler for RbiHandler {
    fn name(&self) -> &str { "http" }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<GuacInstruction>,
        mut guac_rx: mpsc::Receiver<GuacInstruction>,
    ) -> Result<()> {
        use headless_chrome::{Browser, LaunchOptions};

        // 1. Launch isolated Chrome instance
        let url = params.get("url").ok_or(Error::MissingParam)?;

        let browser = Browser::new(LaunchOptions {
            headless: true,
            sandbox: true,
            window_size: Some((1920, 1080)),
            path: Some(self.chromium_path.clone()),
            ..Default::default()
        })?;

        let tab = browser.new_tab()?;
        tab.navigate_to(url)?;

        // 2. Set up screen capture
        let (screenshot_tx, mut screenshot_rx) = mpsc::channel(10);

        // Continuous screenshot capture (or use CDP Page.startScreencast)
        let capture_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;

                match tab.capture_screenshot(
                    headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                    None,
                    None,
                    true,
                ) {
                    Ok(screenshot) => {
                        screenshot_tx.send(screenshot).await.ok();
                    },
                    Err(e) => {
                        error!("Screenshot capture failed: {}", e);
                        break;
                    }
                }
            }
        });

        // 3. Input handling
        let input_task = tokio::spawn(async move {
            while let Some(instruction) = guac_rx.recv().await {
                match instruction.opcode.as_str() {
                    "mouse" => {
                        let (x, y, button) = parse_mouse(&instruction)?;

                        match button {
                            1 => tab.mouse_click(x, y)?,
                            _ => {}
                        }
                    },
                    "key" => {
                        let key_text = parse_key_to_text(&instruction)?;
                        tab.type_str(&key_text)?;
                    },
                    "clipboard" => {
                        // Clipboard integration
                        let text = &instruction.args[1];
                        tab.evaluate(&format!(
                            "navigator.clipboard.writeText('{}')",
                            text.escape_default()
                        ), false)?;
                    },
                    _ => {}
                }
            }
            Ok::<_, Error>(())
        });

        // 4. Screen streaming
        let output_task = tokio::spawn(async move {
            while let Some(screenshot) = screenshot_rx.recv().await {
                // Compress and send screenshot
                let png_data = compress_png(&screenshot)?;

                guac_tx.send(GuacInstruction {
                    opcode: "img".to_string(),
                    args: vec![
                        "0".to_string(),
                        "image/png".to_string(),
                        base64::encode(&png_data),
                    ],
                }).await?;

                // Send sync instruction for frame timing
                guac_tx.send(GuacInstruction {
                    opcode: "sync".to_string(),
                    args: vec![timestamp_ms().to_string()],
                }).await?;
            }
            Ok::<_, Error>(())
        });

        tokio::try_join!(capture_task, input_task, output_task)?;

        Ok(())
    }

    // ... rest
}
```

**Approach 2: FFI to CEF (like current implementation)**

```rust
// Maintain CEF integration via rust-cef bindings
// Use shared memory for frame buffers
// More complex but potentially more efficient
```

**Keeper-specific features to add:**
- Credential autofill from Keeper Vault (requires vault integration)
- TOTP generation and autofill
- Session recording
- URL filtering and security policies

---

## 4. Modern Distribution Layer (Beyond ZeroMQ)

### 4.1 gRPC Service Mesh Architecture

**Replace ZeroMQ with gRPC + service discovery for cloud-native distributed guacr.**

```
┌─────────────────────────────────────────────────────────────┐
│                     Load Balancer                            │
│                   (HAProxy / Envoy)                          │
└───────────────┬─────────────────────────────────────────────┘
                │
       ┌────────┴────────┬────────────┬──────────────┐
       │                 │            │              │
┌──────▼──────┐  ┌──────▼──────┐  ┌──▼────────┐  ┌─▼─────────┐
│   Guacr     │  │   Guacr     │  │  Guacr    │  │  Guacr    │
│  Instance 1 │  │  Instance 2 │  │ Instance 3│  │ Instance N│
│             │  │             │  │           │  │           │
│ gRPC :50051 │  │ gRPC :50051 │  │gRPC :50051│  │gRPC :50051│
└──────┬──────┘  └──────┬──────┘  └──┬────────┘  └─┬─────────┘
       │                 │            │              │
       └─────────────────┴────────────┴──────────────┘
                         │
                ┌────────▼────────┐
                │   Service       │
                │   Discovery     │
                │   (Consul /     │
                │    etcd)        │
                └─────────────────┘
```

**gRPC service definition:**

```protobuf
// guacr.proto
syntax = "proto3";

package guacr.v1;

service GuacrService {
    // Establish connection (bidirectional streaming)
    rpc Connect(stream GuacInstruction) returns (stream GuacInstruction);

    // Health check
    rpc HealthCheck(HealthCheckRequest) returns (HealthCheckResponse);

    // Metrics
    rpc GetMetrics(MetricsRequest) returns (MetricsResponse);

    // Session management
    rpc ListSessions(ListSessionsRequest) returns (ListSessionsResponse);
    rpc TerminateSession(TerminateSessionRequest) returns (TerminateSessionResponse);
}

message GuacInstruction {
    string opcode = 1;
    repeated string args = 2;
    int64 timestamp = 3;
}

message HealthCheckRequest {}

message HealthCheckResponse {
    enum Status {
        HEALTHY = 0;
        DEGRADED = 1;
        UNHEALTHY = 2;
    }
    Status status = 1;
    int32 active_connections = 2;
    double cpu_usage = 3;
    double memory_usage = 4;
}

message MetricsRequest {}

message MetricsResponse {
    int64 total_connections = 1;
    int64 active_connections = 2;
    map<string, int64> protocol_counts = 3;
    double avg_connection_duration = 4;
}

message ListSessionsRequest {}

message ListSessionsResponse {
    repeated Session sessions = 1;
}

message Session {
    string id = 1;
    string protocol = 2;
    string remote_host = 3;
    int64 started_at = 4;
    int64 bytes_sent = 5;
    int64 bytes_received = 6;
}

message TerminateSessionRequest {
    string session_id = 1;
}

message TerminateSessionResponse {
    bool success = 1;
}
```

**Rust implementation with Tonic:**

```rust
use tonic::{transport::Server, Request, Response, Status, Streaming};
use tokio_stream::wrappers::ReceiverStream;

pub struct GuacrServiceImpl {
    handlers: Arc<ProtocolHandlerRegistry>,
    sessions: Arc<RwLock<HashMap<String, SessionHandle>>>,
}

#[tonic::async_trait]
impl GuacrService for GuacrServiceImpl {
    type ConnectStream = ReceiverStream<Result<GuacInstruction, Status>>;

    async fn connect(
        &self,
        request: Request<Streaming<GuacInstruction>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let mut in_stream = request.into_inner();
        let (tx, rx) = mpsc::channel(128);

        tokio::spawn(async move {
            // Read handshake from client stream
            let handshake = match in_stream.message().await {
                Ok(Some(msg)) => parse_handshake(msg)?,
                _ => return Err(Status::invalid_argument("No handshake")),
            };

            // Get protocol handler
            let handler = self.handlers.get(&handshake.protocol)
                .ok_or(Status::not_found("Protocol not supported"))?;

            // Create bidirectional channels
            let (guac_tx, guac_rx) = mpsc::channel(128);
            let (proto_tx, proto_rx) = mpsc::channel(128);

            // Spawn handler
            let handler_task = tokio::spawn(async move {
                handler.connect(handshake.params, proto_tx, proto_rx).await
            });

            // Forward gRPC stream → handler
            let forward_in = tokio::spawn(async move {
                while let Some(msg) = in_stream.message().await? {
                    guac_tx.send(msg).await?;
                }
                Ok::<_, Status>(())
            });

            // Forward handler → gRPC stream
            let forward_out = tokio::spawn(async move {
                while let Some(msg) = guac_rx.recv().await {
                    tx.send(Ok(msg)).await.ok();
                }
            });

            tokio::try_join!(handler_task, forward_in, forward_out)?;

            Ok(())
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let active = self.sessions.read().await.len() as i32;

        Ok(Response::new(HealthCheckResponse {
            status: if active < 1000 { 0 } else { 1 },
            active_connections: active,
            cpu_usage: get_cpu_usage(),
            memory_usage: get_memory_usage(),
        }))
    }

    // ... other methods
}

// Start gRPC server
pub async fn start_grpc_server(config: Arc<Config>) -> Result<()> {
    let addr = config.grpc_bind_address.parse()?;
    let service = GuacrServiceImpl::new(/* ... */);

    Server::builder()
        .add_service(GuacrServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
```

### 4.2 Service Discovery with Consul

```rust
use consul::Client as ConsulClient;

pub async fn register_with_consul(config: &Config) -> Result<()> {
    let consul = ConsulClient::new(consul::Config {
        address: config.consul_address.clone(),
        ..Default::default()
    })?;

    // Register service
    consul.agent.service_register(&ServiceDefinition {
        id: format!("guacr-{}", hostname()),
        name: "guacr".to_string(),
        address: config.bind_address.ip().to_string(),
        port: config.bind_address.port(),
        tags: vec!["guacamole".to_string(), "remote-desktop".to_string()],
        check: Some(ServiceCheck {
            name: "health".to_string(),
            interval: "10s".to_string(),
            http: format!("http://{}:{}/health", config.bind_address.ip(), config.metrics_port),
            ..Default::default()
        }),
        ..Default::default()
    }).await?;

    info!("Registered with Consul as guacr-{}", hostname());

    Ok(())
}

// Deregister on shutdown
pub async fn deregister_from_consul() -> Result<()> {
    let consul = get_consul_client();
    consul.agent.service_deregister(&format!("guacr-{}", hostname())).await?;
    Ok(())
}
```

### 4.3 Load Balancing Strategies

**Client-side load balancing:**

```rust
pub struct GuacrLoadBalancer {
    endpoints: Arc<RwLock<Vec<Endpoint>>>,
    strategy: LoadBalanceStrategy,
}

pub enum LoadBalanceStrategy {
    RoundRobin,
    LeastConnections,
    Random,
    WeightedRandom,
}

impl GuacrLoadBalancer {
    pub async fn select_endpoint(&self) -> Result<Endpoint> {
        let endpoints = self.endpoints.read().await;

        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                // Round-robin selection
                Ok(endpoints[self.next_index()].clone())
            },
            LoadBalanceStrategy::LeastConnections => {
                // Query health endpoints and select least loaded
                let mut min_connections = usize::MAX;
                let mut selected = None;

                for endpoint in endpoints.iter() {
                    let health = endpoint.health_check().await?;
                    if (health.active_connections as usize) < min_connections {
                        min_connections = health.active_connections as usize;
                        selected = Some(endpoint.clone());
                    }
                }

                selected.ok_or(Error::NoHealthyEndpoints)
            },
            LoadBalanceStrategy::Random => {
                Ok(endpoints[rand::random::<usize>() % endpoints.len()].clone())
            },
            _ => unimplemented!(),
        }
    }
}
```

---

## 5. Modern Features & Enhancements

### 5.1 Observability

**OpenTelemetry integration:**

```rust
use opentelemetry::{global, KeyValue, trace::Tracer};
use opentelemetry_otlp::WithExportConfig;

pub fn init_telemetry(config: &Config) -> Result<()> {
    // Traces
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otel_endpoint)
        )
        .with_trace_config(
            opentelemetry::sdk::trace::config()
                .with_resource(opentelemetry::sdk::Resource::new(vec![
                    KeyValue::new("service.name", "guacr"),
                    KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                ]))
        )
        .install_batch(opentelemetry::runtime::Tokio)?;

    // Metrics
    let meter = opentelemetry_otlp::new_pipeline()
        .metrics(opentelemetry::runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&config.otel_endpoint)
        )
        .build()?;

    global::set_tracer_provider(tracer);
    global::set_meter_provider(meter);

    Ok(())
}

// Usage in connection handling
#[instrument(skip(stream), fields(client.id = %client_id, protocol = %protocol))]
async fn handle_connection(stream: TcpStream, client_id: Uuid, protocol: String) {
    let span = Span::current();
    span.set_attribute(KeyValue::new("client.address", format!("{}", stream.peer_addr())));

    // Connection metrics
    CONNECTION_COUNTER.add(1, &[KeyValue::new("protocol", protocol.clone())]);

    // ... handle connection

    CONNECTION_DURATION.record(
        start_time.elapsed().as_secs_f64(),
        &[KeyValue::new("protocol", protocol)]
    );
}
```

**Prometheus metrics endpoint:**

```rust
use prometheus::{Encoder, TextEncoder, Registry};
use warp::Filter;

pub async fn start_metrics_server(port: u16) -> Result<()> {
    let registry = Registry::new();

    // Register metrics
    registry.register(Box::new(ACTIVE_CONNECTIONS.clone()))?;
    registry.register(Box::new(TOTAL_CONNECTIONS.clone()))?;
    registry.register(Box::new(CONNECTION_DURATION.clone()))?;

    let metrics_route = warp::path!("metrics")
        .map(move || {
            let encoder = TextEncoder::new();
            let metric_families = registry.gather();
            let mut buffer = vec![];
            encoder.encode(&metric_families, &mut buffer).unwrap();
            String::from_utf8(buffer).unwrap()
        });

    warp::serve(metrics_route)
        .run(([0, 0, 0, 0], port))
        .await;

    Ok(())
}
```

### 5.2 Health Checks

```rust
use axum::{Router, routing::get, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    status: String,
    version: String,
    uptime_seconds: u64,
    active_connections: usize,
    handlers: HashMap<String, HandlerHealth>,
}

#[derive(Serialize)]
pub struct HandlerHealth {
    enabled: bool,
    healthy: bool,
    error: Option<String>,
}

async fn health_handler(
    state: Arc<AppState>,
) -> Json<HealthResponse> {
    let handlers = state.handlers.read().await;
    let mut handler_health = HashMap::new();

    for (name, handler) in handlers.iter() {
        let health = handler.health_check().await;
        handler_health.insert(name.clone(), HandlerHealth {
            enabled: true,
            healthy: health.is_ok(),
            error: health.err().map(|e| e.to_string()),
        });
    }

    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
        active_connections: state.sessions.read().await.len(),
        handlers: handler_health,
    })
}

pub fn health_routes() -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/health/live", get(|| async { "OK" }))
        .route("/health/ready", get(readiness_handler))
}
```

### 5.3 Dynamic Configuration

**Hot reload configuration without restart:**

```rust
use notify::{Watcher, RecursiveMode, Event};

pub async fn watch_config(
    config_path: PathBuf,
    app_state: Arc<AppState>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(10);

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            tx.blocking_send(event).ok();
        }
    })?;

    watcher.watch(&config_path, RecursiveMode::NonRecursive)?;

    while let Some(event) = rx.recv().await {
        match event.kind {
            notify::EventKind::Modify(_) => {
                info!("Configuration file changed, reloading...");

                match Config::load(&config_path) {
                    Ok(new_config) => {
                        apply_config_changes(&app_state, new_config).await?;
                        info!("Configuration reloaded successfully");
                    },
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                    }
                }
            },
            _ => {}
        }
    }

    Ok(())
}

async fn apply_config_changes(
    state: &AppState,
    new_config: Config,
) -> Result<()> {
    // Update logging level
    if state.config.read().await.logging.level != new_config.logging.level {
        tracing::level_filters::LevelFilter::from_str(&new_config.logging.level)?
            .apply();
    }

    // Update protocol handler configs
    for (protocol, handler_config) in &new_config.protocols {
        if let Some(handler) = state.handlers.read().await.get(protocol) {
            handler.update_config(handler_config).await?;
        }
    }

    // Update state
    *state.config.write().await = new_config;

    Ok(())
}
```

### 5.4 Security Enhancements

**TLS support:**

```rust
use tokio_rustls::{TlsAcceptor, rustls::ServerConfig};
use rustls_pemfile::{certs, rsa_private_keys};

pub async fn create_tls_acceptor(config: &Config) -> Result<TlsAcceptor> {
    let cert_file = File::open(&config.tls_cert)?;
    let key_file = File::open(&config.tls_key)?;

    let certs = certs(&mut BufReader::new(cert_file))?
        .into_iter()
        .map(Certificate)
        .collect();

    let mut keys = rsa_private_keys(&mut BufReader::new(key_file))?;
    let key = PrivateKey(keys.remove(0));

    let server_config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

// Use in listener
let acceptor = create_tls_acceptor(&config).await?;
let stream = acceptor.accept(tcp_stream).await?;
```

**Rate limiting:**

```rust
use governor::{Quota, RateLimiter, clock::DefaultClock};

pub struct ConnectionRateLimiter {
    limiter: RateLimiter<String, DefaultClock>,
}

impl ConnectionRateLimiter {
    pub fn new(per_second: u32) -> Self {
        Self {
            limiter: RateLimiter::keyed(
                Quota::per_second(per_second.try_into().unwrap())
            ),
        }
    }

    pub async fn check(&self, ip: IpAddr) -> Result<()> {
        self.limiter
            .check_key(&ip.to_string())
            .map_err(|_| Error::RateLimitExceeded)?;
        Ok(())
    }
}
```

### 5.5 Session Recording & Audit

**Record all sessions to storage:**

```rust
pub struct SessionRecorder {
    storage: Box<dyn RecordingStorage>,
}

#[async_trait]
pub trait RecordingStorage: Send + Sync {
    async fn create_recording(&self, session_id: &str) -> Result<Box<dyn RecordingWriter>>;
    async fn get_recording(&self, session_id: &str) -> Result<Vec<u8>>;
}

#[async_trait]
pub trait RecordingWriter: Send + Sync {
    async fn write_instruction(&mut self, instruction: &GuacInstruction) -> Result<()>;
    async fn finalize(&mut self) -> Result<()>;
}

// S3 storage implementation
pub struct S3RecordingStorage {
    client: aws_sdk_s3::Client,
    bucket: String,
}

#[async_trait]
impl RecordingStorage for S3RecordingStorage {
    async fn create_recording(&self, session_id: &str) -> Result<Box<dyn RecordingWriter>> {
        Ok(Box::new(S3RecordingWriter {
            client: self.client.clone(),
            bucket: self.bucket.clone(),
            session_id: session_id.to_string(),
            buffer: Vec::new(),
        }))
    }

    async fn get_recording(&self, session_id: &str) -> Result<Vec<u8>> {
        let output = self.client
            .get_object()
            .bucket(&self.bucket)
            .key(&format!("recordings/{}.guac", session_id))
            .send()
            .await?;

        let bytes = output.body.collect().await?.into_bytes();
        Ok(bytes.to_vec())
    }
}

// Usage in connection handler
let recorder = state.recorder.create_recording(&session_id).await?;

// Wrap instruction streams with recorder
let recording_tx = RecordingChannel::new(guac_tx, recorder);
```

---

## 6. Technology Stack Summary

### 6.1 Core Dependencies

```toml
[dependencies]
# Async runtime with io_uring
tokio = { version = "1.35", features = ["full"] }
tokio-uring = "0.4"  # io_uring support for zero-copy I/O
tokio-util = "0.7"
async-trait = "0.1"

# Zero-copy and performance
bytes = "1.5"  # Reference-counted buffers
crossbeam = "0.8"  # Lock-free data structures
parking_lot = "0.12"  # Fast RwLock
flume = "0.11"  # Fast MPSC channels
typed-arena = "2.0"  # Arena allocation
bumpalo = "3.14"  # Bump allocator

# SIMD and platform-specific optimizations
# (Automatically uses AVX2, NEON when available)
wide = "0.7"  # SIMD abstractions

# Shared memory
shared_memory = "0.12"  # Cross-process zero-copy IPC
memmap2 = "0.9"  # Memory-mapped files

# Protocol implementation
base64 = "0.21"

# Networking
tonic = "0.10"  # gRPC
prost = "0.12"  # Protobuf
axum = "0.7"    # HTTP/WebSocket
tower = "0.4"
hyper = "1.0"

# Protocol handlers
russh = "0.40"  # SSH
russh-keys = "0.40"
# For RDP: ironrdp or rdp-rs
ironrdp = "0.1"
# For VNC: async-vnc
async-vnc = "0.2"
# Database
sqlx = { version = "0.7", features = ["runtime-tokio", "mysql", "postgres"] }
tiberius = "0.12"  # SQL Server
# Browser
headless-chrome = "1.0"

# Terminal emulation
vt100 = "0.15"

# Configuration
serde = { version = "1.0", features = ["derive"] }
toml = "0.8"
config = "0.13"

# Logging & observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
tracing-opentelemetry = "0.22"
opentelemetry = "0.21"
opentelemetry-otlp = "0.14"
prometheus = "0.13"

# Service discovery
consul = "0.4"

# TLS
tokio-rustls = "0.25"
rustls = "0.22"
rustls-pemfile = "2.0"

# Rate limiting
governor = "0.6"

# Utilities
uuid = { version = "1.6", features = ["v4", "serde"] }
anyhow = "1.0"
thiserror = "1.0"
lazy_static = "1.4"
parking_lot = "0.12"

# Cloud storage (optional)
aws-sdk-s3 = "1.10"

# Image processing
image = "0.24"
png = "0.17"

# File watching
notify = "6.1"
```

### 6.2 Project Structure

```
guacr/
├── Cargo.toml
├── crates/
│   ├── guacr-daemon/          # Main daemon binary
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── server.rs      # TCP/WebSocket/gRPC servers
│   │   │   ├── connection.rs  # Connection management
│   │   │   ├── config.rs      # Configuration
│   │   │   └── telemetry.rs   # Observability
│   │   └── Cargo.toml
│   │
│   ├── guacr-protocol/        # Guacamole protocol implementation
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── instruction.rs # Instruction parsing/serialization
│   │   │   ├── codec.rs       # Tokio codec
│   │   │   ├── handshake.rs   # Connection handshake
│   │   │   └── types.rs       # Protocol types
│   │   └── Cargo.toml
│   │
│   ├── guacr-handlers/        # Protocol handler trait & registry
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── trait.rs       # ProtocolHandler trait
│   │   │   ├── registry.rs    # Handler registry
│   │   │   └── common.rs      # Shared utilities
│   │   └── Cargo.toml
│   │
│   ├── guacr-ssh/             # SSH handler
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── handler.rs
│   │   │   ├── terminal.rs    # Terminal emulation
│   │   │   └── auth.rs        # Authentication
│   │   └── Cargo.toml
│   │
│   ├── guacr-telnet/          # Telnet handler
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   └── handler.rs
│   │   └── Cargo.toml
│   │
│   ├── guacr-rdp/             # RDP handler
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── handler.rs
│   │   │   ├── bitmap.rs      # Bitmap handling
│   │   │   └── input.rs       # Input events
│   │   └── Cargo.toml
│   │
│   ├── guacr-vnc/             # VNC handler
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   └── handler.rs
│   │   └── Cargo.toml
│   │
│   ├── guacr-database/        # Database handlers (MySQL, PostgreSQL, SQL Server)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── mysql.rs
│   │   │   ├── postgresql.rs
│   │   │   ├── sqlserver.rs
│   │   │   ├── terminal.rs    # SQL terminal UI
│   │   │   └── autocomplete.rs
│   │   └── Cargo.toml
│   │
│   ├── guacr-rbi/             # Remote Browser Isolation
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── handler.rs
│   │   │   ├── chrome.rs      # Chrome/Chromium integration
│   │   │   ├── capture.rs     # Screen capture
│   │   │   └── input.rs       # Input handling
│   │   └── Cargo.toml
│   │
│   ├── guacr-terminal/        # Terminal emulation library (shared)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── vt100.rs       # VT100/ANSI parsing
│   │   │   ├── screen.rs      # Screen buffer
│   │   │   └── render.rs      # Text-to-PNG rendering
│   │   └── Cargo.toml
│   │
│   ├── guacr-grpc/            # gRPC service implementation
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── service.rs     # gRPC service impl
│   │   │   └── client.rs      # gRPC client
│   │   ├── proto/
│   │   │   └── guacr.proto
│   │   ├── build.rs           # Protobuf compilation
│   │   └── Cargo.toml
│   │
│   └── guacr-recorder/        # Session recording
│       ├── src/
│       │   ├── lib.rs
│       │   ├── recorder.rs
│       │   ├── storage/
│       │   │   ├── mod.rs
│       │   │   ├── local.rs   # Local filesystem
│       │   │   ├── s3.rs      # AWS S3
│       │   │   └── azure.rs   # Azure Blob
│       │   └── player.rs      # Playback
│       └── Cargo.toml
│
├── proto/                     # Protobuf definitions
│   └── guacr.proto
│
├── config/                    # Example configs
│   ├── guacr.toml
│   └── guacr-distributed.toml
│
├── docker/                    # Docker files
│   ├── Dockerfile
│   └── docker-compose.yml
│
├── k8s/                       # Kubernetes manifests
│   ├── deployment.yaml
│   ├── service.yaml
│   └── configmap.yaml
│
└── docs/                      # Documentation
    ├── architecture.md
    ├── protocol.md
    ├── handlers.md
    └── deployment.md
```

---

## 7. Implementation Roadmap

### Phase 1: Foundation (Weeks 1-4)

**Goals: Core daemon + Guacamole protocol + SSH handler**

- [ ] Set up project structure and workspace
- [ ] Implement Guacamole protocol parser/codec
- [ ] Build core daemon with Tokio async runtime
- [ ] Implement TCP listener and connection handling
- [ ] Build SSH handler with terminal emulation
- [ ] Basic configuration system (TOML)
- [ ] Structured logging with tracing
- [ ] Unit and integration tests

**Deliverable:** Working guacr that can handle SSH connections via Guacamole protocol

### Phase 2: Additional Protocols (Weeks 5-8)

**Goals: Telnet + VNC + Database handlers**

- [ ] Implement Telnet handler
- [ ] Implement VNC handler (using async-vnc)
- [ ] Implement MySQL handler with sqlx
- [ ] Implement PostgreSQL handler with sqlx
- [ ] Implement SQL Server handler with tiberius
- [ ] SQL terminal emulation and formatting
- [ ] SQL autocompletion
- [ ] Protocol handler registry and dynamic loading

**Deliverable:** guacr supporting SSH, Telnet, VNC, and database protocols

### Phase 3: RDP Support (Weeks 9-12)

**Goals: RDP handler (complex)**

- [ ] Evaluate ironrdp vs FFI to FreeRDP
- [ ] Implement RDP connection and authentication
- [ ] Bitmap rendering and display updates
- [ ] Input event handling (mouse, keyboard)
- [ ] Audio redirection
- [ ] Clipboard integration
- [ ] Session persistence

**Deliverable:** guacr with working RDP support

### Phase 4: Browser Isolation (Weeks 13-16)

**Goals: RBI/HTTP handler**

- [ ] Integrate headless Chrome/Chromium
- [ ] Implement Chrome DevTools Protocol communication
- [ ] Screen capture and streaming
- [ ] Input event injection (mouse, keyboard)
- [ ] Clipboard integration
- [ ] URL filtering and security policies
- [ ] Keeper credential autofill integration
- [ ] TOTP generation and autofill

**Deliverable:** guacr with RBI support and Keeper-specific features

### Phase 5: Distribution Layer (Weeks 17-20)

**Goals: gRPC service mesh and load balancing**

- [ ] Define gRPC service protocol (protobuf)
- [ ] Implement gRPC service with Tonic
- [ ] Build gRPC client for distributed setup
- [ ] Implement service discovery (Consul integration)
- [ ] Client-side load balancing
- [ ] Health checks and metrics endpoints
- [ ] Session migration/failover

**Deliverable:** Distributed guacr cluster with load balancing

### Phase 6: Observability & Operations (Weeks 21-24)

**Goals: Production readiness**

- [ ] OpenTelemetry integration (traces, metrics, logs)
- [ ] Prometheus metrics exporter
- [ ] Grafana dashboards
- [ ] Health and readiness probes
- [ ] Dynamic configuration hot reload
- [ ] Session recording to S3/Azure
- [ ] Playback UI for recorded sessions
- [ ] Rate limiting and DDoS protection
- [ ] TLS/mTLS support
- [ ] Comprehensive documentation

**Deliverable:** Production-ready guacr with full observability

### Phase 7: Migration & Compatibility (Weeks 25-28)

**Goals: Migration tools and testing**

- [ ] Configuration migration tool (guacd.conf → guacr.toml)
- [ ] Side-by-side deployment guide
- [ ] Performance benchmarks vs original guacd
- [ ] Load testing (JMeter scripts)
- [ ] Security audit and penetration testing
- [ ] Compliance verification (FIPS, SOC2, etc.)
- [ ] Migration playbook
- [ ] Rollback procedures

**Deliverable:** Complete migration path from guacd to guacr

### Phase 8: Advanced Features (Weeks 29+)

**Goals: Beyond parity**

- [ ] WebRTC support for better performance
- [ ] H.264 video encoding for screen updates
- [ ] Multi-monitor support
- [ ] File transfer optimization
- [ ] Connection sharing/collaboration
- [ ] AI-powered session anomaly detection
- [ ] Automated credential rotation
- [ ] Zero-trust networking integration

**Deliverable:** Feature-rich guacr exceeding guacd capabilities

---

## 8. Performance Targets

### Latency
- **Handshake**: < 50ms
- **Key press to screen update**: < 100ms (LAN), < 300ms (WAN)
- **Mouse movement**: < 50ms
- **Frame rate**: 30+ FPS for RDP/VNC

### Throughput
- **Concurrent connections**: 10,000+ per instance
- **Bandwidth per connection**: 1-10 Mbps (depending on protocol)
- **CPU usage**: < 0.1% per idle connection
- **Memory**: < 10MB per connection

### Scalability
- **Horizontal scaling**: Linear with instance count
- **Connection distribution**: < 5% variance across instances
- **Failover time**: < 5 seconds

---

## 9. Security Considerations

### Connection Security
- TLS 1.3 for all network connections
- Certificate pinning for gRPC mesh
- mTLS for service-to-service communication

### Authentication
- Support for multiple auth methods per protocol
- Integration with Keeper Vault for credential management
- MFA/TOTP support
- Certificate-based authentication

### Session Security
- Encrypted session recordings
- Audit logs for all connections
- Configurable clipboard restrictions
- File transfer policies

### Network Security
- Rate limiting per IP
- DDoS protection (connection limits)
- IP whitelisting/blacklisting
- VPC/network segmentation support

### Compliance
- FIPS 140-2 cryptographic module
- SOC 2 audit readiness
- GDPR data handling
- Session recording retention policies

---

## 10. Testing Strategy

### Unit Tests
- Protocol parser (instruction encoding/decoding)
- Handler trait implementations
- Configuration loading
- Terminal emulation

### Integration Tests
- End-to-end connection flows
- Protocol handler integration
- gRPC service
- Session recording

### Performance Tests
- Connection limit testing (10k+ connections)
- Latency measurements
- Throughput benchmarks
- Memory profiling

### Compatibility Tests
- Test with original guacamole-client
- Cross-browser testing (Chrome, Firefox, Safari)
- Protocol version compatibility

### Security Tests
- Penetration testing
- Fuzzing protocol parser
- Credential handling audit
- TLS configuration verification

---

## 11. Migration Strategy

### Gradual Rollout

```
Phase 1: Development environment
  ├─ Deploy guacr alongside guacd
  ├─ Route 10% of dev traffic to guacr
  └─ Monitor metrics and logs

Phase 2: Staging environment
  ├─ Full replacement in staging
  ├─ Run load tests
  └─ Validate all protocols

Phase 3: Production (canary)
  ├─ Deploy to single region
  ├─ Route 5% of production traffic
  ├─ Monitor for 1 week
  └─ Rollback if issues detected

Phase 4: Production (blue-green)
  ├─ Deploy full guacr cluster (green)
  ├─ Route 50% of traffic
  ├─ Monitor for 3 days
  └─ Switch 100% to green

Phase 5: Decommission guacd
  ├─ Keep guacd in standby for 2 weeks
  ├─ Remove guacd infrastructure
  └─ Archive old configs/logs
```

### Configuration Migration

```bash
# guacr-migrate tool
guacr-migrate --from /etc/guacd/guacd.conf --to /etc/guacr/guacr.toml

# Validation
guacr config validate /etc/guacr/guacr.toml

# Dry run
guacr --config /etc/guacr/guacr.toml --dry-run
```

### Monitoring During Migration

```yaml
# Critical metrics to watch
- connection_success_rate > 99.9%
- p99_latency < 200ms
- error_rate < 0.1%
- memory_usage < 10GB per instance
- cpu_usage < 50% per instance
```

---

## 12. Open Questions & Decisions

### Terminal Rendering
**Question:** How to efficiently render terminal output to PNG for SSH/Telnet?

**Options:**
1. Use `fontdue` + `image` crate to render text
2. Use `cairo-rs` for text rendering
3. Use `raqote` (pure Rust 2D graphics)
4. Pre-render font atlas and compose characters

**Recommendation:** Start with option 1 (fontdue), evaluate performance

### RDP Implementation
**Question:** Use pure Rust `ironrdp` or FFI to FreeRDP?

**Comparison:**
| Factor | ironrdp | FreeRDP FFI |
|--------|---------|-------------|
| Memory safety |  Safe | ⚠️ Unsafe |
| Feature completeness | ⚠️ Limited |  Complete |
| Maintenance | ⚠️ Young project |  Mature |
| Performance |  Good |  Excellent |
| Async support |  Native | ⚠️ Requires wrapper |

**Recommendation:** Try `ironrdp` first, fall back to FFI if features are missing

### Browser Isolation
**Question:** Headless Chrome or continue with CEF?

**Recommendation:** Start with headless Chrome (easier integration), consider CEF later for performance

### Distributed Architecture
**Question:** Keep ZeroMQ or switch to gRPC?

**Recommendation:** Use gRPC for better cloud-native support, observability, and load balancing

---

## 13. Success Criteria

### Functional
-  All protocols working (SSH, Telnet, RDP, VNC, Database, RBI)
-  Compatible with existing guacamole-client
-  Feature parity with current KCM guacd
-  All Keeper-specific features (credential autofill, TOTP, recording)

### Performance
-  10,000+ concurrent connections per instance
-  < 100ms latency for input events
-  < 10MB memory per connection
-  30+ FPS for graphical protocols

### Operations
-  Zero-downtime deployments
-  Hot configuration reload
-  Comprehensive metrics and tracing
-  < 5 second failover time

### Security
-  Memory-safe (no segfaults)
-  TLS/mTLS support
-  Audit logging
-  FIPS compliance

---

## Conclusion

This plan outlines a comprehensive rewrite of guacd in Rust as **guacr**, embracing modern async I/O, cloud-native architecture, and best-in-class observability. The phased approach allows for incremental development and validation, with early delivery of core functionality and gradual addition of advanced features.

**Key Advantages of guacr:**
- **Memory safety**: No more segfaults or memory leaks
- **Performance**: Async I/O enables 10,000+ connections per instance
- **Cloud-native**: Built for Kubernetes, service mesh, observability
- **Maintainable**: Clean Rust code, strong typing, excellent tooling
- **Modern**: gRPC, OpenTelemetry, Prometheus, hot reload

**Timeline:** ~28 weeks for production-ready system with all features

**Next Steps:**
1. Review and approve architectural plan
2. Set up development environment and project structure
3. Begin Phase 1: Core daemon + protocol + SSH handler
4. Establish CI/CD pipeline and testing framework