mod logger;
pub mod resource_manager;
pub mod webrtc_core;

#[cfg(test)]
mod tests;

mod buffer_pool;
mod channel;
mod error;
pub mod hot_path_macros;
mod metrics;
mod models;
#[cfg(feature = "python")]
mod python;
mod router_helpers;
mod runtime;
mod tube;
mod tube_and_channel_helpers;
mod tube_protocol;
mod tube_registry;
mod webrtc_data_channel;

pub use tube::*;
pub use webrtc_core::*;

#[cfg(feature = "python")]
pub use python::*;

pub use logger::initialize_logger;

#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
#[pyfunction]
pub fn test_rust_logging() {
    // First, try using the log crate directly (should work if pyo3_log is working)
    log::info!("TEST-LOG: This is a log! test message from Rust using target 'registry'");
    log::debug!("TEST-LOG: This is a log! debug message from Rust using target 'ice_config'");
    log::warn!("TEST-LOG: This is a log! warning from Rust using target 'tube_lifecycle'");
    log::error!("TEST-LOG: This is a log! error from Rust using target 'protocol_event'");

    // Then try using log (converted from tracing - should work now)
    log::info!("TEST-CONVERTED: This was a tracing! test message, now using log!");
    log::debug!("TEST-CONVERTED: This was a tracing! debug message, now using log!");
    log::warn!("TEST-CONVERTED: This was a tracing! warning, now using log!");
    log::error!("TEST-CONVERTED: This was a tracing! error, now using log!");
    log::info!("TEST-CONVERTED: This was a tracing! info message, now using log!");
}
