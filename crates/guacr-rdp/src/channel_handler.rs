// RDP channel handling (CLIPRDR, AUDIO, RDPGFX, DISP)
// Manages RDP virtual channels for clipboard, audio, graphics acceleration, and display updates

use crate::clipboard::RdpClipboard;
use bytes::Bytes;
// GuacamoleParser not needed here - clipboard handler uses it internally
use log::{debug, info, warn};

/// RDP channel handler
///
/// Manages RDP virtual channels:
/// - CLIPRDR: Clipboard redirection
/// - AUDIO_INPUT/OUTPUT: Audio redirection
/// - RDPGFX: Graphics acceleration
/// - DISP: Display update (dynamic resize)
pub struct RdpChannelHandler {
    clipboard: RdpClipboard,
    disable_copy: bool,
    disable_paste: bool,
}

impl RdpChannelHandler {
    pub fn new(clipboard_buffer_size: usize, disable_copy: bool, disable_paste: bool) -> Self {
        Self {
            clipboard: RdpClipboard::new(clipboard_buffer_size),
            disable_copy,
            disable_paste,
        }
    }

    /// Handle clipboard data from client (paste)
    ///
    /// Processes Guacamole clipboard instruction and prepares RDP CLIPRDR format data.
    pub fn handle_client_clipboard(&mut self, msg: &Bytes) -> Result<Option<CliprdrData>, String> {
        if self.disable_paste {
            debug!("RDP: Clipboard paste disabled, ignoring");
            return Ok(None);
        }

        match self.clipboard.handle_client_clipboard(msg) {
            Ok(Some(rdp_data)) => {
                info!(
                    "RDP: Clipboard data ready for server: {} bytes",
                    rdp_data.len()
                );
                Ok(Some(CliprdrData {
                    format: CliprdrFormat::UnicodeText,
                    data: rdp_data,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                warn!("RDP: Clipboard error: {}", e);
                Err(e)
            }
        }
    }

    /// Handle clipboard data from server (copy)
    ///
    /// Processes RDP CLIPRDR format data and prepares Guacamole clipboard instruction.
    pub fn handle_server_clipboard(
        &mut self,
        format: CliprdrFormat,
        data: &[u8],
    ) -> Result<Option<Bytes>, String> {
        if self.disable_copy {
            debug!("RDP: Clipboard copy disabled, ignoring");
            return Ok(None);
        }

        match format {
            CliprdrFormat::UnicodeText => {
                // Convert RDP clipboard data to Guacamole clipboard instruction
                let text = String::from_utf8(data.to_vec())
                    .map_err(|e| format!("Invalid UTF-8 in clipboard: {}", e))?;

                // Format as Guacamole clipboard instruction
                let instr = format!("5.clipboard,{}.{};", 8, text);
                Ok(Some(Bytes::from(instr)))
            }
            CliprdrFormat::Text => {
                // ASCII text
                let text = String::from_utf8_lossy(data);
                let instr = format!("5.clipboard,{}.{};", 8, text);
                Ok(Some(Bytes::from(instr)))
            }
            CliprdrFormat::Html => {
                // HTML format - send as-is
                use base64::{engine::general_purpose, Engine as _};
                let base64_data = general_purpose::STANDARD.encode(data);
                let instr = format!("5.clipboard,{}.{};", 8, base64_data);
                Ok(Some(Bytes::from(instr)))
            }
            CliprdrFormat::Bitmap => {
                // Bitmap format - convert to PNG or send as base64
                warn!("RDP: Bitmap clipboard format not yet fully supported");
                Ok(None)
            }
            _ => {
                debug!("RDP: Unsupported clipboard format: {:?}", format);
                Ok(None)
            }
        }
    }

    /// Prepare DISP (Display Update) channel message for dynamic resize
    ///
    /// # Arguments
    ///
    /// * `width` - New desktop width
    /// * `height` - New desktop height
    ///
    /// # Returns
    ///
    /// DISP channel message data for sending to RDP server
    pub fn prepare_disp_resize(&self, width: u32, height: u32) -> DispResizeMessage {
        info!("RDP: Preparing DISP resize: {}x{}", width, height);

        DispResizeMessage {
            width: width as u16,
            height: height as u16,
        }
    }

    /// Handle RDPGFX (Graphics Acceleration) channel message
    ///
    /// Processes RDPGFX messages for hardware-accelerated graphics.
    /// This is important for 4K video performance.
    pub fn handle_rdpgfx(&self, message: &[u8]) -> Result<Option<RdpgfxUpdate>, String> {
        // TODO: Parse RDPGFX messages
        // RDPGFX provides:
        // - Hardware-accelerated graphics
        // - H.264/H.265 video decoding hints
        // - Direct bitmap updates

        debug!("RDP: RDPGFX message received: {} bytes", message.len());

        // Placeholder - will parse actual RDPGFX PDU structure
        Ok(None)
    }

    /// Handle AUDIO_INPUT channel (microphone redirection)
    pub fn handle_audio_input(&self, audio_data: &[u8]) -> Result<(), String> {
        debug!("RDP: Audio input received: {} bytes", audio_data.len());
        // TODO: Process audio input and send to RDP server
        Ok(())
    }

    /// Handle AUDIO_OUTPUT channel (speaker redirection)
    pub fn handle_audio_output(&self, audio_data: &[u8]) -> Result<Option<Bytes>, String> {
        debug!("RDP: Audio output received: {} bytes", audio_data.len());
        // TODO: Process audio output and send to client via Guacamole audio instruction
        Ok(None)
    }
}

/// CLIPRDR format types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliprdrFormat {
    Text = 1,
    Bitmap = 2,
    Metafile = 3,
    UnicodeText = 13,
    Html = 0xD010,
}

/// CLIPRDR data ready for sending to RDP server
#[derive(Debug, Clone)]
pub struct CliprdrData {
    pub format: CliprdrFormat,
    pub data: Vec<u8>,
}

/// DISP (Display Update) resize message
#[derive(Debug, Clone)]
pub struct DispResizeMessage {
    pub width: u16,
    pub height: u16,
}

/// RDPGFX update (for hardware acceleration)
#[derive(Debug, Clone)]
pub struct RdpgfxUpdate {
    pub update_type: RdpgfxUpdateType,
    pub data: Vec<u8>,
}

/// RDPGFX update types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdpgfxUpdateType {
    WireToSurface1 = 0x0001,
    WireToSurface2 = 0x0002,
    DeleteEncodingContext = 0x0003,
    SolidFill = 0x0004,
    SurfaceToSurface = 0x0005,
    SurfaceToCache = 0x0006,
    CacheToSurface = 0x0007,
    EvictCacheEntry = 0x0008,
    CreateSurface = 0x0009,
    DeleteSurface = 0x000A,
    ResetGraphics = 0x000B,
    MapSurfaceToOutput = 0x000C,
    MapSurfaceToWindow = 0x000D,
    MapSurfaceToScaledOutput = 0x000E,
    MapSurfaceToScaledWindow = 0x000F,
    CachedToSurface = 0x0010,
    H264Decode = 0x0011,
    H264DecodeExtended = 0x0012,
    AlphaBlend = 0x0013,
    FrameAcknowledge = 0x0014,
    H264DecodeExtended2 = 0x0015,
}

impl Default for RdpChannelHandler {
    fn default() -> Self {
        Self::new(256 * 1024, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_handler_new() {
        let handler = RdpChannelHandler::new(1024 * 1024, false, false);
        assert!(!handler.disable_copy);
        assert!(!handler.disable_paste);
    }

    #[test]
    fn test_disp_resize() {
        let handler = RdpChannelHandler::new(1024 * 1024, false, false);
        let msg = handler.prepare_disp_resize(1920, 1080);
        assert_eq!(msg.width, 1920);
        assert_eq!(msg.height, 1080);
    }
}
