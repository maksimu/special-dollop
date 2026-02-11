//! Drag detection for graphical protocol handlers
//!
//! Detects window drag operations based on mouse button state + movement + rapid
//! full-screen updates. Protocol-agnostic -- works for RDP, VNC, or any graphical
//! handler that receives mouse events and graphics updates.
//!
//! ## State Machine
//!
//! ```text
//! Idle -> Tracking: Mouse button pressed + mouse moves >5px
//! Tracking -> Dragging: 3+ large updates (>80% of screen) within 500ms while button held
//! Dragging -> Idle: Mouse button released (triggers final full render)
//! Tracking -> Idle: No large updates within 500ms, or button released
//! ```
//!
//! ## Usage
//!
//! ```
//! use guacr_handlers::DragDetector;
//!
//! let mut detector = DragDetector::new(1920, 1080);
//!
//! // Feed mouse events (from Guacamole protocol)
//! detector.notify_mouse_event(100, 100, 1); // left button down
//! detector.notify_mouse_event(120, 120, 1); // moved while held
//!
//! // Feed graphics updates
//! detector.notify_graphics_update(1920, 1080); // full-screen update
//! detector.notify_graphics_update(1920, 1080); // another one
//! detector.notify_graphics_update(1920, 1080); // third one
//!
//! if detector.is_dragging() {
//!     let (dx, dy) = detector.drag_delta();
//!     // Send copy instruction shifting screen by (dx, dy)
//! }
//! ```

use std::collections::VecDeque;
use std::time::Instant;

/// Minimum mouse displacement (in pixels) before we start tracking a potential drag
const DRAG_THRESHOLD_PX: i32 = 5;

/// Number of large updates required within the time window to confirm a drag
const REQUIRED_LARGE_UPDATES: usize = 3;

/// Time window for detecting rapid full-screen updates (milliseconds)
const UPDATE_WINDOW_MS: u128 = 500;

/// Minimum update coverage (as percentage of screen) to count as a "large" update
const LARGE_UPDATE_COVERAGE_PCT: u32 = 80;

/// Maximum number of update timestamps to keep in history
const MAX_UPDATE_HISTORY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragState {
    /// No drag activity
    Idle,
    /// Mouse button held with movement, watching for rapid updates
    Tracking,
    /// Confirmed drag in progress
    Dragging,
}

/// Detects drag operations (window moves, element drags) based on
/// mouse button state + movement + rapid full-screen updates.
///
/// Protocol-agnostic -- works for RDP, VNC, or any graphical handler.
pub struct DragDetector {
    state: DragState,
    /// Whether a mouse button is currently held down
    button_down: bool,
    /// Current mouse position
    last_mouse_x: i32,
    last_mouse_y: i32,
    /// Position when mouse button was first pressed
    mouse_down_x: i32,
    mouse_down_y: i32,
    /// Previous mouse position (for per-frame delta)
    prev_mouse_x: i32,
    prev_mouse_y: i32,
    /// Timestamps of recent large (>80% screen) graphics updates
    update_timestamps: VecDeque<Instant>,
    /// Screen dimensions
    screen_width: u32,
    screen_height: u32,
    /// Set to true on the frame where button is released after dragging
    just_ended: bool,
}

impl DragDetector {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        Self {
            state: DragState::Idle,
            button_down: false,
            last_mouse_x: 0,
            last_mouse_y: 0,
            mouse_down_x: 0,
            mouse_down_y: 0,
            prev_mouse_x: 0,
            prev_mouse_y: 0,
            update_timestamps: VecDeque::with_capacity(MAX_UPDATE_HISTORY),
            screen_width,
            screen_height,
            just_ended: false,
        }
    }

    /// Notify the detector of a mouse event.
    ///
    /// `button_mask` follows the Guacamole protocol convention:
    /// - bit 0: left button
    /// - bit 1: middle button
    /// - bit 2: right button
    /// - bits 3-4: scroll wheel
    pub fn notify_mouse_event(&mut self, x: i32, y: i32, button_mask: u8) {
        // Clear the "just ended" flag on any new mouse event
        self.just_ended = false;

        let was_down = self.button_down;
        // Any of the three main buttons held = potential drag
        let is_down = (button_mask & 0x07) != 0;

        self.prev_mouse_x = self.last_mouse_x;
        self.prev_mouse_y = self.last_mouse_y;
        self.last_mouse_x = x;
        self.last_mouse_y = y;

        if is_down && !was_down {
            // Button just pressed
            self.button_down = true;
            self.mouse_down_x = x;
            self.mouse_down_y = y;
            self.prev_mouse_x = x;
            self.prev_mouse_y = y;
            // Don't change state yet -- wait for movement
        } else if !is_down && was_down {
            // Button just released
            self.button_down = false;
            if self.state == DragState::Dragging {
                self.just_ended = true;
                log::debug!("Drag ended at ({}, {})", x, y);
            }
            self.state = DragState::Idle;
            self.update_timestamps.clear();
        } else if is_down {
            // Button held, mouse moved -- check for drag threshold
            self.button_down = true;
            if self.state == DragState::Idle {
                let dx = (x - self.mouse_down_x).abs();
                let dy = (y - self.mouse_down_y).abs();
                if dx > DRAG_THRESHOLD_PX || dy > DRAG_THRESHOLD_PX {
                    self.state = DragState::Tracking;
                    log::trace!(
                        "Drag tracking started: displacement ({}, {}) from ({}, {})",
                        dx,
                        dy,
                        self.mouse_down_x,
                        self.mouse_down_y
                    );
                }
            }
        }
    }

    /// Notify the detector of a graphics update with the given dimensions.
    ///
    /// Call this for each graphics update received from the remote desktop.
    /// The detector uses update frequency and size to confirm drag operations.
    pub fn notify_graphics_update(&mut self, update_width: u32, update_height: u32) {
        if !self.button_down {
            return;
        }

        // Check if this is a "large" update (>80% of screen area)
        let screen_pixels = self.screen_width as u64 * self.screen_height as u64;
        if screen_pixels == 0 {
            return;
        }
        let update_pixels = update_width as u64 * update_height as u64;
        let coverage = ((update_pixels * 100) / screen_pixels) as u32;

        if coverage < LARGE_UPDATE_COVERAGE_PCT {
            return;
        }

        let now = Instant::now();

        // Prune old timestamps outside the window
        while let Some(front) = self.update_timestamps.front() {
            if now.duration_since(*front).as_millis() > UPDATE_WINDOW_MS {
                self.update_timestamps.pop_front();
            } else {
                break;
            }
        }

        self.update_timestamps.push_back(now);

        // Cap history size
        while self.update_timestamps.len() > MAX_UPDATE_HISTORY {
            self.update_timestamps.pop_front();
        }

        // Transition Tracking -> Dragging if enough rapid large updates
        if self.state == DragState::Tracking
            && self.update_timestamps.len() >= REQUIRED_LARGE_UPDATES
        {
            self.state = DragState::Dragging;
            log::debug!(
                "Drag detected: {} large updates in {}ms, mouse at ({}, {})",
                self.update_timestamps.len(),
                self.update_timestamps
                    .back()
                    .unwrap()
                    .duration_since(*self.update_timestamps.front().unwrap())
                    .as_millis(),
                self.last_mouse_x,
                self.last_mouse_y
            );
        }
    }

    /// Returns true if a drag operation is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.state == DragState::Dragging
    }

    /// Returns the mouse displacement since the last mouse event.
    ///
    /// This gives per-frame delta suitable for copy instruction offsets.
    pub fn drag_delta(&self) -> (i32, i32) {
        (
            self.last_mouse_x - self.prev_mouse_x,
            self.last_mouse_y - self.prev_mouse_y,
        )
    }

    /// Returns true on the frame where a drag operation just ended (button released).
    ///
    /// The handler should force a full re-render on this frame to clean up
    /// any copy artifacts from the drag.
    pub fn drag_ended(&self) -> bool {
        self.just_ended
    }

    /// Update screen dimensions (call on resize).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.screen_width = width;
        self.screen_height = height;
        self.state = DragState::Idle;
        self.update_timestamps.clear();
    }

    /// Reset all state.
    pub fn reset(&mut self) {
        self.state = DragState::Idle;
        self.button_down = false;
        self.update_timestamps.clear();
        self.just_ended = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_drag_without_mouse() {
        let mut d = DragDetector::new(1920, 1080);

        // Graphics updates without mouse should not trigger drag
        for _ in 0..10 {
            d.notify_graphics_update(1920, 1080);
        }
        assert!(!d.is_dragging());
    }

    #[test]
    fn test_no_drag_without_movement() {
        let mut d = DragDetector::new(1920, 1080);

        // Button down but no movement
        d.notify_mouse_event(100, 100, 1);
        for _ in 0..10 {
            d.notify_graphics_update(1920, 1080);
        }
        assert!(!d.is_dragging());
    }

    #[test]
    fn test_drag_detected() {
        let mut d = DragDetector::new(1920, 1080);

        // Press button
        d.notify_mouse_event(100, 100, 1);
        // Move past threshold
        d.notify_mouse_event(120, 120, 1);
        assert!(!d.is_dragging()); // Not yet -- need updates

        // Rapid large updates
        d.notify_graphics_update(1920, 1080);
        d.notify_graphics_update(1920, 1080);
        assert!(!d.is_dragging()); // Only 2

        d.notify_graphics_update(1920, 1080);
        assert!(d.is_dragging()); // 3rd update triggers
    }

    #[test]
    fn test_drag_ends_on_release() {
        let mut d = DragDetector::new(1920, 1080);

        // Start drag
        d.notify_mouse_event(100, 100, 1);
        d.notify_mouse_event(120, 120, 1);
        d.notify_graphics_update(1920, 1080);
        d.notify_graphics_update(1920, 1080);
        d.notify_graphics_update(1920, 1080);
        assert!(d.is_dragging());

        // Release
        d.notify_mouse_event(150, 150, 0);
        assert!(!d.is_dragging());
        assert!(d.drag_ended());
    }

    #[test]
    fn test_small_updates_dont_trigger() {
        let mut d = DragDetector::new(1920, 1080);

        d.notify_mouse_event(100, 100, 1);
        d.notify_mouse_event(120, 120, 1);

        // Small updates (100x100 is <1% of 1920x1080)
        for _ in 0..10 {
            d.notify_graphics_update(100, 100);
        }
        assert!(!d.is_dragging());
    }

    #[test]
    fn test_drag_delta() {
        let mut d = DragDetector::new(1920, 1080);

        d.notify_mouse_event(100, 200, 1);
        d.notify_mouse_event(115, 210, 1);

        assert_eq!(d.drag_delta(), (15, 10));
    }

    #[test]
    fn test_resize_resets_state() {
        let mut d = DragDetector::new(1920, 1080);

        // Start drag
        d.notify_mouse_event(100, 100, 1);
        d.notify_mouse_event(120, 120, 1);
        d.notify_graphics_update(1920, 1080);
        d.notify_graphics_update(1920, 1080);
        d.notify_graphics_update(1920, 1080);
        assert!(d.is_dragging());

        // Resize resets
        d.resize(2560, 1440);
        assert!(!d.is_dragging());
    }
}
