use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::sleep;

#[derive(Error, Debug)]
pub enum ResourceError {
    #[error("Resource exhausted: {resource} limit ({limit}) exceeded")]
    Exhausted { resource: String, limit: usize },
    #[error("Resource allocation failed: {reason}")]
    AllocationFailed { reason: String },
    #[error("Cleanup failed: {reason}")]
    CleanupFailed { reason: String },
}

pub type Result<T> = std::result::Result<T, ResourceError>;

#[derive(Clone)]
pub struct ResourceLimits {
    pub max_concurrent_sockets: usize,
    pub max_interfaces_per_agent: usize,
    pub max_concurrent_ice_agents: usize,
    pub max_turn_connections_per_server: usize,
    pub socket_reuse_enabled: bool,
    pub ice_gather_timeout: Duration,
    pub enable_mdns_candidates: bool,
    // RTCConfiguration tuning options
    pub ice_candidate_pool_size: Option<u8>,
    pub ice_transport_policy: Option<String>, // "all", "relay"
    pub bundle_policy: Option<String>,        // "balanced", "max-compat", "max-bundle"
    pub rtcp_mux_policy: Option<String>,      // "negotiate", "require"
    pub ice_connection_receiving_timeout: Option<Duration>,
    pub ice_backup_candidate_pair_ping_interval: Option<Duration>,
    // Session keepalive settings to prevent timeouts
    pub ice_keepalive_enabled: bool,
    pub ice_keepalive_interval: Duration,
    pub session_timeout: Duration,
    pub turn_credential_refresh_interval: Duration,
    pub connection_health_check_interval: Duration,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            // Production tuning: Set to 100k (effectively unlimited) to let OS limits dictate capacity
            // Real limits will be: file descriptors (~65k typical), memory (~500KB per connection),
            // bandwidth, and CPU - all of which give meaningful errors unlike artificial caps.
            // Typical production capacity: 500-1000 concurrent connections before hitting OS/HW limits.
            max_concurrent_sockets: 100_000,
            max_interfaces_per_agent: 8,
            max_concurrent_ice_agents: 100_000,
            max_turn_connections_per_server: 4,
            socket_reuse_enabled: true,
            ice_gather_timeout: Duration::from_secs(30), // Increased for trickle ICE + signaling latency
            enable_mdns_candidates: false,
            // Conservative defaults for RTCConfiguration tuning
            ice_candidate_pool_size: Some(4), // Limit ICE candidate pool
            ice_transport_policy: None,       // Use default (all)
            bundle_policy: Some("max-bundle".to_string()), // Reduce resource usage
            rtcp_mux_policy: Some("require".to_string()), // Reduce port usage
            ice_connection_receiving_timeout: Some(Duration::from_secs(30)),
            ice_backup_candidate_pair_ping_interval: Some(Duration::from_secs(25)),
            // Session keepalive defaults to prevent NAT timeouts
            ice_keepalive_enabled: true,
            ice_keepalive_interval: Duration::from_secs(60), // 60 seconds - CRITICAL: Frequent keepalive prevents NAT table expiry
            session_timeout: Duration::from_secs(3600),      // 1 hour - longer than NAT timeout
            turn_credential_refresh_interval: Duration::from_secs(600), // 10 minutes
            connection_health_check_interval: Duration::from_secs(120), // 2 minutes
        }
    }
}

#[derive(Clone)]
pub struct TurnConnectionInfo {
    pub server: String,
    pub username: String,
    pub password: String,
    pub created_at: Instant,
    pub ref_count: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub struct ResourceManager {
    limits: Arc<RwLock<ResourceLimits>>,
    socket_semaphore: Arc<Semaphore>,
    ice_agent_semaphore: Arc<Semaphore>,
    current_sockets: Arc<AtomicUsize>,
    current_ice_agents: Arc<AtomicUsize>,
    turn_connections: Arc<Mutex<HashMap<String, Vec<TurnConnectionInfo>>>>,
    stats: Arc<Mutex<ResourceStats>>,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    pub sockets_allocated: usize,
    pub sockets_released: usize,
    pub ice_agents_created: usize,
    pub ice_agents_destroyed: usize,
    pub turn_connections_pooled: usize,
    pub turn_connections_reused: usize,
    pub resource_exhaustion_errors: usize,
    pub last_exhaustion: Option<Instant>,
}

impl ResourceManager {
    pub fn new(limits: ResourceLimits) -> Self {
        let socket_semaphore = Arc::new(Semaphore::new(limits.max_concurrent_sockets));
        let ice_agent_semaphore = Arc::new(Semaphore::new(limits.max_concurrent_ice_agents));

        Self {
            limits: Arc::new(RwLock::new(limits)),
            socket_semaphore,
            ice_agent_semaphore,
            current_sockets: Arc::new(AtomicUsize::new(0)),
            current_ice_agents: Arc::new(AtomicUsize::new(0)),
            turn_connections: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new(ResourceStats::default())),
        }
    }

    pub async fn acquire_socket_permit(&self) -> Result<SocketGuard> {
        let permit = self
            .socket_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                let mut stats = self.stats.lock().unwrap();
                stats.resource_exhaustion_errors += 1;
                stats.last_exhaustion = Some(Instant::now());

                ResourceError::Exhausted {
                    resource: "sockets".to_string(),
                    limit: self.limits.read().unwrap().max_concurrent_sockets,
                }
            })?;

        let count = self.current_sockets.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut stats = self.stats.lock().unwrap();
            stats.sockets_allocated += 1;
        }

        debug!("Acquired socket permit, total: {}", count);
        Ok(SocketGuard::new(
            permit,
            self.current_sockets.clone(),
            self.stats.clone(),
        ))
    }

    pub async fn acquire_ice_agent_permit(&self) -> Result<IceAgentGuard> {
        let permit = self
            .ice_agent_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                let mut stats = self.stats.lock().unwrap();
                stats.resource_exhaustion_errors += 1;
                stats.last_exhaustion = Some(Instant::now());

                ResourceError::Exhausted {
                    resource: "ice_agents".to_string(),
                    limit: self.limits.read().unwrap().max_concurrent_ice_agents,
                }
            })?;

        let count = self.current_ice_agents.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut stats = self.stats.lock().unwrap();
            stats.ice_agents_created += 1;
        }

        debug!("Acquired ICE agent permit, total: {}", count);
        Ok(IceAgentGuard::new(
            permit,
            self.current_ice_agents.clone(),
            self.stats.clone(),
        ))
    }

    pub fn get_turn_connection(&self, server: &str) -> Option<TurnConnectionInfo> {
        let mut connections = self.turn_connections.lock().unwrap();

        if let Some(server_connections) = connections.get_mut(server) {
            server_connections.retain(|conn| conn.ref_count.load(Ordering::SeqCst) > 0);

            if let Some(conn) = server_connections
                .iter()
                .min_by_key(|c| c.ref_count.load(Ordering::SeqCst))
                .filter(|c| c.ref_count.load(Ordering::SeqCst) < 1)
            // No sharing - each tube gets dedicated TURN allocation for maximum throughput
            // This ensures RBI sessions don't compete for bandwidth through shared TURN ports
            {
                conn.ref_count.fetch_add(1, Ordering::SeqCst);
                let mut stats = self.stats.lock().unwrap();
                stats.turn_connections_reused += 1;
                return Some(conn.clone());
            }
        }

        None
    }

    pub fn add_turn_connection(
        &self,
        server: String,
        username: String,
        password: String,
    ) -> Arc<AtomicUsize> {
        let mut connections = self.turn_connections.lock().unwrap();
        let server_connections = connections.entry(server.clone()).or_default();

        let limits = self.limits.read().unwrap();
        if server_connections.len() >= limits.max_turn_connections_per_server {
            server_connections.remove(0);
        }

        let ref_count = Arc::new(AtomicUsize::new(1));
        let conn_info = TurnConnectionInfo {
            server,
            username,
            password,
            created_at: Instant::now(),
            ref_count: ref_count.clone(),
        };

        server_connections.push(conn_info);

        let mut stats = self.stats.lock().unwrap();
        stats.turn_connections_pooled += 1;

        ref_count
    }

    pub fn cleanup_stale_connections(&self) {
        let mut connections = self.turn_connections.lock().unwrap();
        let stale_threshold = Duration::from_secs(300); // 5 minutes
        let now = Instant::now();

        for server_connections in connections.values_mut() {
            server_connections.retain(|conn| {
                let is_recent = now.duration_since(conn.created_at) < stale_threshold;
                let has_refs = conn.ref_count.load(Ordering::SeqCst) > 0;
                is_recent && has_refs
            });
        }

        connections.retain(|_, conns| !conns.is_empty());
    }

    pub fn get_resource_status(&self) -> ResourceStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn update_limits(&self, new_limits: ResourceLimits) {
        let mut limits = self.limits.write().unwrap();
        *limits = new_limits;
        info!(
            "Updated resource limits: max_sockets={}, max_ice_agents={}",
            limits.max_concurrent_sockets, limits.max_concurrent_ice_agents
        );
        warn!(
            "WARNING: Resource limit changes only affect reported limits, not actual semaphore capacity. \
            Semaphores are fixed at initialization time. For testing, increase default limits instead."
        );
    }

    pub fn get_limits(&self) -> ResourceLimits {
        self.limits.read().unwrap().clone()
    }

    /// Retry ICE gathering with exponential backoff for resource failures
    pub async fn gather_candidates_with_backoff<F, Fut, T>(
        &self,
        mut operation: F,
        tube_id: &str,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, String>>,
        T: Send + 'static,
    {
        let mut attempts = 0u32;
        let max_attempts = 3u32;
        let mut delay = Duration::from_millis(100);

        loop {
            match operation().await {
                Ok(result) => {
                    if attempts > 0 {
                        info!(
                            "ICE gathering succeeded after {} retries (tube_id: {}, attempts: {})",
                            attempts, tube_id, attempts
                        );
                    }
                    return Ok(result);
                }
                Err(err) if attempts < max_attempts && self.is_resource_exhaustion_error(&err) => {
                    warn!("ICE gathering failed due to resource exhaustion, retrying in {}ms (tube_id: {}, attempts: {}, delay_ms: {}, error: {})", delay.as_millis(), tube_id, attempts, delay.as_millis(), err);

                    // Wait with exponential backoff
                    sleep(delay).await;
                    delay = std::cmp::min(delay * 2, Duration::from_secs(5)); // Cap at 5 seconds
                    attempts += 1;
                }
                Err(err) => {
                    error!("ICE gathering failed permanently after {} attempts (tube_id: {}, attempts: {}, error: {})", attempts, tube_id, attempts, err);
                    return Err(ResourceError::AllocationFailed { reason: err });
                }
            }
        }
    }

    /// Retry resource acquisition with exponential backoff
    pub async fn acquire_resource_with_backoff<F, Fut, T>(
        &self,
        mut operation: F,
        resource_name: &str,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        T: Send + 'static,
    {
        let mut attempts = 0u32;
        let max_attempts = 3u32;
        let mut delay = Duration::from_millis(50); // Shorter delay for resource acquisition

        loop {
            match operation().await {
                Ok(result) => {
                    if attempts > 0 {
                        info!("Resource acquisition succeeded after {} retries (resource: {}, attempts: {})", attempts, resource_name, attempts);
                    }
                    return Ok(result);
                }
                Err(ResourceError::Exhausted { .. }) if attempts < max_attempts => {
                    warn!("Resource exhausted, retrying in {}ms (resource: {}, attempts: {}, delay_ms: {})", delay.as_millis(), resource_name, attempts, delay.as_millis());

                    // Wait with exponential backoff
                    sleep(delay).await;
                    delay = std::cmp::min(delay * 2, Duration::from_millis(1000)); // Cap at 1 second
                    attempts += 1;
                }
                Err(err) => {
                    error!("Resource acquisition failed permanently: {:?} (resource: {}, attempts: {})", err, resource_name, attempts);
                    return Err(err);
                }
            }
        }
    }

    /// Apply resource-conscious RTCConfiguration tuning
    pub fn apply_rtc_config_tuning(
        &self,
        mut config: webrtc::peer_connection::configuration::RTCConfiguration,
        tube_id: &str,
    ) -> webrtc::peer_connection::configuration::RTCConfiguration {
        let limits = self.limits.read().unwrap();

        // Apply ICE candidate pool size limit
        if let Some(pool_size) = limits.ice_candidate_pool_size {
            config.ice_candidate_pool_size = pool_size;
            debug!(
                "Applied ICE candidate pool size limit (tube_id: {}, pool_size: {})",
                tube_id, pool_size
            );
        }

        // Apply ICE transport policy for resource management
        if let Some(ref policy) = limits.ice_transport_policy {
            match policy.as_str() {
                "relay" => config.ice_transport_policy = webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay,
                "all" => config.ice_transport_policy = webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All,
                _ => warn!("Unknown ICE transport policy, using default (tube_id: {}, policy: {})", tube_id, policy),
            }
            debug!(
                "Applied ICE transport policy (tube_id: {}, policy: {})",
                tube_id, policy
            );
        }

        // Apply bundle policy for resource optimization
        if let Some(ref policy) = limits.bundle_policy {
            match policy.as_str() {
                "balanced" => {
                    config.bundle_policy =
                        webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy::Balanced
                }
                "max-compat" => {
                    config.bundle_policy =
                        webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy::MaxCompat
                }
                "max-bundle" => {
                    config.bundle_policy =
                        webrtc::peer_connection::policy::bundle_policy::RTCBundlePolicy::MaxBundle
                }
                _ => {
                    warn!(
                        "Unknown bundle policy, using default (tube_id: {}, policy: {})",
                        tube_id, policy
                    );
                }
            }
            debug!(
                "Applied bundle policy (tube_id: {}, policy: {})",
                tube_id, policy
            );
        }

        // Apply RTCP mux policy for port reduction
        if let Some(ref policy) = limits.rtcp_mux_policy {
            match policy.as_str() {
                "negotiate" => config.rtcp_mux_policy =
                    webrtc::peer_connection::policy::rtcp_mux_policy::RTCRtcpMuxPolicy::Negotiate,
                "require" => {
                    config.rtcp_mux_policy =
                        webrtc::peer_connection::policy::rtcp_mux_policy::RTCRtcpMuxPolicy::Require
                }
                _ => {
                    warn!(
                        "Unknown RTCP mux policy, using default (tube_id: {}, policy: {})",
                        tube_id, policy
                    );
                }
            }
            debug!(
                "Applied RTCP mux policy (tube_id: {}, policy: {})",
                tube_id, policy
            );
        }

        // Note: ICE connection receiving timeout and backup candidate pair ping interval
        // are not available in this WebRTC library version, but are tracked for future use
        if let Some(_timeout) = limits.ice_connection_receiving_timeout {
            debug!("ICE connection receiving timeout configured but not applied (not supported by WebRTC library) (tube_id: {})", tube_id);
        }

        if let Some(_interval) = limits.ice_backup_candidate_pair_ping_interval {
            debug!("ICE backup candidate pair ping interval configured but not applied (not supported by WebRTC library) (tube_id: {})", tube_id);
        }

        debug!(
            "Applied resource-conscious RTCConfiguration tuning (tube_id: {})",
            tube_id
        );
        config
    }

    /// Check if an error string indicates resource exhaustion
    fn is_resource_exhaustion_error(&self, error: &str) -> bool {
        let error_lower = error.to_lowercase();
        error_lower.contains("too many open files")
            || error_lower.contains("emfile")
            || error_lower.contains("enfile")
            || error_lower.contains("resource exhausted")
            || error_lower.contains("device or resource busy")
            || error_lower.contains("no buffer space available")
    }
}

pub struct SocketGuard {
    _permit: tokio::sync::OwnedSemaphorePermit,
    counter: Arc<AtomicUsize>,
    stats: Arc<Mutex<ResourceStats>>,
}

impl SocketGuard {
    fn new(
        permit: tokio::sync::OwnedSemaphorePermit,
        counter: Arc<AtomicUsize>,
        stats: Arc<Mutex<ResourceStats>>,
    ) -> Self {
        Self {
            _permit: permit,
            counter,
            stats,
        }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let count = self.counter.fetch_sub(1, Ordering::SeqCst) - 1;
        {
            let mut stats = self.stats.lock().unwrap();
            stats.sockets_released += 1;
        }
        debug!("Released socket permit, remaining: {}", count);
    }
}

pub struct IceAgentGuard {
    _permit: tokio::sync::OwnedSemaphorePermit,
    counter: Arc<AtomicUsize>,
    stats: Arc<Mutex<ResourceStats>>,
}

impl IceAgentGuard {
    fn new(
        permit: tokio::sync::OwnedSemaphorePermit,
        counter: Arc<AtomicUsize>,
        stats: Arc<Mutex<ResourceStats>>,
    ) -> Self {
        Self {
            _permit: permit,
            counter,
            stats,
        }
    }
}

impl Drop for IceAgentGuard {
    fn drop(&mut self) {
        let count = self.counter.fetch_sub(1, Ordering::SeqCst) - 1;
        {
            let mut stats = self.stats.lock().unwrap();
            stats.ice_agents_destroyed += 1;
        }
        info!("Released ICE agent permit, remaining: {}", count);
    }
}

// Global resource manager instance
use once_cell::sync::Lazy;

pub static RESOURCE_MANAGER: Lazy<ResourceManager> =
    Lazy::new(|| ResourceManager::new(ResourceLimits::default()));
