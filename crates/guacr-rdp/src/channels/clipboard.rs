//! Clipboard channel handler (cliprdr)
//!
//! Implements clipboard synchronization between the RDP server and Guacamole client.

use crate::error::Result;
use bytes::Bytes;
use log::{debug, trace};

/// Clipboard data format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardFormat {
    /// Plain text (UTF-8)
    Text,
    /// Unicode text (UTF-16)
    UnicodeText,
    /// HTML content
    Html,
    /// Bitmap image
    Bitmap,
    /// File list
    FileList,
    /// Unknown format
    Unknown(u32),
}

impl ClipboardFormat {
    /// Convert from Windows clipboard format ID
    pub fn from_format_id(id: u32) -> Self {
        match id {
            1 => ClipboardFormat::Text,         // CF_TEXT
            13 => ClipboardFormat::UnicodeText, // CF_UNICODETEXT
            2 => ClipboardFormat::Bitmap,       // CF_BITMAP
            _ => ClipboardFormat::Unknown(id),
        }
    }

    /// Convert to Windows clipboard format ID
    pub fn to_format_id(self) -> u32 {
        match self {
            ClipboardFormat::Text => 1,
            ClipboardFormat::UnicodeText => 13,
            ClipboardFormat::Html => 49390, // Registered format
            ClipboardFormat::Bitmap => 2,
            ClipboardFormat::FileList => 49159, // CF_HDROP as registered format
            ClipboardFormat::Unknown(id) => id,
        }
    }
}

/// Clipboard handler for RDP sessions
pub struct ClipboardHandler {
    /// Maximum clipboard size in bytes
    max_size: usize,
    /// Whether copy from server is disabled
    disable_copy: bool,
    /// Whether paste to server is disabled
    disable_paste: bool,
    /// Current clipboard content from server
    server_content: Option<Bytes>,
    /// Current clipboard content from client
    client_content: Option<String>,
}

impl ClipboardHandler {
    /// Create a new clipboard handler
    pub fn new(max_size: usize, disable_copy: bool, disable_paste: bool) -> Self {
        Self {
            max_size,
            disable_copy,
            disable_paste,
            server_content: None,
            client_content: None,
        }
    }

    /// Handle clipboard data received from the RDP server
    pub fn handle_server_clipboard(
        &mut self,
        format: ClipboardFormat,
        data: &[u8],
    ) -> Result<Option<String>> {
        if self.disable_copy {
            debug!("RDP: Clipboard copy disabled, ignoring server clipboard");
            return Ok(None);
        }

        // Check size limit
        if data.len() > self.max_size {
            debug!(
                "RDP: Clipboard data too large ({} > {}), truncating",
                data.len(),
                self.max_size
            );
        }

        let text = match format {
            ClipboardFormat::Text => {
                // ASCII/Latin-1 text
                String::from_utf8_lossy(&data[..data.len().min(self.max_size)]).into_owned()
            }
            ClipboardFormat::UnicodeText => {
                // UTF-16LE text
                let u16_data: Vec<u16> = data
                    .chunks_exact(2)
                    .take(self.max_size / 2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&c| c != 0) // Stop at null terminator
                    .collect();
                String::from_utf16_lossy(&u16_data)
            }
            ClipboardFormat::Html => {
                // HTML is typically UTF-8
                String::from_utf8_lossy(&data[..data.len().min(self.max_size)]).into_owned()
            }
            _ => {
                trace!("RDP: Unsupported clipboard format {:?}", format);
                return Ok(None);
            }
        };

        self.server_content = Some(Bytes::from(text.clone()));
        Ok(Some(text))
    }

    /// Set clipboard content from client to send to server
    pub fn set_client_clipboard(&mut self, text: String) -> Result<()> {
        if self.disable_paste {
            debug!("RDP: Clipboard paste disabled, ignoring client clipboard");
            return Ok(());
        }

        // Check size limit
        if text.len() > self.max_size {
            debug!(
                "RDP: Client clipboard too large ({} > {}), truncating",
                text.len(),
                self.max_size
            );
            self.client_content = Some(text[..self.max_size].to_string());
        } else {
            self.client_content = Some(text);
        }

        Ok(())
    }

    /// Get clipboard content to send to server (UTF-16LE encoded)
    pub fn get_clipboard_for_server(&self) -> Option<Vec<u8>> {
        self.client_content.as_ref().map(|text| {
            // Convert to UTF-16LE with null terminator
            let mut data: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            data.extend_from_slice(&[0, 0]); // Null terminator
            data
        })
    }

    /// Clear clipboard state
    pub fn clear(&mut self) {
        self.server_content = None;
        self.client_content = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_format_from_id() {
        assert_eq!(ClipboardFormat::from_format_id(1), ClipboardFormat::Text);
        assert_eq!(
            ClipboardFormat::from_format_id(13),
            ClipboardFormat::UnicodeText
        );
        assert_eq!(ClipboardFormat::from_format_id(2), ClipboardFormat::Bitmap);
        assert_eq!(
            ClipboardFormat::from_format_id(999),
            ClipboardFormat::Unknown(999)
        );
    }

    #[test]
    fn test_clipboard_format_to_id() {
        assert_eq!(ClipboardFormat::Text.to_format_id(), 1);
        assert_eq!(ClipboardFormat::UnicodeText.to_format_id(), 13);
        assert_eq!(ClipboardFormat::Bitmap.to_format_id(), 2);
        assert_eq!(ClipboardFormat::Unknown(42).to_format_id(), 42);
    }

    #[test]
    fn test_clipboard_handler_creation() {
        let handler = ClipboardHandler::new(1024, false, false);
        assert!(handler.get_clipboard_for_server().is_none());
    }

    #[test]
    fn test_clipboard_set_client_content() {
        let mut handler = ClipboardHandler::new(1024, false, false);
        handler.set_client_clipboard("Hello".to_string()).unwrap();

        let data = handler.get_clipboard_for_server().unwrap();
        // "Hello" in UTF-16LE + null terminator
        // H=0x0048, e=0x0065, l=0x006C, l=0x006C, o=0x006F, \0=0x0000
        assert_eq!(data.len(), 12); // 5 chars * 2 bytes + 2 bytes null
        assert_eq!(&data[0..2], &[0x48, 0x00]); // 'H'
        assert_eq!(&data[10..12], &[0x00, 0x00]); // null terminator
    }

    #[test]
    fn test_clipboard_paste_disabled() {
        let mut handler = ClipboardHandler::new(1024, false, true); // paste disabled
        handler.set_client_clipboard("Test".to_string()).unwrap();

        // Content should not be set when paste is disabled
        assert!(handler.get_clipboard_for_server().is_none());
    }

    #[test]
    fn test_clipboard_copy_disabled() {
        let mut handler = ClipboardHandler::new(1024, true, false); // copy disabled

        let result = handler
            .handle_server_clipboard(ClipboardFormat::Text, b"Hello")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_clipboard_handle_ascii_text() {
        let mut handler = ClipboardHandler::new(1024, false, false);

        let result = handler
            .handle_server_clipboard(ClipboardFormat::Text, b"Hello World")
            .unwrap();
        assert_eq!(result, Some("Hello World".to_string()));
    }

    #[test]
    fn test_clipboard_handle_unicode_text() {
        let mut handler = ClipboardHandler::new(1024, false, false);

        // "Hi" in UTF-16LE: H=0x48,0x00 i=0x69,0x00 \0=0x00,0x00
        let utf16_data = [0x48, 0x00, 0x69, 0x00, 0x00, 0x00];

        let result = handler
            .handle_server_clipboard(ClipboardFormat::UnicodeText, &utf16_data)
            .unwrap();
        assert_eq!(result, Some("Hi".to_string()));
    }

    #[test]
    fn test_clipboard_size_limit() {
        let mut handler = ClipboardHandler::new(5, false, false); // only 5 bytes

        // Try to set 10 chars - should be truncated
        handler
            .set_client_clipboard("0123456789".to_string())
            .unwrap();

        // Content should be truncated
        let content = handler.client_content.as_ref().unwrap();
        assert_eq!(content.len(), 5);
        assert_eq!(content, "01234");
    }

    #[test]
    fn test_clipboard_clear() {
        let mut handler = ClipboardHandler::new(1024, false, false);
        handler.set_client_clipboard("Test".to_string()).unwrap();
        handler
            .handle_server_clipboard(ClipboardFormat::Text, b"Server")
            .unwrap();

        handler.clear();

        assert!(handler.get_clipboard_for_server().is_none());
        assert!(handler.server_content.is_none());
    }

    #[test]
    fn test_clipboard_unsupported_format() {
        let mut handler = ClipboardHandler::new(1024, false, false);

        // Bitmap format returns None (not text)
        let result = handler
            .handle_server_clipboard(ClipboardFormat::Bitmap, &[0, 1, 2, 3])
            .unwrap();
        assert!(result.is_none());
    }
}
