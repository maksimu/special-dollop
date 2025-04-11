use crate::get_or_create_runtime_py;
use crate::webrtc_core::{
    create_data_channel, format_ice_candidate, WebRTCDataChannel, WebRTCPeerConnection,
};
use std::clone::Clone;

use bytes::Bytes;
use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PyModule};
use pyo3::Bound;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;

#[derive(FromPyObject)]
struct IceServer {
    #[pyo3(item("urls"))]
    urls: Vec<String>,
    #[pyo3(item("username"))]
    username: Option<String>,
    #[pyo3(item("credential"))]
    credential: Option<String>,
}

enum WebRTCEvent {
    IceCandidate(String),
    DataChannel(Arc<RTCDataChannel>, Arc<Mutex<Vec<Vec<u8>>>>),
}

// Non-blocking channel send with fallback to blocking if needed
async fn try_send_event<T>(tx: &mpsc::Sender<T>, event: T) {
    // First try non-blocking send
    if let Err(e) = tx.try_send(event) {
        match e {
            mpsc::error::TrySendError::Full(inner_event) => {
                // Only block if the channel is full, not closed
                if let Err(e) = tx.send(inner_event).await {
                    log::error!("Failed to send event: {}", e);
                }
            }
            mpsc::error::TrySendError::Closed(_) => {
                log::error!("Channel closed, failed to send event");
            }
        }
    }
}

#[pyclass]
pub struct PyRTCPeerConnection {
    conn: Arc<WebRTCPeerConnection>,
    _event_task_handle: Option<tokio::task::JoinHandle<()>>,
    is_closing: Arc<AtomicBool>,
}

#[pymethods]
impl PyRTCPeerConnection {
    #[new]
    fn new(
        py: Python<'_>,
        config: Py<PyDict>,
        on_ice_candidate: PyObject,
        on_data_channel: PyObject,
        trickle_ice: bool,
        turn_only: bool,
    ) -> PyResult<Self> {
        // Get shared runtime using the Python-specific accessor
        let runtime = get_or_create_runtime_py()?;

        // Parse the configuration
        let mut rtc_config = RTCConfiguration::default();
        let dict = config.bind(py);
        if let Ok(Some(servers)) = dict.get_item("iceServers") {
            let servers = servers.downcast::<PyList>()?;
            rtc_config.ice_servers = servers
                .iter()
                .filter_map(|server| {
                    server
                        .extract::<IceServer>()
                        .ok()
                        .map(|server| RTCIceServer {
                            urls: server.urls,
                            username: server.username.unwrap_or_default(),
                            credential: server.credential.unwrap_or_default(),
                        })
                })
                .collect();
        }

        if turn_only{
            rtc_config.ice_transport_policy = RTCIceTransportPolicy::Relay;
        }

        // Create the WebRTC connection asynchronously
        let conn = runtime
            .block_on(async { WebRTCPeerConnection::new(Some(rtc_config), trickle_ice).await })
            .map_err(PyErr::new::<PyRuntimeError, _>)?;
        let conn = Arc::new(conn);

        // Create channel for event communication - increased capacity for reduced blocking
        let (event_tx, mut event_rx) = mpsc::channel(64);

        // Track connection state for clean shutdown
        let is_closing = Arc::new(AtomicBool::new(false));

        // Clone necessary items for the event handling task
        let ice_callback = on_ice_candidate.clone_ref(py);
        let dc_callback = on_data_channel.clone_ref(py);

        // Set up WebRTC callbacks
        if trickle_ice {
            let tx = event_tx.clone();
            let pc = Arc::clone(&conn.peer_connection);
            pc.on_ice_candidate(Box::new(move |candidate_option| {
                let tx = tx.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate_option {
                        let candidate_str = format_ice_candidate(&candidate);
                        try_send_event(&tx, WebRTCEvent::IceCandidate(candidate_str)).await;
                    }
                })
            }));
        }

        let tx = event_tx.clone();
        let pc = Arc::clone(&conn.peer_connection);
        pc.on_data_channel(Box::new(move |dc| {
            let tx = tx.clone();

            log::debug!("Data channel detected, setting up initial message buffer");

            // Create a pre-sized buffer for early messages
            let buffer = Arc::new(Mutex::new(Vec::with_capacity(32)));
            let buffer_clone = buffer.clone();

            // Set up an initial message handler to buffer messages
            dc.on_message(Box::new(move |msg| {
                let buffer = Arc::clone(&buffer_clone);
                Box::pin(async move {
                    // Buffer this message until Python takes ownership
                    // Use minimal lock scope
                    {
                        let mut guard = buffer.lock();
                        guard.push(msg.data.to_vec());
                    }
                    log::debug!("Buffered early message");
                })
            }));

            // Now pass the data channel and buffer to Python side
            Box::pin(async move {
                // Send both the data channel and its buffer
                try_send_event(&tx, WebRTCEvent::DataChannel(dc, buffer)).await;
            })
        }));

        // Track event task state
        let is_closing_clone = Arc::clone(&is_closing);

        // Spawn event handling task with optimized priority
        let event_task = runtime.spawn(async move {
            while let Some(event) = event_rx.recv().await {
                // Check if connection is closing before processing events
                if is_closing_clone.load(Ordering::Acquire) {
                    break;
                }

                Python::with_gil(|py| match event {
                    WebRTCEvent::IceCandidate(candidate) => {
                        if let Err(e) = ice_callback.call1(py, (candidate,)) {
                            log::error!("Error calling ice candidate callback: {:?}", e);
                        }
                    }
                    WebRTCEvent::DataChannel(dc, buffer) => {
                        // Create PyRTCDataChannel with optimized buffer handling
                        let py_dc = match PyRTCDataChannel::new_from_existing(dc.clone(), buffer) {
                            Ok(py_dc) => py_dc,
                            Err(e) => {
                                log::error!("Error creating data channel from existing: {}", e);
                                return;
                            }
                        };

                        match Py::new(py, py_dc) {
                            Ok(py_dc_obj) => {
                                if let Err(e) = dc_callback.call1(py, (py_dc_obj,)) {
                                    log::error!("Error calling data channel callback: {:?}", e);
                                }
                            }
                            Err(e) => log::error!("Error creating PyRTCDataChannel: {:?}", e),
                        }
                    }
                });
            }
            log::debug!("Event handling task terminated");
        });

        Ok(Self {
            conn,
            _event_task_handle: Some(event_task),
            is_closing,
        })
    }

    fn create_data_channel(&self, py: Python<'_>, label: &str) -> PyResult<Py<PyRTCDataChannel>> {
        // Get shared runtime
        let runtime = get_or_create_runtime_py()?;

        // Create a buffer for early messages with optimized capacity
        let buffer = Arc::new(Mutex::new(Vec::<Vec<u8>>::with_capacity(32)));
        let buffer_clone = Arc::clone(&buffer);

        // Use the core implementation to create a data channel
        let dc = runtime
            .block_on(async { create_data_channel(&self.conn.peer_connection, label).await })
            .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

        // Set up an initial message handler to buffer messages
        dc.on_message(Box::new(move |msg| {
            let buffer = Arc::clone(&buffer_clone);
            Box::pin(async move {
                // Buffer this message until Python takes ownership
                // Minimize lock scope
                {
                    let mut guard = buffer.lock();
                    guard.push(msg.data.to_vec());
                }
                log::debug!("Buffered early message in created channel");
            })
        }));

        // Create the Python data channel with the buffer using optimized constructor
        match PyRTCDataChannel::new_from_existing(dc, buffer) {
            Ok(py_dc) => Py::new(py, py_dc),
            Err(e) => Err(PyErr::new::<PyRuntimeError, _>(format!(
                "Failed to create data channel: {}",
                e
            ))),
        }
    }

    fn create_offer(&self) -> PyResult<String> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of cloning the Arc
        let conn = &self.conn;

        runtime.block_on(async {
            conn.create_offer()
                .await
                .map_err(PyErr::new::<PyRuntimeError, _>)
        })
    }

    fn create_answer(&self) -> PyResult<String> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of cloning the Arc
        let conn = &self.conn;

        runtime.block_on(async {
            conn.create_answer()
                .await
                .map_err(PyErr::new::<PyRuntimeError, _>)
        })
    }

    fn set_remote_description(&self, sdp: String) -> PyResult<()> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of cloning the Arc
        let conn = &self.conn;

        // Determine if this is an answer based on current state - pre-fetch outside async block
        let is_answer = matches!(
            self.conn.peer_connection.signaling_state(),
            webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer
        );

        runtime.block_on(async {
            conn.set_remote_description(sdp, is_answer)
                .await
                .map_err(PyErr::new::<PyRuntimeError, _>)
        })
    }

    fn add_ice_candidate(&self, candidate: String) -> PyResult<()> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of cloning the Arc
        let conn = &self.conn;

        runtime.block_on(async {
            conn.add_ice_candidate(candidate)
                .await
                .map_err(PyErr::new::<PyRuntimeError, _>)
        })
    }

    #[getter]
    fn connection_state(&self) -> String {
        self.conn.connection_state()
    }

    fn __del__(&self) -> PyResult<()> {
        log::info!("PyRTCPeerConnection is being garbage collected, cleaning up resources");

        // Call our close method to handle all cleanup in one place
        if let Err(e) = self.close() {
            log::error!("Error during connection cleanup in __del__: {}", e);
        }

        Ok(())
    }

    fn close(&self) -> PyResult<()> {
        log::info!("Explicitly closing PyRTCPeerConnection");

        // Mark as closing to stop event processing
        self.is_closing.store(true, Ordering::Release);

        // Abort the event task first to prevent new callbacks
        if let Some(handle) = &self._event_task_handle {
            handle.abort();
        }

        // Use the core implementation to close the connection
        let runtime = get_or_create_runtime_py()?;
        let conn = Arc::clone(&self.conn);

        runtime.block_on(async { conn.close().await.map_err(PyErr::new::<PyRuntimeError, _>) })
    }
}

impl Drop for PyRTCPeerConnection {
    fn drop(&mut self) {
        log::info!("PyRTCPeerConnection is being dropped");

        // Mark as closing to stop event processing
        self.is_closing.store(true, Ordering::Release);

        // Abort any event tasks first
        if let Some(handle) = &self._event_task_handle {
            handle.abort();
        }

        // Clone what we need for cleanup
        let conn = Arc::clone(&self.conn);

        // Spawn a detached thread for cleanup to avoid blocking in drop()
        thread::spawn(move || {
            if let Ok(runtime) = get_or_create_runtime_py() {
                runtime.block_on(async {
                    if let Err(e) = conn.close().await {
                        log::error!("Error closing connection in drop: {}", e);
                    }
                });
            }
        });
    }
}

#[pyclass]
pub struct PyRTCDataChannel {
    dc_wrapper: WebRTCDataChannel,
    message_callback: Arc<Mutex<Option<PyObject>>>,
    error_callback: Arc<Mutex<Option<PyObject>>>,
    // Buffer for messages received before callback is set - using Vec<u8> consistently
    message_buffer: Arc<Mutex<Vec<Vec<u8>>>>,
    is_callback_set: Arc<AtomicBool>,
}

impl PyRTCDataChannel {
    // Optimized constructor from existing data channel and buffer
    fn new_from_existing(
        dc: Arc<RTCDataChannel>,
        initial_buffer: Arc<Mutex<Vec<Vec<u8>>>>,
    ) -> Result<Self, String> {
        // Wrap the data channel
        let dc_wrapper = WebRTCDataChannel::new(dc);

        let message_callback = Arc::new(Mutex::new(None::<PyObject>));
        let error_callback = Arc::new(Mutex::new(None::<PyObject>));

        // Create a message buffer with a reasonable initial capacity based on existing data
        let message_buffer = {
            let src = initial_buffer.lock();
            let capacity = src.len().max(32); // At least 32 or the current size

            if src.is_empty() {
                // If empty, no need to transfer anything
                Arc::new(Mutex::new(Vec::<Vec<u8>>::with_capacity(capacity)))
            } else {
                // If not empty, clone the entire buffer at once
                let mut new_buffer = Vec::with_capacity(capacity);
                new_buffer.extend_from_slice(&src);
                log::info!("Transferred {} buffered messages", src.len());
                Arc::new(Mutex::new(new_buffer))
            }
        };

        let is_callback_set = Arc::new(AtomicBool::new(false));

        Ok(Self {
            dc_wrapper,
            message_callback,
            error_callback,
            message_buffer,
            is_callback_set,
        })
    }
}

#[pymethods]
impl PyRTCDataChannel {
    #[getter]
    fn get_on_message(&self) -> Option<PyObject> {
        Python::with_gil(|py| {
            self.message_callback
                .lock()
                .as_ref()
                .map(|obj| obj.clone_ref(py))
        })
    }

    #[pyo3(signature = (data, timeout=None))]
    fn send(&self, py: Python<'_>, data: PyObject, timeout: Option<f64>) -> PyResult<()> {
        let timeout_duration = timeout.map(Duration::from_secs_f64);
        let runtime = get_or_create_runtime_py()?;
        let dc_wrapper = &self.dc_wrapper; // Use reference instead of clone

        // Efficiently extract the bytes with the most direct method available
        let bytes = if let Ok(bytes) = data.downcast_bound::<PyBytes>(py) {
            Bytes::copy_from_slice(bytes.as_bytes())
        } else if let Ok(bytearray) = data.downcast_bound::<PyByteArray>(py) {
            // Extract bytes with minimal copying
            let bytes_vec: Vec<u8> = bytearray.extract()?;
            Bytes::from(bytes_vec)
        } else {
            // Fallback for other buffer-like objects
            let bytes_vec: Vec<u8> = data.extract(py)?;
            Bytes::from(bytes_vec)
        };

        runtime.block_on(async {
            // Only wrap in timeout if needed
            if let Some(duration) = timeout_duration {
                match tokio::time::timeout(duration, dc_wrapper.data_channel.send(&bytes)).await {
                    Ok(result) => result.map(|_| ()).map_err(|e| {
                        PyErr::new::<PyRuntimeError, _>(format!("Failed to send: {}", e))
                    }),
                    Err(_) => Err(PyErr::new::<PyTimeoutError, _>("Send operation timed out")),
                }
            } else {
                dc_wrapper
                    .data_channel
                    .send(&bytes)
                    .await
                    .map(|_| ())
                    .map_err(|e| PyErr::new::<PyRuntimeError, _>(format!("Failed to send: {}", e)))
            }
        })
    }

    #[setter]
    fn set_on_message(&mut self, callback: Option<PyObject>) -> PyResult<()> {
        match callback {
            Some(cb) => {
                // Store the callback reference
                Python::with_gil(|py| {
                    *self.message_callback.lock() = Some(cb.clone_ref(py));
                });

                // Set up the message handler with direct PyBytes creation
                let message_callback = Arc::clone(&self.message_callback);
                let message_buffer = Arc::clone(&self.message_buffer);
                let is_callback_set = Arc::clone(&self.is_callback_set);

                self.dc_wrapper
                    .data_channel
                    .on_message(Box::new(move |msg| {
                        // Avoid unnecessary copying when possible
                        let bytes = msg.data.to_vec(); // We still need to copy here for ownership transfer
                        let message_callback = Arc::clone(&message_callback);
                        let message_buffer = Arc::clone(&message_buffer);
                        let is_callback_set = Arc::clone(&is_callback_set);

                        Box::pin(async move {
                            if is_callback_set.load(Ordering::Acquire) {
                                // Process outside the Python GIL when possible
                                let callback_option = Python::with_gil(|py| {
                                    message_callback.lock().as_ref().map(|cb| cb.clone_ref(py))
                                });

                                // Only enter Python context when we have a callback
                                if let Some(callback) = callback_option {
                                    Python::with_gil(|py| {
                                        // Create PyBytes directly from the bytes
                                        let py_bytes = PyBytes::new(py, &bytes);
                                        if let Err(e) = callback.call1(py, (py_bytes,)) {
                                            log::error!("Error in Python callback: {}", e);
                                        }
                                    });
                                }
                            } else {
                                // Buffer the message with minimal lock scope
                                {
                                    let mut buffer_guard = message_buffer.lock();
                                    buffer_guard.push(bytes);
                                }
                                log::debug!("Buffered message");
                            }
                        })
                    }));

                // Process existing buffered messages with minimal locking and memory operations
                Python::with_gil(|py| {
                    // Get callback reference before processing
                    let callback_option = {
                        let guard = self.message_callback.lock();
                        guard.as_ref().map(|cb| cb.clone_ref(py))
                    };

                    // Only process if we have a callback
                    if let Some(callback) = callback_option {
                        // Take ownership of the buffered messages in one operation
                        let messages_to_process = {
                            let mut buffer_guard = self.message_buffer.lock();
                            if buffer_guard.is_empty() {
                                Vec::new()
                            } else {
                                let buffered = std::mem::take(&mut *buffer_guard);
                                log::info!("Processing {} buffered messages", buffered.len());
                                buffered
                            }
                        };

                        // Mark callback as set before processing
                        self.is_callback_set.store(true, Ordering::Release);

                        // Process all messages outside of lock
                        for msg in messages_to_process {
                            let py_bytes = PyBytes::new(py, &msg);
                            if let Err(e) = callback.call1(py, (py_bytes,)) {
                                log::error!(
                                    "Error calling Python callback with buffered message: {}",
                                    e
                                );
                            }
                        }
                    }
                });
            }
            None => {
                // Clear callback and reset state
                *self.message_callback.lock() = None;
                self.is_callback_set.store(false, Ordering::Release);

                // Use an empty handler to avoid any processing costs
                self.dc_wrapper
                    .data_channel
                    .on_message(Box::new(move |_| Box::pin(async {})));
            }
        }
        Ok(())
    }

    #[getter]
    fn get_on_error(&self) -> Option<PyObject> {
        Python::with_gil(|py| {
            self.error_callback
                .lock()
                .as_ref()
                .map(|obj| obj.clone_ref(py))
        })
    }

    #[setter]
    fn set_on_error(&mut self, callback: Option<PyObject>) -> PyResult<()> {
        // Minimize lock scope
        match callback {
            Some(cb) => {
                Python::with_gil(|py| {
                    *self.error_callback.lock() = Some(cb.clone_ref(py));
                });
            }
            None => {
                *self.error_callback.lock() = None;
            }
        }
        Ok(())
    }

    #[getter]
    fn buffered_amount(&self) -> PyResult<u64> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of clone
        let dc_wrapper = &self.dc_wrapper;

        runtime.block_on(async { Ok(dc_wrapper.buffered_amount().await) })
    }

    #[pyo3(signature = (timeout=None))]
    fn wait_for_channel_open(&self, timeout: Option<f64>) -> PyResult<bool> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of clone
        let dc_wrapper = &self.dc_wrapper;

        // Optimize by performing timeout handling directly in the async block
        runtime.block_on(async {
            if let Some(timeout_secs) = timeout {
                let duration = Duration::from_secs_f64(timeout_secs);
                match tokio::time::timeout(duration, dc_wrapper.wait_for_channel_open(None)).await {
                    Ok(result) => result.map_err(PyErr::new::<PyRuntimeError, _>),
                    Err(_) => Ok(false), // Timeout occurred
                }
            } else {
                dc_wrapper
                    .wait_for_channel_open(None)
                    .await
                    .map_err(PyErr::new::<PyRuntimeError, _>)
            }
        })
    }

    fn close(&self) -> PyResult<()> {
        let runtime = get_or_create_runtime_py()?;
        // Use reference instead of clone
        let dc_wrapper = &self.dc_wrapper;

        runtime.block_on(async {
            dc_wrapper
                .close()
                .await
                .map_err(PyErr::new::<PyRuntimeError, _>)
        })
    }

    #[getter]
    fn ready_state(&self) -> String {
        self.dc_wrapper.ready_state()
    }

    #[getter]
    fn label(&self) -> String {
        self.dc_wrapper.label()
    }
}

#[pymodule]
fn keeper_pam_webrtc_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRTCPeerConnection>()?;
    m.add_class::<PyRTCDataChannel>()?;

    // Explicitly import and wrap the Python-specific logger initializer
    #[cfg(feature = "python")]
    {
        use crate::logger::initialize_logger as py_initialize_logger;
        m.add_function(wrap_pyfunction!(py_initialize_logger, py)?)?;
    }

    Ok(())
}
