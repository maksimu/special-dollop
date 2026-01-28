use crate::detector::ThreatDetector;
use crate::threat::{ThreatLevel, ThreatResult};
use crate::Result;
use log::{debug, error, warn};
use std::sync::Arc;
use std::time::Duration;

/// Approval decision for a command
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Command approved - can be executed
    Approved,
    /// Command blocked - should not be executed
    Blocked {
        reason: String,
        threat_level: ThreatLevel,
    },
}

impl ApprovalDecision {
    /// Check if command is approved
    pub fn is_approved(&self) -> bool {
        matches!(self, ApprovalDecision::Approved)
    }

    /// Check if command is blocked
    pub fn is_blocked(&self) -> bool {
        matches!(self, ApprovalDecision::Blocked { .. })
    }

    /// Create approval decision from threat result
    pub fn from_threat_result(threat: ThreatResult) -> Self {
        if threat.should_terminate() {
            ApprovalDecision::Blocked {
                reason: threat.description,
                threat_level: threat.level,
            }
        } else {
            ApprovalDecision::Approved
        }
    }
}

/// Approval manager for proactive threat detection
///
/// Manages AI approval requests and responses for commands before execution.
///
/// Lock-free design:
/// - No shared state (stateless)
/// - Each request is independent
/// - No deadlock risk
pub struct ApprovalManager {
    /// Threat detector instance (Arc for cheap cloning)
    detector: Arc<ThreatDetector>,
    /// Approval timeout
    timeout: Duration,
    /// Fail-closed on error (block commands if AI unavailable)
    fail_closed_on_error: bool,
}

impl ApprovalManager {
    /// Create a new approval manager
    pub fn new(
        detector: Arc<ThreatDetector>,
        timeout: Duration,
        fail_closed_on_error: bool,
    ) -> Self {
        Self {
            detector,
            timeout,
            fail_closed_on_error,
        }
    }

    /// Request approval for a command
    ///
    /// This is the main entry point for proactive threat detection.
    /// Blocks until AI responds or timeout occurs.
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique session identifier
    /// * `command` - Command to approve
    /// * `username` - Username for context
    /// * `hostname` - Hostname for context
    /// * `protocol` - Protocol name (ssh, telnet, etc.)
    ///
    /// # Returns
    ///
    /// ApprovalDecision indicating if command should be executed
    pub async fn request_approval(
        &self,
        session_id: &str,
        command: &str,
        username: &str,
        hostname: &str,
        protocol: &str,
    ) -> Result<ApprovalDecision> {
        debug!(
            "Approval requested for session {} command: {}",
            session_id, command
        );

        // Use the threat detector's analyze_keystroke_sequence method
        // which already handles tag checking and BAML API calls
        match tokio::time::timeout(
            self.timeout,
            self.detector
                .analyze_keystroke_sequence(session_id, command, username, hostname, protocol),
        )
        .await
        {
            Ok(Ok(threat_result)) => {
                let decision = ApprovalDecision::from_threat_result(threat_result);

                match &decision {
                    ApprovalDecision::Approved => {
                        debug!("Command approved: {}", command);
                    }
                    ApprovalDecision::Blocked {
                        reason,
                        threat_level,
                    } => {
                        warn!(
                            "Command blocked ({:?}): {} - Reason: {}",
                            threat_level, command, reason
                        );
                    }
                }

                Ok(decision)
            }
            Ok(Err(e)) => {
                error!("Error during approval request: {}", e);
                // Fail-open or fail-closed based on config
                if self.fail_closed_on_error {
                    Ok(ApprovalDecision::Blocked {
                        reason: format!("AI service error: {}", e),
                        threat_level: ThreatLevel::High,
                    })
                } else {
                    warn!("Failing open due to AI error: {}", e);
                    Ok(ApprovalDecision::Approved)
                }
            }
            Err(_) => {
                error!("Approval request timed out after {:?}", self.timeout);
                // Fail-open or fail-closed based on config
                if self.fail_closed_on_error {
                    Ok(ApprovalDecision::Blocked {
                        reason: "AI approval timeout".to_string(),
                        threat_level: ThreatLevel::High,
                    })
                } else {
                    warn!("Failing open due to timeout");
                    Ok(ApprovalDecision::Approved)
                }
            }
        }
    }

    /// Check if a command should be auto-approved without AI call
    ///
    /// Auto-approves simple safe commands like ls, pwd, cd, etc.
    /// This reduces latency and API calls for common commands.
    pub fn should_auto_approve(&self, command: &str) -> bool {
        let cmd = command.trim().to_lowercase();

        // List of safe commands that can be auto-approved
        let safe_commands = [
            "ls", "ll", "pwd", "cd", "echo", "cat", "less", "more", "head", "tail", "grep", "find",
            "which", "whoami", "hostname", "date", "uptime", "w", "who", "ps", "top", "df", "du",
            "free", "uname", "clear", "history", "exit", "logout",
        ];

        // Check if command starts with any safe command
        for safe_cmd in &safe_commands {
            if cmd.starts_with(safe_cmd) {
                // Make sure it's followed by space or end of string (not part of larger command)
                if cmd.len() == safe_cmd.len()
                    || cmd.chars().nth(safe_cmd.len()) == Some(' ')
                    || cmd.chars().nth(safe_cmd.len()) == Some('\t')
                {
                    debug!("Auto-approving safe command: {}", command);
                    return true;
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::ThreatDetectorConfig;

    #[test]
    fn test_approval_decision_from_threat_result() {
        let threat = ThreatResult {
            level: ThreatLevel::Critical,
            confidence: 0.95,
            description: "Dangerous command".to_string(),
            action: "terminate".to_string(),
            metadata: serde_json::Value::Null,
        };

        let decision = ApprovalDecision::from_threat_result(threat);
        assert!(decision.is_blocked());
    }

    #[test]
    fn test_approval_decision_approved() {
        let threat = ThreatResult {
            level: ThreatLevel::Low,
            confidence: 0.1,
            description: "Safe command".to_string(),
            action: "continue".to_string(),
            metadata: serde_json::Value::Null,
        };

        let decision = ApprovalDecision::from_threat_result(threat);
        assert!(decision.is_approved());
    }

    #[tokio::test]
    async fn test_approval_manager_auto_approve() {
        let config = ThreatDetectorConfig::default();
        let detector = Arc::new(ThreatDetector::new(config).unwrap());
        let manager = ApprovalManager::new(detector, Duration::from_secs(5), false);

        assert!(manager.should_auto_approve("ls"));
        assert!(manager.should_auto_approve("ls -la"));
        assert!(manager.should_auto_approve("pwd"));
        assert!(manager.should_auto_approve("cd /tmp"));

        assert!(!manager.should_auto_approve("rm -rf /"));
        assert!(!manager.should_auto_approve("sudo rm"));
        assert!(!manager.should_auto_approve("lsblk")); // Not in safe list
    }
}
