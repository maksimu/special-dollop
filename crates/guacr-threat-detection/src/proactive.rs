use crate::approval::{ApprovalDecision, ApprovalManager};
use crate::command_buffer::CommandBuffer;
use crate::threat::ThreatLevel;
use log::{debug, trace};

/// Result of proactive input handling
#[derive(Debug)]
pub enum ProactiveResult {
    /// Command buffered, waiting for Enter key
    Buffered,
    /// Command approved by AI, ready to send to target
    Approved(Vec<u8>),
    /// Command blocked by AI, should not be sent
    Blocked {
        reason: String,
        threat_level: ThreatLevel,
    },
    /// AI approval timed out
    Timeout,
}

/// X11 keysym constants
pub const KEYSYM_RETURN: u32 = 65293; // Enter key
pub const KEYSYM_BACKSPACE: u32 = 65288; // Backspace
pub const KEYSYM_DELETE: u32 = 65535; // Delete
pub const KEYSYM_CTRL_C: u32 = 99; // 'c' (with Ctrl modifier)
pub const KEYSYM_CTRL_D: u32 = 100; // 'd' (with Ctrl modifier)
pub const KEYSYM_CTRL_Z: u32 = 122; // 'z' (with Ctrl modifier)

/// Handle proactive input with AI approval
///
/// This function buffers keystrokes until a complete command is detected (Enter key),
/// then requests AI approval before sending to the target system.
///
/// # Arguments
///
/// * `buffer` - Command buffer to accumulate keystrokes
/// * `bytes` - Raw bytes from keyboard input
/// * `keysym` - X11 keysym for the key event
/// * `ctrl_pressed` - Whether Ctrl modifier is pressed
/// * `approval_manager` - Approval manager for AI requests
/// * `session_id` - Unique session identifier
/// * `username` - Username for context
/// * `hostname` - Hostname for context
/// * `protocol` - Protocol name (ssh, telnet, etc.)
/// * `auto_approve_safe` - Whether to auto-approve safe commands
///
/// # Returns
///
/// ProactiveResult indicating what action to take
#[allow(clippy::too_many_arguments)]
pub async fn handle_proactive_input(
    buffer: &mut CommandBuffer,
    bytes: &[u8],
    keysym: u32,
    ctrl_pressed: bool,
    approval_manager: &ApprovalManager,
    session_id: &str,
    username: &str,
    hostname: &str,
    protocol: &str,
    auto_approve_safe: bool,
) -> ProactiveResult {
    trace!(
        "Proactive input: keysym={}, ctrl={}, bytes={:?}",
        keysym,
        ctrl_pressed,
        bytes
    );

    // Handle special keys
    match keysym {
        KEYSYM_RETURN => {
            // Enter key - complete command, request approval
            if buffer.is_empty() {
                // Empty command (just Enter) - allow immediately
                debug!("Empty command (Enter only) - auto-approving");
                return ProactiveResult::Approved(bytes.to_vec());
            }

            let command = buffer.as_str().to_string();
            debug!("Complete command detected: {}", command);

            // Check if command should be auto-approved (safe commands)
            if auto_approve_safe && approval_manager.should_auto_approve(&command) {
                debug!("Auto-approving safe command: {}", command);
                let mut full_command = buffer.as_bytes().to_vec();
                full_command.extend_from_slice(bytes); // Add Enter key
                buffer.clear();
                return ProactiveResult::Approved(full_command);
            }

            // Mark as pending approval
            buffer.set_pending_approval(true);

            // Request AI approval
            match approval_manager
                .request_approval(session_id, &command, username, hostname, protocol)
                .await
            {
                Ok(ApprovalDecision::Approved) => {
                    debug!("Command approved by AI: {}", command);
                    // Approved - send buffered command + Enter
                    let mut full_command = buffer.as_bytes().to_vec();
                    full_command.extend_from_slice(bytes); // Add Enter key
                    buffer.clear();
                    ProactiveResult::Approved(full_command)
                }
                Ok(ApprovalDecision::Blocked {
                    reason,
                    threat_level,
                }) => {
                    debug!("Command blocked by AI: {} - Reason: {}", command, reason);
                    // Blocked - clear buffer and notify user
                    buffer.clear();
                    ProactiveResult::Blocked {
                        reason,
                        threat_level,
                    }
                }
                Err(e) => {
                    // Error during approval - timeout or other error
                    debug!("Approval error: {}", e);
                    buffer.clear();
                    ProactiveResult::Timeout
                }
            }
        }

        KEYSYM_BACKSPACE | KEYSYM_DELETE => {
            // Backspace/Delete - remove last character from buffer
            if !buffer.is_empty() {
                buffer.backspace();
                trace!("Backspace - buffer now: {}", buffer.as_str());
            }
            // Always send backspace to target (for visual feedback)
            ProactiveResult::Approved(bytes.to_vec())
        }

        KEYSYM_CTRL_C | KEYSYM_CTRL_D | KEYSYM_CTRL_Z if ctrl_pressed => {
            // Interrupt signals (Ctrl+C, Ctrl+D, Ctrl+Z) - clear buffer and allow immediately
            debug!("Interrupt signal detected - clearing buffer and allowing");
            buffer.clear();
            ProactiveResult::Approved(bytes.to_vec())
        }

        _ => {
            // Regular character - add to buffer
            buffer.append(bytes);
            trace!("Buffered character - buffer now: {}", buffer.as_str());

            // Check if buffer has expired (prevent hanging)
            if buffer.is_expired() {
                debug!(
                    "Buffer expired after {:?} - auto-approving",
                    buffer.elapsed()
                );
                let full_command = buffer.as_bytes().to_vec();
                buffer.clear();
                return ProactiveResult::Approved(full_command);
            }

            // Send character to target immediately for visual feedback
            // (user sees what they type, but command isn't executed until approved)
            ProactiveResult::Approved(bytes.to_vec())
        }
    }
}

/// Parse threat detection configuration from connection parameters
///
/// Extracts proactive mode settings from connection parameters.
pub fn parse_proactive_config(
    params: &std::collections::HashMap<String, String>,
) -> ProactiveConfig {
    ProactiveConfig {
        enabled: params
            .get("threat_detection_proactive_mode")
            .map(|s| s == "true")
            .unwrap_or(false),
        approval_timeout_ms: params
            .get("threat_detection_approval_timeout_ms")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(2000),
        fail_closed_on_error: params
            .get("threat_detection_fail_closed_on_error")
            .map(|s| s == "true")
            .unwrap_or(false),
        show_approval_status: params
            .get("threat_detection_show_approval_status")
            .map(|s| s == "true")
            .unwrap_or(true),
        auto_approve_safe_commands: params
            .get("threat_detection_auto_approve_safe_commands")
            .map(|s| s == "true")
            .unwrap_or(true),
    }
}

/// Proactive mode configuration
#[derive(Debug, Clone)]
pub struct ProactiveConfig {
    pub enabled: bool,
    pub approval_timeout_ms: u64,
    pub fail_closed_on_error: bool,
    pub show_approval_status: bool,
    pub auto_approve_safe_commands: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::{ThreatDetector, ThreatDetectorConfig};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn test_proactive_input_empty_command() {
        let config = ThreatDetectorConfig::default();
        let detector = Arc::new(ThreatDetector::new(config).unwrap());
        let manager = ApprovalManager::new(detector, Duration::from_secs(5), false);
        let mut buffer = CommandBuffer::new();

        let result = handle_proactive_input(
            &mut buffer,
            b"\n",
            KEYSYM_RETURN,
            false,
            &manager,
            "session-1",
            "user",
            "host",
            "ssh",
            true,
        )
        .await;

        match result {
            ProactiveResult::Approved(_) => {
                assert!(buffer.is_empty());
            }
            _ => panic!("Expected Approved for empty command"),
        }
    }

    #[tokio::test]
    async fn test_proactive_input_buffering() {
        let config = ThreatDetectorConfig::default();
        let detector = Arc::new(ThreatDetector::new(config).unwrap());
        let manager = ApprovalManager::new(detector, Duration::from_secs(5), false);
        let mut buffer = CommandBuffer::new();

        // Type "ls"
        let result = handle_proactive_input(
            &mut buffer,
            b"l",
            108, // 'l'
            false,
            &manager,
            "session-1",
            "user",
            "host",
            "ssh",
            true,
        )
        .await;

        match result {
            ProactiveResult::Approved(_) => {
                assert_eq!(buffer.as_str(), "l");
            }
            _ => panic!("Expected Approved for regular character"),
        }

        let result = handle_proactive_input(
            &mut buffer,
            b"s",
            115, // 's'
            false,
            &manager,
            "session-1",
            "user",
            "host",
            "ssh",
            true,
        )
        .await;

        match result {
            ProactiveResult::Approved(_) => {
                assert_eq!(buffer.as_str(), "ls");
            }
            _ => panic!("Expected Approved for regular character"),
        }
    }

    #[tokio::test]
    async fn test_proactive_input_backspace() {
        let config = ThreatDetectorConfig::default();
        let detector = Arc::new(ThreatDetector::new(config).unwrap());
        let manager = ApprovalManager::new(detector, Duration::from_secs(5), false);
        let mut buffer = CommandBuffer::new();

        buffer.append_str("ls");

        let result = handle_proactive_input(
            &mut buffer,
            &[0x7f], // Backspace byte
            KEYSYM_BACKSPACE,
            false,
            &manager,
            "session-1",
            "user",
            "host",
            "ssh",
            true,
        )
        .await;

        match result {
            ProactiveResult::Approved(_) => {
                assert_eq!(buffer.as_str(), "l");
            }
            _ => panic!("Expected Approved for backspace"),
        }
    }

    #[tokio::test]
    async fn test_proactive_input_ctrl_c() {
        let config = ThreatDetectorConfig::default();
        let detector = Arc::new(ThreatDetector::new(config).unwrap());
        let manager = ApprovalManager::new(detector, Duration::from_secs(5), false);
        let mut buffer = CommandBuffer::new();

        buffer.append_str("ls -la");

        let result = handle_proactive_input(
            &mut buffer,
            &[0x03], // Ctrl+C byte
            KEYSYM_CTRL_C,
            true,
            &manager,
            "session-1",
            "user",
            "host",
            "ssh",
            true,
        )
        .await;

        match result {
            ProactiveResult::Approved(_) => {
                assert!(buffer.is_empty());
            }
            _ => panic!("Expected Approved for Ctrl+C"),
        }
    }

    #[test]
    fn test_parse_proactive_config() {
        let mut params = std::collections::HashMap::new();
        params.insert(
            "threat_detection_proactive_mode".to_string(),
            "true".to_string(),
        );
        params.insert(
            "threat_detection_approval_timeout_ms".to_string(),
            "5000".to_string(),
        );
        params.insert(
            "threat_detection_fail_closed_on_error".to_string(),
            "true".to_string(),
        );

        let config = parse_proactive_config(&params);
        assert!(config.enabled);
        assert_eq!(config.approval_timeout_ms, 5000);
        assert!(config.fail_closed_on_error);
    }
}
