//! Performance Metrics Collection System for WebRTC Gateway
//!
//! This module provides comprehensive performance monitoring for WebRTC-based
//! RDP/VNC/SSH tunneling connections. It tracks metrics across all connection
//! legs: WebRTC transport, SCTP, and application-level message flow.

mod alerts;
mod collector;
mod handle;
mod sliding_window;
mod types;

pub use alerts::PerformanceAlert;
pub use collector::METRICS_COLLECTOR;
pub use handle::MetricsHandle;
pub use types::{ConnectionMetrics, ConnectionQuality};

// Note: Constants removed as they are not currently used by the implementation
// They can be re-added when needed for future dashboard or export functionality

/// Connection quality thresholds
pub const EXCELLENT_RTT_MS: f64 = 50.0;
pub const GOOD_RTT_MS: f64 = 100.0;
pub const FAIR_RTT_MS: f64 = 200.0;
pub const POOR_PACKET_LOSS_THRESHOLD: f64 = 0.05; // 5%
