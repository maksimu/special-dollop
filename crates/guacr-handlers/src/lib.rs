// guacr-handlers: Protocol handler trait and registry for remote desktop protocols
//
// This crate defines the ProtocolHandler trait that all protocol implementations
// (SSH, RDP, VNC, Database, RBI) must implement, along with the registry for
// managing and dispatching to handlers.

mod error;
mod events;
mod handler;
mod integration;
mod multi_channel;
mod registry;

#[cfg(test)]
mod mock;

pub use error::{HandlerError, Result};
pub use events::{EventBasedHandler, EventCallback, HandlerEvent, InstructionSender};
pub use handler::{HandlerStats, HealthStatus, ProtocolHandler};
pub use integration::handle_guacd_with_handlers;
pub use multi_channel::{SimpleMultiChannelSender, WebRTCDataChannel};
pub use registry::ProtocolHandlerRegistry;

#[cfg(test)]
pub use mock::MockProtocolHandler;
