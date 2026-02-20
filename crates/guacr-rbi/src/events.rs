// RBI event types for display and input
// Ported from KCM's display_fifo.h and input_fifo.h

/// Maximum URL length (matches KCM's MAX_URL_LENGTH)
pub const MAX_URL_LENGTH: usize = 2048;

/// Maximum clipboard length (50MB, matches KCM's MAX_CLIPBOARD_LENGTH)
pub const MAX_CLIPBOARD_LENGTH: usize = 52_428_800;

/// Maximum viewport width/height (matches KCM's MAX_WIDTH/MAX_HEIGHT)
pub const MAX_WIDTH: u32 = 4096;
pub const MAX_HEIGHT: u32 = 4096;

/// Maximum number of URL patterns for whitelisting
pub const MAX_URL_PATTERNS: usize = 10000;

/// Maximum navigation steps (back/forward)
pub const MAX_NAVIGATIONS: i32 = 1000;

/// Display event surface types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplaySurface {
    /// Main browser viewport (always visible, cannot be moved)
    View,
    /// Popup overlay for dropdowns, menus, etc. (can be hidden/moved)
    Popup,
}

/// Display event types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayEventType {
    /// Current frame has finished rendering
    EndOfFrame,
    /// Region of image data has been updated
    Draw,
    /// Surface has been moved and/or resized
    MoveResize,
    /// Surface should now be shown
    Show,
    /// Surface should now be hidden
    Hide,
    /// Mouse cursor should be I-beam (text cursor)
    CursorIBeam,
    /// Mouse cursor should be hidden
    CursorNone,
    /// Mouse cursor should be standard pointer
    CursorPointer,
}

/// Rectangle for display events
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl DisplayRect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Check if rectangles intersect
    pub fn intersects(&self, other: &DisplayRect) -> bool {
        !(self.x + self.width <= other.x
            || other.x + other.width <= self.x
            || self.y + self.height <= other.y
            || other.y + other.height <= self.y)
    }

    /// Merge with another rectangle
    pub fn union(&self, other: &DisplayRect) -> DisplayRect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);

        DisplayRect {
            x,
            y,
            width: x2 - x,
            height: y2 - y,
        }
    }
}

/// Display event from browser
#[derive(Debug, Clone)]
pub struct DisplayEvent {
    pub surface: DisplaySurface,
    pub event_type: DisplayEventType,
    pub rect: DisplayRect,
}

impl DisplayEvent {
    pub fn draw(surface: DisplaySurface, rect: DisplayRect) -> Self {
        Self {
            surface,
            event_type: DisplayEventType::Draw,
            rect,
        }
    }

    pub fn end_of_frame() -> Self {
        Self {
            surface: DisplaySurface::View,
            event_type: DisplayEventType::EndOfFrame,
            rect: DisplayRect::default(),
        }
    }

    pub fn resize(width: i32, height: i32) -> Self {
        Self {
            surface: DisplaySurface::View,
            event_type: DisplayEventType::MoveResize,
            rect: DisplayRect::new(0, 0, width, height),
        }
    }

    pub fn popup_show(rect: DisplayRect) -> Self {
        Self {
            surface: DisplaySurface::Popup,
            event_type: DisplayEventType::Show,
            rect,
        }
    }

    pub fn popup_hide() -> Self {
        Self {
            surface: DisplaySurface::Popup,
            event_type: DisplayEventType::Hide,
            rect: DisplayRect::default(),
        }
    }

    pub fn cursor(cursor_type: DisplayEventType) -> Self {
        Self {
            surface: DisplaySurface::View,
            event_type: cursor_type,
            rect: DisplayRect::default(),
        }
    }
}

/// Input event types
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum InputEventType {
    /// Mouse movement or button press/release
    Mouse,
    /// Keyboard key press/release
    Keyboard,
    /// Touch start/move/end
    Touch,
    /// Browser window resize
    Resize,
    /// Navigate relative to history (back/forward/refresh)
    NavigateHistory,
    /// Navigate to a specific URL
    NavigateUrl,
    /// Clipboard update from client
    Clipboard,
}

/// Mouse event details
#[derive(Debug, Clone)]
pub struct MouseEventDetails {
    pub x: i32,
    pub y: i32,
    pub mask: u32,
}

/// Keyboard event details
#[derive(Debug, Clone)]
pub struct KeyboardEventDetails {
    pub keysym: u32,
    pub pressed: bool,
}

/// Touch event details
#[derive(Debug, Clone)]
pub struct TouchEventDetails {
    pub id: i32,
    pub x: i32,
    pub y: i32,
    pub radius_x: i32,
    pub radius_y: i32,
    pub angle: f64,
    pub force: f64,
}

/// Resize event details
#[derive(Debug, Clone)]
pub struct ResizeEventDetails {
    pub width: i32,
    pub height: i32,
}

/// Navigate history event details
#[derive(Debug, Clone)]
pub struct NavigateHistoryDetails {
    /// Positive = forward, negative = back, 0 = refresh
    pub position: i32,
}

/// Navigate URL event details
#[derive(Debug, Clone)]
pub struct NavigateUrlDetails {
    pub url: String,
}

/// Clipboard event details
#[derive(Debug, Clone)]
pub struct ClipboardDetails {
    pub mimetype: String,
    pub data: Vec<u8>,
}

/// Input event from Guacamole client
#[derive(Debug, Clone)]
pub enum InputEvent {
    Mouse(MouseEventDetails),
    Keyboard(KeyboardEventDetails),
    Touch(TouchEventDetails),
    Resize(ResizeEventDetails),
    NavigateHistory(NavigateHistoryDetails),
    NavigateUrl(NavigateUrlDetails),
    Clipboard(ClipboardDetails),
}

impl InputEvent {
    pub fn mouse(x: i32, y: i32, mask: u32) -> Self {
        InputEvent::Mouse(MouseEventDetails { x, y, mask })
    }

    pub fn keyboard(keysym: u32, pressed: bool) -> Self {
        InputEvent::Keyboard(KeyboardEventDetails { keysym, pressed })
    }

    pub fn touch(
        id: i32,
        x: i32,
        y: i32,
        radius_x: i32,
        radius_y: i32,
        angle: f64,
        force: f64,
    ) -> Self {
        InputEvent::Touch(TouchEventDetails {
            id,
            x,
            y,
            radius_x,
            radius_y,
            angle,
            force,
        })
    }

    pub fn resize(width: i32, height: i32) -> Self {
        InputEvent::Resize(ResizeEventDetails { width, height })
    }

    pub fn navigate_back() -> Self {
        InputEvent::NavigateHistory(NavigateHistoryDetails { position: -1 })
    }

    pub fn navigate_forward() -> Self {
        InputEvent::NavigateHistory(NavigateHistoryDetails { position: 1 })
    }

    pub fn navigate_refresh() -> Self {
        InputEvent::NavigateHistory(NavigateHistoryDetails { position: 0 })
    }

    pub fn navigate_url(url: String) -> Self {
        InputEvent::NavigateUrl(NavigateUrlDetails { url })
    }

    pub fn clipboard(mimetype: String, data: Vec<u8>) -> Self {
        InputEvent::Clipboard(ClipboardDetails { mimetype, data })
    }
}

/// Browser state flags (matches KCM's GUAC_BROWSER_STATE_* flags)
#[derive(Debug, Clone, Copy, Default)]
pub struct BrowserState {
    pub terminating: bool,
    pub title_updated: bool,
    pub url_updated: bool,
    pub clipboard_updated: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

impl BrowserState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// URL pattern for whitelisting
#[derive(Debug, Clone)]
pub struct UrlPattern {
    pub scheme: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: Option<String>,
}

impl UrlPattern {
    /// Parse URL pattern from string
    pub fn parse(pattern: &str) -> Option<Self> {
        // Simple pattern parsing - can be enhanced with regex
        let mut scheme = None;
        let mut rest = pattern;

        if let Some(idx) = pattern.find("://") {
            scheme = Some(pattern[..idx].to_string());
            rest = &pattern[idx + 3..];
        }

        // Extract host and optional port
        let (host_port, path) = if let Some(idx) = rest.find('/') {
            (&rest[..idx], Some(rest[idx..].to_string()))
        } else {
            (rest, None)
        };

        let (host, port) = if let Some(idx) = host_port.rfind(':') {
            let h = &host_port[..idx];
            let p = host_port[idx + 1..].parse::<u16>().ok();
            (h.to_string(), p)
        } else {
            (host_port.to_string(), None)
        };

        Some(UrlPattern {
            scheme,
            host,
            port,
            path,
        })
    }

    /// Check if URL matches this pattern
    pub fn matches(&self, url: &str) -> bool {
        // Simple matching - can be enhanced
        if let Some(ref scheme) = self.scheme {
            if !url.starts_with(&format!("{}://", scheme)) {
                return false;
            }
        }

        // Check if host matches (allowing wildcards)
        if self.host.starts_with('*') {
            let suffix = &self.host[1..];
            url.contains(suffix)
        } else {
            url.contains(&self.host)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_rect_intersects() {
        let rect1 = DisplayRect::new(10, 10, 20, 20);
        let rect2 = DisplayRect::new(20, 20, 20, 20);
        let rect3 = DisplayRect::new(50, 50, 10, 10);

        assert!(rect1.intersects(&rect2));
        assert!(!rect1.intersects(&rect3));
    }

    #[test]
    fn test_display_rect_union() {
        let rect1 = DisplayRect::new(10, 10, 20, 20);
        let rect2 = DisplayRect::new(20, 20, 20, 20);

        let union = rect1.union(&rect2);
        assert_eq!(union.x, 10);
        assert_eq!(union.y, 10);
        assert_eq!(union.width, 30);
        assert_eq!(union.height, 30);
    }

    #[test]
    fn test_url_pattern_parse() {
        let pattern = UrlPattern::parse("https://example.com:443/path").unwrap();
        assert_eq!(pattern.scheme, Some("https".to_string()));
        assert_eq!(pattern.host, "example.com");
        assert_eq!(pattern.port, Some(443));
        assert_eq!(pattern.path, Some("/path".to_string()));
    }

    #[test]
    fn test_url_pattern_matches() {
        let pattern = UrlPattern::parse("*.google.com").unwrap();
        assert!(pattern.matches("https://www.google.com/search"));
        assert!(pattern.matches("http://mail.google.com"));
        assert!(!pattern.matches("https://example.com"));
    }

    #[test]
    fn test_input_events() {
        let event = InputEvent::navigate_back();
        if let InputEvent::NavigateHistory(details) = event {
            assert_eq!(details.position, -1);
        } else {
            panic!("Expected NavigateHistory event");
        }
    }
}
