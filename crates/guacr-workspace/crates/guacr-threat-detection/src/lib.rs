// guacr-threat-detection: AI threat detection using BAML REST API
//
// Monitors session activity and calls BAML REST API to detect threats.
// Can terminate sessions automatically on threat detection.
//
// Supports two modes:
// 1. Reactive: Analyzes commands after they're sent (current behavior)
// 2. Proactive: Buffers commands and requires AI approval before execution (new)

mod approval;
mod baml_client;
mod command_buffer;
mod detector;
mod proactive;
mod session_guard;
mod terminal_state;
mod threat;

pub use approval::{ApprovalDecision, ApprovalManager};
pub use baml_client::{Analysis, CommandSummaryResponse, KeystrokeAnalysisResponse};
pub use command_buffer::CommandBuffer;
pub use detector::{ThreatDetector, ThreatDetectorConfig};
pub use proactive::{
    handle_proactive_input, parse_proactive_config, ProactiveConfig, ProactiveResult,
    KEYSYM_BACKSPACE, KEYSYM_CTRL_C, KEYSYM_CTRL_D, KEYSYM_CTRL_Z, KEYSYM_DELETE, KEYSYM_RETURN,
};
pub use session_guard::SessionGuard;
pub use terminal_state::{TerminalStateContext, TerminalStateExtractor, UserBehaviorMetrics};
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
