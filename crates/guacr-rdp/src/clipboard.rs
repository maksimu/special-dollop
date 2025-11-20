// RDP clipboard handling with configurable buffer size
// Lessons learned from kcm patches: KCM-405

use bytes::Bytes;
use guacr_protocol::GuacamoleParser;
use log::{info, warn};
use std::sync::{Arc, Mutex};

/// Minimum clipboard buffer size (256KB)
/// Matches GUAC_COMMON_CLIPBOARD_MIN_LENGTH from kcm patches
pub const CLIPBOARD_MIN_SIZE: usize = 256 * 1024;

/// Maximum clipboard buffer size (50MB)
/// Matches GUAC_COMMON_CLIPBOARD_MAX_LENGTH from kcm patches
/// "This should be enough for a raw 4K picture and even more."
pub const CLIPBOARD_MAX_SIZE: usize = 50 * 1024 * 1024;

/// Default clipboard buffer size (256KB)
pub const CLIPBOARD_DEFAULT_SIZE: usize = CLIPBOARD_MIN_SIZE;

/// RDP clipboard handler
///
/// Handles clipboard synchronization between Guacamole client and RDP server.
/// Supports configurable buffer size (256KB - 50MB) as per kcm patches.
pub struct RdpClipboard {
    buffer: Arc<Mutex<Vec<u8>>>,
    buffer_size: usize,
    mimetype: String,
    length: usize,
}

impl RdpClipboard {
    /// Create a new RDP clipboard with configurable buffer size
    ///
    /// # Arguments
    ///
    /// * `buffer_size` - Buffer size in bytes (will be clamped to MIN-MAX range)
    ///
    /// # Returns
    ///
    /// Returns clipboard with validated buffer size.
    pub fn new(buffer_size: usize) -> Self {
        // Validate and clamp buffer size (lesson from kcm patches)
        let validated_size = if buffer_size < CLIPBOARD_MIN_SIZE {
            warn!(
                "Clipboard buffer size {} is too small, using minimum {}",
                buffer_size, CLIPBOARD_MIN_SIZE
            );
            CLIPBOARD_MIN_SIZE
        } else if buffer_size > CLIPBOARD_MAX_SIZE {
            warn!(
                "Clipboard buffer size {} exceeds maximum {}, using maximum {}",
                buffer_size, CLIPBOARD_MAX_SIZE, CLIPBOARD_MAX_SIZE
            );
            CLIPBOARD_MAX_SIZE
        } else {
            buffer_size
        };

        Self {
            buffer: Arc::new(Mutex::new(vec![0u8; validated_size])),
            buffer_size: validated_size,
            mimetype: String::new(),
            length: 0,
        }
    }

    /// Get the configured buffer size
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Handle clipboard data from Guacamole client
    ///
    /// Processes clipboard instruction and prepares data for RDP server.
    pub fn handle_client_clipboard(
        &mut self,
        instruction: &Bytes,
    ) -> Result<Option<Vec<u8>>, String> {
        let instr = GuacamoleParser::parse_instruction(instruction)
            .map_err(|e| format!("Failed to parse clipboard instruction: {}", e))?;

        if instr.opcode != "clipboard" {
            return Ok(None);
        }

        // Clipboard format: clipboard,<mimetype_len>.<mimetype>,<data_len>.<data>;
        if instr.args.len() < 2 {
            return Err("Invalid clipboard instruction format".to_string());
        }

        let mimetype = instr.args[0];
        let data = instr.args[1];

        // Validate data size
        if data.len() > self.buffer_size {
            return Err(format!(
                "Clipboard data size {} exceeds buffer size {}",
                data.len(),
                self.buffer_size
            ));
        }

        // Store clipboard data
        let mut buffer = self.buffer.lock().unwrap();
        buffer[..data.len()].copy_from_slice(data.as_bytes());
        self.length = data.len();
        self.mimetype = mimetype.to_string();

        info!(
            "RDP: Clipboard data received: {} bytes, mimetype: {}",
            self.length, self.mimetype
        );

        // Return data for RDP server (CLIPRDR format)
        Ok(Some(buffer[..self.length].to_vec()))
    }

    /// Handle clipboard data from RDP server
    ///
    /// Processes RDP clipboard data and prepares Guacamole instruction.
    pub fn handle_server_clipboard(&mut self, data: &[u8], format: u32) -> Result<Bytes, String> {
        // Validate data size
        if data.len() > self.buffer_size {
            return Err(format!(
                "RDP clipboard data size {} exceeds buffer size {}",
                data.len(),
                self.buffer_size
            ));
        }

        // Store clipboard data
        let mut buffer = self.buffer.lock().unwrap();
        buffer[..data.len()].copy_from_slice(data);
        self.length = data.len();

        // Determine mimetype based on RDP format
        // CF_TEXT = 1, CF_UNICODETEXT = 13, etc.
        let mimetype = match format {
            1 => "text/plain",  // CF_TEXT
            13 => "text/plain", // CF_UNICODETEXT
            _ => "application/octet-stream",
        };
        self.mimetype = mimetype.to_string();

        info!(
            "RDP: Clipboard data from server: {} bytes, format: 0x{:X}",
            self.length, format
        );

        // Format as Guacamole clipboard instruction
        // Format: clipboard,<mimetype_len>.<mimetype>,<data_len>.<data>;
        let data_str = String::from_utf8_lossy(&buffer[..self.length]);
        let instr = format!(
            "9.clipboard,{}.{},{}.{};",
            mimetype.len(),
            mimetype,
            data_str.len(),
            data_str
        );

        Ok(Bytes::from(instr))
    }

    /// Get current clipboard data
    pub fn get_data(&self) -> Vec<u8> {
        let buffer = self.buffer.lock().unwrap();
        buffer[..self.length].to_vec()
    }

    /// Get current mimetype
    pub fn mimetype(&self) -> &str {
        &self.mimetype
    }
}

impl Default for RdpClipboard {
    fn default() -> Self {
        Self::new(CLIPBOARD_DEFAULT_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_new() {
        let clipboard = RdpClipboard::new(1024 * 1024); // 1MB
        assert_eq!(clipboard.buffer_size(), 1024 * 1024);
    }

    #[test]
    fn test_clipboard_size_validation() {
        // Too small - should use minimum
        let clipboard = RdpClipboard::new(1024);
        assert_eq!(clipboard.buffer_size(), CLIPBOARD_MIN_SIZE);

        // Too large - should use maximum
        let clipboard = RdpClipboard::new(100 * 1024 * 1024);
        assert_eq!(clipboard.buffer_size(), CLIPBOARD_MAX_SIZE);

        // Valid size
        let clipboard = RdpClipboard::new(1024 * 1024);
        assert_eq!(clipboard.buffer_size(), 1024 * 1024);
    }

    #[test]
    fn test_clipboard_default() {
        let clipboard = RdpClipboard::default();
        assert_eq!(clipboard.buffer_size(), CLIPBOARD_DEFAULT_SIZE);
    }
}
