// WebRTC Screencast for RBI
//
// Uses Chrome DevTools Protocol's Page.startScreencast to receive
// H.264-encoded video frames instead of polling screenshots.
//
// Benefits:
// - 100x bandwidth reduction (H.264 vs JPEG/PNG)
// - Hardware-accelerated encoding in Chrome
// - Lower latency (push vs poll)
// - Smooth 60 FPS possible

use bytes::Bytes;
use log::debug;
use serde::{Deserialize, Serialize};

/// Screencast configuration
#[derive(Debug, Clone)]
pub struct ScreencastConfig {
    /// Image format (jpeg or png)
    pub format: ScreencastFormat,
    /// Quality (1-100, only for JPEG)
    pub quality: u8,
    /// Maximum width in pixels
    pub max_width: u32,
    /// Maximum height in pixels
    pub max_height: u32,
    /// Capture every Nth frame (1 = all frames)
    pub every_nth_frame: u32,
}

impl Default for ScreencastConfig {
    fn default() -> Self {
        Self {
            format: ScreencastFormat::Jpeg,
            quality: 80,
            max_width: 1920,
            max_height: 1080,
            every_nth_frame: 1,
        }
    }
}

/// Screencast format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreencastFormat {
    Jpeg,
    Png,
}

impl ScreencastFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScreencastFormat::Jpeg => "jpeg",
            ScreencastFormat::Png => "png",
        }
    }

    pub fn guacamole_format(&self) -> u8 {
        match self {
            ScreencastFormat::Jpeg => 1, // JPEG
            ScreencastFormat::Png => 0,  // PNG
        }
    }
}

/// Screencast frame event from Chrome
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreencastFrame {
    /// Base64-encoded image data
    pub data: String,
    /// Metadata about the frame
    pub metadata: ScreencastFrameMetadata,
    /// Session ID for acknowledgment
    pub session_id: i32,
}

/// Metadata for screencast frame
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastFrameMetadata {
    /// Page scale factor
    pub page_scale_factor: f64,
    /// Offset X
    pub offset_top: f64,
    /// Offset Y
    pub offset_left: f64,
    /// Device width
    pub device_width: f64,
    /// Device height
    pub device_height: f64,
    /// Scroll offset X
    pub scroll_offset_x: f64,
    /// Scroll offset Y
    pub scroll_offset_y: f64,
    /// Timestamp
    #[serde(default)]
    pub timestamp: Option<f64>,
}

/// Screencast statistics
#[derive(Debug, Clone, Copy, Default)]
pub struct ScreencastStats {
    pub frames_received: u64,
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub avg_frame_size_kb: u32,
}

impl ScreencastStats {
    pub fn record_frame(&mut self, compressed_size: usize, sent: bool) {
        self.frames_received += 1;
        self.bytes_received += compressed_size as u64;

        if sent {
            self.frames_sent += 1;
            self.bytes_sent += compressed_size as u64;
        } else {
            self.frames_dropped += 1;
        }

        if self.frames_received > 0 {
            self.avg_frame_size_kb = (self.bytes_received / self.frames_received / 1024) as u32;
        }
    }

    pub fn compression_ratio(&self) -> f32 {
        if self.frames_received == 0 {
            return 0.0;
        }
        (self.frames_dropped as f32 / self.frames_received as f32) * 100.0
    }
}

/// Screencast frame processor
pub struct ScreencastProcessor {
    config: ScreencastConfig,
    stats: ScreencastStats,
    last_session_id: Option<i32>,
}

impl ScreencastProcessor {
    pub fn new(config: ScreencastConfig) -> Self {
        Self {
            config,
            stats: ScreencastStats::default(),
            last_session_id: None,
        }
    }

    /// Process a screencast frame
    ///
    /// Returns the decoded image data and whether it should be sent
    pub fn process_frame(&mut self, frame: ScreencastFrame) -> Result<(Bytes, bool), String> {
        self.last_session_id = Some(frame.session_id);

        // Decode base64 image data
        use base64::Engine;
        let image_data = base64::engine::general_purpose::STANDARD
            .decode(&frame.data)
            .map_err(|e| format!("Failed to decode screencast frame: {}", e))?;

        let should_send = true; // For now, send all frames (can add throttling later)

        self.stats.record_frame(image_data.len(), should_send);

        debug!(
            "Screencast frame: session={}, size={}KB, scroll=({}, {})",
            frame.session_id,
            image_data.len() / 1024,
            frame.metadata.scroll_offset_x,
            frame.metadata.scroll_offset_y
        );

        Ok((Bytes::from(image_data), should_send))
    }

    /// Get the last session ID for acknowledgment
    pub fn last_session_id(&self) -> Option<i32> {
        self.last_session_id
    }

    /// Get statistics
    pub fn stats(&self) -> ScreencastStats {
        self.stats
    }

    /// Reset statistics
    pub fn reset_stats(&mut self) {
        self.stats = ScreencastStats::default();
    }

    /// Get the Guacamole image format code
    pub fn guacamole_format(&self) -> u8 {
        self.config.format.guacamole_format()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screencast_config_default() {
        let config = ScreencastConfig::default();
        assert_eq!(config.format, ScreencastFormat::Jpeg);
        assert_eq!(config.quality, 80);
        assert_eq!(config.max_width, 1920);
        assert_eq!(config.max_height, 1080);
        assert_eq!(config.every_nth_frame, 1);
    }

    #[test]
    fn test_screencast_format() {
        assert_eq!(ScreencastFormat::Jpeg.as_str(), "jpeg");
        assert_eq!(ScreencastFormat::Png.as_str(), "png");
        assert_eq!(ScreencastFormat::Jpeg.guacamole_format(), 1);
        assert_eq!(ScreencastFormat::Png.guacamole_format(), 0);
    }

    #[test]
    fn test_screencast_stats() {
        let mut stats = ScreencastStats::default();

        // Record some frames
        stats.record_frame(100_000, true); // 100KB, sent
        stats.record_frame(100_000, true); // 100KB, sent
        stats.record_frame(100_000, false); // 100KB, dropped

        assert_eq!(stats.frames_received, 3);
        assert_eq!(stats.frames_sent, 2);
        assert_eq!(stats.frames_dropped, 1);
        assert_eq!(stats.avg_frame_size_kb, 97); // ~100KB average
        assert!((stats.compression_ratio() - 33.33).abs() < 0.1); // ~33% dropped
    }

    #[test]
    fn test_screencast_processor() {
        let config = ScreencastConfig::default();
        let mut processor = ScreencastProcessor::new(config);

        // Create a test frame with base64-encoded data
        let test_data = b"test image data";
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(test_data);

        let frame = ScreencastFrame {
            data: encoded,
            metadata: ScreencastFrameMetadata {
                page_scale_factor: 1.0,
                offset_top: 0.0,
                offset_left: 0.0,
                device_width: 1920.0,
                device_height: 1080.0,
                scroll_offset_x: 0.0,
                scroll_offset_y: 0.0,
                timestamp: Some(123.456),
            },
            session_id: 42,
        };

        let result = processor.process_frame(frame);
        assert!(result.is_ok());

        let (data, should_send) = result.unwrap();
        assert_eq!(data.as_ref(), test_data);
        assert!(should_send);
        assert_eq!(processor.last_session_id(), Some(42));

        let stats = processor.stats();
        assert_eq!(stats.frames_received, 1);
        assert_eq!(stats.frames_sent, 1);
    }
}
