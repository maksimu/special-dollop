use crate::webrtc_errors::WebRTCResult;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::{interval, sleep};

/// Network interface information
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkInterface {
    pub name: String,
    pub interface_type: InterfaceType,
    pub is_active: bool,
    pub ip_address: Option<String>,
    pub last_seen: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InterfaceType {
    Ethernet,
    WiFi,
    Cellular,
    Loopback,
    VPN,
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
            InterfaceType::VPN
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
        }
    }
}

impl NetworkMonitor {
    pub fn new(config: NetworkMonitorConfig) -> Self {
        Self {
            interfaces: Arc::new(Mutex::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(Vec::new())),
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

        // Initial network scan
        self.scan_network_interfaces().await?;

        // Start background monitoring task
        let monitor = self.clone_for_task();
        tokio::spawn(async move {
            monitor.monitoring_loop().await;
        });

        info!("Network monitoring started");
        Ok(())
    }

    /// Stop network monitoring
    pub fn stop_monitoring(&self) {
        if let Ok(mut active) = self.monitoring_active.lock() {
            *active = false;
        }
        info!("Network monitoring stopped");
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

    /// Get system network interfaces (simplified cross-platform implementation)
    async fn get_system_interfaces(&self) -> WebRTCResult<HashMap<String, NetworkInterface>> {
        let mut interfaces = HashMap::new();
        let now = Instant::now();

        // Simplified interface detection - in a real implementation, this would use
        // platform-specific APIs (Windows: GetAdaptersAddresses, Linux: getifaddrs, macOS: System Configuration)

        // For now, we simulate common interface detection patterns
        #[cfg(target_os = "macos")]
        {
            // macOS common interfaces
            if self.check_interface_exists("en0").await {
                interfaces.insert(
                    "en0".to_string(),
                    NetworkInterface {
                        name: "en0".to_string(),
                        interface_type: InterfaceType::Ethernet,
                        is_active: true,
                        ip_address: Some("192.168.1.100".to_string()), // Simplified
                        last_seen: now,
                    },
                );
            }

            if self.check_interface_exists("en1").await {
                interfaces.insert(
                    "en1".to_string(),
                    NetworkInterface {
                        name: "en1".to_string(),
                        interface_type: InterfaceType::WiFi,
                        is_active: true,
                        ip_address: Some("192.168.1.101".to_string()), // Simplified
                        last_seen: now,
                    },
                );
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Linux common interfaces
            if self.check_interface_exists("eth0").await {
                interfaces.insert(
                    "eth0".to_string(),
                    NetworkInterface {
                        name: "eth0".to_string(),
                        interface_type: InterfaceType::Ethernet,
                        is_active: true,
                        ip_address: Some("192.168.1.100".to_string()),
                        last_seen: now,
                    },
                );
            }

            if self.check_interface_exists("wlan0").await {
                interfaces.insert(
                    "wlan0".to_string(),
                    NetworkInterface {
                        name: "wlan0".to_string(),
                        interface_type: InterfaceType::WiFi,
                        is_active: true,
                        ip_address: Some("192.168.1.101".to_string()),
                        last_seen: now,
                    },
                );
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Windows interfaces would be detected via Windows API
            // For now, simulate common patterns
            interfaces.insert(
                "ethernet".to_string(),
                NetworkInterface {
                    name: "ethernet".to_string(),
                    interface_type: InterfaceType::Ethernet,
                    is_active: true,
                    ip_address: Some("192.168.1.100".to_string()),
                    last_seen: now,
                },
            );
        }

        // Always include loopback
        interfaces.insert(
            "lo".to_string(),
            NetworkInterface {
                name: "lo".to_string(),
                interface_type: InterfaceType::Loopback,
                is_active: true,
                ip_address: Some("127.0.0.1".to_string()),
                last_seen: now,
            },
        );

        debug!("Detected {} network interfaces", interfaces.len());
        Ok(interfaces)
    }

    /// Check if a network interface exists (simplified)
    async fn check_interface_exists(&self, _interface_name: &str) -> bool {
        // In a real implementation, this would check if the interface actually exists
        // For now, we assume they exist for demonstration
        true
    }

    /// Test connectivity to a specific endpoint
    async fn test_endpoint_connectivity(&self, endpoint: &str) -> bool {
        match tokio::time::timeout(
            self.config.connectivity_timeout,
            tokio::net::TcpStream::connect(endpoint),
        )
        .await
        {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
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
    /// Main monitoring loop
    async fn monitoring_loop(&self) {
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
                debug!("Network monitoring loop terminated");
                break;
            }

            // Perform network interface scan
            if let Err(e) = self.scan_network_interfaces().await {
                error!("Network interface scan failed: {}", e);
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
        debug!("Background network interface scan");
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
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    /// Trigger network change event (delegated implementation)
    async fn trigger_event(&self, event: NetworkChangeEvent) {
        debug!("Network change detected in background task: {:?}", event);

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
            info!(
                "Registered tube {} for network change notifications",
                tube_id
            );
        }
    }

    /// Unregister a tube from network change notifications
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

    /// Handle network change events
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
        };

        if requires_ice_restart {
            info!("Network change requires ICE restart: {:?}", event);

            // Trigger ICE restart for all registered tubes
            if let Ok(callbacks) = callbacks.lock() {
                for (tube_id, callback) in callbacks.iter() {
                    info!(
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
    pub fn stop(&self) {
        self.monitor.stop_monitoring();
    }
}
