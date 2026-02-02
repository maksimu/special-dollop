// throughput.rs - Bandwidth monitoring for protocol handlers
//
// Tracks sent data over a rolling time window to calculate throughput.
// Used by adaptive quality systems to adjust encoding parameters based on
// measured bandwidth.

use std::collections::VecDeque;
use std::time::Instant;

/// Tracks throughput over a rolling window of recent sends
pub struct ThroughputTracker {
    send_history: VecDeque<(Instant, usize)>,
    window_size: usize,
}

impl ThroughputTracker {
    /// Create a new throughput tracker with specified window size
    ///
    /// # Arguments
    ///
    /// * `window_size` - Number of recent sends to track (default: 10)
    pub fn new(window_size: usize) -> Self {
        Self {
            send_history: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Track a sent frame/packet
    ///
    /// Records the current timestamp and byte count. Maintains a bounded
    /// VecDeque by dropping oldest entry when window is full.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Number of bytes sent in this frame
    pub fn track_sent(&mut self, bytes: usize) {
        self.send_history.push_back((Instant::now(), bytes));
        if self.send_history.len() > self.window_size {
            self.send_history.pop_front();
        }
    }

    /// Calculate current throughput in megabits per second
    ///
    /// Returns 0.0 if insufficient data (< 2 samples).
    /// Calculates from oldest sample to now to get accurate time span.
    pub fn throughput_mbps(&self) -> f64 {
        if self.send_history.len() < 2 {
            return 0.0;
        }

        let total_bytes: usize = self.send_history.iter().map(|(_, b)| b).sum();
        let time_span = Instant::now().duration_since(self.send_history.front().unwrap().0);

        if time_span.as_secs_f64() < 0.001 {
            // Avoid division by zero for very rapid calls
            return 0.0;
        }

        (total_bytes as f64 * 8.0) / time_span.as_secs_f64() / 1_000_000.0
    }

    /// Get the number of tracked samples
    pub fn sample_count(&self) -> usize {
        self.send_history.len()
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.send_history.clear();
    }
}

impl Default for ThroughputTracker {
    fn default() -> Self {
        Self::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_throughput_calculation() {
        let mut tracker = ThroughputTracker::new(10);

        // Simulate sending 1MB over 1 second = 8 Mbps
        tracker.track_sent(1_000_000);
        thread::sleep(Duration::from_millis(100));
        tracker.track_sent(1_000_000);

        let throughput = tracker.throughput_mbps();
        assert!(throughput > 0.0);
    }

    #[test]
    fn test_window_size_limit() {
        let mut tracker = ThroughputTracker::new(3);

        for _ in 0..10 {
            tracker.track_sent(1000);
        }

        assert_eq!(tracker.sample_count(), 3);
    }
}
