// Python Handler Task - Batched message delivery to Python callbacks
//
// This module implements the background task that receives messages from
// PythonHandler protocol mode channels and delivers them to Python callbacks
// with efficient batching to minimize GIL acquisition overhead.
//
// It also provides an outbound sender task that processes messages queued by
// Python callbacks (via send_handler_data) and sends them to WebRTC. This
// architecture prevents deadlocks by ensuring Python callbacks never block
// waiting for async operations.

use crate::buffer_pool::BufferPool;
use crate::channel::core::{PythonHandlerMessage, PythonHandlerOutbound};
use crate::runtime::RuntimeHandle;
use crate::tube_protocol::Frame;
use crate::tube_registry::REGISTRY;
use log::{debug, error, warn};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Maximum number of messages to batch before calling Python
const MAX_BATCH_SIZE: usize = 10;

/// Maximum time to wait for more messages before flushing the batch
const BATCH_TIMEOUT_MS: u64 = 1;

/// Set up the Python handler task that receives messages from Rust and delivers to Python
///
/// The task batches messages to minimize GIL acquisition overhead:
/// - Up to MAX_BATCH_SIZE messages per batch
/// - Or BATCH_TIMEOUT_MS timeout, whichever comes first
///
/// # Arguments
/// * `conversation_id` - The conversation ID for this handler
/// * `receiver` - The receiver end of the message channel from Rust
/// * `runtime_handle` - The tokio runtime handle
/// * `callback` - The Python callback function to invoke with batched messages
///
/// The callback receives a list of dicts, each with:
/// - `type`: "data", "connection_opened", or "connection_closed"
/// - `conn_no`: The connection number
/// - `payload`: For data messages, the bytes payload
/// - `reason`: For connection_closed, the close reason code
pub fn setup_python_handler_task(
    conversation_id: String,
    mut receiver: mpsc::Receiver<PythonHandlerMessage>,
    runtime_handle: RuntimeHandle,
    callback: Py<PyAny>,
) {
    let task_conversation_id = conversation_id.clone();
    let runtime = runtime_handle.runtime().clone();

    runtime.spawn(async move {
        debug!(
            "Python handler task started (conversation_id: {})",
            task_conversation_id
        );

        let mut batch: Vec<PythonHandlerMessage> = Vec::with_capacity(MAX_BATCH_SIZE);
        let mut total_messages = 0u64;
        let mut total_batches = 0u64;

        loop {
            // Try to fill the batch
            let should_flush = loop {
                if batch.len() >= MAX_BATCH_SIZE {
                    // Batch is full, flush it
                    break true;
                }

                // Wait for a message with timeout
                match timeout(Duration::from_millis(BATCH_TIMEOUT_MS), receiver.recv()).await {
                    Ok(Some(msg)) => {
                        batch.push(msg);
                        // If batch is full, flush immediately
                        if batch.len() >= MAX_BATCH_SIZE {
                            break true;
                        }
                        // Try to get more messages without blocking
                        while batch.len() < MAX_BATCH_SIZE {
                            match receiver.try_recv() {
                                Ok(msg) => batch.push(msg),
                                Err(_) => break, // No more immediate messages
                            }
                        }
                        // Flush if we have any messages
                        if !batch.is_empty() {
                            break true;
                        }
                    }
                    Ok(None) => {
                        // Channel closed
                        debug!(
                            "Python handler channel closed (conversation_id: {})",
                            task_conversation_id
                        );
                        // Flush any remaining messages
                        break !batch.is_empty();
                    }
                    Err(_) => {
                        // Timeout - flush if we have messages
                        if !batch.is_empty() {
                            break true;
                        }
                        // No messages, continue waiting
                    }
                }
            };

            if should_flush && !batch.is_empty() {
                total_messages += batch.len() as u64;
                total_batches += 1;

                // Deliver batch to Python
                let batch_to_send = std::mem::replace(
                    &mut batch,
                    Vec::with_capacity(MAX_BATCH_SIZE),
                );

                let conv_id = task_conversation_id.clone();

                // Call Python with the batch
                // Note: callback is already owned by this async closure, so we can pass a reference
                if let Err(e) = deliver_batch_to_python(&conv_id, batch_to_send, &callback) {
                    error!(
                        "Error delivering batch to Python handler (conversation_id: {}): {:?}",
                        conv_id, e
                    );
                    // Don't break on error - continue processing
                }
            }

            // Check if channel is closed (receiver returned None)
            if receiver.is_closed() && batch.is_empty() {
                break;
            }
        }

        debug!(
            "Python handler task completed (conversation_id: {}, total_messages: {}, total_batches: {})",
            task_conversation_id, total_messages, total_batches
        );
    });
}

/// Deliver a batch of messages to the Python callback
fn deliver_batch_to_python(
    conversation_id: &str,
    batch: Vec<PythonHandlerMessage>,
    callback: &Py<PyAny>,
) -> PyResult<()> {
    Python::attach(|py| {
        let py_list = PyList::empty(py);

        for msg in batch {
            let py_dict = PyDict::new(py);

            match msg {
                PythonHandlerMessage::ConnectionOpened { conn_no } => {
                    py_dict.set_item("type", "connection_opened")?;
                    py_dict.set_item("conn_no", conn_no)?;
                    py_dict.set_item("conversation_id", conversation_id)?;
                }
                PythonHandlerMessage::Data { conn_no, payload } => {
                    py_dict.set_item("type", "data")?;
                    py_dict.set_item("conn_no", conn_no)?;
                    py_dict.set_item("conversation_id", conversation_id)?;
                    // Convert Bytes to PyBytes
                    let py_bytes = PyBytes::new(py, &payload);
                    py_dict.set_item("payload", py_bytes)?;
                }
                PythonHandlerMessage::ConnectionClosed { conn_no, reason } => {
                    py_dict.set_item("type", "connection_closed")?;
                    py_dict.set_item("conn_no", conn_no)?;
                    py_dict.set_item("conversation_id", conversation_id)?;
                    py_dict.set_item("reason", reason as u16)?;
                }
            }

            py_list.append(py_dict)?;
        }

        // Call the Python callback with the list of messages
        callback.call1(py, (py_list,)).map_err(|e| {
            // Log and convert the Python error
            warn!(
                "Python handler callback error (conversation_id: {}): {:?}",
                conversation_id, e
            );
            PyRuntimeError::new_err(format!("Handler callback failed: {e}"))
        })?;

        Ok(())
    })
}

/// Create a channel pair for Python handler communication
/// Returns (sender, receiver) where:
/// - sender: Used by the Channel to send messages to Python
/// - receiver: Used by the handler task to receive messages
pub fn create_handler_channel() -> (mpsc::Sender<PythonHandlerMessage>, mpsc::Receiver<PythonHandlerMessage>) {
    // Use a bounded channel to provide backpressure
    // 1000 messages should be plenty for most use cases
    mpsc::channel(1000)
}

/// Create an outbound channel for Python handler to send data back to WebRTC
/// Returns (sender, receiver) where:
/// - sender: Stored globally and used by send_handler_data() to queue messages
/// - receiver: Used by the outbound sender task to process messages
pub fn create_outbound_channel() -> (mpsc::Sender<PythonHandlerOutbound>, mpsc::Receiver<PythonHandlerOutbound>) {
    // Use a bounded channel for backpressure
    // 1000 messages should be plenty - sync responses are small
    mpsc::channel(1000)
}

/// Set up the outbound sender task that processes messages from Python callbacks
/// and sends them to WebRTC.
///
/// This task runs independently and processes the outbound queue, ensuring
/// Python callbacks never block waiting for async WebRTC operations.
///
/// # Arguments
/// * `conversation_id` - The conversation ID for logging
/// * `receiver` - The receiver end of the outbound message channel
/// * `runtime_handle` - The tokio runtime handle
pub fn setup_outbound_sender_task(
    conversation_id: String,
    mut receiver: mpsc::Receiver<PythonHandlerOutbound>,
    runtime_handle: RuntimeHandle,
) {
    let task_conversation_id = conversation_id.clone();
    let runtime = runtime_handle.runtime().clone();

    runtime.spawn(async move {
        debug!(
            "Outbound sender task started (conversation_id: {})",
            task_conversation_id
        );

        let buffer_pool = BufferPool::default();
        let mut total_messages = 0u64;
        let mut total_bytes = 0u64;

        while let Some(msg) = receiver.recv().await {
            total_messages += 1;
            total_bytes += msg.data.len() as u64;

            // Get the tube by conversation ID
            let tube_arc = match REGISTRY.get_by_conversation_id(&msg.conversation_id) {
                Some(tube) => tube,
                None => {
                    warn!(
                        "Outbound sender: No tube found for conversation ID: {} (message dropped)",
                        msg.conversation_id
                    );
                    continue;
                }
            };

            // Get the data channel
            let data_channels = tube_arc.data_channels.read().await;
            let channel = match data_channels.get(&msg.conversation_id) {
                Some(ch) => ch.clone(),
                None => {
                    warn!(
                        "Outbound sender: Channel not found: {} (message dropped)",
                        msg.conversation_id
                    );
                    continue;
                }
            };
            drop(data_channels); // Release lock before sending

            // Create and send the frame
            let frame = Frame::new_data_with_pool(msg.conn_no, &msg.data, &buffer_pool);
            let encoded = frame.encode_with_pool(&buffer_pool);

            if let Err(e) = channel.send(encoded).await {
                error!(
                    "Outbound sender: Failed to send frame (conversation_id: {}, conn_no: {}, error: {})",
                    msg.conversation_id, msg.conn_no, e
                );
                // Continue processing other messages
            } else {
                debug!(
                    "Outbound sender: Sent {} bytes (conversation_id: {}, conn_no: {})",
                    msg.data.len(), msg.conversation_id, msg.conn_no
                );
            }
        }

        debug!(
            "Outbound sender task completed (conversation_id: {}, total_messages: {}, total_bytes: {})",
            task_conversation_id, total_messages, total_bytes
        );
    });
}

/// Global outbound sender for Python handler mode
/// This is set up when a PythonHandler tube is created
static OUTBOUND_SENDER: std::sync::OnceLock<mpsc::Sender<PythonHandlerOutbound>> = std::sync::OnceLock::new();

/// Initialize the global outbound sender
/// Called when setting up a PythonHandler tube
pub fn init_outbound_sender(sender: mpsc::Sender<PythonHandlerOutbound>) {
    // Try to set. If already set, that's OK - we use the existing one
    let _ = OUTBOUND_SENDER.set(sender);
}

/// Queue a message to be sent to WebRTC (non-blocking)
/// Called by send_handler_data() from within Python callbacks
///
/// Returns Ok(()) if the message was queued successfully
/// Returns Err if the outbound channel is not initialized or full
pub fn queue_outbound_message(msg: PythonHandlerOutbound) -> Result<(), String> {
    let sender = OUTBOUND_SENDER.get()
        .ok_or_else(|| "Outbound sender not initialized".to_string())?;

    sender.try_send(msg).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => "Outbound queue full".to_string(),
        mpsc::error::TrySendError::Closed(_) => "Outbound channel closed".to_string(),
    })
}
