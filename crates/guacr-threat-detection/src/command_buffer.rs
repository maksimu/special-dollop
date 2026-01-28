use std::time::{Duration, Instant};

/// Command buffer for proactive threat detection
///
/// Accumulates keystrokes until a complete command is detected (Enter key pressed).
/// Used to buffer commands before sending to AI for approval.
#[derive(Debug, Clone)]
pub struct CommandBuffer {
    /// Accumulated keystrokes for current command
    buffer: String,
    /// Whether we're waiting for AI approval
    pending_approval: bool,
    /// Timestamp when buffering started
    started_at: Option<Instant>,
    /// Maximum buffer time before auto-approval (prevents hanging)
    max_buffer_time: Duration,
}

impl CommandBuffer {
    /// Create a new command buffer with default settings
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            pending_approval: false,
            started_at: None,
            max_buffer_time: Duration::from_secs(60), // 60 seconds max
        }
    }

    /// Create a new command buffer with custom max buffer time
    pub fn with_max_buffer_time(max_buffer_time: Duration) -> Self {
        Self {
            buffer: String::new(),
            pending_approval: false,
            started_at: None,
            max_buffer_time,
        }
    }

    /// Append bytes to the buffer
    ///
    /// Converts bytes to UTF-8 string (lossy) and appends to buffer.
    /// Starts the buffer timer if this is the first character.
    pub fn append(&mut self, bytes: &[u8]) {
        if self.buffer.is_empty() {
            self.started_at = Some(Instant::now());
        }

        if let Ok(s) = std::str::from_utf8(bytes) {
            self.buffer.push_str(s);
        } else {
            // Lossy conversion for non-UTF8 bytes
            let s = String::from_utf8_lossy(bytes);
            self.buffer.push_str(&s);
        }
    }

    /// Append a string to the buffer
    pub fn append_str(&mut self, s: &str) {
        if self.buffer.is_empty() {
            self.started_at = Some(Instant::now());
        }
        self.buffer.push_str(s);
    }

    /// Handle backspace - remove last character
    pub fn backspace(&mut self) {
        self.buffer.pop();
        if self.buffer.is_empty() {
            self.started_at = None;
        }
    }

    /// Get the current buffer as a string slice
    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    /// Get the current buffer as bytes
    pub fn as_bytes(&self) -> &[u8] {
        self.buffer.as_bytes()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get buffer length in bytes
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.pending_approval = false;
        self.started_at = None;
    }

    /// Mark buffer as pending approval
    pub fn set_pending_approval(&mut self, pending: bool) {
        self.pending_approval = pending;
    }

    /// Check if buffer is pending approval
    pub fn is_pending_approval(&self) -> bool {
        self.pending_approval
    }

    /// Check if buffer has exceeded max buffer time
    pub fn is_expired(&self) -> bool {
        if let Some(started) = self.started_at {
            started.elapsed() > self.max_buffer_time
        } else {
            false
        }
    }

    /// Get elapsed time since buffering started
    pub fn elapsed(&self) -> Option<Duration> {
        self.started_at.map(|t| t.elapsed())
    }

    /// Get the buffered command and clear the buffer
    ///
    /// Returns the command string and clears the buffer state.
    pub fn take(&mut self) -> String {
        let command = self.buffer.clone();
        self.clear();
        command
    }
}

impl Default for CommandBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_buffer_new() {
        let buffer = CommandBuffer::new();
        assert!(buffer.is_empty());
        assert!(!buffer.is_pending_approval());
        assert!(buffer.elapsed().is_none());
    }

    #[test]
    fn test_command_buffer_append() {
        let mut buffer = CommandBuffer::new();
        buffer.append(b"ls");
        assert_eq!(buffer.as_str(), "ls");
        assert_eq!(buffer.len(), 2);
        assert!(!buffer.is_empty());
    }

    #[test]
    fn test_command_buffer_append_str() {
        let mut buffer = CommandBuffer::new();
        buffer.append_str("ls");
        buffer.append_str(" -la");
        assert_eq!(buffer.as_str(), "ls -la");
    }

    #[test]
    fn test_command_buffer_backspace() {
        let mut buffer = CommandBuffer::new();
        buffer.append_str("ls");
        buffer.backspace();
        assert_eq!(buffer.as_str(), "l");
        buffer.backspace();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_command_buffer_clear() {
        let mut buffer = CommandBuffer::new();
        buffer.append_str("ls -la");
        buffer.set_pending_approval(true);
        buffer.clear();
        assert!(buffer.is_empty());
        assert!(!buffer.is_pending_approval());
    }

    #[test]
    fn test_command_buffer_take() {
        let mut buffer = CommandBuffer::new();
        buffer.append_str("ls -la");
        let command = buffer.take();
        assert_eq!(command, "ls -la");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_command_buffer_elapsed() {
        let mut buffer = CommandBuffer::new();
        assert!(buffer.elapsed().is_none());
        buffer.append_str("ls");
        assert!(buffer.elapsed().is_some());
        std::thread::sleep(Duration::from_millis(10));
        assert!(buffer.elapsed().unwrap() >= Duration::from_millis(10));
    }

    #[test]
    fn test_command_buffer_expired() {
        let mut buffer = CommandBuffer::with_max_buffer_time(Duration::from_millis(10));
        buffer.append_str("ls");
        assert!(!buffer.is_expired());
        std::thread::sleep(Duration::from_millis(20));
        assert!(buffer.is_expired());
    }
}
