mod tube_registry_binding;

use crate::runtime::get_runtime;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use pyo3::wrap_pyfunction;
use pyo3::Bound;
pub use tube_registry_binding::PyTubeRegistry;

#[pymodule]
pub fn keeper_pam_webrtc_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTubeRegistry>()?;

    // Initialize the Tokio runtime at module import time
    let _ = get_runtime();

    // Explicitly import and wrap the Python-specific logger initializer
    #[cfg(feature = "python")]
    {
        use crate::logger::initialize_logger as py_initialize_logger;
        m.add_function(wrap_pyfunction!(py_initialize_logger, py)?)?;
    }

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
