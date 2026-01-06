// Channel module - provides WebRTC data channel integration with TCP connections

// Internal modules
pub mod adaptive_pool; // Adaptive channel pool with fill-then-overflow strategy
pub(crate) mod assembler; // Multi-channel fragmentation/reassembly
mod connect_as;
pub(crate) mod connections;
pub(crate) mod core;
pub(crate) mod frame_handling; // Logic to be merged into core.rs
#[cfg(feature = "handlers")]
pub(crate) mod handler_connections;
mod server;
pub mod types; // Added new types module
mod utils; // Added a new connect_as module

// Re-export the main Channel struct to maintain API compatibility
pub use core::Channel;
pub use core::PythonHandlerMessage;

// Re-export adaptive pool for multi-channel management
#[allow(unused_imports)]
pub use adaptive_pool::{AdaptiveChannelPool, OverflowChannelStats, PoolConfig, PoolStats};

// Re-export Assembler and fragmentation helpers for use by Channel
#[allow(unused_imports)]
pub use assembler::{
    fragment_frame, has_fragment_header, should_fragment, Assembler, AssemblerConfig,
    AssemblerState, FragmentBuffer, FragmentHeader, DEFAULT_FRAGMENT_THRESHOLD,
    DEFAULT_MAX_FRAGMENTS, FRAGMENT_HEADER_SIZE,
};

pub(crate) mod guacd_parser;
pub(crate) mod protocol;
pub(crate) mod socks5;
