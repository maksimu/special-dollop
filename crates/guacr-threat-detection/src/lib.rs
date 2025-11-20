// guacr-threat-detection: AI threat detection using BAML REST API
//
// Monitors session activity and calls BAML REST API to detect threats.
// Can terminate sessions automatically on threat detection.

mod baml_client;
mod detector;
mod threat;

pub use baml_client::{Analysis, CommandSummaryResponse, KeystrokeAnalysisResponse};
pub use detector::{ThreatDetector, ThreatDetectorConfig};
pub use threat::{ThreatLevel, ThreatResult};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ThreatDetectionError {
    #[error("BAML API error: {0}")]
    BamlApiError(String),

    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Configuration error: {0}")]
    ConfigurationError(String),
}

pub type Result<T> = std::result::Result<T, ThreatDetectionError>;
