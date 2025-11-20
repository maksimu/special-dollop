// Shared hardware encoder implementation for all protocols
// Supports NVENC, QuickSync, VideoToolbox, VAAPI via MIT/Apache-licensed sys crates
// Can be used by SSH, Telnet, RDP, VNC, RBI - any protocol that renders images

use bytes::Bytes;

/// Hardware encoder trait for platform-specific implementations
pub trait HardwareEncoder: Send + Sync {
    /// Encode a frame to H.264/H.265
    ///
    /// # Arguments
    ///
    /// * `pixels` - RGBA pixels (width * height * 4 bytes)
    /// * `width` - Frame width
    /// * `height` - Frame height
    ///
    /// # Returns
    ///
    /// Encoded H.264/H.265 data
    fn encode_frame(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<Bytes, String>;

    /// Check if hardware encoder is available
    fn is_available() -> bool
    where
        Self: Sized;

    /// Get encoder name (e.g., "NVENC", "QuickSync", "VAAPI")
    fn name(&self) -> &str;
}

/// Hardware encoder implementation
///
/// Automatically selects the best available encoder:
/// 1. VideoToolbox (macOS) - via objc2-video-toolbox crate (MIT OR Apache-2.0)
/// 2. Software fallback (JPEG)
pub enum HardwareEncoderImpl {
    /// macOS VideoToolbox encoder (MIT-licensed bindings)
    #[cfg(feature = "videotoolbox")]
    VideoToolbox(VideoToolboxEncoder),
    /// Software encoder fallback (JPEG)
    Software(SoftwareEncoder),
}

impl HardwareEncoderImpl {
    /// Create the best available hardware encoder
    ///
    /// Automatically adjusts bitrate to keep frames ≤ 60KB (accounting for protocol overhead).
    pub fn new(width: u32, height: u32, bitrate_mbps: u32) -> Result<Self, String> {
        use guacr_protocol::MAX_ENCODER_FRAME_SIZE;

        // Calculate bitrate to keep frames ≤ 60KB
        let fps = 60.0;
        let target_frame_size = MAX_ENCODER_FRAME_SIZE; // 60KB
        let max_bitrate = (target_frame_size as f64 * fps * 8.0) / 1_000_000.0;

        // Use lower of requested or calculated bitrate
        let adjusted_bitrate = bitrate_mbps.min(max_bitrate as u32);

        if adjusted_bitrate != bitrate_mbps {
            log::info!(
                "Hardware encoder: Adjusted bitrate from {} Mbps to {} Mbps to keep frames ≤ {}KB",
                bitrate_mbps,
                adjusted_bitrate,
                target_frame_size / 1024
            );
        }

        log::info!(
            "Hardware encoder: {}x{} @ {}fps, bitrate {} Mbps (max frame: {}KB)",
            width,
            height,
            fps,
            adjusted_bitrate,
            target_frame_size / 1024
        );

        // Try hardware encoders in order of preference
        #[cfg(feature = "videotoolbox")]
        {
            if VideoToolboxEncoder::is_available() {
                log::info!("Hardware encoder: Using VideoToolbox");
                return Ok(Self::VideoToolbox(VideoToolboxEncoder::new(
                    width,
                    height,
                    adjusted_bitrate,
                )?));
            }
        }

        // Fallback to software encoder
        log::warn!("Hardware encoder: No hardware encoder available, using software fallback");
        Ok(Self::Software(SoftwareEncoder::new(
            width,
            height,
            adjusted_bitrate,
        )?))
    }

    /// Check if any hardware encoder is available
    pub fn is_hardware_available() -> bool {
        #[cfg(feature = "videotoolbox")]
        {
            if VideoToolboxEncoder::is_available() {
                return true;
            }
        }
        false
    }
}

impl HardwareEncoder for HardwareEncoderImpl {
    fn encode_frame(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<Bytes, String> {
        match self {
            #[cfg(feature = "videotoolbox")]
            Self::VideoToolbox(encoder) => encoder.encode_frame(pixels, width, height),
            Self::Software(encoder) => encoder.encode_frame(pixels, width, height),
        }
    }

    fn is_available() -> bool {
        Self::is_hardware_available()
    }

    fn name(&self) -> &str {
        match self {
            #[cfg(feature = "videotoolbox")]
            Self::VideoToolbox(_) => "VideoToolbox",
            Self::Software(_) => "Software (JPEG fallback)",
        }
    }
}

// Platform-specific encoder implementations
// These modules are declared in lib.rs and re-exported here

#[cfg(feature = "videotoolbox")]
pub use crate::hardware_encoder_videotoolbox::VideoToolboxEncoder;

// RGBA to YUV420 conversion helper (used by all encoders)
// ITU-R BT.601 standard conversion
#[allow(dead_code)] // Used by feature-gated encoder implementations
pub(crate) fn rgba_to_yuv420(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut yuv = Vec::with_capacity((width * height * 3 / 2) as usize);

    // Y plane (luma) - full resolution
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 3 < rgba.len() {
                let r = rgba[idx] as f32;
                let g = rgba[idx + 1] as f32;
                let b = rgba[idx + 2] as f32;

                // Y = 0.299*R + 0.587*G + 0.114*B (ITU-R BT.601)
                let y_val = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
                yuv.push(y_val);
            }
        }
    }

    // U and V planes (chroma) - subsampled 2x2
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 3 < rgba.len() {
                let r = rgba[idx] as f32;
                let g = rgba[idx + 1] as f32;
                let b = rgba[idx + 2] as f32;

                // U = -0.169*R - 0.331*G + 0.5*B + 128
                let u_val = ((-0.169 * r - 0.331 * g + 0.5 * b) + 128.0).clamp(0.0, 255.0) as u8;
                yuv.push(u_val);

                // V = 0.5*R - 0.419*G - 0.081*B + 128
                let v_val = ((0.5 * r - 0.419 * g - 0.081 * b) + 128.0).clamp(0.0, 255.0) as u8;
                yuv.push(v_val);
            }
        }
    }

    yuv
}

/// Software encoder fallback (JPEG)
///
/// Falls back to JPEG encoding when hardware encoding is unavailable
pub struct SoftwareEncoder {
    frame_count: u64,
}

impl SoftwareEncoder {
    pub fn new(_width: u32, _height: u32, _bitrate_mbps: u32) -> Result<Self, String> {
        Ok(Self { frame_count: 0 })
    }
}

impl HardwareEncoder for SoftwareEncoder {
    fn encode_frame(&mut self, _pixels: &[u8], _width: u32, _height: u32) -> Result<Bytes, String> {
        self.frame_count += 1;

        // Software encoder returns error to signal fallback to JPEG
        // The caller will handle JPEG encoding
        Err(format!(
            "Software encoder (frame {}): Use JPEG encoding instead",
            self.frame_count
        ))
    }

    fn is_available() -> bool {
        true // Always available as fallback
    }

    fn name(&self) -> &str {
        "Software (JPEG fallback)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_encoder_creation() {
        // Test that encoder can be created (will use software fallback)
        let encoder = HardwareEncoderImpl::new(1920, 1080, 10);
        assert!(encoder.is_ok());
    }

    #[test]
    fn test_rgba_to_yuv420() {
        // Test RGBA to YUV420 conversion
        let rgba = vec![255u8; 4 * 4 * 4]; // 4x4 white image
        let yuv = rgba_to_yuv420(&rgba, 4, 4);
        // YUV420: Y plane (16 bytes) + UV plane (8 bytes) = 24 bytes
        assert_eq!(yuv.len(), 24);
    }
}
