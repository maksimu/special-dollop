// X11 keysym to terminal input byte conversion

/// Modifier key state tracker
///
/// Tracks which modifier keys are currently pressed (Control, Shift, Alt, Meta/Command)
#[derive(Debug, Default, Clone)]
pub struct ModifierState {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool, // Command key on Mac, Windows key on PC
}

impl ModifierState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update modifier state based on keysym
    ///
    /// Returns true if this was a modifier key that was handled
    pub fn update_modifier(&mut self, keysym: u32, pressed: bool) -> bool {
        match keysym {
            // Control keys
            0xFFE3 | 0xFFE4 => {
                self.control = pressed;
                true
            }
            // Shift keys
            0xFFE1 | 0xFFE2 => {
                self.shift = pressed;
                true
            }
            // Alt keys
            0xFFE9 | 0xFFEA => {
                self.alt = pressed;
                true
            }
            // Meta/Command keys (0xFFE7 = left meta, 0xFFE8 = right meta)
            0xFFE7 | 0xFFE8 => {
                self.meta = pressed;
                true
            }
            _ => false,
        }
    }
}

/// Convert X11 keysym to terminal input bytes
///
/// Guacamole protocol uses X11 keysyms for keyboard input.
/// This function converts them to the appropriate bytes to send to a terminal.
///
/// Uses default backspace code (127 = DEL).
///
/// # Arguments
///
/// * `keysym` - X11 keysym value
/// * `pressed` - Whether the key is pressed (true) or released (false)
/// * `modifiers` - Optional modifier state for control character handling
pub fn x11_keysym_to_bytes(
    keysym: u32,
    pressed: bool,
    modifiers: Option<&ModifierState>,
) -> Vec<u8> {
    x11_keysym_to_bytes_with_backspace(keysym, pressed, modifiers, 127)
}

/// Convert X11 keysym to terminal input bytes with configurable backspace
///
/// Like `x11_keysym_to_bytes` but allows specifying the backspace code.
///
/// # Arguments
///
/// * `keysym` - X11 keysym value
/// * `pressed` - Whether the key is pressed (true) or released (false)
/// * `modifiers` - Optional modifier state for control character handling
/// * `backspace_code` - Code to send for backspace key (127 = DEL, 8 = BS)
pub fn x11_keysym_to_bytes_with_backspace(
    keysym: u32,
    pressed: bool,
    modifiers: Option<&ModifierState>,
    backspace_code: u8,
) -> Vec<u8> {
    // Only handle key press events
    if !pressed {
        return Vec::new();
    }

    // Handle control character combinations
    if let Some(mods) = modifiers {
        if mods.control {
            // Control + character combinations
            // X11 keysyms: uppercase A-Z = 0x0041-0x005A, lowercase a-z = 0x0061-0x007A
            // Browser sends LOWERCASE keysyms when typing Ctrl+C (keysym 99 = 'c')
            match keysym {
                // Control + A-Z (uppercase): convert to 0x01-0x1A
                0x0041..=0x005A => {
                    // A-Z: subtract 0x40 to get control character
                    // Ctrl+A (0x41) -> 0x01, Ctrl+C (0x43) -> 0x03, etc.
                    return vec![(keysym - 0x40) as u8];
                }
                // Control + a-z (lowercase): convert to 0x01-0x1A
                // This is the common case - browsers send lowercase when Ctrl is held
                0x0061..=0x007A => {
                    // a-z: subtract 0x60 to get control character
                    // Ctrl+a (0x61) -> 0x01, Ctrl+c (0x63) -> 0x03, etc.
                    return vec![(keysym - 0x60) as u8];
                }
                // Control + [ (0x5B) -> ESC (0x1B)
                0x005B => return vec![0x1B],
                // Control + \ (0x5C) -> FS (0x1C)
                0x005C => return vec![0x1C],
                // Control + ] (0x5D) -> GS (0x1D)
                0x005D => return vec![0x1D],
                // Control + ^ (0x5E) -> RS (0x1E)
                0x005E => return vec![0x1E],
                // Control + _ (0x5F) -> US (0x1F)
                0x005F => return vec![0x1F],
                // Control + @ (0x40) -> NUL (0x00)
                0x0040 => return vec![0x00],
                // Control + Space (0x20) -> NUL (0x00) - some terminals use this
                0x0020 if mods.control => return vec![0x00],
                _ => {}
            }
        }
    }

    match keysym {
        // Return/Enter
        0xFF0D => vec![b'\r'],

        // Backspace - use configurable code (127 = DEL, 8 = BS)
        0xFF08 => vec![backspace_code],

        // Tab
        0xFF09 => vec![b'\t'],

        // Escape
        0xFF1B => vec![0x1B],

        // Arrow keys (VT100 sequences)
        0xFF51 => vec![0x1B, b'[', b'D'], // Left
        0xFF52 => vec![0x1B, b'[', b'A'], // Up
        0xFF53 => vec![0x1B, b'[', b'C'], // Right
        0xFF54 => vec![0x1B, b'[', b'B'], // Down

        // Home/End
        0xFF50 => vec![0x1B, b'[', b'H'], // Home
        0xFF57 => vec![0x1B, b'[', b'F'], // End

        // Page Up/Down
        0xFF55 => vec![0x1B, b'[', b'5', b'~'], // Page Up
        0xFF56 => vec![0x1B, b'[', b'6', b'~'], // Page Down

        // Insert/Delete
        0xFF63 => vec![0x1B, b'[', b'2', b'~'], // Insert
        0xFFFF => vec![0x1B, b'[', b'3', b'~'], // Delete

        // Function keys F1-F12
        0xFFBE => vec![0x1B, b'O', b'P'],             // F1
        0xFFBF => vec![0x1B, b'O', b'Q'],             // F2
        0xFFC0 => vec![0x1B, b'O', b'R'],             // F3
        0xFFC1 => vec![0x1B, b'O', b'S'],             // F4
        0xFFC2 => vec![0x1B, b'[', b'1', b'5', b'~'], // F5
        0xFFC3 => vec![0x1B, b'[', b'1', b'7', b'~'], // F6
        0xFFC4 => vec![0x1B, b'[', b'1', b'8', b'~'], // F7
        0xFFC5 => vec![0x1B, b'[', b'1', b'9', b'~'], // F8
        0xFFC6 => vec![0x1B, b'[', b'2', b'0', b'~'], // F9
        0xFFC7 => vec![0x1B, b'[', b'2', b'1', b'~'], // F10
        0xFFC8 => vec![0x1B, b'[', b'2', b'3', b'~'], // F11
        0xFFC9 => vec![0x1B, b'[', b'2', b'4', b'~'], // F12

        // ASCII printable characters (0x0020 - 0x007E) - must come after special keys
        0x0020..=0x007E => vec![keysym as u8],

        // Unsupported key
        _ => Vec::new(),
    }
}

/// Convert mouse event to X11 mouse escape sequence for terminal mouse support
///
/// Terminal applications (vim, tmux) use X11 mouse escape sequences:
/// `ESC [ M <button> <x> <y>`
///
/// Where:
/// - button: encodes button (0-2) + action (32 for drag, 35 for release)
/// - x, y: character cell coordinates (1-based, +32 to make printable)
///
/// # Arguments
///
/// * `x_px` - X coordinate in pixels
/// * `y_px` - Y coordinate in pixels
/// * `button_mask` - Button mask: 0=move, 1=left, 2=middle, 4=right, 32=drag
/// * `char_width` - Width of character cell in pixels
/// * `char_height` - Height of character cell in pixels
///
/// # Returns
///
/// X11 mouse escape sequence bytes, or empty Vec if invalid
pub fn mouse_event_to_x11_sequence(
    x_px: u32,
    y_px: u32,
    button_mask: u8,
    char_width: u32,
    char_height: u32,
) -> Vec<u8> {
    // Convert pixel coordinates to character cell coordinates (0-based)
    let col = (x_px / char_width.max(1)) as u8;
    let row = (y_px / char_height.max(1)) as u8;

    // X11 mouse protocol uses 1-based coordinates + 32 to make them printable ASCII
    let x_char = col.saturating_add(33); // +1 for 1-based, +32 for printable
    let y_char = row.saturating_add(33);

    // Determine button and action
    // Button mask: 0=move, 1=left, 2=middle, 4=right, 32=drag
    let is_drag = (button_mask & 32) != 0;
    let button = button_mask & 0x07; // Extract button (0-7)

    // Encode button: 0=left, 1=middle, 2=right
    // Action: 0=press, 3=release, 32=drag
    // First map button: 1->0 (left), 2->1 (middle), 4->2 (right)
    let mapped_button = match button {
        1 => 0, // Left
        2 => 1, // Middle
        4 => 2, // Right
        0 => {
            // Move (no button pressed)
            if is_drag {
                return Vec::new(); // Can't drag without a button
            }
            return vec![0x1B, b'[', b'M', 35, x_char, y_char]; // Move/release
        }
        _ => return Vec::new(), // Unknown button
    };

    let button_code = if is_drag {
        // Drag: mapped_button + 32
        mapped_button + 32
    } else {
        // Button press
        mapped_button
    };

    // X11 mouse escape sequence: ESC [ M <button> <x> <y>
    let seq = vec![0x1B, b'[', b'M', button_code, x_char, y_char];

    seq
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_characters() {
        assert_eq!(x11_keysym_to_bytes(0x0041, true, None), vec![b'A']);
        assert_eq!(x11_keysym_to_bytes(0x0061, true, None), vec![b'a']);
        assert_eq!(x11_keysym_to_bytes(0x0030, true, None), vec![b'0']);
        assert_eq!(x11_keysym_to_bytes(0x0020, true, None), vec![b' ']);
    }

    #[test]
    fn test_special_keys() {
        assert_eq!(x11_keysym_to_bytes(0xFF0D, true, None), vec![b'\r']); // Enter
        assert_eq!(x11_keysym_to_bytes(0xFF08, true, None), vec![0x7F]); // Backspace
        assert_eq!(x11_keysym_to_bytes(0xFF09, true, None), vec![b'\t']); // Tab
        assert_eq!(x11_keysym_to_bytes(0xFF1B, true, None), vec![0x1B]); // Escape
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(
            x11_keysym_to_bytes(0xFF51, true, None),
            vec![0x1B, b'[', b'D']
        ); // Left
        assert_eq!(
            x11_keysym_to_bytes(0xFF52, true, None),
            vec![0x1B, b'[', b'A']
        ); // Up
        assert_eq!(
            x11_keysym_to_bytes(0xFF53, true, None),
            vec![0x1B, b'[', b'C']
        ); // Right
        assert_eq!(
            x11_keysym_to_bytes(0xFF54, true, None),
            vec![0x1B, b'[', b'B']
        ); // Down
    }

    #[test]
    fn test_function_keys() {
        assert_eq!(
            x11_keysym_to_bytes(0xFFBE, true, None),
            vec![0x1B, b'O', b'P']
        ); // F1
        assert_eq!(
            x11_keysym_to_bytes(0xFFBF, true, None),
            vec![0x1B, b'O', b'Q']
        ); // F2
    }

    #[test]
    fn test_key_release_ignored() {
        assert_eq!(x11_keysym_to_bytes(0x0041, false, None), Vec::<u8>::new()); // A released
    }

    #[test]
    fn test_unsupported_key() {
        assert_eq!(
            x11_keysym_to_bytes(0xFFFF + 1, true, None),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn test_control_characters() {
        let mut mods = ModifierState::new();
        mods.control = true;

        // Ctrl+C (uppercase C is 0x43, should become 0x03)
        assert_eq!(x11_keysym_to_bytes(0x0043, true, Some(&mods)), vec![0x03]);

        // Ctrl+c (lowercase c is 0x63, should also become 0x03)
        // This is what browsers actually send!
        assert_eq!(x11_keysym_to_bytes(0x0063, true, Some(&mods)), vec![0x03]);

        // Ctrl+A (uppercase A is 0x41, should become 0x01)
        assert_eq!(x11_keysym_to_bytes(0x0041, true, Some(&mods)), vec![0x01]);

        // Ctrl+a (lowercase a is 0x61, should also become 0x01)
        assert_eq!(x11_keysym_to_bytes(0x0061, true, Some(&mods)), vec![0x01]);

        // Ctrl+Z (uppercase Z is 0x5A, should become 0x1A)
        assert_eq!(x11_keysym_to_bytes(0x005A, true, Some(&mods)), vec![0x1A]);

        // Ctrl+z (lowercase z is 0x7A, should also become 0x1A)
        assert_eq!(x11_keysym_to_bytes(0x007A, true, Some(&mods)), vec![0x1A]);

        // Ctrl+[ should become ESC (0x1B)
        assert_eq!(x11_keysym_to_bytes(0x005B, true, Some(&mods)), vec![0x1B]);

        // Ctrl+d (lowercase d is 0x64, should become 0x04 - EOF)
        assert_eq!(x11_keysym_to_bytes(0x0064, true, Some(&mods)), vec![0x04]);
    }

    #[test]
    fn test_modifier_tracking() {
        let mut mods = ModifierState::new();

        // Press left Control
        assert!(mods.update_modifier(0xFFE3, true));
        assert!(mods.control);
        assert!(!mods.shift);

        // Press right Shift
        assert!(mods.update_modifier(0xFFE2, true));
        assert!(mods.control);
        assert!(mods.shift);

        // Release Control
        assert!(mods.update_modifier(0xFFE3, false));
        assert!(!mods.control);
        assert!(mods.shift);

        // Non-modifier key should return false
        assert!(!mods.update_modifier(0x0041, true));
    }

    #[test]
    fn test_mouse_event_left_click() {
        // Left click at character position (10, 5) with 19x38 pixel cells
        let seq = mouse_event_to_x11_sequence(190, 190, 1, 19, 38);

        // Should be: ESC [ M <button> <x> <y>
        // x = 10 + 33 = 43, y = 5 + 33 = 38, button = 0 (left)
        assert_eq!(seq.len(), 6);
        assert_eq!(seq[0], 0x1B); // ESC
        assert_eq!(seq[1], b'[');
        assert_eq!(seq[2], b'M');
        assert_eq!(seq[3], 0); // Left button press
        assert_eq!(seq[4], 43); // x = 10 + 33
        assert_eq!(seq[5], 38); // y = 5 + 33
    }

    #[test]
    fn test_mouse_event_right_click() {
        // Right click at (0, 0) with 19x38 pixel cells
        let seq = mouse_event_to_x11_sequence(0, 0, 4, 19, 38);

        assert_eq!(seq.len(), 6);
        assert_eq!(seq[3], 2); // Right button (4 -> 2)
        assert_eq!(seq[4], 33); // x = 0 + 33
        assert_eq!(seq[5], 33); // y = 0 + 33
    }

    #[test]
    fn test_mouse_event_drag() {
        // Drag with left button at (100, 200) with 19x38 pixel cells
        // Button mask: 1 (left) + 32 (drag) = 33
        let seq = mouse_event_to_x11_sequence(100, 200, 33, 19, 38);

        assert_eq!(seq.len(), 6);
        assert_eq!(seq[3], 32); // Drag: left button (0) + 32 = 32
    }

    #[test]
    fn test_mouse_event_move() {
        // Mouse move (no button) at (50, 50)
        let seq = mouse_event_to_x11_sequence(50, 50, 0, 19, 38);

        assert_eq!(seq.len(), 6);
        assert_eq!(seq[3], 35); // Move/release
    }

    #[test]
    fn test_mouse_event_middle_click() {
        // Middle click at (100, 100)
        let seq = mouse_event_to_x11_sequence(100, 100, 2, 19, 38);

        assert_eq!(seq.len(), 6);
        assert_eq!(seq[3], 1); // Middle button press
    }

    #[test]
    fn test_mouse_event_coordinate_calculation() {
        // Test coordinate conversion
        // At pixel (0, 0) with 19x38 cells: col=0, row=0 -> x_char=33, y_char=33
        let seq = mouse_event_to_x11_sequence(0, 0, 1, 19, 38);
        assert_eq!(seq[4], 33); // x_char = 0 + 33
        assert_eq!(seq[5], 33); // y_char = 0 + 33

        // At pixel (190, 380) with 19x38 cells: col=10, row=10 -> x_char=43, y_char=43
        let seq2 = mouse_event_to_x11_sequence(190, 380, 1, 19, 38);
        assert_eq!(seq2[4], 43); // x_char = 10 + 33
        assert_eq!(seq2[5], 43); // y_char = 10 + 33
    }

    #[test]
    fn test_modifier_state_default() {
        let mods = ModifierState::default();
        assert!(!mods.control);
        assert!(!mods.shift);
        assert!(!mods.alt);
    }

    #[test]
    fn test_modifier_state_all_keys() {
        let mut mods = ModifierState::new();

        // Test all modifier keys
        assert!(mods.update_modifier(0xFFE3, true)); // Left Control
        assert!(mods.update_modifier(0xFFE4, true)); // Right Control (should still work)
        assert!(mods.update_modifier(0xFFE1, true)); // Left Shift
        assert!(mods.update_modifier(0xFFE2, true)); // Right Shift
        assert!(mods.update_modifier(0xFFE9, true)); // Left Alt
        assert!(mods.update_modifier(0xFFEA, true)); // Right Alt

        assert!(mods.control);
        assert!(mods.shift);
        assert!(mods.alt);

        // Release all
        mods.update_modifier(0xFFE3, false);
        mods.update_modifier(0xFFE1, false);
        mods.update_modifier(0xFFE9, false);

        assert!(!mods.control);
        assert!(!mods.shift);
        assert!(!mods.alt);
    }

    #[test]
    fn test_modifier_state_combinations() {
        let mut mods = ModifierState::new();

        // Ctrl+Shift
        mods.update_modifier(0xFFE3, true);
        mods.update_modifier(0xFFE1, true);
        assert!(mods.control && mods.shift && !mods.alt);

        // Add Alt
        mods.update_modifier(0xFFE9, true);
        assert!(mods.control && mods.shift && mods.alt);

        // Release Shift
        mods.update_modifier(0xFFE1, false);
        assert!(mods.control && !mods.shift && mods.alt);
    }
}
