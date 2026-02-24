//! TDS authentication utilities
//!
//! This module provides authentication-related functions for TDS,
//! including password encoding and string conversion utilities.

use zeroize::Zeroize;

/// Encode a password per TDS specification
///
/// TDS uses a simple obfuscation scheme for passwords in LOGIN7:
/// 1. Convert to UTF-16LE
/// 2. For each byte: swap nibbles (upper 4 bits <-> lower 4 bits)
/// 3. XOR with 0xA5
///
/// **Security Note:** This is NOT encryption. It provides no real security.
/// Security comes from TLS encryption and proper credential handling.
///
/// # Arguments
///
/// * `password` - The plaintext password to encode
///
/// # Returns
///
/// Encoded password bytes (UTF-16LE with TDS obfuscation applied)
pub fn encode_password(password: &str) -> Vec<u8> {
    let utf16: Vec<u16> = password.encode_utf16().collect();
    let mut encoded = Vec::with_capacity(utf16.len() * 2);

    for char_code in utf16 {
        let bytes = char_code.to_le_bytes();
        for byte in bytes {
            // Swap nibbles: (high nibble -> low, low nibble -> high)
            let swapped = ((byte << 4) & 0xF0) | ((byte >> 4) & 0x0F);
            // XOR with 0xA5
            encoded.push(swapped ^ 0xA5);
        }
    }

    encoded
}

/// Convert a string to UTF-16LE bytes
///
/// TDS LOGIN7 packet requires all strings to be UTF-16LE encoded.
///
/// # Arguments
///
/// * `s` - The string to convert
///
/// # Returns
///
/// UTF-16LE encoded bytes
pub fn string_to_utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect()
}

/// Convert UTF-16LE bytes back to a string
///
/// # Arguments
///
/// * `bytes` - UTF-16LE encoded bytes
///
/// # Returns
///
/// Decoded string, or None if invalid UTF-16
#[allow(clippy::manual_is_multiple_of)] // is_multiple_of is unstable in Rust 1.85
pub fn utf16le_to_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }

    let u16_chars: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&u16_chars).ok()
}

/// Securely clear password data from memory
///
/// Uses the zeroize crate to ensure password data is zeroed
/// and not optimized away by the compiler.
pub fn zeroize_password(password: &mut Vec<u8>) {
    password.zeroize();
}

/// Securely clear a string from memory
pub fn zeroize_string(s: &mut String) {
    // SAFETY: We're about to zeroize the bytes anyway
    unsafe {
        let bytes = s.as_bytes_mut();
        bytes.zeroize();
    }
    s.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_password_empty() {
        let encoded = encode_password("");
        assert!(encoded.is_empty());
    }

    #[test]
    fn test_encode_password_simple() {
        // Test with "a" (UTF-16LE: 0x61, 0x00)
        // 0x61: swap nibbles -> 0x16, XOR 0xA5 -> 0xB3
        // 0x00: swap nibbles -> 0x00, XOR 0xA5 -> 0xA5
        let encoded = encode_password("a");
        assert_eq!(encoded.len(), 2);
        assert_eq!(encoded[0], 0xB3);
        assert_eq!(encoded[1], 0xA5);
    }

    #[test]
    fn test_encode_password_known_value() {
        // Test with known value from TDS specification
        // Password "foo" should produce specific encoded bytes
        let encoded = encode_password("foo");
        assert_eq!(encoded.len(), 6); // 3 chars * 2 bytes each

        // 'f' (0x66, 0x00):
        // 0x66: swap -> 0x66, XOR 0xA5 -> 0xC3
        // 0x00: swap -> 0x00, XOR 0xA5 -> 0xA5
        assert_eq!(encoded[0], 0xC3);
        assert_eq!(encoded[1], 0xA5);

        // 'o' (0x6F, 0x00):
        // 0x6F: swap -> 0xF6, XOR 0xA5 -> 0x53
        // 0x00: swap -> 0x00, XOR 0xA5 -> 0xA5
        assert_eq!(encoded[2], 0x53);
        assert_eq!(encoded[3], 0xA5);
    }

    #[test]
    fn test_string_to_utf16le() {
        let bytes = string_to_utf16le("AB");
        // 'A' = 0x0041 -> [0x41, 0x00]
        // 'B' = 0x0042 -> [0x42, 0x00]
        assert_eq!(bytes, vec![0x41, 0x00, 0x42, 0x00]);
    }

    #[test]
    fn test_string_to_utf16le_unicode() {
        // Test with a character outside BMP (requires surrogate pair)
        let bytes = string_to_utf16le("\u{1F600}"); // Emoji
        assert_eq!(bytes.len(), 4); // Surrogate pair = 4 bytes
    }

    #[test]
    fn test_utf16le_to_string_roundtrip() {
        let original = "Hello, World!";
        let encoded = string_to_utf16le(original);
        let decoded = utf16le_to_string(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_utf16le_to_string_unicode_roundtrip() {
        let original = "Caf\u{00E9}"; // Café with accented e
        let encoded = string_to_utf16le(original);
        let decoded = utf16le_to_string(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_utf16le_to_string_invalid_length() {
        // Odd number of bytes should fail
        let result = utf16le_to_string(&[0x41, 0x00, 0x42]);
        assert!(result.is_none());
    }

    #[test]
    fn test_zeroize_password() {
        let mut password = vec![0x01, 0x02, 0x03, 0x04];
        zeroize_password(&mut password);
        assert!(password.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_encode_password_unicode() {
        // Test with various unicode characters
        let passwords = [
            "пароль",      // Russian
            "密码",        // Chinese
            "パスワード",  // Japanese
            "كلمة المرور", // Arabic
            "🔒🔑",        // Emoji
        ];

        for password in passwords {
            let encoded = encode_password(password);
            // Should produce non-empty output for non-empty password
            assert!(!encoded.is_empty(), "Failed to encode: {}", password);
            // Each UTF-16 code unit produces 2 bytes
            assert!(encoded.len().is_multiple_of(2));
        }
    }

    #[test]
    fn test_encode_password_special_characters() {
        let password = "P@$$w0rd!#%^&*()_+-=[]{}|;':\",./<>?`~";
        let encoded = encode_password(password);
        assert!(!encoded.is_empty());

        // Encoding should be deterministic
        let encoded2 = encode_password(password);
        assert_eq!(encoded, encoded2);
    }

    #[test]
    fn test_encode_password_long() {
        // Test with a very long password
        let password = "x".repeat(1000);
        let encoded = encode_password(&password);
        assert_eq!(encoded.len(), 2000); // 1000 chars * 2 bytes each
    }

    #[test]
    fn test_utf16le_empty() {
        let bytes = string_to_utf16le("");
        assert!(bytes.is_empty());

        let result = utf16le_to_string(&[]);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn test_zeroize_string() {
        let mut s = String::from("secret password");
        zeroize_string(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn test_encode_password_byte_values() {
        // Test that encoding produces expected byte transformations
        // Character 'A' (0x0041 UTF-16LE: [0x41, 0x00])
        // 0x41: swap nibbles -> 0x14, XOR 0xA5 -> 0xB1
        // 0x00: swap nibbles -> 0x00, XOR 0xA5 -> 0xA5
        let encoded = encode_password("A");
        assert_eq!(encoded.len(), 2);
        assert_eq!(encoded[0], 0xB1);
        assert_eq!(encoded[1], 0xA5);
    }

    #[test]
    fn test_utf16le_to_string_with_invalid_utf16() {
        // Create invalid UTF-16 (unpaired surrogate)
        // High surrogate without low surrogate: 0xD800
        let invalid = [0x00, 0xD8]; // U+D800 is a high surrogate

        // This should return None because it's invalid UTF-16
        let result = utf16le_to_string(&invalid);
        assert!(result.is_none());
    }
}
