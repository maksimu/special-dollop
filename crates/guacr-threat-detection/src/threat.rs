use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Threat level detected by AI
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

impl ThreatLevel {
    /// Convert a numeric risk score (0-20) to a ThreatLevel.
    ///
    /// Ranges:
    /// - 0     = None
    /// - 1-5   = Low
    /// - 6-10  = Medium
    /// - 11-15 = High
    /// - 16-20 = Critical
    pub fn from_risk_score(score: u8) -> Self {
        match score {
            0 => ThreatLevel::None,
            1..=5 => ThreatLevel::Low,
            6..=10 => ThreatLevel::Medium,
            11..=15 => ThreatLevel::High,
            16..=20 => ThreatLevel::Critical,
            // Scores above 20 are clamped to Critical
            _ => ThreatLevel::Critical,
        }
    }

    /// Convert this ThreatLevel to its midpoint risk score.
    ///
    /// Midpoints:
    /// - None     = 0
    /// - Low      = 3
    /// - Medium   = 8
    /// - High     = 13
    /// - Critical = 18
    pub fn to_risk_score(&self) -> u8 {
        match self {
            ThreatLevel::None => 0,
            ThreatLevel::Low => 3,
            ThreatLevel::Medium => 8,
            ThreatLevel::High => 13,
            ThreatLevel::Critical => 18,
        }
    }
}

/// Where a risk score originated from
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSource {
    /// Risk score from the LLM model (default)
    #[default]
    ModelDefault,
    /// Risk score from custom tag rule matching
    CustomTagRule,
    /// Risk score from local action effect classifier
    ActionEffectClassifier,
}

/// A single tag match result from deny/allow tag rule evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagMatch {
    /// The regex pattern that matched
    pub tag: String,
    /// The risk level this tag is configured at (e.g., "critical", "high")
    pub level: String,
    /// "deny" or "allow"
    pub tag_type: String,
}

/// Collection of deny and allow tag matches from rule evaluation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagMatches {
    pub deny_tags: Vec<TagMatch>,
    pub allow_tags: Vec<TagMatch>,
}

impl TagMatches {
    /// Returns true if any deny tags matched
    pub fn has_deny_tags(&self) -> bool {
        !self.deny_tags.is_empty()
    }

    /// Returns true if any allow tags matched
    pub fn has_allow_tags(&self) -> bool {
        !self.allow_tags.is_empty()
    }

    /// Returns true if allow tags matched but no deny tags did
    pub fn has_only_allow_tags(&self) -> bool {
        !self.allow_tags.is_empty() && self.deny_tags.is_empty()
    }
}

/// Threat detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatResult {
    /// Categorical threat level (backward compat)
    pub level: ThreatLevel,
    /// Numeric risk score (1-20), 0 means no threat
    pub risk_score: u8,
    /// Where the risk score came from
    #[serde(default)]
    pub risk_level_source: RiskSource,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Description of the threat (risk_category - reasoning)
    pub description: String,
    /// Recommended action
    pub action: String,
    /// The exact command text identified (if any)
    #[serde(default)]
    pub command_text: Option<String>,
    /// Risk category from analysis
    #[serde(default)]
    pub risk_category: Option<String>,
    /// Tag matches (deny/allow)
    #[serde(default)]
    pub tag_matches: TagMatches,
    /// Whether this result should trigger session termination
    /// (determined by termination gating logic, not just threat level)
    pub should_terminate_session: bool,
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
        self.should_terminate_session
    }
}

impl Default for ThreatResult {
    fn default() -> Self {
        Self {
            level: ThreatLevel::None,
            risk_score: 0,
            risk_level_source: RiskSource::ModelDefault,
            confidence: 0.0,
            description: "No threat detected".to_string(),
            action: "continue".to_string(),
            command_text: None,
            risk_category: None,
            tag_matches: TagMatches::default(),
            should_terminate_session: false,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Structured summary output from analysis, matching Python's AnalysisRecord
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRecord {
    pub risk_score: u8,
    pub risk_level: String,
    pub risk_category: String,
    pub action_effects: String,
    pub command_text: Option<String>,
    pub overall_summary: String,
    pub risk_level_source: RiskSource,
    pub tag_matches: TagMatches,
    pub should_terminate: bool,
}

/// Termination policy configuration (matches Python's ai_settings hierarchy)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminationPolicy {
    /// Global kill switch - if false, AI can NEVER terminate sessions
    pub config_allow_ai_session_terminate: bool,
    /// Per-resource enable - if false, this resource's sessions won't be terminated
    pub resource_ai_session_terminate_enabled: bool,
    /// Per-risk-level termination flags (e.g., "critical" -> true, "high" -> false)
    pub level_terminate_flags: HashMap<String, bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threat_level_from_risk_score_none() {
        assert_eq!(ThreatLevel::from_risk_score(0), ThreatLevel::None);
    }

    #[test]
    fn test_threat_level_from_risk_score_low() {
        assert_eq!(ThreatLevel::from_risk_score(1), ThreatLevel::Low);
        assert_eq!(ThreatLevel::from_risk_score(3), ThreatLevel::Low);
        assert_eq!(ThreatLevel::from_risk_score(5), ThreatLevel::Low);
    }

    #[test]
    fn test_threat_level_from_risk_score_medium() {
        assert_eq!(ThreatLevel::from_risk_score(6), ThreatLevel::Medium);
        assert_eq!(ThreatLevel::from_risk_score(8), ThreatLevel::Medium);
        assert_eq!(ThreatLevel::from_risk_score(10), ThreatLevel::Medium);
    }

    #[test]
    fn test_threat_level_from_risk_score_high() {
        assert_eq!(ThreatLevel::from_risk_score(11), ThreatLevel::High);
        assert_eq!(ThreatLevel::from_risk_score(13), ThreatLevel::High);
        assert_eq!(ThreatLevel::from_risk_score(15), ThreatLevel::High);
    }

    #[test]
    fn test_threat_level_from_risk_score_critical() {
        assert_eq!(ThreatLevel::from_risk_score(16), ThreatLevel::Critical);
        assert_eq!(ThreatLevel::from_risk_score(18), ThreatLevel::Critical);
        assert_eq!(ThreatLevel::from_risk_score(20), ThreatLevel::Critical);
    }

    #[test]
    fn test_threat_level_from_risk_score_above_max() {
        // Scores above 20 should clamp to Critical
        assert_eq!(ThreatLevel::from_risk_score(21), ThreatLevel::Critical);
        assert_eq!(ThreatLevel::from_risk_score(255), ThreatLevel::Critical);
    }

    #[test]
    fn test_threat_level_to_risk_score_midpoints() {
        assert_eq!(ThreatLevel::None.to_risk_score(), 0);
        assert_eq!(ThreatLevel::Low.to_risk_score(), 3);
        assert_eq!(ThreatLevel::Medium.to_risk_score(), 8);
        assert_eq!(ThreatLevel::High.to_risk_score(), 13);
        assert_eq!(ThreatLevel::Critical.to_risk_score(), 18);
    }

    #[test]
    fn test_threat_level_roundtrip() {
        // Converting to midpoint risk score and back should yield the same level
        for level in [
            ThreatLevel::None,
            ThreatLevel::Low,
            ThreatLevel::Medium,
            ThreatLevel::High,
            ThreatLevel::Critical,
        ] {
            let score = level.to_risk_score();
            let roundtripped = ThreatLevel::from_risk_score(score);
            assert_eq!(level, roundtripped, "Roundtrip failed for {:?}", level);
        }
    }

    #[test]
    fn test_threat_level_ordering() {
        assert!(ThreatLevel::None < ThreatLevel::Low);
        assert!(ThreatLevel::Low < ThreatLevel::Medium);
        assert!(ThreatLevel::Medium < ThreatLevel::High);
        assert!(ThreatLevel::High < ThreatLevel::Critical);
    }

    #[test]
    fn test_threat_result_default() {
        let result = ThreatResult::default();
        assert_eq!(result.level, ThreatLevel::None);
        assert_eq!(result.risk_score, 0);
        assert_eq!(result.risk_level_source, RiskSource::ModelDefault);
        assert_eq!(result.confidence, 0.0);
        assert!(!result.is_threat());
        assert!(!result.should_terminate());
        assert!(result.command_text.is_none());
        assert!(result.risk_category.is_none());
        assert!(!result.tag_matches.has_deny_tags());
        assert!(!result.tag_matches.has_allow_tags());
        assert!(!result.should_terminate_session);
    }

    #[test]
    fn test_threat_result_is_threat() {
        let mut result = ThreatResult::default();
        assert!(!result.is_threat());

        result.level = ThreatLevel::Low;
        assert!(result.is_threat());

        result.level = ThreatLevel::Critical;
        assert!(result.is_threat());
    }

    #[test]
    fn test_threat_result_should_terminate_uses_field() {
        let result = ThreatResult {
            level: ThreatLevel::Critical,
            ..Default::default()
        };
        // Even with Critical level, should_terminate returns false unless the field is set
        assert!(!result.should_terminate());

        let result = ThreatResult {
            level: ThreatLevel::Critical,
            should_terminate_session: true,
            ..Default::default()
        };
        assert!(result.should_terminate());
    }

    #[test]
    fn test_risk_source_default() {
        assert_eq!(RiskSource::default(), RiskSource::ModelDefault);
    }

    #[test]
    fn test_tag_matches_empty() {
        let matches = TagMatches::default();
        assert!(!matches.has_deny_tags());
        assert!(!matches.has_allow_tags());
        assert!(!matches.has_only_allow_tags());
    }

    #[test]
    fn test_tag_matches_with_deny() {
        let matches = TagMatches {
            deny_tags: vec![TagMatch {
                tag: "rm -rf".to_string(),
                level: "critical".to_string(),
                tag_type: "deny".to_string(),
            }],
            allow_tags: vec![],
        };
        assert!(matches.has_deny_tags());
        assert!(!matches.has_allow_tags());
        assert!(!matches.has_only_allow_tags());
    }

    #[test]
    fn test_tag_matches_with_allow_only() {
        let matches = TagMatches {
            deny_tags: vec![],
            allow_tags: vec![TagMatch {
                tag: "ls".to_string(),
                level: "low".to_string(),
                tag_type: "allow".to_string(),
            }],
        };
        assert!(!matches.has_deny_tags());
        assert!(matches.has_allow_tags());
        assert!(matches.has_only_allow_tags());
    }

    #[test]
    fn test_tag_matches_with_both() {
        let matches = TagMatches {
            deny_tags: vec![TagMatch {
                tag: "rm".to_string(),
                level: "high".to_string(),
                tag_type: "deny".to_string(),
            }],
            allow_tags: vec![TagMatch {
                tag: "ls".to_string(),
                level: "low".to_string(),
                tag_type: "allow".to_string(),
            }],
        };
        assert!(matches.has_deny_tags());
        assert!(matches.has_allow_tags());
        assert!(!matches.has_only_allow_tags());
    }

    #[test]
    fn test_threat_result_serialization_roundtrip() {
        let result = ThreatResult {
            level: ThreatLevel::High,
            risk_score: 13,
            risk_level_source: RiskSource::CustomTagRule,
            confidence: 0.95,
            description: "Destructive command detected".to_string(),
            action: "terminate".to_string(),
            command_text: Some("rm -rf /".to_string()),
            risk_category: Some("DestructiveActivity".to_string()),
            tag_matches: TagMatches {
                deny_tags: vec![TagMatch {
                    tag: "rm.*-rf".to_string(),
                    level: "critical".to_string(),
                    tag_type: "deny".to_string(),
                }],
                allow_tags: vec![],
            },
            should_terminate_session: true,
            metadata: serde_json::json!({"source": "tag_rule"}),
        };

        let json = serde_json::to_string(&result).expect("serialization failed");
        let deserialized: ThreatResult =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(deserialized.level, ThreatLevel::High);
        assert_eq!(deserialized.risk_score, 13);
        assert_eq!(deserialized.risk_level_source, RiskSource::CustomTagRule);
        assert_eq!(deserialized.confidence, 0.95);
        assert!(deserialized.should_terminate_session);
        assert_eq!(deserialized.command_text, Some("rm -rf /".to_string()));
        assert!(deserialized.tag_matches.has_deny_tags());
    }

    #[test]
    fn test_threat_result_deserialization_missing_optional_fields() {
        // Simulate JSON from an older producer that only has the original fields
        let json = r#"{
            "level": "high",
            "confidence": 0.9,
            "description": "test threat",
            "action": "terminate",
            "risk_score": 13,
            "should_terminate_session": true,
            "metadata": null
        }"#;

        let result: ThreatResult = serde_json::from_str(json).expect("deserialization failed");
        assert_eq!(result.level, ThreatLevel::High);
        assert_eq!(result.risk_level_source, RiskSource::ModelDefault); // default
        assert!(result.command_text.is_none()); // default
        assert!(result.risk_category.is_none()); // default
        assert!(!result.tag_matches.has_deny_tags()); // default empty
    }

    #[test]
    fn test_termination_policy_default() {
        let policy = TerminationPolicy::default();
        assert!(!policy.config_allow_ai_session_terminate);
        assert!(!policy.resource_ai_session_terminate_enabled);
        assert!(policy.level_terminate_flags.is_empty());
    }

    #[test]
    fn test_analysis_record_serialization() {
        let record = AnalysisRecord {
            risk_score: 18,
            risk_level: "critical".to_string(),
            risk_category: "DestructiveActivity".to_string(),
            action_effects: "Deletes all files on system".to_string(),
            command_text: Some("rm -rf /".to_string()),
            overall_summary: "User attempted destructive command".to_string(),
            risk_level_source: RiskSource::ModelDefault,
            tag_matches: TagMatches::default(),
            should_terminate: true,
        };

        let json = serde_json::to_string(&record).expect("serialization failed");
        let deserialized: AnalysisRecord =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(deserialized.risk_score, 18);
        assert_eq!(deserialized.risk_level, "critical");
        assert!(deserialized.should_terminate);
    }
}
