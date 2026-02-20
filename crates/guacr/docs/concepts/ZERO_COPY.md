# Zero-Copy Integration with keeper-pam-webrtc-rs

## Overview

This document describes the zero-copy integration architecture between `pam-guacr` and `keeper-pam-webrtc-rs` for maximum performance.

## ✅ IMPLEMENTATION STATUS (2025-11-20)

**Bidirectional event-based interface is now implemented!**

All protocol handlers (SSH, RDP, VNC, Telnet, SFTP, Database) now support the `EventBasedHandler` trait with proper bidirectional communication:

- **Handler → WebRTC**: Zero-copy via `callback.send_instruction(Bytes)` 
- **WebRTC → Handler**: Via `from_client: mpsc::Receiver<Bytes>` parameter

The `EventBasedHandler::connect_with_events()` signature is:

```rust
async fn connect_with_events(
    &self,
    params: HashMap<String, String>,
    callback: Arc<dyn EventCallback>,
    from_client: mpsc::Receiver<Bytes>,  // ← Client messages (keyboard, mouse, etc.)
) -> Result<(), HandlerError>
```

This fixes the previous issue where `from_client` was created but the sender was immediately dropped, causing all client input to be lost.

## Current Architecture (Channel-Based)

```
keeper-pam-webrtc-rs
    ↓ mpsc::Sender<Bytes> (copy)
guacr-handlers
    ↓ mpsc::Receiver<Bytes> (copy)
Protocol Handler (SSH/RDP/etc)
    ↓ Process & encode
    ↓ mpsc::Sender<Bytes> (copy)
guacr-handlers
    ↓ mpsc::Receiver<Bytes> (copy)
keeper-pam-webrtc-rs
    ↓ Send via WebRTC
```

**Problem**: Multiple copies through channels, even though `Bytes` is reference-counted.

## Proposed Architecture (Zero-Copy)

```
keeper-pam-webrtc-rs
    ↓ Arc<EventCallback> (shared reference)
guacr-handlers
    ↓ Direct callback (zero-copy)
Protocol Handler
    ↓ Process & encode
    ↓ callback.send_instruction(Bytes) (zero-copy, refcounted)
keeper-pam-webrtc-rs
    ↓ Send via WebRTC (same Bytes, no copy)
```

**Benefits**:
- Zero copies: `Bytes` is reference-counted, shared between handler and WebRTC
- Direct callbacks: No channel overhead
- Event-based: Only critical events (error, disconnect, threat) need parsing

## Implementation

### 1. Move Parser to guacr-protocol

The parser is now in `guacr-protocol/src/parser.rs`:

```rust
use guacr_protocol::GuacamoleParser;

// Parse instruction (zero-copy - returns string slices)
let instr = GuacamoleParser::parse_instruction(&bytes)?;
match instr.opcode {
    "key" => { /* handle */ },
    "mouse" => { /* handle */ },
    _ => {}
}
```

### 2. Event-Based Interface

Instead of channels, use event callbacks:

```rust
use guacr_handlers::{EventCallback, HandlerEvent};

// In keeper-pam-webrtc-rs
struct WebRtcEventCallback {
    webrtc_channel: Arc<DataChannel>,
}

impl EventCallback for WebRtcEventCallback {
    fn on_event(&self, event: HandlerEvent) {
        match event {
            HandlerEvent::Error { message, code } => {
                // Handle error - terminate connection
                self.webrtc_channel.close();
            }
            HandlerEvent::Disconnect { reason } => {
                // Handle disconnect
            }
            HandlerEvent::ThreatDetected { level, description } => {
                // Handle threat - terminate immediately
                self.webrtc_channel.close();
            }
            HandlerEvent::Instruction(bytes) => {
                // Send instruction (zero-copy - Bytes is refcounted)
                self.webrtc_channel.send(bytes).await?;
            }
            _ => {}
        }
    }
    
    fn send_instruction(&self, instruction: Bytes) {
        // Zero-copy send - Bytes is reference-counted
        self.webrtc_channel.send(instruction).await?;
    }
}
```

### 3. Protocol Handler Integration

Handlers use `InstructionSender` instead of channels:

```rust
use guacr_handlers::InstructionSender;

impl ProtocolHandler for SshHandler {
    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
    ) -> Result<()> {
        let sender = InstructionSender::new(callback);
        
        // Send instruction (zero-copy)
        let instruction = Bytes::from("3.img,...");
        sender.send(instruction);
        
        // Send error (triggers callback)
        sender.send_error("Connection failed".to_string(), None);
        
        // Send threat (triggers callback)
        sender.send_threat("critical".to_string(), "Malicious command detected".to_string());
        
        Ok(())
    }
}
```

## Memory Sharing

### Bytes (Reference-Counted)

`Bytes` uses `Arc` internally, so sharing is zero-copy:

```rust
let bytes = Bytes::from("data");
let bytes2 = bytes.clone(); // Just increments refcount, no copy

// Both point to same memory
assert_eq!(bytes.as_ptr(), bytes2.as_ptr());
```

### Shared Memory (Future)

For even better performance, we can use shared memory:

```rust
use shared_memory::Shmem;

// Create shared memory region
let shmem = Shmem::create(1024 * 1024)?; // 1MB

// Write to shared memory
let mut data = unsafe { shmem.as_slice_mut() };
data[0..4].copy_from_slice(b"test");

// Share pointer with handler (zero-copy)
handler.process_frame(shmem.as_ptr());
```

## Benefits

1. **Zero Copies**: `Bytes` is reference-counted, shared between handler and WebRTC
2. **Lower Latency**: Direct callbacks instead of channel queuing
3. **Better Integration**: keeper-pam-webrtc-rs only needs to handle critical events
4. **Parser in guacr**: All protocol logic in one place
5. **Event-Based**: Clear separation of concerns

## Migration Path

1. ✅ Add parser to `guacr-protocol`
2. ✅ Add event-based interface to `guacr-handlers`
3. ⏳ Update handlers to support event-based interface
4. ⏳ Update keeper-pam-webrtc-rs to use event callbacks
5. ⏳ Deprecate channel-based interface (keep for backwards compatibility)

## Backwards Compatibility

The channel-based interface remains available:

```rust
// Old way (still works)
handler.connect(params, to_client, from_client).await?;

// New way (zero-copy)
handler.connect_with_events(params, callback).await?;
```

## Size Instruction

With AI threat detection, we can detect screen size changes from terminal output. The `size` instruction is kept for backwards compatibility but not strictly needed:

```rust
// Old way: Client sends size instruction
// New way: Handler detects size from terminal output
// Keep size instruction for backwards compatibility
if let Some(size) = detect_size_from_output(&terminal_output) {
    sender.send_size(size.width, size.height); // Optional
}
```
