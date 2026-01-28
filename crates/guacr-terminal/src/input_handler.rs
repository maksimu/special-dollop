// RDP input handling (keyboard and mouse)
// Converts Guacamole input instructions to RDP input events

use log::{debug, warn};

/// RDP input handler
///
/// Converts Guacamole input instructions to RDP protocol input events.
/// This will be wired to ironrdp's input API once the connection is established.
pub struct RdpInputHandler {
    // Track previous button states to detect transitions
    prev_left: bool,
    prev_middle: bool,
    prev_right: bool,
}

impl RdpInputHandler {
    pub fn new() -> Self {
        Self {
            prev_left: false,
            prev_middle: false,
            prev_right: false,
        }
    }

    /// Handle keyboard input from Guacamole client
    ///
    /// # Arguments
    ///
    /// * `keysym` - X11 keysym (key symbol)
    /// * `pressed` - Whether key is pressed (true) or released (false)
    ///
    /// # Returns
    ///
    /// RDP scancode and flags for sending to RDP server
    pub fn handle_keyboard(&self, keysym: u32, pressed: bool) -> Result<RdpKeyEvent, String> {
        // Convert X11 keysym to RDP scancode
        let scancode = self.keysym_to_scancode(keysym)?;

        debug!(
            "RDP: Keyboard - keysym: {}, scancode: 0x{:02x}, pressed: {}",
            keysym, scancode, pressed
        );

        Ok(RdpKeyEvent {
            scancode,
            pressed,
            extended: self.is_extended_key(keysym),
        })
    }

    /// Handle mouse input from Guacamole client
    ///
    /// # Arguments
    ///
    /// * `mask` - Mouse button mask
    /// * `x` - X coordinate
    /// * `y` - Y coordinate
    ///
    /// # Returns
    ///
    /// RDP pointer event for sending to RDP server
    pub fn handle_mouse(&mut self, mask: u8, x: i32, y: i32) -> Result<RdpPointerEvent, String> {
        // Extract button states from mask
        let left_button = (mask & 0x01) != 0;
        let middle_button = (mask & 0x02) != 0;
        let right_button = (mask & 0x04) != 0;
        let scroll_up = (mask & 0x08) != 0;
        let scroll_down = (mask & 0x10) != 0;

        // Detect button state changes
        let left_down = left_button && !self.prev_left;
        let left_up = !left_button && self.prev_left;
        let middle_down = middle_button && !self.prev_middle;
        let middle_up = !middle_button && self.prev_middle;
        let right_down = right_button && !self.prev_right;
        let right_up = !right_button && self.prev_right;

        // Update previous state
        self.prev_left = left_button;
        self.prev_middle = middle_button;
        self.prev_right = right_button;

        debug!(
            "RDP: Mouse - x: {}, y: {}, buttons: L={} M={} R={}, transitions: L_down={} L_up={} R_down={} R_up={}",
            x, y, left_button, middle_button, right_button, left_down, left_up, right_down, right_up
        );

        Ok(RdpPointerEvent {
            x,
            y,
            left_button,
            middle_button,
            right_button,
            left_down,
            left_up,
            middle_down,
            middle_up,
            right_down,
            right_up,
            scroll_up,
            scroll_down,
        })
    }

    /// Convert X11 keysym to RDP scancode
    ///
    /// This is a simplified mapping - a complete implementation would
    /// handle all keysyms and platform-specific mappings.
    fn keysym_to_scancode(&self, keysym: u32) -> Result<u8, String> {
        // Basic key mapping (X11 keysym to RDP scancode)
        // See: https://www.x.org/releases/X11R7.0/doc/xproto/x11protocol.html#keysym_encoding
        // and RDP scancode table

        match keysym {
            // Letters A-Z
            0x0041..=0x005A => Ok((keysym - 0x0041 + 0x1E) as u8), // A=0x1E, Z=0x2C
            // Numbers 0-9
            0x0030..=0x0039 => Ok((keysym - 0x0030 + 0x0B) as u8), // 0=0x0B, 9=0x14
            // Function keys F1-F12
            0xFFBE..=0xFFC9 => Ok((keysym - 0xFFBE + 0x3B) as u8), // F1=0x3B, F12=0x46
            // Special keys
            0xFF08 => Ok(0x0E), // Backspace
            0xFF09 => Ok(0x0F), // Tab
            0xFF0D => Ok(0x1C), // Enter
            0xFF1B => Ok(0x01), // Escape
            0xFF20 => Ok(0x39), // Space
            0xFF50 => Ok(0x47), // Home
            0xFF51 => Ok(0x4B), // Left Arrow
            0xFF52 => Ok(0x48), // Up Arrow
            0xFF53 => Ok(0x4D), // Right Arrow
            0xFF54 => Ok(0x50), // Down Arrow
            0xFF55 => Ok(0x49), // Page Up
            0xFF56 => Ok(0x51), // Page Down
            0xFF57 => Ok(0x4F), // End
            0xFF63 => Ok(0x53), // Insert
            0xFFFF => Ok(0x53), // Delete
            _ => {
                warn!(
                    "RDP: Unknown keysym: 0x{:04x}, using default scancode",
                    keysym
                );
                // Return a safe default (Space key)
                Ok(0x39)
            }
        }
    }

    /// Check if key requires extended scancode
    fn is_extended_key(&self, keysym: u32) -> bool {
        // Extended keys (right Alt, right Ctrl, arrow keys, etc.)
        match keysym {
            0xFF52 | 0xFF53 | 0xFF54 | 0xFF51 | // Arrow keys
            0xFF55 | 0xFF56 | 0xFF57 | 0xFF50 | // Page Up/Down, End, Home
            0xFF63 | 0xFFFF => true, // Insert, Delete
            _ => false,
        }
    }
}

/// RDP keyboard event
#[derive(Debug, Clone)]
pub struct RdpKeyEvent {
    pub scancode: u8,
    pub pressed: bool,
    pub extended: bool,
}

/// RDP pointer (mouse) event
#[derive(Debug, Clone)]
pub struct RdpPointerEvent {
    pub x: i32,
    pub y: i32,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
    pub left_down: bool, // Button just pressed
    pub left_up: bool,   // Button just released
    pub middle_down: bool,
    pub middle_up: bool,
    pub right_down: bool,
    pub right_up: bool,
    pub scroll_up: bool,
    pub scroll_down: bool,
}

impl Default for RdpInputHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keysym_to_scancode() {
        let handler = RdpInputHandler::new();

        // Test letter 'A'
        assert_eq!(handler.keysym_to_scancode(0x0041).unwrap(), 0x1E);

        // Test number '0'
        assert_eq!(handler.keysym_to_scancode(0x0030).unwrap(), 0x0B);

        // Test Enter
        assert_eq!(handler.keysym_to_scancode(0xFF0D).unwrap(), 0x1C);
    }

    #[test]
    fn test_mouse_handling() {
        let mut handler = RdpInputHandler::new();

        // Test left button click
        let event = handler.handle_mouse(0x01, 100, 200).unwrap();
        assert!(event.left_button);
        assert!(!event.right_button);
        assert_eq!(event.x, 100);
        assert_eq!(event.y, 200);
    }
}
