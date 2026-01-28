pub mod channel_logger;
mod connectivity;
mod enums;
pub mod handler_task;
pub mod logger_task;
mod signal_handler;
mod tube_registry_binding;
mod utils;

use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::Bound;

pub use enums::PyCloseConnectionReason;
pub use tube_registry_binding::PyTubeRegistry;

/// Register WebRTC functionality into a parent Python module.
/// This allows the unified bindings crate to aggregate all functionality.
/// 
/// Called by the unified python-bindings crate to expose WebRTC classes/functions
/// under the keeper_pam_connections module.
pub fn register_webrtc_module(_py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize rustls crypto provider for DTLS - CRITICAL for WebRTC to work!
    // This must be called before any DTLS connections are attempted
    use std::sync::Once;
    static INIT_CRYPTO: Once = Once::new();
    INIT_CRYPTO.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider - required for DTLS/WebRTC");
    });

    // Register classes
    parent.add_class::<PyTubeRegistry>()?;
    parent.add_class::<PyCloseConnectionReason>()?;

    // Register functions
    use crate::logger::{initialize_logger as py_initialize_logger, set_verbose_logging};
    use crate::runtime::shutdown_runtime_from_python;
    
    parent.add_function(pyo3::wrap_pyfunction!(py_initialize_logger, parent)?)?;
    parent.add_function(pyo3::wrap_pyfunction!(set_verbose_logging, parent)?)?;
    parent.add_function(pyo3::wrap_pyfunction!(shutdown_runtime_from_python, parent)?)?;

    Ok(())
}
