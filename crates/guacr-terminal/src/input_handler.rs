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
    /// Converts X11 keysym to RDP scancode with correct extended flag.
    pub fn handle_keyboard(&self, keysym: u32, pressed: bool) -> Result<RdpKeyEvent, String> {
        let (scancode, extended) = keysym_to_scancode(keysym)?;

        debug!(
            "RDP: Keyboard - keysym: 0x{:04x}, scancode: 0x{:02x}, extended: {}, pressed: {}",
            keysym, scancode, extended, pressed
        );

        Ok(RdpKeyEvent {
            scancode,
            pressed,
            extended,
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
}

/// Convert X11 keysym to (RDP Set 1 scancode, extended flag).
///
/// Covers US QWERTY layout: letters, digits, punctuation, modifiers,
/// navigation, function keys. Scancodes follow the IBM PC AT Set 1
/// encoding used by RDP's Fast-Path input.
fn keysym_to_scancode(keysym: u32) -> Result<(u8, bool), String> {
    // (scancode, extended)
    let result: (u8, bool) = match keysym {
        // ── Letters (lowercase a-z and uppercase A-Z share scancodes) ──
        // QWERTY row order, NOT alphabetical
        0x0041 | 0x0061 => (0x1E, false), // A/a
        0x0042 | 0x0062 => (0x30, false), // B/b
        0x0043 | 0x0063 => (0x2E, false), // C/c
        0x0044 | 0x0064 => (0x20, false), // D/d
        0x0045 | 0x0065 => (0x12, false), // E/e
        0x0046 | 0x0066 => (0x21, false), // F/f
        0x0047 | 0x0067 => (0x22, false), // G/g
        0x0048 | 0x0068 => (0x23, false), // H/h
        0x0049 | 0x0069 => (0x17, false), // I/i
        0x004A | 0x006A => (0x24, false), // J/j
        0x004B | 0x006B => (0x25, false), // K/k
        0x004C | 0x006C => (0x26, false), // L/l
        0x004D | 0x006D => (0x32, false), // M/m
        0x004E | 0x006E => (0x31, false), // N/n
        0x004F | 0x006F => (0x18, false), // O/o
        0x0050 | 0x0070 => (0x19, false), // P/p
        0x0051 | 0x0071 => (0x10, false), // Q/q
        0x0052 | 0x0072 => (0x13, false), // R/r
        0x0053 | 0x0073 => (0x1F, false), // S/s
        0x0054 | 0x0074 => (0x14, false), // T/t
        0x0055 | 0x0075 => (0x16, false), // U/u
        0x0056 | 0x0076 => (0x2F, false), // V/v
        0x0057 | 0x0077 => (0x11, false), // W/w
        0x0058 | 0x0078 => (0x2D, false), // X/x
        0x0059 | 0x0079 => (0x15, false), // Y/y
        0x005A | 0x007A => (0x2C, false), // Z/z

        // ── Digits (top row) and their shifted symbols ──
        0x0031 | 0x0021 => (0x02, false), // 1 / !
        0x0032 | 0x0040 => (0x03, false), // 2 / @
        0x0033 | 0x0023 => (0x04, false), // 3 / #
        0x0034 | 0x0024 => (0x05, false), // 4 / $
        0x0035 | 0x0025 => (0x06, false), // 5 / %
        0x0036 | 0x005E => (0x07, false), // 6 / ^
        0x0037 | 0x0026 => (0x08, false), // 7 / &
        0x0038 | 0x002A => (0x09, false), // 8 / *
        0x0039 | 0x0028 => (0x0A, false), // 9 / (
        0x0030 | 0x0029 => (0x0B, false), // 0 / )

        // ── Punctuation and their shifted variants ──
        0x0020 => (0x39, false),          // Space
        0x002D | 0x005F => (0x0C, false), // - / _
        0x003D | 0x002B => (0x0D, false), // = / +
        0x005B | 0x007B => (0x1A, false), // [ / {
        0x005D | 0x007D => (0x1B, false), // ] / }
        0x005C | 0x007C => (0x2B, false), // \ / |
        0x003B | 0x003A => (0x27, false), // ; / :
        0x0027 | 0x0022 => (0x28, false), // ' / "
        0x0060 | 0x007E => (0x29, false), // ` / ~
        0x002C | 0x003C => (0x33, false), // , / <
        0x002E | 0x003E => (0x34, false), // . / >
        0x002F | 0x003F => (0x35, false), // / / ?

        // ── Modifier keys ──
        0xFFE1 => (0x2A, false), // Left Shift
        0xFFE2 => (0x36, false), // Right Shift
        0xFFE3 => (0x1D, false), // Left Control
        0xFFE4 => (0x1D, true),  // Right Control (extended)
        0xFFE5 => (0x3A, false), // Caps Lock
        0xFFE9 => (0x38, false), // Left Alt
        0xFFEA => (0x38, true),  // Right Alt (extended)
        0xFFEB => (0x5B, true),  // Left Super/Windows (extended)
        0xFFEC => (0x5C, true),  // Right Super/Windows (extended)
        0xFF7F => (0x45, false), // Num Lock
        0xFF14 => (0x46, false), // Scroll Lock

        // ── Navigation keys (all extended) ──
        0xFF50 => (0x47, true), // Home
        0xFF51 => (0x4B, true), // Left Arrow
        0xFF52 => (0x48, true), // Up Arrow
        0xFF53 => (0x4D, true), // Right Arrow
        0xFF54 => (0x50, true), // Down Arrow
        0xFF55 => (0x49, true), // Page Up
        0xFF56 => (0x51, true), // Page Down
        0xFF57 => (0x4F, true), // End
        0xFF63 => (0x52, true), // Insert
        0xFFFF => (0x53, true), // Delete

        // ── Editing keys ──
        0xFF08 => (0x0E, false), // Backspace
        0xFF09 => (0x0F, false), // Tab
        0xFF0D => (0x1C, false), // Return/Enter
        0xFF1B => (0x01, false), // Escape
        0xFF61 => (0x37, true),  // Print Screen (extended)
        0xFF13 => (0x46, true),  // Pause (extended, special)
        0xFF67 => (0x5D, true),  // Menu/Apps key (extended)

        // ── Function keys F1-F12 ──
        // F1-F10 are sequential 0x3B-0x44, F11-F12 jump to 0x57-0x58
        0xFFBE => (0x3B, false), // F1
        0xFFBF => (0x3C, false), // F2
        0xFFC0 => (0x3D, false), // F3
        0xFFC1 => (0x3E, false), // F4
        0xFFC2 => (0x3F, false), // F5
        0xFFC3 => (0x40, false), // F6
        0xFFC4 => (0x41, false), // F7
        0xFFC5 => (0x42, false), // F8
        0xFFC6 => (0x43, false), // F9
        0xFFC7 => (0x44, false), // F10
        0xFFC8 => (0x57, false), // F11
        0xFFC9 => (0x58, false), // F12

        // ── Numpad keys ──
        0xFFB0 => (0x52, false), // KP_0
        0xFFB1 => (0x4F, false), // KP_1
        0xFFB2 => (0x50, false), // KP_2
        0xFFB3 => (0x51, false), // KP_3
        0xFFB4 => (0x4B, false), // KP_4
        0xFFB5 => (0x4C, false), // KP_5
        0xFFB6 => (0x4D, false), // KP_6
        0xFFB7 => (0x48, false), // KP_7
        0xFFB8 => (0x49, false), // KP_8
        0xFFB9 => (0x4A, false), // KP_9
        0xFFAA => (0x37, false), // KP_Multiply
        0xFFAB => (0x4E, false), // KP_Add
        0xFFAD => (0x4A, false), // KP_Subtract
        0xFFAE => (0x53, false), // KP_Decimal
        0xFFAF => (0x35, true),  // KP_Divide (extended)
        0xFF8D => (0x1C, true),  // KP_Enter (extended)

        _ => {
            warn!("RDP: Unmapped keysym: 0x{:04x}", keysym);
            return Err(format!("Unmapped keysym: 0x{:04x}", keysym));
        }
    };

    Ok(result)
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
    fn test_keysym_to_scancode_letters() {
        // Uppercase
        assert_eq!(keysym_to_scancode(0x0041).unwrap(), (0x1E, false)); // A
        assert_eq!(keysym_to_scancode(0x0051).unwrap(), (0x10, false)); // Q
        assert_eq!(keysym_to_scancode(0x005A).unwrap(), (0x2C, false)); // Z

        // Lowercase maps to same scancode
        assert_eq!(keysym_to_scancode(0x0061).unwrap(), (0x1E, false)); // a
        assert_eq!(keysym_to_scancode(0x0071).unwrap(), (0x10, false)); // q
        assert_eq!(keysym_to_scancode(0x007A).unwrap(), (0x2C, false)); // z
    }

    #[test]
    fn test_keysym_to_scancode_digits() {
        assert_eq!(keysym_to_scancode(0x0030).unwrap(), (0x0B, false)); // 0
        assert_eq!(keysym_to_scancode(0x0031).unwrap(), (0x02, false)); // 1
        assert_eq!(keysym_to_scancode(0x0039).unwrap(), (0x0A, false)); // 9
    }

    #[test]
    fn test_keysym_to_scancode_special() {
        assert_eq!(keysym_to_scancode(0x0020).unwrap(), (0x39, false)); // Space
        assert_eq!(keysym_to_scancode(0xFF0D).unwrap(), (0x1C, false)); // Enter
        assert_eq!(keysym_to_scancode(0xFF08).unwrap(), (0x0E, false)); // Backspace
        assert_eq!(keysym_to_scancode(0xFF09).unwrap(), (0x0F, false)); // Tab
        assert_eq!(keysym_to_scancode(0xFF1B).unwrap(), (0x01, false)); // Escape
    }

    #[test]
    fn test_keysym_to_scancode_extended() {
        // Navigation keys are extended
        assert_eq!(keysym_to_scancode(0xFF51).unwrap(), (0x4B, true)); // Left
        assert_eq!(keysym_to_scancode(0xFF52).unwrap(), (0x48, true)); // Up
        assert_eq!(keysym_to_scancode(0xFF53).unwrap(), (0x4D, true)); // Right
        assert_eq!(keysym_to_scancode(0xFF54).unwrap(), (0x50, true)); // Down
        assert_eq!(keysym_to_scancode(0xFF50).unwrap(), (0x47, true)); // Home
        assert_eq!(keysym_to_scancode(0xFF57).unwrap(), (0x4F, true)); // End
        assert_eq!(keysym_to_scancode(0xFFFF).unwrap(), (0x53, true)); // Delete
        assert_eq!(keysym_to_scancode(0xFF63).unwrap(), (0x52, true)); // Insert

        // Right Ctrl/Alt are extended
        assert_eq!(keysym_to_scancode(0xFFE4).unwrap(), (0x1D, true)); // Right Ctrl
        assert_eq!(keysym_to_scancode(0xFFEA).unwrap(), (0x38, true)); // Right Alt
    }

    #[test]
    fn test_keysym_to_scancode_function_keys() {
        assert_eq!(keysym_to_scancode(0xFFBE).unwrap(), (0x3B, false)); // F1
        assert_eq!(keysym_to_scancode(0xFFC7).unwrap(), (0x44, false)); // F10
        assert_eq!(keysym_to_scancode(0xFFC8).unwrap(), (0x57, false)); // F11
        assert_eq!(keysym_to_scancode(0xFFC9).unwrap(), (0x58, false)); // F12
    }

    #[test]
    fn test_keysym_to_scancode_modifiers() {
        assert_eq!(keysym_to_scancode(0xFFE1).unwrap(), (0x2A, false)); // Left Shift
        assert_eq!(keysym_to_scancode(0xFFE2).unwrap(), (0x36, false)); // Right Shift
        assert_eq!(keysym_to_scancode(0xFFE3).unwrap(), (0x1D, false)); // Left Ctrl
        assert_eq!(keysym_to_scancode(0xFFE9).unwrap(), (0x38, false)); // Left Alt
        assert_eq!(keysym_to_scancode(0xFFE5).unwrap(), (0x3A, false)); // Caps Lock
    }

    #[test]
    fn test_keysym_to_scancode_punctuation() {
        assert_eq!(keysym_to_scancode(0x002D).unwrap(), (0x0C, false)); // -
        assert_eq!(keysym_to_scancode(0x003D).unwrap(), (0x0D, false)); // =
        assert_eq!(keysym_to_scancode(0x005B).unwrap(), (0x1A, false)); // [
        assert_eq!(keysym_to_scancode(0x005D).unwrap(), (0x1B, false)); // ]
        assert_eq!(keysym_to_scancode(0x003B).unwrap(), (0x27, false)); // ;
        assert_eq!(keysym_to_scancode(0x0027).unwrap(), (0x28, false)); // '
        assert_eq!(keysym_to_scancode(0x002C).unwrap(), (0x33, false)); // ,
        assert_eq!(keysym_to_scancode(0x002E).unwrap(), (0x34, false)); // .
        assert_eq!(keysym_to_scancode(0x002F).unwrap(), (0x35, false)); // /
    }

    #[test]
    fn test_unknown_keysym_returns_error() {
        assert!(keysym_to_scancode(0x9999).is_err());
    }

    #[test]
    fn test_handle_keyboard_integration() {
        let handler = RdpInputHandler::new();
        // Typing lowercase 'a' should produce scancode 0x1E, not extended
        let event = handler.handle_keyboard(0x0061, true).unwrap();
        assert_eq!(event.scancode, 0x1E);
        assert!(event.pressed);
        assert!(!event.extended);
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
