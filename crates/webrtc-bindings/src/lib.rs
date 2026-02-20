use pyo3::prelude::*;

#[pymodule]
fn keeper_pam_webrtc_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    webrtc_core::python::register_webrtc_module(py, m)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
