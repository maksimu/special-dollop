// Channel module - provides WebRTC data channel integration with TCP connections

// Internal modules
pub(crate) mod core;
mod server;
pub(crate) mod connections;
pub(crate) mod frame_handling; // Logic to be merged into core.rs
mod utils;
pub mod types; // Added new types module
mod connect_as; // Added a new connect_as module

// Re-export the main Channel struct to maintain API compatibility
pub use core::Channel;

pub(crate) mod protocol;
pub(crate) mod guacd_parser;