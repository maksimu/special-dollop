// Docker API client layer for container management
//
// Provides a high-level async client for Docker Engine API operations,
// built on the `bollard` crate. Supports two primary connection modes:
//
// - TCP with TLS client certificates (primary PAM path for remote Docker daemons)
// - Unix socket / named pipe (local development, also covers Podman via socket)
//
// SSH tunnel access is handled externally (e.g. by the PAM Gateway establishing
// an SSH port forward to the remote Docker socket before connecting via TCP).
//
// This module is standalone and does not depend on the ResourceBrowser trait.
// It can be used directly by a future DockerHandler or by resource browsing UI.

use log::{debug, info, warn};
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use bollard::container::{
    InspectContainerOptions, ListContainersOptions, LogOutput, LogsOptions, StartContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::Docker;
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// ---------------------------------------------------------------------------
// Connection parameter keys
// ---------------------------------------------------------------------------

/// Docker daemon hostname (for TCP+TLS connections)
const PARAM_HOSTNAME: &str = "hostname";

/// Docker daemon port (default: 2376 for TLS, 2375 for plain)
const PARAM_PORT: &str = "port";

/// Path to CA certificate (PEM) for verifying the Docker daemon
const PARAM_CA_CERT: &str = "docker-ca-cert";

/// Path to client certificate (PEM) for mutual TLS
const PARAM_CLIENT_CERT: &str = "docker-client-cert";

/// Path to client private key (PEM) for mutual TLS
const PARAM_CLIENT_KEY: &str = "docker-client-key";

/// Unix socket path (e.g. /var/run/docker.sock) or Windows named pipe
const PARAM_SOCKET_PATH: &str = "docker-socket";

/// Default TLS port for Docker daemon
const DEFAULT_TLS_PORT: u16 = 2376;

/// Default Unix socket path
const DEFAULT_SOCKET_PATH: &str = "/var/run/docker.sock";

/// Connection timeout in seconds for all Docker API connections
const CONNECTION_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// ContainerInfo -- structured container metadata
// ---------------------------------------------------------------------------

/// Structured information about a Docker container, suitable for display
/// in a spreadsheet/table UI.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Short container ID (first 12 hex characters)
    pub id: String,
    /// Container name (without leading slash)
    pub name: String,
    /// Image name (repository:tag)
    pub image: String,
    /// Human-readable status (e.g. "Up 3 hours", "Exited (0) 2 days ago")
    pub status: String,
    /// Formatted port mappings (e.g. "0.0.0.0:8080->80/tcp, 443/tcp")
    pub ports: String,
    /// Human-readable creation age (e.g. "3 hours ago", "2 days ago")
    pub created: String,
}

impl ContainerInfo {
    /// Column definitions for table rendering: (header, width in characters).
    pub fn columns() -> Vec<(&'static str, usize)> {
        vec![
            ("CONTAINER ID", 14),
            ("NAME", 24),
            ("IMAGE", 28),
            ("STATUS", 22),
            ("PORTS", 30),
            ("CREATED", 16),
        ]
    }

    /// Convert this container's fields into a row of strings matching
    /// the order returned by `columns()`.
    pub fn to_row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.name.clone(),
            self.image.clone(),
            self.status.clone(),
            self.ports.clone(),
            self.created.clone(),
        ]
    }
}

/// Format a Docker port mapping into the conventional `host:container/proto` string.
///
/// Handles cases where only the container port is exposed (no host binding),
/// where a specific host IP is bound, and multiple port mappings.
fn format_port_mappings(ports: &[bollard::models::Port]) -> String {
    if ports.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::with_capacity(ports.len());
    for p in ports {
        let proto = p.typ.as_ref().map_or("tcp", |t| match t {
            bollard::models::PortTypeEnum::TCP => "tcp",
            bollard::models::PortTypeEnum::UDP => "udp",
            bollard::models::PortTypeEnum::SCTP => "sctp",
            bollard::models::PortTypeEnum::EMPTY => "",
        });

        match (p.ip.as_deref(), p.public_port) {
            (Some(ip), Some(public)) => {
                parts.push(format!("{}:{}->{}/{}", ip, public, p.private_port, proto));
            }
            (None, Some(public)) => {
                parts.push(format!("{}->{}/{}", public, p.private_port, proto));
            }
            _ => {
                parts.push(format!("{}/{}", p.private_port, proto));
            }
        }
    }
    parts.join(", ")
}

/// Convert a Unix timestamp (seconds since epoch) into a human-readable age
/// string such as "5 minutes ago", "3 hours ago", "2 days ago".
fn format_age(created_unix: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now.saturating_sub(created_unix);

    if diff < 0 {
        return "just now".to_string();
    }

    let seconds = diff as u64;
    if seconds < 60 {
        if seconds <= 1 {
            return "1 second ago".to_string();
        }
        return format!("{} seconds ago", seconds);
    }

    let minutes = seconds / 60;
    if minutes < 60 {
        if minutes == 1 {
            return "1 minute ago".to_string();
        }
        return format!("{} minutes ago", minutes);
    }

    let hours = minutes / 60;
    if hours < 24 {
        if hours == 1 {
            return "1 hour ago".to_string();
        }
        return format!("{} hours ago", hours);
    }

    let days = hours / 24;
    if days < 30 {
        if days == 1 {
            return "1 day ago".to_string();
        }
        return format!("{} days ago", days);
    }

    let months = days / 30;
    if months < 12 {
        if months == 1 {
            return "1 month ago".to_string();
        }
        return format!("{} months ago", months);
    }

    let years = months / 12;
    if years == 1 {
        return "1 year ago".to_string();
    }
    format!("{} years ago", years)
}

/// Extract the bytes from a `LogOutput` enum variant.
fn log_output_to_bytes(output: LogOutput) -> bytes::Bytes {
    output.into_bytes()
}

// ---------------------------------------------------------------------------
// DockerExecStreams -- bidirectional exec I/O
// ---------------------------------------------------------------------------

/// Bidirectional stream wrapper for `docker exec` sessions.
///
/// Implements `AsyncRead` to read output (stdout+stderr) from the container.
pub struct DockerExecReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
}

impl AsyncRead for DockerExecReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

/// Writer handle for sending input (stdin) to a running exec session.
pub struct DockerExecWriter {
    inner: Pin<Box<dyn AsyncWrite + Send>>,
}

impl AsyncWrite for DockerExecWriter {
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

/// Log reader stream from `docker logs`.
///
/// Implements `AsyncRead` to stream log bytes from a container.
pub struct DockerLogReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
}

impl AsyncRead for DockerLogReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

// ---------------------------------------------------------------------------
// DockerClient
// ---------------------------------------------------------------------------

/// Async client for Docker Engine API operations.
///
/// Wraps `bollard::Docker` with connection management, PAM-friendly
/// parameter parsing, and structured output types.
///
/// # Connection modes
///
/// - **TCP with TLS**: For remote Docker daemons with client certificate auth.
///   Set `hostname`, `docker-ca-cert`, `docker-client-cert`, `docker-client-key`.
/// - **TCP without TLS**: For local dev (set `hostname` without cert params).
/// - **Unix socket**: Default local path or explicit `docker-socket` param.
///   Also works with Podman's API-compatible socket.
pub struct DockerClient {
    client: Docker,
}

impl DockerClient {
    /// Create a `DockerClient` from connection parameters.
    ///
    /// Detects the connection mode from the parameter map:
    ///
    /// 1. If `hostname` is present with TLS cert params, connects via TCP+TLS.
    /// 2. If `hostname` is present without certs, connects via plain TCP.
    /// 3. If `docker-socket` is present, connects via that socket path.
    /// 4. Otherwise, connects via the default Unix socket (`/var/run/docker.sock`).
    ///
    /// After establishing the connection, pings the daemon to verify reachability.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` if the connection cannot be established,
    /// required TLS certificates are missing, or the daemon ping fails.
    pub async fn from_params(params: &HashMap<String, String>) -> Result<Self, String> {
        let client = build_docker_connection(params)?;

        // Validate the connection by pinging the daemon
        client
            .ping()
            .await
            .map_err(|e| format!("Docker daemon ping failed: {}", e))?;

        info!("Docker client connected successfully");
        Ok(DockerClient { client })
    }

    /// Create a `DockerClient` from an already-constructed `bollard::Docker` instance.
    ///
    /// Useful for testing or when the caller manages connection setup externally.
    pub fn from_bollard(client: Docker) -> Self {
        DockerClient { client }
    }

    /// List containers.
    ///
    /// When `all` is true, includes stopped containers (equivalent to `docker ps -a`).
    /// When false, only running containers are returned.
    pub async fn list_containers(&self, all: bool) -> Result<Vec<ContainerInfo>, String> {
        let options = ListContainersOptions::<String> {
            all,
            ..Default::default()
        };

        let containers = self
            .client
            .list_containers(Some(options))
            .await
            .map_err(|e| format!("Failed to list containers: {}", e))?;

        let mut result = Vec::with_capacity(containers.len());
        for c in containers {
            let id_full = c.id.unwrap_or_default();
            let id_short = if id_full.len() >= 12 {
                id_full[..12].to_string()
            } else {
                id_full.clone()
            };

            // Container names from the Docker API have a leading slash
            let name = c
                .names
                .and_then(|names| names.into_iter().next())
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string();

            let image = c.image.unwrap_or_default();
            let status = c.status.unwrap_or_default();

            let ports = c
                .ports
                .as_deref()
                .map_or_else(String::new, format_port_mappings);

            let created = c.created.map_or_else(|| "unknown".to_string(), format_age);

            result.push(ContainerInfo {
                id: id_short,
                name,
                image,
                status,
                ports,
                created,
            });
        }

        debug!("Listed {} containers (all={})", result.len(), all);
        Ok(result)
    }

    /// Get detailed inspection data for a container as a JSON string.
    ///
    /// Returns the full `docker inspect` output serialized as pretty-printed JSON.
    pub async fn inspect_container(&self, id: &str) -> Result<String, String> {
        let details = self
            .client
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| format!("Failed to inspect container '{}': {}", id, e))?;

        serde_json::to_string_pretty(&details)
            .map_err(|e| format!("Failed to serialize inspect output: {}", e))
    }

    /// Start an interactive exec session inside a running container.
    ///
    /// Creates and starts a Docker exec instance with an attached TTY,
    /// returning separate reader (stdout+stderr) and writer (stdin) handles.
    ///
    /// # Arguments
    ///
    /// * `id` - Container ID or name
    /// * `command` - Command and arguments to execute (e.g. `vec!["sh"]` or `vec!["/bin/bash"]`)
    ///
    /// # Returns
    ///
    /// A tuple of `(DockerExecReader, DockerExecWriter)` for bidirectional I/O.
    pub async fn exec_container(
        &self,
        id: &str,
        command: Vec<String>,
    ) -> Result<(DockerExecReader, DockerExecWriter), String> {
        let cmd_refs: Vec<&str> = command.iter().map(|s| s.as_str()).collect();
        let exec_opts = CreateExecOptions {
            cmd: Some(cmd_refs),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(true),
            tty: Some(true),
            ..Default::default()
        };

        let exec_instance = self
            .client
            .create_exec(id, exec_opts)
            .await
            .map_err(|e| format!("Failed to create exec in container '{}': {}", id, e))?;

        let start_opts = StartExecOptions {
            detach: false,
            ..Default::default()
        };

        let start_result = self
            .client
            .start_exec(&exec_instance.id, Some(start_opts))
            .await
            .map_err(|e| format!("Failed to start exec '{}': {}", exec_instance.id, e))?;

        match start_result {
            StartExecResults::Attached { output, input } => {
                // Convert the output stream into an AsyncRead by collecting
                // LogOutput chunks into a byte stream.
                let byte_stream = output.filter_map(|item| async {
                    match item {
                        Ok(log_output) => {
                            Some(Ok::<_, std::io::Error>(log_output_to_bytes(log_output)))
                        }
                        Err(e) => Some(Err(std::io::Error::other(e.to_string()))),
                    }
                });

                let reader = tokio_util::io::StreamReader::new(byte_stream);
                let reader = DockerExecReader {
                    inner: Box::pin(reader),
                };
                let writer = DockerExecWriter {
                    inner: Box::pin(input),
                };

                debug!("Exec session started in container '{}'", id);
                Ok((reader, writer))
            }
            StartExecResults::Detached => Err(format!(
                "Exec started in detached mode unexpectedly for container '{}'",
                id
            )),
        }
    }

    /// Stream container logs.
    ///
    /// # Arguments
    ///
    /// * `id` - Container ID or name
    /// * `follow` - If true, continuously stream new log output (like `docker logs -f`)
    /// * `tail` - If set, only return the last N lines (like `docker logs --tail N`)
    ///
    /// # Returns
    ///
    /// A `DockerLogReader` implementing `AsyncRead` that yields log bytes.
    pub async fn container_logs(
        &self,
        id: &str,
        follow: bool,
        tail: Option<usize>,
    ) -> Result<DockerLogReader, String> {
        let tail_str = tail.map_or_else(|| "all".to_string(), |n| n.to_string());

        let options = LogsOptions::<String> {
            follow,
            stdout: true,
            stderr: true,
            tail: tail_str,
            ..Default::default()
        };

        let log_stream = self.client.logs(id, Some(options));

        let byte_stream = log_stream.filter_map(|item| async {
            match item {
                Ok(log_output) => Some(Ok::<_, std::io::Error>(log_output_to_bytes(log_output))),
                Err(e) => Some(Err(std::io::Error::other(e.to_string()))),
            }
        });

        let reader = tokio_util::io::StreamReader::new(byte_stream);

        debug!(
            "Log stream started for container '{}' (follow={}, tail={:?})",
            id, follow, tail
        );

        Ok(DockerLogReader {
            inner: Box::pin(reader),
        })
    }

    /// Start a stopped container.
    pub async fn start_container(&self, id: &str) -> Result<(), String> {
        self.client
            .start_container(id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| format!("Failed to start container '{}': {}", id, e))?;

        info!("Container '{}' started", id);
        Ok(())
    }

    /// Stop a running container.
    ///
    /// Sends SIGTERM and waits for the container to exit. If the container
    /// does not stop within the specified timeout (10 seconds), SIGKILL
    /// is sent.
    pub async fn stop_container(&self, id: &str) -> Result<(), String> {
        let options = StopContainerOptions { t: 10 };

        self.client
            .stop_container(id, Some(options))
            .await
            .map_err(|e| format!("Failed to stop container '{}': {}", id, e))?;

        info!("Container '{}' stopped", id);
        Ok(())
    }

    /// Return a reference to the underlying bollard client for advanced
    /// operations not covered by the high-level API.
    pub fn inner(&self) -> &Docker {
        &self.client
    }
}

// ---------------------------------------------------------------------------
// Connection builder
// ---------------------------------------------------------------------------

/// Build a `bollard::Docker` connection from handler parameters.
///
/// Connection mode priority:
///
/// 1. **TCP with TLS** (`hostname` + cert params) -- connects to a remote Docker
///    daemon over TCP with mutual TLS authentication (the standard Docker
///    `--tlsverify` mode). This is the primary PAM path.
///
/// 2. **TCP without TLS** (`hostname` without cert params) -- plain TCP
///    connection. Not recommended for production but valid for local dev
///    or when TLS termination happens at a proxy layer.
///
/// 3. **Unix socket / named pipe** (`docker-socket` param or default path) --
///    connects to a local Docker daemon via Unix domain socket (Linux/macOS)
///    or named pipe (Windows).
///
/// For SSH tunnel access: the PAM Gateway should establish an SSH port forward
/// to the remote Docker socket first, then connect via TCP to the forwarded port.
fn build_docker_connection(params: &HashMap<String, String>) -> Result<Docker, String> {
    // Mode 1: TCP with optional TLS
    if let Some(hostname) = params.get(PARAM_HOSTNAME) {
        let port: u16 = params
            .get(PARAM_PORT)
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_TLS_PORT);

        let addr = format!("tcp://{}:{}", hostname, port);

        let has_tls_certs = params.contains_key(PARAM_CA_CERT)
            || params.contains_key(PARAM_CLIENT_CERT)
            || params.contains_key(PARAM_CLIENT_KEY);

        if has_tls_certs {
            let ca_cert = params
                .get(PARAM_CA_CERT)
                .ok_or_else(|| "TLS requires 'docker-ca-cert' parameter".to_string())?;
            let client_cert = params
                .get(PARAM_CLIENT_CERT)
                .ok_or_else(|| "TLS requires 'docker-client-cert' parameter".to_string())?;
            let client_key = params
                .get(PARAM_CLIENT_KEY)
                .ok_or_else(|| "TLS requires 'docker-client-key' parameter".to_string())?;

            debug!("Connecting to Docker via TLS at {}", addr);

            // bollard's connect_with_ssl takes Path references to PEM files
            let docker = Docker::connect_with_ssl(
                &addr,
                Path::new(client_key),
                Path::new(client_cert),
                Path::new(ca_cert),
                CONNECTION_TIMEOUT_SECS,
                bollard::API_DEFAULT_VERSION,
            )
            .map_err(|e| format!("Docker TLS connection to {} failed: {}", addr, e))?;

            return Ok(docker);
        }

        // TCP without TLS (not recommended for production but valid for local dev)
        warn!("Connecting to Docker via unencrypted TCP at {}", addr);
        let docker =
            Docker::connect_with_http(&addr, CONNECTION_TIMEOUT_SECS, bollard::API_DEFAULT_VERSION)
                .map_err(|e| format!("Docker TCP connection to {} failed: {}", addr, e))?;

        return Ok(docker);
    }

    // Mode 2: Unix socket / named pipe
    let socket_path = params
        .get(PARAM_SOCKET_PATH)
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_SOCKET_PATH);

    debug!("Connecting to Docker via Unix socket: {}", socket_path);
    let docker = Docker::connect_with_socket(
        socket_path,
        CONNECTION_TIMEOUT_SECS,
        bollard::API_DEFAULT_VERSION,
    )
    .map_err(|e| {
        format!(
            "Docker socket connection at '{}' failed: {}",
            socket_path, e
        )
    })?;

    Ok(docker)
}

/// Detect which connection mode will be used based on the parameter map.
///
/// Returns a human-readable description for logging/diagnostics.
pub fn detect_connection_mode(params: &HashMap<String, String>) -> &'static str {
    if params.contains_key(PARAM_HOSTNAME) {
        if params.contains_key(PARAM_CA_CERT)
            || params.contains_key(PARAM_CLIENT_CERT)
            || params.contains_key(PARAM_CLIENT_KEY)
        {
            "TCP with TLS"
        } else {
            "TCP (unencrypted)"
        }
    } else {
        "Unix socket"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ContainerInfo::to_row tests --

    #[test]
    fn test_to_row_returns_correct_order() {
        let info = ContainerInfo {
            id: "abc123def456".to_string(),
            name: "my-app".to_string(),
            image: "nginx:latest".to_string(),
            status: "Up 3 hours".to_string(),
            ports: "0.0.0.0:8080->80/tcp".to_string(),
            created: "3 hours ago".to_string(),
        };

        let row = info.to_row();
        assert_eq!(row.len(), 6);
        assert_eq!(row[0], "abc123def456");
        assert_eq!(row[1], "my-app");
        assert_eq!(row[2], "nginx:latest");
        assert_eq!(row[3], "Up 3 hours");
        assert_eq!(row[4], "0.0.0.0:8080->80/tcp");
        assert_eq!(row[5], "3 hours ago");
    }

    #[test]
    fn test_to_row_matches_columns_count() {
        let columns = ContainerInfo::columns();
        let info = ContainerInfo {
            id: "a".to_string(),
            name: "b".to_string(),
            image: "c".to_string(),
            status: "d".to_string(),
            ports: "e".to_string(),
            created: "f".to_string(),
        };
        assert_eq!(info.to_row().len(), columns.len());
    }

    #[test]
    fn test_columns_are_nonempty() {
        for (header, width) in ContainerInfo::columns() {
            assert!(!header.is_empty(), "Column header must not be empty");
            assert!(width > 0, "Column width must be positive for '{}'", header);
        }
    }

    // -- Port formatting tests --

    #[test]
    fn test_format_port_mappings_empty() {
        assert_eq!(format_port_mappings(&[]), "");
    }

    #[test]
    fn test_format_port_mappings_host_bound() {
        let ports = vec![bollard::models::Port {
            ip: Some("0.0.0.0".to_string()),
            private_port: 80,
            public_port: Some(8080),
            typ: Some(bollard::models::PortTypeEnum::TCP),
        }];
        assert_eq!(format_port_mappings(&ports), "0.0.0.0:8080->80/tcp");
    }

    #[test]
    fn test_format_port_mappings_no_host_ip() {
        let ports = vec![bollard::models::Port {
            ip: None,
            private_port: 443,
            public_port: Some(8443),
            typ: Some(bollard::models::PortTypeEnum::TCP),
        }];
        assert_eq!(format_port_mappings(&ports), "8443->443/tcp");
    }

    #[test]
    fn test_format_port_mappings_container_only() {
        let ports = vec![bollard::models::Port {
            ip: None,
            private_port: 3306,
            public_port: None,
            typ: Some(bollard::models::PortTypeEnum::TCP),
        }];
        assert_eq!(format_port_mappings(&ports), "3306/tcp");
    }

    #[test]
    fn test_format_port_mappings_udp() {
        let ports = vec![bollard::models::Port {
            ip: Some("0.0.0.0".to_string()),
            private_port: 53,
            public_port: Some(53),
            typ: Some(bollard::models::PortTypeEnum::UDP),
        }];
        assert_eq!(format_port_mappings(&ports), "0.0.0.0:53->53/udp");
    }

    #[test]
    fn test_format_port_mappings_multiple() {
        let ports = vec![
            bollard::models::Port {
                ip: Some("0.0.0.0".to_string()),
                private_port: 80,
                public_port: Some(8080),
                typ: Some(bollard::models::PortTypeEnum::TCP),
            },
            bollard::models::Port {
                ip: None,
                private_port: 443,
                public_port: None,
                typ: Some(bollard::models::PortTypeEnum::TCP),
            },
        ];
        assert_eq!(
            format_port_mappings(&ports),
            "0.0.0.0:8080->80/tcp, 443/tcp"
        );
    }

    #[test]
    fn test_format_port_mappings_no_type() {
        let ports = vec![bollard::models::Port {
            ip: None,
            private_port: 9090,
            public_port: None,
            typ: None,
        }];
        // Falls through to default "tcp"
        assert_eq!(format_port_mappings(&ports), "9090/tcp");
    }

    #[test]
    fn test_format_port_mappings_sctp() {
        let ports = vec![bollard::models::Port {
            ip: Some("127.0.0.1".to_string()),
            private_port: 9999,
            public_port: Some(9999),
            typ: Some(bollard::models::PortTypeEnum::SCTP),
        }];
        assert_eq!(format_port_mappings(&ports), "127.0.0.1:9999->9999/sctp");
    }

    // -- Age formatting tests --

    #[test]
    fn test_format_age_seconds() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now), "1 second ago");
        assert_eq!(format_age(now - 30), "30 seconds ago");
    }

    #[test]
    fn test_format_age_minutes() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - 60), "1 minute ago");
        assert_eq!(format_age(now - 300), "5 minutes ago");
        assert_eq!(format_age(now - 3540), "59 minutes ago");
    }

    #[test]
    fn test_format_age_hours() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - 3600), "1 hour ago");
        assert_eq!(format_age(now - 10800), "3 hours ago");
        assert_eq!(format_age(now - 82800), "23 hours ago");
    }

    #[test]
    fn test_format_age_days() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - 86400), "1 day ago");
        assert_eq!(format_age(now - 172800), "2 days ago");
        assert_eq!(format_age(now - (86400 * 15)), "15 days ago");
    }

    #[test]
    fn test_format_age_months() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - (86400 * 30)), "1 month ago");
        assert_eq!(format_age(now - (86400 * 90)), "3 months ago");
    }

    #[test]
    fn test_format_age_years() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - (86400 * 365)), "1 year ago");
        assert_eq!(format_age(now - (86400 * 730)), "2 years ago");
    }

    #[test]
    fn test_format_age_future_timestamp() {
        let future = chrono::Utc::now().timestamp() + 3600;
        assert_eq!(format_age(future), "just now");
    }

    #[test]
    fn test_format_age_boundary_59_seconds() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - 59), "59 seconds ago");
    }

    #[test]
    fn test_format_age_boundary_23_hours() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - (23 * 3600)), "23 hours ago");
    }

    #[test]
    fn test_format_age_boundary_29_days() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(format_age(now - (29 * 86400)), "29 days ago");
    }

    // -- Connection mode detection tests --

    #[test]
    fn test_detect_tls_mode() {
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "docker.example.com".to_string());
        params.insert("docker-ca-cert".to_string(), "/path/ca.pem".to_string());
        params.insert(
            "docker-client-cert".to_string(),
            "/path/cert.pem".to_string(),
        );
        params.insert("docker-client-key".to_string(), "/path/key.pem".to_string());
        assert_eq!(detect_connection_mode(&params), "TCP with TLS");
    }

    #[test]
    fn test_detect_tls_mode_partial_certs() {
        // Even a single TLS param triggers TLS mode detection
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "docker.example.com".to_string());
        params.insert("docker-ca-cert".to_string(), "/path/ca.pem".to_string());
        assert_eq!(detect_connection_mode(&params), "TCP with TLS");
    }

    #[test]
    fn test_detect_tcp_unencrypted_mode() {
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "docker.example.com".to_string());
        assert_eq!(detect_connection_mode(&params), "TCP (unencrypted)");
    }

    #[test]
    fn test_detect_socket_mode_default() {
        let params = HashMap::new();
        assert_eq!(detect_connection_mode(&params), "Unix socket");
    }

    #[test]
    fn test_detect_socket_mode_explicit() {
        let mut params = HashMap::new();
        params.insert(
            "docker-socket".to_string(),
            "/run/podman/podman.sock".to_string(),
        );
        assert_eq!(detect_connection_mode(&params), "Unix socket");
    }

    #[test]
    fn test_detect_hostname_takes_priority_over_socket() {
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "docker.example.com".to_string());
        params.insert(
            "docker-socket".to_string(),
            "/var/run/docker.sock".to_string(),
        );
        // hostname takes priority
        assert_eq!(detect_connection_mode(&params), "TCP (unencrypted)");
    }

    // -- Constant tests --

    #[test]
    fn test_default_tls_port() {
        assert_eq!(DEFAULT_TLS_PORT, 2376);
    }

    #[test]
    fn test_default_socket_path() {
        assert_eq!(DEFAULT_SOCKET_PATH, "/var/run/docker.sock");
    }

    #[test]
    fn test_connection_timeout() {
        assert_eq!(CONNECTION_TIMEOUT_SECS, 120);
    }

    // -- ContainerInfo edge cases --

    #[test]
    fn test_container_info_empty_fields() {
        let info = ContainerInfo {
            id: String::new(),
            name: String::new(),
            image: String::new(),
            status: String::new(),
            ports: String::new(),
            created: String::new(),
        };
        let row = info.to_row();
        assert_eq!(row.len(), 6);
        for field in &row {
            assert_eq!(field, "");
        }
    }

    #[test]
    fn test_container_info_clone() {
        let info = ContainerInfo {
            id: "abc123def456".to_string(),
            name: "test".to_string(),
            image: "alpine:latest".to_string(),
            status: "Up".to_string(),
            ports: String::new(),
            created: "1 hour ago".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(info.id, cloned.id);
        assert_eq!(info.name, cloned.name);
    }

    // -- Port formatting edge cases --

    #[test]
    fn test_format_port_mappings_empty_type() {
        let ports = vec![bollard::models::Port {
            ip: None,
            private_port: 8080,
            public_port: None,
            typ: Some(bollard::models::PortTypeEnum::EMPTY),
        }];
        // EMPTY type produces empty string for proto
        assert_eq!(format_port_mappings(&ports), "8080/");
    }

    #[test]
    fn test_format_port_mappings_localhost_binding() {
        let ports = vec![bollard::models::Port {
            ip: Some("127.0.0.1".to_string()),
            private_port: 5432,
            public_port: Some(5432),
            typ: Some(bollard::models::PortTypeEnum::TCP),
        }];
        assert_eq!(format_port_mappings(&ports), "127.0.0.1:5432->5432/tcp");
    }

    #[test]
    fn test_format_port_mappings_ipv6() {
        let ports = vec![bollard::models::Port {
            ip: Some("::".to_string()),
            private_port: 80,
            public_port: Some(80),
            typ: Some(bollard::models::PortTypeEnum::TCP),
        }];
        assert_eq!(format_port_mappings(&ports), ":::80->80/tcp");
    }

    #[test]
    fn test_format_port_mappings_three_ports() {
        let ports = vec![
            bollard::models::Port {
                ip: Some("0.0.0.0".to_string()),
                private_port: 80,
                public_port: Some(80),
                typ: Some(bollard::models::PortTypeEnum::TCP),
            },
            bollard::models::Port {
                ip: Some("0.0.0.0".to_string()),
                private_port: 443,
                public_port: Some(443),
                typ: Some(bollard::models::PortTypeEnum::TCP),
            },
            bollard::models::Port {
                ip: None,
                private_port: 8443,
                public_port: None,
                typ: Some(bollard::models::PortTypeEnum::TCP),
            },
        ];
        assert_eq!(
            format_port_mappings(&ports),
            "0.0.0.0:80->80/tcp, 0.0.0.0:443->443/tcp, 8443/tcp"
        );
    }
}
