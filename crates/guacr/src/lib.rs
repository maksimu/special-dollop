//! # guacr - Protocol Handlers for Remote Desktop Access
//!
//! This crate provides protocol handlers for various remote desktop and database protocols,
//! designed to work with WebRTC-based secure tunneling systems.
//!
//! ## Supported Protocols
//!
//! - **SSH** - Secure Shell protocol handler
//! - **Telnet** - Telnet protocol handler
//! - **RDP** - Remote Desktop Protocol handler (IronRDP, optional)
//! - **VNC** - Virtual Network Computing handler (optional)
//! - **SFTP** - SSH File Transfer Protocol handler (optional)
//! - **Database** - Database protocol handlers (MySQL, PostgreSQL, MongoDB, Redis, etc.) (optional)
//! - **RBI** - Remote Browser Isolation handler (optional)
//!
//! ## Usage
//!
//! ```no_run
//! use guacr::{ProtocolHandlerRegistry, ProtocolHandler};
//!
//! // Create a registry
//! let registry = ProtocolHandlerRegistry::new();
//!
//! // Register handlers
//! # #[cfg(feature = "ssh")]
//! registry.register(guacr::ssh::SshHandler::with_defaults());
//! # #[cfg(feature = "telnet")]
//! registry.register(guacr::telnet::TelnetHandler::with_defaults());
//!
//! // Get a handler
//! # #[cfg(feature = "ssh")]
//! let handler = registry.get("ssh").expect("SSH handler not found");
//! ```

// Re-export async_trait for implementing EventCallback
pub use async_trait::async_trait;

// Re-export core handler traits and registry
pub use guacr_handlers::{
    handle_guacd_with_handlers,
    // Zero-copy event-based interface
    EventBasedHandler,
    EventCallback,
    // Error type (needed for EventCallback implementations)
    HandlerError,
    HandlerEvent,
    HandlerStats,
    HealthStatus,
    InstructionSender,
    ProtocolHandler,
    ProtocolHandlerRegistry,
};

// Re-export protocol handlers
#[cfg(feature = "ssh")]
pub use guacr_ssh as ssh;

#[cfg(feature = "telnet")]
pub use guacr_telnet as telnet;

#[cfg(feature = "rdp")]
pub use guacr_rdp as rdp;

#[cfg(feature = "vnc")]
pub use guacr_vnc as vnc;

#[cfg(feature = "sftp")]
pub use guacr_sftp as sftp;

#[cfg(feature = "database")]
pub use guacr_database as database;

#[cfg(feature = "rbi")]
pub use guacr_rbi as rbi;

use std::sync::Arc;

/// Create a protocol handler registry with all available built-in handlers registered.
///
/// Registers handlers for all protocols enabled via feature flags (SSH, Telnet,
/// RDP, VNC, Database, SFTP, RBI). Handlers remain dormant until actually invoked.
pub fn create_default_registry() -> Arc<ProtocolHandlerRegistry> {
    let registry = Arc::new(ProtocolHandlerRegistry::new());

    #[cfg(feature = "ssh")]
    registry.register(ssh::SshHandler::with_defaults());

    #[cfg(feature = "telnet")]
    registry.register(telnet::TelnetHandler::with_defaults());

    #[cfg(feature = "rdp")]
    registry.register(rdp::RdpHandler::with_defaults());

    #[cfg(feature = "vnc")]
    registry.register(vnc::VncHandler::with_defaults());

    #[cfg(feature = "database")]
    {
        registry.register(database::MySqlHandler::with_defaults());
        registry.register(database::PostgreSqlHandler::with_defaults());
        registry.register(database::SqlServerHandler::with_defaults());
        registry.register(database::MongoDbHandler::with_defaults());
        registry.register(database::RedisHandler::with_defaults());
        registry.register(database::OracleHandler::with_defaults());
        registry.register(database::MariaDbHandler::with_defaults());
    }

    #[cfg(feature = "rbi")]
    registry.register(rbi::RbiHandler::with_defaults());

    registry
}

// guacd wire protocol compatibility (select/args/connect/ready handshake)
// Now provided by guacr-guacd crate (previously in guacr-handlers behind feature flag)
#[cfg(feature = "guacd-compat")]
pub use guacr_guacd::{
    get_protocol_arg_names, get_protocol_args, handle_guacd_connection,
    handle_guacd_connection_with_timeout, protocol_args, run_guacd_server, status, ArgDescriptor,
    ConnectResult, GuacdHandshake, GuacdServer, HandshakeError, SelectResult,
    DEFAULT_HANDSHAKE_TIMEOUT_SECS, GUACAMOLE_PROTOCOL_VERSION, GUACD_DEFAULT_PORT,
    MAX_INSTRUCTION_SIZE,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_default_registry() {
        let registry = create_default_registry();
        // With default features (ssh, telnet), at least those two should be registered
        assert!(registry.count() >= 2);
    }
}
