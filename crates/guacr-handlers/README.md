# guacr-handlers

Protocol handler trait and registry for guacr remote desktop protocols.

## Overview

This crate provides the foundation for implementing remote desktop protocol handlers (SSH, RDP, VNC, Database, RBI/HTTP) that work with the keeper-webrtc WebRTC infrastructure.

**Status:** STABLE v1.0.0 - Interface is locked

## What's Provided

### ProtocolHandler Trait

The core trait that all protocol implementations must implement:

```rust
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    fn name(&self) -> &str;

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<()>;

    // Optional methods with default implementations
    async fn disconnect(&self) -> Result<()>;
    async fn health_check(&self) -> Result<HealthStatus>;
    async fn stats(&self) -> Result<HandlerStats>;
    async fn initialize(&self, config: HashMap<String, String>) -> Result<()>;
}
```

### ProtocolHandlerRegistry

Thread-safe registry using DashMap for lock-free concurrent access:

```rust
let registry = ProtocolHandlerRegistry::new();
registry.register(SshHandler::new());
registry.register(RdpHandler::new());

// Later, retrieve handler
if let Some(handler) = registry.get("ssh") {
    handler.connect(params, to_client, from_client).await?;
}
```

### MockProtocolHandler

For testing without real protocol implementations:

```rust
#[cfg(test)]
use guacr_handlers::MockProtocolHandler;

let mock = MockProtocolHandler::new("ssh");
registry.register(mock);
```

### Integration Helper

```rust
use guacr_handlers::handle_guacd_with_handlers;

// This function handles the complete flow:
// 1. Looks up protocol handler by name
// 2. Spawns handler task
// 3. Forwards messages bidirectionally
handle_guacd_with_handlers(
    "ssh".to_string(),
    params,
    registry,
    to_webrtc_tx,
    from_webrtc_rx,
).await?;
```

## Message Protocol

Messages are Guacamole protocol instructions as raw `Bytes`.

**Parse with GuacdParser** (from keeper-webrtc crate):
```rust
use keeper_webrtc::channel::guacd_parser::GuacdParser;

let instruction = GuacdParser::parse_instruction(&bytes)?;
match instruction.opcode.as_str() {
    "key" => { /* handle keyboard */ },
    "mouse" => { /* handle mouse */ },
    _ => {}
}
```

**Format instructions:**
```rust
fn format_guacamole_instruction(opcode: &str, args: &[&str]) -> Bytes {
    let mut result = String::new();
    result.push_str(&opcode.len().to_string());
    result.push('.');
    result.push_str(opcode);

    for arg in args {
        result.push(',');
        result.push_str(&arg.len().to_string());
        result.push('.');
        result.push_str(arg);
    }

    result.push(';');
    Bytes::from(result)
}
```

## Common Guacamole Instructions

**From Client:**
- `key` - Keyboard: [keysym, pressed]
- `mouse` - Mouse: [x, y, button_mask]
- `clipboard` - Clipboard: [mimetype, data]
- `size` - Screen resize: [width, height]

**To Client:**
- `img` - Image: [layer, mimetype, base64_data]
- `sync` - Frame sync: [timestamp]
- `audio` - Audio: [mimetype, data]

## For Protocol Implementers

See `../../docs/guacr/INTERFACE_STABLE.md` for:
- Complete trait specification
- Parameter naming conventions
- Testing requirements
- Change management process

See `../../docs/guacr/docs/context/GUACR_PROTOCOL_HANDLERS_GUIDE.md` for:
- Implementation patterns
- Terminal protocols (SSH, Telnet)
- Graphical protocols (RDP, VNC)
- Database protocols
- RBI/HTTP handler

## Testing

```bash
cargo test -p guacr-handlers
```

Current test coverage:
- 11 unit tests
- 3 doc tests
- Total: 14 tests, all passing

## Dependencies

- tokio - Async runtime
- async-trait - Async trait support
- bytes - Zero-copy byte buffers
- dashmap - Lock-free concurrent HashMap
- thiserror - Error handling
- log - Logging facade

## Next Steps for Implementers

1. Create your protocol crate (e.g., `guacr-ssh`)
2. Add dependency on `guacr-handlers`
3. Implement `ProtocolHandler` trait
4. Write tests
5. Register with ProtocolHandlerRegistry

See `STATUS.md` for coordination with other instances.
