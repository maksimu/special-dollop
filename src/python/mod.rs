pub mod channel_logger;
mod connectivity;
mod enums;
pub mod handler_task;
pub mod logger_task;
mod signal_handler;
mod tube_registry_binding;
mod utils;

use crate::runtime::shutdown_runtime_from_python;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::wrap_pyfunction;
use pyo3::Bound;

pub use enums::PyCloseConnectionReason;
pub use tube_registry_binding::PyTubeRegistry;

#[pymodule]
pub fn keeper_pam_webrtc_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize rustls crypto provider for DTLS - CRITICAL for WebRTC to work!
    // This must be called before any DTLS connections are attempted
    use std::sync::Once;
    static INIT_CRYPTO: Once = Once::new();
    INIT_CRYPTO.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider - required for DTLS/WebRTC");
    });

    m.add_class::<PyTubeRegistry>()?;
    m.add_class::<PyCloseConnectionReason>()?;

    // Explicitly import and wrap the Python-specific logger initializer
    #[cfg(feature = "python")]
    {
        use crate::logger::{initialize_logger as py_initialize_logger, set_verbose_logging};
        m.add_function(wrap_pyfunction!(py_initialize_logger, py)?)?;
        m.add_function(wrap_pyfunction!(set_verbose_logging, py)?)?;
    }

    // Add runtime shutdown function for Python cleanup
    m.add_function(wrap_pyfunction!(shutdown_runtime_from_python, py)?)?;

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
