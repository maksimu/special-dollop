//! Performance alert system for proactive monitoring

use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Alert severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

impl std::fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertSeverity::Info => write!(f, "Info"),
            AlertSeverity::Warning => write!(f, "Warning"),
            AlertSeverity::Critical => write!(f, "Critical"),
        }
    }
}

/// Types of performance alerts
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PerformanceAlertType {
    HighLatency,
    PacketLoss,
    LowBandwidth,
    ConnectionDegraded,
    ICEConnectionFailed,
    DTLSHandshakeFailed,
    HighErrorRate,
    MessageQueueBacklog,
    ResourceExhaustion,
    UnusualTrafficPattern,
}

impl std::fmt::Display for PerformanceAlertType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PerformanceAlertType::HighLatency => write!(f, "High Latency"),
            PerformanceAlertType::PacketLoss => write!(f, "Packet Loss"),
            PerformanceAlertType::LowBandwidth => write!(f, "Low Bandwidth"),
            PerformanceAlertType::ConnectionDegraded => write!(f, "Connection Degraded"),
            PerformanceAlertType::ICEConnectionFailed => write!(f, "ICE Connection Failed"),
            PerformanceAlertType::DTLSHandshakeFailed => write!(f, "DTLS Handshake Failed"),
            PerformanceAlertType::HighErrorRate => write!(f, "High Error Rate"),
            PerformanceAlertType::MessageQueueBacklog => write!(f, "Message Queue Backlog"),
            PerformanceAlertType::ResourceExhaustion => write!(f, "Resource Exhaustion"),
            PerformanceAlertType::UnusualTrafficPattern => write!(f, "Unusual Traffic Pattern"),
        }
    }
}

/// Performance alert with context and details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceAlert {
    /// Unique alert ID
    pub id: String,
    /// Type of alert
    pub alert_type: PerformanceAlertType,
    /// Severity level
    pub severity: AlertSeverity,
    /// When the alert was first triggered
    pub triggered_at: DateTime<Utc>,
    /// When the alert was last updated
    pub last_updated: DateTime<Utc>,
    /// Associated conversation ID (if applicable)
    pub conversation_id: Option<String>,
    /// Associated tube ID (if applicable)
    pub tube_id: Option<String>,
    /// Human-readable alert message
    pub message: String,
    /// Detailed alert information
    pub details: HashMap<String, String>,
    /// Whether this alert is currently active
    pub active: bool,
    /// Number of times this alert has been triggered
    pub occurrence_count: u32,
}

impl PerformanceAlert {
    pub fn new(
        alert_type: PerformanceAlertType,
        severity: AlertSeverity,
        message: String,
        conversation_id: Option<String>,
        tube_id: Option<String>,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();

        Self {
            id,
            alert_type,
            severity,
            triggered_at: now,
            last_updated: now,
            conversation_id,
            tube_id,
            message,
            details: HashMap::new(),
            active: true,
            occurrence_count: 1,
        }
    }

    /// Add detail to the alert
    pub fn add_detail<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) {
        self.details.insert(key.into(), value.into());
    }

    /// Update alert occurrence
    #[allow(dead_code)]
    pub fn update_occurrence(&mut self) {
        self.occurrence_count += 1;
        self.last_updated = Utc::now();
    }

    /// Resolve the alert
    pub fn resolve(&mut self) {
        self.active = false;
        self.last_updated = Utc::now();
    }

    /// Get alert age in seconds
    #[allow(dead_code)]
    pub fn age_seconds(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.triggered_at)
            .num_seconds()
    }
}

/// Alert configuration and thresholds
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// High latency threshold in milliseconds
    pub high_latency_threshold_ms: f64,
    /// Packet loss threshold (0.0 - 1.0)
    pub packet_loss_threshold: f64,
    /// Low bandwidth threshold in bits per second
    pub low_bandwidth_threshold_bps: f64,
    /// High error rate threshold (errors per second)
    pub high_error_rate_threshold: f64,
    /// Message queue backlog threshold
    pub message_queue_threshold: u32,
    /// Alert suppression time in seconds (prevent spam)
    pub alert_suppression_seconds: u64,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            high_latency_threshold_ms: 200.0,       // 200ms
            packet_loss_threshold: 0.05,            // 5%
            low_bandwidth_threshold_bps: 100_000.0, // 100 Kbps
            high_error_rate_threshold: 5.0,         // 5 errors/sec
            message_queue_threshold: 100,           // 100 queued messages
            alert_suppression_seconds: 300,         // 5 minutes
        }
    }
}

/// Type alias for the suppression cache key-value mapping
type SuppressionCache = Arc<RwLock<HashMap<(PerformanceAlertType, Option<String>), DateTime<Utc>>>>;

/// Alert manager for handling performance alerts
#[derive(Debug)]
pub struct AlertManager {
    config: AlertConfig,
    active_alerts: Arc<RwLock<HashMap<String, PerformanceAlert>>>,
    alert_history: Arc<RwLock<Vec<PerformanceAlert>>>,
    suppression_cache: SuppressionCache,
}

impl AlertManager {
    pub fn new(config: AlertConfig) -> Self {
        Self {
            config,
            active_alerts: Arc::new(RwLock::new(HashMap::new())),
            alert_history: Arc::new(RwLock::new(Vec::new())),
            suppression_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check metrics against thresholds and generate alerts
    pub fn check_metrics(&self, metrics: &crate::metrics::ConnectionMetrics) {
        // Check high latency
        if let Some(rtt) = metrics.webrtc_metrics.rtc_stats.rtt_ms {
            if rtt > self.config.high_latency_threshold_ms {
                self.maybe_trigger_alert(
                    PerformanceAlertType::HighLatency,
                    AlertSeverity::Warning,
                    format!("RTT increased to {:.1}ms", rtt),
                    Some(metrics.conversation_id.clone()),
                    Some(metrics.tube_id.clone()),
                    vec![("rtt_ms", rtt.to_string())],
                );
            }
        }

        // Check packet loss
        let packet_loss = metrics.webrtc_metrics.rtc_stats.packet_loss_rate;
        if packet_loss > self.config.packet_loss_threshold {
            let severity = if packet_loss > 0.1 {
                AlertSeverity::Critical
            } else {
                AlertSeverity::Warning
            };

            self.maybe_trigger_alert(
                PerformanceAlertType::PacketLoss,
                severity,
                format!("Packet loss at {:.1}%", packet_loss * 100.0),
                Some(metrics.conversation_id.clone()),
                Some(metrics.tube_id.clone()),
                vec![("packet_loss_rate", packet_loss.to_string())],
            );
        }

        // Check bandwidth
        let current_bitrate = metrics.webrtc_metrics.rtc_stats.current_bitrate;
        if current_bitrate < self.config.low_bandwidth_threshold_bps && current_bitrate > 0.0 {
            self.maybe_trigger_alert(
                PerformanceAlertType::LowBandwidth,
                AlertSeverity::Warning,
                format!("Low bandwidth: {:.0} bps", current_bitrate),
                Some(metrics.conversation_id.clone()),
                Some(metrics.tube_id.clone()),
                vec![("current_bitrate", current_bitrate.to_string())],
            );
        }

        // Check error rate
        if metrics.error_rate > self.config.high_error_rate_threshold {
            self.maybe_trigger_alert(
                PerformanceAlertType::HighErrorRate,
                AlertSeverity::Critical,
                format!("High error rate: {:.1} errors/sec", metrics.error_rate),
                Some(metrics.conversation_id.clone()),
                Some(metrics.tube_id.clone()),
                vec![
                    ("error_rate", metrics.error_rate.to_string()),
                    ("total_errors", metrics.total_errors.to_string()),
                ],
            );
        }

        // Check message queue backlog
        let queue_depth = metrics.webrtc_metrics.sctp_stats.message_queue_depth;
        if queue_depth > self.config.message_queue_threshold {
            self.maybe_trigger_alert(
                PerformanceAlertType::MessageQueueBacklog,
                AlertSeverity::Warning,
                format!("Message queue backlog: {} messages", queue_depth),
                Some(metrics.conversation_id.clone()),
                Some(metrics.tube_id.clone()),
                vec![("queue_depth", queue_depth.to_string())],
            );
        }
    }

    /// Trigger alert if not suppressed
    fn maybe_trigger_alert(
        &self,
        alert_type: PerformanceAlertType,
        severity: AlertSeverity,
        message: String,
        conversation_id: Option<String>,
        tube_id: Option<String>,
        details: Vec<(&str, String)>,
    ) {
        // Check suppression
        let suppression_key = (alert_type.clone(), conversation_id.clone());
        if let Ok(suppression) = self.suppression_cache.read() {
            if let Some(last_alert) = suppression.get(&suppression_key) {
                let now = Utc::now();
                let elapsed = now.signed_duration_since(*last_alert).num_seconds() as u64;
                if elapsed < self.config.alert_suppression_seconds {
                    return; // Suppressed
                }
            }
        }

        // Create alert
        let mut alert = PerformanceAlert::new(
            alert_type.clone(),
            severity,
            message.clone(),
            conversation_id.clone(),
            tube_id,
        );

        // Add details
        for (key, value) in details {
            alert.add_detail(key, value);
        }

        // Log the alert
        match severity {
            AlertSeverity::Info => {
                info!(
                    "{} (alert_type: {}, conversation_id: {:?})",
                    message, alert_type, conversation_id
                );
            }
            AlertSeverity::Warning => {
                warn!(
                    "{} (alert_type: {}, conversation_id: {:?})",
                    message, alert_type, conversation_id
                );
            }
            AlertSeverity::Critical => {
                log::error!(
                    "{} (alert_type: {}, conversation_id: {:?})",
                    message,
                    alert_type,
                    conversation_id
                );
            }
        }

        // Store alert
        if let Ok(mut active_alerts) = self.active_alerts.write() {
            let alert_id = alert.id.clone();
            active_alerts.insert(alert_id, alert.clone());
        }

        // Add to history
        if let Ok(mut history) = self.alert_history.write() {
            history.push(alert);

            // Limit history size
            while history.len() > 1000 {
                history.remove(0);
            }
        }

        // Update suppression cache
        if let Ok(mut suppression) = self.suppression_cache.write() {
            suppression.insert(suppression_key, Utc::now());
        }
    }

    /// Get all active alerts
    #[allow(dead_code)] // Used by Python bindings
    pub fn get_active_alerts(&self) -> Vec<PerformanceAlert> {
        if let Ok(alerts) = self.active_alerts.read() {
            alerts.values().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Get alerts for a specific conversation
    #[allow(dead_code)]
    pub fn get_alerts_for_conversation(&self, conversation_id: &str) -> Vec<PerformanceAlert> {
        if let Ok(alerts) = self.active_alerts.read() {
            alerts
                .values()
                .filter(|alert| {
                    alert
                        .conversation_id
                        .as_ref()
                        .map(|id| id == conversation_id)
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Resolve an alert by ID
    #[allow(dead_code)]
    pub fn resolve_alert(&self, alert_id: &str) -> bool {
        if let Ok(mut alerts) = self.active_alerts.write() {
            if let Some(alert) = alerts.get_mut(alert_id) {
                alert.resolve();
                debug!("Alert resolved (alert_id: {})", alert_id);
                return true;
            }
        }
        false
    }

    /// Clear alerts for a specific conversation (when connection closes)
    #[allow(dead_code)]
    pub fn clear_conversation_alerts(&self, conversation_id: &str) {
        if let Ok(mut alerts) = self.active_alerts.write() {
            let to_remove: Vec<String> = alerts
                .iter()
                .filter(|(_, alert)| {
                    alert
                        .conversation_id
                        .as_ref()
                        .map(|id| id == conversation_id)
                        .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect();

            for alert_id in to_remove {
                if let Some(mut alert) = alerts.remove(&alert_id) {
                    alert.resolve();
                    debug!(
                        "Alert cleared for closed connection (alert_id: {}, conversation_id: {})",
                        alert_id, conversation_id
                    );
                }
            }
        }
    }

    /// Get alert statistics
    pub fn get_alert_stats(&self) -> (u32, u32, u32) {
        if let Ok(alerts) = self.active_alerts.read() {
            let total = alerts.len() as u32;
            let mut critical = 0;
            let mut warning = 0;

            for alert in alerts.values() {
                match alert.severity {
                    AlertSeverity::Critical => critical += 1,
                    AlertSeverity::Warning => warning += 1,
                    AlertSeverity::Info => {}
                }
            }

            (total, critical, warning)
        } else {
            (0, 0, 0)
        }
    }

    /// Clean up old inactive alerts
    pub fn cleanup_old_alerts(&self) {
        let cutoff = Utc::now() - chrono::Duration::hours(24);

        if let Ok(mut alerts) = self.active_alerts.write() {
            let to_remove: Vec<String> = alerts
                .iter()
                .filter(|(_, alert)| !alert.active && alert.last_updated < cutoff)
                .map(|(id, _)| id.clone())
                .collect();

            for alert_id in to_remove {
                alerts.remove(&alert_id);
            }
        }

        // Clean suppression cache
        if let Ok(mut suppression) = self.suppression_cache.write() {
            suppression.retain(|_, timestamp| {
                Utc::now().signed_duration_since(*timestamp).num_hours() < 24
            });
        }
    }
}
