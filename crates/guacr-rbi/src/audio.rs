// Audio handling for RBI
//
// NOTE: Chrome DevTools Protocol (CDP) does not directly expose audio streams.
// KCM which has cef_audio_handler callbacks. For CDP-based RBI,
// audio capture requires one of:
// 1. Chrome extension with tabCapture API
// 2. Web Audio API hooks (limited to pages that use Web Audio)
// 3. Disable audio (many RBI use cases don't need it)
//
// This module provides the Guacamole protocol audio streaming infrastructure
// that can be used if audio capture becomes available.

use bytes::Bytes;
use log::info;

/// Audio configuration
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Enable audio streaming
    pub enabled: bool,
    /// Sample rate in Hz (e.g., 44100, 48000)
    pub sample_rate: u32,
    /// Bits per sample (8, 16, or 32)
    pub bits_per_sample: u8,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for CDP
            sample_rate: 44100,
            bits_per_sample: 16,
            channels: 2,
        }
    }
}

impl AudioConfig {
    /// Calculate bytes per frame
    pub fn bytes_per_frame(&self) -> usize {
        (self.bits_per_sample as usize / 8) * self.channels as usize
    }

    /// Calculate bytes per second
    pub fn bytes_per_second(&self) -> usize {
        self.bytes_per_frame() * self.sample_rate as usize
    }

    /// Get MIME type for Guacamole protocol
    pub fn mimetype(&self) -> &'static str {
        match self.bits_per_sample {
            8 => "audio/L8",
            16 => "audio/L16",
            _ => "audio/L16", // Default to 16-bit
        }
    }
}

/// Audio packet for streaming
#[derive(Debug, Clone)]
pub struct AudioPacket {
    /// Raw PCM audio data
    pub data: Vec<u8>,
    /// Timestamp in microseconds
    pub timestamp: u64,
}

/// Maximum audio packet size (4KB like KCM)
pub const AUDIO_PACKET_SIZE: usize = 4096;

/// Audio stream state
#[derive(Debug, Default)]
pub struct AudioStream {
    config: AudioConfig,
    stream_id: u32,
    active: bool,
    bytes_sent: u64,
}

impl AudioStream {
    /// Create a new audio stream
    pub fn new(config: AudioConfig, stream_id: u32) -> Self {
        Self {
            config,
            stream_id,
            active: false,
            bytes_sent: 0,
        }
    }

    /// Start the audio stream
    ///
    /// Returns the Guacamole audio instruction to send to client
    pub fn start(&mut self) -> Option<String> {
        if !self.config.enabled {
            return None;
        }

        self.active = true;
        info!(
            "RBI: Starting audio stream {} ({}Hz, {}ch, {}bps)",
            self.stream_id,
            self.config.sample_rate,
            self.config.channels,
            self.config.bits_per_sample
        );

        // Format: audio,<stream>,<mimetype>,<sample_rate>,<channels>;
        Some(format!(
            "5.audio,{}.{},{}.{},{}.{},{}.{};",
            self.stream_id.to_string().len(),
            self.stream_id,
            self.config.mimetype().len(),
            self.config.mimetype(),
            self.config.sample_rate.to_string().len(),
            self.config.sample_rate,
            self.config.channels.to_string().len(),
            self.config.channels,
        ))
    }

    /// Stop the audio stream
    ///
    /// Returns the Guacamole end instruction
    pub fn stop(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }

        self.active = false;
        info!(
            "RBI: Stopping audio stream {} ({} bytes sent)",
            self.stream_id, self.bytes_sent
        );

        // Format: end,<stream>;
        Some(format!(
            "3.end,{}.{};",
            self.stream_id.to_string().len(),
            self.stream_id,
        ))
    }

    /// Send audio data
    ///
    /// Returns the Guacamole blob instruction with base64-encoded audio
    pub fn send_packet(&mut self, packet: &AudioPacket) -> Option<Bytes> {
        if !self.active {
            return None;
        }

        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine;

        let base64_data = BASE64.encode(&packet.data);
        self.bytes_sent += packet.data.len() as u64;

        // Format: blob,<stream>,<base64_data>;
        let instr = format!(
            "4.blob,{}.{},{}.{};",
            self.stream_id.to_string().len(),
            self.stream_id,
            base64_data.len(),
            base64_data,
        );

        Some(Bytes::from(instr))
    }

    /// Check if stream is active
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get stream ID
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }
}

/// Convert float audio samples to PCM bytes
///
/// Port of KCM's float_to_uchar function
pub fn float_to_pcm(samples: &[f32], _channels: usize, bits_per_sample: u8) -> Vec<u8> {
    let bytes_per_sample = bits_per_sample as usize / 8;
    let mut output = vec![0u8; samples.len() * bytes_per_sample];

    for (i, &sample) in samples.iter().enumerate() {
        let offset = i * bytes_per_sample;

        match bits_per_sample {
            32 => {
                let value = (sample * 2147483647.0) as i32;
                output[offset] = (value & 0xff) as u8;
                output[offset + 1] = ((value >> 8) & 0xff) as u8;
                output[offset + 2] = ((value >> 16) & 0xff) as u8;
                output[offset + 3] = ((value >> 24) & 0xff) as u8;
            }
            16 => {
                let value = (sample * 32767.0) as i16;
                output[offset] = (value & 0xff) as u8;
                output[offset + 1] = ((value >> 8) & 0xff) as u8;
            }
            8 => {
                let value = ((sample * 127.5) + 128.0) as u8;
                output[offset] = value;
            }
            _ => {
                // Default to 16-bit
                let value = (sample * 32767.0) as i16;
                output[offset] = (value & 0xff) as u8;
                output[offset + 1] = ((value >> 8) & 0xff) as u8;
            }
        }
    }

    output
}

/// JavaScript to monitor Web Audio API usage
///
/// This hooks into the Web Audio API to detect when audio is playing.
/// Note: This only works for pages that use Web Audio API, not regular
/// HTML5 `<audio>` or `<video>` elements.
pub const WEB_AUDIO_MONITOR_JS: &str = r#"
(function() {
    if (window.__guacAudioMonitor) return;
    
    window.__guacAudioState = {
        hasWebAudio: false,
        isPlaying: false,
        contextCount: 0
    };
    
    // Hook AudioContext constructor
    const OriginalAudioContext = window.AudioContext || window.webkitAudioContext;
    if (OriginalAudioContext) {
        window.AudioContext = function(...args) {
            const ctx = new OriginalAudioContext(...args);
            window.__guacAudioState.hasWebAudio = true;
            window.__guacAudioState.contextCount++;
            
            // Monitor state changes
            ctx.addEventListener('statechange', () => {
                window.__guacAudioState.isPlaying = ctx.state === 'running';
            });
            
            return ctx;
        };
        window.webkitAudioContext = window.AudioContext;
    }
    
    window.__guacAudioMonitor = true;
})()
"#;

/// JavaScript to get audio state
pub const GET_AUDIO_STATE_JS: &str = r#"
(function() {
    return window.__guacAudioState || { hasWebAudio: false, isPlaying: false };
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_config_default() {
        let config = AudioConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.sample_rate, 44100);
        assert_eq!(config.bits_per_sample, 16);
        assert_eq!(config.channels, 2);
    }

    #[test]
    fn test_audio_config_bytes() {
        let config = AudioConfig::default();
        assert_eq!(config.bytes_per_frame(), 4); // 16-bit stereo
        assert_eq!(config.bytes_per_second(), 176400);
    }

    #[test]
    fn test_audio_stream() {
        let config = AudioConfig {
            enabled: true,
            ..Default::default()
        };
        let mut stream = AudioStream::new(config, 1);

        let start = stream.start();
        assert!(start.is_some());
        assert!(stream.is_active());

        let packet = AudioPacket {
            data: vec![0u8; 100],
            timestamp: 0,
        };
        let blob = stream.send_packet(&packet);
        assert!(blob.is_some());

        let stop = stream.stop();
        assert!(stop.is_some());
        assert!(!stream.is_active());
    }

    #[test]
    fn test_float_to_pcm_16bit() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let pcm = float_to_pcm(&samples, 1, 16);

        // 0.0 -> 0
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], 0);

        // 0.5 -> ~16383 (0x3FFF)
        assert!(pcm[2] > 0xF0);
        assert!(pcm[3] > 0x3E);
    }

    #[test]
    fn test_float_to_pcm_8bit() {
        let samples = vec![0.0f32, 1.0, -1.0];
        let pcm = float_to_pcm(&samples, 1, 8);

        // 0.0 -> 128
        assert_eq!(pcm[0], 128);
        // 1.0 -> ~255
        assert!(pcm[1] > 250);
        // -1.0 -> ~0
        assert!(pcm[2] < 5);
    }
}
