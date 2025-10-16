use crate::unlikely;
use crate::webrtc_errors::WebRTCResult;
use log::{debug, info, warn};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tokio::time::interval;

/// Network quality metrics for adaptive optimization
#[derive(Debug, Clone, Copy)]
pub struct NetworkQualityMetrics {
    /// Round-trip time in milliseconds
    pub rtt_ms: f64,
    /// Packet loss rate (0.0 to 1.0)
    pub packet_loss_rate: f64,
    /// Jitter in milliseconds
    pub jitter_ms: f64,
    /// Estimated bandwidth in bits per second
    pub bandwidth_bps: u64,
    /// Current congestion level
    pub congestion_level: CongestionLevel,
    /// Quality score (0-100, higher is better)
    pub quality_score: u8,
    /// Timestamp when metrics were collected
    pub timestamp: Instant,
}

impl Default for NetworkQualityMetrics {
    fn default() -> Self {
        Self {
            rtt_ms: 50.0,
            packet_loss_rate: 0.0,
            jitter_ms: 5.0,
            bandwidth_bps: 1_000_000, // 1 Mbps default
            congestion_level: CongestionLevel::Low,
            quality_score: 80,
            timestamp: Instant::now(),
        }
    }
}

/// Congestion level classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionLevel {
    Low,
    Moderate,
    High,
    Severe,
}

impl CongestionLevel {
    pub fn from_metrics(rtt_ms: f64, packet_loss_rate: f64, jitter_ms: f64) -> Self {
        let rtt_score = if rtt_ms > 300.0 {
            3
        } else if rtt_ms > 150.0 {
            2
        } else if rtt_ms > 75.0 {
            1
        } else {
            0
        };

        let loss_score = if packet_loss_rate > 0.05 {
            3
        } else if packet_loss_rate > 0.02 {
            2
        } else if packet_loss_rate > 0.01 {
            1
        } else {
            0
        };

        let jitter_score = if jitter_ms > 50.0 {
            3
        } else if jitter_ms > 20.0 {
            2
        } else if jitter_ms > 10.0 {
            1
        } else {
            0
        };

        let total_score = rtt_score + loss_score + jitter_score;
        match total_score {
            0..=2 => CongestionLevel::Low,
            3..=4 => CongestionLevel::Moderate,
            5..=7 => CongestionLevel::High,
            _ => CongestionLevel::Severe,
        }
    }

    pub fn bitrate_multiplier(&self) -> f64 {
        match self {
            CongestionLevel::Low => 1.0,
            CongestionLevel::Moderate => 0.8,
            CongestionLevel::High => 0.6,
            CongestionLevel::Severe => 0.4,
        }
    }
}

/// WebRTC statistics snapshot
#[derive(Debug, Clone)]
pub struct WebRTCStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub rtt_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub bitrate_bps: Option<u64>,
    pub timestamp: Instant,
}

impl Default for WebRTCStats {
    fn default() -> Self {
        Self {
            bytes_sent: 0,
            bytes_received: 0,
            packets_sent: 0,
            packets_received: 0,
            packets_lost: 0,
            rtt_ms: None,
            jitter_ms: None,
            bitrate_bps: None,
            timestamp: Instant::now(),
        }
    }
}

/// Bandwidth estimation using transport-cc style algorithms
#[derive(Debug)]
pub struct BandwidthEstimator {
    /// Recent throughput measurements
    throughput_history: VecDeque<ThroughputSample>,
    /// Current bandwidth estimate in bps
    current_estimate_bps: AtomicU64,
    /// Minimum estimate to prevent too aggressive reduction
    min_estimate_bps: u64,
    /// Maximum estimate to prevent excessive usage
    max_estimate_bps: u64,
    /// Estimation window size
    window_size: Duration,
    /// Last update timestamp
    last_update: Mutex<Instant>,
}

#[derive(Debug, Clone)]
struct ThroughputSample {
    timestamp: Instant,
    bytes_transferred: u64,
    rtt_ms: f64,
    packet_loss: f64,
}

impl BandwidthEstimator {
    pub fn new(initial_estimate_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        Self {
            throughput_history: VecDeque::with_capacity(100),
            current_estimate_bps: AtomicU64::new(initial_estimate_bps),
            min_estimate_bps: min_bps,
            max_estimate_bps: max_bps,
            window_size: Duration::from_secs(5),
            last_update: Mutex::new(Instant::now()),
        }
    }

    /// Update bandwidth estimate based on new measurements
    pub fn update_estimate(&mut self, stats: &WebRTCStats, metrics: &NetworkQualityMetrics) {
        let now = Instant::now();
        let mut last_update = self.last_update.lock().unwrap();

        if now.duration_since(*last_update) < Duration::from_millis(100) {
            return; // Too frequent updates
        }

        // Add new sample
        let sample = ThroughputSample {
            timestamp: now,
            bytes_transferred: stats.bytes_sent + stats.bytes_received,
            rtt_ms: metrics.rtt_ms,
            packet_loss: metrics.packet_loss_rate,
        };

        self.throughput_history.push_back(sample);

        // Remove old samples outside the window
        let cutoff_time = now - self.window_size;
        while let Some(front) = self.throughput_history.front() {
            if front.timestamp < cutoff_time {
                self.throughput_history.pop_front();
            } else {
                break;
            }
        }

        // Calculate new estimate
        let new_estimate = self.calculate_bandwidth_estimate();
        self.current_estimate_bps
            .store(new_estimate, Ordering::Relaxed);
        *last_update = now;

        // Only log bandwidth updates if verbose logging is enabled
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Bandwidth estimate updated: {} bps (samples: {})",
                new_estimate,
                self.throughput_history.len()
            );
        }
    }

    fn calculate_bandwidth_estimate(&self) -> u64 {
        if self.throughput_history.len() < 2 {
            return self.current_estimate_bps.load(Ordering::Relaxed);
        }

        // Calculate throughput-based estimate
        let total_bytes: u64 = self
            .throughput_history
            .iter()
            .map(|s| s.bytes_transferred)
            .sum();

        let time_span = self.throughput_history.back().unwrap().timestamp
            - self.throughput_history.front().unwrap().timestamp;

        let throughput_estimate = if time_span.as_secs_f64() > 0.0 {
            (total_bytes as f64 * 8.0 / time_span.as_secs_f64()) as u64
        } else {
            self.current_estimate_bps.load(Ordering::Relaxed)
        };

        // Apply congestion-based adjustment
        let avg_loss: f64 = self
            .throughput_history
            .iter()
            .map(|s| s.packet_loss)
            .sum::<f64>()
            / self.throughput_history.len() as f64;

        let avg_rtt: f64 = self
            .throughput_history
            .iter()
            .filter_map(|s| if s.rtt_ms > 0.0 { Some(s.rtt_ms) } else { None })
            .sum::<f64>()
            / self
                .throughput_history
                .iter()
                .filter(|s| s.rtt_ms > 0.0)
                .count()
                .max(1) as f64;

        let congestion_level = CongestionLevel::from_metrics(avg_rtt, avg_loss, 0.0);
        let adjusted_estimate =
            (throughput_estimate as f64 * congestion_level.bitrate_multiplier()) as u64;

        // Clamp to bounds
        adjusted_estimate
            .max(self.min_estimate_bps)
            .min(self.max_estimate_bps)
    }

    pub fn get_estimate_bps(&self) -> u64 {
        self.current_estimate_bps.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn get_estimate_mbps(&self) -> f64 {
        self.get_estimate_bps() as f64 / 1_000_000.0
    }
}

/// Dynamic bitrate controller
#[derive(Debug)]
pub struct BitrateController {
    /// Current target bitrate
    current_bitrate_bps: AtomicU64,
    /// Minimum allowed bitrate
    min_bitrate_bps: u64,
    /// Maximum allowed bitrate
    max_bitrate_bps: u64,
    /// Adjustment step size (percentage)
    #[allow(dead_code)]
    adjustment_step: f64,
    /// Last adjustment timestamp
    last_adjustment: Mutex<Instant>,
    /// Adjustment history for smoothing
    adjustment_history: Mutex<VecDeque<BitrateAdjustment>>,
}

#[derive(Debug, Clone)]
pub struct BitrateAdjustment {
    #[allow(dead_code)]
    timestamp: Instant,
    #[allow(dead_code)]
    old_bitrate: u64,
    #[allow(dead_code)]
    new_bitrate: u64,
    #[allow(dead_code)]
    reason: String,
}

impl BitrateController {
    pub fn new(initial_bitrate_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        Self {
            current_bitrate_bps: AtomicU64::new(initial_bitrate_bps),
            min_bitrate_bps: min_bps,
            max_bitrate_bps: max_bps,
            adjustment_step: 0.1, // 10% adjustment steps
            last_adjustment: Mutex::new(Instant::now()),
            adjustment_history: Mutex::new(VecDeque::with_capacity(50)),
        }
    }

    /// Adjust bitrate based on network conditions
    pub fn adjust_bitrate(
        &self,
        metrics: &NetworkQualityMetrics,
        bandwidth_estimate_bps: u64,
    ) -> bool {
        let now = Instant::now();
        let mut last_adjustment = self.last_adjustment.lock().unwrap();

        // Rate limit adjustments
        if now.duration_since(*last_adjustment) < Duration::from_millis(500) {
            return false;
        }

        let current_bitrate = self.current_bitrate_bps.load(Ordering::Relaxed);
        let target_bitrate = self.calculate_target_bitrate(metrics, bandwidth_estimate_bps);

        if (target_bitrate as f64 - current_bitrate as f64).abs() / (current_bitrate as f64) < 0.05
        {
            return false; // Less than 5% change, not worth adjusting
        }

        // Apply adjustment
        let new_bitrate = target_bitrate
            .max(self.min_bitrate_bps)
            .min(self.max_bitrate_bps);
        self.current_bitrate_bps
            .store(new_bitrate, Ordering::Relaxed);

        // Record adjustment
        let adjustment = BitrateAdjustment {
            timestamp: now,
            old_bitrate: current_bitrate,
            new_bitrate,
            reason: format!(
                "Quality: {}, Congestion: {:?}",
                metrics.quality_score, metrics.congestion_level
            ),
        };

        let mut history = self.adjustment_history.lock().unwrap();
        history.push_back(adjustment);
        if history.len() > 50 {
            history.pop_front();
        }

        *last_adjustment = now;

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Bitrate adjusted: {} -> {} bps ({})",
                current_bitrate,
                new_bitrate,
                if new_bitrate > current_bitrate {
                    "increased"
                } else {
                    "decreased"
                }
            );
        }

        true
    }

    fn calculate_target_bitrate(
        &self,
        metrics: &NetworkQualityMetrics,
        bandwidth_estimate_bps: u64,
    ) -> u64 {
        // Use 80% of estimated bandwidth as target to leave headroom
        let bandwidth_target = (bandwidth_estimate_bps as f64 * 0.8) as u64;

        // Adjust based on congestion level
        let congestion_adjusted =
            (bandwidth_target as f64 * metrics.congestion_level.bitrate_multiplier()) as u64;

        // Apply quality-based adjustment
        let quality_factor = (metrics.quality_score as f64 / 100.0).clamp(0.3, 1.0);
        (congestion_adjusted as f64 * quality_factor) as u64
    }

    pub fn get_current_bitrate_bps(&self) -> u64 {
        self.current_bitrate_bps.load(Ordering::Relaxed)
    }

    pub fn get_current_bitrate_kbps(&self) -> f64 {
        self.get_current_bitrate_bps() as f64 / 1000.0
    }

    #[allow(dead_code)]
    pub fn get_adjustment_history(&self) -> Vec<BitrateAdjustment> {
        self.adjustment_history.lock().unwrap().clone().into()
    }
}

/// Advanced congestion detector
#[derive(Debug)]
pub struct CongestionDetector {
    /// RTT trend analysis
    rtt_samples: Mutex<VecDeque<f64>>,
    /// Packet loss trend
    loss_samples: Mutex<VecDeque<f64>>,
    /// Jitter trend
    jitter_samples: Mutex<VecDeque<f64>>,
    /// Sample window size
    window_size: usize,
    /// Detection sensitivity (0.0 to 1.0)
    sensitivity: f64,
}

impl CongestionDetector {
    pub fn new(window_size: usize, sensitivity: f64) -> Self {
        Self {
            rtt_samples: Mutex::new(VecDeque::with_capacity(window_size)),
            loss_samples: Mutex::new(VecDeque::with_capacity(window_size)),
            jitter_samples: Mutex::new(VecDeque::with_capacity(window_size)),
            window_size,
            sensitivity: sensitivity.clamp(0.0, 1.0),
        }
    }

    /// Update congestion detection with new metrics
    pub fn update(&self, metrics: &NetworkQualityMetrics) {
        let mut rtt_samples = self.rtt_samples.lock().unwrap();
        let mut loss_samples = self.loss_samples.lock().unwrap();
        let mut jitter_samples = self.jitter_samples.lock().unwrap();

        // Add new samples
        rtt_samples.push_back(metrics.rtt_ms);
        loss_samples.push_back(metrics.packet_loss_rate);
        jitter_samples.push_back(metrics.jitter_ms);

        // Maintain window size
        if rtt_samples.len() > self.window_size {
            rtt_samples.pop_front();
        }
        if loss_samples.len() > self.window_size {
            loss_samples.pop_front();
        }
        if jitter_samples.len() > self.window_size {
            jitter_samples.pop_front();
        }
    }

    /// Detect congestion based on trend analysis
    pub fn detect_congestion(&self) -> CongestionLevel {
        let rtt_samples = self.rtt_samples.lock().unwrap();
        let loss_samples = self.loss_samples.lock().unwrap();
        let jitter_samples = self.jitter_samples.lock().unwrap();

        if rtt_samples.len() < 3 || loss_samples.len() < 3 || jitter_samples.len() < 3 {
            return CongestionLevel::Low; // Not enough data
        }

        let rtt_trend = self.calculate_trend(&rtt_samples);
        let loss_trend = self.calculate_trend(&loss_samples);
        let jitter_trend = self.calculate_trend(&jitter_samples);

        // Recent averages
        let recent_rtt = rtt_samples.iter().rev().take(5).sum::<f64>() / 5.0;
        let recent_loss = loss_samples.iter().rev().take(5).sum::<f64>() / 5.0;
        let recent_jitter = jitter_samples.iter().rev().take(5).sum::<f64>() / 5.0;

        // Base congestion level from current values
        let base_level = CongestionLevel::from_metrics(recent_rtt, recent_loss, recent_jitter);

        // Trend-based adjustment
        let trend_score = (rtt_trend + loss_trend + jitter_trend) * self.sensitivity;

        match base_level {
            CongestionLevel::Low if trend_score > 0.5 => CongestionLevel::Moderate,
            CongestionLevel::Moderate if trend_score > 0.3 => CongestionLevel::High,
            CongestionLevel::High if trend_score > 0.2 => CongestionLevel::Severe,
            level => level,
        }
    }

    /// Calculate trend (positive = increasing, negative = decreasing)
    fn calculate_trend(&self, samples: &VecDeque<f64>) -> f64 {
        if samples.len() < 3 {
            return 0.0;
        }

        let n = samples.len() as f64;
        let x_sum = (1..=samples.len()).sum::<usize>() as f64;
        let y_sum: f64 = samples.iter().sum();
        let xy_sum: f64 = samples
            .iter()
            .enumerate()
            .map(|(i, &y)| (i + 1) as f64 * y)
            .sum();
        let x_squared_sum: f64 = (1..=samples.len()).map(|i| (i * i) as f64).sum();

        // Linear regression slope
        let numerator = n * xy_sum - x_sum * y_sum;
        let denominator = n * x_squared_sum - x_sum * x_sum;

        if denominator.abs() < f64::EPSILON {
            0.0
        } else {
            numerator / denominator
        }
    }
}

/// Quality of Service (QoS) manager for traffic prioritization
#[derive(Debug)]
pub struct QoSManager {
    /// Channel priorities (higher number = higher priority)
    channel_priorities: RwLock<HashMap<String, u8>>,
    /// Traffic shaping rules
    shaping_rules: RwLock<Vec<TrafficShapingRule>>,
    /// QoS statistics
    qos_stats: Arc<QoSStatistics>,
}

#[derive(Debug, Clone)]
pub struct TrafficShapingRule {
    pub channel_pattern: String,
    pub max_bitrate_bps: Option<u64>,
    #[allow(dead_code)]
    pub priority: u8,
    #[allow(dead_code)]
    pub burst_allowance_bytes: u64,
}

#[derive(Debug)]
pub struct QoSStatistics {
    pub total_bytes_shaped: AtomicU64,
    pub priority_adjustments: AtomicUsize,
    pub burst_allowances_used: AtomicUsize,
}

impl Clone for QoSStatistics {
    fn clone(&self) -> Self {
        Self {
            total_bytes_shaped: AtomicU64::new(self.total_bytes_shaped.load(Ordering::Relaxed)),
            priority_adjustments: AtomicUsize::new(
                self.priority_adjustments.load(Ordering::Relaxed),
            ),
            burst_allowances_used: AtomicUsize::new(
                self.burst_allowances_used.load(Ordering::Relaxed),
            ),
        }
    }
}

impl QoSManager {
    pub fn new() -> Self {
        let mut manager = Self {
            channel_priorities: RwLock::new(HashMap::new()),
            shaping_rules: RwLock::new(Vec::new()),
            qos_stats: Arc::new(QoSStatistics {
                total_bytes_shaped: AtomicU64::new(0),
                priority_adjustments: AtomicUsize::new(0),
                burst_allowances_used: AtomicUsize::new(0),
            }),
        };

        // Set default priorities
        manager.set_default_priorities();
        manager
    }

    fn set_default_priorities(&mut self) {
        let mut priorities = self.channel_priorities.write().unwrap();
        priorities.insert("control".to_string(), 10); // Highest priority for control
        priorities.insert("data".to_string(), 5); // Medium priority for data
        priorities.insert("video".to_string(), 3); // Lower priority for video
        priorities.insert("audio".to_string(), 8); // High priority for audio
    }

    /// Set priority for a channel
    pub async fn set_channel_priority(&self, channel: String, priority: u8) {
        let mut priorities = self.channel_priorities.write().unwrap();
        priorities.insert(channel.clone(), priority);
        self.qos_stats
            .priority_adjustments
            .fetch_add(1, Ordering::Relaxed);
        debug!("Set priority for channel '{}': {}", channel, priority);
    }

    /// Get priority for a channel
    pub fn get_channel_priority(&self, channel: &str) -> u8 {
        let priorities = self.channel_priorities.read().unwrap();
        priorities.get(channel).copied().unwrap_or(1) // Default priority: 1
    }

    /// Add traffic shaping rule
    #[allow(dead_code)]
    pub async fn add_shaping_rule(&self, rule: TrafficShapingRule) {
        let mut rules = self.shaping_rules.write().unwrap();
        rules.push(rule.clone());
        debug!(
            "Added traffic shaping rule for pattern: {}",
            rule.channel_pattern
        );
    }

    /// Apply QoS to data transmission
    pub fn apply_qos(&self, channel: &str, data_size: usize) -> QoSDecision {
        let priority = self.get_channel_priority(channel);
        let rules = self.shaping_rules.read().unwrap();

        // Find matching rule
        let matching_rule = rules
            .iter()
            .find(|rule| channel.contains(&rule.channel_pattern));

        let decision = if let Some(rule) = matching_rule {
            QoSDecision {
                allowed: true,
                priority,
                delay_ms: 0,
                max_bitrate_bps: rule.max_bitrate_bps,
                use_burst: data_size > 1024, // Use burst for larger packets
            }
        } else {
            QoSDecision {
                allowed: true,
                priority,
                delay_ms: 0,
                max_bitrate_bps: None,
                use_burst: false,
            }
        };

        self.qos_stats
            .total_bytes_shaped
            .fetch_add(data_size as u64, Ordering::Relaxed);
        if decision.use_burst {
            self.qos_stats
                .burst_allowances_used
                .fetch_add(1, Ordering::Relaxed);
        }

        decision
    }

    pub fn get_statistics(&self) -> QoSStatistics {
        QoSStatistics {
            total_bytes_shaped: AtomicU64::new(
                self.qos_stats.total_bytes_shaped.load(Ordering::Relaxed),
            ),
            priority_adjustments: AtomicUsize::new(
                self.qos_stats.priority_adjustments.load(Ordering::Relaxed),
            ),
            burst_allowances_used: AtomicUsize::new(
                self.qos_stats.burst_allowances_used.load(Ordering::Relaxed),
            ),
        }
    }
}

impl Default for QoSManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct QoSDecision {
    pub allowed: bool,
    pub priority: u8,
    pub delay_ms: u64,
    pub max_bitrate_bps: Option<u64>,
    pub use_burst: bool,
}

/// Main adaptive quality manager coordinating all components
pub struct AdaptiveQualityManager {
    tube_id: String,
    bandwidth_estimator: Arc<Mutex<BandwidthEstimator>>,
    bitrate_controller: Arc<BitrateController>,
    congestion_detector: Arc<CongestionDetector>,
    qos_manager: Arc<QoSManager>,
    current_metrics: Arc<TokioRwLock<NetworkQualityMetrics>>,
    stats_history: Arc<Mutex<VecDeque<WebRTCStats>>>,
    monitoring_active: Arc<Mutex<bool>>,
    config: QualityManagerConfig,
}

#[derive(Debug, Clone)]
pub struct QualityManagerConfig {
    pub stats_collection_interval: Duration,
    pub max_history_samples: usize,
    pub min_bitrate_bps: u64,
    pub max_bitrate_bps: u64,
    pub target_quality_score: u8,
    pub congestion_sensitivity: f64,
}

impl Default for QualityManagerConfig {
    fn default() -> Self {
        Self {
            stats_collection_interval: Duration::from_millis(1000),
            max_history_samples: 300,     // 5 minutes at 1Hz
            min_bitrate_bps: 64_000,      // 64 Kbps minimum
            max_bitrate_bps: 500_000_000, // 500 Mbps maximum (realistic for gigabit networks)
            target_quality_score: 80,
            congestion_sensitivity: 0.7,
        }
    }
}

impl AdaptiveQualityManager {
    pub fn new(tube_id: String, config: QualityManagerConfig) -> Self {
        let initial_bitrate = (config.min_bitrate_bps + config.max_bitrate_bps) / 2;

        Self {
            tube_id: tube_id.clone(),
            bandwidth_estimator: Arc::new(Mutex::new(BandwidthEstimator::new(
                initial_bitrate,
                config.min_bitrate_bps,
                config.max_bitrate_bps,
            ))),
            bitrate_controller: Arc::new(BitrateController::new(
                initial_bitrate,
                config.min_bitrate_bps,
                config.max_bitrate_bps,
            )),
            congestion_detector: Arc::new(CongestionDetector::new(
                50,
                config.congestion_sensitivity,
            )),
            qos_manager: Arc::new(QoSManager::new()),
            current_metrics: Arc::new(TokioRwLock::new(NetworkQualityMetrics::default())),
            stats_history: Arc::new(Mutex::new(VecDeque::with_capacity(
                config.max_history_samples,
            ))),
            monitoring_active: Arc::new(Mutex::new(false)),
            config,
        }
    }

    /// Start adaptive quality monitoring
    pub async fn start_monitoring(&self) -> WebRTCResult<()> {
        {
            let mut active = self.monitoring_active.lock().unwrap();
            if *active {
                return Ok(()); // Already monitoring
            }
            *active = true;
        }

        debug!(
            "Starting adaptive quality monitoring for tube {}",
            self.tube_id
        );

        // Start background monitoring task
        let manager = self.clone();
        tokio::spawn(async move {
            manager.monitoring_loop().await;
        });

        Ok(())
    }

    /// Stop quality monitoring
    pub fn stop_monitoring(&self) {
        let mut active = self.monitoring_active.lock().unwrap();
        *active = false;
        info!(
            "Stopped adaptive quality monitoring for tube {}",
            self.tube_id
        );
    }

    /// Update with new WebRTC statistics
    pub async fn update_stats(&self, stats: WebRTCStats) -> WebRTCResult<()> {
        // Add to history
        {
            let mut history = self.stats_history.lock().unwrap();
            history.push_back(stats.clone());
            if history.len() > self.config.max_history_samples {
                history.pop_front();
            }
        }

        // Calculate current metrics
        let metrics = self.calculate_current_metrics(&stats).await;

        // Update components
        {
            let mut bandwidth_estimator = self.bandwidth_estimator.lock().unwrap();
            bandwidth_estimator.update_estimate(&stats, &metrics);
        }

        self.congestion_detector.update(&metrics);

        let bandwidth_estimate = {
            let bandwidth_estimator = self.bandwidth_estimator.lock().unwrap();
            bandwidth_estimator.get_estimate_bps()
        };

        let adjusted_bitrate = self
            .bitrate_controller
            .adjust_bitrate(&metrics, bandwidth_estimate);

        // Update current metrics
        {
            let mut current = self.current_metrics.write().await;
            *current = metrics;
        }

        // Only log quality adjustments if verbose logging is enabled
        if adjusted_bitrate && unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Quality adjustment for tube {}: bandwidth={} Mbps, bitrate={} kbps",
                self.tube_id,
                bandwidth_estimate as f64 / 1_000_000.0,
                self.bitrate_controller.get_current_bitrate_kbps()
            );
        }

        Ok(())
    }

    async fn calculate_current_metrics(&self, stats: &WebRTCStats) -> NetworkQualityMetrics {
        let mut metrics = NetworkQualityMetrics {
            timestamp: stats.timestamp,
            ..Default::default()
        };

        // Use provided stats if available
        if let Some(rtt) = stats.rtt_ms {
            metrics.rtt_ms = rtt;
        }
        if let Some(jitter) = stats.jitter_ms {
            metrics.jitter_ms = jitter;
        }
        if let Some(bitrate) = stats.bitrate_bps {
            metrics.bandwidth_bps = bitrate;
        }

        // Calculate packet loss rate
        if stats.packets_sent > 0 {
            metrics.packet_loss_rate = stats.packets_lost as f64 / stats.packets_sent as f64;
        }

        // Determine congestion level
        metrics.congestion_level = self.congestion_detector.detect_congestion();

        // Calculate quality score (0-100)
        metrics.quality_score = self.calculate_quality_score(&metrics);

        metrics
    }

    fn calculate_quality_score(&self, metrics: &NetworkQualityMetrics) -> u8 {
        let rtt_score = if metrics.rtt_ms < 50.0 {
            100
        } else if metrics.rtt_ms < 100.0 {
            80
        } else if metrics.rtt_ms < 200.0 {
            60
        } else if metrics.rtt_ms < 400.0 {
            40
        } else {
            20
        };

        let loss_score = if metrics.packet_loss_rate < 0.001 {
            100
        } else if metrics.packet_loss_rate < 0.01 {
            80
        } else if metrics.packet_loss_rate < 0.02 {
            60
        } else if metrics.packet_loss_rate < 0.05 {
            40
        } else {
            20
        };

        let jitter_score = if metrics.jitter_ms < 5.0 {
            100
        } else if metrics.jitter_ms < 10.0 {
            80
        } else if metrics.jitter_ms < 20.0 {
            60
        } else if metrics.jitter_ms < 50.0 {
            40
        } else {
            20
        };

        // Weighted average (RTT is most important for real-time applications)
        ((rtt_score * 50 + loss_score * 30 + jitter_score * 20) / 100).clamp(0, 100) as u8
    }

    async fn monitoring_loop(&self) {
        let mut interval = interval(self.config.stats_collection_interval);

        while {
            let active = self.monitoring_active.lock().unwrap();
            *active
        } {
            interval.tick().await;

            // Perform periodic quality assessment and adjustment
            if let Err(e) = self.perform_quality_assessment().await {
                warn!(
                    "Quality assessment failed for tube {}: {:?}",
                    self.tube_id, e
                );
            }
        }

        debug!("Quality monitoring loop stopped for tube {}", self.tube_id);
    }

    async fn perform_quality_assessment(&self) -> WebRTCResult<()> {
        let current_metrics = {
            let metrics = self.current_metrics.read().await;
            *metrics
        };

        // Check if quality is below target
        if current_metrics.quality_score < self.config.target_quality_score {
            warn!(
                "Quality below target for tube {}: {} < {}",
                self.tube_id, current_metrics.quality_score, self.config.target_quality_score
            );

            // Trigger quality improvement actions
            self.trigger_quality_improvement(&current_metrics).await?;
        }

        Ok(())
    }

    async fn trigger_quality_improvement(
        &self,
        metrics: &NetworkQualityMetrics,
    ) -> WebRTCResult<()> {
        match metrics.congestion_level {
            CongestionLevel::High | CongestionLevel::Severe => {
                // Reduce bitrate aggressively
                let current_bitrate = self.bitrate_controller.get_current_bitrate_bps();
                let reduced_bitrate = (current_bitrate as f64 * 0.7) as u64;

                info!(
                    "Triggering quality improvement for tube {}: reducing bitrate {} -> {} bps",
                    self.tube_id, current_bitrate, reduced_bitrate
                );
            }
            CongestionLevel::Moderate => {
                // Moderate reduction
                info!(
                    "Moderate congestion detected for tube {}, applying gentle bitrate reduction",
                    self.tube_id
                );
            }
            CongestionLevel::Low => {
                // Quality issues might be due to other factors
                debug!(
                    "Quality issues without congestion for tube {}, investigating",
                    self.tube_id
                );
            }
        }

        Ok(())
    }

    /// Get current quality metrics
    pub async fn get_current_metrics(&self) -> NetworkQualityMetrics {
        let metrics = self.current_metrics.read().await;
        *metrics
    }

    /// Get current bitrate recommendation
    pub fn get_recommended_bitrate_bps(&self) -> u64 {
        self.bitrate_controller.get_current_bitrate_bps()
    }

    /// Get bandwidth estimate
    pub fn get_bandwidth_estimate_bps(&self) -> u64 {
        let estimator = self.bandwidth_estimator.lock().unwrap();
        estimator.get_estimate_bps()
    }

    /// Apply QoS to data transmission
    pub fn apply_qos(&self, channel: &str, data_size: usize) -> QoSDecision {
        self.qos_manager.apply_qos(channel, data_size)
    }

    /// Set channel priority for QoS
    pub async fn set_channel_priority(&self, channel: String, priority: u8) {
        self.qos_manager
            .set_channel_priority(channel, priority)
            .await;
    }

    /// Get quality management statistics
    pub fn get_statistics(&self) -> QualityManagerStatistics {
        let bandwidth_estimate = {
            let estimator = self.bandwidth_estimator.lock().unwrap();
            estimator.get_estimate_bps()
        };

        let history_size = {
            let history = self.stats_history.lock().unwrap();
            history.len()
        };

        QualityManagerStatistics {
            tube_id: self.tube_id.clone(),
            bandwidth_estimate_bps: bandwidth_estimate,
            current_bitrate_bps: self.bitrate_controller.get_current_bitrate_bps(),
            stats_samples: history_size,
            qos_stats: self.qos_manager.get_statistics(),
        }
    }
}

impl Clone for AdaptiveQualityManager {
    fn clone(&self) -> Self {
        Self {
            tube_id: self.tube_id.clone(),
            bandwidth_estimator: Arc::clone(&self.bandwidth_estimator),
            bitrate_controller: Arc::clone(&self.bitrate_controller),
            congestion_detector: Arc::clone(&self.congestion_detector),
            qos_manager: Arc::clone(&self.qos_manager),
            current_metrics: Arc::clone(&self.current_metrics),
            stats_history: Arc::clone(&self.stats_history),
            monitoring_active: Arc::clone(&self.monitoring_active),
            config: self.config.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct QualityManagerStatistics {
    pub tube_id: String,
    pub bandwidth_estimate_bps: u64,
    pub current_bitrate_bps: u64,
    pub stats_samples: usize,
    pub qos_stats: QoSStatistics,
}
