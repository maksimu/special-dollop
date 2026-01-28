// RBI input handling - keyboard, mouse, touch events
// Ported from KCM's cef/input.c with comprehensive keysym tables

use log::{debug, warn};
use std::collections::HashMap;

// X11 Keysym constants for modifier keys (from KCM input.h)
pub const KEYSYM_ALT_LEFT: u32 = 0xFFE9;
pub const KEYSYM_ALT_RIGHT: u32 = 0xFFEA;
pub const KEYSYM_CTRL_LEFT: u32 = 0xFFE3;
pub const KEYSYM_CTRL_RIGHT: u32 = 0xFFE4;
pub const KEYSYM_META_LEFT: u32 = 0xFFE7; // Command on Mac
pub const KEYSYM_META_RIGHT: u32 = 0xFFE8;
pub const KEYSYM_SHIFT_LEFT: u32 = 0xFFE1;
pub const KEYSYM_SHIFT_RIGHT: u32 = 0xFFE2;
pub const KEYSYM_ALTGR: u32 = 0xFE03;

// Mouse button masks
pub const MOUSE_BUTTON_LEFT: u32 = 0x01;
pub const MOUSE_BUTTON_MIDDLE: u32 = 0x02;
pub const MOUSE_BUTTON_RIGHT: u32 = 0x04;
pub const MOUSE_SCROLL_UP: u32 = 0x08;
pub const MOUSE_SCROLL_DOWN: u32 = 0x10;

/// Mouse scroll distance in pixels (matches KCM)
pub const MOUSE_SCROLL_DISTANCE: i32 = 160;

/// Maximum number of concurrent touch events (14 fingers per hand = 28 max)
pub const MAX_TOUCH_EVENTS: usize = 28;

/// CEF event flags for modifiers
#[derive(Debug, Clone, Copy, Default)]
pub struct CefEventFlags(pub u32);

impl CefEventFlags {
    pub const CAPS_LOCK_ON: u32 = 1 << 0;
    pub const SHIFT_DOWN: u32 = 1 << 1;
    pub const CONTROL_DOWN: u32 = 1 << 2;
    pub const ALT_DOWN: u32 = 1 << 3;
    pub const LEFT_MOUSE_BUTTON: u32 = 1 << 4;
    pub const MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
    pub const RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
    pub const COMMAND_DOWN: u32 = 1 << 7; // Meta/Cmd on Mac
    pub const NUM_LOCK_ON: u32 = 1 << 8;
    pub const IS_KEY_PAD: u32 = 1 << 9;
    pub const IS_LEFT: u32 = 1 << 10;
    pub const IS_RIGHT: u32 = 1 << 11;
    pub const ALTGR_DOWN: u32 = 1 << 12;
}

/// Key mapping from X11 keysym to JavaScript keyCode
/// Based on KCM's key_mappings table in input.c
#[derive(Debug, Clone, Copy)]
pub struct KeyMapping {
    pub keysym: u32,
    pub js_key_code: u32,
}

/// Complete keysym to JavaScript keyCode mapping table
/// Ported from KCM's input.c key_mappings array
pub static KEY_MAPPINGS: &[KeyMapping] = &[
    // Control keys
    KeyMapping {
        keysym: 0xFF08,
        js_key_code: 8,
    }, // Backspace
    KeyMapping {
        keysym: 0xFF09,
        js_key_code: 9,
    }, // Tab
    KeyMapping {
        keysym: 0xFF0D,
        js_key_code: 13,
    }, // Enter
    KeyMapping {
        keysym: 0xFF1B,
        js_key_code: 27,
    }, // Escape
    KeyMapping {
        keysym: 0xFD1D,
        js_key_code: 135,
    }, // Print Screen
    KeyMapping {
        keysym: 0xFF13,
        js_key_code: 19,
    }, // Pause/Break
    // Modifier keys
    KeyMapping {
        keysym: KEYSYM_ALTGR,
        js_key_code: 225,
    },
    KeyMapping {
        keysym: KEYSYM_SHIFT_LEFT,
        js_key_code: 16,
    },
    KeyMapping {
        keysym: KEYSYM_SHIFT_RIGHT,
        js_key_code: 16,
    },
    KeyMapping {
        keysym: KEYSYM_CTRL_LEFT,
        js_key_code: 17,
    },
    KeyMapping {
        keysym: KEYSYM_CTRL_RIGHT,
        js_key_code: 17,
    },
    KeyMapping {
        keysym: KEYSYM_META_LEFT,
        js_key_code: 91,
    },
    KeyMapping {
        keysym: KEYSYM_META_RIGHT,
        js_key_code: 92,
    },
    KeyMapping {
        keysym: KEYSYM_ALT_LEFT,
        js_key_code: 18,
    },
    KeyMapping {
        keysym: KEYSYM_ALT_RIGHT,
        js_key_code: 18,
    },
    // Navigation/arrow keys
    KeyMapping {
        keysym: 0xFF55,
        js_key_code: 33,
    }, // Page Up
    KeyMapping {
        keysym: 0xFF56,
        js_key_code: 34,
    }, // Page Down
    KeyMapping {
        keysym: 0xFF50,
        js_key_code: 36,
    }, // Home
    KeyMapping {
        keysym: 0xFF57,
        js_key_code: 35,
    }, // End
    KeyMapping {
        keysym: 0xFF51,
        js_key_code: 37,
    }, // Left Arrow
    KeyMapping {
        keysym: 0xFF52,
        js_key_code: 38,
    }, // Up Arrow
    KeyMapping {
        keysym: 0xFF53,
        js_key_code: 39,
    }, // Right Arrow
    KeyMapping {
        keysym: 0xFF54,
        js_key_code: 40,
    }, // Down Arrow
    KeyMapping {
        keysym: 0xFF63,
        js_key_code: 45,
    }, // Insert
    KeyMapping {
        keysym: 0xFFFF,
        js_key_code: 46,
    }, // Delete
    // Function keys F1-F12
    KeyMapping {
        keysym: 0xFFBE,
        js_key_code: 112,
    }, // F1
    KeyMapping {
        keysym: 0xFFBF,
        js_key_code: 113,
    }, // F2
    KeyMapping {
        keysym: 0xFFC0,
        js_key_code: 114,
    }, // F3
    KeyMapping {
        keysym: 0xFFC1,
        js_key_code: 115,
    }, // F4
    KeyMapping {
        keysym: 0xFFC2,
        js_key_code: 116,
    }, // F5
    KeyMapping {
        keysym: 0xFFC3,
        js_key_code: 117,
    }, // F6
    KeyMapping {
        keysym: 0xFFC4,
        js_key_code: 118,
    }, // F7
    KeyMapping {
        keysym: 0xFFC5,
        js_key_code: 119,
    }, // F8
    KeyMapping {
        keysym: 0xFFC6,
        js_key_code: 120,
    }, // F9
    KeyMapping {
        keysym: 0xFFC7,
        js_key_code: 121,
    }, // F10
    KeyMapping {
        keysym: 0xFFC8,
        js_key_code: 122,
    }, // F11
    KeyMapping {
        keysym: 0xFFC9,
        js_key_code: 123,
    }, // F12
    // Lock keys
    KeyMapping {
        keysym: 0xFF7F,
        js_key_code: 144,
    }, // Num Lock
    KeyMapping {
        keysym: 0xFF14,
        js_key_code: 145,
    }, // Scroll Lock
    KeyMapping {
        keysym: 0xFFE5,
        js_key_code: 20,
    }, // Caps Lock
    // Keypad
    KeyMapping {
        keysym: 0xFF8D,
        js_key_code: 3,
    }, // Keypad Enter
    KeyMapping {
        keysym: 0xFFB0,
        js_key_code: 96,
    }, // Keypad 0
    KeyMapping {
        keysym: 0xFFB1,
        js_key_code: 97,
    }, // Keypad 1
    KeyMapping {
        keysym: 0xFFB2,
        js_key_code: 98,
    }, // Keypad 2
    KeyMapping {
        keysym: 0xFFB3,
        js_key_code: 99,
    }, // Keypad 3
    KeyMapping {
        keysym: 0xFFB4,
        js_key_code: 100,
    }, // Keypad 4
    KeyMapping {
        keysym: 0xFFB5,
        js_key_code: 101,
    }, // Keypad 5
    KeyMapping {
        keysym: 0xFFB6,
        js_key_code: 102,
    }, // Keypad 6
    KeyMapping {
        keysym: 0xFFB7,
        js_key_code: 103,
    }, // Keypad 7
    KeyMapping {
        keysym: 0xFFB8,
        js_key_code: 104,
    }, // Keypad 8
    KeyMapping {
        keysym: 0xFFB9,
        js_key_code: 105,
    }, // Keypad 9
    KeyMapping {
        keysym: 0xFFAD,
        js_key_code: 109,
    }, // Keypad Subtract (-)
    KeyMapping {
        keysym: 0xFFAB,
        js_key_code: 107,
    }, // Keypad Add (+)
    KeyMapping {
        keysym: 0xFFAE,
        js_key_code: 110,
    }, // Keypad Decimal (.)
    KeyMapping {
        keysym: 0xFFAF,
        js_key_code: 111,
    }, // Keypad Divide (/)
    KeyMapping {
        keysym: 0xFFAA,
        js_key_code: 106,
    }, // Keypad Multiply (*)
    // Space
    KeyMapping {
        keysym: ' ' as u32,
        js_key_code: ' ' as u32,
    },
    // Shifted digits
    KeyMapping {
        keysym: ')' as u32,
        js_key_code: '0' as u32,
    },
    KeyMapping {
        keysym: '!' as u32,
        js_key_code: '1' as u32,
    },
    KeyMapping {
        keysym: '@' as u32,
        js_key_code: '2' as u32,
    },
    KeyMapping {
        keysym: '#' as u32,
        js_key_code: '3' as u32,
    },
    KeyMapping {
        keysym: '$' as u32,
        js_key_code: '4' as u32,
    },
    KeyMapping {
        keysym: '%' as u32,
        js_key_code: '5' as u32,
    },
    KeyMapping {
        keysym: '^' as u32,
        js_key_code: '6' as u32,
    },
    KeyMapping {
        keysym: '&' as u32,
        js_key_code: '7' as u32,
    },
    KeyMapping {
        keysym: '*' as u32,
        js_key_code: '8' as u32,
    },
    KeyMapping {
        keysym: '(' as u32,
        js_key_code: '9' as u32,
    },
    // Punctuation with non-ASCII key codes
    KeyMapping {
        keysym: '`' as u32,
        js_key_code: 192,
    },
    KeyMapping {
        keysym: '~' as u32,
        js_key_code: 192,
    },
    KeyMapping {
        keysym: '-' as u32,
        js_key_code: 189,
    },
    KeyMapping {
        keysym: '_' as u32,
        js_key_code: 189,
    },
    KeyMapping {
        keysym: '=' as u32,
        js_key_code: 187,
    },
    KeyMapping {
        keysym: '+' as u32,
        js_key_code: 187,
    },
    KeyMapping {
        keysym: '[' as u32,
        js_key_code: 219,
    },
    KeyMapping {
        keysym: '{' as u32,
        js_key_code: 219,
    },
    KeyMapping {
        keysym: ']' as u32,
        js_key_code: 221,
    },
    KeyMapping {
        keysym: '}' as u32,
        js_key_code: 221,
    },
    KeyMapping {
        keysym: ';' as u32,
        js_key_code: 186,
    },
    KeyMapping {
        keysym: ':' as u32,
        js_key_code: 186,
    },
    KeyMapping {
        keysym: '/' as u32,
        js_key_code: 191,
    },
    KeyMapping {
        keysym: '?' as u32,
        js_key_code: 191,
    },
    KeyMapping {
        keysym: '\\' as u32,
        js_key_code: 220,
    },
    KeyMapping {
        keysym: '|' as u32,
        js_key_code: 220,
    },
    KeyMapping {
        keysym: ',' as u32,
        js_key_code: 188,
    },
    KeyMapping {
        keysym: '<' as u32,
        js_key_code: 188,
    },
    KeyMapping {
        keysym: '.' as u32,
        js_key_code: 190,
    },
    KeyMapping {
        keysym: '>' as u32,
        js_key_code: 190,
    },
    KeyMapping {
        keysym: '\'' as u32,
        js_key_code: 222,
    },
    KeyMapping {
        keysym: '"' as u32,
        js_key_code: 222,
    },
];

/// Keyboard state tracking (hash table for pressed keys)
#[derive(Debug, Default)]
pub struct KeyboardState {
    pressed_keys: HashMap<u32, bool>,
    pressed_count: usize,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set key pressed state
    pub fn set_pressed(&mut self, keysym: u32, pressed: bool) {
        let was_pressed = self.pressed_keys.get(&keysym).copied().unwrap_or(false);

        if pressed && !was_pressed {
            self.pressed_count += 1;
        } else if !pressed && was_pressed {
            self.pressed_count = self.pressed_count.saturating_sub(1);
        }

        if pressed {
            self.pressed_keys.insert(keysym, true);
        } else {
            self.pressed_keys.remove(&keysym);
        }
    }

    /// Check if key is pressed
    pub fn is_pressed(&self, keysym: u32) -> bool {
        self.pressed_keys.get(&keysym).copied().unwrap_or(false)
    }

    /// Get number of currently pressed keys
    pub fn pressed_count(&self) -> usize {
        self.pressed_count
    }

    /// Get current modifier flags for CEF events
    pub fn get_modifiers(&self) -> u32 {
        let mut modifiers = 0u32;

        if self.is_pressed(KEYSYM_ALTGR) {
            modifiers |= CefEventFlags::ALTGR_DOWN;
        }

        if self.is_pressed(KEYSYM_SHIFT_LEFT) || self.is_pressed(KEYSYM_SHIFT_RIGHT) {
            modifiers |= CefEventFlags::SHIFT_DOWN;
        }

        if self.is_pressed(KEYSYM_CTRL_LEFT) || self.is_pressed(KEYSYM_CTRL_RIGHT) {
            modifiers |= CefEventFlags::CONTROL_DOWN;
        }

        if self.is_pressed(KEYSYM_META_LEFT) || self.is_pressed(KEYSYM_META_RIGHT) {
            modifiers |= CefEventFlags::COMMAND_DOWN;
        }

        if self.is_pressed(KEYSYM_ALT_LEFT) || self.is_pressed(KEYSYM_ALT_RIGHT) {
            modifiers |= CefEventFlags::ALT_DOWN;
        }

        modifiers
    }
}

/// Mouse state tracking
#[derive(Debug, Default)]
pub struct MouseState {
    pub mask: u32,
    pub modifiers: u32,
    pub x: i32,
    pub y: i32,
}

impl MouseState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get mouse button modifiers for CEF events
    pub fn get_mouse_modifiers(&self, mask: u32) -> u32 {
        let mut modifiers = 0u32;

        if (mask & MOUSE_BUTTON_LEFT) != 0 {
            modifiers |= CefEventFlags::LEFT_MOUSE_BUTTON;
        }
        if (mask & MOUSE_BUTTON_RIGHT) != 0 {
            modifiers |= CefEventFlags::RIGHT_MOUSE_BUTTON;
        }
        if (mask & MOUSE_BUTTON_MIDDLE) != 0 {
            modifiers |= CefEventFlags::MIDDLE_MOUSE_BUTTON;
        }

        modifiers
    }
}

/// Touch state tracking for multi-touch
#[derive(Debug, Default)]
pub struct TouchState {
    touch_points: [f64; MAX_TOUCH_EVENTS],
}

impl TouchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update touch point pressure
    pub fn set_pressure(&mut self, id: usize, pressure: f64) -> Option<TouchEventType> {
        if id >= MAX_TOUCH_EVENTS {
            warn!("Touch ID {} exceeds maximum {}", id, MAX_TOUCH_EVENTS);
            return None;
        }

        let previous = self.touch_points[id];
        self.touch_points[id] = pressure;

        // Determine event type based on state change
        if pressure > 0.0 && previous == 0.0 {
            Some(TouchEventType::Pressed)
        } else if pressure == 0.0 && previous > 0.0 {
            Some(TouchEventType::Released)
        } else {
            Some(TouchEventType::Moved)
        }
    }
}

/// Touch event types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchEventType {
    Pressed,
    Moved,
    Released,
    Cancelled,
}

/// Combined input state
#[derive(Debug, Default)]
pub struct InputState {
    pub keyboard: KeyboardState,
    pub mouse: MouseState,
    pub touch: TouchState,
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Convert X11 keysym to JavaScript keyCode
pub fn keysym_to_js_keycode(keysym: u32) -> u32 {
    // Check explicit mappings first
    for mapping in KEY_MAPPINGS {
        if mapping.keysym == keysym {
            return mapping.js_key_code;
        }
    }

    // Uppercase alphanumeric map directly
    if (keysym >= '0' as u32 && keysym <= '9' as u32)
        || (keysym >= 'A' as u32 && keysym <= 'Z' as u32)
    {
        return keysym;
    }

    // Lowercase alphabetic map to uppercase
    if keysym >= 'a' as u32 && keysym <= 'z' as u32 {
        return keysym - 'a' as u32 + 'A' as u32;
    }

    // Default: return 0 for unmapped
    0
}

/// Check if keysym produces a printable character
pub fn is_printable(keysym: u32) -> bool {
    // Tab and Enter are printable (produce tab/newline)
    match keysym {
        0xFF09 | 0xFF0D => return true,
        _ => {}
    }

    // Basic ASCII, number pad symbols, and Unicode characters
    (keysym > 0 && keysym <= 0xFF)
        || (keysym == 0xFFBD || (0xFFAA..=0xFFB9).contains(&keysym))
        || (keysym >= 0x01000000)
}

/// Convert keysym to Unicode character (for text input)
pub fn keysym_to_unicode(keysym: u32) -> Option<char> {
    match keysym {
        0xFF0D => Some('\r'), // Enter (CEF wants CR)
        0xFF09 => Some('\t'), // Tab

        // Number pad symbols
        0xFFBD => Some('='),
        0xFFAA => Some('*'),
        0xFFAB => Some('+'),
        0xFFAD => Some('-'),
        0xFFAE => Some('.'),
        0xFFAF => Some('/'),

        // Number pad digits
        k if (0xFFB0..=0xFFB9).contains(&k) => char::from_u32((k - 0xFFB0) + '0' as u32),

        // Other printable characters
        k if is_printable(k) => char::from_u32(k & 0xFFFF),

        _ => None,
    }
}

/// Parsed keyboard event ready for browser injection
#[derive(Debug, Clone)]
pub struct BrowserKeyEvent {
    pub js_key_code: u32,
    pub character: Option<char>,
    pub modifiers: u32,
    pub pressed: bool,
    pub is_repeat: bool,
}

/// Parsed mouse event ready for browser injection
#[derive(Debug, Clone)]
pub struct BrowserMouseEvent {
    pub x: i32,
    pub y: i32,
    pub modifiers: u32,
    pub buttons_pressed: Vec<MouseButton>,
    pub buttons_released: Vec<MouseButton>,
    pub scroll_delta_y: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Parsed touch event ready for browser injection
#[derive(Debug, Clone)]
pub struct BrowserTouchEvent {
    pub id: i32,
    pub x: i32,
    pub y: i32,
    pub radius_x: i32,
    pub radius_y: i32,
    pub rotation_angle: f64,
    pub pressure: f64,
    pub event_type: TouchEventType,
}

/// RBI input handler
///
/// Processes Guacamole input events and converts them for browser injection.
/// Maintains keyboard/mouse/touch state for proper modifier handling.
pub struct RbiInputHandler {
    state: InputState,
}

impl RbiInputHandler {
    pub fn new() -> Self {
        Self {
            state: InputState::new(),
        }
    }

    /// Handle keyboard event from Guacamole
    pub fn handle_keyboard(&mut self, keysym: u32, pressed: bool) -> BrowserKeyEvent {
        let was_pressed = self.state.keyboard.is_pressed(keysym);
        self.state.keyboard.set_pressed(keysym, pressed);

        let js_key_code = keysym_to_js_keycode(keysym);
        let character = if pressed {
            keysym_to_unicode(keysym)
        } else {
            None
        };
        let modifiers = self.state.keyboard.get_modifiers();

        debug!(
            "RBI: Key event - keysym: 0x{:04x}, js_code: {}, pressed: {}, mods: 0x{:02x}",
            keysym, js_key_code, pressed, modifiers
        );

        BrowserKeyEvent {
            js_key_code,
            character,
            modifiers,
            pressed,
            is_repeat: pressed && was_pressed,
        }
    }

    /// Check if this is a shortcut that should be handled specially
    /// Returns Some(shortcut_type) if we should intercept
    pub fn check_shortcut(&self, keysym: u32, pressed: bool) -> Option<KeyboardShortcut> {
        if !pressed {
            return None;
        }

        let modifiers = self.state.keyboard.get_modifiers();
        let ctrl_or_cmd =
            (modifiers & (CefEventFlags::CONTROL_DOWN | CefEventFlags::COMMAND_DOWN)) != 0;

        // Only handle 2-key shortcuts (modifier + key)
        if self.state.keyboard.pressed_count() == 2 && ctrl_or_cmd {
            match keysym {
                k if k == 'a' as u32 || k == 'A' as u32 => Some(KeyboardShortcut::SelectAll),
                k if k == 'c' as u32 || k == 'C' as u32 => Some(KeyboardShortcut::Copy),
                k if k == 'v' as u32 || k == 'V' as u32 => Some(KeyboardShortcut::Paste),
                k if k == 'x' as u32 || k == 'X' as u32 => Some(KeyboardShortcut::Cut),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Handle mouse event from Guacamole
    pub fn handle_mouse(&mut self, x: i32, y: i32, mask: u32) -> BrowserMouseEvent {
        let old_mask = self.state.mouse.mask;

        // Buttons just pressed
        let pressed_mask = mask & !old_mask;
        // Buttons just released
        let released_mask = !mask & old_mask;

        let mut buttons_pressed = Vec::new();
        let mut buttons_released = Vec::new();

        // Check each button
        if (pressed_mask & MOUSE_BUTTON_LEFT) != 0 {
            buttons_pressed.push(MouseButton::Left);
        }
        if (pressed_mask & MOUSE_BUTTON_MIDDLE) != 0 {
            buttons_pressed.push(MouseButton::Middle);
        }
        if (pressed_mask & MOUSE_BUTTON_RIGHT) != 0 {
            buttons_pressed.push(MouseButton::Right);
        }

        if (released_mask & MOUSE_BUTTON_LEFT) != 0 {
            buttons_released.push(MouseButton::Left);
        }
        if (released_mask & MOUSE_BUTTON_MIDDLE) != 0 {
            buttons_released.push(MouseButton::Middle);
        }
        if (released_mask & MOUSE_BUTTON_RIGHT) != 0 {
            buttons_released.push(MouseButton::Right);
        }

        // Handle scroll
        let scroll_delta_y = if (pressed_mask & MOUSE_SCROLL_UP) != 0 {
            Some(MOUSE_SCROLL_DISTANCE)
        } else if (pressed_mask & MOUSE_SCROLL_DOWN) != 0 {
            Some(-MOUSE_SCROLL_DISTANCE)
        } else {
            None
        };

        // Update state
        let old_modifiers = self.state.mouse.modifiers;
        let new_modifiers = (old_modifiers & !self.state.mouse.get_mouse_modifiers(released_mask))
            | self.state.mouse.get_mouse_modifiers(pressed_mask);

        // Combine keyboard and mouse modifiers
        let modifiers = self.state.keyboard.get_modifiers() | new_modifiers;

        self.state.mouse.mask = mask;
        self.state.mouse.modifiers = new_modifiers;
        self.state.mouse.x = x;
        self.state.mouse.y = y;

        debug!(
            "RBI: Mouse event - x: {}, y: {}, mask: 0x{:02x}, pressed: {:?}, released: {:?}",
            x, y, mask, buttons_pressed, buttons_released
        );

        BrowserMouseEvent {
            x,
            y,
            modifiers,
            buttons_pressed,
            buttons_released,
            scroll_delta_y,
        }
    }

    /// Handle touch event from Guacamole
    #[allow(clippy::too_many_arguments)]
    pub fn handle_touch(
        &mut self,
        id: i32,
        x: i32,
        y: i32,
        radius_x: i32,
        radius_y: i32,
        angle: f64,
        force: f64,
    ) -> Option<BrowserTouchEvent> {
        if id < 0 || id as usize >= MAX_TOUCH_EVENTS {
            warn!("RBI: Touch ID {} out of range", id);
            return None;
        }

        let event_type = self.state.touch.set_pressure(id as usize, force)?;

        debug!(
            "RBI: Touch event - id: {}, x: {}, y: {}, type: {:?}",
            id, x, y, event_type
        );

        Some(BrowserTouchEvent {
            id,
            x,
            y,
            radius_x,
            radius_y,
            rotation_angle: angle,
            pressure: force,
            event_type,
        })
    }
}

impl Default for RbiInputHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Keyboard shortcuts that we handle specially
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyboardShortcut {
    SelectAll,
    Copy,
    Paste,
    Cut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keysym_to_js_keycode() {
        // Test letter 'A'
        assert_eq!(keysym_to_js_keycode('A' as u32), 'A' as u32);

        // Test lowercase 'a' -> 'A'
        assert_eq!(keysym_to_js_keycode('a' as u32), 'A' as u32);

        // Test Enter
        assert_eq!(keysym_to_js_keycode(0xFF0D), 13);

        // Test F1
        assert_eq!(keysym_to_js_keycode(0xFFBE), 112);

        // Test backtick
        assert_eq!(keysym_to_js_keycode('`' as u32), 192);
    }

    #[test]
    fn test_keyboard_state() {
        let mut state = KeyboardState::new();

        // Press Ctrl
        state.set_pressed(KEYSYM_CTRL_LEFT, true);
        assert!(state.is_pressed(KEYSYM_CTRL_LEFT));
        assert_eq!(state.pressed_count(), 1);
        assert_eq!(
            state.get_modifiers() & CefEventFlags::CONTROL_DOWN,
            CefEventFlags::CONTROL_DOWN
        );

        // Press 'A'
        state.set_pressed('A' as u32, true);
        assert_eq!(state.pressed_count(), 2);

        // Release Ctrl
        state.set_pressed(KEYSYM_CTRL_LEFT, false);
        assert!(!state.is_pressed(KEYSYM_CTRL_LEFT));
        assert_eq!(state.pressed_count(), 1);
    }

    #[test]
    fn test_mouse_state() {
        let state = MouseState::new();

        let modifiers = state.get_mouse_modifiers(MOUSE_BUTTON_LEFT | MOUSE_BUTTON_RIGHT);
        assert_eq!(
            modifiers & CefEventFlags::LEFT_MOUSE_BUTTON,
            CefEventFlags::LEFT_MOUSE_BUTTON
        );
        assert_eq!(
            modifiers & CefEventFlags::RIGHT_MOUSE_BUTTON,
            CefEventFlags::RIGHT_MOUSE_BUTTON
        );
    }

    #[test]
    fn test_input_handler_shortcut() {
        let mut handler = RbiInputHandler::new();

        // Press Ctrl
        handler.handle_keyboard(KEYSYM_CTRL_LEFT, true);

        // Press 'C' - we need to handle the key first so it's tracked
        handler.handle_keyboard('c' as u32, true);

        // Now check shortcut - should detect Copy
        let shortcut = handler.check_shortcut('c' as u32, true);
        assert_eq!(shortcut, Some(KeyboardShortcut::Copy));

        // Release 'C'
        handler.handle_keyboard('c' as u32, false);

        // Press 'V'
        handler.handle_keyboard('v' as u32, true);

        // Check shortcut - should detect Paste
        let shortcut = handler.check_shortcut('v' as u32, true);
        assert_eq!(shortcut, Some(KeyboardShortcut::Paste));
    }

    #[test]
    fn test_unicode_conversion() {
        assert_eq!(keysym_to_unicode(0xFF0D), Some('\r'));
        assert_eq!(keysym_to_unicode(0xFF09), Some('\t'));
        assert_eq!(keysym_to_unicode(0xFFB5), Some('5')); // Keypad 5
        assert_eq!(keysym_to_unicode('a' as u32), Some('a'));
    }
}
