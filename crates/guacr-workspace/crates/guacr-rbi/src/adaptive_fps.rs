// Adaptive FPS Manager for RBI
//
// Dynamically adjusts screenshot capture rate based on page activity.
// Reduces CPU and bandwidth usage for static pages while maintaining
// responsiveness for active content.

use tokio::time::Duration;

/// Manages adaptive frame rate based on content changes
///
/// Starts at max FPS and gradually reduces to min FPS when no changes
/// are detected. Immediately returns to max FPS when activity resumes.
///
/// # Example
///
/// ```
/// use guacr_rbi::adaptive_fps::AdaptiveFps;
/// use tokio::time::Duration;
///
/// let mut fps = AdaptiveFps::new(5, 30); // 5-30 FPS range
///
/// // Active content
/// let interval = fps.update(true);  // Returns ~33ms (30 FPS)
///
/// // Static content (after 30 frames)
/// for _ in 0..30 {
///     fps.update(false);
/// }
/// let interval = fps.update(false); // Returns ~200ms (5 FPS)
/// ```
pub struct AdaptiveFps {
    min_fps: u32,
    max_fps: u32,
    current_fps: u32,
    frames_unchanged: u32,
    total_frames: u64,
    fps_changes: u64,
}

impl AdaptiveFps {
    /// Create a new adaptive FPS manager
    ///
    /// # Arguments
    ///
    /// * `min_fps` - Minimum frame rate for static content (e.g., 5)
    /// * `max_fps` - Maximum frame rate for active content (e.g., 30)
    pub fn new(min_fps: u32, max_fps: u32) -> Self {
        assert!(min_fps > 0, "min_fps must be greater than 0");
        assert!(max_fps >= min_fps, "max_fps must be >= min_fps");

        Self {
            min_fps,
            max_fps,
            current_fps: max_fps, // Start at max FPS
            frames_unchanged: 0,
            total_frames: 0,
            fps_changes: 0,
        }
    }

    /// Update FPS based on whether frame changed
    ///
    /// Returns the duration to wait before next capture.
    ///
    /// # Arguments
    ///
    /// * `frame_changed` - Whether the current frame differs from previous
    pub fn update(&mut self, frame_changed: bool) -> Duration {
        self.total_frames += 1;
        let old_fps = self.current_fps;

        if frame_changed {
            // Activity detected - increase to max FPS
            self.current_fps = self.max_fps;
            self.frames_unchanged = 0;
        } else {
            // No change - decrease FPS gradually
            self.frames_unchanged += 1;

            // Gradual FPS reduction based on inactivity duration
            if self.frames_unchanged > 90 {
                // After 3 seconds at max FPS (90 frames), drop to min
                self.current_fps = self.min_fps;
            } else if self.frames_unchanged > 60 {
                // After 2 seconds, drop to 1/3 between min and max
                self.current_fps = self.min_fps + (self.max_fps - self.min_fps) / 3;
            } else if self.frames_unchanged > 30 {
                // After 1 second, drop to 2/3 between min and max
                self.current_fps = self.min_fps + 2 * (self.max_fps - self.min_fps) / 3;
            }
            // else: stay at max FPS for first second of inactivity
        }

        if old_fps != self.current_fps {
            self.fps_changes += 1;
        }

        Duration::from_millis(1000 / self.current_fps as u64)
    }

    /// Get current FPS
    pub fn current_fps(&self) -> u32 {
        self.current_fps
    }

    /// Get statistics for monitoring
    pub fn stats(&self) -> AdaptiveFpsStats {
        AdaptiveFpsStats {
            current_fps: self.current_fps,
            min_fps: self.min_fps,
            max_fps: self.max_fps,
            frames_unchanged: self.frames_unchanged,
            total_frames: self.total_frames,
            fps_changes: self.fps_changes,
        }
    }

    /// Check if currently at minimum FPS (static content)
    pub fn is_idle(&self) -> bool {
        self.current_fps == self.min_fps
    }

    /// Check if currently at maximum FPS (active content)
    pub fn is_active(&self) -> bool {
        self.current_fps == self.max_fps
    }

    /// Force FPS to maximum (e.g., during scrolling)
    ///
    /// Resets the inactivity counter to ensure high FPS.
    /// Call this when you detect user activity that needs smooth rendering.
    pub fn boost_fps(&mut self) {
        self.current_fps = self.max_fps;
        self.frames_unchanged = 0;
    }

    /// Get the interval for max FPS (useful for immediate captures)
    pub fn max_fps_interval(&self) -> Duration {
        Duration::from_millis(1000 / self.max_fps as u64)
    }
}

/// Statistics from adaptive FPS manager
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveFpsStats {
    pub current_fps: u32,
    pub min_fps: u32,
    pub max_fps: u32,
    pub frames_unchanged: u32,
    pub total_frames: u64,
    pub fps_changes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_fps_new() {
        let fps = AdaptiveFps::new(5, 30);
        assert_eq!(fps.current_fps(), 30); // Starts at max
        assert_eq!(fps.min_fps, 5);
        assert_eq!(fps.max_fps, 30);
    }

    #[test]
    #[should_panic(expected = "min_fps must be greater than 0")]
    fn test_zero_min_fps_panics() {
        AdaptiveFps::new(0, 30);
    }

    #[test]
    #[should_panic(expected = "max_fps must be >= min_fps")]
    fn test_invalid_fps_range_panics() {
        AdaptiveFps::new(30, 5);
    }

    #[test]
    fn test_stays_at_max_fps_when_active() {
        let mut fps = AdaptiveFps::new(5, 30);

        for _ in 0..100 {
            let interval = fps.update(true);
            assert_eq!(fps.current_fps(), 30);
            assert_eq!(interval, Duration::from_millis(33)); // ~30 FPS
        }
    }

    #[test]
    fn test_drops_to_min_fps_when_idle() {
        let mut fps = AdaptiveFps::new(5, 30);

        // First frame changes
        fps.update(true);
        assert_eq!(fps.current_fps(), 30);

        // Simulate 91 frames with no changes
        for _ in 0..91 {
            fps.update(false);
        }

        assert_eq!(fps.current_fps(), 5); // Should be at min FPS
        assert!(fps.is_idle());
    }

    #[test]
    fn test_gradual_fps_reduction() {
        let mut fps = AdaptiveFps::new(5, 30);

        fps.update(true); // Start active
        assert_eq!(fps.current_fps(), 30);

        // After 30 frames (1 second), still at max
        for _ in 0..30 {
            fps.update(false);
        }
        assert_eq!(fps.current_fps(), 30);

        // After 31 frames, should start dropping
        fps.update(false);
        assert!(fps.current_fps() < 30);
        assert!(fps.current_fps() > 5);

        // After 91 frames, should be at min
        for _ in 0..60 {
            fps.update(false);
        }
        assert_eq!(fps.current_fps(), 5);
    }

    #[test]
    fn test_immediate_recovery_on_activity() {
        let mut fps = AdaptiveFps::new(5, 30);

        // Drop to min FPS
        fps.update(true);
        for _ in 0..91 {
            fps.update(false);
        }
        assert_eq!(fps.current_fps(), 5);

        // Single active frame should restore max FPS
        fps.update(true);
        assert_eq!(fps.current_fps(), 30);
        assert!(fps.is_active());
    }

    #[test]
    fn test_stats() {
        let mut fps = AdaptiveFps::new(5, 30);

        fps.update(true);
        fps.update(false);
        fps.update(false);

        let stats = fps.stats();
        assert_eq!(stats.total_frames, 3);
        assert_eq!(stats.current_fps, 30);
        assert_eq!(stats.frames_unchanged, 2);
    }

    #[test]
    fn test_fps_changes_tracked() {
        let mut fps = AdaptiveFps::new(5, 30);

        fps.update(true);
        let initial_changes = fps.stats().fps_changes;

        // Force FPS change by going idle
        for _ in 0..91 {
            fps.update(false);
        }

        let final_changes = fps.stats().fps_changes;
        assert!(final_changes > initial_changes);
    }

    #[test]
    fn test_interval_calculation() {
        let mut fps = AdaptiveFps::new(10, 30);

        // At 30 FPS, interval should be ~33ms
        let interval = fps.update(true);
        assert_eq!(interval, Duration::from_millis(33));

        // Drop to 10 FPS
        for _ in 0..91 {
            fps.update(false);
        }

        // At 10 FPS, interval should be 100ms
        let interval = fps.update(false);
        assert_eq!(interval, Duration::from_millis(100));
    }
}
