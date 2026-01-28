#[cfg(feature = "terminal-state")]
use guacr_terminal::TerminalEmulator;
#[cfg(feature = "terminal-state")]
use regex::Regex;
use std::time::{Duration, Instant};

/// Extract terminal state for AI analysis
///
/// Provides rich context beyond just command text, including:
/// - Screen contents (what user sees)
/// - Command history and output
/// - User behavior metrics (typing speed, patterns)
/// - Navigation history
pub struct TerminalStateExtractor {
    /// Session start time
    session_start: Instant,

    /// Command timestamps for rate calculation
    command_timestamps: Vec<Instant>,

    /// Keystroke timestamps for typing speed
    keystroke_timestamps: Vec<Instant>,

    /// Correction count (backspaces)
    correction_count: u32,

    /// Navigation history (cd commands)
    navigation_history: Vec<String>,
}

impl TerminalStateExtractor {
    pub fn new() -> Self {
        Self {
            session_start: Instant::now(),
            command_timestamps: Vec::new(),
            keystroke_timestamps: Vec::new(),
            correction_count: 0,
            navigation_history: Vec::new(),
        }
    }

    /// Extract current terminal state for AI analysis
    ///
    /// # Arguments
    ///
    /// * `terminal` - Terminal emulator instance
    /// * `command` - Current command being typed/executed
    /// * `command_history` - Recent command history
    ///
    /// # Returns
    ///
    /// TerminalStateContext with full state information
    #[cfg(feature = "terminal-state")]
    pub fn extract_state(
        &self,
        terminal: &TerminalEmulator,
        command: &str,
        command_history: &[String],
    ) -> TerminalStateContext {
        // Get screen contents (visible text)
        let screen_contents = self.extract_screen_contents(terminal);

        // Parse current directory from prompt
        let current_directory = self.parse_current_directory(&screen_contents);

        // Get recent output (last 10 lines before prompt)
        let recent_output = self.extract_recent_output(terminal);

        // Calculate behavior metrics
        let behavior = self.calculate_behavior_metrics();

        TerminalStateContext {
            command: command.to_string(),
            screen_contents,
            current_directory,
            cursor_position: terminal.screen().cursor_position(),
            command_history: command_history.to_vec(),
            recent_output,
            scrollback: self.extract_scrollback(terminal),
            behavior,
        }
    }

    /// Extract visible screen contents as text
    #[cfg(feature = "terminal-state")]
    fn extract_screen_contents(&self, terminal: &TerminalEmulator) -> String {
        let screen = terminal.screen();
        let (rows, cols) = screen.size();

        let mut contents = String::new();
        for row in 0..rows {
            let mut line = String::new();
            for col in 0..cols {
                if let Some(cell) = screen.cell(row, col) {
                    line.push_str(&cell.contents());
                }
            }
            // Trim trailing whitespace but preserve line structure
            contents.push_str(&line.trim_end());
            contents.push('\n');
        }

        contents
    }

    /// Parse current directory from prompt
    ///
    /// Looks for common prompt patterns:
    /// - user@host:/path$
    /// - [/path]
    /// - ~/path
    #[cfg(feature = "terminal-state")]
    fn parse_current_directory(&self, screen_contents: &str) -> Option<String> {
        // Find the last line (current prompt)
        let last_line = screen_contents.lines().last()?;

        // Common prompt patterns
        let patterns = [
            r"[^:]+:([^\$#>]+)[\$#>]", // user@host:/path$
            r"\[([^\]]+)\]",           // [/path]
            r"~/([^\s]+)",             // ~/path
            r"^([/~][^\s$#>]+)",       // /path or ~/path at start
        ];

        for pattern in &patterns {
            if let Ok(re) = Regex::new(pattern) {
                if let Some(caps) = re.captures(last_line) {
                    if let Some(dir) = caps.get(1) {
                        let dir_str = dir.as_str().trim();
                        if !dir_str.is_empty() {
                            return Some(dir_str.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    /// Extract recent terminal output (before current prompt)
    #[cfg(feature = "terminal-state")]
    fn extract_recent_output(&self, terminal: &TerminalEmulator) -> Vec<String> {
        let screen = terminal.screen();
        let (rows, _cols) = screen.size();

        let mut output = Vec::new();

        // Get last 10 lines before the current prompt line
        let start_row = if rows > 10 { rows - 10 } else { 0 };
        for row in start_row..rows.saturating_sub(1) {
            let line = self.extract_line(terminal, row);
            if !line.trim().is_empty() {
                output.push(line);
            }
        }

        output
    }

    /// Extract scrollback buffer (for additional context)
    #[cfg(feature = "terminal-state")]
    fn extract_scrollback(&self, terminal: &TerminalEmulator) -> Vec<String> {
        let mut scrollback = Vec::new();

        // Get scrollback lines (up to 100 lines)
        let scrollback_count = terminal.scrollback_lines().min(100);
        for i in 0..scrollback_count {
            if let Some(line) = terminal.get_scrollback_line(i) {
                // Convert cells to text
                let text: String = line.cells.iter().map(|cell| cell.contents()).collect();
                scrollback.push(text.trim_end().to_string());
            }
        }

        scrollback
    }

    /// Extract a single line from terminal
    #[cfg(feature = "terminal-state")]
    fn extract_line(&self, terminal: &TerminalEmulator, row: u16) -> String {
        let screen = terminal.screen();
        let (_rows, cols) = screen.size();

        let mut line = String::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                line.push_str(&cell.contents());
            }
        }

        line.trim_end().to_string()
    }

    /// Calculate user behavior metrics
    #[cfg(feature = "terminal-state")]
    fn calculate_behavior_metrics(&self) -> UserBehaviorMetrics {
        let session_duration = self.session_start.elapsed();

        // Calculate command rate (commands per minute)
        let command_rate = if !self.command_timestamps.is_empty() {
            let duration_mins = session_duration.as_secs_f64() / 60.0;
            self.command_timestamps.len() as f64 / duration_mins.max(0.01)
        } else {
            0.0
        };

        // Calculate average time between commands
        let avg_command_interval = if self.command_timestamps.len() > 1 {
            let total_time: Duration = self
                .command_timestamps
                .windows(2)
                .map(|w| w[1].duration_since(w[0]))
                .sum();
            total_time / (self.command_timestamps.len() - 1) as u32
        } else {
            Duration::from_secs(0)
        };

        // Calculate typing speed (chars per second)
        let typing_speed = if !self.keystroke_timestamps.is_empty() {
            let duration_secs = session_duration.as_secs_f64();
            self.keystroke_timestamps.len() as f64 / duration_secs.max(0.01)
        } else {
            0.0
        };

        UserBehaviorMetrics {
            session_duration,
            command_rate,
            avg_command_interval,
            typing_speed,
            correction_count: self.correction_count,
            navigation_pattern: self.navigation_history.clone(),
        }
    }

    /// Record a command execution
    pub fn record_command(&mut self, command: &str) {
        self.command_timestamps.push(Instant::now());

        // Track navigation commands
        if command.starts_with("cd ") {
            self.navigation_history.push(command.to_string());

            // Keep only last 20 navigation commands
            if self.navigation_history.len() > 20 {
                self.navigation_history.remove(0);
            }
        }
    }

    /// Record a keystroke
    pub fn record_keystroke(&mut self, is_backspace: bool) {
        self.keystroke_timestamps.push(Instant::now());

        if is_backspace {
            self.correction_count += 1;
        }

        // Keep only last 1000 keystroke timestamps (for memory efficiency)
        if self.keystroke_timestamps.len() > 1000 {
            self.keystroke_timestamps.drain(0..100);
        }
    }

    /// Get session duration
    pub fn session_duration(&self) -> Duration {
        self.session_start.elapsed()
    }

    /// Get command count
    pub fn command_count(&self) -> usize {
        self.command_timestamps.len()
    }

    /// Get keystroke count
    pub fn keystroke_count(&self) -> usize {
        self.keystroke_timestamps.len()
    }
}

impl Default for TerminalStateExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Terminal state context for AI analysis
#[derive(Debug, Clone)]
pub struct TerminalStateContext {
    /// Current command being typed/executed
    pub command: String,

    /// Current screen contents (visible text)
    pub screen_contents: String,

    /// Current working directory (parsed from prompt)
    pub current_directory: Option<String>,

    /// Cursor position (row, col)
    pub cursor_position: (u16, u16),

    /// Recent command history (from this session)
    pub command_history: Vec<String>,

    /// Recent terminal output (last N lines)
    pub recent_output: Vec<String>,

    /// Scrollback buffer (for context)
    pub scrollback: Vec<String>,

    /// User behavior metrics
    pub behavior: UserBehaviorMetrics,
}

impl TerminalStateContext {
    /// Convert to JSON for BAML API
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": self.command,
            "screen_contents": self.screen_contents,
            "current_directory": self.current_directory,
            "cursor_position": {
                "row": self.cursor_position.0,
                "col": self.cursor_position.1,
            },
            "command_history": self.command_history,
            "recent_output": self.recent_output,
            "scrollback_lines": self.scrollback.len(),
            "behavior": {
                "session_duration_seconds": self.behavior.session_duration.as_secs(),
                "command_rate": self.behavior.command_rate,
                "avg_command_interval_seconds": self.behavior.avg_command_interval.as_secs_f64(),
                "typing_speed": self.behavior.typing_speed,
                "correction_count": self.behavior.correction_count,
                "navigation_pattern": self.behavior.navigation_pattern,
            }
        })
    }
}

/// User behavior metrics
#[derive(Debug, Clone)]
pub struct UserBehaviorMetrics {
    /// Time since session started
    pub session_duration: Duration,

    /// Commands per minute
    pub command_rate: f64,

    /// Average time between commands
    pub avg_command_interval: Duration,

    /// Typing speed (chars per second)
    pub typing_speed: f64,

    /// Number of backspaces/corrections
    pub correction_count: u32,

    /// Navigation pattern (cd commands)
    pub navigation_pattern: Vec<String>,
}

impl UserBehaviorMetrics {
    /// Check if behavior is suspicious (automated/bot-like)
    pub fn is_suspicious(&self) -> bool {
        // Too fast typing (>20 chars/sec is superhuman)
        if self.typing_speed > 20.0 {
            return true;
        }

        // Too many commands per minute (>30 is suspicious)
        if self.command_rate > 30.0 {
            return true;
        }

        // No corrections in a long session (bots don't make mistakes)
        if self.session_duration > Duration::from_secs(60)
            && self.correction_count == 0
            && self.command_rate > 5.0
        {
            return true;
        }

        false
    }

    /// Check if user seems confused (many errors/corrections)
    pub fn is_confused(&self) -> bool {
        // High correction rate
        if self.correction_count > 20 && self.session_duration < Duration::from_secs(120) {
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_state_extractor_new() {
        let extractor = TerminalStateExtractor::new();
        assert_eq!(extractor.command_count(), 0);
        assert_eq!(extractor.keystroke_count(), 0);
    }

    #[test]
    fn test_record_command() {
        let mut extractor = TerminalStateExtractor::new();
        extractor.record_command("ls -la");
        assert_eq!(extractor.command_count(), 1);

        extractor.record_command("cd /tmp");
        assert_eq!(extractor.command_count(), 2);
        assert_eq!(extractor.navigation_history.len(), 1);
    }

    #[test]
    fn test_record_keystroke() {
        let mut extractor = TerminalStateExtractor::new();
        extractor.record_keystroke(false);
        extractor.record_keystroke(false);
        extractor.record_keystroke(true); // backspace
        assert_eq!(extractor.keystroke_count(), 3);
        assert_eq!(extractor.correction_count, 1);
    }

    #[test]
    #[cfg(feature = "terminal-state")]
    fn test_parse_current_directory() {
        let extractor = TerminalStateExtractor::new();

        // Test user@host:/path$ format
        let screen = "user@host:/home/user$ ";
        let dir = extractor.parse_current_directory(screen);
        assert_eq!(dir, Some("/home/user".to_string()));

        // Test [/path] format
        let screen = "[/tmp] $ ";
        let dir = extractor.parse_current_directory(screen);
        assert_eq!(dir, Some("/tmp".to_string()));
    }

    #[test]
    fn test_behavior_metrics_suspicious() {
        let metrics = UserBehaviorMetrics {
            session_duration: Duration::from_secs(120),
            command_rate: 35.0, // Too high
            avg_command_interval: Duration::from_secs(1),
            typing_speed: 15.0,
            correction_count: 0,
            navigation_pattern: vec![],
        };

        assert!(metrics.is_suspicious());
    }

    #[test]
    fn test_behavior_metrics_confused() {
        let metrics = UserBehaviorMetrics {
            session_duration: Duration::from_secs(60),
            command_rate: 5.0,
            avg_command_interval: Duration::from_secs(10),
            typing_speed: 3.0,
            correction_count: 25, // Many corrections
            navigation_pattern: vec![],
        };

        assert!(metrics.is_confused());
    }

    #[test]
    fn test_terminal_state_context_to_json() {
        let context = TerminalStateContext {
            command: "ls -la".to_string(),
            screen_contents: "$ ls -la\n".to_string(),
            current_directory: Some("/home/user".to_string()),
            cursor_position: (10, 5),
            command_history: vec!["pwd".to_string(), "cd /tmp".to_string()],
            recent_output: vec!["file1.txt".to_string(), "file2.txt".to_string()],
            scrollback: vec![],
            behavior: UserBehaviorMetrics {
                session_duration: Duration::from_secs(60),
                command_rate: 5.0,
                avg_command_interval: Duration::from_secs(10),
                typing_speed: 3.5,
                correction_count: 2,
                navigation_pattern: vec!["cd /tmp".to_string()],
            },
        };

        let json = context.to_json();
        assert_eq!(json["command"], "ls -la");
        assert_eq!(json["current_directory"], "/home/user");
        assert_eq!(json["behavior"]["command_rate"], 5.0);
    }
}
