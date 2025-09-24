use crate::webrtc_errors::{WebRTCError, WebRTCResult};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// TURN/STUN server configuration with health tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server URL (stun:// or turn://)
    pub url: String,
    /// Username for TURN authentication (optional for STUN)
    pub username: Option<String>,
    /// Password/credential for TURN authentication
    pub password: Option<String>,
    /// Server priority (1-10, higher = preferred)
    pub priority: u8,
    /// Whether this server is currently enabled
    pub enabled: bool,
    /// Server region/location hint
    pub region: Option<String>,
    /// Maximum concurrent connections allowed for this server
    pub max_connections: Option<u32>,
}

impl ServerConfig {
    pub fn new_stun(url: String, priority: u8) -> Self {
        Self {
            url,
            username: None,
            password: None,
            priority,
            enabled: true,
            region: None,
            max_connections: None,
        }
    }

    pub fn new_turn(url: String, username: String, password: String, priority: u8) -> Self {
        Self {
            url,
            username: Some(username),
            password: Some(password),
            priority,
            enabled: true,
            region: None,
            max_connections: Some(100), // Default TURN connection limit
        }
    }

    pub fn is_turn(&self) -> bool {
        self.url.starts_with("turn:")
    }

    pub fn is_stun(&self) -> bool {
        self.url.starts_with("stun:")
    }
}

/// Server health metrics and status
#[derive(Debug, Clone)]
pub struct ServerHealth {
    /// Server configuration
    pub config: ServerConfig,
    /// Current health status
    pub status: ServerStatus,
    /// Last successful connection timestamp
    pub last_success: Option<Instant>,
    /// Last failed connection timestamp
    pub last_failure: Option<Instant>,
    /// Total successful connections
    pub success_count: u64,
    /// Total failed connections
    pub failure_count: u64,
    /// Current active connections
    pub active_connections: u32,
    /// Average response time (moving average)
    pub avg_response_time: Duration,
    /// Recent response times for trend analysis
    pub recent_response_times: Vec<Duration>,
    /// Consecutive failure count (resets on success)
    pub consecutive_failures: u32,
    /// When health was last checked
    pub last_health_check: Instant,
    /// Failure reason (if any)
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServerStatus {
    /// Server is healthy and available
    Healthy,
    /// Server is experiencing issues but still usable
    Degraded,
    /// Server is temporarily unavailable
    Unhealthy,
    /// Server is disabled or unreachable
    Offline,
    /// Server status is unknown (not yet tested)
    Unknown,
}

impl ServerHealth {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            status: ServerStatus::Unknown,
            last_success: None,
            last_failure: None,
            success_count: 0,
            failure_count: 0,
            active_connections: 0,
            avg_response_time: Duration::from_millis(0),
            recent_response_times: Vec::with_capacity(10),
            consecutive_failures: 0,
            last_health_check: Instant::now(),
            last_failure_reason: None,
        }
    }

    /// Update health metrics with a successful connection
    pub fn record_success(&mut self, response_time: Duration) {
        self.last_success = Some(Instant::now());
        self.success_count += 1;
        self.consecutive_failures = 0;
        self.last_failure_reason = None;
        self.active_connections += 1;

        // Update response time metrics
        self.recent_response_times.push(response_time);
        if self.recent_response_times.len() > 10 {
            self.recent_response_times.remove(0);
        }

        // Calculate moving average
        let total_time: Duration = self.recent_response_times.iter().sum();
        self.avg_response_time = total_time / self.recent_response_times.len() as u32;

        // Update health status based on performance
        self.update_health_status();

        debug!(
            "Server success recorded (url: {}, response_time: {:?}, consecutive_failures: {}, status: {:?})",
            self.config.url, response_time, self.consecutive_failures, self.status
        );
    }

    /// Update health metrics with a failed connection
    pub fn record_failure(&mut self, reason: String) {
        self.last_failure = Some(Instant::now());
        self.failure_count += 1;
        self.consecutive_failures += 1;
        self.last_failure_reason = Some(reason.clone());

        // Update health status based on failure pattern
        self.update_health_status();

        warn!(
            "Server failure recorded (url: {}, reason: {}, consecutive_failures: {}, status: {:?})",
            self.config.url, reason, self.consecutive_failures, self.status
        );
    }

    /// Decrease active connection count
    pub fn connection_closed(&mut self) {
        if self.active_connections > 0 {
            self.active_connections -= 1;
        }
    }

    /// Update health status based on current metrics
    fn update_health_status(&mut self) {
        if !self.config.enabled {
            self.status = ServerStatus::Offline;
            return;
        }

        // Check consecutive failures
        if self.consecutive_failures >= 5 {
            self.status = ServerStatus::Offline;
        } else if self.consecutive_failures >= 3 {
            self.status = ServerStatus::Unhealthy;
        } else if self.consecutive_failures >= 1 {
            // Check if we have recent successes
            if let Some(last_success) = self.last_success {
                let time_since_success = Instant::now().duration_since(last_success);
                if time_since_success > Duration::from_secs(300) {
                    self.status = ServerStatus::Degraded;
                } else {
                    self.status = ServerStatus::Healthy;
                }
            } else {
                self.status = ServerStatus::Degraded;
            }
        } else {
            // No recent failures, check response time
            if self.avg_response_time > Duration::from_secs(5) {
                self.status = ServerStatus::Degraded;
            } else {
                self.status = ServerStatus::Healthy;
            }
        }

        self.last_health_check = Instant::now();
    }

    /// Check if server is available for new connections
    pub fn is_available(&self) -> bool {
        match self.status {
            ServerStatus::Healthy | ServerStatus::Degraded => {
                // Check connection limits
                if let Some(max_conn) = self.config.max_connections {
                    self.active_connections < max_conn
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// Get server priority score for selection
    pub fn get_priority_score(&self) -> f64 {
        if !self.is_available() {
            return 0.0;
        }

        let mut score = self.config.priority as f64;

        // Bonus for health status
        match self.status {
            ServerStatus::Healthy => score += 10.0,
            ServerStatus::Degraded => score += 5.0,
            _ => score = 0.0,
        }

        // Penalty for high response time
        if self.avg_response_time > Duration::from_secs(1) {
            score -= 5.0;
        }

        // Penalty for high failure rate
        if self.failure_count > 0 {
            let failure_rate =
                self.failure_count as f64 / (self.success_count + self.failure_count) as f64;
            score -= failure_rate * 10.0;
        }

        // Bonus for recent successful activity
        if let Some(last_success) = self.last_success {
            let time_since_success = Instant::now().duration_since(last_success);
            if time_since_success < Duration::from_secs(60) {
                score += 2.0;
            }
        }

        score.max(0.0)
    }
}

/// Multi-server TURN/STUN configuration manager with automatic failover
pub struct ServerManager {
    /// Server health tracking
    servers: Arc<RwLock<HashMap<String, ServerHealth>>>,
    /// Current server selection preferences
    selection_preferences: Arc<Mutex<ServerSelectionConfig>>,
    /// Health check configuration
    health_config: HealthCheckConfig,
}

#[derive(Debug, Clone)]
pub struct ServerSelectionConfig {
    /// Prefer servers in specific regions
    pub preferred_regions: Vec<String>,
    /// Minimum server priority to consider
    pub min_priority: u8,
    /// Maximum servers to try before giving up
    pub max_attempts: u32,
    /// Whether to randomize server selection within same priority
    pub randomize_selection: bool,
    /// Load balancing strategy
    pub load_balancing: LoadBalancingStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoadBalancingStrategy {
    /// Always prefer highest priority servers
    Priority,
    /// Round-robin among healthy servers
    RoundRobin,
    /// Prefer servers with lowest current load
    LeastConnections,
    /// Prefer servers with best response times
    LowestLatency,
}

#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// How often to perform active health checks
    pub check_interval: Duration,
    /// Timeout for health check operations
    pub check_timeout: Duration,
    /// How long to keep failed servers offline
    pub failure_timeout: Duration,
    /// Maximum consecutive failures before marking offline
    pub max_consecutive_failures: u32,
}

impl Default for ServerSelectionConfig {
    fn default() -> Self {
        Self {
            preferred_regions: Vec::new(),
            min_priority: 1,
            max_attempts: 3,
            randomize_selection: true,
            load_balancing: LoadBalancingStrategy::Priority,
        }
    }
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(120),
            check_timeout: Duration::from_secs(10),
            failure_timeout: Duration::from_secs(300),
            max_consecutive_failures: 5,
        }
    }
}

impl Default for ServerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            selection_preferences: Arc::new(Mutex::new(ServerSelectionConfig::default())),
            health_config: HealthCheckConfig::default(),
        }
    }

    /// Add a server to the configuration
    pub fn add_server(&self, config: ServerConfig) {
        let server_id = config.url.clone();
        let health = ServerHealth::new(config.clone());

        if let Ok(mut servers) = self.servers.write() {
            servers.insert(server_id.clone(), health);
            info!(
                "Added server to configuration (url: {}, priority: {}, type: {})",
                config.url,
                config.priority,
                if config.is_turn() { "TURN" } else { "STUN" }
            );
        }
    }

    /// Remove a server from the configuration
    pub fn remove_server(&self, server_url: &str) {
        if let Ok(mut servers) = self.servers.write() {
            if servers.remove(server_url).is_some() {
                info!("Removed server from configuration (url: {})", server_url);
            }
        }
    }

    /// Get the best available server for a new connection
    pub fn select_best_server(&self, tube_id: &str) -> WebRTCResult<ServerConfig> {
        let servers = self
            .servers
            .read()
            .map_err(|_| WebRTCError::IsolationFailure {
                tube_id: tube_id.to_string(),
                reason: "Failed to acquire server list lock".to_string(),
            })?;

        let selection_config =
            self.selection_preferences
                .lock()
                .map_err(|_| WebRTCError::IsolationFailure {
                    tube_id: tube_id.to_string(),
                    reason: "Failed to acquire selection config lock".to_string(),
                })?;

        // Filter available servers
        let mut available_servers: Vec<_> = servers
            .values()
            .filter(|health| {
                health.is_available()
                    && health.config.priority >= selection_config.min_priority
                    && health.config.enabled
            })
            .collect();

        if available_servers.is_empty() {
            return Err(WebRTCError::NoViableServers {
                tube_id: tube_id.to_string(),
            });
        }

        // Apply selection strategy
        match selection_config.load_balancing {
            LoadBalancingStrategy::Priority => {
                // Sort by priority score (highest first)
                available_servers.sort_by(|a, b| {
                    b.get_priority_score()
                        .partial_cmp(&a.get_priority_score())
                        .unwrap()
                });
            }
            LoadBalancingStrategy::LeastConnections => {
                // Sort by active connections (lowest first)
                available_servers.sort_by_key(|health| health.active_connections);
            }
            LoadBalancingStrategy::LowestLatency => {
                // Sort by response time (lowest first)
                available_servers.sort_by_key(|health| health.avg_response_time);
            }
            LoadBalancingStrategy::RoundRobin => {
                // Use deterministic selection based on tube_id
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};

                let mut hasher = DefaultHasher::new();
                tube_id.hash(&mut hasher);
                let tube_hash = hasher.finish();
                let index = (tube_hash as usize) % available_servers.len();

                // Move selected server to front
                if index > 0 {
                    available_servers.swap(0, index);
                }
            }
        }

        // Select the best server
        if let Some(selected) = available_servers.first() {
            debug!(
                "Selected server for tube {} (url: {}, priority_score: {:.1}, active_connections: {}, status: {:?})",
                tube_id, selected.config.url, selected.get_priority_score(),
                selected.active_connections, selected.status
            );
            Ok(selected.config.clone())
        } else {
            Err(WebRTCError::NoViableServers {
                tube_id: tube_id.to_string(),
            })
        }
    }

    /// Try multiple servers in sequence until one succeeds
    pub async fn try_servers_with_failover<F, Fut>(
        &self,
        tube_id: &str,
        mut operation: F,
    ) -> WebRTCResult<ServerConfig>
    where
        F: FnMut(&ServerConfig) -> Fut,
        Fut: std::future::Future<Output = WebRTCResult<()>>,
    {
        let max_attempts = {
            let selection_config =
                self.selection_preferences
                    .lock()
                    .map_err(|_| WebRTCError::IsolationFailure {
                        tube_id: tube_id.to_string(),
                        reason: "Failed to acquire selection config lock".to_string(),
                    })?;
            selection_config.max_attempts
        };

        for attempt in 1..=max_attempts {
            match self.select_best_server(tube_id) {
                Ok(server_config) => {
                    let server_url = server_config.url.clone();
                    let start_time = Instant::now();

                    debug!(
                        "Attempting server connection (tube_id: {}, url: {}, attempt: {}/{})",
                        tube_id, server_url, attempt, max_attempts
                    );

                    match operation(&server_config).await {
                        Ok(()) => {
                            let response_time = start_time.elapsed();
                            self.record_server_success(&server_url, response_time);

                            info!(
                                "Server connection successful (tube_id: {}, url: {}, response_time: {:?})",
                                tube_id, server_url, response_time
                            );
                            return Ok(server_config);
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            self.record_server_failure(&server_url, reason.clone());

                            warn!(
                                "Server connection failed (tube_id: {}, url: {}, attempt: {}/{}, reason: {})",
                                tube_id, server_url, attempt, max_attempts, reason
                            );

                            if attempt == max_attempts {
                                return Err(error);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "No servers available for tube {} (attempt: {}/{}): {}",
                        tube_id, attempt, max_attempts, e
                    );
                    if attempt == max_attempts {
                        return Err(e);
                    }
                }
            }

            // Brief delay before next attempt
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Err(WebRTCError::NoViableServers {
            tube_id: tube_id.to_string(),
        })
    }

    /// Record successful server operation
    pub fn record_server_success(&self, server_url: &str, response_time: Duration) {
        if let Ok(mut servers) = self.servers.write() {
            if let Some(health) = servers.get_mut(server_url) {
                health.record_success(response_time);
            }
        }
    }

    /// Record failed server operation
    pub fn record_server_failure(&self, server_url: &str, reason: String) {
        if let Ok(mut servers) = self.servers.write() {
            if let Some(health) = servers.get_mut(server_url) {
                health.record_failure(reason);
            }
        }
    }

    /// Record connection closed
    pub fn record_connection_closed(&self, server_url: &str) {
        if let Ok(mut servers) = self.servers.write() {
            if let Some(health) = servers.get_mut(server_url) {
                health.connection_closed();
            }
        }
    }

    /// Get health status for all servers
    pub fn get_server_health_status(&self) -> HashMap<String, ServerStatus> {
        if let Ok(servers) = self.servers.read() {
            servers
                .iter()
                .map(|(url, health)| (url.clone(), health.status))
                .collect()
        } else {
            HashMap::new()
        }
    }

    /// Get detailed health metrics for all servers
    pub fn get_detailed_health_metrics(&self) -> Vec<ServerHealth> {
        if let Ok(servers) = self.servers.read() {
            servers.values().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Enable or disable a server
    pub fn set_server_enabled(&self, server_url: &str, enabled: bool) {
        if let Ok(mut servers) = self.servers.write() {
            if let Some(health) = servers.get_mut(server_url) {
                health.config.enabled = enabled;
                info!(
                    "Server {} (url: {})",
                    if enabled { "enabled" } else { "disabled" },
                    server_url
                );
            }
        }
    }

    /// Update selection preferences
    pub fn update_selection_config(&self, config: ServerSelectionConfig) {
        if let Ok(mut prefs) = self.selection_preferences.lock() {
            *prefs = config;
            info!("Updated server selection configuration");
        }
    }

    /// Perform health check on all servers (for background task)
    pub async fn perform_health_checks(&self) {
        let server_urls: Vec<String> = {
            if let Ok(servers) = self.servers.read() {
                servers.keys().cloned().collect()
            } else {
                return;
            }
        };

        let server_count = server_urls.len();

        for server_url in server_urls {
            // In a real implementation, this would perform actual connectivity tests
            // For now, we just update health status based on recent activity
            if let Ok(mut servers) = self.servers.write() {
                if let Some(health) = servers.get_mut(&server_url) {
                    health.last_health_check = Instant::now();

                    // If server has been offline for too long, try to bring it back online
                    if health.status == ServerStatus::Offline {
                        if let Some(last_failure) = health.last_failure {
                            let time_since_failure = Instant::now().duration_since(last_failure);
                            if time_since_failure > self.health_config.failure_timeout {
                                health.status = ServerStatus::Unknown;
                                health.consecutive_failures = 0;
                                info!(
                                    "Server returned to unknown status after timeout (url: {})",
                                    server_url
                                );
                            }
                        }
                    }
                }
            }
        }

        debug!("Health check cycle completed for {} servers", server_count);
    }

    /// Get statistics about server usage
    pub fn get_server_statistics(&self) -> HashMap<String, (u64, u64, u32)> {
        if let Ok(servers) = self.servers.read() {
            servers
                .iter()
                .map(|(url, health)| {
                    (
                        url.clone(),
                        (
                            health.success_count,
                            health.failure_count,
                            health.active_connections,
                        ),
                    )
                })
                .collect()
        } else {
            HashMap::new()
        }
    }
}
