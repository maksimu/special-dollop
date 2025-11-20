# keeper-pam-webrtc-rs Zero-Copy Integration

## Current Status

The `handler_connections.rs` file in `keeper-pam-webrtc-rs` already has zero-copy integration code, but it **cannot work** because of missing functionality in `guacr-handlers`.

## Problem

In `handler_connections.rs` line 265, the code attempts:

```rust
if let Some(event_handler) = handler.as_any().downcast_ref::<dyn EventBasedHandler>() {
```

**This fails because:**
1. `ProtocolHandler` trait doesn't have an `as_any()` method
2. You cannot downcast `Arc<dyn ProtocolHandler>` to `dyn EventBasedHandler` using trait objects
3. The registry stores `Arc<dyn ProtocolHandler>`, not `Arc<dyn EventBasedHandler>`

## What Needs to Be Done

### 1. Add Type Erasure Support to ProtocolHandler Trait

**File**: `crates/guacr-handlers/src/handler.rs`

Add a method to check if a handler implements `EventBasedHandler`:

```rust
use std::any::Any;

#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    // ... existing methods ...
    
    /// Get as Any for downcasting
    fn as_any(&self) -> &dyn Any;
    
    /// Check if this handler supports event-based interface
    fn as_event_based(&self) -> Option<&dyn EventBasedHandler>;
}
```

### 2. Implement Type Erasure in All Handlers

**Files**: All handler implementations (SSH, RDP, VNC, etc.)

Each handler needs to implement the new methods:

```rust
impl ProtocolHandler for SshHandler {
    // ... existing implementation ...
    
    fn as_any(&self) -> &dyn Any {
        self
    }
    
    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)  // If handler implements EventBasedHandler
        // None  // If handler doesn't implement it yet
    }
}
```

### 3. Make Handlers Implement Both Traits

**Files**: Handler implementations

Handlers that want zero-copy support need to implement both:

```rust
use guacr_handlers::{ProtocolHandler, EventBasedHandler, EventCallback, InstructionSender};
use std::sync::Arc;

impl ProtocolHandler for SshHandler {
    // ... channel-based interface ...
}

impl EventBasedHandler for SshHandler {
    fn name(&self) -> &str {
        "ssh"
    }
    
    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
    ) -> Result<(), HandlerError> {
        let sender = InstructionSender::new(callback);
        
        // Use sender instead of channel
        // sender.send(instruction);  // Zero-copy
        
        Ok(())
    }
}
```

### 4. Fix handler_connections.rs Downcast

**File**: `keeper-pam-webrtc-rs/src/channel/handler_connections.rs`

Change line 265 from:

```rust
if let Some(event_handler) = handler.as_any().downcast_ref::<dyn EventBasedHandler>() {
```

To:

```rust
if let Some(event_handler) = handler.as_event_based() {
```

### 5. Alternative: Use a Wrapper Type

If type erasure is too complex, use a wrapper approach:

**File**: `crates/guacr-handlers/src/registry.rs`

```rust
pub enum HandlerWrapper {
    ChannelOnly(Arc<dyn ProtocolHandler>),
    EventBased(Arc<dyn EventBasedHandler>),
    Both {
        channel: Arc<dyn ProtocolHandler>,
        event: Arc<dyn EventBasedHandler>,
    },
}

impl HandlerWrapper {
    pub fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        match self {
            HandlerWrapper::EventBased(h) => Some(h.as_ref()),
            HandlerWrapper::Both { event, .. } => Some(event.as_ref()),
            _ => None,
        }
    }
    
    pub fn as_protocol_handler(&self) -> &dyn ProtocolHandler {
        match self {
            HandlerWrapper::ChannelOnly(h) => h.as_ref(),
            HandlerWrapper::EventBased(h) => h.as_ref(),  // EventBasedHandler also implements ProtocolHandler
            HandlerWrapper::Both { channel, .. } => channel.as_ref(),
        }
    }
}
```

Then update registry to use `HandlerWrapper` instead of `Arc<dyn ProtocolHandler>`.

## Recommended Approach

**Option 1: Add `as_event_based()` method (Simplest)**

1. Add `as_event_based()` to `ProtocolHandler` trait
2. Implement it in handlers (return `Some(self)` if they implement `EventBasedHandler`, `None` otherwise)
3. Update `handler_connections.rs` to use `as_event_based()` instead of downcast

**Option 2: Make EventBasedHandler extend ProtocolHandler**

```rust
#[async_trait]
pub trait EventBasedHandler: ProtocolHandler {
    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
    ) -> Result<(), HandlerError>;
}
```

Then handlers that implement `EventBasedHandler` automatically implement `ProtocolHandler`, and you can check with:

```rust
if let Ok(event_handler) = Arc::try_unwrap(handler.clone())
    .map_err(|_| ())
    .and_then(|h| h.as_any().downcast::<dyn EventBasedHandler>().ok())
{
    // Use event-based interface
}
```

But this is still complex with trait objects.

## Implementation Priority

1. **High Priority**: Add `as_event_based()` method to `ProtocolHandler` trait
2. **High Priority**: Update `handler_connections.rs` to use the new method
3. **Medium Priority**: Implement `EventBasedHandler` for SSH handler (most used)
4. **Medium Priority**: Implement `EventBasedHandler` for RDP handler
5. **Low Priority**: Implement `EventBasedHandler` for other handlers

## Testing

After implementation, verify:

1. ✅ Zero-copy path works (check logs for "Using event-based interface")
2. ✅ Fallback to channel-based still works
3. ✅ No memory copies in hot path (use profiling tools)
4. ✅ Performance improvement (measure latency/throughput)

## References

- `docs/ZERO_COPY_INTEGRATION.md` - Zero-copy architecture overview
- `docs/docs/context/GUACR_ZERO_COPY_OPTIMIZATION.md` - Detailed optimization guide
- `crates/guacr-handlers/src/events.rs` - Event-based interface definitions
- `keeper-pam-webrtc-rs/src/channel/handler_connections.rs` - Integration code
