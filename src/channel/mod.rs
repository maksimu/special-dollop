// Channel module - provides WebRTC data channel integration with TCP connections

// Internal modules
mod core;
mod server;
mod protocol;
mod connections;
mod frame_handling;
mod utils;
pub mod protocols;

// Re-export main Channel struct to maintain API compatibility
pub use core::Channel;

// Re-export protocol factory to maintain API compatibility
pub use protocols::create_protocol_handler;
