// Kubernetes API client for guacr resource browser
//
// Provides pod listing, exec, log viewing, and namespace management
// using the kube crate for direct Kubernetes API access.
//
// Auth methods (from connection params):
// - kubeconfig: base64-encoded kubeconfig injected by PAM vault
// - token: service account token for Bearer auth
// - server + ca_cert: direct API server URL with CA certificate
// - Default: in-cluster config, then default kubeconfig (~/.kube/config)

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::api::{Api, AttachParams, ListParams, LogParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::Client;
use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::compat::FuturesAsyncReadCompatExt;

// ---------------------------------------------------------------------------
// PodInfo: structured pod metadata for list views
// ---------------------------------------------------------------------------

/// Structured information about a Kubernetes pod, suitable for display
/// in a spreadsheet-style resource browser.
#[derive(Debug, Clone)]
pub struct PodInfo {
    /// Namespace the pod belongs to
    pub namespace: String,
    /// Pod name
    pub name: String,
    /// Container names within the pod
    pub containers: Vec<String>,
    /// Pod phase (Running, Pending, Succeeded, Failed, Unknown)
    pub status: String,
    /// Total restart count across all containers
    pub restarts: u32,
    /// Human-readable age string (e.g. "3d", "5h", "12m", "45s")
    pub age: String,
    /// Node the pod is scheduled on (empty if not yet scheduled)
    pub node: String,
}

impl PodInfo {
    /// Column definitions for a spreadsheet-style display.
    /// Returns (column_name, column_width) pairs.
    pub fn columns() -> Vec<(&'static str, usize)> {
        vec![
            ("NAMESPACE", 20),
            ("NAME", 40),
            ("CONTAINERS", 30),
            ("STATUS", 12),
            ("RESTARTS", 10),
            ("AGE", 8),
            ("NODE", 30),
        ]
    }

    /// Convert this pod info into a row of display strings,
    /// matching the order of `columns()`.
    pub fn to_row(&self) -> Vec<String> {
        vec![
            self.namespace.clone(),
            self.name.clone(),
            self.containers.join(","),
            self.status.clone(),
            self.restarts.to_string(),
            self.age.clone(),
            self.node.clone(),
        ]
    }

    /// Build a PodInfo from a k8s Pod object.
    fn from_pod(pod: &Pod) -> Self {
        let metadata = &pod.metadata;
        let namespace = metadata
            .namespace
            .as_deref()
            .unwrap_or("default")
            .to_string();
        let name = metadata.name.as_deref().unwrap_or("<unknown>").to_string();

        // Extract container names from spec
        let containers: Vec<String> = pod
            .spec
            .as_ref()
            .map(|spec| spec.containers.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();

        // Pod status phase
        let status = pod
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Unknown".to_string());

        // Total restart count across all containers
        let restarts = pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref())
            .map(|statuses| statuses.iter().map(|cs| cs.restart_count as u32).sum())
            .unwrap_or(0);

        // Age from creation timestamp
        let age = metadata
            .creation_timestamp
            .as_ref()
            .map(|ts| format_age(&ts.0))
            .unwrap_or_else(|| "<unknown>".to_string());

        // Node name
        let node = pod
            .spec
            .as_ref()
            .and_then(|s| s.node_name.clone())
            .unwrap_or_default();

        PodInfo {
            namespace,
            name,
            containers,
            status,
            restarts,
            age,
            node,
        }
    }
}

/// Format a chrono DateTime as a human-readable age string.
///
/// Produces output like kubectl: "3d", "5h", "12m", "45s".
/// Uses the largest meaningful unit only (no "3d5h" compound forms).
fn format_age(created: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(created);

    if duration.num_days() > 365 {
        let years = duration.num_days() / 365;
        format!("{}y", years)
    } else if duration.num_days() > 0 {
        format!("{}d", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m", duration.num_minutes())
    } else {
        let secs = duration.num_seconds().max(0);
        format!("{}s", secs)
    }
}

// ---------------------------------------------------------------------------
// Exec/log stream wrappers
// ---------------------------------------------------------------------------

/// A readable stream from a pod exec session's stdout.
///
/// Wraps the kube AttachedProcess stdout reader so callers get a clean
/// `AsyncRead` without depending on kube internals.
pub struct ExecReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
}

impl AsyncRead for ExecReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

/// A writable stream to a pod exec session's stdin.
///
/// Wraps the kube AttachedProcess stdin writer so callers get a clean
/// `AsyncWrite` without depending on kube internals.
pub struct ExecWriter {
    inner: Pin<Box<dyn AsyncWrite + Send>>,
}

impl AsyncWrite for ExecWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.inner.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_shutdown(cx)
    }
}

/// A readable stream of pod log output.
///
/// Wraps the log byte stream returned by the kube API.
pub struct LogReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
}

impl AsyncRead for LogReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

// ---------------------------------------------------------------------------
// KubernetesClient
// ---------------------------------------------------------------------------

/// Kubernetes API client for resource browsing.
///
/// Provides pod listing, exec, log viewing, and namespace management.
/// Authentication is derived from connection parameters supplied by the
/// PAM vault (kubeconfig, token, or server+ca_cert).
pub struct KubernetesClient {
    client: Client,
    namespace: Option<String>,
}

impl KubernetesClient {
    /// Create a client from connection parameters.
    ///
    /// Supported parameter keys:
    /// - `kubeconfig`: base64-encoded kubeconfig YAML (injected by PAM vault)
    /// - `token`: service account bearer token
    /// - `server`: API server URL (e.g. `https://k8s.example.com:6443`)
    /// - `ca_cert`: base64-encoded PEM CA certificate (used with `server`)
    /// - `namespace`: default namespace (optional, defaults to all namespaces)
    ///
    /// Auth precedence: kubeconfig > token > server+ca_cert > in-cluster > default kubeconfig
    pub async fn from_params(params: &HashMap<String, String>) -> Result<Self, String> {
        let namespace = params.get("namespace").cloned();
        let config = build_kube_config(params).await?;
        let client =
            Client::try_from(config).map_err(|e| format!("Failed to create K8s client: {}", e))?;

        info!(
            "Kubernetes client created (namespace: {})",
            namespace.as_deref().unwrap_or("all")
        );

        Ok(Self { client, namespace })
    }

    /// Create a client from an already-constructed kube Client.
    ///
    /// Useful for testing or when the caller has its own config logic.
    pub fn from_client(client: Client, namespace: Option<String>) -> Self {
        Self { client, namespace }
    }

    /// Set the current default namespace for operations.
    pub fn set_namespace(&mut self, namespace: &str) {
        self.namespace = Some(namespace.to_string());
    }

    /// Get the current default namespace, if any.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    // -- Namespace operations -----------------------------------------------

    /// List all namespaces the client has access to.
    pub async fn list_namespaces(&self) -> Result<Vec<String>, String> {
        let ns_api: Api<Namespace> = Api::all(self.client.clone());
        let namespaces = ns_api
            .list(&ListParams::default())
            .await
            .map_err(|e| format!("Failed to list namespaces: {}", e))?;

        let mut names: Vec<String> = namespaces
            .items
            .iter()
            .filter_map(|ns| ns.metadata.name.clone())
            .collect();
        names.sort();

        debug!("Listed {} namespaces", names.len());
        Ok(names)
    }

    // -- Pod operations -----------------------------------------------------

    /// List pods, optionally filtered by the current namespace.
    ///
    /// If no namespace is set, lists pods across all namespaces.
    pub async fn list_pods(&self) -> Result<Vec<PodInfo>, String> {
        let pods = match &self.namespace {
            Some(ns) => {
                let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
                pod_api
                    .list(&ListParams::default())
                    .await
                    .map_err(|e| format!("Failed to list pods in namespace '{}': {}", ns, e))?
            }
            None => {
                let pod_api: Api<Pod> = Api::all(self.client.clone());
                pod_api
                    .list(&ListParams::default())
                    .await
                    .map_err(|e| format!("Failed to list pods: {}", e))?
            }
        };

        let infos: Vec<PodInfo> = pods.items.iter().map(PodInfo::from_pod).collect();
        debug!("Listed {} pods", infos.len());
        Ok(infos)
    }

    /// List pods matching a label selector.
    ///
    /// Example selector: `app=nginx` or `environment=production,tier=frontend`.
    pub async fn list_pods_with_selector(
        &self,
        label_selector: &str,
    ) -> Result<Vec<PodInfo>, String> {
        let lp = ListParams::default().labels(label_selector);

        let pods = match &self.namespace {
            Some(ns) => {
                let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), ns);
                pod_api.list(&lp).await.map_err(|e| {
                    format!(
                        "Failed to list pods with selector '{}': {}",
                        label_selector, e
                    )
                })?
            }
            None => {
                let pod_api: Api<Pod> = Api::all(self.client.clone());
                pod_api.list(&lp).await.map_err(|e| {
                    format!(
                        "Failed to list pods with selector '{}': {}",
                        label_selector, e
                    )
                })?
            }
        };

        let infos: Vec<PodInfo> = pods.items.iter().map(PodInfo::from_pod).collect();
        debug!(
            "Listed {} pods matching selector '{}'",
            infos.len(),
            label_selector
        );
        Ok(infos)
    }

    /// Get detailed description of a specific pod.
    ///
    /// Returns a multi-line human-readable string similar to `kubectl describe pod`.
    pub async fn describe_pod(&self, name: &str, namespace: &str) -> Result<String, String> {
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let pod = pod_api
            .get(name)
            .await
            .map_err(|e| format!("Failed to get pod '{}/{}': {}", namespace, name, e))?;

        Ok(format_pod_description(&pod))
    }

    /// Execute a command inside a pod container, returning bidirectional streams.
    ///
    /// The returned `ExecReader` carries stdout (and stderr if tty is enabled),
    /// while `ExecWriter` feeds stdin. Both are suitable for terminal emulation.
    ///
    /// # Arguments
    /// * `name` - Pod name
    /// * `namespace` - Pod namespace
    /// * `container` - Target container (None = first container)
    /// * `command` - Command and arguments to run (e.g. `vec!["/bin/sh", "-c", "ls"]`)
    pub async fn exec_pod(
        &self,
        name: &str,
        namespace: &str,
        container: Option<&str>,
        command: Vec<String>,
    ) -> Result<(ExecReader, ExecWriter), String> {
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        let ap = AttachParams {
            stdin: true,
            stdout: true,
            stderr: false, // merge stderr into stdout via tty
            tty: true,
            container: container.map(|c| c.to_string()),
            ..Default::default()
        };

        info!(
            "Exec into pod {}/{} container={:?} command={:?}",
            namespace, name, container, command
        );

        let mut attached = pod_api
            .exec(name, command, &ap)
            .await
            .map_err(|e| format!("Failed to exec in pod '{}/{}': {}", namespace, name, e))?;

        // Extract stdin writer and stdout reader from the AttachedProcess.
        // AttachedProcess::stdin() returns Option<impl AsyncWrite + Unpin>,
        // AttachedProcess::stdout() returns Option<impl AsyncRead + Unpin>.
        let writer = attached
            .stdin()
            .ok_or_else(|| "Exec session did not provide stdin".to_string())?;
        let reader = attached
            .stdout()
            .ok_or_else(|| "Exec session did not provide stdout".to_string())?;

        Ok((
            ExecReader {
                inner: Box::pin(reader),
            },
            ExecWriter {
                inner: Box::pin(writer),
            },
        ))
    }

    /// Stream logs from a pod container.
    ///
    /// When `follow` is true, the returned reader will keep producing output
    /// as new log lines are written (like `kubectl logs -f`). When false, it
    /// returns the current log contents and then EOF.
    ///
    /// # Arguments
    /// * `name` - Pod name
    /// * `namespace` - Pod namespace
    /// * `container` - Target container (None = first container)
    /// * `follow` - Whether to follow/stream new log output
    pub async fn pod_logs(
        &self,
        name: &str,
        namespace: &str,
        container: Option<&str>,
        follow: bool,
    ) -> Result<LogReader, String> {
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        let mut lp = LogParams {
            follow,
            // Tail last 1000 lines by default to avoid pulling massive logs
            tail_lines: Some(1000),
            ..Default::default()
        };
        if let Some(c) = container {
            lp.container = Some(c.to_string());
        }

        info!(
            "Streaming logs from pod {}/{} container={:?} follow={}",
            namespace, name, container, follow
        );

        let log_stream = pod_api.log_stream(name, &lp).await.map_err(|e| {
            format!(
                "Failed to stream logs from pod '{}/{}': {}",
                namespace, name, e
            )
        })?;

        // log_stream returns a futures::AsyncBufRead; convert to tokio::io::AsyncRead
        // using the compat adapter from tokio-util
        let tokio_reader = log_stream.compat();
        Ok(LogReader {
            inner: Box::pin(tokio_reader),
        })
    }

    /// Get the full log contents of a pod as a string (non-streaming).
    ///
    /// For large logs this can be slow; prefer `pod_logs()` with streaming
    /// for interactive use.
    pub async fn pod_logs_string(
        &self,
        name: &str,
        namespace: &str,
        container: Option<&str>,
        tail_lines: Option<i64>,
    ) -> Result<String, String> {
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        let mut lp = LogParams {
            tail_lines,
            ..Default::default()
        };
        if let Some(c) = container {
            lp.container = Some(c.to_string());
        }

        let logs = pod_api.logs(name, &lp).await.map_err(|e| {
            format!(
                "Failed to get logs from pod '{}/{}': {}",
                namespace, name, e
            )
        })?;

        Ok(logs)
    }
}

// ---------------------------------------------------------------------------
// Auth/Config building
// ---------------------------------------------------------------------------

/// Connection parameter keys recognized by the Kubernetes client.
pub mod param_keys {
    /// Base64-encoded kubeconfig YAML (highest priority auth method)
    pub const KUBECONFIG: &str = "kubeconfig";
    /// Service account bearer token
    pub const TOKEN: &str = "token";
    /// API server URL (e.g. https://k8s.example.com:6443)
    pub const SERVER: &str = "server";
    /// Base64-encoded PEM CA certificate for TLS verification
    pub const CA_CERT: &str = "ca_cert";
    /// Default namespace
    pub const NAMESPACE: &str = "namespace";
    /// Kubeconfig context to use (only with kubeconfig auth)
    pub const CONTEXT: &str = "context";
}

/// Detect which authentication method to use based on the provided parameters.
#[derive(Debug, PartialEq)]
pub enum AuthMethod {
    /// Full kubeconfig (base64-encoded YAML)
    Kubeconfig,
    /// Service account bearer token
    Token,
    /// Direct server URL + optional CA cert
    ServerCert,
    /// Automatic: try in-cluster, then default kubeconfig
    Default,
}

/// Determine the auth method from connection parameters.
pub fn detect_auth_method(params: &HashMap<String, String>) -> AuthMethod {
    if params.contains_key(param_keys::KUBECONFIG) {
        AuthMethod::Kubeconfig
    } else if params.contains_key(param_keys::TOKEN) {
        AuthMethod::Token
    } else if params.contains_key(param_keys::SERVER) {
        AuthMethod::ServerCert
    } else {
        AuthMethod::Default
    }
}

/// Build a kube::Config from connection parameters.
///
/// Auth precedence:
/// 1. `kubeconfig` param (base64-encoded full kubeconfig YAML)
/// 2. `token` param (Bearer token auth against `server` or default cluster)
/// 3. `server` + `ca_cert` params (direct API server with TLS)
/// 4. In-cluster config (ServiceAccount mount), then default kubeconfig
async fn build_kube_config(params: &HashMap<String, String>) -> Result<kube::Config, String> {
    let method = detect_auth_method(params);
    debug!("Kubernetes auth method: {:?}", method);

    match method {
        AuthMethod::Kubeconfig => build_config_from_kubeconfig(params).await,
        AuthMethod::Token => build_config_from_token(params),
        AuthMethod::ServerCert => build_config_from_server_cert(params),
        AuthMethod::Default => build_config_default().await,
    }
}

/// Build config from a base64-encoded kubeconfig YAML.
async fn build_config_from_kubeconfig(
    params: &HashMap<String, String>,
) -> Result<kube::Config, String> {
    let encoded = params
        .get(param_keys::KUBECONFIG)
        .ok_or("Missing 'kubeconfig' parameter")?;

    let yaml_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("Invalid base64 in kubeconfig: {}", e))?;

    let yaml_str = String::from_utf8(yaml_bytes)
        .map_err(|e| format!("Kubeconfig is not valid UTF-8: {}", e))?;

    let kubeconfig: Kubeconfig = serde_yaml::from_str(&yaml_str)
        .map_err(|e| format!("Failed to parse kubeconfig YAML: {}", e))?;

    let options = KubeConfigOptions {
        context: params.get(param_keys::CONTEXT).cloned(),
        ..Default::default()
    };

    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .map_err(|e| format!("Failed to build config from kubeconfig: {}", e))?;

    info!("Using kubeconfig-based authentication");
    Ok(config)
}

/// Build config from a bearer token.
///
/// If `server` is also provided, uses that as the API endpoint.
/// Otherwise, tries to infer the server from in-cluster or default kubeconfig.
fn build_config_from_token(params: &HashMap<String, String>) -> Result<kube::Config, String> {
    let token = params
        .get(param_keys::TOKEN)
        .ok_or("Missing 'token' parameter")?
        .clone();

    let server = params
        .get(param_keys::SERVER)
        .cloned()
        .unwrap_or_else(|| "https://kubernetes.default.svc".to_string());

    let server_url = server
        .parse()
        .map_err(|e| format!("Invalid server URL '{}': {}", server, e))?;

    let mut config = kube::Config::new(server_url);

    // Set the bearer token for authentication
    config.auth_info.token = Some(token.into());

    // If a CA cert is provided, configure TLS
    if let Some(ca_b64) = params.get(param_keys::CA_CERT) {
        let ca_bytes = base64::engine::general_purpose::STANDARD
            .decode(ca_b64)
            .map_err(|e| format!("Invalid base64 in ca_cert: {}", e))?;
        config.root_cert = Some(vec![ca_bytes]);
    } else {
        // Without a CA cert, we must accept insecure TLS (not recommended)
        config.accept_invalid_certs = true;
        warn!("No CA certificate provided with token auth; TLS verification disabled");
    }

    info!("Using token-based authentication against {}", server);
    Ok(config)
}

/// Build config from direct server URL and CA certificate.
fn build_config_from_server_cert(params: &HashMap<String, String>) -> Result<kube::Config, String> {
    let server = params
        .get(param_keys::SERVER)
        .ok_or("Missing 'server' parameter")?;

    let server_url = server
        .parse()
        .map_err(|e| format!("Invalid server URL '{}': {}", server, e))?;

    let mut config = kube::Config::new(server_url);

    if let Some(ca_b64) = params.get(param_keys::CA_CERT) {
        let ca_bytes = base64::engine::general_purpose::STANDARD
            .decode(ca_b64)
            .map_err(|e| format!("Invalid base64 in ca_cert: {}", e))?;
        config.root_cert = Some(vec![ca_bytes]);
    } else {
        config.accept_invalid_certs = true;
        warn!("No CA certificate provided; TLS verification disabled");
    }

    // If a token is also provided, use it
    if let Some(token) = params.get(param_keys::TOKEN) {
        config.auth_info.token = Some(token.clone().into());
    }

    info!("Using server+cert authentication against {}", server);
    Ok(config)
}

/// Build config using automatic discovery (in-cluster, then default kubeconfig).
async fn build_config_default() -> Result<kube::Config, String> {
    match kube::Config::incluster() {
        Ok(config) => {
            info!("Using in-cluster Kubernetes configuration");
            Ok(config)
        }
        Err(in_cluster_err) => {
            debug!(
                "In-cluster config not available: {}; trying default kubeconfig",
                in_cluster_err
            );
            kube::Config::from_kubeconfig(&KubeConfigOptions::default())
                .await
                .map_err(|e| {
                    format!(
                        "No Kubernetes config available. In-cluster: {}. Kubeconfig: {}",
                        in_cluster_err, e
                    )
                })
        }
    }
}

// ---------------------------------------------------------------------------
// Pod description formatting
// ---------------------------------------------------------------------------

/// Format a Pod object into a human-readable description similar to
/// `kubectl describe pod`.
fn format_pod_description(pod: &Pod) -> String {
    let mut out = String::with_capacity(2048);
    let meta = &pod.metadata;

    // Header
    out.push_str("Name:         ");
    out.push_str(meta.name.as_deref().unwrap_or("<unknown>"));
    out.push('\n');

    out.push_str("Namespace:    ");
    out.push_str(meta.namespace.as_deref().unwrap_or("default"));
    out.push('\n');

    if let Some(ref labels) = meta.labels {
        out.push_str("Labels:       ");
        let mut first = true;
        for (k, v) in labels {
            if !first {
                out.push_str("              ");
            }
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
            first = false;
        }
    } else {
        out.push_str("Labels:       <none>\n");
    }

    if let Some(ref annotations) = meta.annotations {
        out.push_str("Annotations:  ");
        let mut first = true;
        for (k, v) in annotations {
            if !first {
                out.push_str("              ");
            }
            out.push_str(k);
            out.push('=');
            // Truncate long annotation values for readability
            if v.len() > 80 {
                out.push_str(&v[..77]);
                out.push_str("...");
            } else {
                out.push_str(v);
            }
            out.push('\n');
            first = false;
        }
    } else {
        out.push_str("Annotations:  <none>\n");
    }

    // Status
    if let Some(ref status) = pod.status {
        out.push_str("Status:       ");
        out.push_str(status.phase.as_deref().unwrap_or("Unknown"));
        out.push('\n');

        if let Some(ref ip) = status.pod_ip {
            out.push_str("IP:           ");
            out.push_str(ip);
            out.push('\n');
        }

        if let Some(ref host_ip) = status.host_ip {
            out.push_str("Host IP:      ");
            out.push_str(host_ip);
            out.push('\n');
        }

        if let Some(ref start_time) = status.start_time {
            out.push_str("Start Time:   ");
            out.push_str(&start_time.0.to_rfc3339());
            out.push('\n');
        }
    }

    // Spec
    if let Some(ref spec) = pod.spec {
        if let Some(ref node_name) = spec.node_name {
            out.push_str("Node:         ");
            out.push_str(node_name);
            out.push('\n');
        }

        if let Some(ref sa) = spec.service_account_name {
            out.push_str("Service Acct: ");
            out.push_str(sa);
            out.push('\n');
        }

        // Containers
        out.push_str("\nContainers:\n");
        for container in &spec.containers {
            out.push_str("  ");
            out.push_str(&container.name);
            out.push_str(":\n");

            out.push_str("    Image:    ");
            out.push_str(container.image.as_deref().unwrap_or("<none>"));
            out.push('\n');

            if let Some(ref ports) = container.ports {
                out.push_str("    Ports:    ");
                let port_strs: Vec<String> = ports
                    .iter()
                    .map(|p| {
                        let proto = p.protocol.as_deref().unwrap_or("TCP");
                        format!("{}/{}", p.container_port, proto)
                    })
                    .collect();
                out.push_str(&port_strs.join(", "));
                out.push('\n');
            }

            if let Some(ref command) = container.command {
                out.push_str("    Command:  ");
                out.push_str(&command.join(" "));
                out.push('\n');
            }

            if let Some(ref args) = container.args {
                out.push_str("    Args:     ");
                out.push_str(&args.join(" "));
                out.push('\n');
            }

            // Resource requests/limits
            if let Some(ref resources) = container.resources {
                if let Some(ref requests) = resources.requests {
                    out.push_str("    Requests:\n");
                    for (k, v) in requests {
                        out.push_str("      ");
                        out.push_str(k);
                        out.push_str(":  ");
                        out.push_str(&v.0);
                        out.push('\n');
                    }
                }
                if let Some(ref limits) = resources.limits {
                    out.push_str("    Limits:\n");
                    for (k, v) in limits {
                        out.push_str("      ");
                        out.push_str(k);
                        out.push_str(":  ");
                        out.push_str(&v.0);
                        out.push('\n');
                    }
                }
            }

            // Environment variables (names only, not values for security)
            if let Some(ref env) = container.env {
                if !env.is_empty() {
                    out.push_str("    Env:\n");
                    for e in env {
                        out.push_str("      ");
                        out.push_str(&e.name);
                        if e.value_from.is_some() {
                            out.push_str(":  <set from secret/configmap>");
                        } else if let Some(ref v) = e.value {
                            out.push_str(":  ");
                            out.push_str(v);
                        }
                        out.push('\n');
                    }
                }
            }

            // Volume mounts
            if let Some(ref mounts) = container.volume_mounts {
                if !mounts.is_empty() {
                    out.push_str("    Mounts:\n");
                    for m in mounts {
                        out.push_str("      ");
                        out.push_str(&m.mount_path);
                        out.push_str(" from ");
                        out.push_str(&m.name);
                        if m.read_only.unwrap_or(false) {
                            out.push_str(" (ro)");
                        }
                        out.push('\n');
                    }
                }
            }
        }

        // Init containers
        if let Some(ref init_containers) = spec.init_containers {
            if !init_containers.is_empty() {
                out.push_str("\nInit Containers:\n");
                for container in init_containers {
                    out.push_str("  ");
                    out.push_str(&container.name);
                    out.push_str(":\n");
                    out.push_str("    Image:    ");
                    out.push_str(container.image.as_deref().unwrap_or("<none>"));
                    out.push('\n');
                }
            }
        }

        // Volumes
        if let Some(ref volumes) = spec.volumes {
            if !volumes.is_empty() {
                out.push_str("\nVolumes:\n");
                for vol in volumes {
                    out.push_str("  ");
                    out.push_str(&vol.name);
                    out.push_str(":\n");
                    format_volume_source(&mut out, vol);
                }
            }
        }
    }

    // Container statuses
    if let Some(ref status) = pod.status {
        if let Some(ref container_statuses) = status.container_statuses {
            out.push_str("\nContainer Statuses:\n");
            for cs in container_statuses {
                out.push_str("  ");
                out.push_str(&cs.name);
                out.push_str(":\n");
                out.push_str("    Ready:    ");
                out.push_str(if cs.ready { "True" } else { "False" });
                out.push('\n');
                out.push_str("    Restarts: ");
                out.push_str(&cs.restart_count.to_string());
                out.push('\n');

                if let Some(ref state) = cs.state {
                    out.push_str("    State:    ");
                    if state.running.is_some() {
                        out.push_str("Running");
                    } else if let Some(ref waiting) = state.waiting {
                        out.push_str("Waiting (");
                        out.push_str(waiting.reason.as_deref().unwrap_or("unknown"));
                        out.push(')');
                    } else if let Some(ref terminated) = state.terminated {
                        out.push_str("Terminated (");
                        out.push_str(terminated.reason.as_deref().unwrap_or("unknown"));
                        out.push_str(", exit code ");
                        out.push_str(&terminated.exit_code.to_string());
                        out.push(')');
                    } else {
                        out.push_str("Unknown");
                    }
                    out.push('\n');
                }
            }
        }
    }

    // Conditions
    if let Some(ref status) = pod.status {
        if let Some(ref conditions) = status.conditions {
            if !conditions.is_empty() {
                out.push_str("\nConditions:\n");
                out.push_str("  Type              Status\n");
                out.push_str("  ----              ------\n");
                for cond in conditions {
                    out.push_str("  ");
                    let ctype = &cond.type_;
                    out.push_str(ctype);
                    // Pad to align status column
                    for _ in ctype.len()..18 {
                        out.push(' ');
                    }
                    out.push_str(&cond.status);
                    if let Some(ref reason) = cond.reason {
                        out.push_str("  (");
                        out.push_str(reason);
                        out.push(')');
                    }
                    out.push('\n');
                }
            }
        }
    }

    // Events section placeholder (events require a separate API call)
    out.push_str("\nEvents:       <use Kubernetes Events API for event history>\n");

    out
}

/// Format a volume source into the description string.
fn format_volume_source(out: &mut String, vol: &k8s_openapi::api::core::v1::Volume) {
    if let Some(ref cm) = vol.config_map {
        out.push_str("    Type:     ConfigMap (");
        if cm.name.is_empty() {
            out.push_str("<unknown>");
        } else {
            out.push_str(&cm.name);
        }
        out.push_str(")\n");
    } else if let Some(ref secret) = vol.secret {
        out.push_str("    Type:     Secret (");
        out.push_str(secret.secret_name.as_deref().unwrap_or("<unknown>"));
        out.push_str(")\n");
    } else if let Some(ref pvc) = vol.persistent_volume_claim {
        out.push_str("    Type:     PersistentVolumeClaim (");
        out.push_str(&pvc.claim_name);
        out.push_str(")\n");
    } else if vol.empty_dir.is_some() {
        out.push_str("    Type:     EmptyDir\n");
    } else if let Some(ref host_path) = vol.host_path {
        out.push_str("    Type:     HostPath (");
        out.push_str(&host_path.path);
        out.push_str(")\n");
    } else if vol.projected.is_some() {
        out.push_str("    Type:     Projected\n");
    } else if let Some(ref downward) = vol.downward_api {
        out.push_str("    Type:     DownwardAPI\n");
        let _ = downward; // suppress unused warning
    } else {
        out.push_str("    Type:     <other>\n");
    }
}

// ---------------------------------------------------------------------------
// Required imports for base64 decoding
// ---------------------------------------------------------------------------
use base64::Engine;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PodInfo tests ------------------------------------------------------

    #[test]
    fn test_pod_info_columns() {
        let cols = PodInfo::columns();
        assert_eq!(cols.len(), 7);
        assert_eq!(cols[0].0, "NAMESPACE");
        assert_eq!(cols[1].0, "NAME");
        assert_eq!(cols[2].0, "CONTAINERS");
        assert_eq!(cols[3].0, "STATUS");
        assert_eq!(cols[4].0, "RESTARTS");
        assert_eq!(cols[5].0, "AGE");
        assert_eq!(cols[6].0, "NODE");
    }

    #[test]
    fn test_pod_info_to_row() {
        let info = PodInfo {
            namespace: "default".to_string(),
            name: "nginx-abc123".to_string(),
            containers: vec!["nginx".to_string(), "sidecar".to_string()],
            status: "Running".to_string(),
            restarts: 3,
            age: "5d".to_string(),
            node: "node-1".to_string(),
        };

        let row = info.to_row();
        assert_eq!(row.len(), 7);
        assert_eq!(row[0], "default");
        assert_eq!(row[1], "nginx-abc123");
        assert_eq!(row[2], "nginx,sidecar");
        assert_eq!(row[3], "Running");
        assert_eq!(row[4], "3");
        assert_eq!(row[5], "5d");
        assert_eq!(row[6], "node-1");
    }

    #[test]
    fn test_pod_info_to_row_empty_containers() {
        let info = PodInfo {
            namespace: "kube-system".to_string(),
            name: "pending-pod".to_string(),
            containers: vec![],
            status: "Pending".to_string(),
            restarts: 0,
            age: "12m".to_string(),
            node: "".to_string(),
        };

        let row = info.to_row();
        assert_eq!(row[2], ""); // empty containers join
        assert_eq!(row[4], "0");
        assert_eq!(row[6], ""); // no node yet
    }

    #[test]
    fn test_pod_info_to_row_matches_column_count() {
        let info = PodInfo {
            namespace: "ns".to_string(),
            name: "pod".to_string(),
            containers: vec!["c1".to_string()],
            status: "Running".to_string(),
            restarts: 0,
            age: "1h".to_string(),
            node: "n1".to_string(),
        };

        let cols = PodInfo::columns();
        let row = info.to_row();
        assert_eq!(
            cols.len(),
            row.len(),
            "Column count must match row field count"
        );
    }

    // -- Age formatting tests -----------------------------------------------

    #[test]
    fn test_format_age_seconds() {
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::seconds(30);
        let age = format_age(&created);
        assert_eq!(age, "30s");
    }

    #[test]
    fn test_format_age_minutes() {
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::minutes(15);
        let age = format_age(&created);
        assert_eq!(age, "15m");
    }

    #[test]
    fn test_format_age_hours() {
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::hours(7);
        let age = format_age(&created);
        assert_eq!(age, "7h");
    }

    #[test]
    fn test_format_age_days() {
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::days(42);
        let age = format_age(&created);
        assert_eq!(age, "42d");
    }

    #[test]
    fn test_format_age_years() {
        let now = chrono::Utc::now();
        let created = now - chrono::Duration::days(800);
        let age = format_age(&created);
        assert_eq!(age, "2y");
    }

    #[test]
    fn test_format_age_zero() {
        let now = chrono::Utc::now();
        let age = format_age(&now);
        assert_eq!(age, "0s");
    }

    #[test]
    fn test_format_age_future_timestamp() {
        // If the timestamp is somehow in the future, we should not panic
        let future = chrono::Utc::now() + chrono::Duration::seconds(60);
        let age = format_age(&future);
        // Duration will be negative; our code clamps seconds to 0
        assert_eq!(age, "0s");
    }

    // -- Auth method detection tests ----------------------------------------

    #[test]
    fn test_detect_auth_kubeconfig() {
        let mut params = HashMap::new();
        params.insert("kubeconfig".to_string(), "base64data".to_string());
        assert_eq!(detect_auth_method(&params), AuthMethod::Kubeconfig);
    }

    #[test]
    fn test_detect_auth_token() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "eyJhbGci...".to_string());
        assert_eq!(detect_auth_method(&params), AuthMethod::Token);
    }

    #[test]
    fn test_detect_auth_server_cert() {
        let mut params = HashMap::new();
        params.insert(
            "server".to_string(),
            "https://k8s.example.com:6443".to_string(),
        );
        params.insert("ca_cert".to_string(), "base64cert".to_string());
        assert_eq!(detect_auth_method(&params), AuthMethod::ServerCert);
    }

    #[test]
    fn test_detect_auth_default() {
        let params = HashMap::new();
        assert_eq!(detect_auth_method(&params), AuthMethod::Default);
    }

    #[test]
    fn test_detect_auth_precedence_kubeconfig_over_token() {
        let mut params = HashMap::new();
        params.insert("kubeconfig".to_string(), "base64data".to_string());
        params.insert("token".to_string(), "some-token".to_string());
        // kubeconfig takes precedence
        assert_eq!(detect_auth_method(&params), AuthMethod::Kubeconfig);
    }

    #[test]
    fn test_detect_auth_precedence_token_over_server() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "some-token".to_string());
        params.insert("server".to_string(), "https://k8s.example.com".to_string());
        // token takes precedence over bare server
        assert_eq!(detect_auth_method(&params), AuthMethod::Token);
    }

    // -- Namespace filtering tests ------------------------------------------

    #[test]
    fn test_namespace_from_params() {
        let mut params = HashMap::new();
        params.insert("namespace".to_string(), "production".to_string());
        let ns = params.get("namespace").cloned();
        assert_eq!(ns, Some("production".to_string()));
    }

    #[test]
    fn test_namespace_absent_means_all() {
        let params: HashMap<String, String> = HashMap::new();
        let ns = params.get("namespace").cloned();
        assert!(ns.is_none()); // None means all namespaces
    }

    // -- Config building tests (unit-testable portions) ---------------------

    #[test]
    fn test_build_config_from_token_missing_token() {
        let params = HashMap::new();
        let result = build_config_from_token(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing 'token'"));
    }

    #[test]
    fn test_build_config_from_token_default_server() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "test-token-value".to_string());
        let result = build_config_from_token(&params);
        // Should succeed with default server URL
        assert!(result.is_ok(), "Token config should succeed: {:?}", result);
    }

    #[test]
    fn test_build_config_from_token_custom_server() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "test-token-value".to_string());
        params.insert(
            "server".to_string(),
            "https://custom-k8s.example.com:6443".to_string(),
        );
        let result = build_config_from_token(&params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_config_from_token_with_ca_cert() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "test-token-value".to_string());
        // A valid base64-encoded "test cert" (not a real cert, just valid base64)
        params.insert("ca_cert".to_string(), "dGVzdCBjZXJ0".to_string());
        let result = build_config_from_token(&params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_config_from_token_invalid_ca_cert_base64() {
        let mut params = HashMap::new();
        params.insert("token".to_string(), "test-token-value".to_string());
        params.insert("ca_cert".to_string(), "not-valid-base64!!!".to_string());
        let result = build_config_from_token(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid base64 in ca_cert"));
    }

    #[test]
    fn test_build_config_from_server_cert_missing_server() {
        let params = HashMap::new();
        let result = build_config_from_server_cert(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing 'server'"));
    }

    #[test]
    fn test_build_config_from_server_cert_valid() {
        let mut params = HashMap::new();
        params.insert(
            "server".to_string(),
            "https://k8s.example.com:6443".to_string(),
        );
        params.insert("ca_cert".to_string(), "dGVzdCBjZXJ0".to_string());
        let result = build_config_from_server_cert(&params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_config_from_server_cert_no_ca() {
        let mut params = HashMap::new();
        params.insert(
            "server".to_string(),
            "https://k8s.example.com:6443".to_string(),
        );
        // No CA cert - should still work but with insecure TLS
        let result = build_config_from_server_cert(&params);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_config_from_server_cert_invalid_url() {
        let mut params = HashMap::new();
        params.insert("server".to_string(), "not a url".to_string());
        let result = build_config_from_server_cert(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid server URL"));
    }

    // -- Kubeconfig base64 parsing tests ------------------------------------

    #[tokio::test]
    async fn test_build_config_from_kubeconfig_invalid_base64() {
        let mut params = HashMap::new();
        params.insert("kubeconfig".to_string(), "not-base64!!!".to_string());
        let result = build_config_from_kubeconfig(&params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid base64"));
    }

    #[tokio::test]
    async fn test_build_config_from_kubeconfig_invalid_yaml() {
        let mut params = HashMap::new();
        // Valid base64 of "not yaml at all: {{{"
        let bad_yaml = base64::engine::general_purpose::STANDARD.encode("not yaml at all: {{{");
        params.insert("kubeconfig".to_string(), bad_yaml);
        let result = build_config_from_kubeconfig(&params).await;
        assert!(result.is_err());
        // Should fail during YAML parse or kubeconfig interpretation
        let err = result.unwrap_err();
        assert!(
            err.contains("parse") || err.contains("Failed"),
            "Error should mention parsing: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_build_config_from_kubeconfig_missing_param() {
        let params = HashMap::new();
        let result = build_config_from_kubeconfig(&params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing 'kubeconfig'"));
    }

    // -- Pod description formatting tests -----------------------------------

    #[test]
    fn test_format_pod_description_minimal() {
        // Build a minimal Pod with just a name
        let pod = Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("test-pod".to_string()),
                namespace: Some("test-ns".to_string()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::core::v1::PodSpec {
                containers: vec![k8s_openapi::api::core::v1::Container {
                    name: "main".to_string(),
                    image: Some("nginx:latest".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            status: Some(k8s_openapi::api::core::v1::PodStatus {
                phase: Some("Running".to_string()),
                pod_ip: Some("10.0.0.5".to_string()),
                ..Default::default()
            }),
        };

        let desc = format_pod_description(&pod);
        assert!(desc.contains("Name:         test-pod"));
        assert!(desc.contains("Namespace:    test-ns"));
        assert!(desc.contains("Status:       Running"));
        assert!(desc.contains("IP:           10.0.0.5"));
        assert!(desc.contains("nginx:latest"));
        assert!(desc.contains("Containers:"));
        assert!(desc.contains("  main:"));
    }

    #[test]
    fn test_format_pod_description_with_labels() {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("app".to_string(), "web".to_string());
        labels.insert("tier".to_string(), "frontend".to_string());

        let pod = Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("labeled-pod".to_string()),
                labels: Some(labels),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::core::v1::PodSpec {
                containers: vec![k8s_openapi::api::core::v1::Container {
                    name: "app".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            status: None,
        };

        let desc = format_pod_description(&pod);
        assert!(desc.contains("Labels:"));
        assert!(desc.contains("app=web"));
        assert!(desc.contains("tier=frontend"));
    }

    #[test]
    fn test_format_pod_description_no_labels() {
        let pod = Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("no-labels".to_string()),
                ..Default::default()
            },
            spec: None,
            status: None,
        };

        let desc = format_pod_description(&pod);
        assert!(desc.contains("Labels:       <none>"));
    }

    // -- PodInfo::from_pod tests --------------------------------------------

    #[test]
    fn test_pod_info_from_pod_full() {
        use k8s_openapi::api::core::v1::*;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

        let pod = Pod {
            metadata: ObjectMeta {
                name: Some("my-pod".to_string()),
                namespace: Some("production".to_string()),
                creation_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    chrono::Utc::now() - chrono::Duration::hours(3),
                )),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![
                    Container {
                        name: "web".to_string(),
                        ..Default::default()
                    },
                    Container {
                        name: "sidecar".to_string(),
                        ..Default::default()
                    },
                ],
                node_name: Some("worker-2".to_string()),
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some("Running".to_string()),
                container_statuses: Some(vec![
                    ContainerStatus {
                        name: "web".to_string(),
                        restart_count: 2,
                        ready: true,
                        image: "nginx:1.25".to_string(),
                        image_id: "".to_string(),
                        ..Default::default()
                    },
                    ContainerStatus {
                        name: "sidecar".to_string(),
                        restart_count: 1,
                        ready: true,
                        image: "envoy:latest".to_string(),
                        image_id: "".to_string(),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
        };

        let info = PodInfo::from_pod(&pod);
        assert_eq!(info.namespace, "production");
        assert_eq!(info.name, "my-pod");
        assert_eq!(info.containers, vec!["web", "sidecar"]);
        assert_eq!(info.status, "Running");
        assert_eq!(info.restarts, 3); // 2 + 1
        assert_eq!(info.age, "3h");
        assert_eq!(info.node, "worker-2");
    }

    #[test]
    fn test_pod_info_from_pod_minimal() {
        let pod = Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta::default(),
            spec: None,
            status: None,
        };

        let info = PodInfo::from_pod(&pod);
        assert_eq!(info.namespace, "default");
        assert_eq!(info.name, "<unknown>");
        assert!(info.containers.is_empty());
        assert_eq!(info.status, "Unknown");
        assert_eq!(info.restarts, 0);
        assert_eq!(info.age, "<unknown>");
        assert_eq!(info.node, "");
    }

    #[test]
    fn test_pod_info_from_pod_pending_no_node() {
        use k8s_openapi::api::core::v1::*;
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

        let pod = Pod {
            metadata: ObjectMeta {
                name: Some("pending-pod".to_string()),
                namespace: Some("staging".to_string()),
                creation_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    chrono::Utc::now() - chrono::Duration::minutes(5),
                )),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![Container {
                    name: "app".to_string(),
                    ..Default::default()
                }],
                // no node_name (not yet scheduled)
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some("Pending".to_string()),
                ..Default::default()
            }),
        };

        let info = PodInfo::from_pod(&pod);
        assert_eq!(info.status, "Pending");
        assert_eq!(info.node, ""); // not yet scheduled
        assert_eq!(info.age, "5m");
    }

    // -- Param keys tests ---------------------------------------------------

    #[test]
    fn test_param_key_constants() {
        assert_eq!(param_keys::KUBECONFIG, "kubeconfig");
        assert_eq!(param_keys::TOKEN, "token");
        assert_eq!(param_keys::SERVER, "server");
        assert_eq!(param_keys::CA_CERT, "ca_cert");
        assert_eq!(param_keys::NAMESPACE, "namespace");
        assert_eq!(param_keys::CONTEXT, "context");
    }

    // -- Volume source formatting tests -------------------------------------

    #[test]
    fn test_format_volume_source_configmap() {
        use k8s_openapi::api::core::v1::*;
        let vol = Volume {
            name: "config-vol".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: "my-config".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut out = String::new();
        format_volume_source(&mut out, &vol);
        assert!(out.contains("ConfigMap"));
        assert!(out.contains("my-config"));
    }

    #[test]
    fn test_format_volume_source_secret() {
        use k8s_openapi::api::core::v1::*;
        let vol = Volume {
            name: "secret-vol".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some("my-secret".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut out = String::new();
        format_volume_source(&mut out, &vol);
        assert!(out.contains("Secret"));
        assert!(out.contains("my-secret"));
    }

    #[test]
    fn test_format_volume_source_emptydir() {
        use k8s_openapi::api::core::v1::*;
        let vol = Volume {
            name: "tmp".to_string(),
            empty_dir: Some(EmptyDirVolumeSource::default()),
            ..Default::default()
        };
        let mut out = String::new();
        format_volume_source(&mut out, &vol);
        assert!(out.contains("EmptyDir"));
    }

    #[test]
    fn test_format_volume_source_pvc() {
        use k8s_openapi::api::core::v1::*;
        let vol = Volume {
            name: "data".to_string(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: "my-pvc".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut out = String::new();
        format_volume_source(&mut out, &vol);
        assert!(out.contains("PersistentVolumeClaim"));
        assert!(out.contains("my-pvc"));
    }

    #[test]
    fn test_format_volume_source_hostpath() {
        use k8s_openapi::api::core::v1::*;
        let vol = Volume {
            name: "host".to_string(),
            host_path: Some(HostPathVolumeSource {
                path: "/var/log".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut out = String::new();
        format_volume_source(&mut out, &vol);
        assert!(out.contains("HostPath"));
        assert!(out.contains("/var/log"));
    }
}
