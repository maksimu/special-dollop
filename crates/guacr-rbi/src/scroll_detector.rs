// Scroll Detection for RBI
//
// Detects scroll events via JavaScript and sends scroll instructions
// instead of full frames, improving UX and reducing bandwidth.

use log::{debug, info};
use tokio::time::Instant;

/// JavaScript to install scroll event listener
pub const SCROLL_TRACKER_JS: &str = r#"
(function() {
    if (window.__guacr_scroll_installed) return;
    window.__guacr_scroll_installed = true;
    
    window.__guacr_scroll_x = window.scrollX || 0;
    window.__guacr_scroll_y = window.scrollY || 0;
    window.__guacr_scroll_changed = false;
    
    const updateScroll = () => {
        const newX = window.scrollX || 0;
        const newY = window.scrollY || 0;
        
        if (newX !== window.__guacr_scroll_x || newY !== window.__guacr_scroll_y) {
            window.__guacr_scroll_changed = true;
            window.__guacr_scroll_x = newX;
            window.__guacr_scroll_y = newY;
        }
    };
    
    // Listen to scroll events
    window.addEventListener('scroll', updateScroll, { passive: true });
    
    // Also check on resize (can cause scroll position changes)
    window.addEventListener('resize', updateScroll, { passive: true });
    
    console.log('[guacr] Scroll tracker installed');
})();
"#;

/// JavaScript to poll for scroll changes
pub const GET_SCROLL_DATA_JS: &str = r#"
(function() {
    if (!window.__guacr_scroll_installed) return null;
    
    if (window.__guacr_scroll_changed) {
        window.__guacr_scroll_changed = false;
        return {
            x: window.__guacr_scroll_x,
            y: window.__guacr_scroll_y,
            maxX: document.documentElement.scrollWidth - window.innerWidth,
            maxY: document.documentElement.scrollHeight - window.innerHeight
        };
    }
    
    return null;
})();
"#;

/// Scroll position data
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollPosition {
    pub x: i32,
    pub y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl ScrollPosition {
    pub fn new(x: i32, y: i32, max_x: i32, max_y: i32) -> Self {
        Self { x, y, max_x, max_y }
    }

    /// Calculate delta from another position
    pub fn delta_from(&self, other: &ScrollPosition) -> (i32, i32) {
        (self.x - other.x, self.y - other.y)
    }

    /// Check if scroll position is at top
    pub fn is_at_top(&self) -> bool {
        self.y == 0
    }

    /// Check if scroll position is at bottom
    pub fn is_at_bottom(&self) -> bool {
        self.y >= self.max_y
    }

    /// Check if scroll position is at left edge
    pub fn is_at_left(&self) -> bool {
        self.x == 0
    }

    /// Check if scroll position is at right edge
    pub fn is_at_right(&self) -> bool {
        self.x >= self.max_x
    }
}

/// Scroll detector state
pub struct ScrollDetector {
    last_position: Option<ScrollPosition>,
    last_scroll_time: Option<Instant>,
    scroll_velocity: f32, // pixels per second
    scroll_events: u64,
    total_distance_x: i32,
    total_distance_y: i32,
}

impl ScrollDetector {
    pub fn new() -> Self {
        Self {
            last_position: None,
            last_scroll_time: None,
            scroll_velocity: 0.0,
            scroll_events: 0,
            total_distance_x: 0,
            total_distance_y: 0,
        }
    }

    /// Update scroll position and return delta if changed
    ///
    /// Returns Some((delta_x, delta_y)) if position changed, None otherwise
    pub fn update(&mut self, position: ScrollPosition) -> Option<(i32, i32)> {
        let now = Instant::now();
        
        if let Some(last) = self.last_position {
            let (delta_x, delta_y) = position.delta_from(&last);

            if delta_x != 0 || delta_y != 0 {
                self.scroll_events += 1;
                self.total_distance_x += delta_x.abs();
                self.total_distance_y += delta_y.abs();

                // Calculate scroll velocity (pixels per second)
                if let Some(last_time) = self.last_scroll_time {
                    let elapsed = last_time.elapsed().as_secs_f32();
                    if elapsed > 0.0 {
                        let distance = ((delta_x * delta_x + delta_y * delta_y) as f32).sqrt();
                        self.scroll_velocity = distance / elapsed;
                    }
                }
                self.last_scroll_time = Some(now);

                debug!(
                    "Scroll detected: delta=({}, {}), position=({}, {}), velocity={:.0}px/s",
                    delta_x, delta_y, position.x, position.y, self.scroll_velocity
                );

                self.last_position = Some(position);
                return Some((delta_x, delta_y));
            }
        } else {
            // First position
            self.last_position = Some(position);
            self.last_scroll_time = Some(now);
            info!(
                "Scroll tracking initialized at ({}, {})",
                position.x, position.y
            );
        }

        None
    }

    /// Get statistics
    pub fn stats(&self) -> ScrollStats {
        ScrollStats {
            scroll_events: self.scroll_events,
            total_distance_x: self.total_distance_x,
            total_distance_y: self.total_distance_y,
        }
    }

    /// Reset statistics
    pub fn reset_stats(&mut self) {
        self.scroll_events = 0;
        self.total_distance_x = 0;
        self.total_distance_y = 0;
    }

    /// Get current position
    pub fn current_position(&self) -> Option<ScrollPosition> {
        self.last_position
    }

    /// Check if currently scrolling (scrolled within last 2 seconds)
    pub fn is_scrolling(&self) -> bool {
        self.last_scroll_time
            .map(|t| t.elapsed().as_secs() < 2)
            .unwrap_or(false)
    }

    /// Check if scrolling fast (> 500 pixels/second)
    pub fn is_fast_scrolling(&self) -> bool {
        self.scroll_velocity > 500.0
    }

    /// Check if scroll is significant (> 5% of viewport)
    pub fn is_significant_scroll(&self, delta_y: i32, viewport_height: i32) -> bool {
        if viewport_height == 0 {
            return false;
        }
        let scroll_percent = (delta_y.abs() * 100) / viewport_height;
        scroll_percent > 5
    }

    /// Check if this is a page scroll (> 80% of viewport)
    pub fn is_page_scroll(&self, delta_y: i32, viewport_height: i32) -> bool {
        if viewport_height == 0 {
            return false;
        }
        let scroll_percent = (delta_y.abs() * 100) / viewport_height;
        scroll_percent > 80
    }

    /// Get current scroll velocity in pixels per second
    pub fn velocity(&self) -> f32 {
        self.scroll_velocity
    }
}

impl Default for ScrollDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Scroll statistics
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollStats {
    pub scroll_events: u64,
    pub total_distance_x: i32,
    pub total_distance_y: i32,
}

impl ScrollStats {
    pub fn avg_distance_per_scroll(&self) -> (f32, f32) {
        if self.scroll_events == 0 {
            return (0.0, 0.0);
        }
        (
            self.total_distance_x as f32 / self.scroll_events as f32,
            self.total_distance_y as f32 / self.scroll_events as f32,
        )
    }
}

/// Format scroll instruction for Guacamole protocol
///
/// Note: Guacamole doesn't have a native scroll instruction,
/// so we use mouse events to simulate scrolling
pub fn format_scroll_instruction(layer: u32, delta_x: i32, delta_y: i32) -> String {
    // Use mouse instruction with scroll wheel
    // Format: mouse,<x>,<y>,<button_mask>;
    // For scroll: button_mask bit 3 (value 8) = scroll up, bit 4 (value 16) = scroll down

    if delta_y < 0 {
        // Scroll up
        format!("5.mouse,{},{},{};", layer, 0, 8)
    } else if delta_y > 0 {
        // Scroll down
        format!("5.mouse,{},{},{};", layer, 0, 16)
    } else if delta_x < 0 {
        // Scroll left (less common)
        format!("5.mouse,{},{},{};", layer, 0, 32)
    } else if delta_x > 0 {
        // Scroll right (less common)
        format!("5.mouse,{},{},{};", layer, 0, 64)
    } else {
        // No scroll
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scroll_position() {
        let pos = ScrollPosition::new(100, 200, 1000, 2000);
        assert_eq!(pos.x, 100);
        assert_eq!(pos.y, 200);
        assert!(!pos.is_at_top());
        assert!(!pos.is_at_bottom());
        assert!(!pos.is_at_left());
        assert!(!pos.is_at_right());
    }

    #[test]
    fn test_scroll_position_edges() {
        let pos = ScrollPosition::new(0, 0, 1000, 2000);
        assert!(pos.is_at_top());
        assert!(pos.is_at_left());

        let pos = ScrollPosition::new(1000, 2000, 1000, 2000);
        assert!(pos.is_at_bottom());
        assert!(pos.is_at_right());
    }

    #[test]
    fn test_scroll_delta() {
        let pos1 = ScrollPosition::new(100, 200, 1000, 2000);
        let pos2 = ScrollPosition::new(150, 250, 1000, 2000);

        let (dx, dy) = pos2.delta_from(&pos1);
        assert_eq!(dx, 50);
        assert_eq!(dy, 50);
    }

    #[test]
    fn test_scroll_detector() {
        let mut detector = ScrollDetector::new();

        // First update - no delta
        let pos1 = ScrollPosition::new(0, 0, 1000, 2000);
        assert_eq!(detector.update(pos1), None);

        // Second update - has delta
        let pos2 = ScrollPosition::new(0, 100, 1000, 2000);
        assert_eq!(detector.update(pos2), Some((0, 100)));

        // Third update - same position, no delta
        assert_eq!(detector.update(pos2), None);

        // Fourth update - different position
        let pos3 = ScrollPosition::new(50, 150, 1000, 2000);
        assert_eq!(detector.update(pos3), Some((50, 50)));

        let stats = detector.stats();
        assert_eq!(stats.scroll_events, 2);
        assert_eq!(stats.total_distance_x, 50);
        assert_eq!(stats.total_distance_y, 150); // 100 + 50
    }

    #[test]
    fn test_scroll_stats() {
        let stats = ScrollStats {
            scroll_events: 4,
            total_distance_x: 200,
            total_distance_y: 400,
        };

        let (avg_x, avg_y) = stats.avg_distance_per_scroll();
        assert_eq!(avg_x, 50.0);
        assert_eq!(avg_y, 100.0);
    }

    #[test]
    fn test_format_scroll_instruction() {
        let instr = format_scroll_instruction(0, 0, -100);
        assert!(instr.contains("8")); // Scroll up

        let instr = format_scroll_instruction(0, 0, 100);
        assert!(instr.contains("16")); // Scroll down

        let instr = format_scroll_instruction(0, 0, 0);
        assert!(instr.is_empty()); // No scroll
    }

    #[test]
    fn test_scroll_velocity() {
        use std::thread;
        use std::time::Duration;
        
        let mut detector = ScrollDetector::new();
        
        // First position
        let pos1 = ScrollPosition::new(0, 0, 1000, 2000);
        detector.update(pos1);
        
        // Wait a bit
        thread::sleep(Duration::from_millis(100));
        
        // Second position (scrolled 100 pixels)
        let pos2 = ScrollPosition::new(0, 100, 1000, 2000);
        detector.update(pos2);
        
        // Velocity should be ~1000 px/s (100 pixels in 0.1 seconds)
        let velocity = detector.velocity();
        assert!(velocity > 500.0 && velocity < 1500.0);
    }

    #[test]
    fn test_is_scrolling() {
        let mut detector = ScrollDetector::new();
        
        // Not scrolling initially
        assert!(!detector.is_scrolling());
        
        // Scroll
        let pos1 = ScrollPosition::new(0, 0, 1000, 2000);
        detector.update(pos1);
        let pos2 = ScrollPosition::new(0, 100, 1000, 2000);
        detector.update(pos2);
        
        // Should be scrolling now
        assert!(detector.is_scrolling());
    }

    #[test]
    fn test_scroll_heuristics() {
        let detector = ScrollDetector::new();
        
        // Small scroll (5 pixels in 1000px viewport = 0.5%)
        assert!(!detector.is_significant_scroll(5, 1000));
        
        // Significant scroll (60 pixels in 1000px viewport = 6%)
        assert!(detector.is_significant_scroll(60, 1000));
        
        // Page scroll (850 pixels in 1000px viewport = 85%)
        assert!(detector.is_page_scroll(850, 1000));
        
        // Not a page scroll (500 pixels in 1000px viewport = 50%)
        assert!(!detector.is_page_scroll(500, 1000));
    }
}
