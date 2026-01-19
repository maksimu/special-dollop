use crate::baml_client::{BamlClient, CommandSummaryResponse};
use crate::threat::{ThreatLevel, ThreatResult};
use crate::{Result, ThreatDetectionError};
use log::{debug, error, info, warn};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Threat detector configuration
#[derive(Debug, Clone)]
pub struct ThreatDetectorConfig {
    /// BAML REST API endpoint URL (base URL, functions are appended)
    pub baml_endpoint: String,
    /// Optional API key for BAML authentication
    pub baml_api_key: Option<String>,
    /// Enable threat detection
    pub enabled: bool,
    /// Automatically terminate sessions on high/critical threats
    pub auto_terminate: bool,
    /// Minimum threat level to log (default: Low)
    pub min_log_level: ThreatLevel,
    /// Command history size for context
    pub command_history_size: usize,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
    /// Tag-based rules for immediate termination (deny tags)
    pub deny_tags: HashMap<String, Vec<String>>, // level -> list of regex patterns
    /// Tag-based rules for allowing commands (allow tags)
    pub allow_tags: HashMap<String, Vec<String>>, // level -> list of regex patterns
    /// Enable tag-based checking (immediate termination on deny tag match)
    pub enable_tag_checking: bool,
    /// Enable proactive mode (AI approval before execution)
    pub proactive_mode: bool,
    /// Maximum time to wait for AI approval in proactive mode (milliseconds)
    pub approval_timeout_ms: u64,
    /// Fail-closed on AI errors (block commands if AI unavailable)
    pub fail_closed_on_error: bool,
    /// Show approval status to user (visual feedback)
    pub show_approval_status: bool,
    /// Auto-approve simple safe commands (ls, pwd, cd, etc.)
    pub auto_approve_safe_commands: bool,
}

impl Default for ThreatDetectorConfig {
    fn default() -> Self {
        Self {
            baml_endpoint: "http://localhost:8000/api".to_string(),
            baml_api_key: None,
            enabled: false,
            auto_terminate: true,
            min_log_level: ThreatLevel::Low,
            command_history_size: 10,
            timeout_seconds: 5,
            deny_tags: HashMap::new(),
            allow_tags: HashMap::new(),
            enable_tag_checking: true,
            proactive_mode: false,
            approval_timeout_ms: 2000, // 2 seconds default
            fail_closed_on_error: false,
            show_approval_status: true,
            auto_approve_safe_commands: true,
        }
    }
}

/// Threat detector service
///
/// Monitors session activity and calls BAML REST API to detect threats.
/// Can automatically terminate sessions on threat detection.
///
/// Uses lock-free patterns where possible:
/// - parking_lot::RwLock (faster than std::sync::RwLock)
/// - No nested locks
/// - Short critical sections
///
/// **IMPORTANT**: Must call `cleanup_session()` when session ends to prevent memory leaks.
/// The command history HashMap will grow unbounded if sessions aren't cleaned up.
pub struct ThreatDetector {
    config: ThreatDetectorConfig,
    baml_client: BamlClient,
    /// Command history for context (per session)
    /// Using parking_lot::RwLock (faster than std, no async)
    /// **MUST be cleaned up** when session ends via cleanup_session()
    command_history: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl ThreatDetector {
    /// Create a new threat detector
    pub fn new(config: ThreatDetectorConfig) -> Result<Self> {
        if !config.enabled {
            info!("Threat detection disabled");
            return Ok(Self {
                config: config.clone(),
                baml_client: BamlClient::new(
                    config.baml_endpoint.clone(),
                    config.baml_api_key.clone(),
                    Some(std::time::Duration::from_secs(config.timeout_seconds)),
                ),
                command_history: Arc::new(RwLock::new(HashMap::new())),
            });
        }

        info!(
            "Threat detection enabled - BAML endpoint: {}, auto-terminate: {}",
            config.baml_endpoint, config.auto_terminate
        );

        Ok(Self {
            config: config.clone(),
            baml_client: BamlClient::new(
                config.baml_endpoint.clone(),
                config.baml_api_key.clone(),
                Some(std::time::Duration::from_secs(config.timeout_seconds)),
            ),
            command_history: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Check tag-based rules for immediate termination
    ///
    /// Returns true if a deny tag matches (immediate termination)
    fn check_tags(&self, input: &str) -> Option<ThreatResult> {
        if !self.config.enable_tag_checking {
            return None;
        }

        use regex::Regex;

        // Check deny tags first (highest priority - immediate termination)
        for (level_str, patterns) in &self.config.deny_tags {
            for pattern in patterns {
                if let Ok(re) = Regex::new(pattern) {
                    if re.is_match(input) {
                        let level = match level_str.to_lowercase().as_str() {
                            "critical" => ThreatLevel::Critical,
                            "high" => ThreatLevel::High,
                            "medium" => ThreatLevel::Medium,
                            "low" => ThreatLevel::Low,
                            _ => ThreatLevel::High,
                        };

                        warn!("Deny tag matched: '{}' for command: {}", pattern, input);

                        return Some(ThreatResult {
                            level,
                            confidence: 1.0, // Tag matches are 100% confident
                            description: format!("Deny tag matched: {}", pattern),
                            action: "terminate".to_string(),
                            metadata: serde_json::json!({
                                "tag_matched": pattern,
                                "tag_type": "deny",
                                "level": level_str,
                            }),
                        });
                    }
                }
            }
        }

        // Check allow tags (explicitly allowed commands)
        for patterns in self.config.allow_tags.values() {
            for pattern in patterns {
                if let Ok(re) = Regex::new(pattern) {
                    if re.is_match(input) {
                        debug!("Allow tag matched: '{}' for command: {}", pattern, input);
                        // Allow tags don't terminate, but we log them
                        return None; // Explicitly allowed, no threat
                    }
                }
            }
        }

        None
    }

    /// Analyze keyboard input (keystroke sequence) for threats using BAML ExtractKeystrokeAnalysis
    ///
    /// This receives live keyboard data directly from the SSH handler, not parsed from recording files.
    /// The input is the actual keystroke sequence as typed by the user.
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique session identifier
    /// * `keystroke_sequence` - Raw keystroke sequence from keyboard input (e.g., "ls -la\n")
    /// * `username` - Username for context
    /// * `hostname` - Hostname for context
    /// * `protocol` - Protocol name (ssh, rdp, etc.)
    ///
    /// # Returns
    ///
    /// ThreatResult indicating if threat was detected
    pub async fn analyze_keystroke_sequence(
        &self,
        session_id: &str,
        keystroke_sequence: &str,
        _username: &str,
        _hostname: &str,
        _protocol: &str,
    ) -> Result<ThreatResult> {
        if !self.config.enabled {
            return Ok(ThreatResult::default());
        }

        // Check tag-based rules first (immediate termination)
        if let Some(tag_result) = self.check_tags(keystroke_sequence) {
            return Ok(tag_result);
        }

        // Extract command from keystroke sequence (remove newlines, trim)
        let command = keystroke_sequence
            .trim()
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();

        // Add to command history (store the actual command, not raw keystrokes)
        // Using parking_lot::RwLock (non-async, faster than tokio::sync::RwLock)
        if !command.is_empty() {
            let mut history = self.command_history.write();
            let entry = history.entry(session_id.to_string()).or_default();
            entry.push(command.clone());
            // Keep only last N commands (ring buffer behavior)
            if entry.len() > self.config.command_history_size {
                entry.remove(0);
            }
        }

        // Call BAML ExtractKeystrokeAnalysis function with the actual keystroke sequence
        // BAML will analyze the raw input as typed by the user
        match self
            .baml_client
            .extract_keystroke_analysis(keystroke_sequence)
            .await
        {
            Ok(baml_response) => {
                // Extract first analysis report (BAML returns array)
                if let Some(analysis) = baml_response.analysis_report.first() {
                    let threat_result = self
                        .baml_client
                        .analysis_to_threat_result(analysis, &baml_response.overall_summary);

                    // Log if threat level meets minimum
                    if threat_result.level as u8 >= self.config.min_log_level as u8 {
                        match threat_result.level {
                            ThreatLevel::Critical | ThreatLevel::High => {
                                error!(
                                    "THREAT DETECTED [{}] {} - {} (confidence: {:.2})",
                                    threat_result.level as u8,
                                    session_id,
                                    threat_result.description,
                                    threat_result.confidence
                                );
                            }
                            ThreatLevel::Medium => {
                                warn!(
                                    "Suspicious activity [{}] {} - {}",
                                    session_id, threat_result.description, threat_result.confidence
                                );
                            }
                            ThreatLevel::Low => {
                                debug!(
                                    "Unusual activity [{}] {} - {}",
                                    session_id, threat_result.description, threat_result.confidence
                                );
                            }
                            ThreatLevel::None => {}
                        }
                    }

                    Ok(threat_result)
                } else {
                    warn!("BAML returned empty analysis_report");
                    Ok(ThreatResult::default())
                }
            }
            Err(e) => {
                error!("BAML API error during threat detection: {}", e);
                // On API error, return safe default (don't block session)
                // In production, might want to fail-open or fail-closed based on policy
                Ok(ThreatResult::default())
            }
        }
    }

    /// Analyze terminal output for threats
    ///
    /// This receives live terminal output directly from the SSH server, not parsed from recording files.
    /// The output is the actual data received from the remote terminal.
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique session identifier
    /// * `terminal_output` - Raw terminal output bytes from server
    /// * `username` - Username for context
    /// * `hostname` - Hostname for context
    /// * `protocol` - Protocol name (ssh, rdp, etc.)
    ///
    /// # Returns
    ///
    /// ThreatResult indicating if threat was detected
    pub async fn analyze_terminal_output(
        &self,
        session_id: &str,
        terminal_output: &[u8],
        _username: &str,
        _hostname: &str,
        _protocol: &str,
    ) -> Result<ThreatResult> {
        if !self.config.enabled {
            return Ok(ThreatResult::default());
        }

        // Convert bytes to string (best effort - may contain ANSI codes)
        let output_str = String::from_utf8_lossy(terminal_output);

        // Only analyze if output contains potentially suspicious content
        // Skip very short outputs or pure ANSI sequences
        if output_str.trim().len() < 10 {
            return Ok(ThreatResult::default());
        }

        // Check for suspicious patterns in output (errors, permission denied, etc.)
        let suspicious_patterns = [
            "error",
            "permission denied",
            "access denied",
            "unauthorized",
            "forbidden",
            "failed",
            "critical",
            "fatal",
        ];
        let has_suspicious_content = suspicious_patterns
            .iter()
            .any(|pattern| output_str.to_lowercase().contains(pattern));

        if !has_suspicious_content {
            return Ok(ThreatResult::default());
        }

        // Check tag-based rules first
        if let Some(tag_result) = self.check_tags(&output_str) {
            return Ok(tag_result);
        }

        // Call BAML ExtractKeystrokeAnalysis with the output text
        // Note: BAML's ExtractKeystrokeAnalysis can analyze any text, not just keystrokes
        match self
            .baml_client
            .extract_keystroke_analysis(&output_str)
            .await
        {
            Ok(baml_response) => {
                if let Some(analysis) = baml_response.analysis_report.first() {
                    let threat_result = self
                        .baml_client
                        .analysis_to_threat_result(analysis, &baml_response.overall_summary);

                    // Log if threat level meets minimum
                    if threat_result.level as u8 >= self.config.min_log_level as u8 {
                        match threat_result.level {
                            ThreatLevel::Critical | ThreatLevel::High => {
                                error!(
                                    "THREAT DETECTED in output [{}] {} - {}",
                                    session_id, threat_result.description, threat_result.confidence
                                );
                            }
                            ThreatLevel::Medium => {
                                warn!(
                                    "Suspicious output [{}] {}",
                                    session_id, threat_result.description
                                );
                            }
                            _ => {}
                        }
                    }

                    Ok(threat_result)
                } else {
                    Ok(ThreatResult::default())
                }
            }
            Err(e) => {
                error!("BAML API error analyzing terminal output: {}", e);
                Ok(ThreatResult::default())
            }
        }
    }

    /// Legacy method name for backward compatibility
    #[deprecated(note = "Use analyze_keystroke_sequence instead")]
    pub async fn analyze(
        &self,
        session_id: &str,
        input: &str,
        username: &str,
        hostname: &str,
        protocol: &str,
    ) -> Result<ThreatResult> {
        self.analyze_keystroke_sequence(session_id, input, username, hostname, protocol)
            .await
    }

    /// Generate session summary using BAML ExtractCommandSummary
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique session identifier
    ///
    /// # Returns
    ///
    /// CommandSummaryResponse with overall_summary
    pub async fn generate_summary(&self, session_id: &str) -> Result<CommandSummaryResponse> {
        if !self.config.enabled {
            return Err(ThreatDetectionError::ConfigurationError(
                "Threat detection not enabled".to_string(),
            ));
        }

        // Get command history (short critical section)
        let command_sequence: Vec<String> = {
            let history = self.command_history.read();
            history.get(session_id).cloned().unwrap_or_default()
        };

        if command_sequence.is_empty() {
            return Err(ThreatDetectionError::ConfigurationError(
                "No command history available".to_string(),
            ));
        }

        // Call BAML ExtractCommandSummary function
        self.baml_client
            .extract_command_summary(&command_sequence)
            .await
    }

    /// Check if session should be terminated based on threat result
    pub fn should_terminate(&self, threat: &ThreatResult) -> bool {
        self.config.auto_terminate && threat.should_terminate()
    }

    /// Clean up command history for a session
    ///
    /// **CRITICAL**: Must be called when session ends to prevent memory leak.
    /// The command history HashMap will grow unbounded if not cleaned up.
    ///
    /// Call this in protocol handler cleanup/disconnect logic:
    /// ```rust,no_run
    /// # use guacr_threat_detection::ThreatDetector;
    /// # let threat_detector: Option<ThreatDetector> = None;
    /// # let session_id = "test-session";
    /// // In handler's main loop, when session ends:
    /// if let Some(ref detector) = threat_detector {
    ///     detector.cleanup_session(&session_id);
    /// }
    /// ```
    pub fn cleanup_session(&self, session_id: &str) {
        let mut history = self.command_history.write();
        let removed = history.remove(session_id).is_some();
        if removed {
            debug!(
                "Cleaned up threat detection history for session: {}",
                session_id
            );
        } else {
            debug!(
                "No threat detection history found for session: {} (already cleaned or never created)",
                session_id
            );
        }
    }

    /// Get the number of active sessions being tracked
    ///
    /// Useful for monitoring memory usage and detecting leaks.
    /// If this number keeps growing, sessions aren't being cleaned up.
    pub fn active_session_count(&self) -> usize {
        self.command_history.read().len()
    }

    /// Health check - test BAML API connectivity
    pub async fn health_check(&self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        self.baml_client.health_check().await
    }

    /// Get all commands for a session (for summary generation)
    pub fn get_command_sequence(&self, session_id: &str) -> Vec<String> {
        let history = self.command_history.read();
        history.get(session_id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threat_detector_config_default() {
        let config = ThreatDetectorConfig::default();
        assert!(!config.enabled);
        assert!(config.auto_terminate);
    }

    #[test]
    fn test_should_terminate() {
        let config = ThreatDetectorConfig {
            enabled: true,
            auto_terminate: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let critical_threat = ThreatResult {
            level: ThreatLevel::Critical,
            confidence: 0.95,
            description: "Critical threat".to_string(),
            action: "terminate".to_string(),
            metadata: serde_json::Value::Null,
        };

        assert!(detector.should_terminate(&critical_threat));

        let safe_result = ThreatResult::default();
        assert!(!detector.should_terminate(&safe_result));
    }
}
