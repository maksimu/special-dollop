# Guacr Quick Start Guide

This guide will walk you through setting up the guacr project from scratch and implementing your first protocol handler.

---

## Prerequisites

### Required Tools

```bash
# Rust toolchain (1.75+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Protocol Buffers compiler (for gRPC)
# macOS:
brew install protobuf

# Linux:
apt-get install protobuf-compiler

# Additional development tools
cargo install cargo-watch  # Auto-reload during development
cargo install cargo-expand  # Inspect macro expansions
cargo install cargo-audit   # Security auditing
```

### System Libraries

```bash
# macOS:
brew install pkg-config openssl

# Ubuntu/Debian:
apt-get install build-essential pkg-config libssl-dev

# For SSH support:
apt-get install libssh2-1-dev

# For terminal rendering:
apt-get install libfreetype6-dev
```

---

## Project Setup

### 1. Create Workspace

```bash
mkdir guacr
cd guacr

# Initialize workspace
cat > Cargo.toml <<'EOF'
[workspace]
members = [
    "crates/guacr-daemon",
    "crates/guacr-protocol",
    "crates/guacr-handlers",
    "crates/guacr-ssh",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
authors = ["Your Name <you@example.com>"]
license = "MIT OR Apache-2.0"

[workspace.dependencies]
tokio = { version = "1.35", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
async-trait = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
serde = { version = "1.0", features = ["derive"] }
anyhow = "1.0"
thiserror = "1.0"
bytes = "1.5"

[profile.release]
lto = true
codegen-units = 1
opt-level = 3
EOF
```

### 2. Create Protocol Crate

```bash
cargo new --lib crates/guacr-protocol
cd crates/guacr-protocol
```

**crates/guacr-protocol/Cargo.toml:**

```toml
[package]
name = "guacr-protocol"
version.workspace = true
edition.workspace = true

[dependencies]
tokio.workspace = true
tokio-util.workspace = true
bytes.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

**crates/guacr-protocol/src/lib.rs:**

```rust
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub opcode: String,
    pub args: Vec<String>,
}

impl Instruction {
    pub fn new(opcode: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            opcode: opcode.into(),
            args,
        }
    }
}

pub struct GuacCodec;

impl Decoder for GuacCodec {
    type Item = Instruction;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Find terminator (semicolon)
        if let Some(semicolon_pos) = src.iter().position(|&b| b == b';') {
            // Extract instruction data
            let line = src.split_to(semicolon_pos + 1);

            // Parse instruction: LENGTH.ELEMENT,LENGTH.ELEMENT,...;
            let instruction = parse_instruction(&line[..line.len() - 1])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            Ok(Some(instruction))
        } else {
            // Need more data
            Ok(None)
        }
    }
}

impl Encoder<Instruction> for GuacCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Instruction, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Format: LENGTH.OPCODE,LENGTH.ARG1,LENGTH.ARG2,...;

        // Opcode
        write_element(&item.opcode, dst);

        // Arguments
        for arg in &item.args {
            dst.put_u8(b',');
            write_element(arg, dst);
        }

        dst.put_u8(b';');

        Ok(())
    }
}

fn write_element(s: &str, dst: &mut BytesMut) {
    let bytes = s.as_bytes();
    let len = bytes.len();

    // Write length
    dst.put_slice(len.to_string().as_bytes());
    dst.put_u8(b'.');

    // Write content
    dst.put_slice(bytes);
}

fn parse_instruction(data: &[u8]) -> Result<Instruction, String> {
    let s = std::str::from_utf8(data).map_err(|e| e.to_string())?;

    let elements: Vec<&str> = s.split(',').collect();

    if elements.is_empty() {
        return Err("Empty instruction".to_string());
    }

    let mut result = Vec::new();

    for element in elements {
        let parts: Vec<&str> = element.splitn(2, '.').collect();

        if parts.len() != 2 {
            return Err(format!("Invalid element: {}", element));
        }

        let len: usize = parts[0].parse().map_err(|e| format!("Invalid length: {}", e))?;
        let content = parts[1];

        if content.len() != len {
            return Err(format!(
                "Length mismatch: expected {}, got {}",
                len,
                content.len()
            ));
        }

        result.push(content.to_string());
    }

    if result.is_empty() {
        return Err("No elements in instruction".to_string());
    }

    let opcode = result.remove(0);
    Ok(Instruction { opcode, args: result })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_instruction() {
        let data = b"5.mouse,1.0,2.10,3.120";
        let instruction = parse_instruction(data).unwrap();

        assert_eq!(instruction.opcode, "mouse");
        assert_eq!(instruction.args, vec!["0", "10", "120"]);
    }

    #[test]
    fn test_encode_instruction() {
        let mut codec = GuacCodec;
        let mut buf = BytesMut::new();

        let instruction = Instruction::new("sync", vec!["12345".to_string()]);

        codec.encode(instruction, &mut buf).unwrap();

        assert_eq!(&buf[..], b"4.sync,5.12345;");
    }

    #[test]
    fn test_decode_instruction() {
        let mut codec = GuacCodec;
        let mut buf = BytesMut::from(&b"3.key,6.65293,1.1;"[..]);

        let instruction = codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(instruction.opcode, "key");
        assert_eq!(instruction.args, vec!["65293", "1"]);
    }
}
```

### 3. Create Handler Trait Crate

```bash
cargo new --lib crates/guacr-handlers
cd crates/guacr-handlers
```

**crates/guacr-handlers/Cargo.toml:**

```toml
[package]
name = "guacr-handlers"
version.workspace = true
edition.workspace = true

[dependencies]
guacr-protocol = { path = "../guacr-protocol" }
tokio.workspace = true
async-trait.workspace = true
anyhow.workspace = true
thiserror.workspace = true
std::collections::HashMap
parking_lot = "0.12"
```

**crates/guacr-handlers/src/lib.rs:**

```rust
use async_trait::async_trait;
use guacr_protocol::Instruction;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Error, Debug)]
pub enum HandlerError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Missing parameter: {0}")]
    MissingParameter(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, HandlerError>;

#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Unhealthy { reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct HandlerStats {
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Protocol name (e.g., "ssh", "rdp", "vnc")
    fn name(&self) -> &str;

    /// Connect to remote host
    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<Instruction>,
        guac_rx: mpsc::Receiver<Instruction>,
    ) -> Result<()>;

    /// Disconnect (optional, cleanup on drop)
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    /// Health check
    async fn health_check(&self) -> Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    /// Get statistics
    async fn stats(&self) -> Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

/// Registry for protocol handlers
pub struct HandlerRegistry {
    handlers: parking_lot::RwLock<HashMap<String, Arc<dyn ProtocolHandler>>>,
}

impl HandlerRegistry {
    pub fn new() -> Self {
        Self {
            handlers: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    pub fn register<H: ProtocolHandler + 'static>(&self, handler: H) {
        let name = handler.name().to_string();
        self.handlers.write().insert(name, Arc::new(handler));
    }

    pub fn get(&self, protocol: &str) -> Option<Arc<dyn ProtocolHandler>> {
        self.handlers.read().get(protocol).cloned()
    }

    pub fn list(&self) -> Vec<String> {
        self.handlers.read().keys().cloned().collect()
    }
}

impl Default for HandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

### 4. Create SSH Handler (First Implementation)

```bash
cargo new --lib crates/guacr-ssh
cd crates/guacr-ssh
```

**crates/guacr-ssh/Cargo.toml:**

```toml
[package]
name = "guacr-ssh"
version.workspace = true
edition.workspace = true

[dependencies]
guacr-protocol = { path = "../guacr-protocol" }
guacr-handlers = { path = "../guacr-handlers" }
tokio.workspace = true
async-trait.workspace = true
tracing.workspace = true
russh = "0.40"
russh-keys = "0.40"
vt100 = "0.15"
```

**crates/guacr-ssh/src/lib.rs:**

```rust
use async_trait::async_trait;
use guacr_handlers::{HandlerError, ProtocolHandler, Result};
use guacr_protocol::Instruction;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, instrument};

mod terminal;
use terminal::TerminalEmulator;

pub struct SshHandler {
    config: SshConfig,
}

#[derive(Debug, Clone)]
pub struct SshConfig {
    pub default_port: u16,
    pub keepalive_interval: u64,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            default_port: 22,
            keepalive_interval: 30,
        }
    }
}

impl SshHandler {
    pub fn new(config: SshConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl ProtocolHandler for SshHandler {
    fn name(&self) -> &str {
        "ssh"
    }

    #[instrument(skip(self, guac_tx, guac_rx))]
    async fn connect(
        &self,
        params: HashMap<String, String>,
        guac_tx: mpsc::Sender<Instruction>,
        mut guac_rx: mpsc::Receiver<Instruction>,
    ) -> Result<()> {
        // Extract parameters
        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let username = params
            .get("username")
            .ok_or_else(|| HandlerError::MissingParameter("username".to_string()))?;

        info!("Connecting to {}:{}@{}", username, port, hostname);

        // TODO: Implement actual SSH connection using russh
        // This is a stub that shows the structure

        // Send initial screen
        let mut terminal = TerminalEmulator::new(24, 80);
        terminal.write_line("Connected to SSH server (stub)");

        let screen_png = terminal.render()?;
        guac_tx
            .send(Instruction::new(
                "img",
                vec![
                    "0".to_string(),
                    "image/png".to_string(),
                    base64::encode(&screen_png),
                ],
            ))
            .await
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Handle input
        while let Some(instruction) = guac_rx.recv().await {
            match instruction.opcode.as_str() {
                "key" => {
                    info!("Received key: {:?}", instruction.args);
                    // TODO: Forward to SSH
                }
                "clipboard" => {
                    info!("Received clipboard");
                    // TODO: Handle clipboard
                }
                _ => {}
            }
        }

        info!("SSH connection closed");

        Ok(())
    }
}

mod base64 {
    use std::io::Write;

    pub fn encode(data: &[u8]) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
    }
}
```

**crates/guacr-ssh/src/terminal.rs:**

```rust
use guacr_handlers::Result;
use vt100::Parser;

pub struct TerminalEmulator {
    parser: Parser,
}

impl TerminalEmulator {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Parser::new(rows, cols, 0),
        }
    }

    pub fn write_line(&mut self, text: &str) {
        self.parser.process(format!("{}\r\n", text).as_bytes());
    }

    pub fn render(&self) -> Result<Vec<u8>> {
        // TODO: Implement actual rendering
        // For now, return empty PNG
        Ok(create_stub_png())
    }
}

fn create_stub_png() -> Vec<u8> {
    // Minimal valid PNG (1x1 black pixel)
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, // IHDR length
        0x49, 0x48, 0x44, 0x52, // IHDR
        0x00, 0x00, 0x00, 0x01, // Width: 1
        0x00, 0x00, 0x00, 0x01, // Height: 1
        0x08, 0x02, 0x00, 0x00, 0x00, // Bit depth, color type, etc.
        0x90, 0x77, 0x53, 0xDE, // CRC
        0x00, 0x00, 0x00, 0x0C, // IDAT length
        0x49, 0x44, 0x41, 0x54, // IDAT
        0x08, 0xD7, 0x63, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
        0x33, // Data + CRC
        0x00, 0x00, 0x00, 0x00, // IEND length
        0x49, 0x45, 0x4E, 0x44, // IEND
        0xAE, 0x42, 0x60, 0x82, // CRC
    ]
}
```

### 5. Create Main Daemon

```bash
cargo new crates/guacr-daemon
cd crates/guacr-daemon
```

**crates/guacr-daemon/Cargo.toml:**

```toml
[package]
name = "guacr-daemon"
version.workspace = true
edition.workspace = true

[dependencies]
guacr-protocol = { path = "../guacr-protocol" }
guacr-handlers = { path = "../guacr-handlers" }
guacr-ssh = { path = "../guacr-ssh" }
tokio.workspace = true
tokio-util.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
anyhow.workspace = true
clap = { version = "4.4", features = ["derive"] }
```

**crates/guacr-daemon/src/main.rs:**

```rust
use anyhow::Result;
use clap::Parser;
use guacr_handlers::HandlerRegistry;
use guacr_ssh::{SshConfig, SshHandler};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

mod connection;

#[derive(Parser)]
#[command(name = "guacr")]
#[command(about = "Modern Guacamole daemon in Rust", long_about = None)]
struct Cli {
    #[arg(short, long, default_value = "0.0.0.0:4822")]
    bind: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .init();

    let cli = Cli::parse();

    info!("Guacr v{} starting...", env!("CARGO_PKG_VERSION"));

    // Create handler registry
    let registry = Arc::new(HandlerRegistry::new());

    // Register SSH handler
    let ssh_handler = SshHandler::new(SshConfig::default());
    registry.register(ssh_handler);

    info!("Registered handlers: {:?}", registry.list());

    // Start server
    let listener = TcpListener::bind(&cli.bind).await?;
    info!("Listening on {}", cli.bind);

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("New connection from {}", addr);

                let registry = Arc::clone(&registry);

                tokio::spawn(async move {
                    if let Err(e) = connection::handle_connection(stream, registry).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {}", e);
            }
        }
    }
}
```

**crates/guacr-daemon/src/connection.rs:**

```rust
use anyhow::{Context, Result};
use guacr_handlers::HandlerRegistry;
use guacr_protocol::{GuacCodec, Instruction};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{error, info, instrument};

#[instrument(skip(stream, registry))]
pub async fn handle_connection(stream: TcpStream, registry: Arc<HandlerRegistry>) -> Result<()> {
    let mut framed = Framed::new(stream, GuacCodec);

    // Read handshake
    let handshake = read_handshake(&mut framed).await?;

    info!("Handshake: protocol={}", handshake.protocol);

    // Get handler
    let handler = registry
        .get(&handshake.protocol)
        .ok_or_else(|| anyhow::anyhow!("Protocol not supported: {}", handshake.protocol))?;

    // Create channels
    let (guac_tx, guac_rx) = mpsc::channel(128);
    let (handler_tx, mut handler_rx) = mpsc::channel(128);

    // Spawn handler
    let handler_task = tokio::spawn(async move {
        handler
            .connect(handshake.params, handler_tx, guac_rx)
            .await
            .map_err(|e| anyhow::anyhow!("Handler error: {}", e))
    });

    // Forward loop
    let forward_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // Handler → Client
                Some(instruction) = handler_rx.recv() => {
                    if let Err(e) = framed.send(instruction).await {
                        error!("Send error: {}", e);
                        break;
                    }
                },

                // Client → Handler
                result = framed.next() => {
                    match result {
                        Some(Ok(instruction)) => {
                            if let Err(e) = guac_tx.send(instruction).await {
                                error!("Forward error: {}", e);
                                break;
                            }
                        },
                        Some(Err(e)) => {
                            error!("Receive error: {}", e);
                            break;
                        },
                        None => break,
                    }
                }
            }
        }
    });

    tokio::try_join!(handler_task, forward_task)?;

    info!("Connection closed");

    Ok(())
}

struct Handshake {
    protocol: String,
    params: HashMap<String, String>,
}

async fn read_handshake(
    framed: &mut Framed<TcpStream, GuacCodec>,
) -> Result<Handshake> {
    use futures::StreamExt;

    // 1. Expect "select" instruction
    let select = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Connection closed"))?
        .context("Failed to read select")?;

    if select.opcode != "select" {
        anyhow::bail!("Expected 'select', got '{}'", select.opcode);
    }

    let protocol = select.args.get(0)
        .ok_or_else(|| anyhow::anyhow!("No protocol in select"))?
        .clone();

    // 2. Send "args" instruction (request parameters)
    let args_list = get_protocol_args(&protocol);
    framed
        .send(Instruction::new("args", args_list))
        .await
        .context("Failed to send args")?;

    // 3. Expect "connect" instruction with parameters
    let connect = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Connection closed"))?
        .context("Failed to read connect")?;

    if connect.opcode != "connect" {
        anyhow::bail!("Expected 'connect', got '{}'", connect.opcode);
    }

    let params = parse_connect_args(&connect.args);

    // 4. Send "ready" instruction
    framed
        .send(Instruction::new("ready", vec!["connection-1".to_string()]))
        .await
        .context("Failed to send ready")?;

    Ok(Handshake { protocol, params })
}

fn get_protocol_args(protocol: &str) -> Vec<String> {
    match protocol {
        "ssh" => vec![
            "hostname".to_string(),
            "port".to_string(),
            "username".to_string(),
            "password".to_string(),
        ],
        _ => Vec::new(),
    }
}

fn parse_connect_args(args: &[String]) -> HashMap<String, String> {
    // For SSH: hostname, port, username, password
    let keys = get_protocol_args("ssh"); // TODO: use actual protocol

    keys.into_iter()
        .zip(args.iter())
        .map(|(k, v)| (k, v.clone()))
        .collect()
}
```

### 6. Build and Run

```bash
# From workspace root
cargo build

# Run daemon
cargo run --bin guacr-daemon

# In another terminal, test with netcat:
echo "6.select,3.ssh;" | nc localhost 4822
```

---

## Development Workflow

### Auto-rebuild on Changes

```bash
cargo watch -x 'run --bin guacr-daemon'
```

### Running Tests

```bash
# All tests
cargo test

# Specific crate
cargo test -p guacr-protocol

# With logging
RUST_LOG=debug cargo test -- --nocapture
```

### Linting and Formatting

```bash
# Format code
cargo fmt

# Run linter
cargo clippy -- -D warnings

# Security audit
cargo audit
```

---

## Next Steps

1. **Implement SSH Connection**: Replace stub with actual russh integration
2. **Add Terminal Rendering**: Use `image` crate to render terminal to PNG
3. **Add Telnet Handler**: Similar to SSH but simpler
4. **Add Configuration**: TOML config file support
5. **Add Metrics**: Prometheus metrics endpoint
6. **Add Tests**: Integration tests with mock servers

---

## Testing with Real Client

To test with the actual Guacamole web client:

1. Clone guacamole-client
2. Point it at `localhost:4822`
3. Create connection with SSH protocol
4. Should connect to guacr

---

## Troubleshooting

### "Cannot find crate" errors

```bash
# Make sure you're in workspace root
cargo build --workspace
```

### Protocol parse errors

Enable detailed logging:

```bash
RUST_LOG=guacr_protocol=trace cargo run --bin guacr-daemon
```

### Connection hangs

Check handshake sequence - make sure you're sending `args` and `ready` instructions.

---

## Resources

- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)
- [Guacamole Protocol Docs](https://guacamole.apache.org/doc/gug/guacamole-protocol.html)
- [Rust Async Book](https://rust-lang.github.io/async-book/)