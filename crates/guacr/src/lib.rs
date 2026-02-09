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

// guacd wire protocol compatibility (select/args/connect/ready handshake)
#[cfg(feature = "guacd-compat")]
pub use guacr_handlers::{
    get_protocol_arg_names, get_protocol_args, handle_guacd_connection,
    handle_guacd_connection_with_timeout, protocol_args, run_guacd_server, status, ArgDescriptor,
    ConnectResult, GuacdHandshake, GuacdServer, HandlerArg, HandshakeError, SelectResult,
    DEFAULT_HANDSHAKE_TIMEOUT_SECS, GUACAMOLE_PROTOCOL_VERSION, GUACD_DEFAULT_PORT,
    MAX_INSTRUCTION_SIZE,
};
