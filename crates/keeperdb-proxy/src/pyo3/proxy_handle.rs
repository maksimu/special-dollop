//! ProxyHandle for Python lifecycle management.
//!
//! This module provides the ProxyHandle class that Python uses to control
//! the proxy lifecycle (start, stop, status).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::sync::broadcast;

use crate::config::{
    Config, CredentialsConfig, LoggingConfig, ServerConfig, SessionSecurityConfig,
    TargetWithCredentials,
};
use crate::handshake::CredentialStore;
use crate::server::connection_tracker::ConnectionTracker;
use crate::server::Listener;

use super::config::PyDbParams;
use super::gateway_provider::GatewayAuthProvider;

/// Handle to a running database proxy instance.
///
/// Returned by `start_proxy()` and provides methods to control the proxy lifecycle.
///
/// # Example
///
/// ```python
/// proxy = keeperdb_proxy.start_proxy({...})
/// print(f"Listening on {proxy.host}:{proxy.port}")
/// proxy.stop()
/// ```
#[pyclass]
pub struct ProxyHandle {
    /// The host the proxy is listening on
    #[pyo3(get)]
    pub host: String,

    /// The port the proxy is listening on
    #[pyo3(get)]
    pub port: u16,

    /// Whether this is gateway mode (credentials via handshake)
    #[pyo3(get)]
    pub gateway_mode: bool,

    /// Shutdown signal sender
    shutdown_tx: Option<broadcast::Sender<()>>,

    /// Whether the proxy is running
    running: Arc<AtomicBool>,

    /// Credential store for gateway mode (for stats/cleanup)
    credential_store: Option<Arc<CredentialStore>>,

    /// Connection tracker for tunnel management
    connection_tracker: Option<Arc<ConnectionTracker>>,

    /// Tokio runtime handle (kept alive for the proxy)
    _runtime: Option<tokio::runtime::Runtime>,
}

#[pymethods]
impl ProxyHandle {
    /// Stop the proxy gracefully.
    ///
    /// Sends a shutdown signal to the proxy and waits for it to stop.
    /// This method is safe to call multiple times - subsequent calls are no-ops.
    ///
    /// # Example
    ///
    /// ```python
    /// proxy = keeperdb_proxy.start_proxy({...})
    /// # ... use proxy ...
    /// proxy.stop()  # Gracefully shutdown
    /// ```
    fn stop(&mut self) -> PyResult<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(tx) = self.shutdown_tx.take() {
            // Send shutdown signal - ignore errors (receivers may have dropped)
            let _ = tx.send(());
        }

        self.running.store(false, Ordering::SeqCst);

        // Give the server a moment to clean up
        std::thread::sleep(Duration::from_millis(100));

        Ok(())
    }

    /// Check if the proxy is still running.
    ///
    /// # Returns
    ///
    /// `True` if the proxy is running, `False` otherwise.
    ///
    /// # Example
    ///
    /// ```python
    /// proxy = keeperdb_proxy.start_proxy({...})
    /// print(proxy.is_running())  # True
    /// proxy.stop()
    /// print(proxy.is_running())  # False
    /// ```
    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Context manager entry.
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Context manager exit.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &mut self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        self.stop()?;
        Ok(false)
    }

    /// String representation.
    fn __repr__(&self) -> String {
        let mode = if self.gateway_mode {
            "gateway"
        } else {
            "settings"
        };
        format!(
            "ProxyHandle(host='{}', port={}, mode='{}', running={})",
            self.host,
            self.port,
            mode,
            self.is_running()
        )
    }

    /// Get the number of active sessions (gateway mode only).
    ///
    /// Returns the number of credentials currently stored, which corresponds
    /// to active or recently active handshake sessions.
    ///
    /// # Returns
    ///
    /// Number of active sessions, or 0 if not in gateway mode.
    fn active_sessions(&self) -> usize {
        self.credential_store
            .as_ref()
            .map(|store| store.len())
            .unwrap_or(0)
    }

    /// Force-release a tunnel when the underlying transport dies.
    ///
    /// Call this method when the WebRTC tunnel disconnects but the TCP
    /// connections haven't been properly closed yet. This marks the session
    /// as disconnected, allowing new sessions to take over immediately
    /// instead of waiting for the stale cleanup to run.
    ///
    /// # Arguments
    ///
    /// * `session_uid` - The session UID (record UID) associated with the tunnel
    ///
    /// # Example
    ///
    /// ```python
    /// # When WebRTC tunnel disconnects (ICE state: Disconnected)
    /// proxy.force_release_tunnel(session_uid)
    /// # Now a new session can immediately connect to the same target
    /// ```
    fn force_release_tunnel(&self, py: Python<'_>, session_uid: String) -> PyResult<()> {
        let connection_tracker = self.connection_tracker.clone();
        let runtime = self._runtime.as_ref().ok_or_else(|| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Proxy runtime not available")
        })?;

        // Run the async method in the proxy's runtime
        py.detach(|| {
            runtime.block_on(async {
                if let Some(tracker) = connection_tracker {
                    tracker.force_release_tunnel(&session_uid).await;
                }
            });
        });

        Ok(())
    }
}

impl ProxyHandle {
    /// Start a new proxy with the given parameters.
    ///
    /// This is called from Python via start_proxy().
    ///
    /// Supports two modes based on configuration:
    /// - **Settings mode**: credentials provided → uses GatewayAuthProvider
    /// - **Gateway mode**: no credentials → uses CredentialStore + handshake
    pub fn start(db_params: &Bound<'_, PyAny>) -> PyResult<Self> {
        // Parse Python dict to PyDbParams
        let dict: &Bound<'_, PyDict> = db_params.cast()?;
        let params = PyDbParams::from_py_dict(dict)?;

        let gateway_mode = params.is_gateway_mode();

        // Build Config from params
        let config = build_config(&params);
        let config = Arc::new(config);

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        // Create a new Tokio runtime for this proxy
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to create async runtime: {}",
                    e
                ))
            })?;

        if gateway_mode {
            // Gateway mode: credentials come via handshake
            Self::start_gateway_mode(config, shutdown_tx, shutdown_rx, runtime)
        } else {
            // Settings mode: credentials provided at startup
            Self::start_settings_mode(config, &params, shutdown_tx, shutdown_rx, runtime)
        }
    }

    /// Start in settings mode (credentials provided at startup).
    fn start_settings_mode(
        config: Arc<Config>,
        params: &PyDbParams,
        shutdown_tx: broadcast::Sender<()>,
        shutdown_rx: broadcast::Receiver<()>,
        runtime: tokio::runtime::Runtime,
    ) -> PyResult<Self> {
        // Create the GatewayAuthProvider with fixed credentials
        let auth_provider = Arc::new(GatewayAuthProvider::from_params(params).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid credentials config: {}",
                e
            ))
        })?);

        // Start the listener
        let (actual_host, actual_port, running, connection_tracker) = runtime.block_on(async {
            let listener = Listener::bind(Arc::clone(&config), auth_provider, shutdown_rx)
                .await
                .map_err(|e| {
                    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                        "Failed to start proxy: {}",
                        e
                    ))
                })?;

            // Get the actual port (important when binding to port 0)
            let actual_addr = listener.local_addr().map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to get listener address: {}",
                    e
                ))
            })?;
            let actual_port = actual_addr.port();

            // Get the connection tracker before spawning the listener task
            let connection_tracker = listener.connection_tracker();

            let running = Arc::new(AtomicBool::new(true));
            let running_clone = Arc::clone(&running);

            tokio::spawn(async move {
                if let Err(e) = listener.run().await {
                    error!("Proxy error: {}", e);
                }
                running_clone.store(false, Ordering::SeqCst);
            });

            Ok::<_, PyErr>((
                config.server.listen_address.clone(),
                actual_port,
                running,
                connection_tracker,
            ))
        })?;

        info!(
            "Started proxy in settings mode on {}:{}",
            actual_host, actual_port
        );

        Ok(Self {
            host: actual_host,
            port: actual_port,
            gateway_mode: false,
            shutdown_tx: Some(shutdown_tx),
            running,
            credential_store: None,
            connection_tracker: Some(connection_tracker),
            _runtime: Some(runtime),
        })
    }

    /// Start in gateway mode (credentials via handshake).
    fn start_gateway_mode(
        config: Arc<Config>,
        shutdown_tx: broadcast::Sender<()>,
        shutdown_rx: broadcast::Receiver<()>,
        runtime: tokio::runtime::Runtime,
    ) -> PyResult<Self> {
        // Create the credential store for handshake-delivered credentials
        let credential_store = Arc::new(CredentialStore::new());

        // Start the listener in gateway mode
        let (actual_host, actual_port, running, connection_tracker) = runtime.block_on(async {
            let listener = Listener::bind_gateway_mode(
                Arc::clone(&config),
                Arc::clone(&credential_store),
                shutdown_rx,
            )
            .await
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to start proxy: {}",
                    e
                ))
            })?;

            // Get the actual port (important when binding to port 0)
            let actual_addr = listener.local_addr().map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                    "Failed to get listener address: {}",
                    e
                ))
            })?;
            let actual_port = actual_addr.port();

            // Get the connection tracker before spawning the listener task
            let connection_tracker = listener.connection_tracker();

            let running = Arc::new(AtomicBool::new(true));
            let running_clone = Arc::clone(&running);

            tokio::spawn(async move {
                if let Err(e) = listener.run().await {
                    error!("Proxy error: {}", e);
                }
                running_clone.store(false, Ordering::SeqCst);
            });

            Ok::<_, PyErr>((
                config.server.listen_address.clone(),
                actual_port,
                running,
                connection_tracker,
            ))
        })?;

        info!(
            "Started proxy in gateway mode on {}:{}",
            actual_host, actual_port
        );

        Ok(Self {
            host: actual_host,
            port: actual_port,
            gateway_mode: true,
            shutdown_tx: Some(shutdown_tx),
            running,
            credential_store: Some(credential_store),
            connection_tracker: Some(connection_tracker),
            _runtime: Some(runtime),
        })
    }
}

/// Build a Config from PyDbParams.
///
/// For gateway mode (no credentials), the targets map will be empty
/// since credentials come via handshake.
fn build_config(params: &PyDbParams) -> Config {
    use std::collections::HashMap;

    // Build targets map only in settings mode (when we have credentials)
    let targets = if !params.is_gateway_mode() {
        let protocol = match params.database_type {
            Some(crate::auth::DatabaseType::MySQL) => "mysql",
            Some(crate::auth::DatabaseType::PostgreSQL) => "postgresql",
            Some(crate::auth::DatabaseType::SQLServer) => "sqlserver",
            Some(crate::auth::DatabaseType::Oracle) => "oracle",
            None => "mysql", // Should not happen due to validation
        };

        let mut targets = HashMap::new();
        targets.insert(
            protocol.to_string(),
            TargetWithCredentials {
                host: params.target_host.clone().unwrap_or_default(),
                port: params.target_port.unwrap_or(3306),
                database: params.database.clone(),
                tls: params.tls_config.clone(),
                credentials: CredentialsConfig {
                    username: params.username.clone().unwrap_or_default(),
                    password: params.password.clone().unwrap_or_default(),
                },
            },
        );
        targets
    } else {
        // Gateway mode: empty targets, credentials come via handshake
        HashMap::new()
    };

    // Build session security config from SessionConfig
    let session = SessionSecurityConfig {
        max_duration_secs: params
            .session_config
            .max_duration
            .map(|d| d.as_secs())
            .unwrap_or(0),
        idle_timeout_secs: params
            .session_config
            .idle_timeout
            .map(|d| d.as_secs())
            .unwrap_or(300), // Default 5 minutes
        max_queries: params.session_config.max_queries.unwrap_or(0),
        max_connections_per_session: params
            .session_config
            .max_connections_per_session
            .unwrap_or(0),
        connection_grace_period_ms: params
            .session_config
            .connection_grace_period
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1000),
        application_lock_timeout_secs: params
            .session_config
            .application_lock_timeout
            .map(|d| d.as_secs())
            .unwrap_or(5), // Default 5 seconds
        require_token: params.session_config.require_token,
    };

    Config {
        server: ServerConfig {
            listen_address: params.listen_host.clone(),
            listen_port: params.listen_port,
            connect_timeout_secs: 30,
            idle_timeout_secs: params
                .session_config
                .idle_timeout
                .map(|d| d.as_secs())
                .unwrap_or(300),
            max_connections: 1000, // Default max connections
            tls: Default::default(),
        },
        targets,
        target: None,
        credentials: None,
        logging: LoggingConfig {
            level: "info".to_string(),
            protocol_debug: false,
        },
        session,
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        // Ensure cleanup on drop
        if self.running.load(Ordering::SeqCst) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            self.running.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{DatabaseType, SessionConfig};
    use crate::tls::TlsClientConfig;
    use std::time::Duration;

    /// Helper to create a PyDbParams for testing without Python runtime.
    fn make_settings_params() -> PyDbParams {
        PyDbParams {
            database_type: Some(DatabaseType::MySQL),
            target_host: Some("db.example.com".to_string()),
            target_port: Some(3306),
            username: Some("testuser".to_string()),
            password: Some("testpass".to_string()),
            database: Some("mydb".to_string()),
            auth_method: None,
            session_config: SessionConfig::default(),
            tls_config: TlsClientConfig::default(),
            listen_host: "127.0.0.1".to_string(),
            listen_port: 13306,
        }
    }

    fn make_gateway_params() -> PyDbParams {
        PyDbParams {
            database_type: None,
            target_host: None,
            target_port: None,
            username: None,
            password: None,
            database: None,
            auth_method: None,
            session_config: SessionConfig::default(),
            tls_config: TlsClientConfig::default(),
            listen_host: "127.0.0.1".to_string(),
            listen_port: 13307,
        }
    }

    #[test]
    fn test_build_config_settings_mode() {
        let params = make_settings_params();
        let config = build_config(&params);

        // Should have targets populated
        assert!(!config.targets.is_empty());
        assert!(config.targets.contains_key("mysql"));

        // Check target config
        let target = config.targets.get("mysql").unwrap();
        assert_eq!(target.host, "db.example.com");
        assert_eq!(target.port, 3306);
        assert_eq!(target.database, Some("mydb".to_string()));
        assert_eq!(target.credentials.username, "testuser");
        assert_eq!(target.credentials.password, "testpass");
    }

    #[test]
    fn test_build_config_gateway_mode() {
        let params = make_gateway_params();
        let config = build_config(&params);

        // Gateway mode should have empty targets
        assert!(config.targets.is_empty());

        // Server config should still be set
        assert_eq!(config.server.listen_address, "127.0.0.1");
        assert_eq!(config.server.listen_port, 13307);
    }

    #[test]
    fn test_build_config_server_settings() {
        let params = make_settings_params();
        let config = build_config(&params);

        assert_eq!(config.server.listen_address, "127.0.0.1");
        assert_eq!(config.server.listen_port, 13306);
        assert_eq!(config.server.connect_timeout_secs, 30);
        assert_eq!(config.server.max_connections, 1000);
    }

    #[test]
    fn test_build_config_with_session_config() {
        let mut params = make_settings_params();
        params.session_config = SessionConfig {
            max_duration: Some(Duration::from_secs(3600)),
            idle_timeout: Some(Duration::from_secs(600)),
            max_queries: Some(1000),
            max_connections_per_session: Some(5),
            connection_grace_period: Some(Duration::from_millis(500)),
            application_lock_timeout: Some(Duration::from_secs(10)),
            require_token: true,
        };

        let config = build_config(&params);

        assert_eq!(config.session.max_duration_secs, 3600);
        assert_eq!(config.session.idle_timeout_secs, 600);
        assert_eq!(config.session.max_queries, 1000);
        assert_eq!(config.session.max_connections_per_session, 5);
        assert_eq!(config.session.connection_grace_period_ms, 500);
        assert_eq!(config.session.application_lock_timeout_secs, 10);
        assert!(config.session.require_token);
    }

    #[test]
    fn test_build_config_defaults() {
        let params = make_gateway_params();
        let config = build_config(&params);

        // Check default values
        assert_eq!(config.session.max_duration_secs, 0);
        assert_eq!(config.session.idle_timeout_secs, 300); // Default 5 minutes
        assert_eq!(config.session.max_queries, 0);
        assert_eq!(config.session.max_connections_per_session, 0);
        assert_eq!(config.session.connection_grace_period_ms, 1000);
        assert_eq!(config.session.application_lock_timeout_secs, 5);
        assert!(!config.session.require_token);
    }

    #[test]
    fn test_build_config_postgresql_target() {
        let mut params = make_settings_params();
        params.database_type = Some(DatabaseType::PostgreSQL);
        params.target_port = Some(5432);

        let config = build_config(&params);

        assert!(config.targets.contains_key("postgresql"));
        assert!(!config.targets.contains_key("mysql"));

        let target = config.targets.get("postgresql").unwrap();
        assert_eq!(target.port, 5432);
    }

    #[test]
    fn test_build_config_sqlserver_target() {
        let mut params = make_settings_params();
        params.database_type = Some(DatabaseType::SQLServer);
        params.target_port = Some(1433);

        let config = build_config(&params);

        assert!(config.targets.contains_key("sqlserver"));
        assert!(!config.targets.contains_key("mysql"));

        let target = config.targets.get("sqlserver").unwrap();
        assert_eq!(target.port, 1433);
    }

    #[test]
    fn test_is_gateway_mode() {
        let settings_params = make_settings_params();
        assert!(!settings_params.is_gateway_mode());

        let gateway_params = make_gateway_params();
        assert!(gateway_params.is_gateway_mode());

        // Partial credentials = gateway mode
        let mut partial = make_settings_params();
        partial.password = None;
        assert!(partial.is_gateway_mode());
    }
}
