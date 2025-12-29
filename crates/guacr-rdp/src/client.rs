//! FreeRDP client wrapper
//!
//! Provides a safe Rust interface around the FreeRDP C library.
//! This module handles:
//! - FreeRDP instance lifecycle
//! - Connection management
//! - Event handling
//! - Framebuffer access (with zero-copy buffer pooling)

use crate::channels::ClipboardHandler;
use crate::error::{RdpError, Result};
use crate::ffi;
use crate::settings::{RdpSettings, SecurityMode};
use bytes::BytesMut;
use guacr_terminal::{BufferPool, FrameBuffer};
use log::{info, warn};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// FreeRDP client wrapper
///
/// Manages the lifecycle of a FreeRDP instance and provides safe access
/// to RDP functionality.
///
/// # Safety
///
/// This struct contains raw pointers to FreeRDP C structures. All access
/// to these pointers is done through safe wrapper methods that ensure
/// proper synchronization and validity checks.
pub struct FreeRdpClient {
    /// Raw pointer to FreeRDP instance
    instance: *mut ffi::freerdp,
    /// Connection settings
    settings: RdpSettings,
    /// Whether we're connected
    connected: AtomicBool,
    /// Clipboard handler
    clipboard: ClipboardHandler,
    /// Framebuffer width
    width: u32,
    /// Framebuffer height
    height: u32,
    /// Buffer pool for zero-copy framebuffer operations
    buffer_pool: Arc<BufferPool>,
    /// Framebuffer with dirty rectangle tracking
    framebuffer: Option<FrameBuffer>,
}

// FreeRDP instance is not thread-safe by default, but we control access
// through our wrapper methods
unsafe impl Send for FreeRdpClient {}

impl FreeRdpClient {
    /// Create a new FreeRDP client with the given settings
    ///
    /// This allocates a FreeRDP instance but does not connect.
    /// Call `connect()` to establish the RDP session.
    pub fn new(settings: RdpSettings) -> Result<Self> {
        // Create FreeRDP instance
        let instance = unsafe { ffi::freerdp_new() };
        if instance.is_null() {
            return Err(RdpError::InitializationFailed(
                "freerdp_new() returned NULL".to_string(),
            ));
        }

        // Create context
        let result = unsafe { ffi::freerdp_context_new(instance) };
        if result == 0 {
            unsafe { ffi::freerdp_free(instance) };
            return Err(RdpError::InitializationFailed(
                "freerdp_context_new() failed".to_string(),
            ));
        }

        let clipboard = ClipboardHandler::new(
            settings.clipboard_buffer_size,
            settings.disable_copy,
            settings.disable_paste,
        );

        let width = settings.width;
        let height = settings.height;

        // Create buffer pool for framebuffer operations
        // Pool size: 4 buffers, each sized for the display resolution
        let frame_size = (width * height * 4) as usize;
        let buffer_pool = Arc::new(BufferPool::new(4, frame_size));

        let mut client = Self {
            instance,
            settings,
            connected: AtomicBool::new(false),
            clipboard,
            width,
            height,
            buffer_pool,
            framebuffer: None,
        };

        // Configure settings
        client.configure_settings()?;

        Ok(client)
    }

    /// Configure FreeRDP settings from our RdpSettings
    fn configure_settings(&mut self) -> Result<()> {
        unsafe {
            let context = (*self.instance).context;
            if context.is_null() {
                return Err(RdpError::InitializationFailed(
                    "FreeRDP context is NULL".to_string(),
                ));
            }

            let rdp_settings = (*context).settings;
            if rdp_settings.is_null() {
                return Err(RdpError::InitializationFailed(
                    "FreeRDP settings is NULL".to_string(),
                ));
            }

            // Server hostname
            let hostname = CString::new(self.settings.hostname.as_str())
                .map_err(|e| RdpError::InvalidSettings(e.to_string()))?;
            ffi::freerdp_settings_set_string(
                rdp_settings,
                ffi::settings_string::SERVER_HOSTNAME,
                hostname.as_ptr(),
            );

            // Server port
            ffi::freerdp_settings_set_uint32(
                rdp_settings,
                ffi::settings_uint32::SERVER_PORT,
                self.settings.port as u32,
            );

            // Username
            let username = CString::new(self.settings.username.as_str())
                .map_err(|e| RdpError::InvalidSettings(e.to_string()))?;
            ffi::freerdp_settings_set_string(
                rdp_settings,
                ffi::settings_string::USERNAME,
                username.as_ptr(),
            );

            // Password
            let password = CString::new(self.settings.password.as_str())
                .map_err(|e| RdpError::InvalidSettings(e.to_string()))?;
            ffi::freerdp_settings_set_string(
                rdp_settings,
                ffi::settings_string::PASSWORD,
                password.as_ptr(),
            );

            // Domain (optional)
            if let Some(ref domain) = self.settings.domain {
                let domain_cstr = CString::new(domain.as_str())
                    .map_err(|e| RdpError::InvalidSettings(e.to_string()))?;
                ffi::freerdp_settings_set_string(
                    rdp_settings,
                    ffi::settings_string::DOMAIN,
                    domain_cstr.as_ptr(),
                );
            }

            // Display settings
            ffi::freerdp_settings_set_uint32(
                rdp_settings,
                ffi::settings_uint32::DESKTOP_WIDTH,
                self.settings.width,
            );
            ffi::freerdp_settings_set_uint32(
                rdp_settings,
                ffi::settings_uint32::DESKTOP_HEIGHT,
                self.settings.height,
            );
            ffi::freerdp_settings_set_uint32(
                rdp_settings,
                ffi::settings_uint32::COLOR_DEPTH,
                self.settings.color_depth as u32,
            );

            // Security settings
            match self.settings.security_mode {
                SecurityMode::Any => {
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::NLA_SECURITY,
                        1,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::TLS_SECURITY,
                        1,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::RDP_SECURITY,
                        1,
                    );
                }
                SecurityMode::Nla => {
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::NLA_SECURITY,
                        1,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::TLS_SECURITY,
                        0,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::RDP_SECURITY,
                        0,
                    );
                }
                SecurityMode::Tls => {
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::NLA_SECURITY,
                        0,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::TLS_SECURITY,
                        1,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::RDP_SECURITY,
                        0,
                    );
                }
                SecurityMode::Rdp => {
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::NLA_SECURITY,
                        0,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::TLS_SECURITY,
                        0,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::RDP_SECURITY,
                        1,
                    );
                }
                SecurityMode::NlaExt => {
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::NLA_SECURITY,
                        1,
                    );
                    ffi::freerdp_settings_set_bool(
                        rdp_settings,
                        ffi::settings_bool::EXT_SECURITY,
                        1,
                    );
                }
            }

            // Ignore certificate errors if requested
            if self.settings.ignore_certificate {
                ffi::freerdp_settings_set_bool(
                    rdp_settings,
                    ffi::settings_bool::IGNORE_CERTIFICATE,
                    1,
                );
                // Use old license behaviour for compatibility with older RDP servers
                ffi::freerdp_settings_set_bool(
                    rdp_settings,
                    ffi::settings_bool::OLD_LICENSE_BEHAVIOUR,
                    1,
                );
            }

            // Performance settings - FreeRDP 3 prefers individual bool settings
            // over the old PerformanceFlags approach
            if !self.settings.enable_wallpaper {
                ffi::freerdp_settings_set_bool(
                    rdp_settings,
                    ffi::settings_bool::DISABLE_WALLPAPER,
                    1,
                );
            }
            if !self.settings.enable_full_window_drag {
                ffi::freerdp_settings_set_bool(
                    rdp_settings,
                    ffi::settings_bool::DISABLE_FULL_WINDOW_DRAG,
                    1,
                );
            }
            if !self.settings.enable_menu_animations {
                ffi::freerdp_settings_set_bool(
                    rdp_settings,
                    ffi::settings_bool::DISABLE_MENU_ANIMS,
                    1,
                );
            }
            if !self.settings.enable_theming {
                ffi::freerdp_settings_set_bool(rdp_settings, ffi::settings_bool::DISABLE_THEMES, 1);
            }

            // Enable GDI for software rendering
            ffi::freerdp_settings_set_bool(rdp_settings, ffi::settings_bool::SOFTWARE_GDI, 1);

            // Set connection type to LAN for best quality
            ffi::freerdp_settings_set_uint32(
                rdp_settings,
                ffi::settings_uint32::CONNECTION_TYPE,
                1, // CONNECTION_TYPE_LAN
            );

            info!(
                "RDP: Configured settings for {}:{} ({}x{} @ {} bpp)",
                self.settings.hostname,
                self.settings.port,
                self.settings.width,
                self.settings.height,
                self.settings.color_depth
            );
        }

        Ok(())
    }

    /// Connect to the RDP server
    pub fn connect(&mut self) -> Result<()> {
        if self.connected.load(Ordering::SeqCst) {
            return Err(RdpError::ConnectionFailed("Already connected".to_string()));
        }

        info!(
            "RDP: Connecting to {}:{}...",
            self.settings.hostname, self.settings.port
        );

        let result = unsafe { ffi::freerdp_connect(self.instance) };

        if result == 0 {
            // Get error information
            let error_code = unsafe {
                let context = (*self.instance).context;
                if !context.is_null() {
                    // Try to get last error
                    0u32 // Placeholder - would need to call GetLastError equivalent
                } else {
                    0
                }
            };

            return Err(RdpError::ConnectionFailed(format!(
                "freerdp_connect() failed (error: {})",
                error_code
            )));
        }

        self.connected.store(true, Ordering::SeqCst);

        // Initialize GDI after connection
        unsafe {
            let context = (*self.instance).context;
            if !context.is_null() {
                // GDI should be initialized by FreeRDP's pre_connect callback
                // but we can verify it here
                let gdi = (*context).gdi;
                if gdi.is_null() {
                    warn!("RDP: GDI not initialized after connect");
                } else {
                    // Get actual dimensions from GDI
                    self.width = (*gdi).width as u32;
                    self.height = (*gdi).height as u32;

                    // Initialize framebuffer with dirty rect tracking
                    self.framebuffer = Some(FrameBuffer::new(self.width, self.height));

                    info!(
                        "RDP: Connected successfully ({}x{})",
                        self.width, self.height
                    );
                }
            }
        }

        Ok(())
    }

    /// Disconnect from the RDP server
    pub fn disconnect(&mut self) {
        if !self.connected.load(Ordering::SeqCst) {
            return;
        }

        info!("RDP: Disconnecting...");

        unsafe {
            ffi::freerdp_disconnect(self.instance);
        }

        self.connected.store(false, Ordering::SeqCst);
        info!("RDP: Disconnected");
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Check for and process pending events
    ///
    /// Returns true if there are more events to process, false otherwise.
    pub fn check_events(&mut self) -> Result<bool> {
        if !self.is_connected() {
            return Ok(false);
        }

        // Check if we should disconnect
        let should_disconnect = unsafe { ffi::freerdp_shall_disconnect(self.instance) };
        if should_disconnect != 0 {
            info!("RDP: Server requested disconnect");
            self.connected.store(false, Ordering::SeqCst);
            return Ok(false);
        }

        // Process events - freerdp_check_event_handles takes context, not instance
        let result = unsafe {
            let context = (*self.instance).context;
            if context.is_null() {
                return Err(RdpError::ProtocolError("Context is null".to_string()));
            }
            ffi::freerdp_check_event_handles(context)
        };
        if result == 0 {
            // Error or disconnect
            let should_disconnect = unsafe { ffi::freerdp_shall_disconnect(self.instance) };
            if should_disconnect != 0 {
                self.connected.store(false, Ordering::SeqCst);
                return Ok(false);
            }
            return Err(RdpError::ProtocolError(
                "freerdp_check_event_handles() failed".to_string(),
            ));
        }

        Ok(true)
    }

    /// Get file descriptors for event polling
    ///
    /// Returns a vector of file descriptors that should be monitored for read events.
    #[allow(dead_code)]
    pub fn get_event_handles(&self) -> Vec<i32> {
        // Note: freerdp_get_event_handles has a complex signature that varies by version.
        // For now, we return an empty vector and rely on polling in check_events().
        // A full implementation would need to handle platform-specific event handles.
        Vec::new()
    }

    /// Get the current framebuffer as RGBA bytes using pooled buffer
    ///
    /// Returns None if not connected or framebuffer is not available.
    /// Uses buffer pool for zero-allocation in hot path.
    pub fn get_framebuffer(&self) -> Option<BytesMut> {
        if !self.is_connected() {
            return None;
        }

        unsafe {
            let context = (*self.instance).context;
            if context.is_null() {
                return None;
            }

            let gdi = (*context).gdi;
            if gdi.is_null() {
                return None;
            }

            let primary = (*gdi).primary;
            if primary.is_null() {
                return None;
            }

            let bitmap = (*primary).bitmap;
            if bitmap.is_null() {
                return None;
            }

            let data = (*bitmap).data;
            if data.is_null() {
                return None;
            }

            let width = (*gdi).width as usize;
            let height = (*gdi).height as usize;
            let stride = (*bitmap).scanline as usize;

            // Acquire buffer from pool (zero allocation in steady state)
            let mut rgba = self.buffer_pool.acquire();
            let required_size = width * height * 4;
            rgba.reserve(required_size);

            // FreeRDP uses BGRA format, we need to convert to RGBA
            // Use optimized row-based conversion
            for y in 0..height {
                let row = data.add(y * stride);
                for x in 0..width {
                    let pixel = row.add(x * 4);
                    let b = *pixel;
                    let g = *pixel.add(1);
                    let r = *pixel.add(2);
                    let a = *pixel.add(3);
                    rgba.extend_from_slice(&[r, g, b, a]);
                }
            }

            Some(rgba)
        }
    }

    /// Return a framebuffer buffer to the pool for reuse
    ///
    /// Call this after you're done with a framebuffer from get_framebuffer()
    /// to avoid allocations on subsequent calls.
    pub fn release_framebuffer(&self, buf: BytesMut) {
        self.buffer_pool.release(buf);
    }

    /// Get buffer pool statistics
    pub fn buffer_pool_stats(&self) -> guacr_terminal::BufferPoolStats {
        self.buffer_pool.stats()
    }

    /// Get mutable reference to the internal framebuffer for dirty rect tracking
    pub fn framebuffer_mut(&mut self) -> Option<&mut FrameBuffer> {
        self.framebuffer.as_mut()
    }

    /// Get reference to the internal framebuffer
    pub fn framebuffer(&self) -> Option<&FrameBuffer> {
        self.framebuffer.as_ref()
    }

    /// Get framebuffer dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Send keyboard input
    pub fn send_key(&mut self, scancode: u16, pressed: bool) -> Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        unsafe {
            let context = (*self.instance).context;
            if context.is_null() {
                return Ok(());
            }

            let input = (*context).input;
            if input.is_null() {
                return Ok(());
            }

            // FreeRDP 3 uses freerdp_input_send_keyboard_event with flags and u8 scancode
            let flags: u16 = if pressed { 0 } else { 0x8000 }; // KBD_FLAGS_RELEASE
            let code: u8 = (scancode & 0xFF) as u8;
            ffi::freerdp_input_send_keyboard_event(input, flags, code);
        }

        Ok(())
    }

    /// Send mouse input
    pub fn send_mouse(&mut self, x: u16, y: u16, buttons: u16) -> Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        unsafe {
            let context = (*self.instance).context;
            if context.is_null() {
                return Ok(());
            }

            let input = (*context).input;
            if input.is_null() {
                return Ok(());
            }

            // PTR_FLAGS_MOVE = 0x0800
            let flags = 0x0800 | buttons;
            ffi::freerdp_input_send_mouse_event(input, flags, x, y);
        }

        Ok(())
    }

    /// Get mutable reference to clipboard handler
    pub fn clipboard_mut(&mut self) -> &mut ClipboardHandler {
        &mut self.clipboard
    }

    /// Get reference to settings
    pub fn settings(&self) -> &RdpSettings {
        &self.settings
    }
}

impl Drop for FreeRdpClient {
    fn drop(&mut self) {
        if self.is_connected() {
            self.disconnect();
        }

        unsafe {
            if !self.instance.is_null() {
                let context = (*self.instance).context;
                if !context.is_null() {
                    ffi::freerdp_context_free(self.instance);
                }
                ffi::freerdp_free(self.instance);
            }
        }
    }
}
