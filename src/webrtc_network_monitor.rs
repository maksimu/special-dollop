use crate::webrtc_errors::WebRTCResult;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};

/// Network interface information
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkInterface {
    pub name: String,
    pub interface_type: InterfaceType,
    pub is_active: bool,
    pub ip_address: Option<String>,
    pub last_seen: Instant,
    /// Network quality metrics for this interface
    pub quality_metrics: NetworkQualityMetrics,
    /// Interface preference score (higher = more preferred)
    pub preference_score: u8,
}

/// Network quality metrics for interface assessment
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkQualityMetrics {
    /// Average latency in milliseconds
    pub avg_latency_ms: f64,
    /// Packet loss rate (0.0 to 1.0)
    pub packet_loss_rate: f64,
    /// Bandwidth estimate in bps
    pub bandwidth_bps: u64,
    /// Signal strength (0-100, higher is better)
    pub signal_strength: u8,
    /// Quality score (0-100, higher is better)
    pub quality_score: u8,
    /// Last measurement timestamp
    pub last_measured: Instant,
}

impl Default for NetworkQualityMetrics {
    fn default() -> Self {
        Self {
            avg_latency_ms: 50.0,
            packet_loss_rate: 0.0,
            bandwidth_bps: 1_000_000, // 1 Mbps default
            signal_strength: 80,
            quality_score: 80,
            last_measured: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InterfaceType {
    Ethernet,
    WiFi,
    Cellular,
    Loopback,
    Vpn,
    Unknown,
}

/// Network change event types
#[derive(Debug, Clone)]
pub enum NetworkChangeEvent {
    /// New network interface became available
    InterfaceAdded { interface: NetworkInterface },
    /// Network interface was removed
    InterfaceRemoved { interface_name: String },
    /// Network interface status changed (up/down)
    InterfaceStatusChanged {
        interface_name: String,
        was_active: bool,
        now_active: bool,
    },
    /// IP address changed on an interface
    IpAddressChanged {
        interface_name: String,
        old_ip: Option<String>,
        new_ip: Option<String>,
    },
    /// Primary network interface changed
    PrimaryInterfaceChanged {
        old_interface: Option<String>,
        new_interface: String,
    },
    /// Network connectivity lost
    ConnectivityLost,
    /// Network connectivity restored
    ConnectivityRestored,
    /// Network migration completed
    MigrationCompleted {
        from_interface: String,
        to_interface: String,
        success: bool,
        migration_time_ms: u64,
    },
    /// Network quality degradation detected
    QualityDegraded {
        interface_name: String,
        quality_score: u8,
    },
}

/// Predicted network events for proactive handling
#[derive(Debug, Clone)]
pub enum PredictedNetworkEvent {
    /// Quality degradation prediction
    QualityDegradation {
        interface_name: String,
        predicted_score: u8,
        confidence: f64,
    },
    /// Possible disconnection prediction
    PossibleDisconnection {
        interface_name: String,
        probability: f64,
    },
    /// Interface transition likelihood
    InterfaceTransition {
        from_interface: String,
        to_interface: String,
        probability: f64,
    },
}

impl InterfaceType {
    #[allow(dead_code)]
    fn from_name(name: &str) -> Self {
        let name_lower = name.to_lowercase();
        if name_lower.contains("eth") || name_lower.contains("ethernet") {
            InterfaceType::Ethernet
        } else if name_lower.contains("wlan")
            || name_lower.contains("wifi")
            || name_lower.contains("wi-fi")
        {
            InterfaceType::WiFi
        } else if name_lower.contains("cellular")
            || name_lower.contains("mobile")
            || name_lower.contains("wwan")
        {
            InterfaceType::Cellular
        } else if name_lower.contains("lo") || name_lower == "loopback" {
            InterfaceType::Loopback
        } else if name_lower.contains("vpn")
            || name_lower.contains("tun")
            || name_lower.contains("tap")
        {
            InterfaceType::Vpn
        } else {
            InterfaceType::Unknown
        }
    }
}

/// Network change callback function type
pub type NetworkChangeCallback = Box<
    dyn Fn(NetworkChangeEvent) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Network monitor for detecting changes that require ICE restart
pub struct NetworkMonitor {
    /// Current network interfaces
    interfaces: Arc<Mutex<HashMap<String, NetworkInterface>>>,
    /// Network change callbacks
    callbacks: Arc<Mutex<Vec<NetworkChangeCallback>>>,
    /// Monitor configuration
    config: NetworkMonitorConfig,
    /// Last connectivity check result
    last_connectivity_check: Arc<Mutex<Option<bool>>>,
    /// Monitor task handle
    monitoring_active: Arc<Mutex<bool>>,
    /// Connection migration manager
    connection_migrator: Arc<ConnectionMigrator>,
    /// Primary interface tracker
    primary_interface: Arc<Mutex<Option<String>>>,
    /// Quality history for predictive analysis
    quality_history: Arc<Mutex<HashMap<String, Vec<NetworkQualityMetrics>>>>,
}

#[derive(Debug, Clone)]
pub struct NetworkMonitorConfig {
    /// How often to check for network changes
    pub check_interval: Duration,
    /// Timeout for connectivity tests
    pub connectivity_timeout: Duration,
    /// Endpoints to test connectivity against
    pub test_endpoints: Vec<String>,
    /// Whether to monitor IP address changes
    pub monitor_ip_changes: bool,
    /// Whether to monitor interface status changes
    pub monitor_interface_changes: bool,
    /// Delay before triggering change events (debounce)
    pub change_debounce_delay: Duration,
    /// Enable connection migration
    pub enable_migration: bool,
    /// Migration quality threshold (0-100, migrate if current interface drops below this)
    pub migration_quality_threshold: u8,
    /// Maximum migration time before considering it failed
    pub migration_timeout: Duration,
}

impl Default for NetworkMonitorConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            connectivity_timeout: Duration::from_secs(3),
            test_endpoints: vec![
                "8.8.8.8:53".to_string(),        // Google DNS
                "1.1.1.1:53".to_string(),        // Cloudflare DNS
                "208.67.222.222:53".to_string(), // OpenDNS
            ],
            monitor_ip_changes: true,
            monitor_interface_changes: true,
            change_debounce_delay: Duration::from_millis(500),
            enable_migration: true,
            migration_quality_threshold: 60,
            migration_timeout: Duration::from_secs(10),
        }
    }
}

/// Connection migration manager
#[derive(Debug)]
pub struct ConnectionMigrator {
    /// Active migrations in progress
    active_migrations: Arc<Mutex<HashMap<String, MigrationState>>>,
    /// Migration strategies
    migration_strategies: Vec<MigrationStrategy>,
    /// Configuration
    config: NetworkMonitorConfig,
}

/// State of an active migration
#[derive(Debug, Clone)]
pub struct MigrationState {
    pub from_interface: String,
    pub to_interface: String,
    pub start_time: Instant,
    pub phase: MigrationPhase,
    pub attempt_count: u32,
}

/// Migration phases
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationPhase {
    Preparing,
    Testing,
    Migrating,
    Completing,
    Failed(String),
    Completed,
}

/// Migration strategy
#[derive(Debug, Clone)]
pub enum MigrationStrategy {
    /// Make-before-break: establish new connection before closing old
    MakeBeforeBreak,
    /// Break-before-make: close old connection before establishing new
    BreakBeforeMake,
    /// Seamless: attempt to maintain both connections during transition
    Seamless,
}

impl ConnectionMigrator {
    pub fn new(config: NetworkMonitorConfig) -> Self {
        Self {
            active_migrations: Arc::new(Mutex::new(HashMap::new())),
            migration_strategies: vec![
                MigrationStrategy::Seamless,
                MigrationStrategy::MakeBeforeBreak,
                MigrationStrategy::BreakBeforeMake,
            ],
            config,
        }
    }

    /// Start a connection migration from one interface to another
    pub async fn start_migration(
        &self,
        tube_id: &str,
        from_interface: String,
        to_interface: String,
    ) -> WebRTCResult<()> {
        let migration_id = format!("{}:{}→{}", tube_id, from_interface, to_interface);

        info!("Starting connection migration: {}", migration_id);

        let migration_state = MigrationState {
            from_interface: from_interface.clone(),
            to_interface: to_interface.clone(),
            start_time: Instant::now(),
            phase: MigrationPhase::Preparing,
            attempt_count: 1,
        };

        {
            let mut migrations = self.active_migrations.lock().unwrap();
            migrations.insert(migration_id.clone(), migration_state);
        }

        // Execute migration asynchronously
        let migrator = self.clone();
        let migration_id_clone = migration_id.clone();
        tokio::spawn(async move {
            if let Err(e) = migrator.execute_migration(&migration_id_clone).await {
                error!("Migration failed for {}: {:?}", migration_id_clone, e);
                migrator.mark_migration_failed(&migration_id_clone, &format!("{:?}", e));
            }
        });

        Ok(())
    }

    async fn execute_migration(&self, migration_id: &str) -> WebRTCResult<()> {
        // Phase 1: Prepare migration
        self.update_migration_phase(migration_id, MigrationPhase::Preparing);
        self.prepare_migration(migration_id).await?;

        // Phase 2: Test new connection
        self.update_migration_phase(migration_id, MigrationPhase::Testing);
        self.test_new_connection(migration_id).await?;

        // Phase 3: Perform migration
        self.update_migration_phase(migration_id, MigrationPhase::Migrating);
        self.perform_migration(migration_id).await?;

        // Phase 4: Complete migration
        self.update_migration_phase(migration_id, MigrationPhase::Completing);
        self.complete_migration(migration_id).await?;

        self.update_migration_phase(migration_id, MigrationPhase::Completed);
        info!("Migration completed successfully: {}", migration_id);

        Ok(())
    }

    async fn prepare_migration(&self, migration_id: &str) -> WebRTCResult<()> {
        // Get migration state to access target interface
        let (to_interface, from_interface) = {
            let migrations = self.active_migrations.lock().unwrap();
            if let Some(state) = migrations.get(migration_id) {
                (state.to_interface.clone(), state.from_interface.clone())
            } else {
                return Err(crate::WebRTCError::Unknown {
                    tube_id: "".to_string(),
                    reason: format!("Migration {} not found", migration_id),
                });
            }
        };

        // Note: In a real implementation, we would need access to the NetworkMonitor
        // to verify the target interface exists and is active. However, ConnectionMigrator
        // doesn't have a reference to NetworkMonitor to avoid circular dependencies.
        // The caller (NetworkMonitor) should have already validated this before calling start_migration.

        debug!(
            "Migration preparation completed: {} ({}→{})",
            migration_id, from_interface, to_interface
        );
        Ok(())
    }

    async fn test_new_connection(&self, migration_id: &str) -> WebRTCResult<()> {
        // Test connectivity to STUN/TURN servers via new interface
        let test_endpoints = vec![
            "stun.l.google.com:19302",
            "stun1.l.google.com:19302",
            "stun2.l.google.com:19302",
        ];

        debug!("Testing new connection for migration: {}", migration_id);

        for endpoint in &test_endpoints {
            match tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::TcpStream::connect(endpoint),
            )
            .await
            {
                Ok(Ok(_stream)) => {
                    debug!(
                        "New connection tested successfully: {} via {}",
                        migration_id, endpoint
                    );
                    return Ok(());
                }
                Ok(Err(e)) => {
                    debug!(
                        "Connection test failed for {} to {}: {}",
                        migration_id, endpoint, e
                    );
                    continue;
                }
                Err(_) => {
                    debug!(
                        "Connection test timed out for {} to {}",
                        migration_id, endpoint
                    );
                    continue;
                }
            }
        }

        Err(crate::WebRTCError::IceConnectionFailed {
            tube_id: "".to_string(),
            reason: format!(
                "New connection test failed for all endpoints during migration: {}",
                migration_id
            ),
        })
    }

    async fn perform_migration(&self, migration_id: &str) -> WebRTCResult<()> {
        // The actual ICE restart is triggered by the WebRTCNetworkIntegration callbacks
        // when it receives the network change event. This method just coordinates the timing.

        debug!(
            "Connection migration phase: performing handover for {}",
            migration_id
        );

        // Wait for ICE restart to be initiated (this is done via callbacks in the integration layer)
        // Give some time for the network change event to propagate and ICE restart to begin
        sleep(Duration::from_millis(500)).await;

        debug!("Connection migration handover initiated: {}", migration_id);
        Ok(())
    }

    async fn complete_migration(&self, migration_id: &str) -> WebRTCResult<()> {
        // Get migration timing and interfaces for completion event
        let (from_interface, to_interface, migration_time_ms) = {
            let migrations = self.active_migrations.lock().unwrap();
            if let Some(state) = migrations.get(migration_id) {
                let elapsed_ms = state.start_time.elapsed().as_millis() as u64;
                (
                    state.from_interface.clone(),
                    state.to_interface.clone(),
                    elapsed_ms,
                )
            } else {
                return Err(crate::WebRTCError::Unknown {
                    tube_id: "".to_string(),
                    reason: format!("Migration {} not found during completion", migration_id),
                });
            }
        };

        debug!(
            "Migration cleanup completed: {} ({}→{}, took {}ms)",
            migration_id, from_interface, to_interface, migration_time_ms
        );

        // Note: The actual NetworkMonitor update and event triggering should be done
        // by the caller (NetworkMonitor) since ConnectionMigrator doesn't have access to it.
        // This is to avoid circular dependencies.

        Ok(())
    }

    fn update_migration_phase(&self, migration_id: &str, phase: MigrationPhase) {
        let mut migrations = self.active_migrations.lock().unwrap();
        if let Some(migration) = migrations.get_mut(migration_id) {
            migration.phase = phase;
            debug!(
                "Migration {} phase updated to {:?}",
                migration_id, migration.phase
            );
        }
    }

    fn mark_migration_failed(&self, migration_id: &str, reason: &str) {
        let mut migrations = self.active_migrations.lock().unwrap();
        if let Some(migration) = migrations.get_mut(migration_id) {
            migration.phase = MigrationPhase::Failed(reason.to_string());
            warn!("Migration {} failed: {}", migration_id, reason);
        }
    }

    pub fn get_migration_status(&self, migration_id: &str) -> Option<MigrationState> {
        let migrations = self.active_migrations.lock().unwrap();
        migrations.get(migration_id).cloned()
    }

    pub fn cleanup_completed_migrations(&self) {
        let mut migrations = self.active_migrations.lock().unwrap();
        let cutoff_time = Instant::now() - Duration::from_secs(300); // 5 minutes

        migrations.retain(|_, migration| {
            match migration.phase {
                MigrationPhase::Completed | MigrationPhase::Failed(_) => {
                    migration.start_time > cutoff_time
                }
                _ => true, // Keep active migrations
            }
        });
    }
}

impl Clone for ConnectionMigrator {
    fn clone(&self) -> Self {
        Self {
            active_migrations: Arc::clone(&self.active_migrations),
            migration_strategies: self.migration_strategies.clone(),
            config: self.config.clone(),
        }
    }
}

/// Network quality scorer for interface selection
#[derive(Debug)]
pub struct NetworkQualityScorer;

impl NetworkQualityScorer {
    /// Calculate comprehensive network quality score (0-100)
    pub fn calculate_quality_score(interface: &NetworkInterface) -> u8 {
        let metrics = &interface.quality_metrics;

        // Latency score (0-35 points)
        let latency_score = if metrics.avg_latency_ms < 20.0 {
            35
        } else if metrics.avg_latency_ms < 50.0 {
            30
        } else if metrics.avg_latency_ms < 100.0 {
            25
        } else if metrics.avg_latency_ms < 200.0 {
            15
        } else {
            5
        };

        // Packet loss score (0-25 points)
        let loss_score = if metrics.packet_loss_rate < 0.001 {
            25
        } else if metrics.packet_loss_rate < 0.01 {
            20
        } else if metrics.packet_loss_rate < 0.02 {
            15
        } else if metrics.packet_loss_rate < 0.05 {
            10
        } else {
            0
        };

        // Bandwidth score (0-25 points)
        let bandwidth_score = if metrics.bandwidth_bps > 10_000_000 {
            25
        } else if metrics.bandwidth_bps > 5_000_000 {
            20
        } else if metrics.bandwidth_bps > 1_000_000 {
            15
        } else if metrics.bandwidth_bps > 500_000 {
            10
        } else {
            5
        };

        // Signal strength score (0-15 points)
        let signal_score = (metrics.signal_strength as f32 * 0.15) as u8;

        (latency_score + loss_score + bandwidth_score + signal_score).min(100)
    }

    /// Determine if an interface is suitable for migration target
    pub fn is_suitable_for_migration(interface: &NetworkInterface, min_quality: u8) -> bool {
        interface.is_active
            && interface.quality_metrics.quality_score >= min_quality
            && interface.interface_type != InterfaceType::Loopback
    }

    /// Find the best interface for migration
    pub fn find_best_migration_target(
        interfaces: &HashMap<String, NetworkInterface>,
        current_interface: &str,
        min_quality: u8,
    ) -> Option<String> {
        interfaces
            .iter()
            .filter(|(name, interface)| {
                *name != current_interface
                    && Self::is_suitable_for_migration(interface, min_quality)
            })
            .max_by_key(|(_, interface)| {
                (
                    interface.quality_metrics.quality_score,
                    interface.preference_score,
                )
            })
            .map(|(name, _)| name.clone())
    }
}

impl NetworkMonitor {
    pub fn new(config: NetworkMonitorConfig) -> Self {
        let connection_migrator = Arc::new(ConnectionMigrator::new(config.clone()));

        Self {
            interfaces: Arc::new(Mutex::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(Vec::new())),
            connection_migrator,
            primary_interface: Arc::new(Mutex::new(None)),
            quality_history: Arc::new(Mutex::new(HashMap::new())),
            config,
            last_connectivity_check: Arc::new(Mutex::new(None)),
            monitoring_active: Arc::new(Mutex::new(false)),
        }
    }

    /// Start network monitoring
    pub async fn start_monitoring(&self) -> WebRTCResult<()> {
        // Check if already monitoring
        if let Ok(active) = self.monitoring_active.lock() {
            if *active {
                return Ok(());
            }
        }

        // Mark as active
        if let Ok(mut active) = self.monitoring_active.lock() {
            *active = true;
        }

        // SOLUTION: Start background monitoring task but defer initial scan until triggered
        // Network scan will be triggered when WebRTC connection reaches "Connected" state
        debug!("Starting network monitoring background task (initial scan deferred until WebRTC Connected)");

        // Start background monitoring task without initial scan
        let monitor = self.clone_for_task();
        let (error_tx, mut error_rx) = mpsc::unbounded_channel();

        // Spawn background task
        tokio::spawn(async move {
            monitor.monitoring_loop_with_error_reporting(error_tx).await;
        });

        // Spawn error handler task (runs in main context with Python logging)
        tokio::spawn(async move {
            while let Some(error_msg) = error_rx.recv().await {
                error!("Network monitoring: {}", error_msg);
            }
        });

        debug!("Network monitoring started");
        Ok(())
    }

    /// Stop network monitoring
    pub fn stop_monitoring(&self) {
        if let Ok(mut active) = self.monitoring_active.lock() {
            *active = false;
        }
        debug!("Network monitoring stopped");
    }

    /// Trigger initial network scan (called when WebRTC connection is established)
    pub async fn trigger_initial_scan(&self) -> WebRTCResult<()> {
        debug!("Triggering initial network scan (WebRTC connection established)");
        self.scan_network_interfaces().await
    }

    /// Register callback for network change events
    pub fn register_callback<F>(&self, callback: F)
    where
        F: Fn(
                NetworkChangeEvent,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        if let Ok(mut callbacks) = self.callbacks.lock() {
            callbacks.push(Box::new(callback));
            debug!("Registered network change callback");
        }
    }

    /// Get current network interfaces
    pub fn get_current_interfaces(&self) -> HashMap<String, NetworkInterface> {
        if let Ok(interfaces) = self.interfaces.lock() {
            interfaces.clone()
        } else {
            HashMap::new()
        }
    }

    /// Handle network transition (connection migration)
    pub async fn handle_network_transition(
        &self,
        tube_id: &str,
        old_interface: &str,
        new_interface: &str,
    ) -> WebRTCResult<()> {
        info!(
            "Handling network transition for tube {} from {} to {}",
            tube_id, old_interface, new_interface
        );

        if self.config.enable_migration {
            self.connection_migrator
                .start_migration(
                    tube_id,
                    old_interface.to_string(),
                    new_interface.to_string(),
                )
                .await?;
        } else {
            info!("Connection migration disabled, triggering standard ICE restart");
        }

        Ok(())
    }

    /// Assess comprehensive network quality for an interface
    pub async fn assess_network_quality(&self, interface_name: &str) -> Option<u8> {
        let interfaces = self.interfaces.lock().unwrap();
        if let Some(interface) = interfaces.get(interface_name) {
            let quality_score = NetworkQualityScorer::calculate_quality_score(interface);
            Some(quality_score)
        } else {
            None
        }
    }

    /// Predict potential network changes based on quality trends
    pub async fn predict_network_changes(&self) -> Vec<PredictedNetworkEvent> {
        let mut predictions = Vec::new();

        let quality_history = self.quality_history.lock().unwrap();
        let interfaces = self.interfaces.lock().unwrap();

        for (interface_name, interface) in interfaces.iter() {
            if let Some(history) = quality_history.get(interface_name) {
                if history.len() >= 5 {
                    // Analyze trend over last 5 measurements
                    let recent_scores: Vec<u8> = history
                        .iter()
                        .rev()
                        .take(5)
                        .map(|m| m.quality_score)
                        .collect();

                    // Simple trend detection
                    if let (Some(&latest), Some(&oldest)) =
                        (recent_scores.first(), recent_scores.last())
                    {
                        let trend = latest as i16 - oldest as i16;

                        if trend < -15 {
                            predictions.push(PredictedNetworkEvent::QualityDegradation {
                                interface_name: interface_name.clone(),
                                predicted_score: latest.saturating_sub(10),
                                confidence: 0.8,
                            });
                        } else if latest < self.config.migration_quality_threshold
                            && interface.is_active
                        {
                            predictions.push(PredictedNetworkEvent::PossibleDisconnection {
                                interface_name: interface_name.clone(),
                                probability: (self.config.migration_quality_threshold - latest)
                                    as f64
                                    / 100.0,
                            });
                        }
                    }
                }
            }
        }

        predictions
    }

    /// Update quality metrics for an interface
    pub async fn update_interface_quality(
        &self,
        interface_name: &str,
        metrics: NetworkQualityMetrics,
    ) {
        // Update interface quality
        {
            let mut interfaces = self.interfaces.lock().unwrap();
            if let Some(interface) = interfaces.get_mut(interface_name) {
                interface.quality_metrics = metrics.clone();
                interface.quality_metrics.quality_score =
                    NetworkQualityScorer::calculate_quality_score(interface);
            }
        }

        // Add to quality history
        {
            let mut history = self.quality_history.lock().unwrap();
            let interface_history = history.entry(interface_name.to_string()).or_default();
            interface_history.push(metrics.clone());

            // Keep only last 100 measurements per interface
            if interface_history.len() > 100 {
                interface_history.remove(0);
            }
        }

        // Check if quality degradation triggers migration
        if self.config.enable_migration
            && metrics.quality_score < self.config.migration_quality_threshold
        {
            if let Some(current_primary) = self.get_primary_interface().await {
                if current_primary == interface_name {
                    info!(
                        "Primary interface {} quality degraded to {}, looking for migration target",
                        interface_name, metrics.quality_score
                    );

                    if let Some(_target) = self.find_best_migration_target(&current_primary).await {
                        self.trigger_event(NetworkChangeEvent::QualityDegraded {
                            interface_name: interface_name.to_string(),
                            quality_score: metrics.quality_score,
                        })
                        .await;
                    }
                }
            }
        }
    }

    /// Get current primary interface
    pub async fn get_primary_interface(&self) -> Option<String> {
        let primary = self.primary_interface.lock().unwrap();
        primary.clone()
    }

    /// Set primary interface
    pub async fn set_primary_interface(&self, interface_name: String) {
        let (old_interface, should_trigger) = {
            let mut primary = self.primary_interface.lock().unwrap();
            let old_interface = primary.clone();
            *primary = Some(interface_name.clone());
            let should_trigger = old_interface.as_ref() != Some(&interface_name);
            (old_interface, should_trigger)
        };

        if should_trigger {
            info!(
                "Primary interface changed: {:?} -> {}",
                old_interface, interface_name
            );
            self.trigger_event(NetworkChangeEvent::PrimaryInterfaceChanged {
                old_interface,
                new_interface: interface_name,
            })
            .await;
        }
    }

    /// Find best migration target for current primary interface
    async fn find_best_migration_target(&self, current_interface: &str) -> Option<String> {
        let interfaces = self.interfaces.lock().unwrap();
        NetworkQualityScorer::find_best_migration_target(
            &interfaces,
            current_interface,
            self.config.migration_quality_threshold,
        )
    }

    /// Get migration status for a tube
    pub fn get_migration_status(
        &self,
        tube_id: &str,
        from_interface: &str,
        to_interface: &str,
    ) -> Option<MigrationState> {
        let migration_id = format!("{}:{}→{}", tube_id, from_interface, to_interface);
        self.connection_migrator.get_migration_status(&migration_id)
    }

    /// Check if network connectivity is available
    pub async fn check_connectivity(&self) -> bool {
        for endpoint in &self.config.test_endpoints {
            if self.test_endpoint_connectivity(endpoint).await {
                debug!("Connectivity check passed (endpoint: {})", endpoint);
                return true;
            }
        }

        warn!("Connectivity check failed for all endpoints");
        false
    }

    /// Clone for background task (avoids self-referential issues)
    fn clone_for_task(&self) -> NetworkMonitorForTask {
        NetworkMonitorForTask {
            interfaces: self.interfaces.clone(),
            callbacks: self.callbacks.clone(),
            config: self.config.clone(),
            last_connectivity_check: self.last_connectivity_check.clone(),
            monitoring_active: self.monitoring_active.clone(),
        }
    }

    /// Scan current network interfaces
    async fn scan_network_interfaces(&self) -> WebRTCResult<()> {
        debug!("Scanning network interfaces...");

        // Get system network interfaces (simplified implementation)
        let current_interfaces = self.get_system_interfaces().await?;

        // Compare with stored interfaces and detect changes
        let events = {
            let stored_interfaces = self.interfaces.lock().unwrap();
            let mut events = Vec::new();

            // Check for changes in existing interfaces
            for (name, new_interface) in &current_interfaces {
                if let Some(old_interface) = stored_interfaces.get(name) {
                    // Check for status changes
                    if old_interface.is_active != new_interface.is_active
                        && self.config.monitor_interface_changes
                    {
                        events.push(NetworkChangeEvent::InterfaceStatusChanged {
                            interface_name: name.clone(),
                            was_active: old_interface.is_active,
                            now_active: new_interface.is_active,
                        });
                    }

                    // Check for IP address changes
                    if old_interface.ip_address != new_interface.ip_address
                        && self.config.monitor_ip_changes
                    {
                        events.push(NetworkChangeEvent::IpAddressChanged {
                            interface_name: name.clone(),
                            old_ip: old_interface.ip_address.clone(),
                            new_ip: new_interface.ip_address.clone(),
                        });
                    }
                } else if self.config.monitor_interface_changes {
                    // New interface detected
                    events.push(NetworkChangeEvent::InterfaceAdded {
                        interface: new_interface.clone(),
                    });
                }
            }

            // Check for removed interfaces
            let removed_interfaces: Vec<String> = stored_interfaces
                .keys()
                .filter(|name| !current_interfaces.contains_key(*name))
                .cloned()
                .collect();

            for name in removed_interfaces {
                events.push(NetworkChangeEvent::InterfaceRemoved {
                    interface_name: name,
                });
            }

            events
        }; // Lock released here

        // Trigger events without holding lock
        for event in events {
            self.trigger_event(event).await;
        }

        // Update stored interfaces
        {
            let mut stored_interfaces = self.interfaces.lock().unwrap();
            *stored_interfaces = current_interfaces;
        }

        Ok(())
    }

    /// Get system network interfaces using cross-platform interface enumeration
    async fn get_system_interfaces(&self) -> WebRTCResult<HashMap<String, NetworkInterface>> {
        let mut interfaces = HashMap::new();
        let now = Instant::now();

        // Get all network interfaces from the system
        match if_addrs::get_if_addrs() {
            Ok(addrs) => {
                for iface in addrs {
                    // Skip if we've already seen this interface name
                    if interfaces.contains_key(&iface.name) {
                        continue;
                    }

                    // Determine interface type based on name patterns
                    let interface_type = Self::classify_interface_type(&iface.name);

                    // Skip loopback interfaces (we handle them specially below)
                    if interface_type == InterfaceType::Loopback {
                        continue;
                    }

                    // Get IP address
                    let ip_address = Some(iface.addr.ip().to_string());

                    // Assign preference scores based on interface type
                    let preference_score = match interface_type {
                        InterfaceType::Ethernet => 90,
                        InterfaceType::WiFi => 70,
                        InterfaceType::Cellular => 50,
                        InterfaceType::Vpn => 30,
                        InterfaceType::Loopback => 10,
                        InterfaceType::Unknown => 40,
                    };

                    interfaces.insert(
                        iface.name.clone(),
                        NetworkInterface {
                            name: iface.name,
                            interface_type,
                            is_active: true,
                            ip_address,
                            last_seen: now,
                            quality_metrics: NetworkQualityMetrics::default(),
                            preference_score,
                        },
                    );
                }
            }
            Err(e) => {
                warn!("Failed to enumerate network interfaces: {}", e);
            }
        }

        debug!("Detected {} network interfaces", interfaces.len());
        Ok(interfaces)
    }

    /// Classify interface type based on name patterns
    fn classify_interface_type(name: &str) -> InterfaceType {
        let lower_name = name.to_lowercase();

        // Loopback
        if lower_name == "lo" || lower_name == "lo0" || lower_name.starts_with("loopback") {
            return InterfaceType::Loopback;
        }

        // Ethernet
        if lower_name.starts_with("eth")
            || lower_name.starts_with("en")
            || lower_name.starts_with("eno")
            || lower_name.starts_with("ens")
            || lower_name.starts_with("enp")
            || lower_name.contains("ethernet")
        {
            return InterfaceType::Ethernet;
        }

        // WiFi
        if lower_name.starts_with("wlan")
            || lower_name.starts_with("wlp")
            || lower_name.starts_with("wifi")
            || lower_name.starts_with("wi-fi")
            || lower_name.contains("wireless")
            || lower_name.contains("wi-fi")
        {
            return InterfaceType::WiFi;
        }

        // Cellular
        if lower_name.starts_with("wwan")
            || lower_name.starts_with("wwp")
            || lower_name.contains("cellular")
            || lower_name.contains("mobile")
            || lower_name.contains("lte")
            || lower_name.contains("5g")
        {
            return InterfaceType::Cellular;
        }

        // VPN
        if lower_name.starts_with("tun")
            || lower_name.starts_with("tap")
            || lower_name.starts_with("vpn")
            || lower_name.contains("utun")
            || lower_name.contains("ipsec")
            || lower_name.contains("wireguard")
            || lower_name.contains("openvpn")
        {
            return InterfaceType::Vpn;
        }

        InterfaceType::Unknown
    }

    /// Test connectivity to a specific endpoint
    async fn test_endpoint_connectivity(&self, endpoint: &str) -> bool {
        match tokio::time::timeout(
            self.config.connectivity_timeout,
            tokio::net::TcpStream::connect(endpoint),
        )
        .await
        {
            Ok(Ok(_)) => {
                debug!("Connectivity test passed for {}", endpoint);
                true
            }
            Ok(Err(_)) | Err(_) => {
                debug!("Connectivity test failed for {}", endpoint);
                false
            }
        }
    }

    /// Trigger a network change event
    async fn trigger_event(&self, event: NetworkChangeEvent) {
        debug!("Network change detected: {:?}", event);

        // Add debounce delay
        sleep(self.config.change_debounce_delay).await;

        // Call all registered callbacks
        if let Ok(callbacks) = self.callbacks.lock() {
            for callback in callbacks.iter() {
                let future = callback(event.clone());
                tokio::spawn(future);
            }
        }
    }
}

/// Helper struct for background monitoring task
struct NetworkMonitorForTask {
    #[allow(dead_code)]
    interfaces: Arc<Mutex<HashMap<String, NetworkInterface>>>,
    callbacks: Arc<Mutex<Vec<NetworkChangeCallback>>>,
    config: NetworkMonitorConfig,
    last_connectivity_check: Arc<Mutex<Option<bool>>>,
    monitoring_active: Arc<Mutex<bool>>,
}

impl NetworkMonitorForTask {
    /// Main monitoring loop with error reporting
    async fn monitoring_loop_with_error_reporting(&self, error_tx: mpsc::UnboundedSender<String>) {
        let mut interval = interval(self.config.check_interval);

        loop {
            interval.tick().await;

            // Check if monitoring is still active
            let is_active = {
                if let Ok(active) = self.monitoring_active.lock() {
                    *active
                } else {
                    false
                }
            };

            if !is_active {
                // debug!("Network monitoring loop terminated");
                break;
            }

            // Perform network interface scan
            if let Err(e) = self.scan_network_interfaces().await {
                // Send error to main thread for proper logging
                let _ = error_tx.send(format!("Network interface scan failed: {}", e));
                continue;
            }

            // Check connectivity
            let connectivity_ok = self.check_connectivity().await;

            // Compare with last check
            let last_connectivity = {
                if let Ok(mut last) = self.last_connectivity_check.lock() {
                    let previous = *last;
                    *last = Some(connectivity_ok);
                    previous
                } else {
                    None
                }
            };

            // Trigger connectivity events if changed
            if let Some(last) = last_connectivity {
                if last != connectivity_ok {
                    let event = if connectivity_ok {
                        NetworkChangeEvent::ConnectivityRestored
                    } else {
                        NetworkChangeEvent::ConnectivityLost
                    };
                    self.trigger_event(event).await;
                }
            }
        }
    }

    /// Scan network interfaces (delegated implementation)
    async fn scan_network_interfaces(&self) -> WebRTCResult<()> {
        // Simplified scan - in a real implementation this would be more comprehensive
        // debug!("Background network interface scan");
        Ok(())
    }

    /// Check connectivity (delegated implementation)
    async fn check_connectivity(&self) -> bool {
        for endpoint in &self.config.test_endpoints {
            if self.test_endpoint_connectivity(endpoint).await {
                return true;
            }
        }
        false
    }

    /// Test endpoint connectivity (delegated implementation)
    async fn test_endpoint_connectivity(&self, endpoint: &str) -> bool {
        match tokio::time::timeout(
            self.config.connectivity_timeout,
            tokio::net::TcpStream::connect(endpoint),
        )
        .await
        {
            Ok(Ok(_)) => {
                // debug!("Background task: Connectivity test passed for {}", endpoint);
                true
            }
            Ok(Err(_)) | Err(_) => {
                // debug!("Background task: Connectivity test failed for {}", endpoint);
                false
            }
        }
    }

    /// Trigger network change event (delegated implementation)
    async fn trigger_event(&self, event: NetworkChangeEvent) {
        // debug!("Network change detected in background task: {:?}", event);

        sleep(self.config.change_debounce_delay).await;

        if let Ok(callbacks) = self.callbacks.lock() {
            for callback in callbacks.iter() {
                let future = callback(event.clone());
                tokio::spawn(future);
            }
        }
    }
}

/// Type alias for tube callback function
type TubeCallback = Box<dyn Fn() + Send + Sync>;

/// Integration helper for WebRTC connections
pub struct WebRTCNetworkIntegration {
    monitor: Arc<NetworkMonitor>,
    tube_callbacks: Arc<Mutex<HashMap<String, TubeCallback>>>,
}

impl WebRTCNetworkIntegration {
    pub fn new(monitor: Arc<NetworkMonitor>) -> Self {
        let integration = Self {
            monitor: monitor.clone(),
            tube_callbacks: Arc::new(Mutex::new(HashMap::new())),
        };

        // Register network change handler
        let callbacks = integration.tube_callbacks.clone();
        monitor.register_callback(move |event| {
            let callbacks = callbacks.clone();
            Box::pin(async move {
                Self::handle_network_change(callbacks, event).await;
            })
        });

        integration
    }

    /// Register a tube for network change notifications
    pub fn register_tube<F>(&self, tube_id: String, ice_restart_callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        if let Ok(mut callbacks) = self.tube_callbacks.lock() {
            callbacks.insert(tube_id.clone(), Box::new(ice_restart_callback));
            debug!(
                "Registered tube {} for network change notifications",
                tube_id
            );
        }
    }

    /// Unregister a tube from network change notifications
    #[allow(dead_code)]
    pub fn unregister_tube(&self, tube_id: &str) {
        if let Ok(mut callbacks) = self.tube_callbacks.lock() {
            if callbacks.remove(tube_id).is_some() {
                info!(
                    "Unregistered tube {} from network change notifications",
                    tube_id
                );
            }
        }
    }

    /// Trigger ICE restart for a specific tube (for connection state-based triggers)
    pub fn trigger_ice_restart(&self, tube_id: &str, reason: &str) {
        if let Ok(callbacks) = self.tube_callbacks.lock() {
            if let Some(callback) = callbacks.get(tube_id) {
                info!(
                    "Triggering ICE restart for tube {} due to: {}",
                    tube_id, reason
                );
                callback();
            } else {
                debug!("No ICE restart callback registered for tube {}", tube_id);
            }
        }
    }

    /// Handle network change events
    #[allow(dead_code)]
    async fn handle_network_change(
        callbacks: Arc<Mutex<HashMap<String, TubeCallback>>>,
        event: NetworkChangeEvent,
    ) {
        // Determine if this change requires ICE restart
        let requires_ice_restart = match &event {
            NetworkChangeEvent::InterfaceAdded { interface } => {
                // New interface might provide better connectivity
                interface.interface_type != InterfaceType::Loopback
            }
            NetworkChangeEvent::InterfaceRemoved { .. } => {
                // Interface removal might affect current connections
                true
            }
            NetworkChangeEvent::InterfaceStatusChanged { now_active, .. } => {
                // Interface status changes affect connectivity
                *now_active // Only restart if interface became active
            }
            NetworkChangeEvent::IpAddressChanged { .. } => {
                // IP address changes require ICE restart
                true
            }
            NetworkChangeEvent::PrimaryInterfaceChanged { .. } => {
                // Primary interface changes definitely require ICE restart
                true
            }
            NetworkChangeEvent::ConnectivityLost => {
                // Don't restart on connectivity loss - wait for restoration
                false
            }
            NetworkChangeEvent::ConnectivityRestored => {
                // Connectivity restoration requires ICE restart
                true
            }
            NetworkChangeEvent::MigrationCompleted { .. } => {
                // Migration completed - might need ICE restart for cleanup
                false
            }
            NetworkChangeEvent::QualityDegraded { .. } => {
                // Quality degradation might benefit from ICE restart
                true
            }
        };

        if requires_ice_restart {
            debug!("Network change requires ICE restart: {:?}", event);

            // Trigger ICE restart for all registered tubes
            if let Ok(callbacks) = callbacks.lock() {
                for (tube_id, callback) in callbacks.iter() {
                    debug!(
                        "Triggering ICE restart for tube {} due to network change",
                        tube_id
                    );
                    callback();
                }
            }
        } else {
            debug!("Network change does not require ICE restart: {:?}", event);
        }
    }

    /// Start monitoring
    pub async fn start(&self) -> WebRTCResult<()> {
        self.monitor.start_monitoring().await
    }

    /// Stop monitoring
    #[allow(dead_code)]
    pub fn stop(&self) {
        self.monitor.stop_monitoring();
    }

    /// Trigger initial network scan (event-driven startup)
    pub async fn trigger_initial_scan(&self) -> WebRTCResult<()> {
        self.monitor.trigger_initial_scan().await
    }
}
