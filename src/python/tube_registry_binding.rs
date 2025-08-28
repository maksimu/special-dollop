use super::connectivity::test_webrtc_connectivity_internal;
use super::signal_handler::setup_signal_handler;
use super::utils::{pyobj_to_json_hashmap, safe_python_async_execute};
use crate::router_helpers::post_connection_state;
use crate::runtime::{get_runtime, shutdown_runtime_from_python};
use crate::tube_protocol::CloseConnectionReason;
use crate::tube_registry::REGISTRY;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, error, trace, warn};

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
#[pyclass]
pub struct PyTubeRegistry {
    /// Track whether explicit cleanup was called
    explicit_cleanup_called: AtomicBool,
}

#[pymethods]
impl PyTubeRegistry {
    #[new]
    fn new() -> Self {
        Self {
            explicit_cleanup_called: AtomicBool::new(false),
        }
    }

    // =============================================================================
    // CLEANUP AND LIFECYCLE MANAGEMENT
    // =============================================================================

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

    /// Shutdown the runtime - useful for clean process termination
    fn shutdown_runtime(&self, _py: Python<'_>) -> PyResult<()> {
        debug!(target: "python_bindings", "Python requested runtime shutdown");
        shutdown_runtime_from_python();
        Ok(())
    }

    // =============================================================================
    // TUBE CREATION AND WEBRTC OPERATIONS
    // =============================================================================

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

    // =============================================================================
    // REGISTRY MANAGEMENT AND QUERIES
    // =============================================================================

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
                Some(code) => CloseConnectionReason::from_code(code),
                None => CloseConnectionReason::Unknown,
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

                post_connection_state(
                    &ksm_config_from_python,
                    "open_connections", 
                    &tokens_json,
                    None,
                    &client_version,
                    None, // description
                    None, // recording_duration
                    None, // closure_reason
                    None, // ai_overall_risk_level
                    None, // ai_overall_summary
                ).await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to refresh connections on router: {e}")))
            })
        })
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

    // =============================================================================
    // RESOURCE MANAGEMENT
    // =============================================================================

    /// Configure resource management limits
    fn configure_resource_limits(
        &self,
        py: Python<'_>,
        config: &Bound<PyDict>,
    ) -> PyResult<PyObject> {
        use crate::resource_manager::RESOURCE_MANAGER;

        let mut limits = RESOURCE_MANAGER.get_limits();

        // Update limits based on provided configuration
        if let Ok(Some(v)) = config.get_item("max_concurrent_sockets") {
            limits.max_concurrent_sockets = v.extract::<usize>()?;
        }

        if let Ok(Some(v)) = config.get_item("max_interfaces_per_agent") {
            limits.max_interfaces_per_agent = v.extract::<usize>()?;
        }

        if let Ok(Some(v)) = config.get_item("max_concurrent_ice_agents") {
            limits.max_concurrent_ice_agents = v.extract::<usize>()?;
        }

        if let Ok(Some(v)) = config.get_item("max_turn_connections_per_server") {
            limits.max_turn_connections_per_server = v.extract::<usize>()?;
        }

        if let Ok(Some(v)) = config.get_item("socket_reuse_enabled") {
            limits.socket_reuse_enabled = v.extract::<bool>()?;
        }

        if let Ok(Some(v)) = config.get_item("ice_gather_timeout_seconds") {
            let seconds = v.extract::<u64>()?;
            limits.ice_gather_timeout = Duration::from_secs(seconds);
        }

        if let Ok(Some(v)) = config.get_item("enable_mdns_candidates") {
            limits.enable_mdns_candidates = v.extract::<bool>()?;
        }

        if let Ok(Some(v)) = config.get_item("ice_candidate_pool_size") {
            limits.ice_candidate_pool_size = Some(v.extract::<u8>()?);
        }

        if let Ok(Some(v)) = config.get_item("ice_transport_policy") {
            limits.ice_transport_policy = Some(v.extract::<String>()?);
        }

        if let Ok(Some(v)) = config.get_item("bundle_policy") {
            limits.bundle_policy = Some(v.extract::<String>()?);
        }

        if let Ok(Some(v)) = config.get_item("rtcp_mux_policy") {
            limits.rtcp_mux_policy = Some(v.extract::<String>()?);
        }

        if let Ok(Some(v)) = config.get_item("ice_connection_receiving_timeout_seconds") {
            let seconds = v.extract::<u64>()?;
            limits.ice_connection_receiving_timeout = Some(Duration::from_secs(seconds));
        }

        if let Ok(Some(v)) = config.get_item("ice_backup_candidate_pair_ping_interval_seconds") {
            let seconds = v.extract::<u64>()?;
            limits.ice_backup_candidate_pair_ping_interval = Some(Duration::from_secs(seconds));
        }

        // Update the global resource manager
        RESOURCE_MANAGER.update_limits(limits.clone());

        // Return current configuration as Python dict
        let dict = PyDict::new(py);

        dict.set_item("max_concurrent_sockets", limits.max_concurrent_sockets)?;
        dict.set_item("max_interfaces_per_agent", limits.max_interfaces_per_agent)?;
        dict.set_item(
            "max_concurrent_ice_agents",
            limits.max_concurrent_ice_agents,
        )?;
        dict.set_item(
            "max_turn_connections_per_server",
            limits.max_turn_connections_per_server,
        )?;
        dict.set_item("socket_reuse_enabled", limits.socket_reuse_enabled)?;
        dict.set_item(
            "ice_gather_timeout_seconds",
            limits.ice_gather_timeout.as_secs(),
        )?;
        dict.set_item("enable_mdns_candidates", limits.enable_mdns_candidates)?;
        dict.set_item("ice_candidate_pool_size", limits.ice_candidate_pool_size)?;
        dict.set_item("ice_transport_policy", limits.ice_transport_policy)?;
        dict.set_item("bundle_policy", limits.bundle_policy)?;
        dict.set_item("rtcp_mux_policy", limits.rtcp_mux_policy)?;
        dict.set_item(
            "ice_connection_receiving_timeout_seconds",
            limits.ice_connection_receiving_timeout.map(|d| d.as_secs()),
        )?;
        dict.set_item(
            "ice_backup_candidate_pair_ping_interval_seconds",
            limits
                .ice_backup_candidate_pair_ping_interval
                .map(|d| d.as_secs()),
        )?;

        Ok(dict.into())
    }

    /// Get current resource management status and statistics
    fn get_resource_status(&self, py: Python<'_>) -> PyResult<PyObject> {
        use crate::resource_manager::RESOURCE_MANAGER;

        let stats = RESOURCE_MANAGER.get_resource_status();
        let limits = RESOURCE_MANAGER.get_limits();

        let dict = PyDict::new(py);

        // Add statistics
        dict.set_item("sockets_allocated", stats.sockets_allocated)?;
        dict.set_item("sockets_released", stats.sockets_released)?;
        dict.set_item("ice_agents_created", stats.ice_agents_created)?;
        dict.set_item("ice_agents_destroyed", stats.ice_agents_destroyed)?;
        dict.set_item("turn_connections_pooled", stats.turn_connections_pooled)?;
        dict.set_item("turn_connections_reused", stats.turn_connections_reused)?;
        dict.set_item(
            "resource_exhaustion_errors",
            stats.resource_exhaustion_errors,
        )?;

        // Add last exhaustion timestamp if available
        if let Some(last_exhaustion) = stats.last_exhaustion {
            // Convert from Instant to SystemTime then to Unix timestamp
            let system_time = std::time::SystemTime::now() - last_exhaustion.elapsed();
            let timestamp = system_time
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| PyRuntimeError::new_err("Failed to convert timestamp"))?
                .as_secs_f64();
            dict.set_item("last_exhaustion_timestamp", timestamp)?;
        } else {
            dict.set_item("last_exhaustion_timestamp", py.None())?;
        }

        // Add current limits as nested dict
        let limits_dict = PyDict::new(py);

        limits_dict.set_item("max_concurrent_sockets", limits.max_concurrent_sockets)?;
        limits_dict.set_item("max_interfaces_per_agent", limits.max_interfaces_per_agent)?;
        limits_dict.set_item(
            "max_concurrent_ice_agents",
            limits.max_concurrent_ice_agents,
        )?;
        limits_dict.set_item(
            "max_turn_connections_per_server",
            limits.max_turn_connections_per_server,
        )?;
        limits_dict.set_item("socket_reuse_enabled", limits.socket_reuse_enabled)?;
        limits_dict.set_item(
            "ice_gather_timeout_seconds",
            limits.ice_gather_timeout.as_secs(),
        )?;
        limits_dict.set_item("enable_mdns_candidates", limits.enable_mdns_candidates)?;
        limits_dict.set_item("ice_candidate_pool_size", limits.ice_candidate_pool_size)?;
        limits_dict.set_item("ice_transport_policy", limits.ice_transport_policy)?;
        limits_dict.set_item("bundle_policy", limits.bundle_policy)?;
        limits_dict.set_item("rtcp_mux_policy", limits.rtcp_mux_policy)?;
        limits_dict.set_item(
            "ice_connection_receiving_timeout_seconds",
            limits.ice_connection_receiving_timeout.map(|d| d.as_secs()),
        )?;
        limits_dict.set_item(
            "ice_backup_candidate_pair_ping_interval_seconds",
            limits
                .ice_backup_candidate_pair_ping_interval
                .map(|d| d.as_secs()),
        )?;

        dict.set_item("current_limits", limits_dict)?;

        Ok(dict.into())
    }

    /// Clean up stale TURN connections from the connection pool
    fn cleanup_stale_turn_connections(&self, _py: Python<'_>) -> PyResult<()> {
        use crate::resource_manager::RESOURCE_MANAGER;
        RESOURCE_MANAGER.cleanup_stale_connections();
        Ok(())
    }

    // =============================================================================
    // CONNECTIVITY TESTING
    // =============================================================================

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
            let mut default_settings = HashMap::new();
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
                            let py_list = PyList::empty(py);
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
        let _detailed = detailed.unwrap_or(false);

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

    /// Restart ICE for a specific tube
    fn restart_ice(&self, py: Python<'_>, tube_id: &str) -> PyResult<()> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        py.allow_threads(|| {
            master_runtime.block_on(async move {
                let registry = REGISTRY.read().await;
                if let Some(tube) = registry.get_by_tube_id(&tube_id_owned) {
                    tube.restart_ice()
                        .await
                        .map_err(|e| PyRuntimeError::new_err(format!("ICE restart failed: {}", e)))
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {}",
                        tube_id_owned
                    )))
                }
            })
        })?;
        Ok(())
    }

    /// Get connection statistics for a specific tube
    fn get_connection_stats(&self, py: Python<'_>, tube_id: &str) -> PyResult<PyObject> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        let stats_result = py.allow_threads(|| {
            master_runtime.block_on(async move {
                let registry = REGISTRY.read().await;
                if let Some(tube) = registry.get_by_tube_id(&tube_id_owned) {
                    tube.get_connection_stats()
                        .await
                        .map_err(|e| format!("Failed to get stats: {}", e))
                } else {
                    Err(format!("Tube not found: {}", tube_id_owned))
                }
            })
        });

        match stats_result {
            Ok(stats) => {
                let dict = PyDict::new(py);
                dict.set_item("packet_loss_rate", stats.packet_loss_rate)?;
                dict.set_item("rtt_ms", stats.rtt_ms)?;
                dict.set_item("bytes_sent", stats.bytes_sent)?;
                dict.set_item("bytes_received", stats.bytes_received)?;
                Ok(dict.into())
            }
            Err(e) => Err(PyRuntimeError::new_err(e)),
        }
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
