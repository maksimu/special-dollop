use crate::router_helpers::{get_relay_access_creds, post_connection_state};
use crate::runtime::{get_runtime, shutdown_runtime_from_python};
use crate::tube_protocol::CloseConnectionReason;
use crate::tube_registry::REGISTRY;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyFloat, PyInt, PyList, PyNone, PyString};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, error, info, trace, warn};

/// Helper function to safely execute async code from Python bindings
fn safe_python_async_execute<F, R>(py: Python<'_>, future: F) -> R
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    py.allow_threads(|| {
        // Check if we're already in a runtime context
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // We're in a runtime context, spawn the task and wait for it
            let (tx, rx) = std::sync::mpsc::channel();
            handle.spawn(async move {
                let result = future.await;
                let _ = tx.send(result);
            });
            // Block on the std channel receiver (safe in runtime context)
            rx.recv().expect("Task failed to complete")
        } else {
            // We're not in a runtime, safe to use block_on
            let runtime = get_runtime();
            runtime.block_on(future)
        }
    })
}

/// Python bindings for the Rust TubeRegistry.
///
/// This module provides a thin wrapper around the Rust TubeRegistry implementation with
/// additional functionality for Python signal callbacks. The main differences are:
///
/// 1. Signal channels are automatically created and managed
/// 2. Signal messages are forwarded to Python callbacks
/// 3. Python callbacks receive signals as dictionaries with the following keys:
///    - tube_id: The ID of the tube that generated the signal
///    - kind: The type of signal (e.g., "icecandidate", "answer", etc.)
///    - data: The data payload of the signal
///    - conversation_id: The conversation ID associated with the signal
///
/// Usage example:
///
/// ```python
/// from keeper_pam_webrtc_rs import PyTubeRegistry
///
/// # Create a registry
/// registry = PyTubeRegistry()
///
/// # Define a signal callback
/// def on_signal(signal_dict):
///     print(f"Received signal: {signal_dict}")
///
/// # Create a tube with the callback
/// result = registry.create_tube(
///     conversation_id="my_conversation",
///     settings={"use_turn": True},
///     trickle_ice=True,
///     callback_token="my_token",
///     ksm_config="my_config",
///     signal_callback=on_signal
/// )
/// ```
// Helper function to convert any PyAny to serde_json::Value
fn py_any_to_json_value(py_obj: &Bound<PyAny>) -> PyResult<serde_json::Value> {
    if py_obj.is_instance_of::<PyDict>() {
        let dict = py_obj.downcast::<PyDict>()?;
        let mut map = serde_json::Map::new();
        for (key, value) in dict.iter() {
            let key_str = key
                .extract::<String>()
                .map_err(|e| PyRuntimeError::new_err(format!("Dict key is not a string: {e}")))?;
            map.insert(key_str, py_any_to_json_value(&value)?);
        }
        Ok(serde_json::Value::Object(map))
    } else if py_obj.is_instance_of::<PyList>() {
        let list = py_obj.downcast::<PyList>()?;
        let mut vec = Vec::new();
        for item in list.iter() {
            vec.push(py_any_to_json_value(&item)?);
        }
        Ok(serde_json::Value::Array(vec))
    } else if py_obj.is_instance_of::<PyString>() {
        Ok(serde_json::Value::String(py_obj.extract::<String>()?))
    } else if py_obj.is_instance_of::<PyBool>() {
        Ok(serde_json::Value::Bool(py_obj.extract::<bool>()?))
    } else if py_obj.is_instance_of::<PyInt>() {
        // Python int can be large. Try i64, then u64.
        // If it's too large for Rust's 64-bit integers, serde_json will handle it
        // as a Number which can represent larger values or fallback to float if necessary.
        if let Ok(val) = py_obj.extract::<i64>() {
            Ok(serde_json::Value::Number(serde_json::Number::from(val)))
        } else if let Ok(val) = py_obj.extract::<u64>() {
            Ok(serde_json::Value::Number(serde_json::Number::from(val)))
        } else {
            // For very large integers that don't fit i64/u64, PyO3 might allow extraction as f64
            // or you might need a specific BigInt handling if precision is paramount for extremely large numbers
            // not representable by f64. For typical numeric parameters in JSON, f64 is often acceptable.
            let val_f64 = py_obj.extract::<f64>()?;
            serde_json::Number::from_f64(val_f64)
                .map(serde_json::Value::Number)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "Failed to convert large Python int to JSON number: {py_obj:?}"
                    ))
                })
        }
    } else if py_obj.is_instance_of::<PyFloat>() {
        serde_json::Number::from_f64(py_obj.extract::<f64>()?)
            .map(serde_json::Value::Number)
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "Failed to convert float to JSON number: {py_obj:?}"
                ))
            })
    } else if py_obj.is_none() || py_obj.is_instance_of::<PyNone>() {
        Ok(serde_json::Value::Null)
    } else {
        let type_name = py_obj.get_type().name()?;
        warn!(target: "python_bindings", "py_any_to_json_value: Unhandled Python type '{}', falling back to string conversion for value: {:?}", type_name, py_obj);
        let str_val = py_obj.str()?.extract::<String>()?;
        Ok(serde_json::Value::String(str_val))
    }
}

// Convert a Python dictionary (PyObject) to HashMap<String, serde_json::Value>
fn pyobj_to_json_hashmap(
    py: Python<'_>,
    dict_obj: &PyObject,
) -> PyResult<HashMap<String, serde_json::Value>> {
    let bound_settings_obj = dict_obj.bind(py);

    if !bound_settings_obj.is_instance_of::<PyDict>() {
        return Err(PyRuntimeError::new_err(
            "Settings parameter must be a dictionary.",
        ));
    }

    match py_any_to_json_value(bound_settings_obj)? {
        serde_json::Value::Object(map) => {
            // Convert serde_json::Map to HashMap<String, serde_json::Value>
            // This is mostly a type conversion, the structure is already correct.
            Ok(map.into_iter().collect())
        }
        _ => {
            // This case should ideally not be reached if the input is confirmed to be PyDict
            // and py_any_to_json_value handles PyDict correctly.
            Err(PyRuntimeError::new_err(
                "Failed to convert Python dictionary to a Rust HashMap<String, JsonValue>.",
            ))
        }
    }
}

#[pyclass]
pub struct PyTubeRegistry {
    /// Track whether explicit cleanup was called
    explicit_cleanup_called: AtomicBool,
}

/// Python-accessible enum for CloseConnectionReason
///
/// This enum represents the various reasons why a connection might be closed.
/// It can be used when calling close_connection() or close_tube() methods.
///
/// Example:
///     reason = PyCloseConnectionReason.Normal
///     registry.close_connection("connection_id", reason.value())
#[pyclass]
#[derive(Clone, Copy)]
pub enum PyCloseConnectionReason {
    Normal = 0,
    Error = 1,
    Timeout = 2,
    ServerRefuse = 4,
    Client = 5,
    Unknown = 6,
    InvalidInstruction = 7,
    GuacdRefuse = 8,
    ConnectionLost = 9,
    ConnectionFailed = 10,
    TunnelClosed = 11,
    AdminClosed = 12,
    ErrorRecording = 13,
    GuacdError = 14,
    AIClosed = 15,
    AddressResolutionFailed = 16,
    DecryptionFailed = 17,
    ConfigurationError = 18,
    ProtocolError = 19,
    UpstreamClosed = 20,
}

#[pymethods]
impl PyCloseConnectionReason {
    /// Get the numeric value of the reason
    fn value(&self) -> u16 {
        *self as u16
    }

    /// Create from a numeric code
    #[staticmethod]
    fn from_code(code: u16) -> Self {
        match code {
            0 => PyCloseConnectionReason::Normal,
            1 => PyCloseConnectionReason::Error,
            2 => PyCloseConnectionReason::Timeout,
            4 => PyCloseConnectionReason::ServerRefuse,
            5 => PyCloseConnectionReason::Client,
            6 => PyCloseConnectionReason::Unknown,
            7 => PyCloseConnectionReason::InvalidInstruction,
            8 => PyCloseConnectionReason::GuacdRefuse,
            9 => PyCloseConnectionReason::ConnectionLost,
            10 => PyCloseConnectionReason::ConnectionFailed,
            11 => PyCloseConnectionReason::TunnelClosed,
            12 => PyCloseConnectionReason::AdminClosed,
            13 => PyCloseConnectionReason::ErrorRecording,
            14 => PyCloseConnectionReason::GuacdError,
            15 => PyCloseConnectionReason::AIClosed,
            16 => PyCloseConnectionReason::AddressResolutionFailed,
            17 => PyCloseConnectionReason::DecryptionFailed,
            18 => PyCloseConnectionReason::ConfigurationError,
            19 => PyCloseConnectionReason::ProtocolError,
            20 => PyCloseConnectionReason::UpstreamClosed,
            _ => PyCloseConnectionReason::Unknown,
        }
    }
}

#[pymethods]
impl PyTubeRegistry {
    #[new]
    fn new() -> Self {
        Self {
            explicit_cleanup_called: AtomicBool::new(false),
        }
    }

    /// Clean up all tubes and resources in the registry
    fn cleanup_all(&self, py: Python<'_>) -> PyResult<()> {
        // Mark that explicit cleanup was called
        self.explicit_cleanup_called.store(true, Ordering::SeqCst);

        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let mut registry = REGISTRY.write().await;
                let tube_ids: Vec<String> = registry.tubes_by_id.keys().cloned().collect();
                debug!(target: "python_bindings", tube_count = tube_ids.len(), "Starting explicit cleanup of all tubes");
                // Close all tubes (this will also clean up their signal channels)
                for tube_id in tube_ids {
                    if let Err(e) = registry.close_tube(&tube_id, Some(CloseConnectionReason::Normal)).await {
                        error!(target: "python_bindings", tube_id = %tube_id, "Failed to close tube during cleanup: {}", e);
                    }
                }
                // Clear any remaining mappings (should be empty after close_tube calls)
                registry.conversation_mappings.clear();
                registry.signal_channels.clear();
                debug!(target: "python_bindings", "Registry cleanup complete");
            })
        });

        // Now shutdown the runtime - this ensures all async tasks are properly terminated
        debug!(target: "python_bindings", "Shutting down runtime after tube cleanup");
        crate::runtime::shutdown_runtime_from_python();

        Ok(())
    }

    /// Clean up specific tubes by ID
    fn cleanup_tubes(&self, py: Python<'_>, tube_ids: Vec<String>) -> PyResult<()> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let mut registry = REGISTRY.write().await;
                debug!(target: "python_bindings", tube_count = tube_ids.len(), "Starting cleanup of specific tubes");
                for tube_id in tube_ids {
                    if let Err(e) = registry.close_tube(&tube_id, Some(CloseConnectionReason::Normal)).await {
                        error!(target: "python_bindings", tube_id = %tube_id, "Failed to close tube during selective cleanup: {}", e);
                    }
                }
                Ok(())
            })
        })
    }

    /// Check if there are any active tubes
    fn has_active_tubes(&self, py: Python<'_>) -> bool {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                !registry.tubes_by_id.is_empty()
            })
        })
    }

    /// Get count of active tubes
    fn active_tube_count(&self, py: Python<'_>) -> usize {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.tubes_by_id.len()
            })
        })
    }

    /// Python destructor - safety net cleanup
    fn __del__(&self, py: Python<'_>) {
        if !self.explicit_cleanup_called.load(Ordering::SeqCst) {
            warn!(target: "python_bindings", 
                "PyTubeRegistry.__del__ called without explicit cleanup! Consider using cleanup_all() explicitly or using a context manager.");

            // Attempt force cleanup - ignore errors since we're in destructor
            let _ = self.do_force_cleanup(py);
        } else {
            debug!(target: "python_bindings", "PyTubeRegistry.__del__ called after explicit cleanup - OK");
        }
    }

    /// Internal Force cleanup for __del__ - more permissive error handling
    fn do_force_cleanup(&self, py: Python<'_>) -> PyResult<()> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                match REGISTRY.try_write() {
                    Ok(mut registry) => {
                        let tube_ids: Vec<String> = registry.tubes_by_id.keys().cloned().collect();
                        warn!(target: "python_bindings", tube_count = tube_ids.len(), 
                            "Force cleanup in __del__ - {} tubes to close", tube_ids.len());
                        // Close all tubes - ignore individual errors in force cleanup
                        for tube_id in tube_ids {
                            if let Err(e) = registry.close_tube(&tube_id, Some(CloseConnectionReason::Unknown)).await {
                                error!(target: "python_bindings", tube_id = %tube_id, 
                                    "Failed to close tube during force cleanup: {}", e);
                            }
                        }
                        // Clear mappings
                        registry.conversation_mappings.clear();
                        registry.signal_channels.clear();
                        debug!(target: "python_bindings", "Force cleanup complete");
                    }
                    Err(_) => {
                        warn!(target: "python_bindings", 
                            "Could not acquire registry lock for Force cleanup - registry may be in use");
                    }
                }
            })
        });

        // Force shutdown the runtime - this is the safety net for thread cleanup
        warn!(target: "python_bindings", "Force shutting down runtime in __del__");
        crate::runtime::shutdown_runtime_from_python();

        Ok(())
    }

    /// Set server mode in the registry
    fn set_server_mode(&self, py: Python<'_>, server_mode: bool) -> PyResult<()> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let mut registry = REGISTRY.write().await;
                registry.set_server_mode(server_mode);
            });
        });
        Ok(())
    }

    /// Check if the registry is in server mode
    fn is_server_mode(&self, py: Python<'_>) -> bool {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.is_server_mode()
            })
        })
    }

    /// Associate a conversation ID with a tube
    fn associate_conversation(
        &self,
        py: Python<'_>,
        tube_id: &str,
        connection_id: &str,
    ) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let mut registry = REGISTRY.write().await;
                registry
                    .associate_conversation(&tube_id_owned, &connection_id_owned)
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to associate conversation: {e}"))
                    })
            })
        })
    }

    /// find if a tube already exists
    fn tube_found(&self, py: Python<'_>, tube_id: &str) -> bool {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.get_by_tube_id(&tube_id_owned).is_some()
            })
        })
    }

    /// Get all tube IDs
    fn all_tube_ids(&self, py: Python<'_>) -> Vec<String> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.all_tube_ids_sync()
            })
        })
    }

    /// Get all Conversation IDs by Tube ID
    fn get_conversation_ids_by_tube_id(&self, py: Python<'_>, tube_id: &str) -> Vec<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry
                    .conversation_ids_by_tube_id(&tube_id_owned)
                    .into_iter()
                    .cloned()
                    .collect()
            })
        })
    }

    /// find tube by connection ID
    fn tube_id_from_connection_id(&self, py: Python<'_>, connection_id: &str) -> Option<String> {
        let connection_id_owned = connection_id.to_string();
        safe_python_async_execute(py, async move {
            let registry = REGISTRY.read().await;
            registry
                .tube_id_from_conversation_id(&connection_id_owned)
                .cloned()
        })
    }

    /// Find tubes by partial match of tube ID or conversation ID
    fn find_tubes(&self, py: Python<'_>, search_term: &str) -> PyResult<Vec<String>> {
        let master_runtime = get_runtime();
        let search_term_owned = search_term.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                let tubes = registry.find_tubes(&search_term_owned);
                Ok(tubes)
            })
        })
    }

    /// Create a tube with settings
    #[pyo3(signature = (
        conversation_id,
        settings,
        trickle_ice,
        callback_token,
        krelay_server,
        client_version = None,
        ksm_config = None,
        offer = None,
        signal_callback = None,
        tube_id = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn create_tube(
        &self,
        py: Python<'_>,
        conversation_id: &str,
        settings: PyObject,
        trickle_ice: bool,
        callback_token: &str,
        krelay_server: &str,
        client_version: Option<&str>,
        ksm_config: Option<&str>,
        offer: Option<&str>,
        signal_callback: Option<PyObject>,
        tube_id: Option<&str>,
    ) -> PyResult<PyObject> {
        let master_runtime = get_runtime();

        // Validate that client_version is provided
        let client_version = client_version.ok_or_else(|| {
            PyRuntimeError::new_err("client_version is required and must be provided")
        })?;

        // Convert Python settings dictionary to Rust HashMap<String, serde_json::Value>
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;

        // Create an MPSC channel for signaling between Rust and Python
        let (signal_sender_rust, signal_receiver_py) =
            unbounded_channel::<crate::tube_registry::SignalMessage>();

        // Prepare owned versions of string parameters to move into async blocks
        let offer_string_owned = offer.map(String::from);
        let conversation_id_owned = conversation_id.to_string();
        let callback_token_owned = callback_token.to_string();
        let krelay_server_owned = krelay_server.to_string();
        let ksm_config_owned = ksm_config.map(String::from);
        let client_version_owned = client_version.to_string();
        let tube_id_owned = tube_id.map(String::from);

        // This outer block_on will handle the call to the registry's create_tube and setup signal handler
        let creation_result_map = py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                trace!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: Acquiring REGISTRY write lock for create_tube.");
                let mut registry = REGISTRY.write().await;
                trace!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: REGISTRY write lock acquired.");

                // Delegate to the main TubeRegistry::create_tube method
                // This method now encapsulates Tube creation, ICE config, peer connection setup, and offer/answer generation.
                registry.create_tube(
                    &conversation_id_owned,
                    settings_json, // Already a HashMap<String, serde_json::Value>
                    offer_string_owned,
                    trickle_ice,
                    &callback_token_owned,
                    &krelay_server_owned,
                    ksm_config_owned.as_deref(),
                    &client_version_owned,
                    signal_sender_rust, // Pass the sender part of the MPSC channel
                    tube_id_owned,
                ).await
                 .map_err(|e| {
                    error!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: TubeRegistry::create_tube CRITICAL FAILURE: {e}");
                    PyRuntimeError::new_err(format!("Failed to create tube via registry: {e}"))
                 })
            })
        })?; // Propagate errors from block_on or create_tube

        trace!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: TubeRegistry::create_tube call complete. Result map has {} keys.", creation_result_map.len());

        // Extract tube_id for signal handler setup (it must be in the map)
        let final_tube_id = creation_result_map
            .get("tube_id")
            .ok_or_else(|| PyRuntimeError::new_err("Tube ID missing from create_tube response"))?
            .clone();

        // Set up Python signal handler if a callback was provided
        if let Some(cb) = signal_callback {
            setup_signal_handler(
                final_tube_id.clone(),
                signal_receiver_py,
                master_runtime.clone(),
                cb,
            );
        }

        // Convert the resulting HashMap to a Python dictionary to return
        let py_dict = PyDict::new(py);
        for (key, value) in creation_result_map.iter() {
            py_dict.set_item(key, value)?;
        }

        Ok(py_dict.into())
    }

    /// Create an offer for a tube
    fn create_offer(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                if let Some(tube) = registry.get_by_tube_id(&tube_id_owned) {
                    tube.create_offer().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create offer: {e}"))
                    })
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {tube_id_owned}"
                    )))
                }
            })
        })
    }

    /// Create an answer for a tube
    fn create_answer(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                if let Some(tube) = registry.get_by_tube_id(&tube_id_owned) {
                    tube.create_answer().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create answer: {e}"))
                    })
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {tube_id_owned}"
                    )))
                }
            })
        })
    }

    /// Set a remote description for a tube
    #[pyo3(signature = (
        tube_id,
        sdp,
        is_answer = false,
    ))]
    fn set_remote_description(
        &self,
        py: Python<'_>,
        tube_id: &str,
        sdp: String,
        is_answer: bool,
    ) -> PyResult<Option<String>> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry
                    .set_remote_description(tube_id, &sdp, is_answer)
                    .await
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to set remote description: {e}"))
                    })
            })
        })
    }

    /// Add an ICE candidate to a tube
    fn add_ice_candidate(&self, _py: Python<'_>, tube_id: &str, candidate: String) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let candidate_owned = candidate;

        master_runtime.spawn(async move {
            let registry = REGISTRY.read().await;
            if let Err(e) = registry.add_external_ice_candidate(&tube_id_owned, &candidate_owned).await {
                warn!(target: "python_bindings", "Error adding ICE candidate for tube {}: {}", tube_id_owned, e);
            }
        });

        Ok(())
    }

    /// Get connection state of a tube
    fn get_connection_state(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry
                    .get_connection_state(&tube_id_owned)
                    .await
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to get connection state: {e}"))
                    })
            })
        })
    }

    /// Close a specific connection on a tube
    #[pyo3(signature = (
        connection_id,
        reason = None,
    ))]
    fn close_connection(
        &self,
        py: Python<'_>,
        connection_id: &str,
        reason: Option<u16>,
    ) -> PyResult<()> {
        let connection_id_owned = connection_id.to_string();

        safe_python_async_execute(py, async move {
            // Get tube reference while holding registry lock, then release lock
            let tube_arc = {
                let registry = REGISTRY.read().await;

                // Look up the tube ID from the connection ID
                let tube_id_owned =
                    match registry.tube_id_from_conversation_id(&connection_id_owned) {
                        Some(tube_id) => tube_id.clone(),
                        None => {
                            return Err(PyRuntimeError::new_err(format!(
                                "Rust: No tube found for connection ID: {connection_id_owned}"
                            )));
                        }
                    };

                // Get tube reference before releasing the lock
                match registry.get_by_tube_id(&tube_id_owned) {
                    Some(tube) => tube.clone(), // Clone the Arc to keep reference
                    None => {
                        return Err(PyRuntimeError::new_err(format!(
                            "Rust: Tube not found {tube_id_owned} during close_connection for connection {connection_id_owned}"
                        )));
                    }
                }
                // Registry lock is automatically released here
            };

            // Convert the reason code to CloseConnectionReason enum
            let close_reason = match reason {
                Some(0) => CloseConnectionReason::Normal,
                Some(1) => CloseConnectionReason::Error,
                Some(2) => CloseConnectionReason::Timeout,
                Some(4) => CloseConnectionReason::ServerRefuse,
                Some(5) => CloseConnectionReason::Client,
                Some(6) => CloseConnectionReason::Unknown,
                Some(7) => CloseConnectionReason::InvalidInstruction,
                Some(8) => CloseConnectionReason::GuacdRefuse,
                Some(9) => CloseConnectionReason::ConnectionLost,
                Some(10) => CloseConnectionReason::ConnectionFailed,
                Some(11) => CloseConnectionReason::TunnelClosed,
                Some(12) => CloseConnectionReason::AdminClosed,
                Some(13) => CloseConnectionReason::ErrorRecording,
                Some(14) => CloseConnectionReason::GuacdError,
                Some(15) => CloseConnectionReason::AIClosed,
                Some(16) => CloseConnectionReason::AddressResolutionFailed,
                Some(17) => CloseConnectionReason::DecryptionFailed,
                Some(18) => CloseConnectionReason::ConfigurationError,
                Some(19) => CloseConnectionReason::ProtocolError,
                Some(20) => CloseConnectionReason::UpstreamClosed,
                Some(_) => CloseConnectionReason::Unknown, // Unknown code defaults to Unknown
                None => CloseConnectionReason::Unknown,    // Default when no reason specified
            };

            // Now call close_channel_with_reason without holding any registry locks
            tube_arc
                .close_channel(&connection_id_owned, Some(close_reason))
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "Rust: Failed to close connection {connection_id_owned}: {e}"
                    ))
                })
        })
    }

    /// Close an entire tube
    #[pyo3(signature = (
        tube_id,
        reason = None,
    ))]
    fn close_tube(&self, py: Python<'_>, tube_id: &str, reason: Option<u16>) -> PyResult<()> {
        let tube_id_owned = tube_id.to_string();

        safe_python_async_execute(py, async move {
            let mut registry = REGISTRY.write().await;
            let close_reason = reason.map(CloseConnectionReason::from_code);
            registry
                .close_tube(&tube_id_owned, close_reason)
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "Rust: Failed to close tube {tube_id_owned}: {e}"
                    ))
                })
        })
    }

    ///
    /// Get a tube object by conversation ID
    fn get_tube_id_by_conversation_id(
        &self,
        py: Python<'_>,
        conversation_id: &str,
    ) -> PyResult<String> {
        let conversation_id_owned = conversation_id.to_string();
        safe_python_async_execute(py, async move {
            let registry = REGISTRY.read().await;
            if let Some(tube) = registry.get_by_conversation_id(&conversation_id_owned) {
                Ok(tube.id().to_string())
            } else {
                Err(PyRuntimeError::new_err(format!(
                    "No tube found for conversation: {conversation_id_owned}"
                )))
            }
        })
    }

    /// Refresh connections on router - collect all callback tokens and send to router
    fn refresh_connections(
        &self,
        py: Python<'_>,
        ksm_config_from_python: String,
        client_version: String,
    ) -> PyResult<()> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;

                // Collect all callback tokens from active tubes
                let mut callback_tokens = Vec::new();

                for tube_arc in registry.tubes_by_id.values() {
                    // Get callback tokens from all channels in this tube
                    let tube_channel_tokens = tube_arc.get_callback_tokens().await;
                    callback_tokens.extend(tube_channel_tokens);
                }
                // The post_connection_state function handles TEST_MODE_KSM_CONFIG internally.
                // It will also error out if ksm_config_from_python is empty and not a test string.
                debug!(
                    target: "python_bindings",
                    token_count = callback_tokens.len(),
                    ksm_source = "python_direct_arg",
                    "Preparing to send refresh_connections (open_connections) callback with KSM config from Python."
                );

                let tokens_json = serde_json::Value::Array(
                    callback_tokens.into_iter()
                        .map(serde_json::Value::String)
                        .collect()
                );

                post_connection_state(&ksm_config_from_python, "open_connections", &tokens_json, None, &client_version).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to refresh connections on router: {e}")))
            })
        })
    }

    /// Shutdown the runtime - useful for clean process termination
    fn shutdown_runtime(&self, _py: Python<'_>) -> PyResult<()> {
        debug!(target: "python_bindings", "Python requested runtime shutdown");
        shutdown_runtime_from_python();
        Ok(())
    }

    /// Print all existing tubes with their connections for debugging
    fn print_tubes_with_connections(&self, py: Python<'_>) -> PyResult<()> {
        let master_runtime = get_runtime();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;

                debug!("=== Tube Registry Status ===");
                debug!("Total tubes: {}", registry.tubes_by_id.len());
                debug!("Server mode: {}\n", registry.is_server_mode());

                if registry.tubes_by_id.is_empty() {
                    debug!("No active tubes.");
                    return Ok(());
                }

                for tube_id in registry.tubes_by_id.keys() {
                    debug!("Tube ID: {tube_id}");

                    // Get conversation IDs for this tube
                    let conversation_ids = registry.conversation_ids_by_tube_id(tube_id);
                    if conversation_ids.is_empty() {
                        debug!("  └─ No conversations");
                    } else {
                        debug!("  └─ Conversations ({}):", conversation_ids.len());
                        for (i, conv_id) in conversation_ids.iter().enumerate() {
                            let is_last = i == conversation_ids.len() - 1;
                            let prefix = if is_last {
                                "     └─"
                            } else {
                                "     ├─"
                            };
                            debug!("{prefix}  {conv_id}");
                        }
                    }

                    // Check if there's a signal channel for this tube
                    let has_signal_channel = registry.signal_channels.contains_key(tube_id);
                    debug!(
                        "  └─ Signal channel: {}\n",
                        if has_signal_channel { "Active" } else { "None" }
                    );
                }

                // Show reverse mapping summary
                debug!("=== Conversation Mappings ===");
                debug!("Total mappings: {}", registry.conversation_mappings.len());
                if !registry.conversation_mappings.is_empty() {
                    for (conv_id, mapped_tube_id) in &registry.conversation_mappings {
                        debug!("  {conv_id} → {mapped_tube_id}");
                    }
                }
                debug!("============================");

                Ok(())
            })
        })
    }

    /// Test network connectivity to krelay server with comprehensive diagnostics
    /// Returns detailed results in JSON format for IT personnel to analyze
    #[pyo3(signature = (
        krelay_server,
        settings = None,
        timeout_seconds = None,
        ksm_config = None,
        client_version = None,
        username = None,
        password = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn test_webrtc_connectivity(
        &self,
        py: Python<'_>,
        krelay_server: &str,
        settings: Option<PyObject>,
        timeout_seconds: Option<u64>,
        ksm_config: Option<&str>,
        client_version: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) -> PyResult<PyObject> {
        let master_runtime = get_runtime();
        let krelay_server_owned = krelay_server.to_string();
        let timeout = timeout_seconds.unwrap_or(30);

        // Convert Python settings dictionary to Rust HashMap if provided
        let settings_json = if let Some(settings_obj) = settings {
            pyobj_to_json_hashmap(py, &settings_obj)?
        } else {
            // Default test settings
            let mut default_settings = std::collections::HashMap::new();
            default_settings.insert("use_turn".to_string(), serde_json::Value::Bool(true));
            default_settings.insert("turn_only".to_string(), serde_json::Value::Bool(false));
            default_settings
        };

        let result = py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                test_webrtc_connectivity_internal(
                    &krelay_server_owned,
                    settings_json,
                    timeout,
                    ksm_config,
                    client_version,
                    username,
                    password,
                )
                .await
            })
        });

        match result {
            Ok(test_results) => {
                // Convert the test results to a Python dictionary
                let py_dict = PyDict::new(py);
                for (key, value) in test_results.iter() {
                    match value {
                        serde_json::Value::String(s) => py_dict.set_item(key, s)?,
                        serde_json::Value::Bool(b) => py_dict.set_item(key, *b)?,
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                py_dict.set_item(key, i)?;
                            } else if let Some(f) = n.as_f64() {
                                py_dict.set_item(key, f)?;
                            }
                        }
                        serde_json::Value::Array(arr) => {
                            let py_list = pyo3::types::PyList::empty(py);
                            for item in arr {
                                if let serde_json::Value::String(s) = item {
                                    py_list.append(s)?;
                                }
                            }
                            py_dict.set_item(key, py_list)?;
                        }
                        serde_json::Value::Object(obj) => {
                            let nested_dict = PyDict::new(py);
                            for (nested_key, nested_value) in obj.iter() {
                                if let serde_json::Value::String(s) = nested_value {
                                    nested_dict.set_item(nested_key, s)?;
                                } else if let serde_json::Value::Bool(b) = nested_value {
                                    nested_dict.set_item(nested_key, *b)?;
                                } else if let serde_json::Value::Number(n) = nested_value {
                                    if let Some(i) = n.as_i64() {
                                        nested_dict.set_item(nested_key, i)?;
                                    } else if let Some(f) = n.as_f64() {
                                        nested_dict.set_item(nested_key, f)?;
                                    }
                                }
                            }
                            py_dict.set_item(key, nested_dict)?;
                        }
                        _ => {}
                    }
                }
                Ok(py_dict.into())
            }
            Err(e) => Err(PyRuntimeError::new_err(format!(
                "WebRTC connectivity test failed: {e}"
            ))),
        }
    }

    /// Format WebRTC connectivity test results in a human-readable format for IT personnel
    #[pyo3(signature = (
        results,
        detailed = false,
    ))]
    fn format_connectivity_results(
        &self,
        py: Python<'_>,
        results: PyObject,
        detailed: Option<bool>,
    ) -> PyResult<String> {
        let detailed = detailed.unwrap_or(false);

        // Convert Python results back to JSON for processing
        let results_dict = results.extract::<HashMap<String, PyObject>>(py)?;
        let mut formatted_output = Vec::new();

        // Header
        formatted_output.push("=".repeat(80));
        formatted_output.push("WebRTC Connectivity Test Results".to_string());
        formatted_output.push("=".repeat(80));

        // Basic info
        let server = results_dict
            .get("server")
            .and_then(|v| v.extract::<String>(py).ok())
            .unwrap_or_else(|| "unknown".to_string());
        let test_time = results_dict
            .get("test_started_at")
            .and_then(|v| v.extract::<String>(py).ok())
            .unwrap_or_else(|| "unknown".to_string());
        let timeout = results_dict
            .get("timeout_seconds")
            .and_then(|v| v.extract::<u64>(py).ok())
            .unwrap_or(30);

        formatted_output.push(format!("Server: {server}"));
        formatted_output.push(format!("Test Time: {test_time}"));
        formatted_output.push(format!("Timeout: {timeout}s"));
        formatted_output.push("".to_string());

        // Settings
        if let Some(settings_obj) = results_dict.get("settings") {
            if let Ok(settings) = settings_obj.extract::<HashMap<String, PyObject>>(py) {
                formatted_output.push("Test Settings:".to_string());
                for (key, value) in settings.iter() {
                    if let Ok(value_str) = value.extract::<String>(py) {
                        formatted_output.push(format!("  {key}: {value_str}"));
                    } else if let Ok(value_bool) = value.extract::<bool>(py) {
                        formatted_output.push(format!("  {key}: {value_bool}"));
                    }
                }
                formatted_output.push("".to_string());
            }
        }

        // Individual test results
        let test_order = vec![
            "dns_resolution",
            "aws_connectivity",
            "tcp_connectivity",
            "udp_binding",
            "ice_configuration",
            "webrtc_peer_connection",
        ];

        formatted_output.push("Test Results:".to_string());
        formatted_output.push("-".repeat(40));

        for test_name in test_order {
            if let Some(test_result_obj) = results_dict.get(test_name) {
                if let Ok(test_result) = test_result_obj.extract::<HashMap<String, PyObject>>(py) {
                    let success = test_result
                        .get("success")
                        .and_then(|v| v.extract::<bool>(py).ok())
                        .unwrap_or(false);
                    let duration = test_result
                        .get("duration_ms")
                        .and_then(|v| v.extract::<u64>(py).ok())
                        .unwrap_or(0);
                    let message = test_result
                        .get("message")
                        .and_then(|v| v.extract::<String>(py).ok())
                        .unwrap_or_else(|| "No message".to_string());

                    let status = if success { "PASS" } else { "FAIL" };
                    let title = test_name
                        .replace('_', " ")
                        .split(' ')
                        .map(|word| {
                            let mut chars = word.chars();
                            match chars.next() {
                                None => String::new(),
                                Some(first) => {
                                    first.to_uppercase().collect::<String>() + chars.as_str()
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");

                    formatted_output.push(format!("{status} {title:<25} ({duration}ms)"));
                    formatted_output.push(format!("     {message}"));

                    if !success {
                        if let Some(error_obj) = test_result.get("error") {
                            if let Ok(error_msg) = error_obj.extract::<String>(py) {
                                formatted_output.push(format!("     Error: {error_msg}"));
                            }
                        }
                    }

                    // Additional details for specific tests
                    if test_name == "dns_resolution" {
                        if let Some(ips_obj) = test_result.get("resolved_ips") {
                            if let Ok(ips) = ips_obj.extract::<Vec<String>>(py) {
                                if !ips.is_empty() {
                                    formatted_output
                                        .push(format!("     Resolved IPs: {}", ips.join(", ")));
                                }
                            }
                        }
                    }

                    if test_name == "ice_configuration" && success {
                        if let Some(servers_obj) = test_result.get("ice_servers") {
                            if let Ok(servers) = servers_obj.extract::<Vec<String>>(py) {
                                formatted_output
                                    .push(format!("     ICE Servers ({}): ", servers.len()));
                                let display_count = if detailed {
                                    servers.len()
                                } else {
                                    3.min(servers.len())
                                };
                                for server in servers.iter().take(display_count) {
                                    formatted_output.push(format!("       - {server}"));
                                }
                                if servers.len() > 3 && !detailed {
                                    formatted_output.push(format!(
                                        "       - ... and {} more",
                                        servers.len() - 3
                                    ));
                                }
                            }
                        }
                    }

                    formatted_output.push("".to_string());
                }
            }
        }

        // Overall result
        if let Some(overall_obj) = results_dict.get("overall_result") {
            if let Ok(overall) = overall_obj.extract::<HashMap<String, PyObject>>(py) {
                let success = overall
                    .get("success")
                    .and_then(|v| v.extract::<bool>(py).ok())
                    .unwrap_or(false);
                let duration = overall
                    .get("total_duration_ms")
                    .and_then(|v| v.extract::<u64>(py).ok())
                    .unwrap_or(0);
                let tests_run = overall
                    .get("tests_run")
                    .and_then(|v| v.extract::<u64>(py).ok())
                    .unwrap_or(0);

                formatted_output.push("=".repeat(40));
                let status = if success {
                    "ALL TESTS PASSED".to_string()
                } else if let Some(failed_obj) = overall.get("failed_tests") {
                    if let Ok(failed_tests) = failed_obj.extract::<Vec<String>>(py) {
                        format!("{} TESTS FAILED", failed_tests.len())
                    } else {
                        "TESTS FAILED".to_string()
                    }
                } else {
                    "TESTS FAILED".to_string()
                };
                formatted_output.push(status);
                formatted_output.push(format!("Total Duration: {duration}ms"));
                formatted_output.push(format!("Tests Run: {tests_run}"));

                if !success {
                    if let Some(failed_obj) = overall.get("failed_tests") {
                        if let Ok(failed_tests) = failed_obj.extract::<Vec<String>>(py) {
                            formatted_output
                                .push(format!("Failed Tests: {}", failed_tests.join(", ")));
                        }
                    }
                }
                formatted_output.push("".to_string());
            }
        }

        // Recommendations
        if let Some(recs_obj) = results_dict.get("recommendations") {
            if let Ok(recommendations) = recs_obj.extract::<Vec<String>>(py) {
                if !recommendations.is_empty() {
                    formatted_output.push("IT Recommendations:".to_string());
                    formatted_output.push("-".repeat(40));
                    for (i, rec) in recommendations.iter().enumerate() {
                        formatted_output.push(format!("{}. {}", i + 1, rec));
                    }
                    formatted_output.push("".to_string());
                }
            }
        }

        // Suggested CLI tests
        if let Some(cli_obj) = results_dict.get("suggested_cli_tests") {
            if let Ok(cli_tests) = cli_obj.extract::<Vec<String>>(py) {
                if !cli_tests.is_empty() {
                    formatted_output.push("Suggested Command Line Tests:".to_string());
                    formatted_output.push("-".repeat(40));
                    for (i, cmd) in cli_tests.iter().enumerate() {
                        formatted_output.push(format!("{}. {}", i + 1, cmd));
                    }
                    formatted_output.push("".to_string());
                }
            }
        }

        formatted_output.push("=".repeat(80));

        Ok(formatted_output.join("\n"))
    }
}

// Implement Drop trait for PyTubeRegistry as a safety net
impl Drop for PyTubeRegistry {
    fn drop(&mut self) {
        if !self.explicit_cleanup_called.load(Ordering::SeqCst) {
            eprintln!("FORCE CLOSE: PyTubeRegistry Drop - forcing immediate cleanup!");

            // FORCE CLOSE: No async, no waiting, clean everything up
            match REGISTRY.try_write() {
                Ok(mut registry) => {
                    let tube_count = registry.tubes_by_id.len();
                    if tube_count > 0 {
                        eprintln!("FORCE CLOSE: Clearing {tube_count} tubes immediately");

                        // 1. Immediately drop all signal channels (stops background tasks)
                        registry.signal_channels.clear();

                        // 2. Clear all conversation mappings
                        registry.conversation_mappings.clear();

                        // 3. Drop all tube references (this will trigger their Drop)
                        registry.tubes_by_id.clear();

                        eprintln!("FORCE CLOSE: Registry cleaned up successfully");
                    }
                }
                Err(_) => {
                    eprintln!("FORCE CLOSE: Registry locked - someone else is cleaning up");
                }
            }

            // CRITICAL: Force shutdown the runtime to prevent hanging threads
            eprintln!("FORCE CLOSE: Shutting down runtime to prevent hanging threads");
            crate::runtime::shutdown_runtime_from_python();
        }
    }
}

// Helper function to set up signal handling for a tube
fn setup_signal_handler(
    tube_id_key: String,
    mut signal_receiver: tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
    runtime_handle: crate::runtime::RuntimeHandle,
    callback_pyobj: PyObject, // Use the passed callback object
) {
    let task_tube_id = tube_id_key.clone();
    let runtime = runtime_handle.runtime().clone(); // Extract the Arc<Runtime>
    runtime.spawn(async move {
        debug!(target: "python_bindings", "Signal handler task started for tube_id: {}", task_tube_id);
        let mut signal_count = 0;
        while let Some(signal) = signal_receiver.recv().await {
            signal_count += 1;
            debug!(target: "python_bindings", "Rust task received signal {}: {:?} for tube {}. Preparing Python callback.", signal_count, signal.kind, task_tube_id);

            Python::with_gil(|py| {
                let py_dict = PyDict::new(py);
                let mut success = true;
                if let Err(e) = py_dict.set_item("tube_id", &signal.tube_id) {
                    warn!("Failed to set 'tube_id' in signal dict for {}: {:?}", task_tube_id, e);
                    success = false;
                }
                if success {
                    if let Err(e) = py_dict.set_item("kind", &signal.kind) {
                        warn!("Failed to set 'kind' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }
                if success {
                    if let Err(e) = py_dict.set_item("data", &signal.data) {
                        warn!("Failed to set 'data' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }
                if success {
                    if let Err(e) = py_dict.set_item("conversation_id", &signal.conversation_id) {
                        warn!("Failed to set 'conversation_id' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }

                if success {
                    let result = callback_pyobj.call1(py, (py_dict,));
                    if let Err(e) = result {
                        // Only log if it's not an expected KeyError during closure
                        if !(e.is_instance_of::<pyo3::exceptions::PyKeyError>(py)
                            && (signal.kind == "channel_closed" || signal.kind == "disconnect")) {
                            warn!("Error in Python signal kind:{} callback for tube {}: {:?}: ", signal.kind, task_tube_id, e);
                        }
                    }
                } else {
                    warn!("Skipping Python callback for tube {} due to error setting dict items for signal {:?}", task_tube_id, signal.kind);
                }
            });
            debug!(target: "python_bindings", "Rust task completed Python callback GIL block for signal {}: {:?} for tube {}", signal_count, signal.kind, task_tube_id);
        }
        // Only log termination if it's not a normal closure
        if signal_count > 0 {
            debug!(target: "python_bindings", "Signal handler task for tube {} completed normally after processing {} signals", task_tube_id, signal_count);
        } else {
            warn!(target: "python_bindings", "Signal handler task FOR TUBE {} IS TERMINATING (processed {} signals) because MPSC channel receive loop ended.", task_tube_id, signal_count);
        }
    });
}

/// Internal implementation of WebRTC connectivity test
/// Performs comprehensive diagnostics similar to turnutils but for IT personnel
async fn test_webrtc_connectivity_internal(
    krelay_server: &str,
    settings: HashMap<String, serde_json::Value>,
    timeout_seconds: u64,
    ksm_config: Option<&str>,
    client_version: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let start_time = Instant::now();
    let mut results = HashMap::new();

    info!(target: "connectivity_test", "Starting WebRTC connectivity test for server: {}", krelay_server);

    // Basic test information
    results.insert("server".to_string(), json!(krelay_server));
    results.insert(
        "test_started_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    results.insert("timeout_seconds".to_string(), json!(timeout_seconds));
    results.insert(
        "settings".to_string(),
        serde_json::to_value(&settings).unwrap_or(json!({})),
    );

    // Step 1: DNS Resolution
    info!(target: "connectivity_test", "Step 1: Testing DNS resolution for {}", krelay_server);
    let dns_start = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((krelay_server, 3478)),
    )
    .await
    {
        Ok(Ok(addresses)) => {
            let addrs: Vec<String> = addresses.map(|addr| addr.ip().to_string()).collect();
            results.insert("dns_resolution".to_string(), json!({
                "success": true,
                "duration_ms": dns_start.elapsed().as_millis(),
                "resolved_ips": addrs,
                "message": format!("Successfully resolved {} to {} IP addresses", krelay_server, addrs.len())
            }));
            addrs
        }
        Ok(Err(e)) => {
            results.insert(
                "dns_resolution".to_string(),
                json!({
                    "success": false,
                    "duration_ms": dns_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("DNS resolution failed for {}: {}", krelay_server, e),
                    "it_diagnosis": "DNS resolution failure indicates either the hostname is incorrect, DNS servers are unreachable, or network connectivity is blocked",
                    "suggested_tests": [
                        format!("nslookup {krelay_server}"),
                        format!("dig {krelay_server}")
                    ]
                }),
            );
            return Ok(results);
        }
        Err(_) => {
            results.insert(
                "dns_resolution".to_string(),
                json!({
                    "success": false,
                    "duration_ms": dns_start.elapsed().as_millis(),
                    "error": "DNS resolution timeout",
                    "message": format!("DNS resolution timed out for {}", krelay_server),
                    "it_diagnosis": "DNS timeout suggests network connectivity issues, DNS server problems, or restrictive firewall rules blocking DNS queries",
                    "suggested_tests": [
                        "ping 8.8.8.8  # Test basic internet connectivity",
                        "nslookup google.com  # Test if DNS is working at all",
                        format!("telnet {} 53  # Test if DNS port is accessible", krelay_server),
                        "Check firewall rules for outbound UDP/53 and TCP/53",
                        "Verify corporate proxy/DNS filtering settings"
                    ]
                }),
            );
            return Ok(results);
        }
    };

    // Step 2: AWS Infrastructure connectivity test
    info!(target: "connectivity_test", "Step 2: Testing AWS infrastructure connectivity");
    let aws_start = Instant::now();
    let mut aws_issues = Vec::new();
    let mut aws_success = true;

    // Test connectivity to common AWS endpoints that might be involved in load balancing
    let aws_endpoints = vec![("amazonaws.com", 443), ("aws.amazon.com", 443)];

    let mut aws_results = Vec::new();
    for (endpoint, port) in aws_endpoints {
        match tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((endpoint, port)),
        )
        .await
        {
            Ok(Ok(stream)) => {
                drop(stream);
                aws_results.push(json!({
                    "endpoint": format!("{}:{}", endpoint, port),
                    "success": true,
                    "message": format!("Successfully connected to {endpoint}")
                }));
            }
            Ok(Err(e)) => {
                aws_success = false;
                aws_issues.push(format!("Failed to connect to {endpoint}: {e}"));
                aws_results.push(json!({
                    "endpoint": format!("{endpoint}:{port}"),
                    "success": false,
                    "error": e.to_string()
                }));
            }
            Err(_) => {
                aws_success = false;
                aws_issues.push(format!("Connection timeout to {endpoint}"));
                aws_results.push(json!({
                    "endpoint": format!("{endpoint}:{port}"),
                    "success": false,
                    "error": "Connection timeout"
                }));
            }
        }
    }

    // AWS connectivity is informational only - don't fail the test if it's not available
    let aws_message = if aws_success {
        "AWS infrastructure endpoints are accessible".to_string()
    } else {
        format!(
            "AWS connectivity not available (may not be required): {}",
            aws_issues.join("; ")
        )
    };

    results.insert(
        "aws_connectivity".to_string(),
        json!({
            "success": true, // Always succeed - this is informational only
            "duration_ms": aws_start.elapsed().as_millis(),
            "endpoints_tested": aws_results,
            "issues": aws_issues,
            "warning": !aws_success,
            "message": aws_message
        }),
    );

    // Step 3: Basic TCP connectivity test (port 3478)
    info!(target: "connectivity_test", "Step 3: Testing TCP connectivity to {}:3478", krelay_server);
    let tcp_start = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((krelay_server, 3478)),
    )
    .await
    {
        Ok(Ok(stream)) => {
            drop(stream);
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": true,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "message": format!("Successfully connected to {}:3478 via TCP", krelay_server)
                }),
            );
            true
        }
        Ok(Err(e)) => {
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": false,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "error": e.to_string(),
                    "message": format!("TCP connection failed to {}:3478: {}", krelay_server, e),
                    "it_diagnosis": "TCP connection failure indicates firewall blocking, incorrect routing, or server unavailability",
                    "suggested_tests": [
                        format!("telnet {} 3478  # Test direct TCP connection", krelay_server),
                        format!("nc -v {} 3478  # Alternative TCP connection test", krelay_server),
                        "Check firewall rules for outbound TCP/3478",
                        "Verify corporate firewall/proxy settings",
                        "Test from different network if possible"
                    ]
                }),
            );
            false
        }
        Err(_) => {
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": false,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "error": "Connection timeout",
                    "message": format!("TCP connection timed out to {}:3478", krelay_server),
                    "it_diagnosis": "TCP timeout typically indicates firewall blocking, network routing issues, or server overload",
                    "suggested_tests": [
                        format!("ping {}  # Test if server is reachable", krelay_server),
                        format!("traceroute {}  # Check network path", krelay_server),
                        format!("nmap -p 3478 {}  # Check if port is open", krelay_server),
                        "Check corporate firewall for TCP/3478 restrictions",
                        "Verify no local firewall software is blocking"
                    ]
                }),
            );
            false
        }
    };

    // Step 4: UDP socket binding test (to ensure we can send UDP)
    info!(target: "connectivity_test", "Step 4: Testing UDP socket binding");
    let udp_start = Instant::now();
    match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => {
            let local_addr = socket
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or("unknown".to_string());
            results.insert(
                "udp_binding".to_string(),
                json!({
                    "success": true,
                    "duration_ms": udp_start.elapsed().as_millis(),
                    "local_address": local_addr,
                    "message": "Successfully bound UDP socket for STUN/TURN communication"
                }),
            );
            true
        }
        Err(e) => {
            results.insert(
                "udp_binding".to_string(),
                json!({
                    "success": false,
                    "duration_ms": udp_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("Failed to bind UDP socket: {e}"),
                    "it_diagnosis": "UDP socket binding failure indicates local network restrictions or system resource limits",
                    "suggested_tests": [
                        "netstat -ul  # Check UDP ports in use",
                        "ss -ul  # Alternative UDP port listing",
                        "Check local firewall software settings",
                        "Verify system ulimits for socket creation",
                        "Test with admin/root privileges if permitted"
                    ]
                }),
            );
            false
        }
    };

    // Step 4: WebRTC ICE configuration validation
    info!(target: "connectivity_test", "Step 4: Testing WebRTC ICE configuration");
    let ice_start = Instant::now();

    // Extract settings for ICE server configuration
    let use_turn = settings
        .get("use_turn")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let turn_only = settings
        .get("turn_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Build ICE server configuration (similar to tube_registry.rs:330-420)
    let mut ice_urls = Vec::new();

    if !turn_only {
        ice_urls.push(format!("stun:{krelay_server}:3478"));
    }

    // Add TURN servers if configured
    if use_turn {
        ice_urls.push(format!("turn:{krelay_server}:3478"));
        ice_urls.push(format!("turns:{krelay_server}:5349"));
    }

    results.insert(
        "ice_configuration".to_string(),
        json!({
            "success": true,
            "duration_ms": ice_start.elapsed().as_millis(),
            "use_turn": use_turn,
            "turn_only": turn_only,
            "ice_servers": ice_urls,
            "server_count": ice_urls.len(),
            "message": format!("Generated ICE configuration with {} servers", ice_urls.len())
        }),
    );

    // Step 5: Simple WebRTC Peer Connection creation test
    info!(target: "connectivity_test", "Step 5: Testing WebRTC peer connection creation");
    let webrtc_start = Instant::now();

    // Build the RTCConfiguration
    use webrtc::ice_transport::ice_gathering_state::RTCIceGatheringState;
    use webrtc::ice_transport::ice_server::RTCIceServer;
    use webrtc::peer_connection::configuration::RTCConfiguration;

    let mut webrtc_ice_servers = Vec::new();

    if !turn_only {
        webrtc_ice_servers.push(RTCIceServer {
            urls: vec![format!("stun:{}:3478", krelay_server)],
            ..Default::default()
        });
    }

    // Track whether we're using real credentials (either from ksm_config or passed parameters)
    let mut using_real_credentials = false;

    // Add TURN servers if configured
    if use_turn {
        // Priority 1: Try to get credentials from ksm_config if provided
        if let (Some(ksm_cfg), Some(client_ver)) = (ksm_config, client_version) {
            if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                debug!(target: "connectivity_test", "Fetching TURN credentials from KSM router");
                match get_relay_access_creds(ksm_cfg, None, client_ver).await {
                    Ok(creds) => {
                        debug!(target: "connectivity_test", "Successfully fetched TURN credentials from router");

                        // Extract username and password from credentials
                        if let (Some(router_username), Some(router_password)) = (
                            creds.get("username").and_then(|v| v.as_str()),
                            creds.get("password").and_then(|v| v.as_str()),
                        ) {
                            debug!(target: "connectivity_test", "Using router TURN credentials for test");
                            using_real_credentials = true;
                            webrtc_ice_servers.push(RTCIceServer {
                                urls: vec![format!("turn:{}:3478", krelay_server)],
                                username: router_username.to_string(),
                                credential: router_password.to_string(),
                            });
                        } else {
                            warn!(target: "connectivity_test", "Invalid router credentials format, checking for passed credentials");
                            // Fall through to check passed credentials
                        }
                    }
                    Err(e) => {
                        warn!(target: "connectivity_test", "Failed to get router credentials: {}, checking for passed credentials", e);
                        // Fall through to check passed credentials
                    }
                }
            }
        }

        // Priority 2: Use passed username/password if we don't have router credentials
        if !using_real_credentials {
            if let (Some(user), Some(pass)) = (username, password) {
                debug!(target: "connectivity_test", "Using passed TURN credentials for test");
                using_real_credentials = true;
                webrtc_ice_servers.push(RTCIceServer {
                    urls: vec![format!("turn:{}:3478", krelay_server)],
                    username: user.to_string(),
                    credential: pass.to_string(),
                });
            } else {
                return Err("TURN server testing requires either ksm_config with client_version or username/password parameters".to_string());
            }
        }
    }

    let config = RTCConfiguration {
        ice_servers: webrtc_ice_servers,
        ice_transport_policy: if turn_only {
            webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay
        } else {
            webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All
        },
        ..Default::default()
    };

    match crate::webrtc_core::create_peer_connection(Some(config)).await {
        Ok(pc) => {
            let mut webrtc_success = true;
            let webrtc_message: String;
            let mut webrtc_details = serde_json::Map::new();

            // Test creating a data channel
            match crate::webrtc_core::create_data_channel(&pc, "test-connectivity").await {
                Ok(_dc) => {
                    webrtc_details.insert("data_channel_created".to_string(), json!(true));

                    // Create an offer to trigger ICE candidate gathering
                    match pc.create_offer(None).await {
                        Ok(offer) => {
                            match pc.set_local_description(offer).await {
                                Ok(_) => {
                                    debug!(target: "connectivity_test", "Set local description, starting ICE candidate gathering");
                                    webrtc_details.insert("offer_created".to_string(), json!(true));

                                    // Wait for ICE gathering to complete or timeout
                                    let ice_timeout = Duration::from_secs(10);
                                    let ice_gathering_start = Instant::now();

                                    let gathering_result = tokio::time::timeout(ice_timeout, async {
                                        let mut last_state = pc.ice_gathering_state();
                                        debug!(target: "connectivity_test", "Initial ICE gathering state: {:?}", last_state);
                                        while last_state != RTCIceGatheringState::Complete {
                                            tokio::time::sleep(Duration::from_millis(100)).await;
                                            let current_state = pc.ice_gathering_state();
                                            if current_state != last_state {
                                                debug!(target: "connectivity_test", "ICE gathering state changed: {:?} -> {:?}", last_state, current_state);
                                                last_state = current_state;
                                            }
                                        }
                                        debug!(target: "connectivity_test", "ICE gathering completed");
                                    }).await;

                                    let ice_gathering_duration = ice_gathering_start.elapsed();
                                    webrtc_details.insert(
                                        "ice_gathering_duration_ms".to_string(),
                                        json!(ice_gathering_duration.as_millis()),
                                    );

                                    match gathering_result {
                                        Ok(_) => {
                                            debug!(target: "connectivity_test", "ICE gathering completed successfully");
                                            webrtc_details.insert(
                                                "ice_gathering_completed".to_string(),
                                                json!(true),
                                            );

                                            // Analyze gathered ICE candidates
                                            if let Some(local_desc) = pc.local_description().await {
                                                let sdp_text = local_desc.sdp;
                                                let candidate_analysis = analyze_ice_candidates(
                                                    &sdp_text,
                                                    using_real_credentials,
                                                );

                                                // Add the analysis to results
                                                for (key, value) in candidate_analysis {
                                                    webrtc_details.insert(key, value);
                                                }

                                                // Determine overall success based on candidate analysis
                                                if use_turn && using_real_credentials {
                                                    // If we're testing TURN with real credentials, we should get relay candidates
                                                    let relay_candidates = webrtc_details
                                                        .get("relay_candidates_count")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);

                                                    if relay_candidates > 0 {
                                                        webrtc_message = format!("WebRTC peer connection successful with {relay_candidates} TURN relay candidates gathered");
                                                    } else {
                                                        webrtc_success = false;
                                                        webrtc_message = "TURN server configured but no relay candidates were gathered - TURN server may not be working".to_string();
                                                    }
                                                } else {
                                                    // For STUN-only or no-TURN tests, just check if we got any candidates
                                                    let total_candidates = webrtc_details
                                                        .get("total_candidates_count")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);

                                                    if total_candidates > 0 {
                                                        webrtc_message = format!("WebRTC peer connection successful with {total_candidates} ICE candidates gathered");
                                                    } else {
                                                        webrtc_success = false;
                                                        webrtc_message = "No ICE candidates were gathered - network connectivity issues".to_string();
                                                    }
                                                }
                                            } else {
                                                webrtc_success = false;
                                                webrtc_message = "ICE gathering completed but no local description available".to_string();
                                            }
                                        }
                                        Err(_) => {
                                            warn!(target: "connectivity_test", "ICE gathering timed out after {}ms", ice_timeout.as_millis());
                                            webrtc_details.insert(
                                                "ice_gathering_completed".to_string(),
                                                json!(false),
                                            );
                                            webrtc_details.insert(
                                                "ice_gathering_timeout".to_string(),
                                                json!(true),
                                            );

                                            // Even if it timed out, analyze what we got
                                            if let Some(local_desc) = pc.local_description().await {
                                                let sdp_text = local_desc.sdp;
                                                let candidate_analysis = analyze_ice_candidates(
                                                    &sdp_text,
                                                    using_real_credentials,
                                                );

                                                for (key, value) in candidate_analysis {
                                                    webrtc_details.insert(key, value);
                                                }

                                                let total_candidates = webrtc_details
                                                    .get("total_candidates_count")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0);

                                                if total_candidates > 0 {
                                                    webrtc_message = format!("ICE gathering timed out but {total_candidates} candidates were collected");
                                                } else {
                                                    webrtc_success = false;
                                                    webrtc_message = "ICE gathering timed out with no candidates collected".to_string();
                                                }
                                            } else {
                                                webrtc_success = false;
                                                webrtc_message = "ICE gathering timed out and no local description available".to_string();
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    webrtc_success = false;
                                    webrtc_message =
                                        format!("Failed to set local description: {e}");
                                    webrtc_details.insert(
                                        "set_local_description_error".to_string(),
                                        json!(e.to_string()),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            webrtc_success = false;
                            webrtc_message = format!("Failed to create offer: {e}");
                            webrtc_details
                                .insert("create_offer_error".to_string(), json!(e.to_string()));
                        }
                    }
                }
                Err(e) => {
                    webrtc_success = false;
                    webrtc_message =
                        format!("Peer connection created but data channel failed: {e}");
                    webrtc_details.insert("data_channel_created".to_string(), json!(false));
                    webrtc_details.insert("data_channel_error".to_string(), json!(e.to_string()));
                }
            }

            // Add final connection states
            webrtc_details.insert(
                "final_ice_gathering_state".to_string(),
                json!(format!("{:?}", pc.ice_gathering_state())),
            );
            webrtc_details.insert(
                "final_connection_state".to_string(),
                json!(format!("{:?}", pc.connection_state())),
            );

            results.insert(
                "webrtc_peer_connection".to_string(),
                json!({
                    "success": webrtc_success,
                    "duration_ms": webrtc_start.elapsed().as_millis(),
                    "message": webrtc_message,
                    "details": webrtc_details
                }),
            );

            // Close the peer connection
            let _ = pc.close().await;
        }
        Err(e) => {
            results.insert(
                "webrtc_peer_connection".to_string(),
                json!({
                    "success": false,
                    "duration_ms": webrtc_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("Failed to create WebRTC peer connection: {}", e)
                }),
            );
        }
    }

    // Overall test summary
    let total_duration = start_time.elapsed();
    let mut overall_success = true;
    let mut failed_tests = Vec::new();

    // Check each test result
    for (test_name, test_result) in &results {
        if let Some(success) = test_result.get("success").and_then(|v| v.as_bool()) {
            if !success {
                overall_success = false;
                failed_tests.push(test_name.clone());
            }
        }
    }

    results.insert("overall_result".to_string(), json!({
        "success": overall_success,
        "total_duration_ms": total_duration.as_millis(),
        "tests_run": results.len() - 1, // -1 to exclude this summary
        "failed_tests": failed_tests,
        "message": if overall_success {
            format!("All connectivity tests passed in {}ms", total_duration.as_millis())
        } else {
            format!("Connectivity test completed with {} failures in {}ms", failed_tests.len(), total_duration.as_millis())
        }
    }));

    // Add comprehensive IT-friendly recommendations and diagnostics
    let mut recommendations: Vec<String> = Vec::new();
    let mut advanced_diagnostics = serde_json::Map::new();
    let mut stun_alternatives: Vec<String> = Vec::new();

    // DNS troubleshooting
    if !results
        .get("dns_resolution")
        .unwrap()
        .get("success")
        .unwrap()
        .as_bool()
        .unwrap()
    {
        recommendations
            .push("CRITICAL: DNS resolution failed - this blocks all connectivity".to_string());
        recommendations.push("Verify DNS servers are configured and accessible".to_string());
        recommendations
            .push("Check if corporate DNS filtering is blocking the hostname".to_string());

        advanced_diagnostics.insert(
            "dns_troubleshooting".to_string(),
            json!({
                "priority": "critical",
                "commands": [
                    format!("nslookup {krelay_server}"),
                    format!("dig {krelay_server}"),
                    format!("dig @8.8.8.8 {krelay_server}  # Try Google DNS"),
                    format!("dig @1.1.1.1 {krelay_server}  # Try Cloudflare DNS"),
                    "cat /etc/resolv.conf  # Check DNS config (Linux)",
                    "scutil --dns  # Check DNS config (macOS)",
                    "ipconfig /all  # Check DNS config (Windows)"
                ],
                "network_tests": [
                    "ping 8.8.8.8  # Test basic internet",
                    "ping 1.1.1.1  # Test alternate DNS",
                    "nslookup google.com  # Test if DNS works for known domains"
                ]
            }),
        );
    }

    // TCP connectivity troubleshooting
    if !results
        .get("tcp_connectivity")
        .unwrap()
        .get("success")
        .unwrap()
        .as_bool()
        .unwrap()
    {
        recommendations
            .push("HIGH: TCP port 3478 blocked - STUN/TURN servers require this port".to_string());
        recommendations.push("Configure firewall to allow outbound TCP/3478".to_string());
        recommendations
            .push("Contact network administrator about STUN/TURN server access".to_string());

        advanced_diagnostics.insert(
            "tcp_troubleshooting".to_string(),
            json!({
                "priority": "high",
                "commands": [
                    format!("telnet {} 3478", krelay_server),
                    format!("nc -v {} 3478", krelay_server),
                    format!("nmap -p 3478 {}", krelay_server),
                    "netstat -an | grep 3478  # Check if port is in use locally"
                ],
                "firewall_checks": [
                    "Check corporate firewall rules for TCP/3478",
                    "Verify no local firewall blocking outbound connections",
                    "Test from different network (mobile hotspot) to isolate issue",
                    "Check if HTTP proxy is intercepting connections"
                ]
            }),
        );
    }

    // UDP binding troubleshooting
    if !results
        .get("udp_binding")
        .unwrap()
        .get("success")
        .unwrap()
        .as_bool()
        .unwrap()
    {
        recommendations.push("HIGH: Cannot bind UDP sockets - WebRTC requires UDP".to_string());
        recommendations.push("Check local security software blocking socket creation".to_string());

        advanced_diagnostics.insert(
            "udp_troubleshooting".to_string(),
            json!({
                "priority": "high",
                "commands": [
                    "netstat -ul  # List UDP sockets in use",
                    "ss -ul  # Alternative UDP socket listing",
                    "lsof -i UDP  # Show processes using UDP",
                    "ulimit -n  # Check file descriptor limits"
                ],
                "system_checks": [
                    "Check antivirus/security software UDP restrictions",
                    "Verify user has permission to create sockets",
                    "Test with elevated privileges if possible",
                    "Check system resource limits (ulimits)"
                ]
            }),
        );
    }

    // WebRTC-specific troubleshooting
    if let Some(webrtc_result) = results.get("webrtc_peer_connection") {
        if !webrtc_result.get("success").unwrap().as_bool().unwrap() {
            recommendations.push(
                "MEDIUM: WebRTC peer connection failed - protocol may be restricted".to_string(),
            );

            // Check ICE candidate results for specific guidance
            let relay_count = webrtc_result
                .get("details")
                .and_then(|d| d.get("relay_candidates_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let srflx_count = webrtc_result
                .get("details")
                .and_then(|d| d.get("srflx_candidates_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if using_real_credentials && relay_count == 0 {
                recommendations
                    .push("TURN server not accessible despite valid credentials".to_string());
                recommendations
                    .push("Verify TURN server is running and accepting connections".to_string());
            }

            if srflx_count == 0 {
                recommendations
                    .push("STUN server not accessible - NAT traversal will fail".to_string());
                recommendations.push(
                    "Verify your STUN server configuration and network connectivity".to_string(),
                );
            }

            advanced_diagnostics.insert(
                "webrtc_troubleshooting".to_string(),
                json!({
                    "priority": "medium",
                    "network_tests": [
                        format!("nc -u {} 3478  # Test UDP to STUN port", krelay_server),
                        format!("nc -u {} 5349  # Test UDP to TURNS port", krelay_server),
                        "Check if WebRTC is blocked by DPI (Deep Packet Inspection)",
                        "Test from browser developer tools: new RTCPeerConnection()"
                    ],
                    "protocol_checks": [
                        "Verify no SIP/RTP traffic filtering",
                        "Check if SRTP/DTLS protocols are allowed",
                        "Test if ICE/STUN packets are being filtered",
                        "Verify no VPN interference with UDP traffic"
                    ]
                }),
            );
        }
    }

    // STUN testing alternatives when TURN access is restricted
    let relay_count = results
        .get("webrtc_peer_connection")
        .and_then(|r| r.get("details"))
        .and_then(|d| d.get("relay_candidates_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if use_turn && (relay_count == 0 || !using_real_credentials) {
        stun_alternatives.extend([
            "Alternative STUN testing options (when TURN is restricted):".to_string(),
            format!("turnutils_stunclient {krelay_server}  # Basic STUN test (no auth required)"),
            "".to_string(),
            "Network troubleshooting for restricted environments:".to_string(),
            "1. Test basic UDP connectivity:".to_string(),
            format!("   nc -u {krelay_server} 3478 < /dev/null"),
            "2. Check NAT behavior:".to_string(),
            "   turnutils_natdiscovery".to_string(),
            "3. Test STUN binding requests:".to_string(),
            format!("   turnutils_stunclient -v {krelay_server}"),
        ]);

        advanced_diagnostics.insert(
            "stun_alternatives".to_string(),
            json!({
                "stun_test_commands": [
                    format!("turnutils_stunclient {}", krelay_server),
                    "turnutils_natdiscovery  # Discover NAT type"
                ],
                "troubleshooting_steps": [
                    "If STUN works but TURN doesn't: authentication or server issue",
                    "If no STUN response: UDP/3478 likely blocked by firewall",
                    "If local candidates only: severe network restrictions",
                    "Test alternative ports if 3478 is blocked (some STUN servers use 443, 80)"
                ]
            }),
        );
    }

    // Add common corporate network issues and solutions
    advanced_diagnostics.insert(
        "corporate_network_guidance".to_string(),
        json!({
            "common_issues": [
                "Corporate firewalls blocking UDP traffic (STUN/TURN requirement)",
                "Deep Packet Inspection (DPI) filtering WebRTC protocols",
                "Proxy servers interfering with direct connections",
                "NAT/Firewall blocking ICE connectivity checks",
                "VPN software interfering with local network discovery"
            ],
            "escalation_guidance": [
                "Request firewall exception for UDP/3478 (STUN)",
                "Request firewall exception for TCP/3478 (TURN over TCP)",
                "Consider TURN over TLS on port 443 if available",
                "Test during different times to rule out bandwidth throttling",
                "Document specific error messages for network team"
            ],
            "fallback_options": [
                "Configure TURN over TCP instead of UDP",
                "Use TURN over TLS (port 443) if supported",
                "Test with mobile hotspot to bypass corporate restrictions",
                "Consider VPN solutions that support WebRTC",
                "Request dedicated WebRTC infrastructure from IT"
            ]
        }),
    );

    if recommendations.is_empty() {
        recommendations.push(
            "SUCCESS: All connectivity tests passed - WebRTC should function properly".to_string(),
        );
        recommendations
            .push("Network configuration appears optimal for real-time communication".to_string());
    }

    // Add the enhanced stun alternatives if we have any
    if !stun_alternatives.is_empty() {
        recommendations.extend(stun_alternatives);
    }

    results.insert("recommendations".to_string(), json!(recommendations));
    results.insert(
        "advanced_diagnostics".to_string(),
        json!(advanced_diagnostics),
    );

    // Add suggested command line tests with enhanced options
    let mut cli_tests = Vec::new();

    // Basic connectivity tests
    cli_tests.push("# Basic connectivity tests:".to_string());
    cli_tests.push(format!("ping {krelay_server}  # Test basic reachability"));
    cli_tests.push(format!(
        "telnet {krelay_server} 3478  # Test TCP connectivity"
    ));
    cli_tests.push(format!(
        "nc -u {krelay_server} 3478 < /dev/null  # Test UDP connectivity"
    ));

    // STUN testing (works without credentials)
    cli_tests.push("".to_string());
    cli_tests.push("# STUN server testing (no credentials required):".to_string());
    cli_tests.push(format!(
        "turnutils_stunclient {krelay_server}  # Test STUN server"
    ));
    cli_tests.push("turnutils_natdiscovery  # Discover NAT type and behavior".to_string());

    if use_turn {
        cli_tests.push("".to_string());
        cli_tests.push("# TURN server testing (requires credentials):".to_string());

        // Try to use actual credentials if available
        if let (Some(ksm_cfg), Some(client_ver)) = (ksm_config, client_version) {
            if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                match get_relay_access_creds(ksm_cfg, None, client_ver).await {
                    Ok(creds) => {
                        if let (Some(username), Some(password)) = (
                            creds.get("username").and_then(|v| v.as_str()),
                            creds.get("password").and_then(|v| v.as_str()),
                        ) {
                            cli_tests.push(format!(
                                "turnutils_uclient -t -u {username} -w {password} {krelay_server}  # Test TURN with router credentials"
                            ));
                            cli_tests.push(format!(
                                "turnutils_uclient -k -u {username} -w {password} {krelay_server}  # Keep alive test"
                            ));
                        }
                    }
                    Err(_) => {
                        cli_tests.push(format!("turnutils_uclient -t -u USERNAME -w PASSWORD {krelay_server}  # Replace with actual credentials"));
                    }
                }
            }
        } else if let (Some(user), Some(pass)) = (username, password) {
            // Use passed credentials for CLI test
            cli_tests.push(format!(
                "turnutils_uclient -t -u {user} -w {pass} {krelay_server}"
            ));
        }
        // Don't add any CLI test if no credentials are available
    }

    results.insert("suggested_cli_tests".to_string(), json!(cli_tests));

    info!(target: "connectivity_test", "WebRTC connectivity test completed in {}ms - Success: {}", 
          total_duration.as_millis(), overall_success);

    Ok(results)
}

/// Analyze ICE candidates from SDP to determine TURN server functionality
fn analyze_ice_candidates(
    sdp: &str,
    using_real_credentials: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut analysis = serde_json::Map::new();

    let mut host_candidates = 0;
    let mut srflx_candidates = 0; // Server reflexive (STUN)
    let mut relay_candidates = 0; // TURN relay
    let mut total_candidates = 0;

    let mut candidate_details = Vec::new();

    debug!(target: "connectivity_test", "Analyzing SDP for ICE candidates");

    // Parse SDP for a= lines containing candidates
    for line in sdp.lines() {
        if line.starts_with("a=candidate:") {
            total_candidates += 1;

            // Parse the candidate line: a=candidate:foundation component transport priority ip port typ candidate-type
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 8 {
                let candidate_type = parts[7]; // typ value
                let ip = parts[4];
                let port = parts[5];
                let transport = parts[2];

                match candidate_type {
                    "host" => {
                        host_candidates += 1;
                        candidate_details.push(json!({
                            "type": "host",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "Local network interface"
                        }));
                    }
                    "srflx" => {
                        srflx_candidates += 1;
                        candidate_details.push(json!({
                            "type": "srflx",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "STUN server reflexive candidate"
                        }));
                    }
                    "relay" => {
                        relay_candidates += 1;
                        candidate_details.push(json!({
                            "type": "relay",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "TURN relay candidate"
                        }));
                    }
                    _ => {
                        // Other types like prflx (peer reflexive)
                        candidate_details.push(json!({
                            "type": candidate_type,
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": format!("Other candidate type: {}", candidate_type)
                        }));
                    }
                }
            }
        }
    }

    debug!(target: "connectivity_test", 
        "ICE candidate analysis: total={}, host={}, srflx={}, relay={}", 
        total_candidates, host_candidates, srflx_candidates, relay_candidates);

    // Add counts to analysis
    analysis.insert(
        "total_candidates_count".to_string(),
        json!(total_candidates),
    );
    analysis.insert("host_candidates_count".to_string(), json!(host_candidates));
    analysis.insert(
        "srflx_candidates_count".to_string(),
        json!(srflx_candidates),
    );
    analysis.insert(
        "relay_candidates_count".to_string(),
        json!(relay_candidates),
    );
    analysis.insert("candidate_details".to_string(), json!(candidate_details));

    // Analysis and recommendations
    let mut candidate_analysis = Vec::new();

    if host_candidates > 0 {
        candidate_analysis.push("Local network interfaces are accessible".to_string());
    } else {
        candidate_analysis
            .push("No local network candidates - unusual network configuration".to_string());
    }

    if srflx_candidates > 0 {
        candidate_analysis.push(format!(
            "STUN server is working - {srflx_candidates} reflexive candidates gathered"
        ));
    } else {
        candidate_analysis
            .push("No STUN reflexive candidates - STUN server may not be accessible".to_string());
    }

    if using_real_credentials {
        if relay_candidates > 0 {
            candidate_analysis.push(format!(
                "TURN server is working - {relay_candidates} relay candidates gathered with real credentials"
            ));
        } else {
            candidate_analysis.push(
                "TURN server not working - no relay candidates despite valid credentials"
                    .to_string(),
            );
        }
    } else if relay_candidates > 0 {
        candidate_analysis.push(format!(
                "TURN server appears accessible - {relay_candidates} relay candidates (but credentials not tested)"
        ));
    } else {
        candidate_analysis
            .push("TURN relay functionality not tested (no credentials provided)".to_string());
    }

    analysis.insert("candidate_analysis".to_string(), json!(candidate_analysis));

    // Overall assessment
    let mut overall_assessment = String::new();
    if total_candidates == 0 {
        overall_assessment =
            "Critical: No ICE candidates gathered - severe network connectivity issues".to_string();
    } else if using_real_credentials && relay_candidates == 0 {
        overall_assessment = "TURN server not functioning despite valid credentials".to_string();
    } else if relay_candidates > 0 {
        overall_assessment =
            "Network connectivity appears good with relay capabilities".to_string();
    } else if srflx_candidates > 0 {
        overall_assessment =
            "Basic connectivity working via STUN (NAT traversal possible)".to_string();
    } else if host_candidates > 0 {
        overall_assessment =
            "Only local candidates - may have issues with NAT traversal".to_string();
    }

    analysis.insert("overall_assessment".to_string(), json!(overall_assessment));

    analysis
}
