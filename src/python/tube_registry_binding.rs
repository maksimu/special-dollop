use super::connectivity::test_webrtc_connectivity_internal;
use super::signal_handler::setup_signal_handler;
use super::utils::{pyobj_to_json_hashmap, safe_python_async_execute};
use crate::router_helpers::post_connection_state;
use crate::runtime::{get_runtime, shutdown_runtime_from_python};
use crate::tube_protocol::CloseConnectionReason;
use crate::tube_registry::REGISTRY;
use crate::unlikely;
use log::{debug, error, warn};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

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
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Get all tube IDs from DashMap
                let tube_ids = REGISTRY.all_tube_ids_sync();
                debug!(
                    "Starting explicit cleanup of all tubes (tube_count: {})",
                    tube_ids.len()
                );

                // Close all tubes via actor (coordinated)
                for tube_id in tube_ids {
                    if let Err(e) = REGISTRY
                        .close_tube(&tube_id, Some(CloseConnectionReason::Normal))
                        .await
                    {
                        error!(
                            "Failed to close tube during cleanup: {} (tube_id: {})",
                            e, tube_id
                        );
                    }
                }

                debug!("Registry cleanup complete (actor handles all cleanup)");
            })
        });

        // Now shutdown the runtime - this ensures all async tasks are properly terminated
        debug!("Shutting down runtime after tube cleanup");
        crate::runtime::shutdown_runtime_from_python();

        Ok(())
    }

    /// Clean up specific tubes by ID
    fn cleanup_tubes(&self, py: Python<'_>, tube_ids: Vec<String>) -> PyResult<()> {
        let master_runtime = get_runtime();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                debug!(
                    "Starting cleanup of specific tubes (tube_count: {})",
                    tube_ids.len()
                );

                // LOCK-FREE: Close tubes via actor
                for tube_id in tube_ids {
                    if let Err(e) = REGISTRY
                        .close_tube(&tube_id, Some(CloseConnectionReason::Normal))
                        .await
                    {
                        error!(
                            "Failed to close tube during selective cleanup: {} (tube_id: {})",
                            e, tube_id
                        );
                    }
                }
                Ok(())
            })
        })
    }

    /// Python destructor - safety net cleanup
    fn __del__(&self, py: Python<'_>) {
        if !self.explicit_cleanup_called.load(Ordering::SeqCst) {
            warn!("PyTubeRegistry.__del__ called without explicit cleanup! Consider using cleanup_all() explicitly or using a context manager.");

            // Attempt force cleanup - ignore errors since we're in destructor
            let _ = self.do_force_cleanup(py);
        } else {
            debug!("PyTubeRegistry.__del__ called after explicit cleanup - OK");
        }
    }

    /// Internal Force cleanup for __del__ - more permissive error handling
    fn do_force_cleanup(&self, py: Python<'_>) -> PyResult<()> {
        let master_runtime = get_runtime();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Get all tube IDs
                let tube_ids = REGISTRY.all_tube_ids_sync();
                warn!(
                    "Force cleanup in __del__ - {} tubes to close",
                    tube_ids.len()
                );

                // Close all tubes via actor - ignore individual errors
                for tube_id in tube_ids {
                    if let Err(e) = REGISTRY
                        .close_tube(&tube_id, Some(CloseConnectionReason::Unknown))
                        .await
                    {
                        error!(
                            "Failed to close tube during force cleanup: {} (tube_id: {})",
                            e, tube_id
                        );
                    }
                }

                debug!("Force cleanup complete (actor handles all cleanup)");
            })
        });

        // Force shutdown the runtime - this is the safety net for thread cleanup
        warn!("Force shutting down runtime in __del__");
        crate::runtime::shutdown_runtime_from_python();

        Ok(())
    }

    /// Shutdown the runtime - useful for clean process termination
    fn shutdown_runtime(&self, _py: Python<'_>) -> PyResult<()> {
        debug!("Python requested runtime shutdown");
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
        settings: Py<PyAny>,
        trickle_ice: bool,
        callback_token: &str,
        krelay_server: &str,
        client_version: Option<&str>,
        ksm_config: Option<&str>,
        offer: Option<&str>,
        signal_callback: Option<Py<PyAny>>,
        tube_id: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
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
        let creation_result_map = Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!(
                        "PyBind: Creating tube via actor (conversation_id: {})",
                        conversation_id
                    );
                }

                // Build CreateTubeRequest for actor
                let req = crate::tube_registry::CreateTubeRequest {
                    conversation_id: conversation_id_owned,
                    settings: settings_json,
                    initial_offer_sdp: offer_string_owned,
                    trickle_ice,
                    callback_token: callback_token_owned,
                    krelay_server: krelay_server_owned,
                    ksm_config: ksm_config_owned,
                    client_version: client_version_owned,
                    signal_sender: signal_sender_rust,
                    tube_id: tube_id_owned,
                };

                // LOCK-FREE + NON-BLOCKING: Send via actor
                REGISTRY.create_tube(req).await.map_err(|e| {
                    error!(
                        "PyBind: Tube creation via actor failed: {e} (conversation_id: {})",
                        conversation_id
                    );
                    PyRuntimeError::new_err(format!("Failed to create tube: {e}"))
                })
            })
        })?; // Propagate errors from block_on

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!("PyBind: TubeRegistry::create_tube call complete. Result map has {} keys. (conversation_id: {})", creation_result_map.len(), conversation_id);
        }

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

    /// Create an offer for a tube (returns base64-encoded SDP)
    fn create_offer(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Get tube from DashMap
                let tube = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube {
                    let offer = tube.create_offer().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create offer: {e}"))
                    })?;

                    // Encode to base64 for Python/Rust API boundary
                    use base64::prelude::*;
                    Ok(BASE64_STANDARD.encode(offer))
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {tube_id_owned}"
                    )))
                }
            })
        })
    }

    /// Create an answer for a tube (returns base64-encoded SDP)
    fn create_answer(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Get tube from DashMap
                let tube = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube {
                    let answer = tube.create_answer().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create answer: {e}"))
                    })?;

                    // Encode to base64 for Python/Rust API boundary
                    use base64::prelude::*;
                    Ok(BASE64_STANDARD.encode(answer))
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
        let tube_id_owned = tube_id.to_string();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                let tube = REGISTRY.get_by_tube_id(&tube_id_owned).ok_or_else(|| {
                    PyRuntimeError::new_err(format!("Tube not found: {}", tube_id_owned))
                })?;

                // Decode SDP from base64 (API contract: all SDP is base64-encoded over Python/Rust boundary)
                use base64::prelude::*;
                let sdp_bytes = BASE64_STANDARD.decode(&sdp).map_err(|e| {
                    PyRuntimeError::new_err(format!("Failed to decode SDP from base64: {}", e))
                })?;
                let sdp_decoded = String::from_utf8(sdp_bytes).map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "Failed to convert decoded SDP to String: {}",
                        e
                    ))
                })?;

                // Set the remote description
                tube.set_remote_description(sdp_decoded, is_answer)
                    .await
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to set remote description: {}", e))
                    })?;

                // If this is an offer, create an answer
                if !is_answer {
                    let answer = tube.create_answer().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create answer: {}", e))
                    })?;

                    return Ok(Some(BASE64_STANDARD.encode(answer))); // Encode the answer to base64
                }

                Ok(None)
            })
        })
    }

    /// Add an ICE candidate to a tube
    fn add_ice_candidate(&self, _py: Python<'_>, tube_id: &str, candidate: String) -> PyResult<()> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        let candidate_owned = candidate;

        master_runtime.spawn(async move {
            // LOCK-FREE: Direct call to registry (no locks!)
            let result = REGISTRY
                .add_external_ice_candidate(&tube_id_owned, &candidate_owned)
                .await;

            if let Err(e) = result {
                warn!(
                    "Error adding ICE candidate for tube {}: {}",
                    tube_id_owned, e
                );
            }
        });

        Ok(())
    }

    /// Get connection state of a tube
    fn get_connection_state(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let master_runtime = get_runtime();
        let tube_id_owned = tube_id.to_string();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                REGISTRY
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
    fn has_active_tubes(&self, _py: Python<'_>) -> bool {
        // LOCK-FREE: Direct call to registry (no locks!)
        REGISTRY.has_tubes()
    }

    /// Get count of active tubes
    fn active_tube_count(&self, _py: Python<'_>) -> usize {
        // LOCK-FREE: Direct call to registry (no locks!)
        REGISTRY.tube_count()
    }

    /// Set server mode in the registry
    fn set_server_mode(&self, py: Python<'_>, server_mode: bool) -> PyResult<()> {
        let master_runtime = get_runtime();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Direct call to registry via actor (no locks!)
                REGISTRY.set_server_mode(server_mode).await.map_err(|e| {
                    error!("Failed to set server mode: {}", e);
                    PyRuntimeError::new_err(format!("Failed to set server mode: {}", e))
                })
            })
        })
    }

    /// Check if the registry is in server mode
    fn is_server_mode(&self, py: Python<'_>) -> PyResult<bool> {
        let master_runtime = get_runtime();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // Query actor for authoritative server mode state
                REGISTRY
                    .is_server_mode()
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("Failed to get server mode: {e}")))
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
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Direct call to registry via actor (no locks!)
                REGISTRY
                    .associate_conversation(&tube_id_owned, &connection_id_owned)
                    .await
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to associate conversation: {e}"))
                    })
            })
        })
    }

    /// find if a tube already exists
    fn tube_found(&self, _py: Python<'_>, tube_id: &str) -> bool {
        // LOCK-FREE: Direct call to registry (no locks!)
        REGISTRY.get_by_tube_id(tube_id).is_some()
    }

    /// Get all tube IDs
    fn all_tube_ids(&self, _py: Python<'_>) -> Vec<String> {
        // LOCK-FREE: Direct call to registry (no locks!)
        REGISTRY.all_tube_ids_sync()
    }

    /// Get all Conversation IDs by Tube ID
    fn get_conversation_ids_by_tube_id(&self, _py: Python<'_>, tube_id: &str) -> Vec<String> {
        // LOCK-FREE: Direct call to registry (no locks!)
        REGISTRY.conversation_ids_by_tube_id(tube_id)
    }

    /// find tube by connection ID
    fn tube_id_from_connection_id(&self, _py: Python<'_>, connection_id: &str) -> Option<String> {
        // LOCK-FREE: Direct call to registry (no locks!)
        // Get the tube_id from the conversation mapping
        REGISTRY
            .conversations()
            .get(connection_id)
            .map(|entry| entry.value().clone())
    }

    /// Find tubes by partial match of tube ID or conversation ID
    fn find_tubes(&self, _py: Python<'_>, search_term: &str) -> PyResult<Vec<String>> {
        // LOCK-FREE: Direct call to registry (no locks!)
        Ok(REGISTRY.find_tubes(search_term))
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
            // LOCK-FREE: Direct call to registry (no locks!)
            let tube_arc = REGISTRY
                .get_by_conversation_id(&connection_id_owned)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "Rust: No tube found for connection ID: {connection_id_owned}"
                    ))
                })?;

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
            // LOCK-FREE: Direct call to registry via actor (no locks!)
            let close_reason = reason.map(CloseConnectionReason::from_code);
            REGISTRY
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
        _py: Python<'_>,
        conversation_id: &str,
    ) -> PyResult<String> {
        // LOCK-FREE: Direct call to registry (no locks!)
        if let Some(tube) = REGISTRY.get_by_conversation_id(conversation_id) {
            Ok(tube.id().to_string())
        } else {
            Err(PyRuntimeError::new_err(format!(
                "No tube found for conversation: {conversation_id}"
            )))
        }
    }

    /// Refresh connections on router - collect all callback tokens and send to router
    fn refresh_connections(
        &self,
        py: Python<'_>,
        ksm_config_from_python: String,
        client_version: String,
    ) -> PyResult<()> {
        let master_runtime = get_runtime();
        Python::detach(py, || {
            master_runtime.clone().block_on(async move {
                // LOCK-FREE: Get all tube Arcs from DashMap (no locks!)
                let tube_arcs: Vec<Arc<crate::Tube>> = REGISTRY.tubes()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect();

                // Now collect callback tokens WITHOUT holding registry lock
                let mut callback_tokens = Vec::new();
                for tube_arc in tube_arcs {
                    // Get callback tokens from all channels in this tube (can await safely now)
                    let tube_channel_tokens = tube_arc.get_callback_tokens().await;
                    callback_tokens.extend(tube_channel_tokens);
                }

                // The post_connection_state function handles TEST_MODE_KSM_CONFIG internally.
                // It will also error out if ksm_config_from_python is empty and not a test string.
                debug!(
                    "Preparing to send refresh_connections (open_connections) connection count {} with KSM config from Python. python_direct_arg", callback_tokens.len()
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
    fn print_tubes_with_connections(&self, _py: Python<'_>) -> PyResult<()> {
        // LOCK-FREE: Direct calls to registry (no locks!)
        debug!("=== Tube Registry Status ===");
        debug!("Total tubes: {}", REGISTRY.tube_count());
        debug!("Has tubes: {}\n", REGISTRY.has_tubes());

        if !REGISTRY.has_tubes() {
            debug!("No active tubes.");
            return Ok(());
        }

        let all_tube_ids = REGISTRY.all_tube_ids_sync();
        for tube_id in all_tube_ids {
            debug!("Tube ID: {tube_id}");

            // Get conversation IDs for this tube
            let conversation_ids = REGISTRY.conversation_ids_by_tube_id(&tube_id);
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

            // Check if there's a signal channel for this tube (via Tube.signal_sender)
            let has_signal_channel = REGISTRY.get_signal_sender(&tube_id).is_some();
            debug!(
                "  └─ Signal channel: {}\n",
                if has_signal_channel { "Active" } else { "None" }
            );
        }

        // Show reverse mapping summary
        debug!("=== Conversation Mappings ===");
        let conversation_count = REGISTRY.conversations().len();
        debug!("Total mappings: {}", conversation_count);
        if conversation_count > 0 {
            for entry in REGISTRY.conversations().iter() {
                debug!("  {} → {}", entry.key(), entry.value());
            }
        }
        debug!("============================");

        Ok(())
    }

    // =============================================================================
    // BACKPRESSURE AND CAPACITY MANAGEMENT
    // =============================================================================

    /// Check registry capacity for backpressure coordination
    /// Returns metrics that Python can use to decide whether to create more tubes
    fn check_capacity(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let master_runtime = get_runtime();

        let metrics_result = master_runtime.block_on(async move {
            REGISTRY.get_metrics().await.map_err(|e| {
                PyRuntimeError::new_err(format!("Failed to get registry metrics: {}", e))
            })
        })?;

        // Convert metrics to Python dict
        let dict = PyDict::new(py);
        dict.set_item(
            "can_accept_more",
            metrics_result.active_creates < metrics_result.max_concurrent,
        )?;
        dict.set_item("active_creates", metrics_result.active_creates)?;
        dict.set_item("max_concurrent", metrics_result.max_concurrent)?;
        dict.set_item("queue_depth", metrics_result.command_queue_depth)?;
        dict.set_item(
            "avg_create_time_ms",
            metrics_result.avg_create_time.as_millis() as u64,
        )?;
        dict.set_item("total_creates", metrics_result.total_creates)?;
        dict.set_item("total_failures", metrics_result.total_failures)?;

        // Calculate suggested backoff delay based on load
        let load_pct = if metrics_result.max_concurrent > 0 {
            (metrics_result.active_creates * 100) / metrics_result.max_concurrent
        } else {
            0
        };

        let suggested_delay_ms: u64 = match load_pct {
            0..=50 => 0,     // No delay - plenty of capacity
            51..=75 => 100,  // Light delay - getting busy
            76..=90 => 500,  // Moderate delay - high load
            91..=99 => 2000, // Heavy delay - near capacity
            _ => 5000,       // Max delay - at/over capacity
        };

        dict.set_item("load_percentage", load_pct)?;
        dict.set_item("suggested_delay_ms", suggested_delay_ms)?;

        Ok(dict.into())
    }

    // =============================================================================
    // RESOURCE MANAGEMENT
    // =============================================================================

    /// Configure resource management limits
    fn configure_resource_limits(
        &self,
        py: Python<'_>,
        config: &Bound<PyDict>,
    ) -> PyResult<Py<PyAny>> {
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
    fn get_resource_status(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
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
        settings: Option<Py<PyAny>>,
        timeout_seconds: Option<u64>,
        ksm_config: Option<&str>,
        client_version: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
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

        let result = Python::detach(py, || {
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
        results: Py<PyAny>,
        detailed: Option<bool>,
    ) -> PyResult<String> {
        let _detailed = detailed.unwrap_or(false);

        // Convert Python results back to JSON for processing
        let results_dict = results.extract::<HashMap<String, Py<PyAny>>>(py)?;
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
            if let Ok(settings) = settings_obj.extract::<HashMap<String, Py<PyAny>>>(py) {
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
                if let Ok(test_result) = test_result_obj.extract::<HashMap<String, Py<PyAny>>>(py) {
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
            if let Ok(overall) = overall_obj.extract::<HashMap<String, Py<PyAny>>>(py) {
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

    /// Restart ICE for a specific tube and return the restart offer SDP (base64-encoded)
    fn restart_ice(&self, py: Python<'_>, tube_id: &str) -> PyResult<String> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        Python::detach(py, || {
            master_runtime.block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                let tube_arc = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube_arc {
                    let offer = tube.restart_ice().await.map_err(|e| {
                        PyRuntimeError::new_err(format!("ICE restart failed: {}", e))
                    })?;

                    // Encode to base64 for Python/Rust API boundary
                    use base64::prelude::*;
                    Ok(BASE64_STANDARD.encode(offer))
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {}",
                        tube_id_owned
                    )))
                }
            })
        })
    }

    /// Export detailed connection leg metrics for visualization
    fn export_connection_leg_metrics(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let export_result = Python::detach(py, || -> Result<String, String> {
            let rt = get_runtime();
            rt.block_on(async {
                let snapshot = crate::metrics::METRICS_COLLECTOR.create_snapshot();
                let mut leg_metrics = HashMap::new();

                for (conversation_id, metrics) in snapshot.connections.iter() {
                    let mut connection_data = HashMap::new();

                    // Basic connection info
                    connection_data.insert(
                        "tube_id".to_string(),
                        serde_json::Value::String(metrics.tube_id.clone()),
                    );
                    connection_data.insert(
                        "established_at".to_string(),
                        serde_json::Value::String(metrics.established_at.to_rfc3339()),
                    );

                    // Connection leg latencies
                    let legs = &metrics.webrtc_metrics.connection_legs;
                    if let Some(client_krelay) = legs.client_to_krelay_latency_ms {
                        if let Some(num) = serde_json::Number::from_f64(client_krelay) {
                            connection_data.insert(
                                "client_to_krelay_latency_ms".to_string(),
                                serde_json::Value::Number(num),
                            );
                        }
                    }
                    if let Some(krelay_gateway) = legs.krelay_to_gateway_latency_ms {
                        if let Some(num) = serde_json::Number::from_f64(krelay_gateway) {
                            connection_data.insert(
                                "krelay_to_gateway_latency_ms".to_string(),
                                serde_json::Value::Number(num),
                            );
                        }
                    }
                    if let Some(end_to_end) = legs.end_to_end_latency_ms {
                        if let Some(num) = serde_json::Number::from_f64(end_to_end) {
                            connection_data.insert(
                                "end_to_end_latency_ms".to_string(),
                                serde_json::Value::Number(num),
                            );
                        }
                    }

                    // ICE/TURN performance metrics
                    let ice_stats = &metrics.webrtc_metrics.rtc_stats.ice_stats;
                    connection_data.insert(
                        "ice_candidates_total".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(
                            ice_stats.total_candidates,
                        )),
                    );
                    connection_data.insert(
                        "ice_candidates_host".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(
                            ice_stats.host_candidates,
                        )),
                    );
                    connection_data.insert(
                        "ice_candidates_srflx".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(
                            ice_stats.srflx_candidates,
                        )),
                    );
                    connection_data.insert(
                        "ice_candidates_relay".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(
                            ice_stats.relay_candidates,
                        )),
                    );
                    if let Some(num) =
                        serde_json::Number::from_f64(ice_stats.turn_allocation_success_rate)
                    {
                        connection_data.insert(
                            "turn_allocation_success_rate".to_string(),
                            serde_json::Value::Number(num),
                        );
                    }

                    if let Some(gathering_time) = ice_stats.gathering_complete_time_ms {
                        if let Some(num) = serde_json::Number::from_f64(gathering_time) {
                            connection_data.insert(
                                "ice_gathering_duration_ms".to_string(),
                                serde_json::Value::Number(num),
                            );
                        }
                    }

                    // Current performance metrics
                    if let Some(rtt) = metrics.webrtc_metrics.rtc_stats.rtt_ms {
                        if let Some(num) = serde_json::Number::from_f64(rtt) {
                            connection_data.insert(
                                "current_rtt_ms".to_string(),
                                serde_json::Value::Number(num),
                            );
                        }
                    }
                    if let Some(num) = serde_json::Number::from_f64(
                        metrics.webrtc_metrics.rtc_stats.packet_loss_rate,
                    ) {
                        connection_data.insert(
                            "packet_loss_rate".to_string(),
                            serde_json::Value::Number(num),
                        );
                    }
                    if let Some(num) = serde_json::Number::from_f64(
                        metrics.webrtc_metrics.rtc_stats.current_bitrate,
                    ) {
                        connection_data.insert(
                            "current_bitrate_bps".to_string(),
                            serde_json::Value::Number(num),
                        );
                    }

                    leg_metrics.insert(conversation_id.clone(), connection_data);
                }

                // System-wide aggregated metrics
                let mut system_metrics = HashMap::new();
                system_metrics.insert(
                    "timestamp".to_string(),
                    serde_json::Value::String(snapshot.aggregated.timestamp.to_rfc3339()),
                );
                system_metrics.insert(
                    "active_connections".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(
                        snapshot.aggregated.active_connections,
                    )),
                );
                system_metrics.insert(
                    "active_tubes".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(
                        snapshot.aggregated.active_tubes,
                    )),
                );
                system_metrics.insert(
                    "avg_system_rtt_ms".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(
                        snapshot.aggregated.avg_system_rtt.as_millis() as u64,
                    )),
                );
                if let Some(num) = serde_json::Number::from_f64(snapshot.aggregated.avg_packet_loss)
                {
                    system_metrics.insert(
                        "avg_packet_loss".to_string(),
                        serde_json::Value::Number(num),
                    );
                }
                if let Some(num) = serde_json::Number::from_f64(snapshot.aggregated.total_bandwidth)
                {
                    system_metrics.insert(
                        "total_bandwidth_bps".to_string(),
                        serde_json::Value::Number(num),
                    );
                }

                let mut export_data = HashMap::new();
                export_data.insert(
                    "connections".to_string(),
                    serde_json::Value::Object(serde_json::Map::from_iter(
                        leg_metrics.into_iter().map(|(k, v)| {
                            (k, serde_json::Value::Object(serde_json::Map::from_iter(v)))
                        }),
                    )),
                );
                export_data.insert(
                    "system".to_string(),
                    serde_json::Value::Object(serde_json::Map::from_iter(system_metrics)),
                );

                serde_json::to_string_pretty(&export_data)
                    .map_err(|e| format!("JSON serialization failed: {}", e))
            })
        })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to execute metrics export: {}", e)))?;

        Ok(pyo3::types::PyString::new(py, &export_result).into())
    }

    /// Get connection statistics for a specific tube
    fn get_connection_stats(&self, py: Python<'_>, tube_id: &str) -> PyResult<Py<PyAny>> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        let stats_result = Python::detach(py, || {
            master_runtime.block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                let tube_arc = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube_arc {
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

    /// Post connection state to the router
    #[pyo3(signature = (
        ksm_config,
        connection_state,
        token,
        is_terminated = None,
        client_version = "unknown",
        recording_duration = None,
        closure_reason = None,
        ai_overall_risk_level = None,
        ai_overall_summary = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn post_connection_state(
        &self,
        py: Python<'_>,
        ksm_config: &str,
        connection_state: &str,
        token: Py<PyAny>,
        is_terminated: Option<bool>,
        client_version: &str,
        recording_duration: Option<u64>,
        closure_reason: Option<u32>,
        ai_overall_risk_level: Option<String>,
        ai_overall_summary: Option<String>,
    ) -> PyResult<()> {
        let master_runtime = get_runtime();
        let ksm_config_owned = ksm_config.to_string();
        let connection_state_owned = connection_state.to_string();
        let client_version_owned = client_version.to_string();

        // Convert Python token to JSON Value
        let token_json = if let Ok(token_str) = token.extract::<String>(py) {
            serde_json::Value::String(token_str)
        } else if let Ok(token_list) = token.extract::<Vec<String>>(py) {
            serde_json::Value::Array(
                token_list
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            )
        } else {
            return Err(PyRuntimeError::new_err(
                "Token must be a string or list of strings",
            ));
        };

        Python::detach(py, || {
            master_runtime.block_on(async move {
                post_connection_state(
                    &ksm_config_owned,
                    &connection_state_owned,
                    &token_json,
                    is_terminated,
                    &client_version_owned,
                    recording_duration,
                    closure_reason,
                    ai_overall_risk_level,
                    ai_overall_summary,
                )
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!("Failed to post connection state: {}", e))
                })
            })
        })
    }

    // =============================================================================
    // METRICS AND PERFORMANCE MONITORING
    // =============================================================================

    /// Get live performance statistics for a specific connection
    fn get_live_stats(&self, py: Python<'_>, conversation_id: &str) -> PyResult<Option<Py<PyAny>>> {
        let conversation_id_owned = conversation_id.to_string();

        // Get metrics from the metrics collector (non-blocking)
        if let Some(metrics) =
            crate::metrics::METRICS_COLLECTOR.get_live_stats(&conversation_id_owned)
        {
            // Convert metrics to Python dictionary
            let dict = PyDict::new(py);

            // Connection identifiers
            dict.set_item("conversation_id", &metrics.conversation_id)?;
            dict.set_item("tube_id", &metrics.tube_id)?;
            dict.set_item("established_at", metrics.established_at.to_rfc3339())?;

            // Message flow metrics
            dict.set_item("messages_sent", metrics.messages_sent)?;
            dict.set_item("messages_received", metrics.messages_received)?;
            dict.set_item("total_bytes_sent", metrics.total_bytes_sent)?;
            dict.set_item("total_bytes_received", metrics.total_bytes_received)?;

            // Latency metrics (convert to milliseconds)
            dict.set_item("avg_rtt_ms", metrics.avg_rtt.as_millis() as f64)?;
            dict.set_item("p95_latency_ms", metrics.p95_latency.as_millis() as f64)?;
            dict.set_item("p99_latency_ms", metrics.p99_latency.as_millis() as f64)?;

            if let Some(current_latency) = metrics.current_message_latency {
                dict.set_item(
                    "current_message_latency_ms",
                    current_latency.as_millis() as f64,
                )?;
            }

            // WebRTC metrics
            let webrtc_dict = PyDict::new(py);
            if let Some(rtt_ms) = metrics.webrtc_metrics.rtc_stats.rtt_ms {
                webrtc_dict.set_item("rtt_ms", rtt_ms)?;
            }
            webrtc_dict.set_item("jitter_ms", metrics.webrtc_metrics.rtc_stats.jitter_ms)?;
            webrtc_dict.set_item(
                "packet_loss_rate",
                metrics.webrtc_metrics.rtc_stats.packet_loss_rate,
            )?;
            webrtc_dict.set_item(
                "current_bitrate",
                metrics.webrtc_metrics.rtc_stats.current_bitrate,
            )?;
            webrtc_dict.set_item(
                "ice_connection_state",
                &metrics.webrtc_metrics.rtc_stats.ice_connection_state,
            )?;
            webrtc_dict.set_item("dtls_ready", metrics.webrtc_metrics.rtc_stats.dtls_ready)?;
            webrtc_dict.set_item(
                "active_data_channels",
                metrics.webrtc_metrics.rtc_stats.active_data_channels,
            )?;

            // ICE statistics
            let ice_dict = PyDict::new(py);
            ice_dict.set_item(
                "total_candidates",
                metrics.webrtc_metrics.rtc_stats.ice_stats.total_candidates,
            )?;
            ice_dict.set_item(
                "host_candidates",
                metrics.webrtc_metrics.rtc_stats.ice_stats.host_candidates,
            )?;
            ice_dict.set_item(
                "srflx_candidates",
                metrics.webrtc_metrics.rtc_stats.ice_stats.srflx_candidates,
            )?;
            ice_dict.set_item(
                "relay_candidates",
                metrics.webrtc_metrics.rtc_stats.ice_stats.relay_candidates,
            )?;

            if let Some(first_candidate_time) = metrics
                .webrtc_metrics
                .rtc_stats
                .ice_stats
                .first_candidate_time_ms
            {
                ice_dict.set_item("first_candidate_time_ms", first_candidate_time)?;
            }
            if let Some(gathering_time) = metrics
                .webrtc_metrics
                .rtc_stats
                .ice_stats
                .gathering_complete_time_ms
            {
                ice_dict.set_item("gathering_complete_time_ms", gathering_time)?;
            }
            ice_dict.set_item(
                "turn_allocation_success_rate",
                metrics
                    .webrtc_metrics
                    .rtc_stats
                    .ice_stats
                    .turn_allocation_success_rate,
            )?;
            if let Some(turn_time) = metrics
                .webrtc_metrics
                .rtc_stats
                .ice_stats
                .turn_allocation_time_ms
            {
                ice_dict.set_item("turn_allocation_time_ms", turn_time)?;
            }

            if !metrics
                .webrtc_metrics
                .rtc_stats
                .ice_stats
                .stun_response_times
                .is_empty()
            {
                ice_dict.set_item(
                    "stun_response_times",
                    metrics
                        .webrtc_metrics
                        .rtc_stats
                        .ice_stats
                        .stun_response_times
                        .clone(),
                )?;
            }
            webrtc_dict.set_item("ice_stats", ice_dict)?;

            // Connection leg performance
            let legs_dict = PyDict::new(py);
            if let Some(client_krelay) = metrics
                .webrtc_metrics
                .connection_legs
                .client_to_krelay_latency_ms
            {
                legs_dict.set_item("client_to_krelay_latency_ms", client_krelay)?;
            }
            if let Some(krelay_gateway) = metrics
                .webrtc_metrics
                .connection_legs
                .krelay_to_gateway_latency_ms
            {
                legs_dict.set_item("krelay_to_gateway_latency_ms", krelay_gateway)?;
            }
            if let Some(end_to_end) = metrics.webrtc_metrics.connection_legs.end_to_end_latency_ms {
                legs_dict.set_item("end_to_end_latency_ms", end_to_end)?;
            }
            if let Some(stun_time) = metrics.webrtc_metrics.connection_legs.stun_response_time_ms {
                legs_dict.set_item("stun_response_time_ms", stun_time)?;
            }
            if let Some(turn_time) = metrics
                .webrtc_metrics
                .connection_legs
                .turn_allocation_latency_ms
            {
                legs_dict.set_item("turn_allocation_latency_ms", turn_time)?;
            }
            if let Some(ice_time) = metrics
                .webrtc_metrics
                .connection_legs
                .ice_connection_establishment_ms
            {
                legs_dict.set_item("ice_connection_establishment_ms", ice_time)?;
            }
            webrtc_dict.set_item("connection_legs", legs_dict)?;

            dict.set_item("webrtc_stats", webrtc_dict)?;

            // Connection quality and health
            let quality_str = match metrics.connection_quality {
                crate::metrics::ConnectionQuality::Excellent => "excellent",
                crate::metrics::ConnectionQuality::Good => "good",
                crate::metrics::ConnectionQuality::Fair => "fair",
                crate::metrics::ConnectionQuality::Poor => "poor",
            };
            dict.set_item("connection_quality", quality_str)?;
            dict.set_item("active_alert_count", metrics.active_alert_count)?;

            // Performance metrics
            dict.set_item("error_rate", metrics.error_rate)?;
            dict.set_item("total_errors", metrics.total_errors)?;
            dict.set_item("retry_count", metrics.retry_count)?;
            dict.set_item("message_throughput", metrics.message_throughput())?;
            dict.set_item("bandwidth_utilization", metrics.bandwidth_utilization())?;

            Ok(Some(dict.into()))
        } else {
            Ok(None)
        }
    }

    /// Get connection health summary for a specific tube
    fn get_connection_health(&self, py: Python<'_>, tube_id: &str) -> PyResult<Option<Py<PyAny>>> {
        let tube_id_owned = tube_id.to_string();

        if let Some(metrics) =
            crate::metrics::METRICS_COLLECTOR.get_connection_health(&tube_id_owned)
        {
            // Create simplified health summary
            let dict = PyDict::new(py);

            dict.set_item("tube_id", &metrics.tube_id)?;
            dict.set_item("conversation_id", &metrics.conversation_id)?;

            let quality_str = match metrics.connection_quality {
                crate::metrics::ConnectionQuality::Excellent => "excellent",
                crate::metrics::ConnectionQuality::Good => "good",
                crate::metrics::ConnectionQuality::Fair => "fair",
                crate::metrics::ConnectionQuality::Poor => "poor",
            };
            dict.set_item("quality", quality_str)?;

            // Key health indicators
            if let Some(rtt_ms) = metrics.webrtc_metrics.rtc_stats.rtt_ms {
                dict.set_item("rtt_ms", rtt_ms)?;
            }
            dict.set_item(
                "packet_loss_rate",
                metrics.webrtc_metrics.rtc_stats.packet_loss_rate,
            )?;
            dict.set_item("active_alerts", metrics.active_alert_count)?;
            dict.set_item("error_rate", metrics.error_rate)?;
            dict.set_item(
                "ice_connection_state",
                &metrics.webrtc_metrics.rtc_stats.ice_connection_state,
            )?;

            Ok(Some(dict.into()))
        } else {
            Ok(None)
        }
    }

    /// Export all metrics data as JSON string for dashboard consumption
    fn export_metrics_json(&self, _py: Python<'_>) -> PyResult<String> {
        crate::metrics::METRICS_COLLECTOR
            .export_metrics_json()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to export metrics: {}", e)))
    }

    /// Get aggregated system-wide metrics
    fn get_aggregated_metrics(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let aggregated = crate::metrics::METRICS_COLLECTOR.get_aggregated_metrics();

        let dict = PyDict::new(py);

        dict.set_item("timestamp", aggregated.timestamp.to_rfc3339())?;
        dict.set_item("active_connections", aggregated.active_connections)?;
        dict.set_item("active_tubes", aggregated.active_tubes)?;

        // System averages
        dict.set_item(
            "avg_system_rtt_ms",
            aggregated.avg_system_rtt.as_millis() as f64,
        )?;
        dict.set_item("avg_packet_loss", aggregated.avg_packet_loss)?;
        dict.set_item(
            "total_message_throughput",
            aggregated.total_message_throughput,
        )?;
        dict.set_item("total_bandwidth", aggregated.total_bandwidth)?;

        // Quality distribution
        dict.set_item("excellent_connections", aggregated.excellent_connections)?;
        dict.set_item("good_connections", aggregated.good_connections)?;
        dict.set_item("fair_connections", aggregated.fair_connections)?;
        dict.set_item("poor_connections", aggregated.poor_connections)?;

        // Alert summary
        dict.set_item("total_alerts", aggregated.total_alerts)?;
        dict.set_item("critical_alerts", aggregated.critical_alerts)?;
        dict.set_item("warning_alerts", aggregated.warning_alerts)?;

        // Resource utilization
        dict.set_item("memory_usage_bytes", aggregated.memory_usage_bytes)?;
        dict.set_item("cpu_utilization", aggregated.cpu_utilization)?;
        dict.set_item("network_utilization", aggregated.network_utilization)?;

        Ok(dict.into())
    }

    /// Get active performance alerts
    fn get_active_alerts<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyList>> {
        let collector = &crate::metrics::METRICS_COLLECTOR;
        let snapshot = collector.create_snapshot();

        let py_list = PyList::empty(py);

        for alert in snapshot.alerts {
            let alert_dict = PyDict::new(py);

            alert_dict.set_item("id", &alert.id)?;
            alert_dict.set_item("alert_type", format!("{:?}", alert.alert_type))?;
            alert_dict.set_item("severity", format!("{:?}", alert.severity))?;
            alert_dict.set_item("triggered_at", alert.triggered_at.to_rfc3339())?;
            alert_dict.set_item("last_updated", alert.last_updated.to_rfc3339())?;
            alert_dict.set_item("message", &alert.message)?;
            alert_dict.set_item("active", alert.active)?;
            alert_dict.set_item("occurrence_count", alert.occurrence_count)?;

            if let Some(conv_id) = &alert.conversation_id {
                alert_dict.set_item("conversation_id", conv_id)?;
            }
            if let Some(tube_id) = &alert.tube_id {
                alert_dict.set_item("tube_id", tube_id)?;
            }

            // Add details
            let details_dict = PyDict::new(py);
            for (key, value) in &alert.details {
                details_dict.set_item(key, value)?;
            }
            alert_dict.set_item("details", details_dict)?;

            py_list.append(alert_dict)?;
        }

        Ok(py_list)
    }

    /// Get system uptime and basic stats
    fn get_system_stats(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let collector = &crate::metrics::METRICS_COLLECTOR;

        let dict = PyDict::new(py);

        dict.set_item("uptime_seconds", collector.get_uptime().as_secs())?;
        dict.set_item(
            "active_connection_count",
            collector.active_connection_count(),
        )?;

        // LOCK-FREE: Get tube count from registry (no locks!)
        dict.set_item("active_tube_count", REGISTRY.tube_count())?;

        Ok(dict.into())
    }

    /// Clear all connections from metrics tracking (for testing purposes)
    fn clear_metrics_connections(&self, _py: Python<'_>) -> PyResult<()> {
        crate::metrics::METRICS_COLLECTOR.clear_all_connections();
        Ok(())
    }

    // =============================================================================
    // CIRCUIT BREAKER MONITORING
    // =============================================================================

    /// Get detailed circuit breaker statistics for a specific tube
    /// Returns comprehensive stats including error-type specific metrics
    fn get_tube_circuit_breaker_stats(&self, py: Python<'_>, tube_id: &str) -> PyResult<Py<PyAny>> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        let stats_result = Python::detach(py, || {
            master_runtime.block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                let tube_arc = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube_arc {
                    tube.get_circuit_breaker_stats()
                        .await
                        .map_err(|e| format!("Failed to get circuit breaker stats: {}", e))
                } else {
                    Err(format!("Tube not found: {}", tube_id_owned))
                }
            })
        });

        match stats_result {
            Ok(stats) => {
                let dict = PyDict::new(py);
                dict.set_item("tube_id", &stats.tube_id)?;
                dict.set_item("state", &stats.state)?;
                dict.set_item("total_requests", stats.total_requests)?;
                dict.set_item("successful_requests", stats.successful_requests)?;
                dict.set_item("failed_requests", stats.failed_requests)?;
                dict.set_item("circuit_opens", stats.circuit_opens)?;
                dict.set_item("circuit_closes", stats.circuit_closes)?;
                dict.set_item("timeouts", stats.timeouts)?;

                // Error type counts
                let error_counts_dict = PyDict::new(py);
                for (error_type, count) in stats.error_type_counts.iter() {
                    error_counts_dict.set_item(error_type, *count)?;
                }
                dict.set_item("error_type_counts", error_counts_dict)?;

                // Error types that triggered circuit opens
                let triggered_opens_dict = PyDict::new(py);
                for (error_type, count) in stats.error_type_triggered_opens.iter() {
                    triggered_opens_dict.set_item(error_type, *count)?;
                }
                dict.set_item("error_type_triggered_opens", triggered_opens_dict)?;

                // Current error type failures (in Closed state)
                let current_failures_dict = PyDict::new(py);
                for (error_type, count) in stats.current_error_type_failures.iter() {
                    current_failures_dict.set_item(error_type, *count)?;
                }
                dict.set_item("current_error_type_failures", current_failures_dict)?;

                Ok(dict.into())
            }
            Err(e) => Err(PyRuntimeError::new_err(e)),
        }
    }

    /// Check if a tube's circuit breaker is healthy (closed state)
    /// Returns boolean indicating if the circuit is closed (healthy)
    fn get_tube_circuit_breaker_health(&self, py: Python<'_>, tube_id: &str) -> PyResult<bool> {
        let tube_id_owned = tube_id.to_string();
        let master_runtime = get_runtime();

        Python::detach(py, || {
            master_runtime.block_on(async move {
                // LOCK-FREE: Direct call to registry (no locks!)
                let tube_arc = REGISTRY.get_by_tube_id(&tube_id_owned);

                if let Some(tube) = tube_arc {
                    tube.is_circuit_breaker_healthy().await.map_err(|e| {
                        PyRuntimeError::new_err(format!(
                            "Failed to get circuit breaker health: {}",
                            e
                        ))
                    })
                } else {
                    Err(PyRuntimeError::new_err(format!(
                        "Tube not found: {}",
                        tube_id_owned
                    )))
                }
            })
        })
    }
}

// Implement Drop trait for PyTubeRegistry as a safety net
impl Drop for PyTubeRegistry {
    fn drop(&mut self) {
        if !self.explicit_cleanup_called.load(Ordering::SeqCst) {
            eprintln!("FORCE CLOSE: PyTubeRegistry Drop - forcing immediate cleanup!");

            // LOCK-FREE: Direct access to registry DashMaps (no locks!)
            let tube_count = REGISTRY.tube_count();
            if tube_count > 0 {
                eprintln!("FORCE CLOSE: Clearing {tube_count} tubes immediately");

                // 1. Clean up metrics for all conversations before clearing tubes
                let all_tube_ids = REGISTRY.all_tube_ids_sync();
                for tube_id in &all_tube_ids {
                    let conversation_ids = REGISTRY.conversation_ids_by_tube_id(tube_id);
                    for conversation_id in &conversation_ids {
                        crate::metrics::METRICS_COLLECTOR.unregister_connection(conversation_id);
                    }
                    // Also clean up the tube_id itself if it's registered as a conversation
                    crate::metrics::METRICS_COLLECTOR.unregister_connection(tube_id);
                }

                // 2. Clear all conversation mappings
                REGISTRY.conversations().clear();

                // 3. Drop all tube references (this will trigger their Drop)
                // When Tubes drop, their signal_senders auto-close (RAII handles it!)
                REGISTRY.tubes().clear();

                eprintln!("FORCE CLOSE: Registry cleaned up successfully");
            }

            // CRITICAL: Force shutdown the runtime to prevent hanging threads
            eprintln!("FORCE CLOSE: Shutting down runtime to prevent hanging threads");
            crate::runtime::shutdown_runtime_from_python();
        }
    }
}
