//! Core metrics data structures and types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Connection quality assessment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionQuality {
    Excellent,
    Good,
    Fair,
    Poor,
}

impl ConnectionQuality {
    pub fn assess(rtt_ms: f64, packet_loss_rate: f64, jitter_ms: f64) -> Self {
        if packet_loss_rate > crate::metrics::POOR_PACKET_LOSS_THRESHOLD {
            return Self::Poor;
        }

        if rtt_ms <= crate::metrics::EXCELLENT_RTT_MS && jitter_ms <= 10.0 {
            Self::Excellent
        } else if rtt_ms <= crate::metrics::GOOD_RTT_MS && jitter_ms <= 20.0 {
            Self::Good
        } else if rtt_ms <= crate::metrics::FAIR_RTT_MS {
            Self::Fair
        } else {
            Self::Poor
        }
    }
}

/// ICE candidate gathering and connection metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ICEStats {
    /// Total ICE candidates gathered
    pub total_candidates: u32,
    /// Host candidates (direct connection)
    pub host_candidates: u32,
    /// Server reflexive candidates (STUN)
    pub srflx_candidates: u32,
    /// Relay candidates (TURN)
    pub relay_candidates: u32,
    /// Time to gather first candidate (ms)
    pub first_candidate_time_ms: Option<f64>,
    /// Time to complete gathering (ms)
    pub gathering_complete_time_ms: Option<f64>,
    /// STUN server response times (ms)
    pub stun_response_times: Vec<f64>,
    /// TURN allocation success rate (0.0 - 1.0)
    pub turn_allocation_success_rate: f64,
    /// TURN allocation time (ms)
    pub turn_allocation_time_ms: Option<f64>,
    /// Selected candidate pair details
    pub selected_candidate_pair: Option<CandidatePairStats>,
}

impl Default for ICEStats {
    fn default() -> Self {
        Self {
            total_candidates: 0,
            host_candidates: 0,
            srflx_candidates: 0,
            relay_candidates: 0,
            first_candidate_time_ms: None,
            gathering_complete_time_ms: None,
            stun_response_times: Vec::new(),
            turn_allocation_success_rate: 0.0,
            turn_allocation_time_ms: None,
            selected_candidate_pair: None,
        }
    }
}

/// Selected ICE candidate pair performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePairStats {
    /// Local candidate type (host, srflx, relay)
    pub local_candidate_type: String,
    /// Remote candidate type
    pub remote_candidate_type: String,
    /// Current round-trip time (ms)
    pub current_rtt_ms: f64,
    /// Total round-trip time samples
    pub total_rtt_measurements: u64,
    /// Connection establishment time (ms)
    pub connection_time_ms: Option<f64>,
    /// Bytes sent through this pair
    pub bytes_sent: u64,
    /// Bytes received through this pair
    pub bytes_received: u64,
    /// Path type (direct, relay, etc)
    pub transport_protocol: String,
}

/// Real-time WebRTC transport statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RTCStats {
    /// Round-trip time in milliseconds
    pub rtt_ms: Option<f64>,
    /// Packet jitter in milliseconds
    pub jitter_ms: f64,
    /// Packet loss rate (0.0 - 1.0)
    pub packet_loss_rate: f64,
    /// Current bitrate in bits per second
    pub current_bitrate: f64,
    /// Available bandwidth estimation
    pub available_bandwidth: Option<f64>,
    /// Target bitrate
    pub target_bitrate: Option<f64>,
    /// ICE connection state
    pub ice_connection_state: String,
    /// DTLS connection ready state
    pub dtls_ready: bool,
    /// Number of active data channels
    pub active_data_channels: u32,
    /// ICE and candidate statistics
    pub ice_stats: ICEStats,
}

impl Default for RTCStats {
    fn default() -> Self {
        Self {
            rtt_ms: None,
            jitter_ms: 0.0,
            packet_loss_rate: 0.0,
            current_bitrate: 0.0,
            available_bandwidth: None,
            target_bitrate: None,
            ice_connection_state: "new".to_string(),
            dtls_ready: false,
            active_data_channels: 0,
            ice_stats: ICEStats::default(),
        }
    }
}

/// SCTP transport layer statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SCTPStats {
    /// Congestion window size
    pub cwnd: u32,
    /// Receiver window size
    pub rwnd: u32,
    /// Number of fast retransmissions
    pub fast_retransmits: u64,
    /// Number of timeout retransmissions
    pub timeout_retransmits: u64,
    /// Current message queue depth
    pub message_queue_depth: u32,
    /// Buffer utilization percentage (0.0 - 1.0)
    pub buffer_utilization: f64,
}

impl Default for SCTPStats {
    fn default() -> Self {
        Self {
            cwnd: 0,
            rwnd: 0,
            fast_retransmits: 0,
            timeout_retransmits: 0,
            message_queue_depth: 0,
            buffer_utilization: 0.0,
        }
    }
}

/// Connection leg performance breakdown
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionLegMetrics {
    /// Client to KRelay latency (ms)
    pub client_to_krelay_latency_ms: Option<f64>,
    /// KRelay to Gateway latency (ms)
    pub krelay_to_gateway_latency_ms: Option<f64>,
    /// End-to-end client to gateway latency (ms)
    pub end_to_end_latency_ms: Option<f64>,
    /// STUN server response time (ms)
    pub stun_response_time_ms: Option<f64>,
    /// TURN allocation latency (ms)
    pub turn_allocation_latency_ms: Option<f64>,
    /// Data channel establishment time (ms)
    pub data_channel_establishment_ms: Option<f64>,
    /// ICE connection establishment time (ms)
    pub ice_connection_establishment_ms: Option<f64>,
}

/// Combined WebRTC metrics including RTC and SCTP stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebRTCMetrics {
    pub rtc_stats: RTCStats,
    pub sctp_stats: SCTPStats,
    /// Connection leg performance breakdown
    pub connection_legs: ConnectionLegMetrics,
    /// Timestamp when these metrics were collected
    pub collected_at: DateTime<Utc>,
}

impl Default for WebRTCMetrics {
    fn default() -> Self {
        Self {
            rtc_stats: RTCStats::default(),
            sctp_stats: SCTPStats::default(),
            connection_legs: ConnectionLegMetrics::default(),
            collected_at: Utc::now(),
        }
    }
}

/// Comprehensive per-connection metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMetrics {
    /// Unique conversation ID
    pub conversation_id: String,
    /// Associated tube ID
    pub tube_id: String,
    /// When connection was established
    pub established_at: DateTime<Utc>,

    // Message flow metrics
    /// Total messages sent
    pub messages_sent: u64,
    /// Total messages received
    pub messages_received: u64,
    /// Total bytes sent
    pub total_bytes_sent: u64,
    /// Total bytes received
    pub total_bytes_received: u64,

    // Latency metrics (sliding window averages)
    /// Current average RTT
    pub avg_rtt: Duration,
    /// 95th percentile latency
    pub p95_latency: Duration,
    /// 99th percentile latency
    pub p99_latency: Duration,
    /// Current message latency
    pub current_message_latency: Option<Duration>,

    // Throughput metrics (sliding window averages)
    /// Current average throughput in bytes per second
    pub avg_throughput: f64,
    /// Peak throughput in bytes per second (1-second window)
    pub peak_throughput: f64,

    // Real-time WebRTC metrics
    pub webrtc_metrics: WebRTCMetrics,

    // Health indicators
    /// Overall connection quality assessment
    pub connection_quality: ConnectionQuality,
    /// Number of active alerts for this connection
    pub active_alert_count: u32,

    // Error tracking
    /// Error rate (errors per second)
    pub error_rate: f64,
    /// Total error count
    pub total_errors: u64,
    /// Retry count
    pub retry_count: u64,
}

impl ConnectionMetrics {
    #[allow(dead_code)]
    pub fn new(conversation_id: String, tube_id: String) -> Self {
        Self {
            conversation_id,
            tube_id,
            established_at: Utc::now(),
            messages_sent: 0,
            messages_received: 0,
            total_bytes_sent: 0,
            total_bytes_received: 0,
            avg_rtt: Duration::from_millis(0),
            p95_latency: Duration::from_millis(0),
            p99_latency: Duration::from_millis(0),
            current_message_latency: None,
            avg_throughput: 0.0,
            peak_throughput: 0.0,
            webrtc_metrics: WebRTCMetrics::default(),
            connection_quality: ConnectionQuality::Poor, // Start pessimistic
            active_alert_count: 0,
            error_rate: 0.0,
            total_errors: 0,
            retry_count: 0,
        }
    }

    /// Update connection quality based on current metrics
    pub fn update_quality(&mut self) {
        let rtt_ms = self.webrtc_metrics.rtc_stats.rtt_ms.unwrap_or(f64::MAX);
        let packet_loss = self.webrtc_metrics.rtc_stats.packet_loss_rate;
        let jitter_ms = self.webrtc_metrics.rtc_stats.jitter_ms;

        self.connection_quality = ConnectionQuality::assess(rtt_ms, packet_loss, jitter_ms);
    }

    /// Get current throughput in messages per second
    pub fn message_throughput(&self) -> f64 {
        let duration = Utc::now().signed_duration_since(self.established_at);
        let duration_secs = duration.num_seconds() as f64;
        if duration_secs > 0.0 {
            (self.messages_sent + self.messages_received) as f64 / duration_secs
        } else {
            0.0
        }
    }

    /// Get current bandwidth utilization in bytes per second
    pub fn bandwidth_utilization(&self) -> f64 {
        let duration = Utc::now().signed_duration_since(self.established_at);
        let duration_secs = duration.num_seconds() as f64;
        if duration_secs > 0.0 {
            (self.total_bytes_sent + self.total_bytes_received) as f64 / duration_secs
        } else {
            0.0
        }
    }
}

/// System-wide aggregated metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedMetrics {
    /// Timestamp of this aggregation
    pub timestamp: DateTime<Utc>,
    /// Total number of active connections
    pub active_connections: u32,
    /// Total number of tubes
    pub active_tubes: u32,

    // System-wide averages
    /// Average RTT across all connections
    pub avg_system_rtt: Duration,
    /// Average packet loss rate
    pub avg_packet_loss: f64,
    /// Average P95 latency across all connections
    pub avg_p95_latency: Duration,
    /// Average P99 latency across all connections
    pub avg_p99_latency: Duration,
    /// Total system throughput (messages/sec)
    pub total_message_throughput: f64,
    /// Total system bandwidth (bytes/sec)
    pub total_bandwidth: f64,

    // Quality distribution
    /// Number of excellent quality connections
    pub excellent_connections: u32,
    /// Number of good quality connections
    pub good_connections: u32,
    /// Number of fair quality connections
    pub fair_connections: u32,
    /// Number of poor quality connections
    pub poor_connections: u32,

    // Alert summary
    /// Total number of active alerts
    pub total_alerts: u32,
    /// Number of critical alerts
    pub critical_alerts: u32,
    /// Number of warning alerts
    pub warning_alerts: u32,

    // Resource utilization
    /// Memory usage in bytes
    pub memory_usage_bytes: u64,
    /// CPU utilization percentage (0.0 - 1.0)
    pub cpu_utilization: f64,
    /// Network utilization percentage (0.0 - 1.0)
    pub network_utilization: f64,
}

impl Default for AggregatedMetrics {
    fn default() -> Self {
        Self {
            timestamp: Utc::now(),
            active_connections: 0,
            active_tubes: 0,
            avg_system_rtt: Duration::from_millis(0),
            avg_packet_loss: 0.0,
            avg_p95_latency: Duration::from_millis(0),
            avg_p99_latency: Duration::from_millis(0),
            total_message_throughput: 0.0,
            total_bandwidth: 0.0,
            excellent_connections: 0,
            good_connections: 0,
            fair_connections: 0,
            poor_connections: 0,
            total_alerts: 0,
            critical_alerts: 0,
            warning_alerts: 0,
            memory_usage_bytes: 0,
            cpu_utilization: 0.0,
            network_utilization: 0.0,
        }
    }
}

/// Point-in-time metrics snapshot for export
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Used by Python bindings
pub struct MetricsSnapshot {
    /// When this snapshot was taken
    pub timestamp: DateTime<Utc>,
    /// Per-connection metrics
    pub connections: HashMap<String, ConnectionMetrics>,
    /// System-wide aggregated metrics
    pub aggregated: AggregatedMetrics,
    /// Currently active alerts
    pub alerts: Vec<crate::metrics::PerformanceAlert>,
}

impl MetricsSnapshot {
    #[allow(dead_code)] // Used by Python bindings
    pub fn new() -> Self {
        Self {
            timestamp: Utc::now(),
            connections: HashMap::new(),
            aggregated: AggregatedMetrics::default(),
            alerts: Vec::new(),
        }
    }
}

/// Atomic metrics counters for high-performance collection
#[derive(Debug)]
pub struct AtomicMetrics {
    pub messages_sent: AtomicU64,
    pub messages_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub error_count: AtomicU64,
    pub retry_count: AtomicU64,
}

impl AtomicMetrics {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            retry_count: AtomicU64::new(0),
        }
    }

    pub fn increment_messages_sent(&self) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_messages_received(&self) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_bytes_sent(&self, bytes: u64) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_bytes_received(&self, bytes: u64) {
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn increment_errors(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64) {
        (
            self.messages_sent.load(Ordering::Relaxed),
            self.messages_received.load(Ordering::Relaxed),
            self.bytes_sent.load(Ordering::Relaxed),
            self.bytes_received.load(Ordering::Relaxed),
            self.error_count.load(Ordering::Relaxed),
            self.retry_count.load(Ordering::Relaxed),
        )
    }
}
