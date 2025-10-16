//! Central metrics collector for WebRTC Gateway performance monitoring

use super::alerts::{AlertConfig, AlertManager};
use super::sliding_window::MultiWindow;
use super::types::{
    AggregatedMetrics, AtomicMetrics, ConnectionMetrics, MetricsSnapshot, RTCStats, SCTPStats,
    WebRTCMetrics,
};
use crate::unlikely;
use chrono::Utc;
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::time::interval;
use webrtc::stats::StatsReportType;

/// Configuration constants for metrics collection timing
const METRICS_AGGREGATION_INTERVAL_SECS: u64 = 10;
const METRICS_CLEANUP_INTERVAL_SECS: u64 = 300;
const WEBRTC_STATS_COLLECTION_INTERVAL_SECS: u64 = 5;

/// Global metrics collector instance
pub static METRICS_COLLECTOR: Lazy<Arc<MetricsCollector>> = Lazy::new(|| {
    debug!("Initializing global metrics collector");
    let collector = Arc::new(MetricsCollector::new());
    collector.clone().start_background_tasks();
    collector
});

/// Connection-specific metrics state
#[derive(Debug)]
struct ConnectionState {
    metrics: ConnectionMetrics,
    atomic_counters: AtomicMetrics,
    rtt_window: MultiWindow,
    latency_window: MultiWindow,
    throughput_window: MultiWindow,
    error_window: MultiWindow,
    last_webrtc_update: Instant,
    last_alert_check: Instant,
}

impl ConnectionState {
    #[allow(dead_code)]
    fn new(conversation_id: String, tube_id: String) -> Self {
        Self {
            metrics: ConnectionMetrics::new(conversation_id, tube_id),
            atomic_counters: AtomicMetrics::new(),
            rtt_window: MultiWindow::new(),
            latency_window: MultiWindow::new(),
            throughput_window: MultiWindow::new(),
            error_window: MultiWindow::new(),
            last_webrtc_update: Instant::now(),
            last_alert_check: Instant::now(),
        }
    }
}

/// Central metrics collection and management system
#[derive(Debug)]
pub struct MetricsCollector {
    /// Per-connection metrics state
    connection_states: Arc<RwLock<HashMap<String, ConnectionState>>>,
    /// Alert manager for performance monitoring
    alert_manager: Arc<AlertManager>,
    /// Aggregated system metrics
    aggregated_metrics: Arc<RwLock<AggregatedMetrics>>,
    /// Metrics collection start time
    #[allow(dead_code)] // Used in get_uptime() method
    start_time: Instant,
    /// Background task handle
    background_task_running: Arc<RwLock<bool>>,
    /// Track if we've logged idle state (0 connections) to avoid spam
    idle_state_logged: Arc<std::sync::atomic::AtomicBool>,
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            connection_states: Arc::new(RwLock::new(HashMap::new())),
            alert_manager: Arc::new(AlertManager::new(AlertConfig::default())),
            aggregated_metrics: Arc::new(RwLock::new(AggregatedMetrics::default())),
            start_time: Instant::now(),
            background_task_running: Arc::new(RwLock::new(false)),
            idle_state_logged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start background tasks for metrics aggregation and cleanup
    fn start_background_tasks(self: Arc<Self>) {
        let task_running = self.background_task_running.clone();

        // Check if already running
        if let Ok(running) = task_running.read() {
            if *running {
                return;
            }
        }

        // Check if there's a Tokio runtime available before spawning tasks
        let runtime_handle = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            Err(_) => {
                // No runtime available, skip background tasks
                debug!("No Tokio runtime available, skipping background tasks");
                return;
            }
        };

        // Mark as running
        if let Ok(mut running) = task_running.write() {
            *running = true;
        }

        let collector_for_aggregation = self.clone();
        runtime_handle.spawn(async move {
            let mut interval = interval(Duration::from_secs(METRICS_AGGREGATION_INTERVAL_SECS));
            loop {
                interval.tick().await;
                collector_for_aggregation.aggregate_metrics().await;
            }
        });

        let collector_for_cleanup = self.clone();
        runtime_handle.spawn(async move {
            let mut interval = interval(Duration::from_secs(METRICS_CLEANUP_INTERVAL_SECS));
            loop {
                interval.tick().await;
                collector_for_cleanup.cleanup_old_data().await;
            }
        });

        let collector_for_stats = self.clone();
        runtime_handle.spawn(async move {
            let mut interval = interval(Duration::from_secs(WEBRTC_STATS_COLLECTION_INTERVAL_SECS));
            loop {
                interval.tick().await;
                collector_for_stats.collect_webrtc_stats().await;
            }
        });

        // Start zombie tube sweeper (backstop for missed cleanups)
        let collector_for_sweeper = self.clone();
        runtime_handle.spawn(async move {
            collector_for_sweeper.zombie_tube_sweeper().await;
        });

        debug!("Background metrics tasks started (aggregation, cleanup, stats collection, zombie sweeper)");
    }

    /// Register a new connection for metrics tracking
    #[allow(dead_code)]
    pub fn register_connection(&self, conversation_id: String, tube_id: String) {
        if let Ok(mut states) = self.connection_states.write() {
            let state = ConnectionState::new(conversation_id.clone(), tube_id.clone());
            states.insert(conversation_id.clone(), state);
            debug!(
                "Registered connection for metrics tracking (conversation_id: {}, tube_id: {})",
                conversation_id, tube_id
            );
        }
    }

    /// Unregister a connection from metrics tracking
    #[allow(dead_code)]
    pub fn unregister_connection(&self, conversation_id: &str) {
        if let Ok(mut states) = self.connection_states.write() {
            if states.remove(conversation_id).is_some() {
                // Clear alerts for this connection
                self.alert_manager
                    .clear_conversation_alerts(conversation_id);
                debug!(
                    "Unregistered connection from metrics tracking (conversation_id: {})",
                    conversation_id
                );
            }
        }
    }

    /// Record a message sent
    pub fn record_message_sent(
        &self,
        conversation_id: &str,
        bytes: u64,
        latency: Option<Duration>,
    ) {
        if let Ok(states) = self.connection_states.read() {
            if let Some(state) = states.get(conversation_id) {
                state.atomic_counters.increment_messages_sent();
                state.atomic_counters.add_bytes_sent(bytes);

                if let Some(latency_ms) = latency.map(|d| d.as_millis() as f64) {
                    state.latency_window.add(latency_ms);
                }

                // Track throughput: bytes per second
                state.throughput_window.add(bytes as f64);

                // Show periodic activity at DEBUG level (every 100th message or large messages)
                let msg_count = state.atomic_counters.snapshot().0;
                if msg_count % 100 == 0 || bytes > 1024 {
                    debug!(
                        "Message sent activity (conversation_id: {}, bytes: {}, msg_count: {})",
                        conversation_id, bytes, msg_count
                    );
                }

                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "Recorded message sent (conversation_id: {}, bytes: {})",
                        conversation_id, bytes
                    );
                }
            }
        }
    }

    /// Record a message received
    pub fn record_message_received(
        &self,
        conversation_id: &str,
        bytes: u64,
        latency: Option<Duration>,
    ) {
        if let Ok(states) = self.connection_states.read() {
            if let Some(state) = states.get(conversation_id) {
                state.atomic_counters.increment_messages_received();
                state.atomic_counters.add_bytes_received(bytes);

                if let Some(latency_ms) = latency.map(|d| d.as_millis() as f64) {
                    state.latency_window.add(latency_ms);
                }

                // Track throughput: bytes per second
                state.throughput_window.add(bytes as f64);

                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "Recorded message received (conversation_id: {}, bytes: {})",
                        conversation_id, bytes
                    );
                }
            }
        }
    }

    /// Record an error
    pub fn record_error(&self, conversation_id: &str, error_type: &str) {
        if let Ok(states) = self.connection_states.read() {
            if let Some(state) = states.get(conversation_id) {
                state.atomic_counters.increment_errors();
                state.error_window.add(1.0);
                debug!(
                    "Recorded error (conversation_id: {}, error_type: {})",
                    conversation_id, error_type
                );
            }
        }
    }

    /// Update ICE gathering start time
    pub fn update_ice_gathering_start(&self, conversation_id: &str, timestamp_ms: f64) {
        if let Ok(mut states) = self.connection_states.write() {
            if let Some(state) = states.get_mut(conversation_id) {
                // Mark first candidate time if not already set
                if state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .first_candidate_time_ms
                    .is_none()
                {
                    state
                        .metrics
                        .webrtc_metrics
                        .rtc_stats
                        .ice_stats
                        .first_candidate_time_ms = Some(timestamp_ms);
                }
                debug!(
                    "ICE gathering started (conversation_id: {}, timestamp: {})",
                    conversation_id, timestamp_ms
                );
            }
        }
    }

    /// Update ICE gathering completion time
    pub fn update_ice_gathering_complete(&self, conversation_id: &str, timestamp_ms: f64) {
        if let Ok(mut states) = self.connection_states.write() {
            if let Some(state) = states.get_mut(conversation_id) {
                state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .gathering_complete_time_ms = Some(timestamp_ms);

                // Calculate gathering duration if we have start time
                if let Some(start_time) = state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .first_candidate_time_ms
                {
                    let duration_ms = timestamp_ms - start_time;
                    state
                        .metrics
                        .webrtc_metrics
                        .connection_legs
                        .ice_connection_establishment_ms = Some(duration_ms);
                    debug!(
                        "ICE gathering completed (conversation_id: {}, duration: {:.1}ms)",
                        conversation_id, duration_ms
                    );
                } else {
                    debug!(
                        "ICE gathering completed (conversation_id: {}, timestamp: {})",
                        conversation_id, timestamp_ms
                    );
                }
            }
        }
    }

    /// Record STUN server response time
    #[allow(dead_code)]
    pub fn record_stun_response_time(&self, conversation_id: &str, response_time_ms: f64) {
        if let Ok(mut states) = self.connection_states.write() {
            if let Some(state) = states.get_mut(conversation_id) {
                state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .stun_response_times
                    .push(response_time_ms);
                state
                    .metrics
                    .webrtc_metrics
                    .connection_legs
                    .stun_response_time_ms = Some(response_time_ms);
                debug!(
                    "STUN response time recorded (conversation_id: {}, rtt: {:.1}ms)",
                    conversation_id, response_time_ms
                );
            }
        }
    }

    /// Record TURN allocation timing and success
    pub fn record_turn_allocation(
        &self,
        conversation_id: &str,
        allocation_time_ms: f64,
        success: bool,
    ) {
        if let Ok(mut states) = self.connection_states.write() {
            if let Some(state) = states.get_mut(conversation_id) {
                state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .turn_allocation_time_ms = Some(allocation_time_ms);
                state
                    .metrics
                    .webrtc_metrics
                    .connection_legs
                    .turn_allocation_latency_ms = Some(allocation_time_ms);

                // Update success rate (simple running average for now)
                let current_rate = state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .turn_allocation_success_rate;
                state
                    .metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .turn_allocation_success_rate = if current_rate == 0.0 {
                    if success {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    (current_rate + if success { 1.0 } else { 0.0 }) / 2.0
                };

                debug!(
                    "TURN allocation recorded (conversation_id: {}, time: {:.1}ms, success: {})",
                    conversation_id, allocation_time_ms, success
                );
            }
        }
    }

    /// Update WebRTC stats for a connection
    pub fn update_webrtc_stats(
        &self,
        conversation_id: &str,
        webrtc_stats: &HashMap<String, StatsReportType>,
    ) {
        // Only log if verbose logging is enabled
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "update_webrtc_stats called (conversation_id: {}, stats_count: {})",
                conversation_id,
                webrtc_stats.len()
            );
        }
        if let Ok(mut states) = self.connection_states.write() {
            if let Some(state) = states.get_mut(conversation_id) {
                let now = Instant::now();

                // Throttle WebRTC updates (expensive operation)
                if now.duration_since(state.last_webrtc_update) < Duration::from_millis(500) {
                    return;
                }
                state.last_webrtc_update = now;

                // Extract relevant metrics from WebRTC stats
                let mut rtc_stats = RTCStats::default();
                let sctp_stats = SCTPStats::default();

                // Parse WebRTC stats reports for relevant metrics
                let stats_count = webrtc_stats.len();
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "Processing WebRTC stats (conversation_id: {}, stats_count: {})",
                        conversation_id, stats_count
                    );
                }

                for (_id, report) in webrtc_stats.iter() {
                    match report {
                        StatsReportType::InboundRTP(inbound) => {
                            // Update bytes received for throughput calculation
                            if inbound.bytes_received > 0 {
                                rtc_stats.current_bitrate = inbound.bytes_received as f64 * 8.0;
                                // Convert to bits
                            }
                        }
                        StatsReportType::OutboundRTP(outbound) => {
                            // Update bytes sent for throughput calculation
                            if outbound.bytes_sent > 0 && rtc_stats.current_bitrate == 0.0 {
                                rtc_stats.current_bitrate = outbound.bytes_sent as f64 * 8.0;
                                // Convert to bits
                            }
                        }
                        StatsReportType::RemoteInboundRTP(remote_inbound) => {
                            // Use remote inbound stats for packet loss
                            rtc_stats.packet_loss_rate = remote_inbound.fraction_lost;
                            if let Some(rtt) = remote_inbound.round_trip_time {
                                rtc_stats.rtt_ms = Some(rtt * 1000.0); // Convert to milliseconds
                            }
                        }
                        StatsReportType::CandidatePair(pair) => {
                            if pair.nominated {
                                rtc_stats.rtt_ms = Some(pair.current_round_trip_time * 1000.0);

                                // Always set E2E latency from nominated pair (even if RTT is 0)
                                let connection_legs =
                                    &mut state.metrics.webrtc_metrics.connection_legs;
                                connection_legs.end_to_end_latency_ms =
                                    Some(pair.current_round_trip_time * 1000.0);

                                // Parse candidate types from IDs for better display
                                // Candidate IDs contain type info, extract it for readable path
                                let local_type = if pair.local_candidate_id.contains("relay") {
                                    "relay"
                                } else if pair.local_candidate_id.contains("srflx") {
                                    "srflx"
                                } else {
                                    "host"
                                };

                                let remote_type = if pair.remote_candidate_id.contains("relay") {
                                    "relay"
                                } else if pair.remote_candidate_id.contains("srflx") {
                                    "srflx"
                                } else {
                                    "host"
                                };

                                // Update ICE candidate pair stats with parsed types
                                rtc_stats.ice_stats.selected_candidate_pair =
                                    Some(crate::metrics::types::CandidatePairStats {
                                        local_candidate_type: local_type.to_string(),
                                        remote_candidate_type: remote_type.to_string(),
                                        current_rtt_ms: pair.current_round_trip_time * 1000.0,
                                        total_rtt_measurements: pair.responses_received,
                                        connection_time_ms: None, // Not available in this stat
                                        bytes_sent: pair.bytes_sent,
                                        bytes_received: pair.bytes_received,
                                        transport_protocol: "UDP".to_string(), // Default assumption
                                    });

                                // If using relay, use TURN allocation latency for Gateway↔KRelay
                                if local_type == "relay" {
                                    // Gateway is using TURN - use TURN allocation latency
                                    connection_legs.krelay_to_gateway_latency_ms =
                                        connection_legs.turn_allocation_latency_ms;
                                }

                                debug!("Updated selected candidate pair stats (conversation_id: {}, local: {}, remote: {}, rtt: {:.1}ms)",
                                    conversation_id,
                                    local_type,
                                    remote_type,
                                    pair.current_round_trip_time * 1000.0
                                );
                            }
                        }
                        StatsReportType::LocalCandidate(local_candidate) => {
                            // Track candidate gathering progress
                            rtc_stats.ice_stats.total_candidates += 1;

                            // Use string representation for candidate type classification
                            let candidate_type_str =
                                format!("{:?}", local_candidate.candidate_type);
                            if candidate_type_str.contains("Host") {
                                rtc_stats.ice_stats.host_candidates += 1;
                            } else if candidate_type_str.contains("ServerReflexive") {
                                rtc_stats.ice_stats.srflx_candidates += 1;
                            } else if candidate_type_str.contains("Relay") {
                                rtc_stats.ice_stats.relay_candidates += 1;
                            }

                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("ICE candidate gathered (conversation_id: {}, candidate_type: {:?})",
                                    conversation_id, local_candidate.candidate_type);
                            }
                        }
                        StatsReportType::RemoteCandidate(_remote_candidate) => {
                            // Track remote candidate information for connection analysis
                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!(
                                    "Remote ICE candidate processed (conversation_id: {})",
                                    conversation_id
                                );
                            }
                        }
                        StatsReportType::DataChannel(data_channel) => {
                            // Track data channel message counts and bytes - record directly to avoid deadlock
                            if data_channel.messages_sent > 0 || data_channel.bytes_sent > 0 {
                                debug!("Recording data channel sent activity (conversation_id: {}, messages_sent: {}, bytes_sent: {})", conversation_id, data_channel.messages_sent, data_channel.bytes_sent);
                                state.atomic_counters.increment_messages_sent();
                                state
                                    .atomic_counters
                                    .add_bytes_sent(data_channel.bytes_sent as u64);
                                state.throughput_window.add(data_channel.bytes_sent as f64);
                            }
                            if data_channel.messages_received > 0 || data_channel.bytes_received > 0
                            {
                                debug!("Recording data channel received activity (conversation_id: {}, messages_received: {}, bytes_received: {})", conversation_id, data_channel.messages_received, data_channel.bytes_received);
                                state.atomic_counters.increment_messages_received();
                                state
                                    .atomic_counters
                                    .add_bytes_received(data_channel.bytes_received as u64);
                                state
                                    .throughput_window
                                    .add(data_channel.bytes_received as f64);
                            }
                        }
                        StatsReportType::Transport(transport) => {
                            // Transport stats for data channel metrics - record directly without recursive calls
                            if transport.bytes_sent > 0 || transport.bytes_received > 0 {
                                debug!("Recording transport-level data activity (conversation_id: {}, bytes_sent: {}, bytes_received: {})", conversation_id, transport.bytes_sent, transport.bytes_received);

                                // Record transport-level data transfer directly (avoid recursive lock)
                                if transport.bytes_sent > 0 {
                                    state.atomic_counters.increment_messages_sent();
                                    state
                                        .atomic_counters
                                        .add_bytes_sent(transport.bytes_sent as u64);
                                    state.throughput_window.add(transport.bytes_sent as f64);
                                }
                                if transport.bytes_received > 0 {
                                    state.atomic_counters.increment_messages_received();
                                    state
                                        .atomic_counters
                                        .add_bytes_received(transport.bytes_received as u64);
                                    state.throughput_window.add(transport.bytes_received as f64);
                                }
                            } else if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("Transport stats available but no data transferred yet (conversation_id: {})", conversation_id);
                            }
                        }
                        _ => {
                            // Log unhandled stat types for debugging (reduced verbosity)
                            // Most of these are ICE candidates, certificates, etc. that don't contain metrics we need
                        }
                    }
                }

                // Add RTT data to sliding window if available
                if let Some(rtt_ms) = rtc_stats.rtt_ms {
                    state.rtt_window.add(rtt_ms);
                    debug!(
                        "Added RTT to sliding window (conversation_id: {}, rtt_ms: {})",
                        conversation_id, rtt_ms
                    );
                } else if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "No RTT data found in WebRTC stats (conversation_id: {})",
                        conversation_id
                    );
                }

                // Update metrics
                state.metrics.webrtc_metrics = WebRTCMetrics {
                    rtc_stats,
                    sctp_stats,
                    connection_legs: state.metrics.webrtc_metrics.connection_legs.clone(),
                    collected_at: Utc::now(),
                };

                // Update atomic counters snapshot
                let (msgs_sent, msgs_received, bytes_sent, bytes_received, errors, retries) =
                    state.atomic_counters.snapshot();

                state.metrics.messages_sent = msgs_sent;
                state.metrics.messages_received = msgs_received;
                state.metrics.total_bytes_sent = bytes_sent;
                state.metrics.total_bytes_received = bytes_received;
                state.metrics.total_errors = errors;
                state.metrics.retry_count = retries;

                // Update sliding window averages
                state.metrics.avg_rtt =
                    Duration::from_millis(state.rtt_window.one_second.average() as u64);
                state.metrics.p95_latency =
                    Duration::from_millis(state.latency_window.one_second.p95() as u64);
                state.metrics.p99_latency =
                    Duration::from_millis(state.latency_window.one_second.p99() as u64);

                // Update throughput metrics from sliding window
                state.metrics.avg_throughput = state.throughput_window.one_second.average();
                // Peak throughput is the maximum value in the 1-second window
                state.metrics.peak_throughput = state.throughput_window.one_second.max();

                // Calculate error rate
                state.metrics.error_rate = state.error_window.one_second.rate();

                // Update connection quality
                state.metrics.update_quality();

                // Check for alerts periodically
                if now.duration_since(state.last_alert_check) > Duration::from_secs(30) {
                    self.alert_manager.check_metrics(&state.metrics);
                    state.last_alert_check = now;
                }

                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "Updated WebRTC stats (conversation_id: {})",
                        conversation_id
                    );
                }
            }
        }
    }

    /// Get live stats for a specific connection
    #[allow(dead_code)] // Used by Python bindings
    pub fn get_live_stats(&self, conversation_id: &str) -> Option<ConnectionMetrics> {
        if let Ok(states) = self.connection_states.read() {
            states
                .get(conversation_id)
                .map(|state| state.metrics.clone())
        } else {
            None
        }
    }

    /// Get connection health summary
    #[allow(dead_code)] // Used by Python bindings
    pub fn get_connection_health(&self, tube_id: &str) -> Option<ConnectionMetrics> {
        if let Ok(states) = self.connection_states.read() {
            states
                .values()
                .find(|state| state.metrics.tube_id == tube_id)
                .map(|state| state.metrics.clone())
        } else {
            None
        }
    }

    /// Export current metrics as JSON
    #[allow(dead_code)] // Used by Python bindings
    pub fn export_metrics_json(&self) -> Result<String, serde_json::Error> {
        let snapshot = self.create_snapshot();
        serde_json::to_string_pretty(&snapshot)
    }

    /// Create a metrics snapshot
    #[allow(dead_code)] // Used by Python bindings
    pub fn create_snapshot(&self) -> MetricsSnapshot {
        let mut snapshot = MetricsSnapshot::new();

        // Collect per-connection metrics
        if let Ok(states) = self.connection_states.read() {
            for (conversation_id, state) in states.iter() {
                snapshot
                    .connections
                    .insert(conversation_id.clone(), state.metrics.clone());
            }
        }

        // Get aggregated metrics
        if let Ok(aggregated) = self.aggregated_metrics.read() {
            snapshot.aggregated = aggregated.clone();
        }

        // Get active alerts
        snapshot.alerts = self.alert_manager.get_active_alerts();

        snapshot
    }

    /// Get aggregated system metrics
    #[allow(dead_code)] // Used by Python bindings
    pub fn get_aggregated_metrics(&self) -> AggregatedMetrics {
        if let Ok(metrics) = self.aggregated_metrics.read() {
            metrics.clone()
        } else {
            AggregatedMetrics::default()
        }
    }

    /// Background task: Aggregate metrics from all connections
    async fn aggregate_metrics(&self) {
        let mut aggregated = AggregatedMetrics::default();

        if let Ok(states) = self.connection_states.read() {
            aggregated.active_connections = states.len() as u32;

            let mut tube_ids = std::collections::HashSet::new();
            let mut total_rtt_ms = 0.0;
            let mut total_packet_loss = 0.0;
            let mut total_p95_latency_ms = 0.0;
            let mut total_p99_latency_ms = 0.0;
            let mut total_throughput = 0.0;
            let mut total_bandwidth = 0.0;
            let mut rtt_count = 0;
            let mut latency_count = 0;

            for state in states.values() {
                tube_ids.insert(state.metrics.tube_id.clone());

                // RTT aggregation from sliding window average
                let avg_rtt_ms = state.metrics.avg_rtt.as_millis() as f64;
                if avg_rtt_ms > 0.0 {
                    total_rtt_ms += avg_rtt_ms;
                    rtt_count += 1;
                }

                // Latency aggregation
                let p95_latency_ms = state.metrics.p95_latency.as_millis() as f64;
                let p99_latency_ms = state.metrics.p99_latency.as_millis() as f64;
                if p95_latency_ms > 0.0 || p99_latency_ms > 0.0 {
                    total_p95_latency_ms += p95_latency_ms;
                    total_p99_latency_ms += p99_latency_ms;
                    latency_count += 1;
                }

                // Packet loss aggregation
                total_packet_loss += state.metrics.webrtc_metrics.rtc_stats.packet_loss_rate;

                // Throughput aggregation
                total_throughput += state.metrics.message_throughput();
                total_bandwidth += state.metrics.bandwidth_utilization();

                // Quality distribution
                match state.metrics.connection_quality {
                    crate::metrics::ConnectionQuality::Excellent => {
                        aggregated.excellent_connections += 1
                    }
                    crate::metrics::ConnectionQuality::Good => aggregated.good_connections += 1,
                    crate::metrics::ConnectionQuality::Fair => aggregated.fair_connections += 1,
                    crate::metrics::ConnectionQuality::Poor => aggregated.poor_connections += 1,
                }
            }

            aggregated.active_tubes = tube_ids.len() as u32;

            if rtt_count > 0 {
                aggregated.avg_system_rtt =
                    Duration::from_millis((total_rtt_ms / rtt_count as f64) as u64);
            }

            if latency_count > 0 {
                aggregated.avg_p95_latency =
                    Duration::from_millis((total_p95_latency_ms / latency_count as f64) as u64);
                aggregated.avg_p99_latency =
                    Duration::from_millis((total_p99_latency_ms / latency_count as f64) as u64);
            }

            if aggregated.active_connections > 0 {
                aggregated.avg_packet_loss =
                    total_packet_loss / aggregated.active_connections as f64;
            }

            aggregated.total_message_throughput = total_throughput;
            aggregated.total_bandwidth = total_bandwidth;
        }

        // Get alert statistics
        let (total_alerts, critical_alerts, warning_alerts) = self.alert_manager.get_alert_stats();
        aggregated.total_alerts = total_alerts;
        aggregated.critical_alerts = critical_alerts;
        aggregated.warning_alerts = warning_alerts;

        // Update aggregated metrics
        if let Ok(mut metrics) = self.aggregated_metrics.write() {
            *metrics = aggregated.clone();
        }

        // Heartbeat logging at TRACE level to show periodic stats
        // Format explanation:
        // - Connections: Active WebRTC connections being tracked
        // - Tubes: Active communication channels (tubes)
        // - Avg RTT: Average Round-Trip Time for network requests
        // - Packet Loss: Percentage of lost packets (lower is better)
        // - P95 Latency: 95th percentile message processing time
        // - Throughput: Total data transfer rate in KB/s
        // - Quality: Connection quality distribution (Excellent/Good/Fair/Poor)
        // - Alerts: Number of active performance alerts

        // Skip heartbeat logging when idle (0 connections/tubes) unless first time transitioning to idle
        let is_idle = aggregated.active_connections == 0 && aggregated.active_tubes == 0;

        if unlikely!(crate::logger::is_verbose_logging()) {
            if is_idle {
                // Log once when transitioning to idle, then silence
                if !self
                    .idle_state_logged
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    debug!(
                        "Metrics Heartbeat - System now IDLE (Connections: 0, Tubes: 0) - heartbeat logging paused until activity resumes"
                    );
                }
                // Skip logging on subsequent idle heartbeats
            } else {
                // Reset idle flag when activity resumes
                self.idle_state_logged
                    .store(false, std::sync::atomic::Ordering::Relaxed);

                // Log normal heartbeat
                debug!(
                    "Metrics Heartbeat - Connections: {}, Tubes: {}, Avg RTT: {:.1}ms, Packet Loss: {:.2}%, P95 Latency: {:.1}ms, Throughput: {:.1}KB/s, Quality: {}/{}/{}/{}, Alerts: {}",
                    aggregated.active_connections,
                    aggregated.active_tubes,
                    aggregated.avg_system_rtt.as_millis() as f64,
                    aggregated.avg_packet_loss * 100.0,
                    aggregated.avg_p95_latency.as_millis() as f64,
                    aggregated.total_bandwidth / 1024.0, // Convert to KB/s
                    aggregated.excellent_connections,
                    aggregated.good_connections,
                    aggregated.fair_connections,
                    aggregated.poor_connections,
                    aggregated.total_alerts
                );
            }
        }

        // Only log metrics update when not idle (avoids spam when system has no activity)
        if unlikely!(crate::logger::is_verbose_logging()) && !is_idle {
            debug!("Aggregated metrics updated");
        }
    }

    /// Background task: Collect WebRTC stats from all registered connections
    async fn collect_webrtc_stats(&self) {
        let mut stats_collected = 0;

        // Collect connection info without holding the lock across await points
        let connections: Vec<(String, String)> = {
            if let Ok(states) = self.connection_states.read() {
                states
                    .iter()
                    .map(|(conversation_id, state)| {
                        (conversation_id.clone(), state.metrics.tube_id.clone())
                    })
                    .collect()
            } else {
                Vec::new()
            }
        };

        // Early exit if no connections to process
        if connections.is_empty() {
            return;
        }

        // Now process each connection without holding any locks
        for (conversation_id, tube_id) in connections {
            // Double-check that the connection is still registered before processing
            let is_still_registered = {
                if let Ok(states) = self.connection_states.read() {
                    states.contains_key(&conversation_id)
                } else {
                    false
                }
            };

            if !is_still_registered {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Skipping stats collection for unregistered connection (conversation_id: {}, tube_id: {})", conversation_id, tube_id);
                }
                continue;
            }

            // LOCK-FREE: Get tube from DashMap-based registry (no locks!)
            let tube_arc = crate::tube_registry::REGISTRY.get_by_tube_id(&tube_id);

            if let Some(tube) = tube_arc {
                // Now we can safely await without holding the registry lock

                // MULTI-FACTOR ZOMBIE TUBE DETECTION
                // Factor 1: Check if tube has active channels (classic check)
                let has_active_channels = tube.has_active_channels().await;

                // Factor 2: Check connection state (Failed/Disconnected)
                let connection_state = tube.get_connection_state().await;
                let is_failed_or_disconnected = matches!(
                    connection_state,
                    Some(webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed)
                        | Some(webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected)
                );

                // Factor 3: Check inactivity (no data for 60s)
                let inactive_60s = tube.is_inactive_for_duration(Duration::from_secs(60)).await;

                // PATH 1: Classic cleanup (no channels + terminal state)
                if !has_active_channels {
                    // Tube has no active channels - check if it's stale
                    if tube.is_stale().await {
                        debug!(
                            "Tube has no active channels and is stale, auto-unregistering from metrics (conversation_id: {}, tube_id: {})",
                            conversation_id, tube_id
                        );
                        self.unregister_connection(&conversation_id);
                        continue;
                    }
                }

                // PATH 2: ZOMBIE CLEANUP - Failed/Disconnected + inactive for 60s
                // This catches tubes where data channel on_close event didn't fire (browser force-close, network drop)
                if is_failed_or_disconnected && inactive_60s {
                    warn!(
                        "ZOMBIE TUBE DETECTED: Connection in failed/disconnected state with 60s inactivity - force closing (conversation_id: {}, tube_id: {}, state: {:?}, has_channels: {})",
                        conversation_id, tube_id, connection_state, has_active_channels
                    );

                    // Force close the zombie tube via registry (actor-based, no locks!)
                    let tube_id_clone = tube_id.clone();
                    let conversation_id_clone = conversation_id.clone();

                    match crate::tube_registry::REGISTRY
                        .close_tube(
                            &tube_id_clone,
                            Some(crate::tube_protocol::CloseConnectionReason::Timeout),
                        )
                        .await
                    {
                        Ok(_) => {
                            info!(
                                "Successfully force-closed zombie tube (tube_id: {})",
                                tube_id_clone
                            );
                        }
                        Err(e) => {
                            error!(
                                "Failed to force-close zombie tube: {} (tube_id: {})",
                                e, tube_id_clone
                            );
                        }
                    }

                    // Unregister from metrics regardless of close outcome
                    self.unregister_connection(&conversation_id_clone);
                    continue;
                }

                // Collect stats if tube is still active
                match tube.get_connection_stats().await {
                    Ok(_) => {
                        stats_collected += 1;
                        // Only log if verbose logging is enabled
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            debug!(
                                "Collected WebRTC stats (conversation_id: {}, tube_id: {})",
                                conversation_id, tube_id
                            );
                        }
                    }
                    Err(e) => {
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            debug!(
                                "Failed to collect WebRTC stats (conversation_id: {}, tube_id: {}, error: {})",
                                conversation_id, tube_id, e
                            );
                        }
                    }
                }
            } else {
                // Tube not found in registry, remove it from our metrics tracking
                debug!(
                    "Tube not found in registry, auto-unregistering from metrics (conversation_id: {}, tube_id: {})",
                    conversation_id, tube_id
                );
                self.unregister_connection(&conversation_id);
            }
        }

        // Only log if verbose logging is enabled
        if stats_collected > 0 && unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "WebRTC stats collection completed (stats_collected: {})",
                stats_collected
            );
        }
    }

    /// Background task: Clean up old metrics data
    async fn cleanup_old_data(&self) {
        // Clean up old alerts
        self.alert_manager.cleanup_old_alerts();

        // Trigger stale tube cleanup in the registry (lock-free!)
        let cleaned_tubes = crate::tube_registry::REGISTRY.cleanup_stale_tubes().await;
        let cleaned_count = cleaned_tubes.len();

        if cleaned_count > 0 {
            debug!(
                "Cleaned up {} stale tubes during periodic maintenance",
                cleaned_count
            );
        }

        debug!("Metrics cleanup completed");
    }

    /// Get system uptime
    #[allow(dead_code)] // Used by Python bindings
    pub fn get_uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get number of active connections being tracked
    #[allow(dead_code)] // Used by Python bindings
    pub fn active_connection_count(&self) -> usize {
        if let Ok(states) = self.connection_states.read() {
            states.len()
        } else {
            0
        }
    }

    /// Clear all connections from metrics tracking (for testing purposes)
    #[allow(dead_code)] // Used by Python bindings
    pub fn clear_all_connections(&self) {
        if let Ok(mut states) = self.connection_states.write() {
            let count = states.len();
            states.clear();
            debug!("Cleared all {} connections from metrics tracking", count);
        }

        // Also clear aggregated metrics
        if let Ok(mut aggregated) = self.aggregated_metrics.write() {
            *aggregated = AggregatedMetrics::default();
            debug!("Cleared aggregated metrics");
        }
    }

    /// Background task to sweep for zombie tubes every 5 minutes
    /// This is a safety net for tubes that slip through metrics-based detection
    /// Zombies are tubes in Failed/Disconnected state with prolonged inactivity
    async fn zombie_tube_sweeper(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(300)).await; // 5 minutes

            debug!("Zombie tube sweeper: Starting periodic sweep");

            // Get all tube IDs from registry
            // LOCK-FREE: Get all tube IDs from DashMap
            let tube_ids = crate::tube_registry::REGISTRY.all_tube_ids_sync();

            let mut zombies_found = 0;

            for tube_id in tube_ids {
                // LOCK-FREE: Get tube from DashMap
                let tube_arc = crate::tube_registry::REGISTRY.get_by_tube_id(&tube_id);

                if let Some(tube) = tube_arc {
                    let state = tube.get_connection_state().await;
                    let inactive_5min = tube
                        .is_inactive_for_duration(Duration::from_secs(300))
                        .await;

                    // Zombie criteria: Failed/Disconnected + 5min inactivity
                    if matches!(
                        state,
                        Some(
                            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                        ) | Some(
                            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected
                        )
                    ) && inactive_5min
                    {
                        zombies_found += 1;
                        warn!(
                            "Zombie sweeper: Found zombie tube (tube_id: {}, state: {:?}, inactive: 5min+)",
                            tube_id, state
                        );

                        // Force close the zombie tube (actor-based, no locks!)
                        match crate::tube_registry::REGISTRY
                            .close_tube(
                                &tube_id,
                                Some(crate::tube_protocol::CloseConnectionReason::Timeout),
                            )
                            .await
                        {
                            Ok(_) => {
                                info!(
                                    "Zombie sweeper: Closed zombie tube (tube_id: {})",
                                    tube_id
                                );
                            }
                            Err(e) => {
                                error!(
                                    "Zombie sweeper: Failed to close tube: {} (tube_id: {})",
                                    e, tube_id
                                );
                            }
                        }
                    }
                }
            }

            if zombies_found > 0 {
                debug!("Zombie sweeper: Cleaned {} zombie tubes", zombies_found);
            } else {
                debug!("Zombie sweeper: No zombies found");
            }
        }
    }
}
