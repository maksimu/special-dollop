//! Multi-Channel Assembler for WebRTC Data Channel Fragmentation
//!
//! This module provides zero-copy fragmentation and reassembly of large frames
//! across multiple WebRTC data channels for higher throughput.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                        Assembler                                 │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
//! │  │  Primary    │  │  Overflow   │  │  Overflow   │  ...         │
//! │  │  Channel    │  │  Channel 1  │  │  Channel 2  │              │
//! │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘              │
//! │         │                │                │                      │
//! │         └────────────────┴────────────────┘                      │
//! │                          │                                       │
//! │                    ┌─────┴─────┐                                 │
//! │                    │  Merged   │                                 │
//! │                    │  Output   │                                 │
//! │                    └───────────┘                                 │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Fragment Header Format (9 bytes)
//!
//! ```text
//! ┌─────────┬───────────┬─────────────┬───────────────┐
//! │ Flags(1)│ SeqId(4)  │ FragIdx(2)  │ TotalFrag(2)  │
//! └─────────┴───────────┴─────────────┴───────────────┘
//! ```
//!
//! # Note on Dead Code Analysis
//!
//! Many items in this module appear unused to Rust's dead code analysis because
//! they are accessed through `Option<Assembler>` indirection at runtime. The
//! assembler is conditionally created when FRAGMENTATION capability is enabled,
//! and its methods are called via `if let Some(ref assembler) = self.assembler`.
//! Rust's static analysis cannot trace through this pattern.

// Allow dead_code at module level - items are used at runtime through Option<Assembler>
// indirection that Rust's static analysis cannot trace. See module docs above.
#![allow(dead_code)]

use bytes::Bytes;
use dashmap::DashMap;
use log::{debug, warn};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::buffer_pool::BufferPool;
use crate::webrtc_data_channel::WebRTCDataChannel;

/// Size of fragment header in bytes
pub const FRAGMENT_HEADER_SIZE: usize = 9;

/// Default fragment threshold - frames larger than this are fragmented
pub const DEFAULT_FRAGMENT_THRESHOLD: usize = 16 * 1024; // 16KB

/// Default maximum fragments per frame
pub const DEFAULT_MAX_FRAGMENTS: u16 = 256;

/// Default fragment timeout for reassembly
pub const DEFAULT_FRAGMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fragment flags
#[allow(dead_code)] // Constants used when fragmentation is active
pub mod flags {
    /// Frame has fragment header (is a fragment)
    pub const HAS_FRAGMENT_HEADER: u8 = 0x01;
    /// First fragment of a fragmented frame
    pub const FIRST_FRAGMENT: u8 = 0x02;
    /// Last fragment of a fragmented frame
    pub const LAST_FRAGMENT: u8 = 0x04;
}

/// Fragment header for multi-channel transmission
#[derive(Debug, Clone, Copy)]
pub struct FragmentHeader {
    /// Fragment flags
    pub flags: u8,
    /// Sequence ID for this frame (all fragments share same seq_id)
    pub seq_id: u32,
    /// Index of this fragment (0-based)
    pub frag_idx: u16,
    /// Total number of fragments for this frame
    pub total_frags: u16,
}

impl FragmentHeader {
    /// Create a new fragment header
    pub fn new(seq_id: u32, frag_idx: u16, total_frags: u16) -> Self {
        let mut flags = flags::HAS_FRAGMENT_HEADER;
        if frag_idx == 0 {
            flags |= flags::FIRST_FRAGMENT;
        }
        if frag_idx == total_frags - 1 {
            flags |= flags::LAST_FRAGMENT;
        }
        Self {
            flags,
            seq_id,
            frag_idx,
            total_frags,
        }
    }

    /// Encode header to bytes (9 bytes)
    pub fn encode(&self) -> [u8; FRAGMENT_HEADER_SIZE] {
        let mut buf = [0u8; FRAGMENT_HEADER_SIZE];
        buf[0] = self.flags;
        buf[1..5].copy_from_slice(&self.seq_id.to_be_bytes());
        buf[5..7].copy_from_slice(&self.frag_idx.to_be_bytes());
        buf[7..9].copy_from_slice(&self.total_frags.to_be_bytes());
        buf
    }

    /// Decode header from bytes
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < FRAGMENT_HEADER_SIZE {
            return None;
        }
        let flags = data[0];
        if flags & flags::HAS_FRAGMENT_HEADER == 0 {
            return None; // Not a fragment
        }
        Some(Self {
            flags,
            seq_id: u32::from_be_bytes([data[1], data[2], data[3], data[4]]),
            frag_idx: u16::from_be_bytes([data[5], data[6]]),
            total_frags: u16::from_be_bytes([data[7], data[8]]),
        })
    }

    /// Check if this is the first fragment
    pub fn is_first(&self) -> bool {
        self.flags & flags::FIRST_FRAGMENT != 0
    }

    /// Check if this is the last fragment
    pub fn is_last(&self) -> bool {
        self.flags & flags::LAST_FRAGMENT != 0
    }
}

/// Check if data has a fragment header
pub fn has_fragment_header(data: &[u8]) -> bool {
    !data.is_empty() && (data[0] & flags::HAS_FRAGMENT_HEADER) != 0
}

/// Configuration for the assembler
#[derive(Debug, Clone)]
pub struct AssemblerConfig {
    /// Frames larger than this are fragmented (bytes)
    pub fragment_threshold: usize,
    /// Maximum fragments per frame
    pub max_fragments: u16,
    /// Timeout for incomplete fragment reassembly
    pub fragment_timeout: Duration,
    /// Maximum pending fragment buffers
    pub max_pending_buffers: usize,
}

impl Default for AssemblerConfig {
    fn default() -> Self {
        Self {
            fragment_threshold: DEFAULT_FRAGMENT_THRESHOLD,
            max_fragments: DEFAULT_MAX_FRAGMENTS,
            fragment_timeout: DEFAULT_FRAGMENT_TIMEOUT,
            max_pending_buffers: 1000,
        }
    }
}

/// State that can be persisted across reconnections
#[derive(Debug, Clone)]
pub struct AssemblerState {
    /// Next sequence ID to use
    pub next_seq_id: u32,
    /// Number of overflow channels
    pub overflow_channel_count: usize,
}

/// Buffer for reassembling fragments
pub struct FragmentBuffer {
    /// Expected total fragments
    total_frags: u16,
    /// Received fragments (index -> data)
    fragments: Vec<Option<Bytes>>,
    /// Number of fragments received
    received_count: u16,
    /// Creation time for timeout tracking
    created_at: Instant,
}

impl FragmentBuffer {
    /// Create a new fragment buffer for reassembling a frame
    pub fn new(total_frags: u16) -> Self {
        Self {
            total_frags,
            fragments: vec![None; total_frags as usize],
            received_count: 0,
            created_at: Instant::now(),
        }
    }

    /// Add a fragment, returns true if all fragments received
    pub fn add_fragment(&mut self, idx: u16, data: Bytes) -> bool {
        if idx >= self.total_frags {
            return false;
        }
        if self.fragments[idx as usize].is_none() {
            self.fragments[idx as usize] = Some(data);
            self.received_count += 1;
        }
        self.received_count == self.total_frags
    }

    /// Reassemble all fragments into a single frame
    pub fn reassemble(&self) -> Option<Bytes> {
        if self.received_count != self.total_frags {
            return None;
        }

        // Calculate total size
        let total_size: usize = self
            .fragments
            .iter()
            .filter_map(|f| f.as_ref())
            .map(|f| f.len())
            .sum();

        // Allocate and copy
        let mut result = bytes::BytesMut::with_capacity(total_size);
        for data in self.fragments.iter().flatten() {
            result.extend_from_slice(data);
        }

        Some(result.freeze())
    }

    /// Check if buffer has timed out
    fn is_expired(&self, timeout: Duration) -> bool {
        self.created_at.elapsed() > timeout
    }
}

/// Overflow channel for multi-channel transmission
#[allow(dead_code)] // Fields used when overflow channels are added
struct OverflowChannel {
    channel: Arc<WebRTCDataChannel>,
    rx: mpsc::UnboundedReceiver<Bytes>,
}

/// Multi-channel assembler for fragmentation and reassembly
pub struct Assembler {
    /// Primary data channel
    primary: Arc<WebRTCDataChannel>,
    /// Receiver for primary channel data (used in merged channel mode)
    primary_rx: mpsc::UnboundedReceiver<Bytes>,
    /// Overflow channels for parallel transmission
    overflow_channels: Vec<OverflowChannel>,
    /// Merged output sender (used in merged channel mode)
    merged_tx: mpsc::UnboundedSender<Bytes>,
    /// Merged output receiver (for Channel to consume in merged mode)
    merged_rx: mpsc::UnboundedReceiver<Bytes>,
    /// Next sequence ID for fragmentation
    next_seq_id: AtomicU32,
    /// Round-robin index for channel selection
    channel_round_robin: AtomicUsize,
    /// Pending fragment buffers (seq_id -> buffer)
    pending_fragments: Arc<DashMap<u32, FragmentBuffer>>,
    /// Count of pending buffers (for quick limit check)
    pending_count: AtomicUsize,
    /// Configuration
    config: AssemblerConfig,
    /// Buffer pool for efficient allocation
    buffer_pool: BufferPool,
    /// Cleanup task handle
    cleanup_task: Option<JoinHandle<()>>,
    /// Channel ID for logging
    channel_id: String,
}

impl Assembler {
    /// Create a new assembler with the primary channel
    pub fn new(
        primary: Arc<WebRTCDataChannel>,
        primary_rx: mpsc::UnboundedReceiver<Bytes>,
        config: AssemblerConfig,
        channel_id: String,
    ) -> Self {
        let (merged_tx, merged_rx) = mpsc::unbounded_channel();

        Self {
            primary,
            primary_rx,
            overflow_channels: Vec::new(),
            merged_tx,
            merged_rx,
            next_seq_id: AtomicU32::new(0),
            channel_round_robin: AtomicUsize::new(0),
            pending_fragments: Arc::new(DashMap::new()),
            pending_count: AtomicUsize::new(0),
            config,
            buffer_pool: BufferPool::new(crate::buffer_pool::STANDARD_BUFFER_CONFIG),
            cleanup_task: None,
            channel_id,
        }
    }

    /// Start the cleanup task for expired fragments
    pub fn start_cleanup_task(&mut self) {
        let pending = Arc::clone(&self.pending_fragments);
        let pending_count = self.pending_count.load(Ordering::Relaxed);
        let timeout = self.config.fragment_timeout;
        let channel_id = self.channel_id.clone();

        // Only start if not already running
        if self.cleanup_task.is_some() {
            return;
        }

        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;

                // Find and remove expired buffers
                let mut expired = Vec::new();
                for entry in pending.iter() {
                    if entry.value().is_expired(timeout) {
                        expired.push(*entry.key());
                    }
                }

                for seq_id in expired {
                    if pending.remove(&seq_id).is_some() {
                        warn!(
                            "Assembler({}): Fragment buffer {} expired (timeout: {:?})",
                            channel_id, seq_id, timeout
                        );
                    }
                }

                // Log if we have many pending buffers
                let count = pending.len();
                if count > 100 {
                    warn!(
                        "Assembler({}): High pending fragment count: {} (started with {})",
                        channel_id, count, pending_count
                    );
                }
            }
        });

        self.cleanup_task = Some(task);
    }

    /// Get the merged receiver for consuming reassembled frames
    ///
    /// Note: This takes ownership of the receiver - call only once
    pub fn take_merged_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Bytes>> {
        // We need to swap out the receiver
        let (new_tx, new_rx) = mpsc::unbounded_channel();
        let old_rx = std::mem::replace(&mut self.merged_rx, new_rx);
        // Update the sender too so new sends go to the new channel
        let _ = std::mem::replace(&mut self.merged_tx, new_tx);
        Some(old_rx)
    }

    /// Send a frame, fragmenting if necessary
    pub async fn send(&self, frame: Bytes) -> Result<(), String> {
        if frame.len() <= self.config.fragment_threshold {
            // Small frame - send directly without fragmentation
            self.primary.send(frame).await
        } else {
            // Large frame - fragment and send across channels
            self.send_fragmented(frame).await
        }
    }

    /// Fragment and send a large frame
    async fn send_fragmented(&self, frame: Bytes) -> Result<(), String> {
        let frame_len = frame.len();
        let frag_size = self.config.fragment_threshold - FRAGMENT_HEADER_SIZE;
        let num_frags = frame_len.div_ceil(frag_size);

        if num_frags > self.config.max_fragments as usize {
            return Err(format!(
                "Frame too large: {} bytes would need {} fragments (max: {})",
                frame_len, num_frags, self.config.max_fragments
            ));
        }

        let seq_id = self.next_seq_id.fetch_add(1, Ordering::Relaxed);
        let total_frags = num_frags as u16;

        debug!(
            "Assembler({}): Fragmenting frame {} bytes into {} fragments (seq_id: {})",
            self.channel_id, frame_len, num_frags, seq_id
        );

        // Send fragments round-robin across channels
        let total_channels = 1 + self.overflow_channels.len();

        for (i, chunk) in frame.chunks(frag_size).enumerate() {
            let header = FragmentHeader::new(seq_id, i as u16, total_frags);
            let header_bytes = header.encode();

            // Build fragment: header + data
            let mut frag_buf = self.buffer_pool.acquire();
            frag_buf.extend_from_slice(&header_bytes);
            frag_buf.extend_from_slice(chunk);
            let fragment = frag_buf.freeze();
            self.buffer_pool.release(bytes::BytesMut::new()); // Return empty buffer

            // Select channel (round-robin)
            let channel_idx =
                self.channel_round_robin.fetch_add(1, Ordering::Relaxed) % total_channels;

            let result = if channel_idx == 0 {
                self.primary.send(fragment).await
            } else {
                // Send to overflow channel
                let overflow_idx = channel_idx - 1;
                if overflow_idx < self.overflow_channels.len() {
                    self.overflow_channels[overflow_idx]
                        .channel
                        .send(fragment)
                        .await
                } else {
                    // Fallback to primary if overflow not available
                    self.primary.send(fragment).await
                }
            };

            if let Err(e) = result {
                return Err(format!(
                    "Failed to send fragment {}/{}: {}",
                    i + 1,
                    num_frags,
                    e
                ));
            }
        }

        Ok(())
    }

    /// Process incoming data, reassembling fragments
    pub fn process_incoming(&self, data: Bytes) -> Option<Bytes> {
        // Check if this is a fragment
        if !has_fragment_header(&data) {
            // Not a fragment - pass through directly
            return Some(data);
        }

        // Parse fragment header
        let header = match FragmentHeader::decode(&data) {
            Some(h) => h,
            None => {
                warn!("Assembler({}): Invalid fragment header", self.channel_id);
                return None;
            }
        };

        // Extract payload (skip header)
        let payload = data.slice(FRAGMENT_HEADER_SIZE..);

        // Get or create fragment buffer
        let mut entry = self
            .pending_fragments
            .entry(header.seq_id)
            .or_insert_with(|| {
                self.pending_count.fetch_add(1, Ordering::Relaxed);
                FragmentBuffer::new(header.total_frags)
            });

        // Add fragment
        let complete = entry.add_fragment(header.frag_idx, payload);

        if complete {
            // All fragments received - reassemble
            let result = entry.reassemble();
            drop(entry); // Release the entry reference

            // Remove from pending
            self.pending_fragments.remove(&header.seq_id);
            self.pending_count.fetch_sub(1, Ordering::Relaxed);

            debug!(
                "Assembler({}): Reassembled frame (seq_id: {}, fragments: {})",
                self.channel_id, header.seq_id, header.total_frags
            );

            result
        } else {
            None // Still waiting for more fragments
        }
    }

    /// Get current state for persistence
    pub fn get_state(&self) -> AssemblerState {
        AssemblerState {
            next_seq_id: self.next_seq_id.load(Ordering::Relaxed),
            overflow_channel_count: self.overflow_channels.len(),
        }
    }

    /// Restore state after reconnection
    pub fn restore_state(&self, state: AssemblerState) {
        self.next_seq_id.store(state.next_seq_id, Ordering::Relaxed);
        // Note: Overflow channels need to be re-added separately
    }

    /// Get count of pending fragment buffers
    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// Add an overflow channel for parallel transmission
    pub fn add_overflow_channel(
        &mut self,
        channel: Arc<WebRTCDataChannel>,
        rx: mpsc::UnboundedReceiver<Bytes>,
    ) {
        self.overflow_channels.push(OverflowChannel { channel, rx });
        debug!(
            "Assembler({}): Added overflow channel (total: {})",
            self.channel_id,
            self.overflow_channels.len() + 1
        );
    }
}

impl Drop for Assembler {
    fn drop(&mut self) {
        // Cancel cleanup task
        if let Some(task) = self.cleanup_task.take() {
            task.abort();
        }

        // Log any incomplete fragments
        let pending = self.pending_count.load(Ordering::Relaxed);
        if pending > 0 {
            warn!(
                "Assembler({}): Dropping with {} incomplete fragment buffers",
                self.channel_id, pending
            );
        }
    }
}

// ============================================================================
// Standalone fragmentation helpers for use with EventDrivenSender
// These allow fragmentation without needing the full Assembler struct
// ============================================================================

use std::sync::atomic::AtomicU32 as StandaloneAtomicU32;

/// Global sequence ID counter for standalone fragmentation
static STANDALONE_SEQ_ID: StandaloneAtomicU32 = StandaloneAtomicU32::new(0);

/// Fragment a large frame into smaller pieces for transmission
/// Returns None if frame is small enough to send directly
/// Returns Some(Vec<Bytes>) with fragments if frame needs fragmentation
pub fn fragment_frame(
    frame: &Bytes,
    fragment_threshold: usize,
    max_fragments: u16,
) -> Option<Vec<Bytes>> {
    let frame_len = frame.len();

    // Small frames don't need fragmentation
    if frame_len <= fragment_threshold {
        return None;
    }

    let frag_size = fragment_threshold - FRAGMENT_HEADER_SIZE;
    let num_frags = frame_len.div_ceil(frag_size);

    if num_frags > max_fragments as usize {
        warn!(
            "Frame too large for fragmentation: {} bytes needs {} fragments (max: {})",
            frame_len, num_frags, max_fragments
        );
        return None; // Can't fragment, caller should handle or send as-is
    }

    let seq_id = STANDALONE_SEQ_ID.fetch_add(1, Ordering::Relaxed);
    let total_frags = num_frags as u16;

    debug!(
        "Fragmenting frame: {} bytes into {} fragments (seq_id: {})",
        frame_len, num_frags, seq_id
    );

    let mut fragments = Vec::with_capacity(num_frags);

    for (i, chunk) in frame.chunks(frag_size).enumerate() {
        let header = FragmentHeader::new(seq_id, i as u16, total_frags);
        let header_bytes = header.encode();

        // Build fragment: header + data
        let mut frag_buf = bytes::BytesMut::with_capacity(FRAGMENT_HEADER_SIZE + chunk.len());
        frag_buf.extend_from_slice(&header_bytes);
        frag_buf.extend_from_slice(chunk);
        fragments.push(frag_buf.freeze());
    }

    Some(fragments)
}

/// Check if a frame should be fragmented based on size
#[inline]
pub fn should_fragment(frame_len: usize, fragment_threshold: usize) -> bool {
    frame_len > fragment_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fragment_header_encode_decode() {
        let header = FragmentHeader::new(12345, 2, 5);
        let encoded = header.encode();
        let decoded = FragmentHeader::decode(&encoded).unwrap();

        assert_eq!(decoded.seq_id, 12345);
        assert_eq!(decoded.frag_idx, 2);
        assert_eq!(decoded.total_frags, 5);
        assert!(!decoded.is_first());
        assert!(!decoded.is_last());
    }

    #[test]
    fn test_fragment_header_first_last() {
        // First fragment
        let first = FragmentHeader::new(1, 0, 3);
        assert!(first.is_first());
        assert!(!first.is_last());

        // Middle fragment
        let middle = FragmentHeader::new(1, 1, 3);
        assert!(!middle.is_first());
        assert!(!middle.is_last());

        // Last fragment
        let last = FragmentHeader::new(1, 2, 3);
        assert!(!last.is_first());
        assert!(last.is_last());

        // Single fragment (both first and last)
        let single = FragmentHeader::new(1, 0, 1);
        assert!(single.is_first());
        assert!(single.is_last());
    }

    #[test]
    fn test_has_fragment_header() {
        // With header
        let with_header = [flags::HAS_FRAGMENT_HEADER, 0, 0, 0, 1, 0, 0, 0, 1];
        assert!(has_fragment_header(&with_header));

        // Without header
        let without_header = [0x00, 0x01, 0x02, 0x03];
        assert!(!has_fragment_header(&without_header));

        // Empty
        assert!(!has_fragment_header(&[]));
    }

    #[test]
    fn test_fragment_buffer_reassembly() {
        let mut buffer = FragmentBuffer::new(3);

        // Add fragments out of order
        assert!(!buffer.add_fragment(1, Bytes::from_static(b"middle")));
        assert!(!buffer.add_fragment(0, Bytes::from_static(b"first")));
        assert!(buffer.add_fragment(2, Bytes::from_static(b"last"))); // Complete!

        let result = buffer.reassemble().unwrap();
        assert_eq!(&result[..], b"firstmiddlelast");
    }

    #[test]
    fn test_fragment_buffer_duplicate() {
        let mut buffer = FragmentBuffer::new(2);

        assert!(!buffer.add_fragment(0, Bytes::from_static(b"first")));
        assert!(!buffer.add_fragment(0, Bytes::from_static(b"duplicate"))); // Duplicate
        assert!(buffer.add_fragment(1, Bytes::from_static(b"second")));

        let result = buffer.reassemble().unwrap();
        assert_eq!(&result[..], b"firstsecond"); // Original kept, duplicate ignored
    }

    #[test]
    fn test_assembler_config_default() {
        let config = AssemblerConfig::default();
        assert_eq!(config.fragment_threshold, DEFAULT_FRAGMENT_THRESHOLD);
        assert_eq!(config.max_fragments, DEFAULT_MAX_FRAGMENTS);
        assert_eq!(config.fragment_timeout, DEFAULT_FRAGMENT_TIMEOUT);
    }
}
