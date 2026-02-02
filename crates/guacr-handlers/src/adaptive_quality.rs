// adaptive_quality.rs - Dynamic quality adjustment for image-based protocols
//
// Automatically adjusts JPEG/WebP quality based on measured throughput to prevent
// buffering and frame drops on slow connections. Extracted from RDP handler for
// reuse across SSH, VNC, and other image-based protocols.

use std::time::Instant;

use crate::throughput::ThroughputTracker;

/// Manages adaptive quality adjustment based on measured throughput
///
/// Quality is adjusted on a scale from 10 (minimum) to max_quality (typically 85-95).
/// The algorithm smoothly adjusts quality to match available bandwidth:
///
/// - 10+ Mbps: Use maximum configured quality
/// - 1-10 Mbps: Scale from 50 to max_quality
/// - 100 kbps - 1 Mbps: Aggressive reduction (20-50)
/// - < 100 kbps: Emergency mode (10-20)
///
/// Adjustments are rate-limited to once per second with max change of ±10 per adjustment
/// to avoid quality thrashing.
pub struct AdaptiveQuality {
    current_quality: u8,
    max_quality: u8,
    throughput_tracker: ThroughputTracker,
    last_quality_adjustment: Instant,
    adjustment_interval_secs: u64,
    max_adjustment_per_interval: u8,
}

impl AdaptiveQuality {
    /// Create a new adaptive quality manager
    ///
    /// # Arguments
    ///
    /// * `max_quality` - Maximum quality level (1-100, typically 85-95)
    pub fn new(max_quality: u8) -> Self {
        Self {
            current_quality: max_quality,
            max_quality: max_quality.clamp(10, 100),
            throughput_tracker: ThroughputTracker::new(10),
            last_quality_adjustment: Instant::now(),
            adjustment_interval_secs: 1,
            max_adjustment_per_interval: 10,
        }
    }

    /// Track a sent frame for throughput calculation
    ///
    /// Call this after sending each frame. The frame size is used to calculate
    /// throughput over a rolling window.
    ///
    /// # Arguments
    ///
    /// * `bytes_sent` - Number of bytes in the frame that was sent
    pub fn track_frame_sent(&mut self, bytes_sent: usize) {
        self.throughput_tracker.track_sent(bytes_sent);
    }

    /// Calculate the current recommended quality level
    ///
    /// This should be called before encoding each frame. Quality is adjusted
    /// at most once per adjustment interval (default 1 second) to avoid thrashing.
    ///
    /// # Returns
    ///
    /// Quality level from 10 (minimum) to max_quality (maximum)
    pub fn calculate_quality(&mut self) -> u8 {
        let now = Instant::now();

        // Rate-limit adjustments to avoid thrashing
        if now.duration_since(self.last_quality_adjustment).as_secs()
            < self.adjustment_interval_secs
        {
            return self.current_quality;
        }

        // Need at least 2 samples for meaningful throughput calculation
        if self.throughput_tracker.sample_count() < 2 {
            return self.current_quality;
        }

        let throughput_mbps = self.throughput_tracker.throughput_mbps();

        // Map throughput to target quality
        let target_quality = if throughput_mbps >= 10.0 {
            // High bandwidth: use maximum quality
            self.max_quality
        } else if throughput_mbps >= 1.0 {
            // 1-10 Mbps: scale from 50 to max_quality
            let ratio = (throughput_mbps - 1.0) / 9.0;
            50 + (ratio * (self.max_quality - 50) as f64) as u8
        } else if throughput_mbps >= 0.1 {
            // 100 kbps - 1 Mbps: aggressive reduction (20-50)
            let ratio = (throughput_mbps - 0.1) / 0.9;
            20 + (ratio * 30.0) as u8
        } else {
            // < 100 kbps: emergency mode (10-20)
            10 + ((throughput_mbps / 0.1) * 10.0) as u8
        };

        // Smooth adjustments: limit change to max_adjustment_per_interval
        let new_quality = if target_quality > self.current_quality {
            self.current_quality
                .saturating_add(self.max_adjustment_per_interval)
                .min(target_quality)
        } else {
            self.current_quality
                .saturating_sub(self.max_adjustment_per_interval)
                .max(target_quality)
        };

        self.current_quality = new_quality.clamp(10, self.max_quality);
        self.last_quality_adjustment = now;

        self.current_quality
    }

    /// Get the current quality level without recalculating
    pub fn current_quality(&self) -> u8 {
        self.current_quality
    }

    /// Get the maximum configured quality
    pub fn max_quality(&self) -> u8 {
        self.max_quality
    }

    /// Get the current measured throughput in Mbps
    pub fn current_throughput_mbps(&self) -> f64 {
        self.throughput_tracker.throughput_mbps()
    }

    /// Reset quality to maximum and clear history
    ///
    /// Useful when connection conditions change dramatically
    /// (e.g., after a long pause or reconnection)
    pub fn reset(&mut self) {
        self.current_quality = self.max_quality;
        self.throughput_tracker.clear();
        self.last_quality_adjustment = Instant::now();
    }

    /// Configure adjustment parameters
    ///
    /// # Arguments
    ///
    /// * `interval_secs` - Minimum seconds between adjustments
    /// * `max_change` - Maximum quality change per adjustment
    pub fn configure_adjustment(&mut self, interval_secs: u64, max_change: u8) {
        self.adjustment_interval_secs = interval_secs.max(1);
        self.max_adjustment_per_interval = max_change.clamp(1, 50);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_initial_quality() {
        let aq = AdaptiveQuality::new(85);
        assert_eq!(aq.current_quality(), 85);
    }

    #[test]
    fn test_quality_bounds() {
        let aq = AdaptiveQuality::new(200); // Invalid, should clamp
        assert_eq!(aq.max_quality(), 100);
    }

    #[test]
    fn test_rate_limiting() {
        let mut aq = AdaptiveQuality::new(85);

        // Track some frames to simulate low bandwidth
        for _ in 0..5 {
            aq.track_frame_sent(100);
            thread::sleep(Duration::from_millis(10));
        }

        let q1 = aq.calculate_quality();
        let q2 = aq.calculate_quality(); // Should return same due to rate limit

        assert_eq!(q1, q2);
    }

    #[test]
    fn test_reset() {
        let mut aq = AdaptiveQuality::new(85);
        aq.current_quality = 20; // Simulate degraded quality
        aq.reset();
        assert_eq!(aq.current_quality(), 85);
    }
}
