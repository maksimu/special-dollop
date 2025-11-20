// Simple multi-channel sender for WebRTC data channels
//
// Distributes video frames across multiple channels using round-robin.
// Drops oversized frames (video codec handles missing frames gracefully).

use bytes::Bytes;
use log::warn;
use std::sync::{Arc, Mutex};

/// Simple multi-channel sender for WebRTC data channels
///
/// Distributes video frames across multiple channels using round-robin.
/// Frames are sent whole (no fragmentation) - if a frame is too large, it's dropped.
///
/// This is simpler than fragmentation-based multi-channel because:
/// - No reassembly needed on client
/// - Video codec handles missing frames gracefully
/// - Lower latency (no fragmentation overhead)
/// - Accounts for protocol overhead (60KB payload limit)
pub struct SimpleMultiChannelSender {
    channels: Vec<Arc<dyn WebRTCDataChannel>>,
    current_channel: Arc<Mutex<usize>>,
    max_payload_size: usize, // 60KB (accounts for overhead)
}

/// Trait for WebRTC data channel (adapt to your actual type)
pub trait WebRTCDataChannel: Send + Sync {
    /// Send data on this channel
    fn send(&self, data: Bytes) -> Result<(), String>;
}

impl SimpleMultiChannelSender {
    /// Create a new simple multi-channel sender
    ///
    /// # Arguments
    ///
    /// * `channels` - Vector of WebRTC data channels
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use guacr_handlers::multi_channel::{SimpleMultiChannelSender, WebRTCDataChannel};
    /// use guacr_protocol::MAX_SAFE_PAYLOAD_SIZE;
    /// use std::sync::Arc;
    ///
    /// struct MyChannel;
    /// impl WebRTCDataChannel for MyChannel {
    ///     fn send(&self, _data: Bytes) -> Result<(), String> { Ok(()) }
    /// }
    ///
    /// let channels: Vec<Arc<dyn WebRTCDataChannel>> = vec![
    ///     Arc::new(MyChannel),
    ///     Arc::new(MyChannel),
    ///     Arc::new(MyChannel),
    /// ];
    /// let sender = SimpleMultiChannelSender::new(channels);
    /// ```
    pub fn new(channels: Vec<Arc<dyn WebRTCDataChannel>>) -> Self {
        use guacr_protocol::MAX_SAFE_PAYLOAD_SIZE;

        Self {
            channels,
            current_channel: Arc::new(Mutex::new(0)),
            // 60KB payload + 25 bytes overhead = ~60KB total message
            // Well under 64KB WebRTC limit, leaves headroom
            max_payload_size: MAX_SAFE_PAYLOAD_SIZE,
        }
    }

    /// Send a video frame
    ///
    /// # Arguments
    ///
    /// * `frame` - Video frame data (e.g., H.264 NAL units)
    ///
    /// # Returns
    ///
    /// `Ok(())` if frame was sent or dropped (oversized frames are dropped).
    /// `Err(String)` if channel send failed.
    ///
    /// # Behavior
    ///
    /// - Frames ≤ 60KB: Sent on next channel (round-robin)
    /// - Frames > 60KB: Dropped with warning (video codec handles missing frames)
    ///
    /// Frame will be wrapped in Frame protocol (17 bytes) + Binary protocol (8 bytes).
    /// Total message: frame.len() + 25 bytes
    /// This should be < 64KB if frame.len() <= 60KB
    pub fn send_frame(&self, frame: Bytes) -> Result<(), String> {
        // Check payload size (before overhead)
        if frame.len() > self.max_payload_size {
            warn!(
                "Frame payload too large ({} bytes > {} bytes), dropping",
                frame.len(),
                self.max_payload_size
            );
            // Drop frame - video codec handles missing frames
            return Ok(());
        }

        if self.channels.is_empty() {
            return Err("No channels available".to_string());
        }

        // Round-robin to channel
        let channel_idx = {
            let mut idx = self.current_channel.lock().unwrap();
            let current = *idx;
            *idx = (*idx + 1) % self.channels.len();
            current
        };

        // Frame will be wrapped in Frame protocol (17 bytes) + Binary protocol (8 bytes)
        // Total message: frame.len() + 25 bytes
        // This should be < 64KB if frame.len() <= 60KB
        self.channels[channel_idx].send(frame)
    }

    /// Get the maximum payload size (before protocol overhead)
    pub fn max_payload_size(&self) -> usize {
        self.max_payload_size
    }

    /// Get the number of channels
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockChannel {
        sent: Arc<AtomicUsize>,
    }

    impl WebRTCDataChannel for MockChannel {
        fn send(&self, _data: Bytes) -> Result<(), String> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn test_round_robin() {
        let sent1 = Arc::new(AtomicUsize::new(0));
        let sent2 = Arc::new(AtomicUsize::new(0));
        let sent3 = Arc::new(AtomicUsize::new(0));

        let channels: Vec<Arc<dyn WebRTCDataChannel>> = vec![
            Arc::new(MockChannel {
                sent: sent1.clone(),
            }),
            Arc::new(MockChannel {
                sent: sent2.clone(),
            }),
            Arc::new(MockChannel {
                sent: sent3.clone(),
            }),
        ];

        let sender = SimpleMultiChannelSender::new(channels);

        // Send 3 frames - should round-robin
        for _ in 0..3 {
            sender.send_frame(Bytes::from(vec![0u8; 1000])).unwrap();
        }

        assert_eq!(sent1.load(Ordering::Relaxed), 1);
        assert_eq!(sent2.load(Ordering::Relaxed), 1);
        assert_eq!(sent3.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_oversized_frame_dropped() {
        let sent = Arc::new(AtomicUsize::new(0));
        let channels: Vec<Arc<dyn WebRTCDataChannel>> =
            vec![Arc::new(MockChannel { sent: sent.clone() })];

        let sender = SimpleMultiChannelSender::new(channels);

        // Send oversized frame (> 60KB)
        let oversized = Bytes::from(vec![0u8; 100 * 1024]); // 100KB
        sender.send_frame(oversized).unwrap(); // Should drop, not error

        // Should not have sent anything
        assert_eq!(sent.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_empty_channels() {
        let channels: Vec<Arc<dyn WebRTCDataChannel>> = vec![];
        let sender = SimpleMultiChannelSender::new(channels);

        let result = sender.send_frame(Bytes::from(vec![0u8; 1000]));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No channels"));
    }
}
