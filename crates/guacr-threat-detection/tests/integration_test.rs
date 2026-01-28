//! Integration tests for threat detection
//!
//! These tests verify the threat detection module compiles and basic functionality works.
//!
//! Run tests with:
//!   cargo test --package guacr-threat-detection --test integration_test -- --include-ignored

use guacr_threat_detection::{ThreatDetector, ThreatDetectorConfig, ThreatLevel};

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_threat_detector_creation() {
        // Test that we can create a threat detector with default config
        let config = ThreatDetectorConfig::default();
        let detector = ThreatDetector::new(config);

        // Verify it was created successfully
        assert!(detector.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn test_threat_detector_with_custom_config() {
        // Test creating detector with custom configuration
        let config = ThreatDetectorConfig {
            enabled: true,
            baml_endpoint: "http://localhost:8080".to_string(),
            ..Default::default()
        };

        let detector = ThreatDetector::new(config);
        assert!(detector.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn test_threat_detector_disabled() {
        // Test that disabled detector can be created
        let config = ThreatDetectorConfig {
            enabled: false,
            ..Default::default()
        };

        let detector = ThreatDetector::new(config);
        assert!(detector.is_ok());
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_threat_level_exists() {
        // Just verify the types exist and can be constructed
        let _low = ThreatLevel::Low;
        let _medium = ThreatLevel::Medium;
        let _high = ThreatLevel::High;
        let _critical = ThreatLevel::Critical;
    }

    #[test]
    fn test_config_defaults() {
        let config = ThreatDetectorConfig::default();
        assert!(!config.enabled); // Disabled by default
        assert!(config.auto_terminate);
        assert_eq!(config.command_history_size, 10);
    }

    #[test]
    fn test_detector_creation() {
        let config = ThreatDetectorConfig::default();
        let detector = ThreatDetector::new(config);
        assert!(detector.is_ok());
    }
}
