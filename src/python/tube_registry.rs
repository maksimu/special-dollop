use std::collections::HashMap;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use parking_lot::RwLock;
use once_cell::sync::Lazy;
use pyo3::exceptions::PyRuntimeError;
use tokio::sync::mpsc::unbounded_channel;
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
    // Get the PyDict directly
    let dict = dict_obj.downcast_bound::<PyDict>(py)
        .map_err(|_| PyRuntimeError::new_err("Expected a dictionary"))?;
    let mut result = HashMap::new();
    
    // Iterate through the key-value pairs
    for (key, value) in dict.iter() {
        let key_str = key.extract::<String>()?;
        
        // Convert different Python types to serde_json::Value
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
            // Default to string representation
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
        let mut registry = REGISTRY.write();
        registry.set_server_mode(server_mode);
        Ok(())
    }
    
    /// Check if the registry is in server mode
    fn is_server_mode(&self) -> bool {
        let registry = REGISTRY.read();
        registry.is_server_mode()
    }
    
    /// Associate a conversation ID with a tube
    fn associate_conversation(&self, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let mut registry = REGISTRY.write();
        registry.associate_conversation(tube_id, connection_id)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to associate conversation: {}", e)))
    }
    
    /// find if a tube already exists
    fn tube_found(&self, tube_id:&str) -> bool {
        let registry = REGISTRY.read();
        registry.tubes_by_id.contains_key(tube_id)
    }
    
    /// Get all tube IDs
    fn all_tube_ids(&self) -> Vec<String> {
        let registry = REGISTRY.read();
        registry.all_tube_ids()
    }
    
    /// Get all Connection IDs for a tube
    fn all_connection_ids(&self, tube_id: &str) -> Vec<String> {
        let registry = REGISTRY.read();
        registry.conversation_ids_by_tube_id(tube_id).into_iter().cloned().collect()
    }
    
    /// Get all Conversation IDs by Tube ID
    fn get_conversation_ids_by_tube_id(&self, tube_id: &str) -> Vec<String> {
        let registry = REGISTRY.read();
        registry.conversation_ids_by_tube_id(tube_id).into_iter().cloned().collect()
    }
    
    /// find tube by connection ID
    fn tube_id_from_connection_id(&self, connection_id: &str) -> Option<String> {
        let registry = REGISTRY.read();
        registry.tube_id_from_conversation_id(connection_id).cloned()
    }
    
    /// Find tubes by partial match of tube ID or conversation ID
    fn find_tubes(&self, search_term: &str) -> PyResult<Vec<String>> {
        let registry = REGISTRY.read();
        let tubes = registry.find_tubes(search_term);
            
        Ok(tubes)
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
        let runtime = get_runtime();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Setup signal channel and handling
        let (signal_sender, signal_receiver) = unbounded_channel();
        
        // If there's a signal callback, register it with a temporary ID
        let temp_id = if signal_callback.is_some() {
            let temp_id = format!("temp_{}", Uuid::new_v4().to_string());
            let mut callbacks = PYTHON_CALLBACKS.write();
            if let Some(callback) = signal_callback {
                callbacks.insert(temp_id.clone(), callback);
            }
            Some(temp_id)
        } else {
            None
        };
        
        // Convert offer &str to String if present
        let offer_string = offer.map(String::from);
        
        // Create the tube using the registry
        let result_map = runtime.block_on(async move {
            let mut registry = REGISTRY.write();
            
            let result_map = registry.create_tube(
                conversation_id,
                settings_json.clone(),
                offer_string,
                trickle_ice,
                callback_token,
                ksm_config,
                signal_sender,
            ).await.map_err(|e| PyRuntimeError::new_err(format!("Failed to create tube: {}", e)))?;
            
            // Get the tube_id from the result
            let tube_id = result_map.get("tube_id")
                .ok_or_else(|| PyRuntimeError::new_err("No tube_id in result"))?
                .clone();
                
            // Update the callback registry with the real tube ID if we have a temp_id
            if let Some(tid) = temp_id {
                let mut callbacks = PYTHON_CALLBACKS.write();
                if let Some(callback) = callbacks.remove(&tid) {
                    callbacks.insert(tube_id.clone(), callback);
                    
                    // Use the helper function to set up signal handling
                    setup_signal_handler(tube_id.clone(), signal_receiver);
                }
            }
            
            Ok::<HashMap<String, String>, PyErr>(result_map)
        })?;
        
        // Convert the result HashMap to a Python dictionary
        let py_dict = PyDict::new(py);
        for (key, value) in result_map.iter() {
            py_dict.set_item(key, value)?;
        }
        
        Ok(py_dict.into())
    }
    
    /// Create an offer for a tube
    fn create_offer(&self, tube_id: &str) -> PyResult<String> {
        let registry = REGISTRY.read();
        if let Some(tube) = registry.get_by_tube_id(tube_id) {
            let runtime = get_runtime();
            runtime.block_on(async move {
                tube.create_offer().await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create offer: {}", e)))
            })
        } else {
            Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))
        }
    }
    
    /// Create an answer for a tube
    fn create_answer(&self, tube_id: &str) -> PyResult<String> {
        let registry = REGISTRY.read();
        if let Some(tube) = registry.get_by_tube_id(tube_id) {
            let runtime = get_runtime();
            runtime.block_on(async move {
                tube.create_answer().await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to create answer: {}", e)))
            })
        } else {
            Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))
        }
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
        let runtime = get_runtime();
        runtime.block_on(async move {
            let registry = REGISTRY.read();
            registry.set_remote_description(tube_id, &sdp, is_answer).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to set remote description: {}", e)))
        })
    }
    
    /// Add an ICE candidate to a tube
    fn add_ice_candidate(&self, tube_id: &str, candidate: String) -> PyResult<()> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            let registry = REGISTRY.read();
            registry.add_external_ice_candidate(tube_id, &candidate).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to add ICE candidate: {}", e)))
        })
    }
    
    /// Get connection state of a tube
    fn get_connection_state(&self, tube_id: &str) -> PyResult<String> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            let registry = REGISTRY.read();
            registry.get_connection_state(tube_id).await
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
        let runtime = get_runtime();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Create signal channel if we're creating a new tube
        let (signal_sender, signal_receiver) = if tube_id.is_none() && signal_callback.is_some() {
            let (tx, rx) = unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        
        // If there's a signal callback and we're creating a new tube, register it with a temporary ID
        let temp_id = if tube_id.is_none() && signal_callback.is_some() {
            let temp_id = format!("temp_{}", Uuid::new_v4().to_string());
            let mut callbacks = PYTHON_CALLBACKS.write();
            if let Some(callback) = signal_callback {
                callbacks.insert(temp_id.clone(), callback);
            }
            Some(temp_id)
        } else {
            None
        };
        
        // Convert offer &str to String if present
        let offer_string = offer.map(String::from);
        
        // Call the Rust implementation
        let result = runtime.block_on(async move {
            let mut registry = REGISTRY.write();
            let new_tube_id = registry.new_connection(
                tube_id, 
                connection_id, 
                settings_json, 
                offer_string,
                Some(trickle_ice), 
                signal_sender
            ).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create connection: {}", e)))?;
            
            // If we created a new tube and have a signal callback, set up signal handling
            if tube_id.is_none() && temp_id.is_some() && signal_receiver.is_some() {
                let mut callbacks = PYTHON_CALLBACKS.write();
                if let Some(tid) = temp_id {
                    if let Some(callback) = callbacks.remove(&tid) {
                        callbacks.insert(new_tube_id.clone(), callback);
                        
                        // Use the helper function to set up signal handling
                        if let Some(receiver) = signal_receiver {
                            setup_signal_handler(new_tube_id.clone(), receiver);
                        }
                    }
                }
            }
            
            Ok::<String, PyErr>(new_tube_id)
        })?;
        
        Ok(result)
    }
    
    /// Close a specific connection on a tube
    #[pyo3(signature = (
        tube_id,
        connection_id,
    ))]
    fn close_connection(&self, tube_id: &str, connection_id: &str) -> PyResult<()> {
        let runtime = get_runtime();
        
        runtime.block_on(async move {
            // Get the tube from registry
            let tube = {
                let registry = REGISTRY.read();
                registry.get_by_tube_id(tube_id)
                    .ok_or_else(|| PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))?
            };
            
            // Close the specific connection
            tube.close_channel(connection_id)
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to close connection: {}", e)))
        })
    }
    
    /// Close an entire tube
    fn close_tube(&self, tube_id: &str) -> PyResult<()> {
        let runtime = get_runtime();
        
        runtime.block_on(async move {
            // Directly use the Rust implementation
            let mut registry = REGISTRY.write();
            registry.close_tube(tube_id).await
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
        let mut registry = REGISTRY.write();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Register the signal callback if provided
        if let Some(callback) = signal_callback {
            let mut callbacks = PYTHON_CALLBACKS.write();
            callbacks.insert(tube_id.to_string(), callback);
            
            // Get or create a signal channel
            let _signal_sender = registry.get_signal_channel(tube_id).unwrap_or_else(|| {
                let (sender, receiver) = unbounded_channel();
                registry.signal_channels.insert(tube_id.to_string(), sender.clone());
                
                // Set up the signal handler with the receiver
                setup_signal_handler(tube_id.to_string(), receiver);
                
                sender
            });
        }
        
        let runtime = get_runtime();
        runtime.block_on(async move {
            // Use register_channel from the Rust implementation
            registry.register_channel(connection_id, tube_id, &settings_json).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create channel: {}", e)))?;
                
            Ok::<(), PyErr>(())
        })?;
        
        // Return success (None in Python)
        Ok(py.None())
    }
    
    /// Unregister a signal callback for a tube
    fn unregister_signal_callback(&self, tube_id: &str) -> PyResult<()> {
        let mut callbacks = PYTHON_CALLBACKS.write();
        callbacks.remove(tube_id);
        Ok(())
    }
    
    /// Get a tube object by conversation ID
    fn get_tube_id_by_conversation_id(&self, conversation_id: &str) -> PyResult<String> {
        let registry = REGISTRY.read();
        if let Some(tube) = registry.get_by_conversation_id(conversation_id) {
            Ok(tube.id().to_string())
        } else {
            Err(PyRuntimeError::new_err(format!("No tube found for conversation: {}", conversation_id)))
        }
    }
}

// Helper function to set up signal handling for a tube
fn setup_signal_handler(
    tube_id: String,
    mut signal_receiver: tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
) {
    tokio::spawn(async move {
        while let Some(signal) = signal_receiver.recv().await {
            // Handle the signal by calling the Python callback
            Python::with_gil(|py| {
                let callbacks = PYTHON_CALLBACKS.read();
                if let Some(callback) = callbacks.get(&tube_id) {
                    // Create a Python dictionary from the signal
                    let py_dict = PyDict::new(py);
                    py_dict.set_item("tube_id", &signal.tube_id).unwrap();
                    py_dict.set_item("kind", &signal.kind).unwrap();
                    py_dict.set_item("data", &signal.data).unwrap();
                    py_dict.set_item("conversation_id", &signal.conversation_id).unwrap();
                    
                    // Call the Python callback
                    if let Err(e) = callback.call1(py, (py_dict,)) {
                        let _ = e.print(py);
                    }
                }
            });
        }
    });
}