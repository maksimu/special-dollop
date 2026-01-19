# WebRTC Integration Guide

**Summary**: No changes required in keeper-pam-webrtc-rs - existing code continues to work. Optional event-based interface available for better performance.

This document describes the integration options for `keeper-pam-webrtc-rs` with guacr handlers.

## Current Integration (Channel-Based)

The current integration uses channels:

```rust
// Current code in keeper-pam-webrtc-rs
use guacr_handlers::{handle_guacd_with_handlers, ProtocolHandlerRegistry};
use tokio::sync::mpsc;
use bytes::Bytes;

// Create channels
let (to_webrtc, mut from_handler) = mpsc::channel::<Bytes>(128);
let (to_handler, mut from_webrtc) = mpsc::channel::<Bytes>(128);

// Call handler
handle_guacd_with_handlers(
    conversation_type,
    params,
    registry,
    to_webrtc,
    from_webrtc,
).await?;

// Forward messages
loop {
    tokio::select! {
        Some(msg) = from_handler.recv() => {
            // Send to WebRTC client
            data_channel.send(msg).await?;
        }
        Some(msg) = from_webrtc.recv() => {
            // Send to handler (already handled by handle_guacd_with_handlers)
        }
    }
}
```

## New Integration (Event-Based, Zero-Copy)

### Option 1: Use Event-Based Interface (Recommended)

```rust
// New code in keeper-pam-webrtc-rs
use guacr_handlers::{
    EventCallback, EventBasedHandler, HandlerEvent, InstructionSender,
    ProtocolHandlerRegistry,
};
use std::sync::Arc;
use bytes::Bytes;

// Implement EventCallback for WebRTC
pub struct WebRtcEventCallback {
    data_channel: Arc<dyn DataChannelTrait>, // Your WebRTC data channel type
}

impl EventCallback for WebRtcEventCallback {
    fn on_event(&self, event: HandlerEvent) {
        match event {
            HandlerEvent::Error { message, code } => {
                log::error!("Protocol error: {} (code: {:?})", message, code);
                // Close WebRTC connection
                self.data_channel.close();
            }
            HandlerEvent::Disconnect { reason } => {
                log::info!("Protocol disconnect: {}", reason);
                // Close WebRTC connection gracefully
                self.data_channel.close();
            }
            HandlerEvent::ThreatDetected { level, description } => {
                log::error!("Threat detected [{}]: {}", level, description);
                // Immediately close connection
                self.data_channel.close();
            }
            HandlerEvent::Ready { connection_id } => {
                log::info!("Connection ready: {}", connection_id);
                // Connection established, can start sending data
            }
            HandlerEvent::Size { width, height } => {
                log::debug!("Size change: {}x{}", width, height);
                // Optional: Handle size change (not needed with threat detection)
            }
            HandlerEvent::Instruction(bytes) => {
                // This shouldn't happen - use send_instruction instead
                self.send_instruction(bytes);
            }
        }
    }
    
    fn send_instruction(&self, instruction: Bytes) {
        // Zero-copy send - Bytes is reference-counted
        // This is the hot path - no copying happens
        if let Err(e) = self.data_channel.send(instruction) {
            log::error!("Failed to send instruction: {}", e);
        }
    }
}

// Use event-based handler
pub async fn connect_with_events(
    conversation_type: &str,
    params: HashMap<String, String>,
    registry: Arc<ProtocolHandlerRegistry>,
    data_channel: Arc<dyn DataChannelTrait>,
) -> Result<()> {
    // Get handler
    let handler = registry.get(conversation_type)
        .ok_or_else(|| format!("No handler for {}", conversation_type))?;
    
    // Create event callback
    let callback = Arc::new(WebRtcEventCallback {
        data_channel: data_channel.clone(),
    });
    
    // Check if handler supports event-based interface
    if let Some(event_handler) = handler.as_any().downcast_ref::<dyn EventBasedHandler>() {
        // Use event-based interface (zero-copy)
        event_handler.connect_with_events(params, callback).await?;
    } else {
        // Fallback to channel-based interface (backwards compatible)
        // ... use existing handle_guacd_with_handlers ...
    }
    
    Ok(())
}
```

### Option 2: Minimal Changes (Backwards Compatible)

If you want minimal changes, you can keep using the channel-based interface:

```rust
// No changes needed - existing code continues to work
use guacr_handlers::handle_guacd_with_handlers;

// Existing code works as-is
handle_guacd_with_handlers(
    conversation_type,
    params,
    registry,
    to_webrtc,
    from_webrtc,
).await?;
```

The channel-based interface remains fully functional for backwards compatibility.

### Option 3: Hybrid Approach (Best Performance)

Use event-based when available, fallback to channels:

```rust
use guacr_handlers::{EventBasedHandler, EventCallback, handle_guacd_with_handlers};

pub async fn connect_protocol(
    conversation_type: &str,
    params: HashMap<String, String>,
    registry: Arc<ProtocolHandlerRegistry>,
    data_channel: Arc<dyn DataChannelTrait>,
) -> Result<()> {
    let handler = registry.get(conversation_type)?;
    
    // Try event-based first (zero-copy)
    if let Some(event_handler) = handler.as_any().downcast_ref::<dyn EventBasedHandler>() {
        let callback = Arc::new(WebRtcEventCallback {
            data_channel: data_channel.clone(),
        });
        event_handler.connect_with_events(params, callback).await?;
    } else {
        // Fallback to channel-based (backwards compatible)
        let (to_webrtc, mut from_handler) = mpsc::channel::<Bytes>(128);
        let (to_handler, mut from_webrtc) = mpsc::channel::<Bytes>(128);
        
        // Spawn forwarding task
        let channel_clone = data_channel.clone();
        tokio::spawn(async move {
            while let Some(msg) = from_handler.recv().await {
                channel_clone.send(msg).await?;
            }
            Ok::<(), Error>(())
        });
        
        handle_guacd_with_handlers(
            conversation_type.to_string(),
            params,
            registry,
            to_webrtc,
            from_webrtc,
        ).await?;
    }
    
    Ok(())
}
```

## Required Changes Summary

### Minimal Changes (Backwards Compatible)

**No changes required** - existing code continues to work. The channel-based interface remains fully functional.

### Recommended Changes (Zero-Copy)

1. **Implement `EventCallback` trait**:
   ```rust
   struct WebRtcEventCallback { ... }
   impl EventCallback for WebRtcEventCallback { ... }
   ```

2. **Handle critical events**:
   - `Error` - Terminate connection
   - `Disconnect` - Close gracefully
   - `ThreatDetected` - Immediate termination

3. **Use `send_instruction()` for zero-copy**:
   - All regular instructions go through `send_instruction()`
   - `Bytes` is reference-counted, no copying

4. **Optional: Check for event-based handler**:
   - Try `EventBasedHandler` first
   - Fallback to channel-based if not available

## Benefits of Event-Based Interface

1. **Zero-Copy**: `Bytes` shared between handler and WebRTC (reference-counted)
2. **Lower Latency**: Direct callbacks instead of channel queuing
3. **Better Error Handling**: Critical events (error, disconnect, threat) handled immediately
4. **Simpler Code**: No need to forward all messages through channels

## Migration Checklist

- [ ] Implement `EventCallback` trait for WebRTC data channel
- [ ] Handle critical events (`Error`, `Disconnect`, `ThreatDetected`)
- [ ] Update handler connection code to use event-based interface (optional)
- [ ] Test zero-copy integration with 4K video streaming
- [ ] Keep channel-based interface as fallback (backwards compatible)

## Example: Complete Integration

See `docs/ZERO_COPY_INTEGRATION.md` for complete example code.

## Backwards Compatibility

**Important**: The channel-based interface (`handle_guacd_with_handlers`) remains fully functional. You can migrate gradually:

1. **Phase 1**: No changes - existing code works
2. **Phase 2**: Implement `EventCallback`, use when available
3. **Phase 3**: Migrate all handlers to event-based (optional)

No breaking changes required!
