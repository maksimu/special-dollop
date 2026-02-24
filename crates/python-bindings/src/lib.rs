use pyo3::prelude::*;
use pyo3::types::PyModule;

/// Unified Python module that aggregates all Keeper PAM functionality.
///
/// This module serves as the single entry point for all connection management
/// functionality, aggregating WebRTC and future protocol-specific modules.
///
/// Usage:
/// ```python
/// import keeper_pam_connections
///
/// # Access WebRTC functionality
/// registry = keeper_pam_connections.PyTubeRegistry()
///
/// # Future: Access other protocol-specific functionality
/// # All available through the same import
/// ```
#[pymodule]
fn keeper_pam_connections(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register WebRTC functionality from keeper-pam-webrtc-rs
    keeper_pam_webrtc_rs::python::register_webrtc_module(py, m)?;

    // Register keeperdb-proxy database proxy functionality
    keeperdb_proxy::pyo3::register_keeperdb_proxy_module(py, m)?;

    // Future: Add other protocol-specific modules here
    // Example:
    // keeper_pam_ssh_rs::python::register_ssh_module(py, m)?;
    // keeper_pam_rdp_rs::python::register_rdp_module(py, m)?;

    // Set version from this crate (unified version)
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    Ok(())
}
