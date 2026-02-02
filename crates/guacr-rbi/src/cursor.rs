// Cursor handling for RBI
//
// Tracks cursor style changes and sends custom cursors to client.
// Uses JavaScript to monitor CSS cursor property changes.

use log::{debug, info};

/// JavaScript to track cursor style
pub const CURSOR_TRACKER_JS: &str = r#"
(function() {
    if (window.__guacCursorTracker) return;
    
    window.__guacCursor = {
        style: 'default',
        x: 0,
        y: 0,
        timestamp: 0
    };
    
    // Track cursor style changes
    document.addEventListener('mouseover', function(e) {
        const style = window.getComputedStyle(e.target).cursor;
        if (style !== window.__guacCursor.style) {
            window.__guacCursor = {
                style: style,
                x: e.clientX,
                y: e.clientY,
                timestamp: Date.now()
            };
        }
    }, true);
    
    // Track mouse position
    document.addEventListener('mousemove', function(e) {
        window.__guacCursor.x = e.clientX;
        window.__guacCursor.y = e.clientY;
    }, true);
    
    window.__guacCursorTracker = true;
})()
"#;

/// JavaScript to get current cursor state
pub const GET_CURSOR_JS: &str = r#"
(function() {
    return window.__guacCursor || { style: 'default', x: 0, y: 0, timestamp: 0 };
})()
"#;

/// Standard cursor types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorType {
    Default,
    Pointer,
    Text,
    Wait,
    Progress,
    NotAllowed,
    Help,
    Crosshair,
    Move,
    ColResize,
    RowResize,
    NResize,
    SResize,
    EResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    Grab,
    Grabbing,
    ZoomIn,
    ZoomOut,
    Custom,
    None,
}

impl CursorType {
    /// Parse cursor style string from CSS
    pub fn from_css(style: &str) -> Self {
        match style.to_lowercase().as_str() {
            "default" | "auto" => Self::Default,
            "pointer" => Self::Pointer,
            "text" | "vertical-text" => Self::Text,
            "wait" => Self::Wait,
            "progress" => Self::Progress,
            "not-allowed" | "no-drop" => Self::NotAllowed,
            "help" => Self::Help,
            "crosshair" => Self::Crosshair,
            "move" | "all-scroll" => Self::Move,
            "col-resize" | "ew-resize" => Self::ColResize,
            "row-resize" | "ns-resize" => Self::RowResize,
            "n-resize" => Self::NResize,
            "s-resize" => Self::SResize,
            "e-resize" => Self::EResize,
            "w-resize" => Self::WResize,
            "ne-resize" => Self::NeResize,
            "nw-resize" => Self::NwResize,
            "se-resize" => Self::SeResize,
            "sw-resize" => Self::SwResize,
            "grab" => Self::Grab,
            "grabbing" => Self::Grabbing,
            "zoom-in" => Self::ZoomIn,
            "zoom-out" => Self::ZoomOut,
            "none" => Self::None,
            _ if style.starts_with("url(") => Self::Custom,
            _ => Self::Default,
        }
    }

    /// Get Guacamole cursor instruction opcode
    ///
    /// Returns the built-in cursor name for Guacamole protocol
    pub fn to_guac_cursor(&self) -> Option<&'static str> {
        match self {
            Self::Default => Some("0"), // Default cursor (layer 0)
            Self::Pointer => Some("pointer"),
            Self::Text => Some("text"),
            Self::Wait => Some("wait"),
            Self::Progress => Some("progress"),
            Self::NotAllowed => Some("not-allowed"),
            Self::Help => Some("help"),
            Self::Crosshair => Some("crosshair"),
            Self::Move => Some("move"),
            Self::ColResize => Some("col-resize"),
            Self::RowResize => Some("row-resize"),
            Self::NResize | Self::SResize => Some("ns-resize"),
            Self::EResize | Self::WResize => Some("ew-resize"),
            Self::NeResize | Self::SwResize => Some("nesw-resize"),
            Self::NwResize | Self::SeResize => Some("nwse-resize"),
            Self::Grab => Some("grab"),
            Self::Grabbing => Some("grabbing"),
            Self::ZoomIn => Some("zoom-in"),
            Self::ZoomOut => Some("zoom-out"),
            Self::None => Some("none"),
            Self::Custom => None, // Custom cursors need special handling
        }
    }
}

/// Cursor state tracker
#[derive(Debug, Default)]
pub struct CursorState {
    current_type: Option<CursorType>,
    last_timestamp: u64,
    hotspot_x: i32,
    hotspot_y: i32,
}

impl CursorState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if cursor has changed
    pub fn update(&mut self, style: &str, timestamp: u64) -> Option<CursorType> {
        if timestamp <= self.last_timestamp {
            return None;
        }

        let new_type = CursorType::from_css(style);

        if self.current_type != Some(new_type) {
            self.current_type = Some(new_type);
            self.last_timestamp = timestamp;
            info!("RBI: Cursor changed to {:?}", new_type);
            Some(new_type)
        } else {
            None
        }
    }

    /// Get current cursor type
    pub fn current(&self) -> Option<CursorType> {
        self.current_type
    }

    /// Set hotspot for custom cursors
    pub fn set_hotspot(&mut self, x: i32, y: i32) {
        self.hotspot_x = x;
        self.hotspot_y = y;
    }

    /// Get hotspot coordinates
    pub fn hotspot(&self) -> (i32, i32) {
        (self.hotspot_x, self.hotspot_y)
    }
}

/// Format Guacamole cursor instruction
///
/// The cursor instruction sets the client-side cursor:
/// `cursor,<hotspot-x>,<hotspot-y>,<src-layer>,<src-x>,<src-y>,<width>,<height>;`
pub fn format_cursor_instruction(
    hotspot_x: i32,
    hotspot_y: i32,
    layer: i32,
    src_x: i32,
    src_y: i32,
    width: i32,
    height: i32,
) -> String {
    format!(
        "6.cursor,{}.{},{}.{},{}.{},{}.{},{}.{},{}.{},{}.{};",
        hotspot_x.to_string().len(),
        hotspot_x,
        hotspot_y.to_string().len(),
        hotspot_y,
        layer.to_string().len(),
        layer,
        src_x.to_string().len(),
        src_x,
        src_y.to_string().len(),
        src_y,
        width.to_string().len(),
        width,
        height.to_string().len(),
        height,
    )
}

/// Format standard cursor using Guacamole's built-in cursor names
///
/// Guacamole clients have built-in cursors that can be referenced by name.
/// Format: cursor,<hotspot-x>,<hotspot-y>,<cursor-name>,<src-x>,<src-y>,<width>,<height>;
pub fn format_standard_cursor(cursor_type: CursorType) -> Option<String> {
    let cursor_name = match cursor_type {
        CursorType::Default | CursorType::Pointer => "pointer",
        CursorType::Text => "text",
        CursorType::Wait => "wait",
        CursorType::Progress => "progress",
        CursorType::NotAllowed => "not-allowed",
        CursorType::Help => "help",
        CursorType::Crosshair => "crosshair",
        CursorType::Move => "move",
        CursorType::ColResize => "col-resize",
        CursorType::RowResize => "row-resize",
        CursorType::None => "none",
        _ => {
            // For other cursors, fall back to pointer
            debug!("RBI: Cursor {:?} falling back to pointer", cursor_type);
            "pointer"
        }
    };

    // Format: cursor,<hotspot-x>,<hotspot-y>,<cursor-name>,<src-x>,<src-y>,<width>,<height>;
    // All values are 0 for standard cursors (client renders them)
    Some(format!(
        "6.cursor,1.0,1.0,{}.{},1.0,1.0,1.0,1.0;",
        cursor_name.len(),
        cursor_name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_type_from_css() {
        assert_eq!(CursorType::from_css("pointer"), CursorType::Pointer);
        assert_eq!(CursorType::from_css("text"), CursorType::Text);
        assert_eq!(CursorType::from_css("wait"), CursorType::Wait);
        assert_eq!(CursorType::from_css("default"), CursorType::Default);
        assert_eq!(CursorType::from_css("auto"), CursorType::Default);
        assert_eq!(CursorType::from_css("url(cursor.png)"), CursorType::Custom);
        assert_eq!(CursorType::from_css("unknown"), CursorType::Default);
    }

    #[test]
    fn test_cursor_state() {
        let mut state = CursorState::new();

        // First update
        let result = state.update("pointer", 1);
        assert_eq!(result, Some(CursorType::Pointer));

        // Same cursor, higher timestamp - no change
        let result = state.update("pointer", 2);
        assert_eq!(result, None);

        // Different cursor
        let result = state.update("text", 3);
        assert_eq!(result, Some(CursorType::Text));

        // Old timestamp - ignored
        let result = state.update("wait", 1);
        assert_eq!(result, None);
    }

    #[test]
    fn test_format_cursor_instruction() {
        let instr = format_cursor_instruction(0, 0, 0, 0, 0, 16, 16);
        assert!(instr.starts_with("6.cursor,"));
        assert!(instr.contains("16"));
    }
}
