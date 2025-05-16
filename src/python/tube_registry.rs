use crate::runtime::get_runtime;
use crate::tube_registry::REGISTRY;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use crate::tube::Tube;
use std::collections::HashMap;
use std::sync::Arc;
use pyo3::types::{PyDict, PyString};
use crate::tube_registry::SignalMessage;
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use parking_lot::RwLock;
use once_cell::sync::Lazy;

// Map of tube IDs to Python callbacks
static PYTHON_CALLBACKS: Lazy<RwLock<HashMap<String, PyObject>>> = Lazy::new(|| {
    RwLock::new(HashMap::new())
});

/// Convert PyObject to HashMap of serde_json::Value
fn pyobj_to_json_hashmap(py: Python<'_>, dict_obj: &PyObject) -> PyResult<HashMap<String, serde_json::Value>> {
    let dict = dict_obj.downcast_bound::<PyDict>(py)?;
    let mut result = HashMap::new();
    
    for key_pair in dict.iter() {
        let (key, value) = key_pair;
        if let Ok(key_str) = key.extract::<String>() {
            // Try to convert PyAny to serde_json::Value
            if let Ok(value_str) = value.extract::<String>() {
                result.insert(key_str, serde_json::Value::String(value_str));
            } else if let Ok(value_bool) = value.extract::<bool>() {
                result.insert(key_str, serde_json::Value::Bool(value_bool));
            } else if let Ok(value_i64) = value.extract::<i64>() {
                result.insert(key_str, serde_json::Value::Number(serde_json::Number::from(value_i64)));
            } else if let Ok(value_f64) = value.extract::<f64>() {
                if let Some(num) = serde_json::Number::from_f64(value_f64) {
                    result.insert(key_str, serde_json::Value::Number(num));
                }
            }
        }
    }
    
    Ok(result)
}

/// Python representation of a Tube
#[pyclass]
pub struct PyTube {
    pub tube: Arc<Tube>,
    #[pyo3(get)]
    pub sdp: Option<String>,
    signal_receiver: Option<Arc<Mutex<Receiver<SignalMessage>>>>,
}

#[pymethods]
impl PyTube {
    #[getter]
    fn id(&self) -> String {
        self.tube.id()
    }
    
    #[getter]
    fn status(&self) -> String {
        self.tube.status().to_string()
    }
    
    /// Get connection state
    fn connection_state(&self) -> PyResult<String> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            Ok(self.tube.connection_state().await)
        })
    }
    
    /// Close the tube
    fn close(&self) -> PyResult<()> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            self.tube.close().await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to close tube: {}", e)))
        })
    }
    
    /// Create an offer
    fn create_offer(&self) -> PyResult<String> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            self.tube.create_offer().await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create offer: {}", e)))
        })
    }
    
    /// Create an answer
    fn create_answer(&self) -> PyResult<String> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            self.tube.create_answer().await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create answer: {}", e)))
        })
    }
    
    /// Register a callback for signaling events
    fn register_signal_callback(&self, callback: PyObject) -> PyResult<()> {
        let mut callbacks = PYTHON_CALLBACKS.write();
        callbacks.insert(self.tube.id(), callback);
        Ok(())
    }
    
    /// Process pending signaling events (call periodically from Python)
    fn process_signals(&self) -> PyResult<()> {
        if let Some(receiver) = &self.signal_receiver {
            let receiver_guard = receiver.lock().map_err(|_| PyRuntimeError::new_err("Failed to lock receiver"))?;
            
            // Check if we have a callback registered
            let callbacks = PYTHON_CALLBACKS.read();
            let callback = match callbacks.get(&self.tube.id()) {
                Some(cb) => cb,
                None => return Ok(()) // No callback registered, nothing to do
            };
            
            // Process messages in a non-blocking way
            while let Ok(message) = receiver_guard.try_recv() {
                Python::with_gil(|py| {
                    // Convert the message to Python dict
                    let py_dict = PyDict::new(py);
                    py_dict.set_item("tube_id", message.tube_id).unwrap();
                    py_dict.set_item("kind", message.kind).unwrap();
                    py_dict.set_item("data", message.data).unwrap();
                    py_dict.set_item("conversation_id", message.conversation_id).unwrap();
                    
                    // Call the Python callback
                    if let Err(e) = callback.call1(py, (py_dict,)) {
                        let _ = e.print(py); // Print error but continue
                    }
                });
            }
        }
        
        Ok(())
    }
    
    /// Add an ICE candidate
    fn add_ice_candidate(&self, candidate: String) -> PyResult<()> {
        let runtime = get_runtime();
        runtime.block_on(async move {
            self.tube.add_ice_candidate(candidate).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to add ICE candidate: {}", e)))
        })
    }
    
    /// Get all channel IDs
    fn get_channels(&self) -> Vec<String> {
        let channels = self.tube.channels.read();
        channels.keys().cloned().collect()
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
    fn find_tubes(&self, search_term: &str) -> PyResult<Vec<PyTube>> {
        let registry = REGISTRY.read();
        let tubes = registry.find_tubes(search_term);
        
        let result = tubes.into_iter()
            .map(|tube| PyTube {
                tube,
                sdp: None,
                signal_receiver: None,
            })
            .collect();
            
        Ok(result)
    }
    
    /// Create a new tube with WebRTC connection
    #[pyo3(signature = (
        conversation_id,
        protocol_type,
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
        protocol_type: &str,
        settings: PyObject,
        trickle_ice: bool,
        callback_token: &str,
        ksm_config: &str,
        offer: Option<&str>,
        signal_callback: Option<PyObject>,
    ) -> PyResult<PyTube> {
        let runtime = get_runtime();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Determine if we're in server mode or client mode based on whether an offer was provided
        let is_server_mode = offer.is_none();
        
        // Do the full setup using the Rust implementation
        let result = runtime.block_on(async move {
            // Create the tube using the Rust implementation
            let mut registry = REGISTRY.write();
            
            // Create the tube first so we can get its ID
            let (tube, sdp) = match registry.create_tube(
                conversation_id,
                protocol_type,
                settings_json,
                is_server_mode,
                trickle_ice,
                callback_token,
                ksm_config,
            ).await {
                Ok((t, s)) => (t, s),
                Err(e) => return Err((None::<Arc<Tube>>, PyRuntimeError::new_err(format!("Failed to create tube: {}", e)), None::<Arc<Mutex<Receiver<SignalMessage>>>>)),
            };
            
            // Now register a signal channel for this tube
            let receiver = registry.register_signal_channel(&tube.id());
            let signal_receiver = Some(Arc::new(Mutex::new(receiver)));
            
            // If there's a signal callback, store it in the global map
            if let Some(callback) = signal_callback {
                let mut callbacks = PYTHON_CALLBACKS.write();
                callbacks.insert(tube.id(), callback);
            }
            
            // Handle the offer if provided (client mode)
            if let Some(offer_sdp) = offer {
                // Set the remote description using the registry
                if let Err(e) = registry.set_remote_description(&tube.id(), offer_sdp, false).await {
                    return Err((None::<Arc<Tube>>, PyRuntimeError::new_err(format!("Failed to set remote description: {}", e)), None::<Arc<Mutex<Receiver<SignalMessage>>>>));
                }
                
                // Create an answer
                let answer = match tube.create_answer().await {
                    Ok(a) => a,
                    Err(e) => return Err((None::<Arc<Tube>>, PyRuntimeError::new_err(format!("Failed to create answer: {}", e)), None::<Arc<Mutex<Receiver<SignalMessage>>>>)),
                };
                
                Ok((Some(tube), Some(answer), signal_receiver))
            } else {
                // Server mode - return the offer that was generated
                Ok((Some(tube), sdp, signal_receiver))
            }
        });
        
        // Handle the result
        match result {
            Ok((Some(tube), sdp, signal_receiver)) => Ok(PyTube { tube, sdp, signal_receiver }),
            Ok((None, _, _)) => Err(PyRuntimeError::new_err("Failed to create tube: no tube instance returned")),
            Err((_, py_err, _)) => Err(py_err),
        }
    }
    
    /// Get a tube object by ID
    fn get_tube(&self, tube_id: &str) -> PyResult<PyTube> {
        let registry = REGISTRY.read();
        if let Some(tube) = registry.get_by_tube_id(tube_id) {
            // Create a signal receiver for existing tube
            let signal_receiver = {
                drop(registry); // Release read lock before writing
                let mut registry = REGISTRY.write();
                let receiver = registry.register_signal_channel(tube_id);
                Some(Arc::new(Mutex::new(receiver)))
            };
            
            Ok(PyTube { 
                tube, 
                sdp: None, // No SDP available for existing tubes
                signal_receiver,
            })
        } else {
            Err(PyRuntimeError::new_err(format!("Tube not found: {}", tube_id)))
        }
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
        server_mode = false,
        trickle_ice = false,
    ))]
    fn new_connection(
        &self,
        py: Python<'_>,
        tube_id: Option<&str>,
        connection_id: &str,
        settings: PyObject,
        server_mode: bool,
        trickle_ice: bool,
    ) -> PyResult<PyObject> {
        let runtime = get_runtime();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
        // Call the Rust implementation
        let result = runtime.block_on(async move {
            let mut registry = REGISTRY.write();
            registry.new_connection(tube_id, connection_id, settings_json, server_mode, trickle_ice).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to create connection: {}", e)))
        })?;
        
        // Return the tube ID as a Python object
        Python::with_gil(|py| Ok(PyString::new(py, &result).into()))
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
    ))]
    fn create_channel(
        &self,
        py: Python<'_>,
        connection_id: &str,
        tube_id: &str,
        settings: PyObject,
    ) -> PyResult<PyObject> {
        let mut registry = REGISTRY.write();
        
        // Convert PyObject to HashMap
        let settings_json = pyobj_to_json_hashmap(py, &settings)?;
        
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
    
    /// Register a protocol handler for a channel
    #[pyo3(signature = (
        tube_id,
        connection_id,
        protocol_type,
        settings = None,
    ))]
    fn register_protocol_handler(
        &self,
        py: Python<'_>,
        tube_id: &str,
        connection_id: &str,
        protocol_type: &str,
        settings: Option<PyObject>,
    ) -> PyResult<PyObject> {
        let runtime = get_runtime();
        
        // Convert PyObject to HashMap<String, serde_json::Value>
        let settings_json = if let Some(settings_obj) = settings {
            pyobj_to_json_hashmap(py, &settings_obj)?
        } else {
            HashMap::new()
        };
        
        runtime.block_on(async move {
            // Use the Rust implementation directly
            let registry = REGISTRY.read();
            registry.register_protocol_handler(tube_id, connection_id, protocol_type, settings_json).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to register protocol handler: {}", e)))
        })?;
        
        // Return success (None in Python)
        Ok(py.None())
    }
    
    /// Send a command to a protocol handler
    #[pyo3(signature = (
        tube_id,
        connection_id,
        protocol_type,
        command,
        params = None,
    ))]
    fn send_handler_command(
        &self,
        py: Python<'_>,
        tube_id: &str,
        connection_id: &str,
        protocol_type: &str,
        command: &str,
        params: Option<Vec<u8>>,
    ) -> PyResult<PyObject> {
        let runtime = get_runtime();
        let params_bytes = match params {
            Some(p) => bytes::Bytes::from(p),
            None => bytes::Bytes::new(),
        };
        
        runtime.block_on(async move {
            // Use the Rust implementation directly
            let registry = REGISTRY.read();
            registry.send_handler_command(tube_id, connection_id, protocol_type, command, params_bytes).await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to send command: {}", e)))
        })?;
        
        // Return success (None in Python)
        Ok(py.None())
    }
    
    /// Get the status of a protocol handler
    fn get_handler_status(
        &self,
        tube_id: &str,
        connection_id: &str,
        protocol_type: &str,
    ) -> PyResult<String> {
        // Use the Rust implementation directly
        let registry = REGISTRY.read();
        registry.get_handler_status(tube_id, connection_id, protocol_type)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to get handler status: {}", e)))
    }
    
    /// Unregister a signal callback for a tube
    fn unregister_signal_callback(&self, tube_id: &str) -> PyResult<()> {
        let mut callbacks = PYTHON_CALLBACKS.write();
        callbacks.remove(tube_id);
        Ok(())
    }
    
    /// Get a tube object by conversation ID
    fn get_tube_by_conversation_id(&self, conversation_id: &str) -> PyResult<PyTube> {
        let registry = REGISTRY.read();
        if let Some(tube) = registry.get_by_conversation_id(conversation_id) {
            // Create a signal receiver for existing tube
            let signal_receiver = {
                drop(registry); // Release read lock before writing
                let mut registry = REGISTRY.write();
                let receiver = registry.register_signal_channel(&tube.id());
                Some(Arc::new(Mutex::new(receiver)))
            };
            
            Ok(PyTube { 
                tube, 
                sdp: None, // No SDP available for existing tubes
                signal_receiver,
            })
        } else {
            Err(PyRuntimeError::new_err(format!("No tube found for conversation: {}", conversation_id)))
        }
    }
}