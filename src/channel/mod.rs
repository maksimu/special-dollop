// Channel module - provides WebRTC data channel integration with TCP connections

// Internal modules
mod connect_as;
pub(crate) mod connections;
pub(crate) mod core;
pub(crate) mod frame_handling; // Logic to be merged into core.rs
mod server;
pub mod types; // Added new types module
mod utils; // Added a new connect_as module

// Re-export the main Channel struct to maintain API compatibility
pub use core::Channel;

pub(crate) mod guacd_parser;
pub(crate) mod protocol;
pub(crate) mod socks5;
pub(crate) mod udp;
