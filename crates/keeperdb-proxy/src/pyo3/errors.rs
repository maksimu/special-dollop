//! Error handling for PyO3 bindings.
//!
//! This module provides conversions from Rust errors to Python exceptions.

use pyo3::exceptions::{PyConnectionError, PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::PyErr;

use crate::error::ProxyError;

/// Convert a ProxyError to a Python exception.
///
/// Maps Rust error types to appropriate Python exception types:
/// - Config errors → ValueError
/// - I/O errors → ConnectionError or RuntimeError
/// - Auth errors → ValueError
/// - Timeout errors → TimeoutError
/// - Other errors → RuntimeError
impl From<ProxyError> for PyErr {
    fn from(err: ProxyError) -> PyErr {
        match err {
            // Configuration and validation errors
            ProxyError::Config(msg) => {
                PyValueError::new_err(format!("Configuration error: {}", msg))
            }

            // Authentication errors (likely bad credentials)
            ProxyError::Auth(msg) => {
                PyValueError::new_err(format!("Authentication error: {}", msg))
            }

            // Protocol errors (invalid input)
            ProxyError::Protocol(msg) => PyValueError::new_err(format!("Protocol error: {}", msg)),

            // Unsupported features (bad input)
            ProxyError::UnsupportedProtocol(msg) => {
                PyValueError::new_err(format!("Unsupported protocol: {}", msg))
            }
            ProxyError::UnsupportedAuthMethod(msg) => {
                PyValueError::new_err(format!("Unsupported auth method: {}", msg))
            }

            // Connection errors
            ProxyError::Io(err) => PyConnectionError::new_err(format!("I/O error: {}", err)),
            ProxyError::Connection(msg) => {
                PyConnectionError::new_err(format!("Connection error: {}", msg))
            }

            // Timeout errors
            ProxyError::Timeout(msg) => PyTimeoutError::new_err(format!("Timeout: {}", msg)),

            // Token validation (auth error)
            ProxyError::TokenValidation(msg) => {
                PyValueError::new_err(format!("Token validation failed: {}", msg))
            }

            // Credential retrieval (runtime error)
            ProxyError::CredentialRetrieval(msg) => {
                PyRuntimeError::new_err(format!("Failed to retrieve credentials: {}", msg))
            }

            // TLS errors
            ProxyError::Tls(err) => PyConnectionError::new_err(format!("TLS error: {}", err)),

            // Session limits
            ProxyError::SessionLimit(msg) => {
                PyRuntimeError::new_err(format!("Session limit: {}", msg))
            }
        }
    }
}

/// Helper to convert a Result<T, ProxyError> to PyResult<T>.
///
/// # Example
///
/// ```ignore
/// use keeperdb_proxy::pyo3::errors::to_py_result;
///
/// let result: Result<(), ProxyError> = some_operation();
/// let py_result: PyResult<()> = to_py_result(result);
/// ```
#[allow(dead_code)]
pub fn to_py_result<T>(result: crate::error::Result<T>) -> pyo3::PyResult<T> {
    result.map_err(PyErr::from)
}

// Note: Tests that verify PyErr types require Python linking and cannot be
// run via `cargo test`. Error type mapping is verified by Python integration
// tests in tests/python/test_error_handling.py instead.
