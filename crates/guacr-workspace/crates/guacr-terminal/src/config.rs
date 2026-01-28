// Terminal configuration for SSH and Telnet handlers
//
// Provides guacd-compatible terminal settings including:
// - Font configuration (font-name, font-size)
// - Color schemes (color-scheme)
// - Terminal type (terminal-type)
// - Scrollback buffer size (scrollback)
// - Backspace key handling (backspace)
// - Clipboard buffer size (clipboard-buffer-size)

use crate::clipboard::{CLIPBOARD_DEFAULT_SIZE, CLIPBOARD_MAX_SIZE, CLIPBOARD_MIN_SIZE};
use std::collections::HashMap;

/// Terminal configuration matching guacd parameters
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Font name (guacd: font-name)
    /// Currently only the embedded Noto Sans Mono is supported
    pub font_name: Option<String>,

    /// Font size in points (guacd: font-size, default: 12)
    pub font_size: u32,

    /// Color scheme preset (guacd: color-scheme)
    /// Supported: "black-white", "gray-black", "green-black", "white-black"
    pub color_scheme: ColorScheme,

    /// Terminal type sent to remote (guacd: terminal-type, default: xterm-256color)
    pub terminal_type: String,

    /// Scrollback buffer size in lines (guacd: scrollback, default: 1000)
    pub scrollback_size: usize,

    /// Backspace key code (guacd: backspace, default: 127)
    /// 127 = DEL (default), 8 = BS
    pub backspace_code: u8,

    /// Clipboard buffer size in bytes (guacd: clipboard-buffer-size, default: 256KB)
    /// Range: 256KB - 50MB (as per KCM patches)
    pub clipboard_buffer_size: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_name: None,
            font_size: 12,
            color_scheme: ColorScheme::default(),
            terminal_type: "xterm-256color".to_string(),
            scrollback_size: 1000,
            backspace_code: 127, // DEL
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,
        }
    }
}

impl TerminalConfig {
    /// Create a TerminalConfig from guacd-compatible parameters
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let mut config = Self::default();

        // Font name
        if let Some(name) = params.get("font-name") {
            if !name.is_empty() {
                config.font_name = Some(name.clone());
            }
        }

        // Font size (default: 12)
        if let Some(size) = params.get("font-size") {
            if let Ok(s) = size.parse::<u32>() {
                // Clamp to reasonable range
                config.font_size = s.clamp(6, 72);
            }
        }

        // Color scheme (support both kebab-case and camelCase)
        if let Some(scheme) = params
            .get("color-scheme")
            .or_else(|| params.get("colorScheme"))
        {
            config.color_scheme = ColorScheme::from_name(scheme);
        }

        // Terminal type (default: xterm-256color)
        if let Some(term_type) = params.get("terminal-type") {
            if !term_type.is_empty() {
                config.terminal_type = term_type.clone();
            }
        }

        // Scrollback buffer size (default: 1000)
        if let Some(scrollback) = params.get("scrollback") {
            if let Ok(s) = scrollback.parse::<usize>() {
                // Clamp to reasonable range (0 to 10000)
                config.scrollback_size = s.min(10000);
            }
        }

        // Backspace key code (default: 127)
        if let Some(backspace) = params.get("backspace") {
            config.backspace_code = match backspace.as_str() {
                "127" | "DEL" | "del" => 127,
                "8" | "BS" | "bs" => 8,
                _ => 127, // Default to DEL
            };
        }

        // Clipboard buffer size (default: 256KB, range: 256KB - 50MB)
        // Inspired by KCM-405 patch for configurable clipboard limits
        if let Some(size_str) = params.get("clipboard-buffer-size") {
            if let Ok(size) = size_str.parse::<usize>() {
                if size < CLIPBOARD_MIN_SIZE {
                    log::warn!(
                        "Clipboard buffer size {} is below minimum {}, using minimum",
                        size,
                        CLIPBOARD_MIN_SIZE
                    );
                    config.clipboard_buffer_size = CLIPBOARD_MIN_SIZE;
                } else if size > CLIPBOARD_MAX_SIZE {
                    log::warn!(
                        "Clipboard buffer size {} exceeds maximum {}, using maximum",
                        size,
                        CLIPBOARD_MAX_SIZE
                    );
                    config.clipboard_buffer_size = CLIPBOARD_MAX_SIZE;
                } else {
                    config.clipboard_buffer_size = size;
                }
            }
        }

        config
    }

    /// Get the terminal type for PTY request
    pub fn term_type(&self) -> &str {
        &self.terminal_type
    }
}

/// Color scheme preset
///
/// Defines foreground/background colors for terminal rendering.
/// Matches guacd's supported color schemes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorScheme {
    /// Foreground color (RGB)
    pub foreground: [u8; 3],
    /// Background color (RGB)
    pub background: [u8; 3],
    /// Name of this scheme
    name: &'static str,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self::GRAY_BLACK
    }
}

impl ColorScheme {
    /// Black text on white background
    pub const BLACK_WHITE: Self = Self {
        foreground: [0, 0, 0],
        background: [255, 255, 255],
        name: "black-white",
    };

    /// Gray text on black background (default)
    pub const GRAY_BLACK: Self = Self {
        foreground: [229, 229, 229],
        background: [0, 0, 0],
        name: "gray-black",
    };

    /// Green text on black background (classic terminal)
    pub const GREEN_BLACK: Self = Self {
        foreground: [0, 255, 0],
        background: [0, 0, 0],
        name: "green-black",
    };

    /// White text on black background
    pub const WHITE_BLACK: Self = Self {
        foreground: [255, 255, 255],
        background: [0, 0, 0],
        name: "white-black",
    };

    /// Parse color scheme from name string
    ///
    /// Supports guacd color scheme names:
    /// - "black-white" - Black text on white background
    /// - "gray-black" - Gray text on black background (default)
    /// - "green-black" - Green text on black background
    /// - "white-black" - White text on black background
    ///
    /// Also supports custom "foreground;background" format where each color is
    /// specified as "R,G,B" (e.g., "255,255,255;0,0,0" for white on black)
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "black-white" => Self::BLACK_WHITE,
            "gray-black" => Self::GRAY_BLACK,
            "green-black" => Self::GREEN_BLACK,
            "white-black" => Self::WHITE_BLACK,
            _ => {
                // Try to parse custom format: "R,G,B;R,G,B"
                if let Some(scheme) = Self::parse_custom(name) {
                    scheme
                } else {
                    // Default to gray-black
                    Self::GRAY_BLACK
                }
            }
        }
    }

    /// Parse custom color format: "foreground;background" where each is "R,G,B"
    fn parse_custom(spec: &str) -> Option<Self> {
        let parts: Vec<&str> = spec.split(';').collect();
        if parts.len() != 2 {
            return None;
        }

        let fg = Self::parse_rgb(parts[0])?;
        let bg = Self::parse_rgb(parts[1])?;

        Some(Self {
            foreground: fg,
            background: bg,
            name: "custom",
        })
    }

    /// Parse RGB color from "R,G,B" format
    fn parse_rgb(s: &str) -> Option<[u8; 3]> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 3 {
            return None;
        }

        let r = parts[0].trim().parse().ok()?;
        let g = parts[1].trim().parse().ok()?;
        let b = parts[2].trim().parse().ok()?;

        Some([r, g, b])
    }

    /// Get the scheme name
    pub fn name(&self) -> &str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TerminalConfig::default();
        assert_eq!(config.font_size, 12);
        assert_eq!(config.terminal_type, "xterm-256color");
        assert_eq!(config.scrollback_size, 1000);
        assert_eq!(config.backspace_code, 127);
        assert_eq!(config.color_scheme, ColorScheme::GRAY_BLACK);
    }

    #[test]
    fn test_from_params() {
        let mut params = HashMap::new();
        params.insert("font-name".to_string(), "Consolas".to_string());
        params.insert("font-size".to_string(), "14".to_string());
        params.insert("color-scheme".to_string(), "green-black".to_string());
        params.insert("terminal-type".to_string(), "xterm".to_string());
        params.insert("scrollback".to_string(), "500".to_string());
        params.insert("backspace".to_string(), "8".to_string());

        let config = TerminalConfig::from_params(&params);
        assert_eq!(config.font_name, Some("Consolas".to_string()));
        assert_eq!(config.font_size, 14);
        assert_eq!(config.color_scheme, ColorScheme::GREEN_BLACK);
        assert_eq!(config.terminal_type, "xterm");
        assert_eq!(config.scrollback_size, 500);
        assert_eq!(config.backspace_code, 8);
    }

    #[test]
    fn test_font_size_clamping() {
        let mut params = HashMap::new();

        // Too small
        params.insert("font-size".to_string(), "2".to_string());
        let config = TerminalConfig::from_params(&params);
        assert_eq!(config.font_size, 6);

        // Too large
        params.insert("font-size".to_string(), "100".to_string());
        let config = TerminalConfig::from_params(&params);
        assert_eq!(config.font_size, 72);
    }

    #[test]
    fn test_scrollback_clamping() {
        let mut params = HashMap::new();
        params.insert("scrollback".to_string(), "999999".to_string());
        let config = TerminalConfig::from_params(&params);
        assert_eq!(config.scrollback_size, 10000);
    }

    #[test]
    fn test_color_scheme_from_name() {
        assert_eq!(
            ColorScheme::from_name("black-white"),
            ColorScheme::BLACK_WHITE
        );
        assert_eq!(
            ColorScheme::from_name("gray-black"),
            ColorScheme::GRAY_BLACK
        );
        assert_eq!(
            ColorScheme::from_name("green-black"),
            ColorScheme::GREEN_BLACK
        );
        assert_eq!(
            ColorScheme::from_name("white-black"),
            ColorScheme::WHITE_BLACK
        );

        // Case insensitive
        assert_eq!(
            ColorScheme::from_name("BLACK-WHITE"),
            ColorScheme::BLACK_WHITE
        );

        // Unknown defaults to gray-black
        assert_eq!(ColorScheme::from_name("unknown"), ColorScheme::GRAY_BLACK);
    }

    #[test]
    fn test_custom_color_scheme() {
        let scheme = ColorScheme::from_name("255,0,0;0,0,255");
        assert_eq!(scheme.foreground, [255, 0, 0]);
        assert_eq!(scheme.background, [0, 0, 255]);
    }

    #[test]
    fn test_backspace_variants() {
        let mut params = HashMap::new();

        params.insert("backspace".to_string(), "127".to_string());
        assert_eq!(TerminalConfig::from_params(&params).backspace_code, 127);

        params.insert("backspace".to_string(), "DEL".to_string());
        assert_eq!(TerminalConfig::from_params(&params).backspace_code, 127);

        params.insert("backspace".to_string(), "8".to_string());
        assert_eq!(TerminalConfig::from_params(&params).backspace_code, 8);

        params.insert("backspace".to_string(), "BS".to_string());
        assert_eq!(TerminalConfig::from_params(&params).backspace_code, 8);
    }
}
