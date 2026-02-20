// guacr-handlers: vSphere REST API client
//
// Provides a Rust client for the VMware vSphere REST API (vCenter 7.0+).
// Handles session-based authentication, VM lifecycle management, and
// console ticket acquisition for WMKS-based remote display.
//
// All API calls go through reqwest with TLS support. The session ID
// obtained from POST /api/session is injected into every subsequent
// request via the `vmware-api-session-id` header.

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Power state of a virtual machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerState {
    PoweredOn,
    PoweredOff,
    Suspended,
}

impl fmt::Display for PowerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PowerState::PoweredOn => write!(f, "POWERED_ON"),
            PowerState::PoweredOff => write!(f, "POWERED_OFF"),
            PowerState::Suspended => write!(f, "SUSPENDED"),
        }
    }
}

impl PowerState {
    /// Parse from the vSphere API string representation.
    fn from_api_str(s: &str) -> Self {
        match s {
            "POWERED_ON" => PowerState::PoweredOn,
            "POWERED_OFF" => PowerState::PoweredOff,
            "SUSPENDED" => PowerState::Suspended,
            _ => PowerState::PoweredOff,
        }
    }
}

/// Summary information about a virtual machine, as returned by the list endpoint.
#[derive(Debug, Clone)]
pub struct VmInfo {
    pub vm_id: String,
    pub name: String,
    pub power_state: PowerState,
    pub guest_os: String,
    pub num_cpus: u32,
    pub memory_mb: u64,
}

impl VmInfo {
    /// Column definitions for tabular display: (header, width).
    pub fn columns() -> Vec<(&'static str, usize)> {
        vec![
            ("VM ID", 16),
            ("Name", 30),
            ("Power State", 14),
            ("Guest OS", 20),
            ("CPUs", 6),
            ("Memory (MB)", 12),
        ]
    }

    /// Format this VM as a row of strings matching the column order from `columns()`.
    pub fn to_row(&self) -> Vec<String> {
        vec![
            self.vm_id.clone(),
            self.name.clone(),
            self.power_state.to_string(),
            self.guest_os.clone(),
            self.num_cpus.to_string(),
            self.memory_mb.to_string(),
        ]
    }
}

/// Disk information attached to a VM.
#[derive(Debug, Clone, Deserialize)]
pub struct DiskInfo {
    /// Disk label (e.g. "Hard disk 1").
    pub label: String,
    /// Capacity in bytes.
    pub capacity: u64,
}

/// Network interface information attached to a VM.
#[derive(Debug, Clone, Deserialize)]
pub struct NicInfo {
    /// NIC label (e.g. "Network adapter 1").
    pub label: String,
    /// MAC address.
    pub mac_address: String,
    /// Backing network name or ID.
    pub network: Option<String>,
}

/// Detailed information about a virtual machine, including hardware inventory.
#[derive(Debug, Clone)]
pub struct VmDetail {
    pub info: VmInfo,
    pub ip_address: Option<String>,
    pub host: Option<String>,
    pub disks: Vec<DiskInfo>,
    pub nics: Vec<NicInfo>,
}

/// Console ticket for WMKS (WebMKS) remote display connection.
#[derive(Debug, Clone)]
pub struct ConsoleTicket {
    /// The one-time ticket string.
    pub ticket: String,
    /// The ESXi host to connect to.
    pub host: String,
    /// The port for the WMKS connection.
    pub port: u16,
    /// SSL thumbprint of the target host (if available).
    pub ssl_thumbprint: Option<String>,
}

/// Summary information about an ESXi host managed by vCenter.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub host_id: String,
    pub name: String,
    pub connection_state: String,
    pub power_state: String,
}

// ---------------------------------------------------------------------------
// Internal JSON response shapes (map directly to vSphere REST API JSON)
// ---------------------------------------------------------------------------

/// JSON shape returned by GET /api/vcenter/vm (array element).
#[derive(Deserialize)]
struct VmSummaryJson {
    #[serde(rename = "vm")]
    vm_id: String,
    name: String,
    power_state: String,
    #[serde(default, rename = "guest_OS")]
    guest_os: String,
    #[serde(default)]
    cpu_count: Option<u32>,
    #[serde(default, rename = "memory_size_MiB")]
    memory_size_mib: Option<u64>,
}

/// JSON shape returned by GET /api/vcenter/vm/{vm}.
#[derive(Deserialize)]
struct VmDetailJson {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "guest_OS")]
    guest_os: Option<String>,
    #[serde(default)]
    power_state: Option<String>,
    #[serde(default)]
    cpu: Option<CpuJson>,
    #[serde(default)]
    memory: Option<MemoryJson>,
    #[serde(default)]
    disks: Option<serde_json::Value>,
    #[serde(default)]
    nics: Option<serde_json::Value>,
    #[serde(default)]
    identity: Option<IdentityJson>,
    #[serde(default)]
    host: Option<String>,
}

#[derive(Deserialize)]
struct CpuJson {
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Deserialize)]
struct MemoryJson {
    #[serde(default, rename = "size_MiB")]
    size_mib: Option<u64>,
}

#[derive(Deserialize)]
struct IdentityJson {
    #[serde(default)]
    ip_address: Option<String>,
}

/// JSON shape returned by POST /api/vcenter/vm/{vm}/console/tickets.
#[derive(Deserialize)]
struct ConsoleTicketJson {
    ticket: String,
    host: String,
    port: u16,
    #[serde(default)]
    ssl_thumbprint: Option<String>,
}

/// JSON shape returned by GET /api/vcenter/host (array element).
#[derive(Deserialize)]
struct HostSummaryJson {
    #[serde(rename = "host")]
    host_id: String,
    name: String,
    connection_state: String,
    power_state: String,
}

// ---------------------------------------------------------------------------
// VSphereClient
// ---------------------------------------------------------------------------

/// Client for the VMware vSphere REST API.
///
/// Manages session-based authentication and provides methods for VM lifecycle
/// management, host enumeration, and console ticket acquisition.
///
/// # Usage
///
/// ```no_run
/// # async fn example() -> Result<(), String> {
/// use guacr_handlers::VSphereClient;
///
/// let mut client = VSphereClient::connect(
///     "vcenter.example.com",
///     "administrator@vsphere.local",
///     "password",
///     true,
/// ).await?;
///
/// let vms = client.list_vms().await?;
/// for vm in &vms {
///     println!("{}: {} ({})", vm.vm_id, vm.name, vm.power_state);
/// }
///
/// client.logout().await?;
/// # Ok(())
/// # }
/// ```
pub struct VSphereClient {
    client: reqwest::Client,
    base_url: String,
    session_id: Option<String>,
}

impl VSphereClient {
    /// Create a new vSphere client and authenticate against the given vCenter host.
    ///
    /// - `hostname`: vCenter FQDN or IP (e.g. "vcenter.example.com")
    /// - `username`: vSphere SSO username (e.g. "administrator@vsphere.local")
    /// - `password`: vSphere SSO password
    /// - `verify_ssl`: whether to verify the server TLS certificate
    pub async fn connect(
        hostname: &str,
        username: &str,
        password: &str,
        verify_ssl: bool,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(!verify_ssl)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        let base_url = format!("https://{}", hostname);

        let mut instance = VSphereClient {
            client,
            base_url,
            session_id: None,
        };

        instance.authenticate(username, password).await?;
        Ok(instance)
    }

    /// Authenticate via POST /api/session with HTTP Basic auth.
    ///
    /// The vSphere REST API returns a session ID as a plain JSON string in the
    /// response body. This session ID is used for all subsequent requests in the
    /// `vmware-api-session-id` header.
    async fn authenticate(&mut self, username: &str, password: &str) -> Result<(), String> {
        let url = format!("{}/api/session", self.base_url);

        let credentials = format!("{}:{}", username, password);
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            credentials.as_bytes(),
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {}", encoded))
                .map_err(|e| format!("Invalid auth header: {}", e))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Authentication request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "Authentication failed (HTTP {}): {}",
                status.as_u16(),
                body
            ));
        }

        // The session ID comes back as a JSON string (quoted).
        let session_id: String = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse session ID: {}", e))?;

        if session_id.is_empty() {
            return Err("Server returned empty session ID".to_string());
        }

        self.session_id = Some(session_id);
        Ok(())
    }

    /// Build the standard headers for an authenticated API request.
    fn auth_headers(&self) -> Result<HeaderMap, String> {
        let session_id = self
            .session_id
            .as_ref()
            .ok_or_else(|| "Not authenticated (no session ID)".to_string())?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "vmware-api-session-id",
            HeaderValue::from_str(session_id)
                .map_err(|e| format!("Invalid session ID header value: {}", e))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// Build the full URL for an API path (e.g. "/api/vcenter/vm").
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// List all virtual machines managed by vCenter.
    ///
    /// Calls GET /api/vcenter/vm and returns a vector of `VmInfo` summaries.
    pub async fn list_vms(&self) -> Result<Vec<VmInfo>, String> {
        let url = self.api_url("/api/vcenter/vm");
        let headers = self.auth_headers()?;

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Failed to list VMs: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "List VMs failed (HTTP {}): {}",
                status.as_u16(),
                body
            ));
        }

        let summaries: Vec<VmSummaryJson> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse VM list: {}", e))?;

        let vms = summaries
            .into_iter()
            .map(|s| VmInfo {
                vm_id: s.vm_id,
                name: s.name,
                power_state: PowerState::from_api_str(&s.power_state),
                guest_os: if s.guest_os.is_empty() {
                    "Unknown".to_string()
                } else {
                    s.guest_os
                },
                num_cpus: s.cpu_count.unwrap_or(0),
                memory_mb: s.memory_size_mib.unwrap_or(0),
            })
            .collect();

        Ok(vms)
    }

    /// Get detailed information about a specific VM.
    ///
    /// Calls GET /api/vcenter/vm/{vm} and returns hardware details including
    /// disks, NICs, IP address, and host placement.
    pub async fn get_vm(&self, vm_id: &str) -> Result<VmDetail, String> {
        let url = self.api_url(&format!("/api/vcenter/vm/{}", vm_id));
        let headers = self.auth_headers()?;

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Failed to get VM {}: {}", vm_id, e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "Get VM failed (HTTP {}): {}",
                status.as_u16(),
                body
            ));
        }

        let detail: VmDetailJson = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse VM detail: {}", e))?;

        let disks = Self::parse_disks(&detail.disks);
        let nics = Self::parse_nics(&detail.nics);

        let info = VmInfo {
            vm_id: vm_id.to_string(),
            name: detail.name.unwrap_or_else(|| "Unknown".to_string()),
            power_state: PowerState::from_api_str(
                detail.power_state.as_deref().unwrap_or("POWERED_OFF"),
            ),
            guest_os: detail.guest_os.unwrap_or_else(|| "Unknown".to_string()),
            num_cpus: detail.cpu.and_then(|c| c.count).unwrap_or(0),
            memory_mb: detail.memory.and_then(|m| m.size_mib).unwrap_or(0),
        };

        let ip_address = detail.identity.and_then(|id| id.ip_address);

        Ok(VmDetail {
            info,
            ip_address,
            host: detail.host,
            disks,
            nics,
        })
    }

    /// Parse the disks map from the vSphere JSON response.
    ///
    /// The vSphere API returns disks as a JSON object keyed by disk ID (e.g.
    /// `{"2000": {"label": "Hard disk 1", "capacity": 10737418240}}`).
    fn parse_disks(disks_value: &Option<serde_json::Value>) -> Vec<DiskInfo> {
        let Some(value) = disks_value else {
            return Vec::new();
        };
        let Some(obj) = value.as_object() else {
            return Vec::new();
        };

        let mut result = Vec::with_capacity(obj.len());
        for (_key, disk_val) in obj {
            let label = disk_val
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let capacity = disk_val
                .get("capacity")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            result.push(DiskInfo { label, capacity });
        }
        result
    }

    /// Parse the NICs map from the vSphere JSON response.
    ///
    /// The vSphere API returns NICs as a JSON object keyed by NIC ID (e.g.
    /// `{"4000": {"label": "Network adapter 1", "mac_address": "00:50:56:...", ...}}`).
    fn parse_nics(nics_value: &Option<serde_json::Value>) -> Vec<NicInfo> {
        let Some(value) = nics_value else {
            return Vec::new();
        };
        let Some(obj) = value.as_object() else {
            return Vec::new();
        };

        let mut result = Vec::with_capacity(obj.len());
        for (_key, nic_val) in obj {
            let label = nic_val
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let mac_address = nic_val
                .get("mac_address")
                .and_then(|v| v.as_str())
                .unwrap_or("00:00:00:00:00:00")
                .to_string();
            let network = nic_val
                .get("backing")
                .and_then(|b| b.get("network"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            result.push(NicInfo {
                label,
                mac_address,
                network,
            });
        }
        result
    }

    /// Power on a virtual machine.
    ///
    /// Calls POST /api/vcenter/vm/{vm}/power?action=start.
    pub async fn power_on(&self, vm_id: &str) -> Result<(), String> {
        self.power_action(vm_id, "start").await
    }

    /// Power off a virtual machine.
    ///
    /// Calls POST /api/vcenter/vm/{vm}/power?action=stop.
    pub async fn power_off(&self, vm_id: &str) -> Result<(), String> {
        self.power_action(vm_id, "stop").await
    }

    /// Execute a VM power action (start, stop, suspend, reset).
    async fn power_action(&self, vm_id: &str, action: &str) -> Result<(), String> {
        let url = format!(
            "{}/api/vcenter/vm/{}/power?action={}",
            self.base_url, vm_id, action
        );
        let headers = self.auth_headers()?;

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Power {} for VM {} failed: {}", action, vm_id, e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "Power {} failed (HTTP {}): {}",
                action,
                status.as_u16(),
                body
            ));
        }

        Ok(())
    }

    /// Acquire a console ticket for WMKS (Web Machine Key Sequence) remote display.
    ///
    /// Calls POST /api/vcenter/vm/{vm}/console/tickets with a WEBMKS ticket type.
    /// The returned ticket, host, and port can be used to establish a WebSocket
    /// connection for remote console access.
    pub async fn get_console_ticket(&self, vm_id: &str) -> Result<ConsoleTicket, String> {
        let url = self.api_url(&format!("/api/vcenter/vm/{}/console/tickets", vm_id));
        let headers = self.auth_headers()?;

        let body = serde_json::json!({
            "spec": {
                "type": "WEBMKS"
            }
        });

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Get console ticket for VM {} failed: {}", vm_id, e))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "Console ticket request failed (HTTP {}): {}",
                status.as_u16(),
                body_text
            ));
        }

        let ticket_json: ConsoleTicketJson = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse console ticket: {}", e))?;

        Ok(ConsoleTicket {
            ticket: ticket_json.ticket,
            host: ticket_json.host,
            port: ticket_json.port,
            ssl_thumbprint: ticket_json.ssl_thumbprint,
        })
    }

    /// List all ESXi hosts managed by vCenter.
    ///
    /// Calls GET /api/vcenter/host and returns a vector of `HostInfo` summaries.
    pub async fn list_hosts(&self) -> Result<Vec<HostInfo>, String> {
        let url = self.api_url("/api/vcenter/host");
        let headers = self.auth_headers()?;

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Failed to list hosts: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "List hosts failed (HTTP {}): {}",
                status.as_u16(),
                body
            ));
        }

        let summaries: Vec<HostSummaryJson> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse host list: {}", e))?;

        let hosts = summaries
            .into_iter()
            .map(|h| HostInfo {
                host_id: h.host_id,
                name: h.name,
                connection_state: h.connection_state,
                power_state: h.power_state,
            })
            .collect();

        Ok(hosts)
    }

    /// Terminate the vSphere session.
    ///
    /// Calls DELETE /api/session. After logout, the client can no longer make
    /// authenticated requests. It is safe to call logout multiple times.
    pub async fn logout(&mut self) -> Result<(), String> {
        if self.session_id.is_none() {
            return Ok(());
        }

        let url = self.api_url("/api/session");
        let headers = self.auth_headers()?;

        let response = self
            .client
            .delete(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Logout request failed: {}", e))?;

        let status = response.status();
        // Clear session regardless of response -- the session may have already
        // expired on the server side.
        self.session_id = None;

        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(format!(
                "Logout failed (HTTP {}): {}",
                status.as_u16(),
                body
            ));
        }

        Ok(())
    }

    /// Returns the base URL this client is configured to use.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns true if the client currently holds a valid session ID.
    pub fn is_authenticated(&self) -> bool {
        self.session_id.is_some()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PowerState tests --

    #[test]
    fn test_power_state_display() {
        assert_eq!(PowerState::PoweredOn.to_string(), "POWERED_ON");
        assert_eq!(PowerState::PoweredOff.to_string(), "POWERED_OFF");
        assert_eq!(PowerState::Suspended.to_string(), "SUSPENDED");
    }

    #[test]
    fn test_power_state_from_api_str() {
        assert_eq!(
            PowerState::from_api_str("POWERED_ON"),
            PowerState::PoweredOn
        );
        assert_eq!(
            PowerState::from_api_str("POWERED_OFF"),
            PowerState::PoweredOff
        );
        assert_eq!(PowerState::from_api_str("SUSPENDED"), PowerState::Suspended);
        // Unknown values default to PoweredOff
        assert_eq!(
            PowerState::from_api_str("UNKNOWN_STATE"),
            PowerState::PoweredOff
        );
    }

    #[test]
    fn test_power_state_roundtrip() {
        for state in &[
            PowerState::PoweredOn,
            PowerState::PoweredOff,
            PowerState::Suspended,
        ] {
            let display = state.to_string();
            let parsed = PowerState::from_api_str(&display);
            assert_eq!(&parsed, state);
        }
    }

    // -- VmInfo tests --

    fn sample_vm_info() -> VmInfo {
        VmInfo {
            vm_id: "vm-42".to_string(),
            name: "test-server".to_string(),
            power_state: PowerState::PoweredOn,
            guest_os: "UBUNTU_64".to_string(),
            num_cpus: 4,
            memory_mb: 8192,
        }
    }

    #[test]
    fn test_vm_info_to_row() {
        let vm = sample_vm_info();
        let row = vm.to_row();
        assert_eq!(row.len(), 6);
        assert_eq!(row[0], "vm-42");
        assert_eq!(row[1], "test-server");
        assert_eq!(row[2], "POWERED_ON");
        assert_eq!(row[3], "UBUNTU_64");
        assert_eq!(row[4], "4");
        assert_eq!(row[5], "8192");
    }

    #[test]
    fn test_vm_info_to_row_powered_off() {
        let vm = VmInfo {
            vm_id: "vm-99".to_string(),
            name: "offline-box".to_string(),
            power_state: PowerState::PoweredOff,
            guest_os: "WINDOWS_9_SERVER_64".to_string(),
            num_cpus: 2,
            memory_mb: 4096,
        };
        let row = vm.to_row();
        assert_eq!(row[2], "POWERED_OFF");
        assert_eq!(row[4], "2");
        assert_eq!(row[5], "4096");
    }

    #[test]
    fn test_vm_info_columns() {
        let cols = VmInfo::columns();
        assert_eq!(cols.len(), 6);
        assert_eq!(cols[0].0, "VM ID");
        assert_eq!(cols[1].0, "Name");
        assert_eq!(cols[2].0, "Power State");
        assert_eq!(cols[3].0, "Guest OS");
        assert_eq!(cols[4].0, "CPUs");
        assert_eq!(cols[5].0, "Memory (MB)");
    }

    #[test]
    fn test_vm_info_columns_row_alignment() {
        // The number of columns must match the number of row elements
        let cols = VmInfo::columns();
        let vm = sample_vm_info();
        let row = vm.to_row();
        assert_eq!(cols.len(), row.len());
    }

    // -- API URL construction --

    #[test]
    fn test_api_url_construction() {
        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vcenter.example.com".to_string(),
            session_id: Some("test-session-id".to_string()),
        };

        assert_eq!(
            client.api_url("/api/session"),
            "https://vcenter.example.com/api/session"
        );
        assert_eq!(
            client.api_url("/api/vcenter/vm"),
            "https://vcenter.example.com/api/vcenter/vm"
        );
        assert_eq!(
            client.api_url("/api/vcenter/vm/vm-42"),
            "https://vcenter.example.com/api/vcenter/vm/vm-42"
        );
        assert_eq!(
            client.api_url("/api/vcenter/host"),
            "https://vcenter.example.com/api/vcenter/host"
        );
    }

    #[test]
    fn test_api_url_vm_power() {
        let base = "https://vc.local";
        let vm_id = "vm-123";
        let url = format!("{}/api/vcenter/vm/{}/power?action=start", base, vm_id);
        assert_eq!(
            url,
            "https://vc.local/api/vcenter/vm/vm-123/power?action=start"
        );

        let url = format!("{}/api/vcenter/vm/{}/power?action=stop", base, vm_id);
        assert_eq!(
            url,
            "https://vc.local/api/vcenter/vm/vm-123/power?action=stop"
        );
    }

    #[test]
    fn test_api_url_console_tickets() {
        let base = "https://vc.local";
        let vm_id = "vm-456";
        let url = format!("{}/api/vcenter/vm/{}/console/tickets", base, vm_id);
        assert_eq!(
            url,
            "https://vc.local/api/vcenter/vm/vm-456/console/tickets"
        );
    }

    // -- Session header injection --

    #[test]
    fn test_auth_headers_with_session() {
        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vcenter.example.com".to_string(),
            session_id: Some("abc-session-123".to_string()),
        };

        let headers = client.auth_headers().unwrap();
        assert_eq!(
            headers
                .get("vmware-api-session-id")
                .unwrap()
                .to_str()
                .unwrap(),
            "abc-session-123"
        );
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap().to_str().unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_auth_headers_without_session() {
        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vcenter.example.com".to_string(),
            session_id: None,
        };

        let result = client.auth_headers();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Not authenticated"));
    }

    // -- JSON response parsing --

    #[test]
    fn test_parse_vm_list_response() {
        let json = r#"[
            {
                "vm": "vm-42",
                "name": "web-server-01",
                "power_state": "POWERED_ON",
                "guest_OS": "UBUNTU_64",
                "cpu_count": 4,
                "memory_size_MiB": 8192
            },
            {
                "vm": "vm-43",
                "name": "db-server-01",
                "power_state": "POWERED_OFF",
                "guest_OS": "RHEL_8_64",
                "cpu_count": 8,
                "memory_size_MiB": 16384
            }
        ]"#;

        let summaries: Vec<VmSummaryJson> = serde_json::from_str(json).unwrap();
        assert_eq!(summaries.len(), 2);

        let vms: Vec<VmInfo> = summaries
            .into_iter()
            .map(|s| VmInfo {
                vm_id: s.vm_id,
                name: s.name,
                power_state: PowerState::from_api_str(&s.power_state),
                guest_os: if s.guest_os.is_empty() {
                    "Unknown".to_string()
                } else {
                    s.guest_os
                },
                num_cpus: s.cpu_count.unwrap_or(0),
                memory_mb: s.memory_size_mib.unwrap_or(0),
            })
            .collect();

        assert_eq!(vms[0].vm_id, "vm-42");
        assert_eq!(vms[0].name, "web-server-01");
        assert_eq!(vms[0].power_state, PowerState::PoweredOn);
        assert_eq!(vms[0].guest_os, "UBUNTU_64");
        assert_eq!(vms[0].num_cpus, 4);
        assert_eq!(vms[0].memory_mb, 8192);

        assert_eq!(vms[1].vm_id, "vm-43");
        assert_eq!(vms[1].name, "db-server-01");
        assert_eq!(vms[1].power_state, PowerState::PoweredOff);
        assert_eq!(vms[1].guest_os, "RHEL_8_64");
        assert_eq!(vms[1].num_cpus, 8);
        assert_eq!(vms[1].memory_mb, 16384);
    }

    #[test]
    fn test_parse_vm_list_minimal_fields() {
        // The API may omit optional fields
        let json = r#"[
            {
                "vm": "vm-1",
                "name": "minimal",
                "power_state": "POWERED_OFF"
            }
        ]"#;

        let summaries: Vec<VmSummaryJson> = serde_json::from_str(json).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].vm_id, "vm-1");
        assert_eq!(summaries[0].guest_os, "");
        assert_eq!(summaries[0].cpu_count, None);
        assert_eq!(summaries[0].memory_size_mib, None);
    }

    #[test]
    fn test_parse_vm_detail_response() {
        let json = r#"{
            "name": "web-server-01",
            "guest_OS": "UBUNTU_64",
            "power_state": "POWERED_ON",
            "cpu": {"count": 4},
            "memory": {"size_MiB": 8192},
            "identity": {"ip_address": "10.0.1.50"},
            "host": "host-10",
            "disks": {
                "2000": {
                    "label": "Hard disk 1",
                    "capacity": 107374182400
                },
                "2001": {
                    "label": "Hard disk 2",
                    "capacity": 53687091200
                }
            },
            "nics": {
                "4000": {
                    "label": "Network adapter 1",
                    "mac_address": "00:50:56:ab:cd:ef",
                    "backing": {
                        "network": "network-15",
                        "type": "STANDARD_PORTGROUP"
                    }
                }
            }
        }"#;

        let detail: VmDetailJson = serde_json::from_str(json).unwrap();
        assert_eq!(detail.name.as_deref(), Some("web-server-01"));
        assert_eq!(detail.guest_os.as_deref(), Some("UBUNTU_64"));
        assert_eq!(detail.power_state.as_deref(), Some("POWERED_ON"));
        assert_eq!(detail.cpu.as_ref().unwrap().count, Some(4));
        assert_eq!(detail.memory.as_ref().unwrap().size_mib, Some(8192));
        assert_eq!(
            detail.identity.as_ref().unwrap().ip_address.as_deref(),
            Some("10.0.1.50")
        );
        assert_eq!(detail.host.as_deref(), Some("host-10"));

        let disks = VSphereClient::parse_disks(&detail.disks);
        assert_eq!(disks.len(), 2);
        // Disk order may vary since it's a map; check both exist
        let labels: Vec<&str> = disks.iter().map(|d| d.label.as_str()).collect();
        assert!(labels.contains(&"Hard disk 1"));
        assert!(labels.contains(&"Hard disk 2"));

        let nics = VSphereClient::parse_nics(&detail.nics);
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0].label, "Network adapter 1");
        assert_eq!(nics[0].mac_address, "00:50:56:ab:cd:ef");
        assert_eq!(nics[0].network.as_deref(), Some("network-15"));
    }

    #[test]
    fn test_parse_vm_detail_minimal() {
        let json = r#"{}"#;

        let detail: VmDetailJson = serde_json::from_str(json).unwrap();
        assert!(detail.name.is_none());
        assert!(detail.guest_os.is_none());
        assert!(detail.power_state.is_none());
        assert!(detail.cpu.is_none());
        assert!(detail.memory.is_none());
        assert!(detail.identity.is_none());
        assert!(detail.host.is_none());

        let disks = VSphereClient::parse_disks(&detail.disks);
        assert!(disks.is_empty());

        let nics = VSphereClient::parse_nics(&detail.nics);
        assert!(nics.is_empty());
    }

    #[test]
    fn test_parse_console_ticket_response() {
        let json = r#"{
            "ticket": "cst-VCT-52067f42-7e60-2c44-e87a-))VMware-):f))VM-cert",
            "host": "esxi-01.example.com",
            "port": 443,
            "ssl_thumbprint": "AB:CD:EF:12:34:56:78:90:AB:CD:EF:12:34:56:78:90:AB:CD:EF:12"
        }"#;

        let ticket: ConsoleTicketJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            ticket.ticket,
            "cst-VCT-52067f42-7e60-2c44-e87a-))VMware-):f))VM-cert"
        );
        assert_eq!(ticket.host, "esxi-01.example.com");
        assert_eq!(ticket.port, 443);
        assert_eq!(
            ticket.ssl_thumbprint.as_deref(),
            Some("AB:CD:EF:12:34:56:78:90:AB:CD:EF:12:34:56:78:90:AB:CD:EF:12")
        );
    }

    #[test]
    fn test_parse_console_ticket_no_thumbprint() {
        let json = r#"{
            "ticket": "ticket-abc-123",
            "host": "esxi-02.local",
            "port": 902
        }"#;

        let ticket: ConsoleTicketJson = serde_json::from_str(json).unwrap();
        assert_eq!(ticket.ticket, "ticket-abc-123");
        assert_eq!(ticket.host, "esxi-02.local");
        assert_eq!(ticket.port, 902);
        assert!(ticket.ssl_thumbprint.is_none());
    }

    #[test]
    fn test_parse_host_list_response() {
        let json = r#"[
            {
                "host": "host-10",
                "name": "esxi-01.example.com",
                "connection_state": "CONNECTED",
                "power_state": "POWERED_ON"
            },
            {
                "host": "host-11",
                "name": "esxi-02.example.com",
                "connection_state": "DISCONNECTED",
                "power_state": "POWERED_OFF"
            }
        ]"#;

        let summaries: Vec<HostSummaryJson> = serde_json::from_str(json).unwrap();
        assert_eq!(summaries.len(), 2);

        let hosts: Vec<HostInfo> = summaries
            .into_iter()
            .map(|h| HostInfo {
                host_id: h.host_id,
                name: h.name,
                connection_state: h.connection_state,
                power_state: h.power_state,
            })
            .collect();

        assert_eq!(hosts[0].host_id, "host-10");
        assert_eq!(hosts[0].name, "esxi-01.example.com");
        assert_eq!(hosts[0].connection_state, "CONNECTED");
        assert_eq!(hosts[0].power_state, "POWERED_ON");

        assert_eq!(hosts[1].host_id, "host-11");
        assert_eq!(hosts[1].name, "esxi-02.example.com");
        assert_eq!(hosts[1].connection_state, "DISCONNECTED");
        assert_eq!(hosts[1].power_state, "POWERED_OFF");
    }

    // -- Disk/NIC parsing edge cases --

    #[test]
    fn test_parse_disks_empty_map() {
        let val = Some(serde_json::json!({}));
        let disks = VSphereClient::parse_disks(&val);
        assert!(disks.is_empty());
    }

    #[test]
    fn test_parse_disks_none() {
        let disks = VSphereClient::parse_disks(&None);
        assert!(disks.is_empty());
    }

    #[test]
    fn test_parse_disks_non_object() {
        let val = Some(serde_json::json!("not an object"));
        let disks = VSphereClient::parse_disks(&val);
        assert!(disks.is_empty());
    }

    #[test]
    fn test_parse_disks_missing_fields() {
        let val = Some(serde_json::json!({
            "2000": {}
        }));
        let disks = VSphereClient::parse_disks(&val);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].label, "Unknown");
        assert_eq!(disks[0].capacity, 0);
    }

    #[test]
    fn test_parse_nics_empty_map() {
        let val = Some(serde_json::json!({}));
        let nics = VSphereClient::parse_nics(&val);
        assert!(nics.is_empty());
    }

    #[test]
    fn test_parse_nics_none() {
        let nics = VSphereClient::parse_nics(&None);
        assert!(nics.is_empty());
    }

    #[test]
    fn test_parse_nics_no_backing() {
        let val = Some(serde_json::json!({
            "4000": {
                "label": "Network adapter 1",
                "mac_address": "00:50:56:00:00:01"
            }
        }));
        let nics = VSphereClient::parse_nics(&val);
        assert_eq!(nics.len(), 1);
        assert_eq!(nics[0].label, "Network adapter 1");
        assert_eq!(nics[0].mac_address, "00:50:56:00:00:01");
        assert!(nics[0].network.is_none());
    }

    // -- Client state tests --

    #[test]
    fn test_is_authenticated() {
        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vc.local".to_string(),
            session_id: Some("session-123".to_string()),
        };
        assert!(client.is_authenticated());

        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vc.local".to_string(),
            session_id: None,
        };
        assert!(!client.is_authenticated());
    }

    #[test]
    fn test_base_url() {
        let client = VSphereClient {
            client: reqwest::Client::new(),
            base_url: "https://vcenter.example.com".to_string(),
            session_id: None,
        };
        assert_eq!(client.base_url(), "https://vcenter.example.com");
    }

    // -- Error handling tests --

    #[test]
    fn test_parse_vm_list_invalid_json() {
        let json = r#"not valid json"#;
        let result: Result<Vec<VmSummaryJson>, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_vm_detail_invalid_json() {
        let json = r#"[1, 2, 3]"#;
        let result: Result<VmDetailJson, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_console_ticket_missing_required() {
        // Missing "port" should fail
        let json = r#"{
            "ticket": "abc",
            "host": "esxi.local"
        }"#;
        let result: Result<ConsoleTicketJson, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_host_list_empty() {
        let json = r#"[]"#;
        let summaries: Vec<HostSummaryJson> = serde_json::from_str(json).unwrap();
        assert!(summaries.is_empty());
    }
}
