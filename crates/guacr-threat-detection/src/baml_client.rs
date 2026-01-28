use crate::threat::{ThreatLevel, ThreatResult};
use crate::{Result, ThreatDetectionError};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// BAML REST API client for threat detection
///
/// Calls BAML REST API endpoint to analyze commands/activity for threats.
/// Based on BAML documentation: https://docs.boundaryml.com/guide/installation-language/rest-api-other-languages
///
/// Uses BAML functions:
/// - ExtractKeystrokeAnalysis: Analyze individual commands
/// - ExtractCommandSummary: Generate session summaries
pub struct BamlClient {
    endpoint: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

/// BAML function request payload for ExtractKeystrokeAnalysis
#[derive(Debug, Serialize)]
struct ExtractKeystrokeAnalysisRequest {
    keystroke_sequence: String,
}

/// BAML function request payload for ExtractCommandSummary
#[derive(Debug, Serialize)]
struct ExtractCommandSummaryRequest {
    command_sequence: Vec<String>,
}

/// BAML KeystrokeAnalysisResponse (matches BAML schema)
#[derive(Debug, Deserialize)]
pub struct KeystrokeAnalysisResponse {
    pub analysis_report: Vec<Analysis>,
    pub overall_summary: String,
}

/// BAML Analysis (matches BAML schema)
#[derive(Debug, Deserialize, Clone)]
pub struct Analysis {
    pub risk_level: String, // "Low", "Medium", "High", "Critical"
    pub risk_category: String,
    pub reasoning: String,
}

/// BAML CommandSummaryResponse (matches BAML schema)
#[derive(Debug, Deserialize)]
pub struct CommandSummaryResponse {
    pub overall_summary: String,
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
        let client = reqwest::Client::builder()
            .timeout(timeout.unwrap_or(Duration::from_secs(5)))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            endpoint,
            api_key,
            client,
        }
    }

    /// Analyze keystroke sequence using BAML ExtractKeystrokeAnalysis function
    ///
    /// # Arguments
    ///
    /// * `keystroke_sequence` - The command/keystroke sequence to analyze
    ///
    /// # Returns
    ///
    /// KeystrokeAnalysisResponse with analysis_report and overall_summary
    pub async fn extract_keystroke_analysis(
        &self,
        keystroke_sequence: &str,
    ) -> Result<KeystrokeAnalysisResponse> {
        debug!("BAML: Analyzing keystroke sequence: {}", keystroke_sequence);

        // BAML REST API endpoint format: POST /api/ExtractKeystrokeAnalysis
        let function_endpoint = format!(
            "{}/ExtractKeystrokeAnalysis",
            self.endpoint.trim_end_matches('/')
        );

        let request = ExtractKeystrokeAnalysisRequest {
            keystroke_sequence: keystroke_sequence.to_string(),
        };

        let mut req = self.client.post(&function_endpoint).json(&request);

        // Add API key header if provided
        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        // Add Content-Type header
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

        let baml_response: KeystrokeAnalysisResponse = response
            .json()
            .await
            .map_err(ThreatDetectionError::HttpError)?;

        debug!(
            "BAML: Analysis complete - {} reports, summary: {}",
            baml_response.analysis_report.len(),
            baml_response.overall_summary
        );

        Ok(baml_response)
    }

    /// Generate command summary using BAML ExtractCommandSummary function
    ///
    /// # Arguments
    ///
    /// * `command_sequence` - List of commands in order
    ///
    /// # Returns
    ///
    /// CommandSummaryResponse with overall_summary
    pub async fn extract_command_summary(
        &self,
        command_sequence: &[String],
    ) -> Result<CommandSummaryResponse> {
        debug!(
            "BAML: Generating summary for {} commands",
            command_sequence.len()
        );

        // BAML REST API endpoint format: POST /api/ExtractCommandSummary
        let function_endpoint = format!(
            "{}/ExtractCommandSummary",
            self.endpoint.trim_end_matches('/')
        );

        let request = ExtractCommandSummaryRequest {
            command_sequence: command_sequence.to_vec(),
        };

        let mut req = self.client.post(&function_endpoint).json(&request);

        // Add API key header if provided
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

        debug!("BAML: Summary generated: {}", baml_response.overall_summary);

        Ok(baml_response)
    }

    /// Convert BAML Analysis to ThreatResult
    pub fn analysis_to_threat_result(
        &self,
        analysis: &Analysis,
        overall_summary: &str,
    ) -> ThreatResult {
        let risk_level_lower = analysis.risk_level.to_lowercase();

        let level = if risk_level_lower == "critical" {
            ThreatLevel::Critical
        } else if risk_level_lower == "high" {
            ThreatLevel::High
        } else if risk_level_lower == "medium" {
            ThreatLevel::Medium
        } else if risk_level_lower == "low" {
            ThreatLevel::Low
        } else {
            ThreatLevel::None
        };

        ThreatResult {
            level,
            confidence: 0.8, // BAML doesn't return confidence, use default
            description: format!("{} - {}", analysis.risk_category, analysis.reasoning),
            action: if level == ThreatLevel::Critical || level == ThreatLevel::High {
                "terminate".to_string()
            } else {
                "monitor".to_string()
            },
            metadata: serde_json::json!({
                "risk_category": analysis.risk_category,
                "reasoning": analysis.reasoning,
                "overall_summary": overall_summary,
            }),
        }
    }

    /// Health check - test BAML API connectivity
    pub async fn health_check(&self) -> Result<()> {
        let test_input = "ls";
        match self.extract_keystroke_analysis(test_input).await {
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

    #[test]
    fn test_analysis_to_threat_result_critical() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = Analysis {
            risk_level: "Critical".to_string(),
            risk_category: "DestructiveActivity".to_string(),
            reasoning: "Command deletes files".to_string(),
        };

        let threat = client.analysis_to_threat_result(&analysis, "User deleted files");
        assert_eq!(threat.level, ThreatLevel::Critical);
        assert!(threat.should_terminate());
    }

    #[test]
    fn test_analysis_to_threat_result_low() {
        let client = BamlClient::new("http://localhost:8000".to_string(), None, None);
        let analysis = Analysis {
            risk_level: "Low".to_string(),
            risk_category: "RoutineOperations".to_string(),
            reasoning: "Lists directory contents".to_string(),
        };

        let threat = client.analysis_to_threat_result(&analysis, "User listed files");
        assert_eq!(threat.level, ThreatLevel::Low);
        assert!(!threat.should_terminate());
    }
}
