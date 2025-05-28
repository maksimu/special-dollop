use std::collections::HashMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyBool, PyFloat, PyInt, PyNone, PyAny};
use tokio::sync::{mpsc::unbounded_channel};
use pyo3::exceptions::PyRuntimeError;
use tracing::{debug, info, trace, warn, error};
use crate::runtime::get_runtime;
use crate::tube_registry::REGISTRY;
use std::sync::Arc;
use base64::{Engine as _};
use serde_json::json;

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
        
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        let (signal_sender, signal_receiver) = unbounded_channel();
        
        let offer_string = offer.map(String::from);
        let conversation_id_owned = conversation_id.to_string();
        let callback_token_owned = callback_token.to_string();
        let ksm_config_owned = ksm_config.to_string();

        // Clone values that need to be used in the second async block, 
        // as the originals will be moved into the first block.
        let offer_string_for_second_block = offer_string.clone();
        let conversation_id_for_second_block = conversation_id_owned.clone();
        // Ksm_config_owned and callback_token_owned are already cloned inside the second block where needed,
        // or they can be cloned here too if a more consistent pattern is desired.
        // For now, let's stick to cloning only what's strictly necessary for the reported errors.

        let (tube_arc_for_async_ops, initial_tube_id_for_async_ops) = py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                trace!(target: "lifecycle", "PyBind: Acquiring REGISTRY write lock (Phase 1)");
                let mut registry = REGISTRY.write().await;
                trace!(target: "lifecycle", "PyBind: REGISTRY write lock acquired (Phase 1)");

                // Check if a tube for this conversation_id already exists. 
                // This is a guard against re-creating a tube that might be in the process of being set up elsewhere
                // or if there's a rapid re-request for the same conversation.
                if let Some(existing_tube_id) = registry.conversation_mappings.get(&conversation_id_owned) {
                    if let Some(tube) = registry.tubes_by_id.get(existing_tube_id) {
                        warn!(target: "lifecycle", tube_id = %existing_tube_id, conversation_id = %conversation_id_owned, "PyBind: Tube for this conversation_id already exists. Returning existing tube Arc.");
                        // It's crucial that the returned tube_id here matches what Python expects for subsequent operations.
                        // The original conversation_id_owned is what Python used to make the request.
                        return Ok::<_, PyErr>((Arc::clone(tube), existing_tube_id.clone())); 
                    }
                }

                trace!(target: "lifecycle", conversation_id = %conversation_id_owned, "PyBind: Phase 1: About to call Tube::new().");
                let tube_arc = crate::tube::Tube::new(offer_string.is_none())
                    .map_err(|e| {
                        error!(target: "lifecycle", conversation_id = %conversation_id_owned, "PyBind: Phase 1: Tube::new() CRITICAL FAILURE: {}", e);
                        PyRuntimeError::new_err(format!("Failed to create tube instance: {}", e))
                    })?;
                let tube_id = tube_arc.id(); // Get ID right after creation
                trace!(target: "lifecycle", tube_id = %tube_id, conversation_id = %conversation_id_owned, "PyBind: Phase 1: Tube::new() successful, tube_id obtained.");

                // Existing trace log, slightly reworded for clarity in new sequence
                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: Tube instance created and ID confirmed."); 

                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: About to call registry.add_tube().");
                registry.add_tube(Arc::clone(&tube_arc));
                // Combined existing log with more detail
                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: registry.add_tube() called. Tube Arc cloned into registry.");

                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: About to call registry.associate_conversation().");
                registry.associate_conversation(&tube_id, &conversation_id_owned)
                    .map_err(|e| {
                        error!(target: "lifecycle", tube_id = %tube_id, conversation_id = %conversation_id_owned, "PyBind: Phase 1: associate_conversation() CRITICAL FAILURE: {}", e);
                        PyRuntimeError::new_err(format!("Failed to associate conversation: {}", e))
                    })?;
                // Combined existing log
                trace!(target: "lifecycle", tube_id = %tube_id, conversation_id = %conversation_id_owned, "PyBind: Phase 1: Conversation associated.");

                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: About to call registry.set_server_mode().");
                registry.set_server_mode(offer_string.is_none());
                // Combined existing log
                trace!(target: "lifecycle", tube_id = %tube_id, server_mode = offer_string.is_none(), "PyBind: Phase 1: Server mode set.");
                
                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: About to insert signal_sender into registry.signal_channels.");
                registry.signal_channels.insert(tube_id.clone(), signal_sender.clone());
                // Combined existing log
                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: Signal channel inserted. REGISTRY write lock will be released upon exiting block.");
                
                trace!(target: "lifecycle", tube_id = %tube_id, "PyBind: Phase 1: About to return Ok from block_on.");
                Ok::<_, PyErr>((tube_arc, tube_id))
            })
        })?;
        trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 1 complete. REGISTRY lock released.");

        let tube_id_for_error_path = initial_tube_id_for_async_ops.clone();

        let creation_outcome = py.allow_threads(|| {
            master_runtime.clone().block_on(async move {
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2 (async ops) started.");
                
                // Before starting long async ops, verify the tube is still in the registry.
                // This helps detect if it was removed by another thread between Phase 1 and Phase 2.
                {
                    let registry_guard = REGISTRY.read().await;
                    if !registry_guard.tubes_by_id.contains_key(&initial_tube_id_for_async_ops) {
                        error!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Tube disappeared from registry before Phase 2! Aborting Phase 2.");
                        return Err(PyRuntimeError::new_err(format!("Tube {} was removed from registry prematurely before Phase 2 operations could start.", initial_tube_id_for_async_ops)));
                    }
                    trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Tube confirmed in registry at start of Phase 2.");
                }

                tube_arc_for_async_ops.create_peer_connection(
                    None,
                    trickle_ice,
                    settings_json.get("turn_only").map_or(false, |v| v.as_bool().unwrap_or(false)),
                    ksm_config_owned.clone(),
                    callback_token_owned.clone(),
                    settings_json.clone(),
                    {
                        let registry_guard = REGISTRY.read().await;
                        registry_guard.signal_channels.get(&initial_tube_id_for_async_ops)
                            .cloned()
                            .ok_or_else(|| PyRuntimeError::new_err(format!("Signal sender not found for tube {} after initial registration", initial_tube_id_for_async_ops)))?
                    },
                ).await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create peer connection: {}", e)))?;
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: create_peer_connection completed.");

                if let Err(e) = tube_arc_for_async_ops.create_control_channel(ksm_config_owned.clone(), callback_token_owned.clone()).await {
                    warn!("Failed to create control channel for tube {}: {}", initial_tube_id_for_async_ops, e);
                }
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: create_control_channel attempt completed.");

                let data_channel_arc = tube_arc_for_async_ops.create_data_channel(&conversation_id_for_second_block, ksm_config_owned.clone(), callback_token_owned.clone()).await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create data channel: {}",e)))?;
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: create_data_channel completed.");
                
                let listening_port_option = tube_arc_for_async_ops.create_channel(&conversation_id_for_second_block, &data_channel_arc, None, settings_json.clone()).await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create logical channel: {}", e)))?;
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: create_channel (logical) completed.");

                let mut result_map = HashMap::new();
                result_map.insert("tube_id".to_string(), initial_tube_id_for_async_ops.clone());

                if let Some(port) = listening_port_option {
                    // Assuming 127.0.0.1 as host, consistent with how Channel::start_server often works for local listeners.
                    // If host can be dynamic, this might need adjustment or to be sourced from channel info.
                    result_map.insert("actual_local_listen_addr".to_string(), format!("127.0.0.1:{}", port));
                }

                if offer_string_for_second_block.is_none() { // Server mode
                    trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Server mode, creating offer.");
                    let offer_sdp = tube_arc_for_async_ops.create_offer().await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create offer: {}", e)))?;
                    result_map.insert("offer".to_string(), base64::engine::general_purpose::STANDARD.encode(offer_sdp)); 
                } else { // Client mode
                    trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Client mode, processing offer.");
                    if let Some(offer_sdp_b64_str) = &offer_string_for_second_block { 
                        trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Decoding offer SDP.");
                        let offer_decoded_bytes = base64::engine::general_purpose::STANDARD.decode(offer_sdp_b64_str)
                            .map_err(|e| PyRuntimeError::new_err(format!("Failed to decode offer SDP from base64: {}", e)))?;
                        let offer_plain_sdp = String::from_utf8(offer_decoded_bytes)
                            .map_err(|e| PyRuntimeError::new_err(format!("Failed to convert decoded offer SDP to String: {}", e)))?;
                        trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Offer SDP decoded. Setting remote description.");

                        tube_arc_for_async_ops.set_remote_description(offer_plain_sdp, false).await.map_err(|e| PyRuntimeError::new_err(format!("Client: Failed to set remote description (offer): {}", e)))?;
                        trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Remote description (offer) set. Creating answer.");
                        let answer_sdp = tube_arc_for_async_ops.create_answer().await.map_err(|e| PyRuntimeError::new_err(format!("Client: Failed to create answer: {}", e)))?;
                        trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Answer created. Encoding answer.");
                        result_map.insert("answer".to_string(), base64::engine::general_purpose::STANDARD.encode(answer_sdp)); 
                    } else {
                        // This path should ideally not be hit if offer_string_for_second_block was Some initially.
                        error!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Phase 2: Client mode, but offer_sdp_b64_str is None unexpectedly!");
                        return Err(PyRuntimeError::new_err("Offer SDP is required for client mode but was None after initial check"));
                    }
                }
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Offer/Answer logic completed.");
                
                // Set status to Active as all setup is complete from create_tube's perspective
                *tube_arc_for_async_ops.status.write().await = crate::tube_and_channel_helpers::TubeStatus::Active;
                trace!(target: "lifecycle", tube_id = %initial_tube_id_for_async_ops, "PyBind: Tube status set to Active.");

                Ok::<(HashMap<String, String>, String), PyErr>((result_map, initial_tube_id_for_async_ops.clone()))
            })
        });

        match creation_outcome {
            Ok((result_map_final, final_tube_id)) => {
                trace!(target: "lifecycle", tube_id = %final_tube_id, "PyBind: Phase 2 (async ops) complete. Result map has {} keys.", result_map_final.len());
                if let Some(cb) = signal_callback {
                    setup_signal_handler(final_tube_id.clone(), signal_receiver, master_runtime.clone(), cb);
                }
                let py_dict = PyDict::new(py);
                for (key, value) in result_map_final.iter() {
                    py_dict.set_item(key, value)?;
                }
                Ok(py_dict.into())
            }
            Err(e) => {
                error!(target: "lifecycle", tube_id = %tube_id_for_error_path, "PyBind: Phase 2 CRITICAL FAILURE: {}", e);
                let tube_id_for_cleanup = tube_id_for_error_path.clone();
                // Attempt to mark the tube as Failed in the registry.
                // This runs in a new block_on to ensure it executes even if the main runtime is unwinding parts of Phase 2.
                master_runtime.clone().block_on(async move {
                    let registry = REGISTRY.read().await;
                    if let Some(tube_to_mark_failed) = registry.get_by_tube_id(&tube_id_for_cleanup) {
                        let mut status = tube_to_mark_failed.status.write().await;
                        if *status != crate::tube_and_channel_helpers::TubeStatus::Closed 
                            && *status != crate::tube_and_channel_helpers::TubeStatus::Closing
                            && *status != crate::tube_and_channel_helpers::TubeStatus::Ready {
                            *status = crate::tube_and_channel_helpers::TubeStatus::Failed;
                            warn!(target: "lifecycle", tube_id = %tube_id_for_cleanup, "PyBind: Marked tube as Failed due to Phase 2 error.");
                        }
                    }
                });
                Err(e) // Propagate the original error to Python
            }
        }
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