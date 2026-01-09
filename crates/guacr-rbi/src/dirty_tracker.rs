// RBI Dirty Region Tracker
//
// Detects when screenshots have changed to avoid sending duplicate frames.
// Uses hash-based comparison for fast change detection.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Tracks screenshot changes to avoid sending duplicate frames
///
/// Uses hash-based comparison for O(1) change detection without
/// pixel-by-pixel comparison. Stores last screenshot for optional
/// detailed change analysis.
pub struct RbiDirtyTracker {
    last_hash: u64,
    last_screenshot: Option<Vec<u8>>,
    frames_captured: u64,
    frames_sent: u64,
    frames_skipped: u64,
}

impl RbiDirtyTracker {
    /// Create a new dirty tracker
    pub fn new() -> Self {
        Self {
            last_hash: 0,
            last_screenshot: None,
            frames_captured: 0,
            frames_sent: 0,
            frames_skipped: 0,
        }
    }

    /// Check if screenshot has changed since last check
    ///
    /// Returns true if this is a new screenshot or if content has changed.
    /// Updates internal state to track the new screenshot.
    pub fn has_changed(&mut self, screenshot: &[u8]) -> bool {
        self.frames_captured += 1;

        // Fast hash comparison
        let mut hasher = DefaultHasher::new();
        screenshot.hash(&mut hasher);
        let hash = hasher.finish();

        if hash != self.last_hash {
            self.last_hash = hash;
            self.last_screenshot = Some(screenshot.to_vec());
            self.frames_sent += 1;
            true
        } else {
            self.frames_skipped += 1;
            false
        }
    }

    /// Get percentage of pixels that changed (requires pixel comparison)
    ///
    /// Returns None if no previous screenshot exists.
    /// This is more expensive than has_changed() but provides detailed metrics.
    pub fn change_percentage(&self, new_screenshot: &[u8]) -> Option<f32> {
        let old = self.last_screenshot.as_ref()?;
        if old.len() != new_screenshot.len() {
            return Some(100.0);
        }

        let changed = old
            .iter()
            .zip(new_screenshot.iter())
            .filter(|(a, b)| a != b)
            .count();

        Some((changed as f32 / old.len() as f32) * 100.0)
    }

    /// Get compression ratio (frames skipped / frames captured)
    pub fn compression_ratio(&self) -> f32 {
        if self.frames_captured == 0 {
            return 0.0;
        }
        (self.frames_skipped as f32 / self.frames_captured as f32) * 100.0
    }

    /// Get statistics for monitoring
    pub fn stats(&self) -> DirtyTrackerStats {
        DirtyTrackerStats {
            frames_captured: self.frames_captured,
            frames_sent: self.frames_sent,
            frames_skipped: self.frames_skipped,
            compression_ratio: self.compression_ratio(),
        }
    }

    /// Reset statistics (useful for periodic reporting)
    pub fn reset_stats(&mut self) {
        self.frames_captured = 0;
        self.frames_sent = 0;
        self.frames_skipped = 0;
    }
}

impl Default for RbiDirtyTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics from dirty tracker
#[derive(Debug, Clone, Copy)]
pub struct DirtyTrackerStats {
    pub frames_captured: u64,
    pub frames_sent: u64,
    pub frames_skipped: u64,
    pub compression_ratio: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_tracker_new() {
        let tracker = RbiDirtyTracker::new();
        assert_eq!(tracker.frames_captured, 0);
        assert_eq!(tracker.frames_sent, 0);
        assert_eq!(tracker.frames_skipped, 0);
    }

    #[test]
    fn test_first_frame_always_changes() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot = vec![1, 2, 3, 4];
        assert!(tracker.has_changed(&screenshot));
        assert_eq!(tracker.frames_captured, 1);
        assert_eq!(tracker.frames_sent, 1);
        assert_eq!(tracker.frames_skipped, 0);
    }

    #[test]
    fn test_identical_frames_skipped() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot = vec![1, 2, 3, 4];

        // First frame
        assert!(tracker.has_changed(&screenshot));

        // Second identical frame
        assert!(!tracker.has_changed(&screenshot));
        assert_eq!(tracker.frames_captured, 2);
        assert_eq!(tracker.frames_sent, 1);
        assert_eq!(tracker.frames_skipped, 1);

        // Third identical frame
        assert!(!tracker.has_changed(&screenshot));
        assert_eq!(tracker.frames_captured, 3);
        assert_eq!(tracker.frames_sent, 1);
        assert_eq!(tracker.frames_skipped, 2);
    }

    #[test]
    fn test_changed_frames_detected() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot1 = vec![1, 2, 3, 4];
        let screenshot2 = vec![1, 2, 3, 5]; // Last byte different

        assert!(tracker.has_changed(&screenshot1));
        assert!(tracker.has_changed(&screenshot2));
        assert_eq!(tracker.frames_sent, 2);
        assert_eq!(tracker.frames_skipped, 0);
    }

    #[test]
    fn test_compression_ratio() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot = vec![1, 2, 3, 4];

        // Send first frame
        tracker.has_changed(&screenshot);

        // Skip 9 identical frames
        for _ in 0..9 {
            tracker.has_changed(&screenshot);
        }

        // Should be 90% compression (9 skipped / 10 total)
        assert_eq!(tracker.compression_ratio(), 90.0);
    }

    #[test]
    fn test_change_percentage() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot1 = vec![1, 2, 3, 4];
        let screenshot2 = vec![1, 2, 3, 5]; // 1 byte different out of 4 = 25%

        tracker.has_changed(&screenshot1);
        let change_pct = tracker.change_percentage(&screenshot2);
        assert_eq!(change_pct, Some(25.0));
    }

    #[test]
    fn test_stats() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot = vec![1, 2, 3, 4];

        tracker.has_changed(&screenshot);
        tracker.has_changed(&screenshot);
        tracker.has_changed(&screenshot);

        let stats = tracker.stats();
        assert_eq!(stats.frames_captured, 3);
        assert_eq!(stats.frames_sent, 1);
        assert_eq!(stats.frames_skipped, 2);
    }

    #[test]
    fn test_reset_stats() {
        let mut tracker = RbiDirtyTracker::new();
        let screenshot = vec![1, 2, 3, 4];

        tracker.has_changed(&screenshot);
        tracker.has_changed(&screenshot);

        tracker.reset_stats();

        let stats = tracker.stats();
        assert_eq!(stats.frames_captured, 0);
        assert_eq!(stats.frames_sent, 0);
        assert_eq!(stats.frames_skipped, 0);
    }
}
