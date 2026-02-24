//! TCP listener for incoming database connections

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Semaphore};

use super::connection::Connection;
use super::connection_tracker::ConnectionTracker;
use crate::auth::AuthProvider;
use crate::config::Config;
use crate::error::Result;
use crate::handshake::CredentialStore;

/// Listener statistics
#[derive(Debug, Default)]
pub struct ListenerStats {
    /// Total connections accepted
    pub connections_accepted: AtomicU64,
    /// Currently active connections
    pub connections_active: AtomicU64,
    /// Connections rejected due to limit
    pub connections_rejected: AtomicU64,
}

/// TCP listener that accepts incoming database connections
pub struct Listener {
    /// TCP listener
    listener: TcpListener,
    /// Configuration
    config: Arc<Config>,
    /// Authentication provider (used in settings mode)
    auth_provider: Option<Arc<dyn AuthProvider>>,
    /// Statistics
    stats: Arc<ListenerStats>,
    /// Shutdown signal receiver
    shutdown_rx: broadcast::Receiver<()>,
    /// Connection limit semaphore (None = unlimited)
    connection_semaphore: Option<Arc<Semaphore>>,
    /// Connection tracker for single-connection enforcement
    connection_tracker: Arc<ConnectionTracker>,
    /// Credential store for gateway mode (None = settings mode)
    credential_store: Option<Arc<CredentialStore>>,
}

impl Listener {
    /// Bind to the configured address and create a new listener (settings mode).
    ///
    /// In settings mode, credentials are provided via the AuthProvider at startup.
    pub async fn bind(
        config: Arc<Config>,
        auth_provider: Arc<dyn AuthProvider>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Self> {
        let addr = format!(
            "{}:{}",
            config.server.listen_address, config.server.listen_port
        );

        let listener = TcpListener::bind(&addr).await?;

        // Create connection limit semaphore (0 = unlimited)
        let connection_semaphore = if config.server.max_connections > 0 {
            info!(
                "Listening on {} (max {} connections, settings mode)",
                addr, config.server.max_connections
            );
            Some(Arc::new(Semaphore::new(config.server.max_connections)))
        } else {
            info!(
                "Listening on {} (unlimited connections, settings mode)",
                addr
            );
            None
        };

        Ok(Self {
            listener,
            config,
            auth_provider: Some(auth_provider),
            stats: Arc::new(ListenerStats::default()),
            shutdown_rx,
            connection_semaphore,
            connection_tracker: ConnectionTracker::shared(),
            credential_store: None,
        })
    }

    /// Bind to the configured address in gateway mode.
    ///
    /// In gateway mode, credentials are delivered via Gateway handshake
    /// when each connection is established.
    pub async fn bind_gateway_mode(
        config: Arc<Config>,
        credential_store: Arc<CredentialStore>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<Self> {
        let addr = format!(
            "{}:{}",
            config.server.listen_address, config.server.listen_port
        );

        let listener = TcpListener::bind(&addr).await?;

        // Create connection limit semaphore (0 = unlimited)
        let connection_semaphore = if config.server.max_connections > 0 {
            info!(
                "Listening on {} (max {} connections, gateway mode)",
                addr, config.server.max_connections
            );
            Some(Arc::new(Semaphore::new(config.server.max_connections)))
        } else {
            info!(
                "Listening on {} (unlimited connections, gateway mode)",
                addr
            );
            None
        };

        Ok(Self {
            listener,
            config,
            auth_provider: None,
            stats: Arc::new(ListenerStats::default()),
            shutdown_rx,
            connection_semaphore,
            connection_tracker: ConnectionTracker::shared(),
            credential_store: Some(credential_store),
        })
    }

    /// Get listener statistics
    pub fn stats(&self) -> Arc<ListenerStats> {
        Arc::clone(&self.stats)
    }

    /// Get the connection tracker.
    ///
    /// This is useful for external components that need to manage the connection
    /// tracker, such as notifying when a tunnel dies.
    pub fn connection_tracker(&self) -> Arc<ConnectionTracker> {
        Arc::clone(&self.connection_tracker)
    }

    /// Get the local address the listener is bound to.
    ///
    /// This is useful when binding to port 0 to get an OS-assigned port.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Run the accept loop
    pub async fn run(mut self) -> Result<()> {
        // Start the background cleanup task for stale sessions
        let cleanup_shutdown_rx = self.shutdown_rx.resubscribe();
        let _cleanup_handle = ConnectionTracker::start_cleanup_task(
            Arc::clone(&self.connection_tracker),
            cleanup_shutdown_rx,
        );
        debug!("Started background session cleanup task");

        loop {
            tokio::select! {
                // Accept new connections
                result = self.listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            // Try to acquire a connection permit if limiting is enabled
                            let permit = if let Some(ref semaphore) = self.connection_semaphore {
                                match semaphore.clone().try_acquire_owned() {
                                    Ok(permit) => Some(permit),
                                    Err(_) => {
                                        // Connection limit reached
                                        warn!(
                                            "Connection from {} rejected: max connections ({}) reached",
                                            addr,
                                            self.config.server.max_connections
                                        );
                                        self.stats.connections_rejected.fetch_add(1, Ordering::Relaxed);
                                        // Drop the stream immediately
                                        drop(stream);
                                        continue;
                                    }
                                }
                            } else {
                                None
                            };

                            debug!("Accepted connection from {}", addr);
                            self.stats.connections_accepted.fetch_add(1, Ordering::Relaxed);
                            self.stats.connections_active.fetch_add(1, Ordering::Relaxed);

                            // Spawn a task to handle the connection
                            let config = Arc::clone(&self.config);
                            let auth_provider = self.auth_provider.clone();
                            let credential_store = self.credential_store.clone();
                            let stats = Arc::clone(&self.stats);
                            let shutdown_rx = self.shutdown_rx.resubscribe();
                            let connection_tracker = Arc::clone(&self.connection_tracker);

                            tokio::spawn(async move {
                                // Hold permit for connection lifetime (drops when connection closes)
                                let _permit = permit;

                                if let Err(e) = Self::handle_connection(
                                    stream,
                                    addr,
                                    config,
                                    auth_provider,
                                    credential_store,
                                    shutdown_rx,
                                    connection_tracker,
                                ).await {
                                    warn!("Connection from {} error: {}", addr, e);
                                }
                                stats.connections_active.fetch_sub(1, Ordering::Relaxed);
                                debug!("Connection from {} closed", addr);
                            });
                        }
                        Err(e) => {
                            error!("Accept error: {}", e);
                            // Brief delay before retrying
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                }

                // Shutdown signal
                _ = self.shutdown_rx.recv() => {
                    info!("Shutdown signal received, stopping listener");
                    break;
                }
            }
        }

        info!(
            "Listener stopped. Total: {}, Active: {}, Rejected: {}",
            self.stats.connections_accepted.load(Ordering::Relaxed),
            self.stats.connections_active.load(Ordering::Relaxed),
            self.stats.connections_rejected.load(Ordering::Relaxed)
        );

        Ok(())
    }

    /// Handle a single connection.
    ///
    /// Creates the appropriate Connection type based on mode:
    /// - Settings mode: uses auth_provider for credential lookup
    /// - Gateway mode: uses credential_store for handshake-delivered credentials
    async fn handle_connection(
        stream: TcpStream,
        addr: std::net::SocketAddr,
        config: Arc<Config>,
        auth_provider: Option<Arc<dyn AuthProvider>>,
        credential_store: Option<Arc<CredentialStore>>,
        shutdown_rx: broadcast::Receiver<()>,
        connection_tracker: Arc<ConnectionTracker>,
    ) -> Result<()> {
        let connection = match (auth_provider, credential_store) {
            // Settings mode: auth_provider is present
            (Some(auth_provider), _) => Connection::new(
                stream,
                addr,
                config,
                auth_provider,
                shutdown_rx,
                connection_tracker,
            ),
            // Gateway mode: credential_store is present
            (None, Some(credential_store)) => Connection::new_gateway_mode(
                stream,
                addr,
                config,
                credential_store,
                shutdown_rx,
                connection_tracker,
            ),
            // Neither - this shouldn't happen
            (None, None) => {
                error!("Connection rejected: no auth_provider or credential_store configured");
                return Err(crate::error::ProxyError::Config(
                    "Listener not properly configured".into(),
                ));
            }
        };
        connection.handle().await
    }
}
