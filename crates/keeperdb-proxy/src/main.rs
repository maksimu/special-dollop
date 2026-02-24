//! keeperdb-proxy - Database protocol proxy with credential injection
//!
//! This binary provides a standalone database proxy that:
//! - Detects database protocols (MySQL, PostgreSQL, SQL Server)
//! - Injects configured credentials during authentication
//! - Relays all traffic transparently after authentication

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::broadcast;
use tracing::{error, info};

use keeperdb_proxy::{config, CredentialStore, Listener, Result, StaticAuthProvider};

#[derive(Parser)]
#[command(name = "keeperdb-proxy")]
#[command(version = "0.1.0")]
#[command(about = "Database protocol proxy with credential injection")]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: PathBuf,

    /// Override listen address
    #[arg(long)]
    listen_address: Option<String>,

    /// Override listen port
    #[arg(long)]
    listen_port: Option<u16>,

    /// Enable verbose/debug logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    // Priority: --verbose flag, then RUST_LOG env var, then default "info"
    let log_level = if cli.verbose {
        "debug".to_string()
    } else {
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string())
    };
    tracing_subscriber::fmt().with_env_filter(&log_level).init();

    info!("Starting keeperdb-proxy v{}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let mut config = config::load_config(&cli.config)?;
    info!("Loaded configuration from {:?}", cli.config);

    // Apply CLI overrides
    if let Some(addr) = cli.listen_address {
        config.server.listen_address = addr;
    }
    if let Some(port) = cli.listen_port {
        config.server.listen_port = port;
    }

    let config = Arc::new(config);

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    // Determine mode: gateway (no credentials) or settings (with credentials)
    let is_gateway_mode = !config.has_credentials();

    let listener = if is_gateway_mode {
        // Gateway mode: credentials come from Gateway handshake
        let credential_store = Arc::new(CredentialStore::new());
        info!("Starting in gateway mode (credentials via handshake)");
        Listener::bind_gateway_mode(Arc::clone(&config), credential_store, shutdown_rx).await?
    } else {
        // Settings mode: credentials from config
        let auth_provider = Arc::new(StaticAuthProvider::from_config(&config)?);
        info!("Starting in settings mode (credentials from config)");
        Listener::bind(Arc::clone(&config), auth_provider, shutdown_rx).await?
    };

    let stats = listener.stats();

    // Log target information based on config mode
    if is_gateway_mode {
        info!(
            "Proxy ready: listening on {}:{} (gateway mode - awaiting handshake)",
            config.server.listen_address, config.server.listen_port,
        );
    } else if config.is_multi_target() {
        info!(
            "Proxy ready: listening on {}:{} (multi-target mode)",
            config.server.listen_address, config.server.listen_port,
        );
        for (protocol, target) in &config.targets {
            info!("  {} -> {}:{}", protocol, target.host, target.port);
        }
    } else if let Some(target) = config.get_single_target() {
        info!(
            "Proxy ready: listening on {}:{} -> {}:{}",
            config.server.listen_address, config.server.listen_port, target.host, target.port
        );
    }

    // Spawn the listener task
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = listener.run().await {
            error!("Listener error: {}", e);
        }
    });

    // Wait for shutdown signal
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received Ctrl+C, initiating shutdown...");
        }
        _ = async {
            #[cfg(unix)]
            {
                let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                    .expect("Failed to install SIGTERM handler");
                sigterm.recv().await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        } => {
            info!("Received SIGTERM, initiating shutdown...");
        }
    }

    // Send shutdown signal
    let _ = shutdown_tx.send(());

    // Wait for listener to finish
    let _ = listener_handle.await;

    info!(
        "Shutdown complete. Total connections handled: {}",
        stats
            .connections_accepted
            .load(std::sync::atomic::Ordering::Relaxed)
    );

    Ok(())
}
