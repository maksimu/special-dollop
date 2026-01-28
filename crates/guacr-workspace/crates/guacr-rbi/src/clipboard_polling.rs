// Clipboard polling for browser → client sync
// Alternative to KCM's CEF patch - uses JavaScript polling
//
// KCM patches CEF to add cef_kcm_add_clipboard_observer() which hooks into
// Chromium's internal clipboard. Without that patch, we can use JavaScript
// to poll the clipboard periodically.
//
// This is slightly less efficient but doesn't require Chrome patches.

// Imports for future async polling implementation
// use log::{debug, warn};
// use std::sync::{Arc, Mutex};
// use std::time::Duration;
// use tokio::time::interval;

/// Clipboard polling configuration
#[derive(Debug, Clone)]
pub struct ClipboardPollingConfig {
    /// Enable clipboard polling (browser → client)
    pub enabled: bool,
    /// Polling interval in milliseconds
    pub interval_ms: u64,
    /// Maximum clipboard size to sync
    pub max_size: usize,
}

impl Default for ClipboardPollingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_ms: 500,      // Poll every 500ms
            max_size: 1024 * 1024, // 1MB max
        }
    }
}

/// JavaScript code to read clipboard
///
/// This is injected into the page to read clipboard contents.
/// Note: Requires page focus and user gesture for security.
pub const CLIPBOARD_READ_JS: &str = r#"
(async function() {
    try {
        // Check if clipboard API is available and we have focus
        if (!navigator.clipboard || !document.hasFocus()) {
            return null;
        }
        
        // Try to read text from clipboard
        const text = await navigator.clipboard.readText();
        return text;
    } catch (e) {
        // Permission denied or not focused
        return null;
    }
})()
"#;

/// JavaScript code to write clipboard
pub const CLIPBOARD_WRITE_JS: &str = r#"
(async function(text) {
    try {
        await navigator.clipboard.writeText(text);
        return true;
    } catch (e) {
        return false;
    }
})
"#;

/// Alternative: Use document selection for copy
/// This works without focus requirements in some cases
pub const SELECTION_COPY_JS: &str = r#"
(function() {
    try {
        // Get current selection
        const selection = window.getSelection();
        if (selection && selection.rangeCount > 0) {
            return selection.toString();
        }
        return null;
    } catch (e) {
        return null;
    }
})()
"#;

/// Alternative: Monitor copy events via JavaScript
///
/// This injects a listener that captures copy events and stores them.
/// More reliable than polling but requires user to copy.
pub const COPY_EVENT_LISTENER_JS: &str = r#"
(function() {
    if (window.__guacClipboardListener) return;
    
    window.__guacClipboardData = null;
    
    document.addEventListener('copy', function(e) {
        // Get the copied text
        const selection = window.getSelection();
        if (selection) {
            window.__guacClipboardData = {
                text: selection.toString(),
                timestamp: Date.now()
            };
        }
    });
    
    window.__guacClipboardListener = true;
})()
"#;

/// JavaScript to retrieve captured copy data
pub const GET_CLIPBOARD_DATA_JS: &str = r#"
(function() {
    const data = window.__guacClipboardData;
    window.__guacClipboardData = null; // Clear after read
    return data;
})()
"#;

/// Clipboard state for tracking changes
#[derive(Debug, Default)]
pub struct ClipboardState {
    last_content: String,
    last_hash: u64,
}

impl ClipboardState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if content has changed
    pub fn has_changed(&mut self, new_content: &str) -> bool {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        new_content.hash(&mut hasher);
        let new_hash = hasher.finish();

        if new_hash != self.last_hash {
            self.last_content = new_content.to_string();
            self.last_hash = new_hash;
            true
        } else {
            false
        }
    }

    /// Get last known content
    pub fn last_content(&self) -> &str {
        &self.last_content
    }
}

/// Format clipboard data as CDP evaluate call for chromiumoxide
#[cfg(feature = "chrome")]
#[allow(dead_code)]
pub fn format_evaluate_clipboard_read() -> chromiumoxide::cdp::js_protocol::runtime::EvaluateParams
{
    chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
        .expression(CLIPBOARD_READ_JS)
        .await_promise(true)
        .build()
        .expect("Failed to build evaluate params")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_state() {
        let mut state = ClipboardState::new();

        // First content
        assert!(state.has_changed("Hello"));
        assert_eq!(state.last_content(), "Hello");

        // Same content
        assert!(!state.has_changed("Hello"));

        // New content
        assert!(state.has_changed("World"));
        assert_eq!(state.last_content(), "World");
    }

    #[test]
    fn test_config_default() {
        let config = ClipboardPollingConfig::default();
        assert!(config.enabled);
        assert_eq!(config.interval_ms, 500);
    }
}
