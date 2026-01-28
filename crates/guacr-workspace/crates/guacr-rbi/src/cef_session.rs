// CEF (Chromium Embedded Framework) session manager for RBI
//
// SECURITY ARCHITECTURE (DESIGN GOALS - PARTIAL IMPLEMENTATION):
// - Each CefSession should spawn a dedicated CEF subprocess (no process sharing)
// - Each session should get its own isolated Chromium process
// - Profile directories should be locked to prevent concurrent access
// - Processes should terminate when sessions end
//
// NOTE: This is currently a stub implementation. Full isolation guarantees
// are not yet implemented. Do not rely on this for security-critical isolation
// until the implementation is complete (see TODOs throughout this file).
//
// This provides the same capabilities as KCM's CEF implementation:
// - RenderHandler: Receives raw BGRA pixels for each frame
// - AudioHandler: Receives raw PCM audio samples
// - Input injection: Keyboard, mouse, touch
// - Lifecycle management: Popups, navigation, dialogs
//
// Unlike the chromiumoxide/CDP approach, CEF embeds the browser directly
// and provides callback-based access to audio streams.
//
// ## Process Architecture
//
// Each CefSession creates:
// 1. Main CEF process (browser process)
// 2. Renderer process (isolated sandbox)
// 3. GPU process (if available)
// 4. Utility processes (network, audio, etc.)
//
// All processes terminate when CefSession::close() is called.
//
// ## Build Requirements
//
// 1. Download CEF binaries from https://cef-builds.spotifycdn.com/
// 2. Set CEF_PATH environment variable to extracted location
// 3. Build: cargo build --features cef

#[cfg(feature = "cef")]
use log::info;
use std::sync::Arc;
use tokio::sync::mpsc;

// Use parking_lot for CEF features (faster, no poisoning)
#[cfg(feature = "cef")]
use parking_lot::Mutex;

// Use std Mutex when CEF is not enabled
#[cfg(not(feature = "cef"))]
use std::sync::Mutex;

/// Maximum dimensions (same as KCM)
pub const MAX_WIDTH: u32 = 8192;
pub const MAX_HEIGHT: u32 = 8192;

/// Bytes per pixel (BGRA)
pub const BYTES_PER_PIXEL: usize = 4;

/// Stride for image data (bytes per row)
pub const IMAGE_DATA_STRIDE: usize = MAX_WIDTH as usize * BYTES_PER_PIXEL;

/// Audio packet size (same as KCM: 4KB)
pub const AUDIO_PACKET_SIZE: usize = 4096;

/// Display event types (matching KCM's display_fifo events)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplayEventType {
    /// Draw pixels to a rectangular region
    Draw,
    /// End of current frame (sync point)
    EndOfFrame,
    /// Resize the display surface
    Resize,
    /// Move and resize (for popups)
    MoveResize,
    /// Show a surface
    Show,
    /// Hide a surface
    Hide,
    /// Cursor style changed
    CursorChange,
}

/// Display surface types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisplaySurface {
    /// Main browser view
    View,
    /// Popup overlay (dropdowns, menus)
    Popup,
}

/// Cursor types (matching CEF cursor types)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CefCursorType {
    #[default]
    Pointer,
    IBeam,
    Hand,
    Wait,
    Help,
    CrossHair,
    Move,
    NotAllowed,
    None,
}

/// A display event from CEF
#[derive(Debug, Clone)]
pub struct CefDisplayEvent {
    pub event_type: DisplayEventType,
    pub surface: DisplaySurface,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Raw BGRA pixel data for the dirty rect
    pub pixels: Option<Vec<u8>>,
    /// Cursor type (for CursorChange events)
    pub cursor: Option<CefCursorType>,
}

impl CefDisplayEvent {
    /// Create a draw event
    pub fn draw(
        surface: DisplaySurface,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            event_type: DisplayEventType::Draw,
            surface,
            x,
            y,
            width,
            height,
            pixels: Some(pixels),
            cursor: None,
        }
    }

    /// Create an end-of-frame event
    pub fn end_of_frame() -> Self {
        Self {
            event_type: DisplayEventType::EndOfFrame,
            surface: DisplaySurface::View,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: None,
            cursor: None,
        }
    }

    /// Create a resize event
    pub fn resize(width: i32, height: i32) -> Self {
        Self {
            event_type: DisplayEventType::Resize,
            surface: DisplaySurface::View,
            x: 0,
            y: 0,
            width,
            height,
            pixels: None,
            cursor: None,
        }
    }

    /// Create a cursor change event
    pub fn cursor_change(cursor: CefCursorType) -> Self {
        Self {
            event_type: DisplayEventType::CursorChange,
            surface: DisplaySurface::View,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: None,
            cursor: Some(cursor),
        }
    }
}

/// An audio packet from CEF
#[derive(Debug, Clone)]
pub struct CefAudioPacket {
    /// Raw PCM audio data (already converted from float)
    pub data: Vec<u8>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels
    pub channels: u8,
    /// Bits per sample
    pub bits_per_sample: u8,
    /// Presentation timestamp
    pub pts: i64,
}

impl CefAudioPacket {
    /// Create a new audio packet
    pub fn new(
        data: Vec<u8>,
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        pts: i64,
    ) -> Self {
        Self {
            data,
            sample_rate,
            channels,
            bits_per_sample,
            pts,
        }
    }
}

/// Audio output format configuration
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g., 44100)
    pub sample_rate: u32,
    /// Number of channels (1=mono, 2=stereo)
    pub channels: u8,
    /// Bits per sample (8, 16, or 32)
    pub bits_per_sample: u8,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
        }
    }
}

impl AudioFormat {
    /// Calculate bytes per frame (all channels)
    pub fn bytes_per_frame(&self) -> usize {
        self.channels as usize * (self.bits_per_sample as usize / 8)
    }

    /// Calculate frames that fit in given byte count
    pub fn frames_in_bytes(&self, bytes: usize) -> usize {
        bytes / self.bytes_per_frame()
    }
}

/// Shared state between CEF callbacks and the session
#[derive(Debug)]
pub struct CefSharedState {
    /// Current viewport width
    pub width: u32,
    /// Current viewport height
    pub height: u32,
    /// Screen width (for get_screen_info)
    pub screen_width: u32,
    /// Screen height (for get_screen_info)
    pub screen_height: u32,
    /// Audio format configuration
    pub audio_format: AudioFormat,
    /// Whether audio is enabled
    pub audio_enabled: bool,
    /// Channel to send display events
    pub display_tx: Option<mpsc::Sender<CefDisplayEvent>>,
    /// Channel to send audio packets
    pub audio_tx: Option<mpsc::Sender<CefAudioPacket>>,
    /// Current page URL
    pub current_url: String,
    /// Current page title
    pub current_title: String,
    /// Can navigate back
    pub can_go_back: bool,
    /// Can navigate forward
    pub can_go_forward: bool,
    /// Browser is terminating
    pub terminating: bool,
    /// Clipboard content (from browser)
    pub clipboard: String,
    /// Clipboard content (to browser)
    pub clipboard_pending: Option<String>,
}

impl Default for CefSharedState {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            screen_width: 1920,
            screen_height: 1080,
            audio_format: AudioFormat::default(),
            audio_enabled: true,
            display_tx: None,
            audio_tx: None,
            current_url: String::new(),
            current_title: String::new(),
            can_go_back: false,
            can_go_forward: false,
            terminating: false,
            clipboard: String::new(),
            clipboard_pending: None,
        }
    }
}

// =============================================================================
// CEF Session Implementation (with cef feature)
// =============================================================================
//
// NOTE: The CEF integration below is a design document / stub implementation.
// The actual CEF crate API may differ. This code compiles but requires:
// 1. CEF binaries downloaded (CEF_PATH environment variable)
// 2. Verification against actual cef crate API when CEF builds are enabled
//
// The Chrome/CDP backend (--features chrome) is fully functional and recommended
// for most use cases. CEF is only needed for full audio streaming support.

#[cfg(feature = "cef")]
mod cef_impl {
    use super::*;

    /// CEF session - stub implementation
    ///
    /// CRITICAL SECURITY: Each CefSession spawns a DEDICATED CEF process
    /// - NO process sharing between sessions
    /// - Each session gets its own isolated Chromium process tree
    /// - Profile directories are locked to prevent concurrent access
    /// - Process terminates when session ends (Drop impl)
    /// - Complete memory and process isolation
    ///
    /// Process Architecture:
    /// - Main CEF browser process (spawned per session)
    /// - Renderer process (sandboxed, isolated)
    /// - GPU process (if available)
    /// - Utility processes (network, audio, etc.)
    ///
    /// All processes terminate when CefSession is dropped.
    ///
    /// This is a placeholder that documents the intended CEF API.
    /// Full implementation requires CEF binaries and verification of the cef crate API.
    pub struct CefSession {
        state: Arc<Mutex<CefSharedState>>,
        #[allow(dead_code)]
        initialized: bool,
        /// Isolated profile directory for this session (auto-cleaned on drop)
        /// SECURITY: Each session gets unique directory
        #[allow(dead_code)]
        profile_dir: Option<tempfile::TempDir>,
        /// Profile lock for persistent profiles (prevents concurrent use)
        /// SECURITY: Locked to prevent multiple sessions using same profile
        #[allow(dead_code)]
        profile_lock: Option<crate::profile_isolation::ProfileLock>,
        /// DBus isolation for Linux (prevents cross-session IPC)
        /// SECURITY: Isolated DBus socket per session (KCM-436 pattern)
        #[cfg(target_os = "linux")]
        #[allow(dead_code)]
        dbus_isolation: Option<crate::profile_isolation::DbusIsolation>,
        /// Process ID of the CEF browser process
        /// SECURITY: Tracked for monitoring and termination
        #[allow(dead_code)]
        browser_pid: Option<u32>,
    }

    impl CefSession {
        /// Create a new CEF session
        ///
        /// Does NOT spawn CEF process yet - call launch() to spawn
        /// Each launch() call will spawn a DEDICATED CEF process (no sharing)
        pub fn new(width: u32, height: u32) -> Self {
            let state = Arc::new(Mutex::new(CefSharedState {
                width,
                height,
                screen_width: width,
                screen_height: height,
                ..Default::default()
            }));

            Self {
                state,
                initialized: false,
                profile_dir: None,
                profile_lock: None,
                #[cfg(target_os = "linux")]
                dbus_isolation: None,
                browser_pid: None,
            }
        }

        /// Initialize CEF (must be called once per process)
        ///
        /// NOTE: This is a stub. Full implementation requires CEF binaries.
        pub fn initialize() -> Result<(), String> {
            Err(
                "CEF feature is enabled but CEF integration is not yet complete. \
                 The CEF crate requires CEF binaries to be downloaded and the API \
                 verified. Use --features chrome for a working browser backend."
                    .to_string(),
            )
        }

        /// Shutdown CEF (must be called before process exit)
        pub fn shutdown() {
            info!("CEF: Shutdown called (stub)");
        }

        /// Launch a browser - stub implementation
        pub async fn launch(
            &mut self,
            url: &str,
            display_tx: mpsc::Sender<CefDisplayEvent>,
            audio_tx: mpsc::Sender<CefAudioPacket>,
        ) -> Result<(), String> {
            self.launch_with_profile(url, display_tx, audio_tx, None)
                .await
        }

        /// Launch browser with optional persistent profile - stub implementation
        ///
        /// SECURITY: Spawns a DEDICATED CEF process for this session
        /// - Creates unique profile directory (locked)
        /// - Spawns separate process tree (browser + renderer + GPU + utility)
        /// - No sharing with other sessions
        /// - Process terminates when close() is called or session is dropped
        pub async fn launch_with_profile(
            &mut self,
            url: &str,
            display_tx: mpsc::Sender<CefDisplayEvent>,
            audio_tx: mpsc::Sender<CefAudioPacket>,
            profile_directory: Option<&str>,
        ) -> Result<(), String> {
            info!("CEF: Launch requested for URL: {} (stub)", url);
            info!("CEF: SECURITY - Will spawn DEDICATED process (no sharing)");

            // Store channels in shared state (for when real implementation is added)
            {
                let mut state = self.state.lock();
                state.display_tx = Some(display_tx);
                state.audio_tx = Some(audio_tx);
                state.current_url = url.to_string();
            }

            // Profile isolation would be set up here
            if let Some(profile_dir) = profile_directory {
                info!("CEF: Would use profile directory: {} (locked)", profile_dir);
                // TODO: Lock profile directory to prevent concurrent access
                // self.profile_lock = Some(ProfileLock::acquire(profile_dir)?);
            } else {
                info!("CEF: Would create temporary profile directory");
                // TODO: Create temp directory with automatic cleanup
                // self.profile_dir = Some(tempfile::tempdir()?);
            }

            // TODO: Spawn CEF process here
            // 1. Initialize CEF with profile directory
            // 2. Create browser instance (headless)
            // 3. Navigate to URL
            // 4. Store browser PID: self.browser_pid = Some(pid);
            // 5. Start message loop in background thread

            info!("CEF: Would spawn dedicated CEF process here");
            info!("CEF: Process would terminate when close() is called");

            Err("CEF browser launch not yet implemented. \
                 Use --features chrome for a working browser backend."
                .to_string())
        }

        /// Navigate to a new URL
        pub fn navigate(&self, url: &str) -> Result<(), String> {
            info!("CEF: Navigate to {} (stub)", url);
            Err("CEF not initialized".to_string())
        }

        /// Navigate back in history
        pub fn go_back(&self) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Navigate forward in history
        pub fn go_forward(&self) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Reload the current page
        pub fn reload(&self) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Inject keyboard event
        pub fn inject_keyboard(&self, _keysym: u32, _pressed: bool) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Inject mouse event
        pub fn inject_mouse(
            &self,
            _x: i32,
            _y: i32,
            _button: i32,
            _pressed: bool,
        ) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Inject mouse move event
        pub fn inject_mouse_move(&self, _x: i32, _y: i32) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Inject scroll event
        pub fn inject_scroll(
            &self,
            _x: i32,
            _y: i32,
            _delta_x: i32,
            _delta_y: i32,
        ) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Resize the browser viewport
        pub fn resize(&self, width: u32, height: u32) -> Result<(), String> {
            let mut state = self.state.lock();
            state.width = width;
            state.height = height;
            Ok(())
        }

        /// Execute JavaScript in the browser
        pub fn execute_javascript(&self, _script: &str) -> Result<(), String> {
            Err("CEF not initialized".to_string())
        }

        /// Set clipboard content (inject into browser)
        pub fn set_clipboard(&self, text: &str) -> Result<(), String> {
            let mut state = self.state.lock();
            state.clipboard_pending = Some(text.to_string());
            Ok(())
        }

        /// Get current dimensions
        pub fn dimensions(&self) -> (u32, u32) {
            let state = self.state.lock();
            (state.width, state.height)
        }

        /// Close the browser
        ///
        /// SECURITY: Terminates the dedicated CEF process
        /// - Closes browser window
        /// - Shuts down CEF
        /// - Terminates all CEF subprocesses (browser, renderer, GPU, utility)
        /// - Unlocks profile directory (if locked)
        /// - Ensures no process leakage
        pub fn close(&mut self) {
            info!("CEF: Closing browser and terminating dedicated process (stub)");

            if let Some(pid) = self.browser_pid {
                info!("CEF: Would terminate browser process (PID: {})", pid);
            }

            let mut state = self.state.lock();
            state.terminating = true;

            // TODO: Actual CEF cleanup
            // 1. Close browser window (cef_browser_host_close_browser)
            // 2. Shutdown CEF (cef_shutdown)
            // 3. Wait for process to terminate
            // 4. Verify all subprocesses terminated
            // 5. Profile directory lock is automatically released on drop

            info!("CEF: Browser closed (stub)");
            info!("CEF: Profile directory lock will be released on drop");
        }

        /// Get shared state (for handlers)
        pub fn state(&self) -> Arc<Mutex<CefSharedState>> {
            self.state.clone()
        }
    }
}

// =============================================================================
// Fallback for when CEF feature is not enabled
// =============================================================================

#[cfg(not(feature = "cef"))]
pub struct CefSession;

#[cfg(not(feature = "cef"))]
impl CefSession {
    pub fn new(_width: u32, _height: u32) -> Self {
        Self
    }

    pub fn initialize() -> Result<(), String> {
        Err(
            "CEF feature not enabled. Build with --features cef for CEF backend \
             or --features chrome for Chrome/CDP backend."
                .to_string(),
        )
    }

    pub fn shutdown() {}
}

// =============================================================================
// Re-export the appropriate implementation
// =============================================================================

#[cfg(feature = "cef")]
pub use cef_impl::CefSession;

// =============================================================================
// Helper Functions
// =============================================================================

/// Extract pixels for a specific rectangle from the full buffer
pub fn extract_rect_pixels(
    buffer: &[u8],
    buffer_width: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width * height * BYTES_PER_PIXEL);
    let stride = buffer_width * BYTES_PER_PIXEL;

    for row in 0..height {
        let src_offset = (y + row) * stride + x * BYTES_PER_PIXEL;
        let src_end = src_offset + width * BYTES_PER_PIXEL;

        if src_end <= buffer.len() {
            pixels.extend_from_slice(&buffer[src_offset..src_end]);
        }
    }

    pixels
}

/// Convert interleaved float audio to PCM bytes (like KCM's float_to_uchar)
pub fn float_to_pcm_interleaved(
    data: &[&[f32]],
    frames: usize,
    channels: u8,
    bits_per_sample: u8,
) -> Vec<u8> {
    let bytes_per_sample = bits_per_sample as usize / 8;
    let mut output = Vec::with_capacity(frames * channels as usize * bytes_per_sample);

    for frame in 0..frames {
        for ch in 0..channels as usize {
            if ch < data.len() && frame < data[ch].len() {
                let sample = data[ch][frame];

                match bits_per_sample {
                    32 => {
                        let value = (sample * 2147483647.0) as i32;
                        output.extend_from_slice(&value.to_le_bytes());
                    }
                    16 => {
                        let value = (sample * 32767.0) as i16;
                        output.extend_from_slice(&value.to_le_bytes());
                    }
                    8 => {
                        let value = ((sample * 127.5) + 128.0) as u8;
                        output.push(value);
                    }
                    _ => {
                        // Default to 16-bit
                        let value = (sample * 32767.0) as i16;
                        output.extend_from_slice(&value.to_le_bytes());
                    }
                }
            }
        }
    }

    output
}

/// Convert X11 keysym to Windows virtual key code
pub fn keysym_to_vk(keysym: u32) -> (i32, i32) {
    match keysym {
        // Function keys
        0xFF08 => (0x08, 0x08), // Backspace
        0xFF09 => (0x09, 0x09), // Tab
        0xFF0D => (0x0D, 0x0D), // Enter
        0xFF1B => (0x1B, 0x1B), // Escape
        0xFFFF => (0x2E, 0x2E), // Delete
        0xFF50 => (0x24, 0x24), // Home
        0xFF51 => (0x25, 0x25), // Left
        0xFF52 => (0x26, 0x26), // Up
        0xFF53 => (0x27, 0x27), // Right
        0xFF54 => (0x28, 0x28), // Down
        0xFF55 => (0x21, 0x21), // Page Up
        0xFF56 => (0x22, 0x22), // Page Down
        0xFF57 => (0x23, 0x23), // End
        0xFF63 => (0x2D, 0x2D), // Insert

        // Modifiers
        0xFFE1 => (0x10, 0x10), // Shift L
        0xFFE2 => (0x10, 0x10), // Shift R
        0xFFE3 => (0x11, 0x11), // Ctrl L
        0xFFE4 => (0x11, 0x11), // Ctrl R
        0xFFE9 => (0x12, 0x12), // Alt L
        0xFFEA => (0x12, 0x12), // Alt R

        // Letters A-Z
        0x0041..=0x005A => (keysym as i32, keysym as i32),
        0x0061..=0x007A => ((keysym - 0x20) as i32, (keysym - 0x20) as i32), // lowercase

        // Numbers 0-9
        0x0030..=0x0039 => (keysym as i32, keysym as i32),

        // Space
        0x0020 => (0x20, 0x20),

        // Default: use keysym as-is
        _ => (keysym as i32, keysym as i32),
    }
}

/// Convert keysym to character
pub fn keysym_to_char(keysym: u32) -> Option<char> {
    match keysym {
        0x0020..=0x007E => char::from_u32(keysym), // Printable ASCII
        0xFF0D => Some('\r'),                      // Enter
        0xFF09 => Some('\t'),                      // Tab
        0xFF08 => Some('\x08'),                    // Backspace
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rect_pixels() {
        // 4x4 buffer, BGRA
        let buffer: Vec<u8> = (0..64).collect();

        // Extract 2x2 from (1,1)
        let pixels = extract_rect_pixels(&buffer, 4, 1, 1, 2, 2);

        // Should be 2*2*4 = 16 bytes
        assert_eq!(pixels.len(), 16);
    }

    #[test]
    fn test_float_to_pcm_16bit() {
        let channel1 = [0.0f32, 0.5, -0.5];
        let channel2 = [0.0f32, -0.5, 0.5];
        let data: Vec<&[f32]> = vec![&channel1, &channel2];

        let pcm = float_to_pcm_interleaved(&data, 3, 2, 16);

        // 3 frames * 2 channels * 2 bytes = 12 bytes
        assert_eq!(pcm.len(), 12);

        // First sample (0.0, 0.0) should be zeros
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], 0);
        assert_eq!(pcm[2], 0);
        assert_eq!(pcm[3], 0);
    }

    #[test]
    fn test_keysym_to_vk() {
        assert_eq!(keysym_to_vk(0xFF0D), (0x0D, 0x0D)); // Enter
        assert_eq!(keysym_to_vk(0xFF08), (0x08, 0x08)); // Backspace
        assert_eq!(keysym_to_vk(0x0041), (0x41, 0x41)); // 'A'
        assert_eq!(keysym_to_vk(0x0061), (0x41, 0x41)); // 'a' -> 'A'
    }

    #[test]
    fn test_keysym_to_char() {
        assert_eq!(keysym_to_char(0x0041), Some('A'));
        assert_eq!(keysym_to_char(0x0061), Some('a'));
        assert_eq!(keysym_to_char(0xFF0D), Some('\r'));
        assert_eq!(keysym_to_char(0xFF09), Some('\t'));
    }

    #[test]
    fn test_audio_format() {
        let format = AudioFormat::default();
        assert_eq!(format.bytes_per_frame(), 4); // 2 channels * 2 bytes
        assert_eq!(format.frames_in_bytes(4096), 1024);
    }

    #[test]
    fn test_cef_display_event() {
        let event = CefDisplayEvent::end_of_frame();
        assert_eq!(event.event_type, DisplayEventType::EndOfFrame);

        let resize = CefDisplayEvent::resize(1920, 1080);
        assert_eq!(resize.width, 1920);
        assert_eq!(resize.height, 1080);
    }
}
