use std::collections::HashMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyBool, PyFloat, PyInt, PyNone, PyAny};
use tokio::sync::{mpsc::unbounded_channel};
use pyo3::exceptions::PyRuntimeError;
use tracing::{debug, info, trace, warn, error};
use crate::runtime::get_runtime;
use crate::tube_registry::REGISTRY;
use std::sync::Arc;

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
/// from pam_rustwebrtc import PyTubeRegistry
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
            let key_str = key.extract::<String>()
                .map_err(|e| PyRuntimeError::new_err(format!("Dict key is not a string: {}", e)))?;
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
                .ok_or_else(|| PyRuntimeError::new_err(format!("Failed to convert large Python int to JSON number: {:?}", py_obj)))
        }
    } else if py_obj.is_instance_of::<PyFloat>() {
        serde_json::Number::from_f64(py_obj.extract::<f64>()?)
            .map(serde_json::Value::Number)
            .ok_or_else(|| PyRuntimeError::new_err(format!("Failed to convert float to JSON number: {:?}", py_obj)))
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
fn pyobj_to_json_hashmap(py: Python<'_>, dict_obj: &PyObject) -> PyResult<HashMap<String, serde_json::Value>> {
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
pub struct PyTubeRegistry {}

#[pymethods]
impl PyTubeRegistry {
    #[new]
    fn new() -> Self {
        Self {}
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
    fn associate_conversation(&self, py: Python<'_>, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let mut registry = REGISTRY.write().await;
                registry.associate_conversation(&tube_id_owned, &connection_id_owned)
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to associate conversation: {}", e)))
            })
        })
    }
    
    /// find if a tube already exists
    fn tube_found(&self, py: Python<'_>, tube_id:&str) -> bool {
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
    
    /// Get all Connection IDs for a tube
    fn all_connection_ids(&self, py: Python<'_>, tube_id: &str) -> Vec<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.conversation_ids_by_tube_id(&tube_id_owned).into_iter().cloned().collect()
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
                registry.conversation_ids_by_tube_id(&tube_id_owned).into_iter().cloned().collect()
            })
        })
    }
    
    /// find tube by connection ID
    fn tube_id_from_connection_id(&self, py: Python<'_>, connection_id: &str) -> Option<String> {
        let master_runtime = get_runtime();
        let connection_id_owned = connection_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                let registry = REGISTRY.read().await;
                registry.tube_id_from_conversation_id(&connection_id_owned).cloned()
            })
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
    
    /// Create a new tube with WebRTC connection
    #[pyo3(signature = (
        conversation_id,
        settings,
        trickle_ice = false,
        callback_token = "",
        ksm_config = "",
        offer = None,
        signal_callback = None,
    ))]
    fn create_tube(
        &self,
        py: Python<'_>,
        conversation_id: &str,
        settings: PyObject,
        trickle_ice: bool,
        callback_token: &str,
        ksm_config: &str,
        offer: Option<&str>,
        signal_callback: Option<PyObject>,
    ) -> PyResult<PyObject> {
        let master_runtime = get_runtime();
        
        // Convert Python settings dictionary to Rust HashMap<String, serde_json::Value>
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Create an MPSC channel for signaling between Rust and Python
        let (signal_sender_rust, signal_receiver_py) = unbounded_channel::<crate::tube_registry::SignalMessage>();
        
        // Prepare owned versions of string parameters to move into async blocks
        let offer_string_owned = offer.map(String::from);
        let conversation_id_owned = conversation_id.to_string();
        let callback_token_owned = callback_token.to_string();
        let ksm_config_owned = ksm_config.to_string();

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
                    &ksm_config_owned,
                    signal_sender_rust, // Pass the sender part of the MPSC channel
                ).await
                 .map_err(|e| {
                    error!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: TubeRegistry::create_tube CRITICAL FAILURE: {}", e);
                    PyRuntimeError::new_err(format!("Failed to create tube via registry: {}", e))
                 })
            })
        })?; // Propagate errors from block_on or create_tube

        trace!(target: "lifecycle", conversation_id = %conversation_id, "PyBind: TubeRegistry::create_tube call complete. Result map has {} keys.", creation_result_map.len());

        // Extract tube_id for signal handler setup (it must be in the map)
        let final_tube_id = creation_result_map.get("tube_id")
            .ok_or_else(|| PyRuntimeError::new_err("Tube ID missing from create_tube response"))?
            .clone();

        // Setup Python signal handler if a callback was provided
        if let Some(cb) = signal_callback {
            setup_signal_handler(final_tube_id.clone(), signal_receiver_py, master_runtime.clone(), cb);
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
                    tube.create_offer().await
                        .map_err(|e| PyRuntimeError::new_err(format!("Failed to create offer: {}", e)))
                } else {
                    Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id_owned)))
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
                    tube.create_answer().await
                        .map_err(|e| PyRuntimeError::new_err(format!("Failed to create answer: {}", e)))
                } else {
                    Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id_owned)))
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
                registry.set_remote_description(tube_id, &sdp, is_answer).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to set remote description: {}", e)))
            })
        })
    }
    
    /// Add an ICE candidate to a tube
    fn add_ice_candidate(&self, _py: Python<'_>, tube_id: &str, candidate: String) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string(); 
        let candidate_owned = candidate;

        // Spawn the async work onto the runtime, don't block the current (potentially Tokio worker) thread
        master_runtime.spawn(async move {
            let registry = REGISTRY.read().await;
            if let Err(e) = registry.add_external_ice_candidate(&tube_id_owned, &candidate_owned).await {
                warn!(target: "python_bindings", "Error in spawned add_ice_candidate for tube {}: {}", tube_id_owned, e);
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
                registry.get_connection_state(&tube_id_owned).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to get connection state: {}", e)))
            })
        })
    }
    
    /// Add a connection
    #[pyo3(signature = (
        tube_id,
        connection_id,
        settings,
        offer = None,
        trickle_ice = false,
        signal_callback = None,
    ))]
    fn new_connection(
        &self,
        py: Python<'_>,
        tube_id: Option<&str>,
        connection_id: &str,
        settings: PyObject,
        offer: Option<&str>,
        trickle_ice: bool,
        signal_callback: Option<PyObject>,
    ) -> PyResult<String> {
        let master_runtime = get_runtime();
        
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Conditionally prepare for signal handling if a new tube is created with a callback
        let mut opt_signal_sender_for_rust: Option<tokio::sync::mpsc::UnboundedSender<crate::tube_registry::SignalMessage>> = None;
        let mut opt_signal_receiver_for_handler: Option<tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>> = None;
        let mut cb_obj_for_handler: Option<PyObject> = None;

        if tube_id.is_none() { // If creating a new tube
            if let Some(cb_provided) = signal_callback {
                let (tx, rx) = unbounded_channel();
                opt_signal_sender_for_rust = Some(tx);
                opt_signal_receiver_for_handler = Some(rx);
                cb_obj_for_handler = Some(cb_provided.clone_ref(py)); // Clone for the handler task
            }
        }
        
        let offer_string = offer.map(String::from);
        let connection_id_owned = connection_id.to_string();
        let tube_id_for_rust = tube_id.map(String::from);

        let result_new_tube_id = py.allow_threads(|| {
            master_runtime.clone().block_on(async move { 
                let mut registry = REGISTRY.write().await;
                registry.new_connection(
                    tube_id_for_rust.as_deref(), 
                    &connection_id_owned, 
                    settings_json, 
                    offer_string,
                    Some(trickle_ice), 
                    opt_signal_sender_for_rust // Pass the sender if created
                ).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create connection: {}", e)))
            })
        })?;
        
        // If a new tube was created, and we have a receiver and callback, set up the handler
        if tube_id.is_none() {
            if let (Some(receiver), Some(cb)) = (opt_signal_receiver_for_handler, cb_obj_for_handler) {
                setup_signal_handler(result_new_tube_id.clone(), receiver, master_runtime.clone(), cb);
            }
        }
        
        Ok(result_new_tube_id)
    }
    
    /// Close a specific connection on a tube
    #[pyo3(signature = (
        tube_id,
        connection_id,
    ))]
    fn close_connection(&self, py: Python<'_>, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();
        
        py.allow_threads(move || {
            master_runtime.clone().block_on(async move {
                let tube_result = {
                    let registry = REGISTRY.read().await;
                    registry.get_by_tube_id(&tube_id_owned)
                };

                if let Some(tube) = tube_result {
                    tube.close_channel(&connection_id_owned).await
                        .map_err(|e| PyRuntimeError::new_err(format!("Rust: Failed to close connection {} on tube {}: {}", connection_id_owned, tube_id_owned, e)))
                } else {
                    // Tube isn't found, perhaps already closed. Consider if this should be an error or a warning.
                    // For now, mirroring the PyRuntimeError pattern for consistency if an action was expected.
                    // However, if closing a non-existent connection is acceptable, Ok(()) might be better here.
                    // Let's make it an error if the tube itself isn't found, as an action was requested on it.
                    Err(PyRuntimeError::new_err(format!("Rust: Tube not found {} during close_connection for connection {}", tube_id_owned, connection_id_owned)))
                }
            })
        })
    }
    
    /// Close an entire tube
    fn close_tube(&self, py: Python<'_>, tube_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        
        py.allow_threads(move || {
            master_runtime.clone().block_on(async move { 
                let mut registry = REGISTRY.write().await;
                registry.close_tube(&tube_id_owned).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Rust: Failed to close tube {}: {}", tube_id_owned, e)))
            })
        })
    }
    
    /// Create a channel on a tube
    #[pyo3(signature = (
        connection_id,
        tube_id,
        settings,
        signal_callback = None,
    ))]
    fn create_channel(
        &self,
        py: Python<'_>,
        connection_id: &str,
        tube_id: &str,
        settings: PyObject,
        signal_callback: Option<PyObject>,
    ) -> PyResult<()> {
        let master_runtime = get_runtime();
        
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();

        if signal_callback.is_some() {
            warn!(target: "python_bindings", "PyTubeRegistry.create_channel was called with a signal_callback, but this is currently ignored for existing tubes.");
        }
        
        py.allow_threads(move || {
            master_runtime.clone().block_on(async move { 
                let mut registry = REGISTRY.write().await;
                registry.register_channel(&connection_id_owned, &tube_id_owned, &settings_json).await
                    .map_err(|e| PyRuntimeError::new_err(format!(
                        "Rust: Failed to register channel {} on tube {}: {}", 
                        connection_id_owned, tube_id_owned, e
                    )))
            })
        })
    }
    
    /// Get a tube object by conversation ID
    fn get_tube_id_by_conversation_id(&self, py: Python<'_>, conversation_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let conversation_id_owned = conversation_id.to_string();
        py.allow_threads(|| {
            master_runtime.clone().block_on(async move { 
                let registry = REGISTRY.read().await;
                if let Some(tube) = registry.get_by_conversation_id(&conversation_id_owned) {
                    Ok(tube.id().to_string())
                } else {
                    Err(PyRuntimeError::new_err(format!("No tube found for conversation: {}", conversation_id_owned)))
                }
            })
        })
    }
}

// Helper function to set up signal handling for a tube
fn setup_signal_handler(
    tube_id_key: String,
    mut signal_receiver: tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
    runtime: Arc<tokio::runtime::Runtime>, 
    callback_pyobj: PyObject, // Use the passed callback object
) {
    let task_tube_id = tube_id_key.clone(); 
    runtime.spawn(async move {
        info!(target: "python_bindings", "Signal handler task started for tube_id: {}", task_tube_id);
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
                        warn!("Error in Python signal callback for tube {}: {:?}", task_tube_id, e);
                    }
                } else {
                    warn!("Skipping Python callback for tube {} due to error setting dict items for signal {:?}", task_tube_id, signal.kind);
                }
            });
            debug!(target: "python_bindings", "Rust task completed Python callback GIL block for signal {}: {:?} for tube {}", signal_count, signal.kind, task_tube_id);
        }
        warn!(target: "python_bindings", "Signal handler task FOR TUBE {} IS TERMINATING (processed {} signals) because MPSC channel receive loop ended.", task_tube_id, signal_count);
    });
}