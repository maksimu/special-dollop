use crate::baml_client::{BamlClient, CommandSummaryResponse};
use crate::threat::{RiskSource, TagMatch, TagMatches, ThreatLevel, ThreatResult};
use crate::{Result, ThreatDetectionError};
use log::{debug, error, info, warn};
use parking_lot::RwLock;
use regex::Regex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Protocol-specific submit types that trigger analysis.
/// Returns true if the given submit_type should trigger analysis for the protocol.
/// Uses static slices instead of allocating a HashSet per call.
pub fn should_trigger_analysis(protocol: &str, submit_type: &str) -> bool {
    let triggers: &[&str] = match protocol {
        "ssh" | "telnet" | "kubernetes" | "mysql" | "postgres" | "sql-server" => &["return"],
        "rdp" | "vnc" => &["return", "click", "escape"],
        "http" => &["return", "click"],
        _ => &["return"],
    };
    triggers.contains(&submit_type)
}

/// Per-session state for threat detection
#[derive(Debug)]
struct SessionState {
    /// Command history for context (bounded, O(1) pop_front via VecDeque)
    command_history: VecDeque<String>,
    /// Commands already analyzed (for deduplication, bounded, O(1) pop_front)
    processed_commands: VecDeque<String>,
    /// Whether this is the first interaction in the session
    is_first_interaction: bool,
}

impl SessionState {
    fn new() -> Self {
        Self {
            command_history: VecDeque::new(),
            processed_commands: VecDeque::new(),
            is_first_interaction: true,
        }
    }
}

/// A compiled tag rule (regex compiled once, reused across calls)
struct CompiledTagRule {
    regex: Regex,
    pattern: String,
    level: String,
    tag_type: String, // "deny" or "allow"
}

/// Tag check result with richer information
struct TagCheckResult {
    /// The threat result (if deny tags matched)
    threat: Option<ThreatResult>,
    /// All matched deny tags (also embedded in threat.tag_matches when present)
    #[allow(dead_code)]
    deny_matches: Vec<TagMatch>,
    /// All matched allow tags
    allow_matches: Vec<TagMatch>,
}

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
    /// Global kill switch - if false, AI can NEVER terminate sessions
    pub config_allow_ai_session_terminate: bool,
    /// Per-resource enable - if false, this resource's sessions won't be terminated
    pub resource_ai_session_terminate_enabled: bool,
    /// Per-risk-level termination flags (e.g., "critical" -> true, "high" -> true, "medium" -> false)
    pub level_terminate_flags: HashMap<String, bool>,
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
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: true,
            level_terminate_flags: HashMap::new(),
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
/// The sessions HashMap will grow unbounded if sessions aren't cleaned up.
pub struct ThreatDetector {
    config: ThreatDetectorConfig,
    baml_client: BamlClient,
    /// Pre-compiled tag rules (compiled once at construction, never recompiled)
    compiled_tags: Vec<CompiledTagRule>,
    /// Per-session state (command history, deduplication, first-interaction tracking)
    /// Using parking_lot::RwLock (faster than std, no async)
    /// **MUST be cleaned up** when session ends via cleanup_session()
    sessions: Arc<RwLock<HashMap<String, SessionState>>>,
}

impl ThreatDetector {
    /// Compile all tag regex patterns once at construction time.
    /// Invalid patterns are logged and skipped (not a fatal error).
    fn compile_tag_rules(config: &ThreatDetectorConfig) -> Vec<CompiledTagRule> {
        let mut rules = Vec::new();

        for (level, patterns) in &config.deny_tags {
            for pattern in patterns {
                match Regex::new(pattern) {
                    Ok(regex) => rules.push(CompiledTagRule {
                        regex,
                        pattern: pattern.clone(),
                        level: level.clone(),
                        tag_type: "deny".to_string(),
                    }),
                    Err(e) => {
                        warn!("Invalid deny tag regex '{}': {} -- skipping", pattern, e);
                    }
                }
            }
        }

        for (level, patterns) in &config.allow_tags {
            for pattern in patterns {
                match Regex::new(pattern) {
                    Ok(regex) => rules.push(CompiledTagRule {
                        regex,
                        pattern: pattern.clone(),
                        level: level.clone(),
                        tag_type: "allow".to_string(),
                    }),
                    Err(e) => {
                        warn!("Invalid allow tag regex '{}': {} -- skipping", pattern, e);
                    }
                }
            }
        }

        rules
    }

    /// Create a new threat detector
    pub fn new(config: ThreatDetectorConfig) -> Result<Self> {
        let baml_client = BamlClient::new(
            config.baml_endpoint.clone(),
            config.baml_api_key.clone(),
            Some(std::time::Duration::from_secs(config.timeout_seconds)),
        );

        if !config.enabled {
            info!("Threat detection disabled");
            return Ok(Self {
                compiled_tags: Vec::new(),
                config,
                baml_client,
                sessions: Arc::new(RwLock::new(HashMap::new())),
            });
        }

        info!(
            "Threat detection enabled - BAML endpoint: {}, auto-terminate: {}, \
             global-terminate: {}, resource-terminate: {}",
            config.baml_endpoint,
            config.auto_terminate,
            config.config_allow_ai_session_terminate,
            config.resource_ai_session_terminate_enabled
        );

        let compiled_tags = Self::compile_tag_rules(&config);
        if !compiled_tags.is_empty() {
            info!(
                "Compiled {} tag rules ({} deny, {} allow)",
                compiled_tags.len(),
                compiled_tags
                    .iter()
                    .filter(|r| r.tag_type == "deny")
                    .count(),
                compiled_tags
                    .iter()
                    .filter(|r| r.tag_type == "allow")
                    .count(),
            );
        }

        Ok(Self {
            compiled_tags,
            config,
            baml_client,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Check tag-based rules and return richer information about all matches.
    ///
    /// Collects ALL deny tag matches and ALL allow tag matches, then:
    /// - If deny matches exist: creates ThreatResult with the highest deny level,
    ///   risk_level_source = CustomTagRule
    /// - If ONLY allow matches exist: returns None threat (explicitly allowed)
    /// - Otherwise: returns None threat (no tags matched)
    fn check_tags(&self, input: &str) -> TagCheckResult {
        if !self.config.enable_tag_checking || self.compiled_tags.is_empty() {
            return TagCheckResult {
                threat: None,
                deny_matches: Vec::new(),
                allow_matches: Vec::new(),
            };
        }

        let mut deny_matches: Vec<TagMatch> = Vec::new();
        let mut allow_matches: Vec<TagMatch> = Vec::new();

        // Match against pre-compiled regexes (no per-call compilation)
        for rule in &self.compiled_tags {
            if rule.regex.is_match(input) {
                let tag_match = TagMatch {
                    tag: rule.pattern.clone(),
                    level: rule.level.clone(),
                    tag_type: rule.tag_type.clone(),
                };

                if rule.tag_type == "deny" {
                    warn!(
                        "Deny tag matched: '{}' (level: {}) for command: {}",
                        rule.pattern, rule.level, input
                    );
                    deny_matches.push(tag_match);
                } else {
                    debug!(
                        "Allow tag matched: '{}' (level: {}) for command: {}",
                        rule.pattern, rule.level, input
                    );
                    allow_matches.push(tag_match);
                }
            }
        }

        let tag_matches = TagMatches {
            deny_tags: deny_matches.clone(),
            allow_tags: allow_matches.clone(),
        };

        // If deny matches exist, create a ThreatResult with the highest deny level
        if !deny_matches.is_empty() {
            let highest_level = deny_matches
                .iter()
                .map(|m| match m.level.to_lowercase().as_str() {
                    "critical" => ThreatLevel::Critical,
                    "high" => ThreatLevel::High,
                    "medium" => ThreatLevel::Medium,
                    "low" => ThreatLevel::Low,
                    _ => ThreatLevel::High,
                })
                .max()
                .unwrap_or(ThreatLevel::High);

            let deny_descriptions: Vec<String> = deny_matches
                .iter()
                .map(|m| format!("{} ({})", m.tag, m.level))
                .collect();

            let threat = ThreatResult {
                level: highest_level,
                risk_score: highest_level.to_risk_score(),
                risk_level_source: RiskSource::CustomTagRule,
                confidence: 1.0, // Tag matches are 100% confident
                description: format!("Deny tag(s) matched: {}", deny_descriptions.join(", ")),
                action: "terminate".to_string(),
                command_text: Some(input.to_string()),
                risk_category: Some("TagRule".to_string()),
                tag_matches,
                should_terminate_session: false, // Will be set by determine_should_terminate
                metadata: serde_json::json!({
                    "deny_matches": deny_matches.len(),
                    "allow_matches": allow_matches.len(),
                }),
            };

            return TagCheckResult {
                threat: Some(threat),
                deny_matches,
                allow_matches,
            };
        }

        // If only allow matches, return None threat (explicitly allowed, no analysis needed)
        if !allow_matches.is_empty() {
            debug!(
                "Command explicitly allowed by {} allow tag(s), skipping analysis",
                allow_matches.len()
            );
        }

        TagCheckResult {
            threat: None,
            deny_matches,
            allow_matches,
        }
    }

    /// Determine if a threat result should trigger session termination.
    ///
    /// Implements the termination gating hierarchy:
    /// 1. config_allow_ai_session_terminate must be true (global gate)
    /// 2. resource_ai_session_terminate_enabled must be true (resource gate)
    /// 3. Deny tags always terminate
    /// 4. Allow-only tags never terminate
    /// 5. Fall back to level_terminate_flags for the risk level
    /// 6. Default: terminate on High/Critical
    fn determine_should_terminate(&self, threat: &ThreatResult) -> bool {
        // Gate 1: Global kill switch
        if !self.config.config_allow_ai_session_terminate {
            return false;
        }

        // Gate 2: Resource-level enable
        if !self.config.resource_ai_session_terminate_enabled {
            return false;
        }

        // Gate 3: Deny tags always terminate
        if threat.tag_matches.has_deny_tags() {
            return true;
        }

        // Gate 4: Allow-only tags never terminate
        if threat.tag_matches.has_only_allow_tags() {
            return false;
        }

        // Gate 5: Check level-specific flags
        let level_str = format!("{:?}", threat.level).to_lowercase();
        if let Some(&should_terminate) = self.config.level_terminate_flags.get(&level_str) {
            return should_terminate;
        }

        // Default: terminate on High/Critical
        matches!(threat.level, ThreatLevel::High | ThreatLevel::Critical)
    }

    /// Process all analysis reports from BAML and return the highest-severity result.
    ///
    /// Iterates all reports, converts each to ThreatResult, logs them, and returns
    /// the one with the highest risk_score.
    fn aggregate_baml_reports(
        &self,
        session_id: &str,
        reports: &[crate::baml_client::Analysis],
        overall_summary: &str,
    ) -> ThreatResult {
        if reports.is_empty() {
            warn!("BAML returned empty analysis_report");
            return ThreatResult::default();
        }

        let mut best_result: Option<ThreatResult> = None;

        for (i, analysis) in reports.iter().enumerate() {
            let mut threat_result = self
                .baml_client
                .analysis_to_threat_result(analysis, overall_summary);

            // Apply termination gating
            threat_result.should_terminate_session =
                self.determine_should_terminate(&threat_result);

            // Log each report if it meets the minimum level
            if threat_result.level >= self.config.min_log_level {
                match threat_result.level {
                    ThreatLevel::Critical | ThreatLevel::High => {
                        error!(
                            "THREAT DETECTED [report {}/{}] [{}] {} - {} \
                             (confidence: {:.2}, score: {})",
                            i + 1,
                            reports.len(),
                            session_id,
                            threat_result.description,
                            threat_result.action,
                            threat_result.confidence,
                            threat_result.risk_score
                        );
                    }
                    ThreatLevel::Medium => {
                        warn!(
                            "Suspicious activity [report {}/{}] [{}] {} (score: {})",
                            i + 1,
                            reports.len(),
                            session_id,
                            threat_result.description,
                            threat_result.risk_score
                        );
                    }
                    ThreatLevel::Low => {
                        debug!(
                            "Unusual activity [report {}/{}] [{}] {} (score: {})",
                            i + 1,
                            reports.len(),
                            session_id,
                            threat_result.description,
                            threat_result.risk_score
                        );
                    }
                    ThreatLevel::None => {}
                }
            }

            // Keep the result with the highest risk_score
            let is_higher = match &best_result {
                Some(existing) => threat_result.risk_score > existing.risk_score,
                None => true,
            };
            if is_higher {
                best_result = Some(threat_result);
            }
        }

        if reports.len() > 1 {
            debug!(
                "Aggregated {} BAML reports for session {}, returning highest-severity result",
                reports.len(),
                session_id
            );
        }

        best_result.unwrap_or_default()
    }

    /// Analyze keyboard input (keystroke sequence) for threats using BAML ExtractKeystrokeAnalysis
    ///
    /// This receives live keyboard data directly from the SSH handler, not parsed from recording files.
    /// The input is the actual keystroke sequence as typed by the user.
    ///
    /// Features:
    /// - Tag-based rules checked first (immediate deny/allow)
    /// - Command deduplication (skips already-analyzed commands)
    /// - First-interaction skip (skips empty first interactions)
    /// - Multi-report aggregation (returns highest-severity from all BAML reports)
    /// - Termination gating hierarchy (global -> resource -> tags -> level flags)
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
        protocol: &str,
    ) -> Result<ThreatResult> {
        if !self.config.enabled {
            return Ok(ThreatResult::default());
        }

        // Check tag-based rules first (immediate deny/allow)
        let tag_check = self.check_tags(keystroke_sequence);
        if let Some(mut tag_threat) = tag_check.threat {
            // Apply termination gating to tag result
            tag_threat.should_terminate_session = self.determine_should_terminate(&tag_threat);
            return Ok(tag_threat);
        }
        // If only allow tags matched, skip analysis entirely
        if !tag_check.allow_matches.is_empty() {
            return Ok(ThreatResult::default());
        }

        // Extract command from keystroke sequence (remove newlines, trim)
        let command = keystroke_sequence
            .trim()
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();

        // Ensure session state exists
        {
            let mut sessions = self.sessions.write();
            sessions
                .entry(session_id.to_string())
                .or_insert_with(SessionState::new);
        }

        // Check first interaction
        {
            let sessions = self.sessions.read();
            if let Some(state) = sessions.get(session_id) {
                if state.is_first_interaction && command.is_empty() {
                    debug!("Skipping analysis on first interaction with no input");
                    return Ok(ThreatResult::default());
                }
            }
        }

        // Check deduplication
        {
            let sessions = self.sessions.read();
            if let Some(state) = sessions.get(session_id) {
                if state.processed_commands.iter().any(|c| c == &command) {
                    debug!("Skipping already-analyzed command: {}", command);
                    return Ok(ThreatResult::default());
                }
            }
        }

        // Add to command history (store the actual command, not raw keystrokes)
        // Using parking_lot::RwLock (non-async, faster than tokio::sync::RwLock)
        if !command.is_empty() {
            let mut sessions = self.sessions.write();
            if let Some(state) = sessions.get_mut(session_id) {
                state.command_history.push_back(command.clone());
                // Keep only last N commands (ring buffer behavior, O(1) pop_front)
                while state.command_history.len() > self.config.command_history_size {
                    state.command_history.pop_front();
                }
            }
        }

        // Get previous commands for richer context
        let previous_commands: Vec<String> = {
            let sessions = self.sessions.read();
            sessions
                .get(session_id)
                .map(|s| s.command_history.iter().cloned().collect())
                .unwrap_or_default()
        };

        // Call BAML AnalyzeCliText with protocol context and command history
        let cli_lines = vec![command.clone()];
        match self
            .baml_client
            .analyze_cli_text(&cli_lines, protocol, &previous_commands)
            .await
        {
            Ok(baml_response) => {
                // Process ALL reports and return highest-severity
                let mut threat_result = self.aggregate_baml_reports(
                    session_id,
                    &baml_response.analysis_report,
                    &baml_response.overall_summary,
                );

                // Set command text if not already set by BAML
                if threat_result.command_text.is_none() && !command.is_empty() {
                    threat_result.command_text = Some(command.clone());
                }

                // After successful analysis, update session state
                {
                    let mut sessions = self.sessions.write();
                    if let Some(state) = sessions.get_mut(session_id) {
                        // Mark first interaction complete
                        state.is_first_interaction = false;

                        // Add to processed commands (keep last 10 for deduplication)
                        if !command.is_empty() {
                            state.processed_commands.push_back(command);
                            while state.processed_commands.len() > 10 {
                                state.processed_commands.pop_front();
                            }
                        }
                    }
                }

                Ok(threat_result)
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
        protocol: &str,
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
        let tag_check = self.check_tags(&output_str);
        if let Some(mut tag_threat) = tag_check.threat {
            tag_threat.should_terminate_session = self.determine_should_terminate(&tag_threat);
            return Ok(tag_threat);
        }
        if !tag_check.allow_matches.is_empty() {
            return Ok(ThreatResult::default());
        }

        // Get previous commands for richer context
        let previous_commands: Vec<String> = {
            let sessions = self.sessions.read();
            sessions
                .get(session_id)
                .map(|s| s.command_history.iter().cloned().collect())
                .unwrap_or_default()
        };

        // Call BAML AnalyzeCliText with protocol context and command history
        let cli_lines: Vec<String> = output_str.lines().map(|l| l.to_string()).collect();
        match self
            .baml_client
            .analyze_cli_text(&cli_lines, protocol, &previous_commands)
            .await
        {
            Ok(baml_response) => {
                // Process ALL reports and return highest-severity (multi-report aggregation)
                let threat_result = self.aggregate_baml_reports(
                    session_id,
                    &baml_response.analysis_report,
                    &baml_response.overall_summary,
                );

                Ok(threat_result)
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
            let sessions = self.sessions.read();
            sessions
                .get(session_id)
                .map(|s| s.command_history.iter().cloned().collect())
                .unwrap_or_default()
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

    /// Check if analysis should be triggered for this protocol and submit type
    pub fn should_analyze(&self, protocol: &str, submit_type: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        should_trigger_analysis(protocol, submit_type)
    }

    /// Clean up session state for a session
    ///
    /// **CRITICAL**: Must be called when session ends to prevent memory leak.
    /// The sessions HashMap will grow unbounded if not cleaned up.
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
        let mut sessions = self.sessions.write();
        let removed = sessions.remove(session_id).is_some();
        if removed {
            debug!(
                "Cleaned up threat detection state for session: {}",
                session_id
            );
        } else {
            debug!(
                "No threat detection state found for session: {} \
                 (already cleaned or never created)",
                session_id
            );
        }
    }

    /// Get the number of active sessions being tracked
    ///
    /// Useful for monitoring memory usage and detecting leaks.
    /// If this number keeps growing, sessions aren't being cleaned up.
    pub fn active_session_count(&self) -> usize {
        self.sessions.read().len()
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
        let sessions = self.sessions.read();
        sessions
            .get(session_id)
            .map(|s| s.command_history.iter().cloned().collect())
            .unwrap_or_default()
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
        assert!(config.config_allow_ai_session_terminate);
        assert!(config.resource_ai_session_terminate_enabled);
        assert!(config.level_terminate_flags.is_empty());
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
            risk_score: 18,
            risk_level_source: RiskSource::ModelDefault,
            confidence: 0.95,
            description: "Critical threat".to_string(),
            action: "terminate".to_string(),
            command_text: None,
            risk_category: None,
            tag_matches: TagMatches::default(),
            should_terminate_session: true,
            metadata: serde_json::Value::Null,
        };

        assert!(detector.should_terminate(&critical_threat));

        let safe_result = ThreatResult::default();
        assert!(!detector.should_terminate(&safe_result));
    }

    #[test]
    fn test_should_trigger_analysis_ssh() {
        assert!(should_trigger_analysis("ssh", "return"));
        assert!(!should_trigger_analysis("ssh", "click"));
        assert!(!should_trigger_analysis("ssh", "escape"));
    }

    #[test]
    fn test_should_trigger_analysis_rdp() {
        assert!(should_trigger_analysis("rdp", "return"));
        assert!(should_trigger_analysis("rdp", "click"));
        assert!(should_trigger_analysis("rdp", "escape"));
        assert!(!should_trigger_analysis("rdp", "tab"));
    }

    #[test]
    fn test_should_trigger_analysis_http() {
        assert!(should_trigger_analysis("http", "return"));
        assert!(should_trigger_analysis("http", "click"));
        assert!(!should_trigger_analysis("http", "escape"));
    }

    #[test]
    fn test_should_trigger_analysis_unknown_protocol() {
        assert!(should_trigger_analysis("unknown", "return"));
        assert!(!should_trigger_analysis("unknown", "click"));
    }

    #[test]
    fn test_should_analyze_disabled() {
        let config = ThreatDetectorConfig {
            enabled: false,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();
        assert!(!detector.should_analyze("ssh", "return"));
    }

    #[test]
    fn test_should_analyze_enabled() {
        let config = ThreatDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();
        assert!(detector.should_analyze("ssh", "return"));
        assert!(!detector.should_analyze("ssh", "click"));
        assert!(detector.should_analyze("rdp", "click"));
    }

    #[test]
    fn test_check_tags_deny_collects_all_matches() {
        let mut deny_tags = HashMap::new();
        deny_tags.insert("critical".to_string(), vec!["rm.*-rf".to_string()]);
        deny_tags.insert("high".to_string(), vec!["rm".to_string()]);

        let config = ThreatDetectorConfig {
            enabled: true,
            enable_tag_checking: true,
            deny_tags,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let result = detector.check_tags("rm -rf /");

        // Both deny tags should match
        assert_eq!(result.deny_matches.len(), 2);
        assert!(result.threat.is_some());

        let threat = result.threat.unwrap();
        // Should use the highest level (Critical)
        assert_eq!(threat.level, ThreatLevel::Critical);
        assert_eq!(threat.risk_level_source, RiskSource::CustomTagRule);
        assert_eq!(threat.confidence, 1.0);
    }

    #[test]
    fn test_check_tags_allow_only() {
        let mut allow_tags = HashMap::new();
        allow_tags.insert("low".to_string(), vec!["^ls".to_string()]);

        let config = ThreatDetectorConfig {
            enabled: true,
            enable_tag_checking: true,
            allow_tags,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let result = detector.check_tags("ls -la");

        assert!(result.threat.is_none());
        assert!(result.deny_matches.is_empty());
        assert_eq!(result.allow_matches.len(), 1);
        assert_eq!(result.allow_matches[0].tag_type, "allow");
    }

    #[test]
    fn test_check_tags_no_matches() {
        let config = ThreatDetectorConfig {
            enabled: true,
            enable_tag_checking: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let result = detector.check_tags("echo hello");

        assert!(result.threat.is_none());
        assert!(result.deny_matches.is_empty());
        assert!(result.allow_matches.is_empty());
    }

    #[test]
    fn test_check_tags_disabled() {
        let mut deny_tags = HashMap::new();
        deny_tags.insert("critical".to_string(), vec!["rm".to_string()]);

        let config = ThreatDetectorConfig {
            enabled: true,
            enable_tag_checking: false,
            deny_tags,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let result = detector.check_tags("rm -rf /");

        // Tag checking disabled, so no matches
        assert!(result.threat.is_none());
        assert!(result.deny_matches.is_empty());
    }

    #[test]
    fn test_determine_should_terminate_global_gate() {
        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: false,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let threat = ThreatResult {
            level: ThreatLevel::Critical,
            risk_score: 18,
            should_terminate_session: false,
            ..Default::default()
        };

        // Global gate is off, so should never terminate
        assert!(!detector.determine_should_terminate(&threat));
    }

    #[test]
    fn test_determine_should_terminate_resource_gate() {
        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: false,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let threat = ThreatResult {
            level: ThreatLevel::Critical,
            risk_score: 18,
            should_terminate_session: false,
            ..Default::default()
        };

        // Resource gate is off, so should never terminate
        assert!(!detector.determine_should_terminate(&threat));
    }

    #[test]
    fn test_determine_should_terminate_deny_tags_always() {
        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let threat = ThreatResult {
            level: ThreatLevel::Low, // Even low level
            risk_score: 3,
            tag_matches: TagMatches {
                deny_tags: vec![TagMatch {
                    tag: "rm".to_string(),
                    level: "low".to_string(),
                    tag_type: "deny".to_string(),
                }],
                allow_tags: vec![],
            },
            should_terminate_session: false,
            ..Default::default()
        };

        // Deny tags always terminate (regardless of level)
        assert!(detector.determine_should_terminate(&threat));
    }

    #[test]
    fn test_determine_should_terminate_allow_only_never() {
        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        let threat = ThreatResult {
            level: ThreatLevel::High, // Even high level
            risk_score: 13,
            tag_matches: TagMatches {
                deny_tags: vec![],
                allow_tags: vec![TagMatch {
                    tag: "ls".to_string(),
                    level: "low".to_string(),
                    tag_type: "allow".to_string(),
                }],
            },
            should_terminate_session: false,
            ..Default::default()
        };

        // Allow-only tags never terminate
        assert!(!detector.determine_should_terminate(&threat));
    }

    #[test]
    fn test_determine_should_terminate_level_flags() {
        let mut level_flags = HashMap::new();
        level_flags.insert("medium".to_string(), true);
        level_flags.insert("high".to_string(), false);

        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: true,
            level_terminate_flags: level_flags,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        // Medium should terminate (per flag)
        let medium_threat = ThreatResult {
            level: ThreatLevel::Medium,
            risk_score: 8,
            ..Default::default()
        };
        assert!(detector.determine_should_terminate(&medium_threat));

        // High should NOT terminate (per flag, overriding default)
        let high_threat = ThreatResult {
            level: ThreatLevel::High,
            risk_score: 13,
            ..Default::default()
        };
        assert!(!detector.determine_should_terminate(&high_threat));
    }

    #[test]
    fn test_determine_should_terminate_default_behavior() {
        let config = ThreatDetectorConfig {
            enabled: true,
            config_allow_ai_session_terminate: true,
            resource_ai_session_terminate_enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        // No level flags set, defaults apply
        assert!(!detector.determine_should_terminate(&ThreatResult {
            level: ThreatLevel::None,
            ..Default::default()
        }));
        assert!(!detector.determine_should_terminate(&ThreatResult {
            level: ThreatLevel::Low,
            ..Default::default()
        }));
        assert!(!detector.determine_should_terminate(&ThreatResult {
            level: ThreatLevel::Medium,
            ..Default::default()
        }));
        assert!(detector.determine_should_terminate(&ThreatResult {
            level: ThreatLevel::High,
            ..Default::default()
        }));
        assert!(detector.determine_should_terminate(&ThreatResult {
            level: ThreatLevel::Critical,
            ..Default::default()
        }));
    }

    #[test]
    fn test_session_state_lifecycle() {
        let config = ThreatDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        assert_eq!(detector.active_session_count(), 0);

        // Manually insert a session state to test cleanup
        {
            let mut sessions = detector.sessions.write();
            sessions.insert("test-session".to_string(), SessionState::new());
        }

        assert_eq!(detector.active_session_count(), 1);

        detector.cleanup_session("test-session");
        assert_eq!(detector.active_session_count(), 0);
    }

    #[test]
    fn test_get_command_sequence_with_session_state() {
        let config = ThreatDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        // No session yet
        assert!(detector.get_command_sequence("test-session").is_empty());

        // Insert session with commands
        {
            let mut sessions = detector.sessions.write();
            let mut state = SessionState::new();
            state.command_history.push_back("ls".to_string());
            state.command_history.push_back("pwd".to_string());
            sessions.insert("test-session".to_string(), state);
        }

        let cmds = detector.get_command_sequence("test-session");
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "ls");
        assert_eq!(cmds[1], "pwd");
    }

    #[test]
    fn test_cleanup_nonexistent_session() {
        let config = ThreatDetectorConfig {
            enabled: true,
            ..Default::default()
        };
        let detector = ThreatDetector::new(config).unwrap();

        // Should not panic
        detector.cleanup_session("nonexistent");
        assert_eq!(detector.active_session_count(), 0);
    }

    #[test]
    fn test_session_state_new() {
        let state = SessionState::new();
        assert!(state.command_history.is_empty());
        assert!(state.processed_commands.is_empty());
        assert!(state.is_first_interaction);
    }
}
