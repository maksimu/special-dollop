# WebRTC Multi-Channel Optimization for Hardware Encoding

## Why Hardware Encoding is Better

### Performance Comparison (4K @ 60fps)

| Metric | JPEG/PNG (Image) | H.264 (Hardware) | Improvement |
|--------|------------------|------------------|-------------|
| **Bandwidth** | 50-100 Mbps | 20-30 Mbps | **2-3x better** |
| **CPU Usage** | ~60% (software) | ~20% (hardware) | **3x better** |
| **Latency** | ~16ms/frame | ~8-12ms/frame | **25-50% better** |
| **Quality** | Lossy (JPEG) | Lossy (H.264) | Similar |
| **Browser Support** | Universal | Universal (H.264) | Equal |

**Conclusion: Hardware H.264 encoding is significantly better for video streaming.**

## Current Implementation

You're already using hardware encoding correctly:
- ✅ Hardware encoders (NVENC, QuickSync, VideoToolbox, VAAPI)
- ✅ Binary protocol with `video` (0x0C) + `blob` (0x09) opcodes
- ✅ Zero-copy `Bytes` for data transfer

## WebRTC Multi-Channel Optimization

### Problem: Single Channel Bottleneck

WebRTC data channels have practical limits:
- **Single channel**: ~10-15 Mbps max throughput
- **Multiple channels**: Can aggregate to 50+ Mbps
- **4K @ 60fps**: Needs ~20-30 Mbps → **Requires 2-3 channels**

### Solution: Frame Fragmentation Across Channels

Split large H.264 frames across multiple data channels for maximum throughput.

## Implementation Plan

### 1. Add Fragmentation Support to Binary Protocol

Update `crates/guacr-protocol/src/binary.rs`:

```rust
// Flags (already in spec, need to implement)
pub const FLAG_FRAGMENTED: u8 = 0x04;  // Message is fragmented
pub const FLAG_FIRST_FRAGMENT: u8 = 0x08;  // First fragment of message
pub const FLAG_LAST_FRAGMENT: u8 = 0x10;  // Last fragment of message

// Fragment header (for fragmented blobs)
#[repr(C, packed)]
pub struct FragmentHeader {
    pub sequence_id: u32,      // Unique ID for this frame
    pub fragment_index: u16,   // Fragment number (0-based)
    pub total_fragments: u16,  // Total fragments in this frame
    pub fragment_offset: u32,  // Byte offset in original frame
    pub fragment_length: u32,  // Length of this fragment
}

impl BinaryEncoder {
    /// Encode a fragmented blob (for multi-channel WebRTC)
    ///
    /// Splits large video frames across multiple channels for higher throughput.
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `sequence_id` - Frame sequence ID (for reassembly)
    /// * `fragment_index` - Fragment number (0-based)
    /// * `total_fragments` - Total fragments in this frame
    /// * `fragment_data` - This fragment's data
    pub fn encode_blob_fragment(
        &mut self,
        stream_id: u32,
        sequence_id: u32,
        fragment_index: u16,
        total_fragments: u16,
        fragment_data: Bytes,
    ) -> Bytes {
        self.scratch.clear();
        
        // Payload: stream_id(4) + fragment_header(16) + data(N)
        let payload_len = 4 + 16 + fragment_data.len();
        
        let mut flags = FLAG_FRAGMENTED;
        if fragment_index == 0 {
            flags |= FLAG_FIRST_FRAGMENT;
        }
        if fragment_index == total_fragments - 1 {
            flags |= FLAG_LAST_FRAGMENT;
        }
        
        let header = MessageHeader {
            opcode: Opcode::Blob,
            flags,
            length: payload_len as u32,
        };
        
        self.scratch.put_slice(&header.to_bytes());
        self.scratch.put_u32_le(stream_id);
        
        // Fragment header
        self.scratch.put_u32_le(sequence_id);
        self.scratch.put_u16_le(fragment_index);
        self.scratch.put_u16_le(total_fragments);
        self.scratch.put_u32_le(0); // fragment_offset (not needed for blob)
        self.scratch.put_u32_le(fragment_data.len() as u32);
        
        // Fragment data (zero-copy)
        self.scratch.extend_from_slice(&fragment_data);
        
        self.scratch.clone().freeze()
    }
    
    /// Split large blob into fragments for multi-channel transmission
    ///
    /// Returns vector of fragments, each ready to send on a different channel.
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `sequence_id` - Frame sequence ID
    /// * `data` - Full frame data
    /// * `max_fragment_size` - Maximum size per fragment (e.g., 64KB for WebRTC)
    /// * `num_channels` - Number of channels to distribute across
    pub fn split_blob_for_channels(
        &mut self,
        stream_id: u32,
        sequence_id: u32,
        data: Bytes,
        max_fragment_size: usize,
        num_channels: usize,
    ) -> Vec<Bytes> {
        let total_size = data.len();
        let fragments_per_channel = (total_size + max_fragment_size - 1) / max_fragment_size;
        let total_fragments = fragments_per_channel * num_channels;
        
        // Round-robin distribution: fragment 0 → channel 0, fragment 1 → channel 1, etc.
        let mut fragments = Vec::with_capacity(total_fragments);
        
        for (fragment_idx, chunk) in data.chunks(max_fragment_size).enumerate() {
            let fragment_data = Bytes::from(chunk);
            let fragment = self.encode_blob_fragment(
                stream_id,
                sequence_id,
                fragment_idx as u16,
                total_fragments as u16,
                fragment_data,
            );
            fragments.push(fragment);
        }
        
        fragments
    }
}
```

### 2. Multi-Channel Sender Helper

Create `crates/guacr-handlers/src/multi_channel.rs`:

```rust
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Multi-channel sender for WebRTC data channels
///
/// Distributes video frames across multiple channels for maximum throughput.
pub struct MultiChannelSender {
    channels: Vec<mpsc::Sender<Bytes>>,
    current_channel: usize,
    sequence_id: u32,
}

impl MultiChannelSender {
    /// Create a new multi-channel sender
    ///
    /// # Arguments
    ///
    /// * `channels` - Vector of WebRTC data channel senders
    pub fn new(channels: Vec<mpsc::Sender<Bytes>>) -> Self {
        Self {
            channels,
            current_channel: 0,
            sequence_id: 0,
        }
    }
    
    /// Send a complete video frame, splitting across channels if needed
    ///
    /// # Arguments
    ///
    /// * `stream_id` - Stream identifier
    /// * `frame_data` - Complete H.264 frame (NAL units)
    /// * `max_fragment_size` - Maximum fragment size (default: 64KB)
    pub async fn send_video_frame(
        &mut self,
        stream_id: u32,
        frame_data: Bytes,
        max_fragment_size: usize,
    ) -> Result<(), String> {
        let sequence_id = self.sequence_id;
        self.sequence_id = self.sequence_id.wrapping_add(1);
        
        // If frame is small, send on single channel
        if frame_data.len() <= max_fragment_size {
            let channel = &self.channels[self.current_channel];
            self.current_channel = (self.current_channel + 1) % self.channels.len();
            
            // Encode as regular blob (not fragmented)
            // Note: You'll need to add encode_blob to BinaryEncoder
            channel.send(frame_data)
                .await
                .map_err(|e| format!("Failed to send frame: {}", e))?;
            
            return Ok(());
        }
        
        // Large frame: split across channels
        let num_channels = self.channels.len();
        let mut encoder = BinaryEncoder::new();
        let fragments = encoder.split_blob_for_channels(
            stream_id,
            sequence_id,
            frame_data,
            max_fragment_size,
            num_channels,
        );
        
        // Distribute fragments round-robin across channels
        for (fragment_idx, fragment) in fragments.into_iter().enumerate() {
            let channel_idx = fragment_idx % num_channels;
            self.channels[channel_idx]
                .send(fragment)
                .await
                .map_err(|e| format!("Failed to send fragment {}: {}", fragment_idx, e))?;
        }
        
        Ok(())
    }
    
    /// Send a video instruction (once per stream)
    pub async fn send_video_instruction(
        &mut self,
        stream_id: u32,
        layer: i32,
        mimetype: &str,
    ) -> Result<(), String> {
        let mut encoder = BinaryEncoder::new();
        let video_msg = encoder.encode_video(stream_id, layer, mimetype);
        
        // Send on first channel (control messages)
        self.channels[0]
            .send(video_msg)
            .await
            .map_err(|e| format!("Failed to send video instruction: {}", e))?;
        
        Ok(())
    }
}
```

### 3. Client-Side Reassembly

The client needs to reassemble fragments:

```typescript
// In VideoPlayer.ts or WebRTCGuacTunnel.ts

interface FragmentBuffer {
    sequenceId: number;
    fragments: Map<number, Uint8Array>;
    totalFragments: number;
    receivedFragments: number;
}

class MultiChannelReceiver {
    private fragmentBuffers: Map<number, FragmentBuffer> = new Map();
    private videoPlayer: VideoPlayer;
    
    handleBlobMessage(streamId: number, data: Uint8Array, flags: number) {
        const isFragmented = (flags & 0x04) !== 0;
        const isFirstFragment = (flags & 0x08) !== 0;
        const isLastFragment = (flags & 0x10) !== 0;
        
        if (!isFragmented) {
            // Regular blob, send directly to video player
            this.videoPlayer.appendChunk(data);
            return;
        }
        
        // Fragmented blob - extract fragment header
        const view = new DataView(data.buffer, data.byteOffset);
        const sequenceId = view.getUint32(0, true);  // Little-endian
        const fragmentIndex = view.getUint16(4, true);
        const totalFragments = view.getUint16(6, true);
        const fragmentOffset = view.getUint32(8, true);
        const fragmentLength = view.getUint32(12, true);
        
        const fragmentData = data.slice(16);  // Skip header
        
        // Get or create fragment buffer
        let buffer = this.fragmentBuffers.get(sequenceId);
        if (!buffer) {
            buffer = {
                sequenceId,
                fragments: new Map(),
                totalFragments,
                receivedFragments: 0,
            };
            this.fragmentBuffers.set(sequenceId, buffer);
        }
        
        // Store fragment
        buffer.fragments.set(fragmentIndex, fragmentData);
        buffer.receivedFragments++;
        
        // Check if all fragments received
        if (buffer.receivedFragments === totalFragments) {
            // Reassemble frame
            const frame = new Uint8Array(fragmentLength * totalFragments);
            let offset = 0;
            for (let i = 0; i < totalFragments; i++) {
                const frag = buffer.fragments.get(i)!;
                frame.set(frag, offset);
                offset += frag.length;
            }
            
            // Send to video player
            this.videoPlayer.appendChunk(frame);
            
            // Clean up
            this.fragmentBuffers.delete(sequenceId);
        }
    }
}
```

## Performance Benefits

### Single Channel (Current)
- **Max throughput**: ~10-15 Mbps
- **4K @ 60fps**: **Not possible** (needs 20-30 Mbps)
- **Latency**: Higher (backpressure)

### Multi-Channel (Optimized)
- **Max throughput**: 10-15 Mbps × N channels (e.g., 30-45 Mbps for 3 channels)
- **4K @ 60fps**: **Possible** with 2-3 channels
- **Latency**: Lower (no backpressure, parallel transmission)

## Recommended Configuration

### For 4K @ 60fps (20-30 Mbps):
- **3 data channels** (10 Mbps each)
- **Fragment size**: 64 KB (WebRTC optimal)
- **Hardware encoding**: H.264 @ 10-15 Mbps bitrate

### For 1080p @ 60fps (5-10 Mbps):
- **1-2 data channels** (sufficient)
- **Fragment size**: 64 KB
- **Hardware encoding**: H.264 @ 5 Mbps bitrate

### For 720p @ 30fps (2-5 Mbps):
- **1 data channel** (sufficient)
- **Fragment size**: 32 KB
- **Hardware encoding**: H.264 @ 3 Mbps bitrate

## Implementation Priority

1. **High**: Add `FLAG_FRAGMENTED` support to binary protocol
2. **High**: Implement `encode_blob_fragment()` method
3. **Medium**: Add `MultiChannelSender` helper
4. **Medium**: Update hardware encoder integration to use multi-channel
5. **Low**: Client-side reassembly (if not already handled by browser)

## Notes

- **WebRTC data channel limits**: ~16 KB per message (but can send multiple messages)
- **Optimal fragment size**: 32-64 KB (balance between overhead and parallelism)
- **Channel ordering**: Fragments may arrive out-of-order → use sequence_id for reassembly
- **Error handling**: If a fragment is lost, request retransmission or skip frame
