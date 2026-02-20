# SFTP Channel Adapter

## Overview

The SFTP handler requires converting russh's message-based `Channel` API to the stream-based `AsyncRead + AsyncWrite` interface expected by `russh-sftp`. This is accomplished via the `ChannelStreamAdapter`.

## Problem

- **russh Channel**: Uses message-based API (`channel.wait()` returns `ChannelMsg::Data { data }`, `channel.data(&[u8])` sends data)
- **russh-sftp**: Expects `AsyncRead + AsyncWrite` stream interface
- **Challenge**: Channels cannot be cloned, and the message-based API doesn't directly implement stream traits

## Solution: ChannelStreamAdapter

The adapter bridges these two APIs:

### Architecture

```
┌─────────────┐         ┌──────────────────┐         ┌─────────────┐
│ russh       │         │ ChannelStream     │         │ russh-sftp   │
│ Channel     │ ──────> │ Adapter           │ ──────> │ SftpSession  │
│ (messages)  │         │ (stream)          │         │ (stream)     │
└─────────────┘         └──────────────────┘         └─────────────┘
```

### Implementation

1. **Channel Wrapping**: Wrap `Channel` in `Arc<Mutex<>>` for shared access
2. **Read Task**: Spawn background task that:
   - Polls `channel.wait()` for `ChannelMsg::Data`
   - Converts `CryptoVec` to `Bytes`
   - Sends to internal `mpsc::UnboundedReceiver`
3. **Write Task**: Spawn background task that:
   - Receives from internal `mpsc::UnboundedSender`
   - Calls `channel.data()` to send data
4. **Stream Implementation**: 
   - `AsyncRead`: Polls receiver, buffers partial reads
   - `AsyncWrite`: Sends to write channel (non-blocking)

### Code Structure

```rust
pub struct ChannelStreamAdapter {
    channel: Arc<Mutex<russh::Channel<russh::client::Msg>>>,
    rx: mpsc::UnboundedReceiver<Bytes>,      // Read direction
    write_tx: mpsc::UnboundedSender<Bytes>,  // Write direction
    read_buffer: Vec<u8>,                     // Partial read buffer
    eof: bool,                                 // EOF flag
}
```

### Usage

```rust
let (channel_stream, _forwarder_handle) = ChannelStreamAdapter::new(channel);
let sftp = SftpSession::new(channel_stream).await?;
```

## Benefits

- ✅ Clean separation: Channel management vs. SFTP operations
- ✅ Non-blocking: Write operations don't block the main loop
- ✅ Buffering: Handles partial reads gracefully
- ✅ Error handling: Properly signals EOF and channel closure

## Limitations

- Uses `Arc<Mutex<>>` which adds some overhead (acceptable for SFTP)
- Background tasks must be kept alive (returned `JoinHandle` should be stored)
- Write operations are fire-and-forget (no backpressure handling)

## Future Improvements

- Add backpressure handling for writes
- Add timeout handling
- Add connection health monitoring
- Consider using `russh-sftp::client::run()` if it provides better integration
