use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyPermissionError, PyConnectionError, PyValueError};
use pyo3::PyErr;
use std::collections::VecDeque;
use bytes::Bytes;
use parking_lot::Mutex;
use crate::error::ChannelError;

pub const DEFAULT_BUFFER_CAPACITY: usize = 64 * 1024; // 64KB for standard compatibility

// Convert Rust ChannelError to Python exceptions
pub fn channel_error_to_py_err(err: ChannelError) -> PyErr {
    match err {
        ChannelError::Timeout(msg) => PyErr::new::<PyTimeoutError, _>(msg),
        ChannelError::NetworkAccessDenied(msg) => PyErr::new::<PyPermissionError, _>(msg),
        ChannelError::ConnectionFailed(msg) => PyErr::new::<PyConnectionError, _>(msg),
        ChannelError::InvalidFormat(msg) => PyErr::new::<PyValueError, _>(msg),
        _ => PyErr::new::<PyRuntimeError, _>(err.to_string()),
    }
}

// Efficient message buffer that prioritizes newer messages if capacity is reached
pub struct MessageBuffer {
    messages: VecDeque<Bytes>,
    capacity: usize,
    pool: Option<Vec<Bytes>>,
}

impl MessageBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            messages: VecDeque::with_capacity(capacity), 
            capacity,
            pool: Some(Vec::with_capacity(capacity)),
        }
    }

    pub fn push(&mut self, message: Bytes) {
        if self.messages.len() >= self.capacity {
            // Optionally, return to the pool instead of dropping
            if let Some(pool) = &mut self.pool {
                if pool.len() < self.capacity {
                    if let Some(old_msg) = self.messages.pop_front() {
                        pool.push(old_msg);
                    }
                }
            } else {
                self.messages.pop_front();
            }
        }
        self.messages.push_back(message);
    }

    pub fn drain(&mut self) -> Vec<Bytes> {
        self.messages.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

pub enum WebRTCEvent {
    IceCandidate(String),
    DataChannel(std::sync::Arc<webrtc::data_channel::RTCDataChannel>, std::sync::Arc<Mutex<MessageBuffer>>),
}

// Non-blocking channel to send with fallback to blocking if needed
pub async fn try_send_event<T>(tx: &tokio::sync::mpsc::Sender<T>, event: T) {
    // First try non-blocking send
    if let Err(e) = tx.try_send(event) {
        match e {
            tokio::sync::mpsc::error::TrySendError::Full(inner_event) => {
                // Only block if the channel is full, not closed
                if let Err(e) = tx.send(inner_event).await {
                    log::error!("Failed to send event: {}", e);
                }
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                log::error!("Channel closed, failed to send event");
            }
        }
    }
} 