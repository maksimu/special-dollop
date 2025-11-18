// guacr-handlers: Protocol handler trait and registry for remote desktop protocols
//
// This crate defines the ProtocolHandler trait that all protocol implementations
// (SSH, RDP, VNC, Database, RBI) must implement, along with the registry for
// managing and dispatching to handlers.

mod error;
mod handler;
mod integration;
mod registry;

#[cfg(test)]
mod mock;

pub use error::{HandlerError, Result};
pub use handler::{HandlerStats, HealthStatus, ProtocolHandler};
pub use integration::handle_guacd_with_handlers;
pub use registry::ProtocolHandlerRegistry;

#[cfg(test)]
pub use mock::MockProtocolHandler;
