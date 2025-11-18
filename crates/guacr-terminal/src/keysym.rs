// X11 keysym to terminal input byte conversion

/// Convert X11 keysym to terminal input bytes
///
/// Guacamole protocol uses X11 keysyms for keyboard input.
/// This function converts them to the appropriate bytes to send to a terminal.
pub fn x11_keysym_to_bytes(keysym: u32, pressed: bool) -> Vec<u8> {
    // Only handle key press events
    if !pressed {
        return Vec::new();
    }

    match keysym {
        // Return/Enter
        0xFF0D => vec![b'\r'],

        // Backspace
        0xFF08 => vec![0x7F],

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii_characters() {
        assert_eq!(x11_keysym_to_bytes(0x0041, true), vec![b'A']);
        assert_eq!(x11_keysym_to_bytes(0x0061, true), vec![b'a']);
        assert_eq!(x11_keysym_to_bytes(0x0030, true), vec![b'0']);
        assert_eq!(x11_keysym_to_bytes(0x0020, true), vec![b' ']);
    }

    #[test]
    fn test_special_keys() {
        assert_eq!(x11_keysym_to_bytes(0xFF0D, true), vec![b'\r']); // Enter
        assert_eq!(x11_keysym_to_bytes(0xFF08, true), vec![0x7F]); // Backspace
        assert_eq!(x11_keysym_to_bytes(0xFF09, true), vec![b'\t']); // Tab
        assert_eq!(x11_keysym_to_bytes(0xFF1B, true), vec![0x1B]); // Escape
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(x11_keysym_to_bytes(0xFF51, true), vec![0x1B, b'[', b'D']); // Left
        assert_eq!(x11_keysym_to_bytes(0xFF52, true), vec![0x1B, b'[', b'A']); // Up
        assert_eq!(x11_keysym_to_bytes(0xFF53, true), vec![0x1B, b'[', b'C']); // Right
        assert_eq!(x11_keysym_to_bytes(0xFF54, true), vec![0x1B, b'[', b'B']); // Down
    }

    #[test]
    fn test_function_keys() {
        assert_eq!(x11_keysym_to_bytes(0xFFBE, true), vec![0x1B, b'O', b'P']); // F1
        assert_eq!(x11_keysym_to_bytes(0xFFBF, true), vec![0x1B, b'O', b'Q']); // F2
    }

    #[test]
    fn test_key_release_ignored() {
        assert_eq!(x11_keysym_to_bytes(0x0041, false), Vec::<u8>::new()); // A released
    }

    #[test]
    fn test_unsupported_key() {
        assert_eq!(x11_keysym_to_bytes(0xFFFF + 1, true), Vec::<u8>::new());
    }
}
