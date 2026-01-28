use serde::{Deserialize, Serialize};

/// Threat level detected by AI
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreatLevel {
    /// No threat detected
    None,
    /// Low risk - suspicious but not dangerous
    Low,
    /// Medium risk - potentially dangerous
    Medium,
    /// High risk - likely malicious
    High,
    /// Critical - immediate threat, terminate session
    Critical,
}

/// Threat detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatResult {
    /// Threat level
    pub level: ThreatLevel,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Description of the threat
    pub description: String,
    /// Recommended action
    pub action: String,
    /// Additional context/metadata
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ThreatResult {
    pub fn is_threat(&self) -> bool {
        matches!(
            self.level,
            ThreatLevel::Low | ThreatLevel::Medium | ThreatLevel::High | ThreatLevel::Critical
        )
    }

    pub fn should_terminate(&self) -> bool {
        matches!(self.level, ThreatLevel::High | ThreatLevel::Critical)
    }
}

impl Default for ThreatResult {
    fn default() -> Self {
        Self {
            level: ThreatLevel::None,
            confidence: 0.0,
            description: "No threat detected".to_string(),
            action: "continue".to_string(),
            metadata: serde_json::Value::Null,
        }
    }
}
