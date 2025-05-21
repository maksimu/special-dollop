use std::collections::HashMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::sync::{RwLock, mpsc::unbounded_channel};
use once_cell::sync::Lazy;
use pyo3::exceptions::PyRuntimeError;
use tracing::warn;
use uuid::Uuid;
use crate::runtime::get_runtime;
use crate::tube_registry::REGISTRY;

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

// Map of tube IDs to Python callbacks
pub(crate) static PYTHON_CALLBACKS: Lazy<RwLock<HashMap<String, PyObject>>> = Lazy::new(|| {
    RwLock::new(HashMap::new())
});

/// Convert a Python dictionary to HashMap<String, serde_json::Value>
fn pyobj_to_json_hashmap(py: Python<'_>, dict_obj: &PyObject) -> PyResult<HashMap<String, serde_json::Value>> {
    let bound_dict = dict_obj.downcast_bound::<PyDict>(py)
        .map_err(|_| PyRuntimeError::new_err("Expected a dictionary"))?;
    let mut result = HashMap::new();
    
    for (key, value) in bound_dict.iter() {
        let key_str = key.extract::<String>()?;
        
        if let Ok(bool_val) = value.extract::<bool>() {
            result.insert(key_str, serde_json::Value::Bool(bool_val));
        } else if let Ok(int_val) = value.extract::<i64>() {
            result.insert(key_str, serde_json::Value::Number(serde_json::Number::from(int_val)));
        } else if let Ok(float_val) = value.extract::<f64>() {
            if let Some(num) = serde_json::Number::from_f64(float_val) {
                result.insert(key_str, serde_json::Value::Number(num));
            }
        } else if value.is_none() {
            result.insert(key_str, serde_json::Value::Null);
        } else {
            let str_val = value.str()?.to_string();
            result.insert(key_str, serde_json::Value::String(str_val));
        }
    }
    
    Ok(result)
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
    fn set_server_mode(&self, server_mode: bool) -> PyResult<()> {
        let master_runtime = get_runtime();
        master_runtime.clone().block_on(async move {
            let mut registry = REGISTRY.write().await;
            registry.set_server_mode(server_mode).await;
        });
        Ok(())
    }
    
    /// Check if the registry is in server mode
    fn is_server_mode(&self) -> bool {
        let master_runtime = get_runtime();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.is_server_mode().await
        })
    }
    
    /// Associate a conversation ID with a tube
    fn associate_conversation(&self, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();
        master_runtime.clone().block_on(async move {
            let mut registry = REGISTRY.write().await;
            registry.associate_conversation(&tube_id_owned, &connection_id_owned).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to associate conversation: {}", e)))
        })
    }
    
    /// find if a tube already exists
    fn tube_found(&self, tube_id:&str) -> bool {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.get_by_tube_id(&tube_id_owned).await.is_some()
        })
    }
    
    /// Get all tube IDs
    fn all_tube_ids(&self) -> Vec<String> {
        let master_runtime = get_runtime();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.all_tube_ids().await
        })
    }
    
    /// Get all Connection IDs for a tube
    fn all_connection_ids(&self, tube_id: &str) -> Vec<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.conversation_ids_by_tube_id(&tube_id_owned).await.into_iter().cloned().collect()
        })
    }
    
    /// Get all Conversation IDs by Tube ID
    fn get_conversation_ids_by_tube_id(&self, tube_id: &str) -> Vec<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.conversation_ids_by_tube_id(&tube_id_owned).await.into_iter().cloned().collect()
        })
    }
    
    /// find tube by connection ID
    fn tube_id_from_connection_id(&self, connection_id: &str) -> Option<String> {
        let master_runtime = get_runtime();
        let connection_id_owned = connection_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.tube_id_from_conversation_id(&connection_id_owned).await.cloned()
        })
    }
    
    /// Find tubes by partial match of tube ID or conversation ID
    fn find_tubes(&self, search_term: &str) -> PyResult<Vec<String>> {
        let master_runtime = get_runtime();
        let search_term_owned = search_term.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            let tubes = registry.find_tubes(&search_term_owned).await;
            Ok(tubes)
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
        
        let temp_callback_id_for_storage = if let Some(callback_py_obj) = signal_callback.as_ref() {
            let temp_id = format!("temp_callback_store_{}", Uuid::new_v4());
            let callback_clone_for_map = callback_py_obj.clone_ref(py);
            master_runtime.clone().block_on(async { 
                let mut callbacks = PYTHON_CALLBACKS.write().await;
                callbacks.insert(temp_id.clone(), callback_clone_for_map);
            });
            Some(temp_id)
        } else {
            None
        };
        
        let offer_string = offer.map(String::from);
        let conversation_id_owned = conversation_id.to_string();
        let callback_token_owned = callback_token.to_string();
        let ksm_config_owned = ksm_config.to_string();

        let creation_outcome = master_runtime.clone().block_on(async move {
            let mut registry = REGISTRY.write().await;
            
            let result_map = registry.create_tube(
                &conversation_id_owned,
                settings_json.clone(), 
                offer_string,
                trickle_ice,
                &callback_token_owned,
                &ksm_config_owned,
                signal_sender, 
            ).await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create tube: {}", e)))?;
            
            let tube_id = result_map.get("tube_id")
                .ok_or_else(|| PyRuntimeError::new_err("No tube_id in result from registry.create_tube"))?
                .clone();
                
            if let Some(tid) = temp_callback_id_for_storage {
                let mut callbacks = PYTHON_CALLBACKS.write().await;
                if let Some(callback_obj) = callbacks.remove(&tid) {
                    callbacks.insert(tube_id.clone(), callback_obj);
                } else {
                    warn!("Callback for temp_id {} not found when creating tube {}", tid, tube_id);
                }
            }
            
            Ok::< (HashMap<String, String>, String), PyErr>((result_map, tube_id))
        })?;

        let (result_map, final_tube_id) = creation_outcome;

        if signal_callback.is_some() {
            setup_signal_handler(final_tube_id.clone(), signal_receiver, master_runtime.clone());
        }
        
        let py_dict = PyDict::new(py);
        for (key, value) in result_map.iter() {
            py_dict.set_item(key, value)?;
        }
        
        Ok(py_dict.into())
    }
    
    /// Create an offer for a tube
    fn create_offer(&self, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            if let Some(tube) = registry.get_by_tube_id(&tube_id_owned).await {
                tube.create_offer().await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create offer: {}", e)))
            } else {
                Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))
            }
        })
    }
    
    /// Create an answer for a tube
    fn create_answer(&self, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            if let Some(tube) = registry.get_by_tube_id(&tube_id_owned).await {
                tube.create_answer().await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create answer: {}", e)))
            } else {
                Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))
            }
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
        tube_id: &str,
        sdp: String,
        is_answer: bool,
    ) -> PyResult<Option<String>> {
        let master_runtime = get_runtime();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.set_remote_description(tube_id, &sdp, is_answer).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to set remote description: {}", e)))
        })
    }
    
    /// Add an ICE candidate to a tube
    fn add_ice_candidate(&self, tube_id: &str, candidate: String) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string(); 
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.add_external_ice_candidate(&tube_id_owned, &candidate).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to add ICE candidate: {}", e)))
        })
    }
    
    /// Get connection state of a tube
    fn get_connection_state(&self, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move {
            let registry = REGISTRY.read().await;
            registry.get_connection_state(&tube_id_owned).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to get connection state: {}", e)))
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
        
        let (opt_signal_sender, opt_signal_receiver) = 
            if tube_id.is_none() && signal_callback.is_some() {
                let (tx, rx) = unbounded_channel();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };
        
        let temp_id_for_new_tube_callback = if tube_id.is_none() && signal_callback.is_some() {
            let temp_id = format!("temp_new_conn_callback_{}", Uuid::new_v4().to_string());
            if let Some(sc) = signal_callback.as_ref() {
                 let callback_clone = sc.clone_ref(py);
                 master_runtime.clone().block_on(async { 
                    let mut callbacks = PYTHON_CALLBACKS.write().await;
                    callbacks.insert(temp_id.clone(), callback_clone);
                });
            }
            Some(temp_id)
        } else {
            None
        };
        
        let offer_string = offer.map(String::from);
        let connection_id_owned = connection_id.to_string();
        let tube_id_owned = tube_id.map(String::from);

        let result_new_tube_id = master_runtime.clone().block_on(async move { 
            let mut registry = REGISTRY.write().await;
            let new_tube_id_str = registry.new_connection(
                tube_id_owned.as_deref(), 
                &connection_id_owned, 
                settings_json, 
                offer_string,
                Some(trickle_ice), 
                opt_signal_sender 
            ).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create connection: {}", e)))?;
            
            if tube_id_owned.is_none() { 
                if let Some(tid) = temp_id_for_new_tube_callback {
                    let mut callbacks = PYTHON_CALLBACKS.write().await;
                    if let Some(callback_obj) = callbacks.remove(&tid) {
                        callbacks.insert(new_tube_id_str.clone(), callback_obj);
                    } else {
                         warn!("Callback for temp_id {} not found during new_connection for new tube {}", tid, new_tube_id_str);
                    }
                }
            }
            
            Ok::<String, PyErr>(new_tube_id_str)
        })?;
        
        if tube_id.is_none() && signal_callback.is_some() { 
            if let Some(receiver) = opt_signal_receiver {
                setup_signal_handler(result_new_tube_id.clone(), receiver, master_runtime.clone()); 
            }
        }
        
        Ok(result_new_tube_id)
    }
    
    /// Close a specific connection on a tube
    #[pyo3(signature = (
        tube_id,
        connection_id,
    ))]
    fn close_connection(&self, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();
        
        master_runtime.clone().block_on(async move { 
            let tube = {
                let registry = REGISTRY.read().await;
                registry.get_by_tube_id(&tube_id_owned).await
                    .ok_or_else(|| PyRuntimeError::new_err(format!("Tube not found: {}", tube_id_owned)))?
            };
            
            tube.close_channel(&connection_id_owned).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to close connection: {}", e)))
        })
    }
    
    /// Close an entire tube
    fn close_tube(&self, tube_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        
        master_runtime.clone().block_on(async move { 
            let mut registry = REGISTRY.write().await;
            registry.close_tube(&tube_id_owned).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to close tube: {}", e)))
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
    ) -> PyResult<PyObject> { 
        let master_runtime = get_runtime();
        
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        let tube_id_owned = tube_id.to_string();
        let connection_id_owned = connection_id.to_string();

        if let Some(callback) = signal_callback {
             master_runtime.clone().block_on(async { 
                let mut callbacks = PYTHON_CALLBACKS.write().await;
                callbacks.insert(tube_id_owned.clone(), callback); 
            });

            let (receiver_to_use, new_receiver_created) = master_runtime.clone().block_on(async { 
                let mut registry_w = REGISTRY.write().await;
                if registry_w.signal_channels.contains_key(&tube_id_owned) {
                     let (sender, receiver) = unbounded_channel();
                     registry_w.signal_channels.entry(tube_id_owned.clone()).or_insert(sender);
                     (Some(receiver), true) 
                } else {
                    let (sender, receiver) = unbounded_channel();
                    registry_w.signal_channels.insert(tube_id_owned.clone(), sender);
                    (Some(receiver), true)
                }
            });
            
            if new_receiver_created {
                if let Some(receiver) = receiver_to_use {
                     setup_signal_handler(tube_id_owned.clone(), receiver, master_runtime.clone()); 
                }
            }
        }
        
        master_runtime.clone().block_on(async move { 
            let mut registry = REGISTRY.write().await;
            registry.register_channel(&connection_id_owned, &tube_id_owned, &settings_json).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to register channel: {}", e)))
        })?;
        
        Ok(py.None()) 
    }
    
    /// Unregister a signal callback for a tube
    fn unregister_signal_callback(&self, tube_id: &str) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        master_runtime.clone().block_on(async move { 
            let mut callbacks = PYTHON_CALLBACKS.write().await;
            callbacks.remove(&tube_id_owned);
        });
        Ok(())
    }
    
    /// Get a tube object by conversation ID
    fn get_tube_id_by_conversation_id(&self, conversation_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let conversation_id_owned = conversation_id.to_string();
        master_runtime.clone().block_on(async move { 
            let registry = REGISTRY.read().await;
            if let Some(tube) = registry.get_by_conversation_id(&conversation_id_owned).await {
                Ok(tube.id().to_string())
            } else {
                Err(PyRuntimeError::new_err(format!("No tube found for conversation: {}", conversation_id)))
            }
        })
    }
}

// Helper function to set up signal handling for a tube
fn setup_signal_handler(
    tube_id_key: String,
    mut signal_receiver: tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
    runtime: std::sync::Arc<tokio::runtime::Runtime>, // Takes ownership of a cloned Arc
) {
    runtime.spawn(async move { // Uses the owned Arc to spawn
        while let Some(signal) = signal_receiver.recv().await {
            // Acquire the lock and retrieve the callback PyObject outside Python::with_gil
            let callback_pyobj_opt: Option<PyObject> = {
                let callbacks_guard = PYTHON_CALLBACKS.read().await;
                // Clone the PyObject while the guard is held, if it exists
                callbacks_guard.get(&tube_id_key).map(|py_obj_ref| {
                    Python::with_gil(|py| py_obj_ref.clone_ref(py))
                })
            };

            if let Some(actual_callback) = callback_pyobj_opt {
                Python::with_gil(|py| {
                    let py_dict = PyDict::new(py);
                    if let Err(e) = py_dict.set_item("tube_id", &signal.tube_id) {
                        warn!("Failed to set 'tube_id' in signal dict for {}: {:?}", tube_id_key, e);
                        return;
                    }
                    if let Err(e) = py_dict.set_item("kind", &signal.kind) {
                        warn!("Failed to set 'kind' in signal dict for {}: {:?}", tube_id_key, e);
                        return;
                    }
                    if let Err(e) = py_dict.set_item("data", &signal.data) {
                        warn!("Failed to set 'data' in signal dict for {}: {:?}", tube_id_key, e);
                        return;
                    }
                    if let Err(e) = py_dict.set_item("conversation_id", &signal.conversation_id) {
                        warn!("Failed to set 'conversation_id' in signal dict for {}: {:?}", tube_id_key, e);
                        return;
                    }
                    
                    let result = actual_callback.call1(py, (py_dict,));
                    if let Err(e) = result {
                        warn!("Error in Python signal callback for tube {}: {:?}", tube_id_key, e);
                    }
                });
            } else {
                warn!("No Python callback found for tube_id_key: {}", tube_id_key);
            }
        }
    });
}