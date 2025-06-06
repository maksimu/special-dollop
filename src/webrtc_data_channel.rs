use bytes::Bytes;
#[cfg(test)]
use futures::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};
use webrtc::data_channel::RTCDataChannel;

// Type alias for complex callback type
type BufferedAmountLowCallback = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync + 'static>>>>;

// Async-first wrapper for data channel functionality
pub struct WebRTCDataChannel {
    pub data_channel: Arc<RTCDataChannel>,
    pub(crate) is_closing: Arc<AtomicBool>,
    pub(crate) buffered_amount_low_threshold: Arc<Mutex<u64>>,
    pub(crate) on_buffered_amount_low_callback: BufferedAmountLowCallback,
    pub(crate) threshold_monitor: Arc<AtomicBool>,

    #[cfg(test)]
    pub(crate) test_send_hook:
        Arc<Mutex<Option<Box<dyn Fn(Bytes) -> BoxFuture<'static, ()> + Send + Sync + 'static>>>>,
}

impl Clone for WebRTCDataChannel {
    fn clone(&self) -> Self {
        WebRTCDataChannel {
            data_channel: Arc::clone(&self.data_channel),
            is_closing: Arc::clone(&self.is_closing),
            buffered_amount_low_threshold: Arc::clone(&self.buffered_amount_low_threshold),
            on_buffered_amount_low_callback: Arc::clone(&self.on_buffered_amount_low_callback),
            threshold_monitor: Arc::clone(&self.threshold_monitor),

            #[cfg(test)]
            test_send_hook: Arc::clone(&self.test_send_hook),
        }
    }
}

impl WebRTCDataChannel {
    pub fn new(data_channel: Arc<RTCDataChannel>) -> Self {
        Self {
            data_channel,
            is_closing: Arc::new(AtomicBool::new(false)),
            buffered_amount_low_threshold: Arc::new(Mutex::new(0)),
            on_buffered_amount_low_callback: Arc::new(Mutex::new(None)),
            threshold_monitor: Arc::new(AtomicBool::new(false)),

            #[cfg(test)]
            test_send_hook: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the buffered amount low threshold
    pub fn set_buffered_amount_low_threshold(&self, threshold: u64) {
        let mut guard = self.buffered_amount_low_threshold.lock().unwrap();
        *guard = threshold;

        // Log the threshold change
        debug!("Set buffered amount low threshold to {} bytes", threshold);

        // Set the native WebRTC bufferedAmountLowThreshold
        if threshold > 0 {
            let dc = self.clone();
            let threshold_clone = threshold;

            // Spawn a task to set the threshold and register the callback on the native data channel
            tokio::spawn(async move {
                // Set the native threshold - convert u64 to usize
                let threshold_usize = threshold_clone.try_into().unwrap_or(usize::MAX);
                dc.data_channel
                    .set_buffered_amount_low_threshold(threshold_usize)
                    .await;

                // Make a separate clone for the callback
                let callback_dc = dc.clone();

                // Register the onBufferedAmountLow callback
                dc.data_channel
                    .on_buffered_amount_low(Box::new(move || {
                        let callback_dc = callback_dc.clone();
                        let threshold_value = threshold_clone;

                        Box::pin(async move {
                            debug!(
                                "Native bufferedAmountLow event triggered (buffer below {})",
                                threshold_value
                            );

                            // Get and call our callback
                            let callback_guard =
                                callback_dc.on_buffered_amount_low_callback.lock().unwrap();
                            if let Some(ref callback) = *callback_guard {
                                callback();
                            }
                        })
                    }))
                    .await;
            });
        }
    }

    /// Set the callback to be called when the buffered amount drops below the threshold
    pub fn on_buffered_amount_low(&self, callback: Option<Box<dyn Fn() + Send + Sync + 'static>>) {
        // Check is_some() before moving the callback
        let has_callback = callback.is_some();

        // Now move it into the mutex
        let mut guard = self.on_buffered_amount_low_callback.lock().unwrap();
        *guard = callback;

        debug!("Set buffered amount low callback: {}", has_callback);
    }

    // Add a test method to set the sending hook for testing
    #[cfg(test)]
    pub fn set_test_send_hook<F>(&self, hook: F)
    where
        F: Fn(Bytes) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    {
        let mut guard = self.test_send_hook.lock().unwrap();
        *guard = Some(Box::new(hook));
    }

    pub async fn send(&self, data: Bytes) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Channel is closing".to_string());
        }

        // For testing: call the test hook if set
        #[cfg(test)]
        {
            let hook_guard = self.test_send_hook.lock().unwrap();
            if let Some(ref hook) = *hook_guard {
                // Clone the data for the hook
                let data_clone = data.clone();

                // Call the hook with a clone of the data
                let hook_future = hook(data_clone);

                // Spawn the hook execution to avoid blocking
                tokio::spawn(hook_future);
            }
        }

        // Send data with detailed error handling
        let result = self
            .data_channel
            .send(&data)
            .await
            .map(|_| ())
            .map_err(|e| format!("Failed to send data: {}", e));

        // No need to manually monitor buffered amount - we rely on the native WebRTC event
        // The onBufferedAmountLow event will fire when the buffer drops below the threshold

        result
    }

    pub async fn buffered_amount(&self) -> u64 {
        // Early return if the channel is closing
        if self.is_closing.load(Ordering::Acquire) {
            return 0;
        }

        self.data_channel.buffered_amount().await as u64
    }

    #[cfg(test)]
    pub async fn wait_for_channel_open(&self, timeout: Option<Duration>) -> Result<bool, String> {
        let timeout_duration = timeout.unwrap_or(Duration::from_secs(10));

        // Use oneshot channel for event-driven notification
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Wrap sender in Arc<Mutex<Option<>>> so we can safely consume it from either callback
        let sender = Arc::new(Mutex::new(Some(tx)));

        // Create shared flags for state
        let is_open = Arc::new(AtomicBool::new(false));
        let is_open_for_close = Arc::clone(&is_open);

        // We don't need this variable anymore
        let is_closing = self.is_closing.clone();

        // Set up onOpen callback if not already open
        if self.data_channel.ready_state()
            != webrtc::data_channel::data_channel_state::RTCDataChannelState::Open
        {
            let sender_clone = Arc::clone(&sender);

            self.data_channel.on_open(Box::new(move || {
                // Set the is_open flag
                is_open.store(true, Ordering::Release);

                // Send the notification, consuming the sender
                if let Some(tx) = sender_clone.lock().unwrap().take() {
                    let _ = tx.send(true);
                }

                Box::pin(async {})
            }));
        }

        // Set up onClose callback
        let sender_for_close = Arc::clone(&sender);

        self.data_channel.on_close(Box::new(move || {
            // If opened and then closed during this wait, send it false
            if is_open_for_close.load(Ordering::Acquire) {
                // Send false to indicate a closed state, consuming the sender
                if let Some(tx) = sender_for_close.lock().unwrap().take() {
                    let _ = tx.send(false);
                }
            }

            Box::pin(async {})
        }));

        // Check if already closed first (fast path)
        if is_closing.load(Ordering::Acquire) {
            return Err("Data channel is closing".to_string());
        }

        // Check if already open the second (fast path)
        if self.data_channel.ready_state()
            == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open
        {
            return Ok(true);
        }

        // Wait for the event or timeout
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(state)) => Ok(state),
            Ok(Err(_)) => {
                // Channel closed without sending a value
                Ok(false)
            }
            Err(_) => {
                // Timeout occurred
                Ok(self.data_channel.ready_state()
                    == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open)
            }
        }
    }

    pub async fn close(&self) -> Result<(), String> {
        // Avoid duplicate close operations
        if self.is_closing.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already closing or closed
        }

        // Close with timeout to avoid hanging
        match tokio::time::timeout(Duration::from_secs(3), self.data_channel.close()).await {
            Ok(result) => result.map_err(|e| format!("Failed to close data channel: {}", e)),
            Err(_) => {
                warn!("Data channel close operation timed out, forcing abandonment");
                Ok(()) // Force success even though it timed out
            }
        }
    }

    pub fn ready_state(&self) -> String {
        // Fast path for closing
        if self.is_closing.load(Ordering::Acquire) {
            return "Closed".to_string();
        }

        format!("{:?}", self.data_channel.ready_state())
    }

    pub fn label(&self) -> String {
        self.data_channel.label().to_string()
    }
}

/// Event-driven sender that uses WebRTC native bufferedAmountLow events
/// Eliminates polling and provides natural backpressure
pub struct EventDrivenSender {
    data_channel: Arc<WebRTCDataChannel>,
    pending_frames: Arc<Mutex<VecDeque<Bytes>>>,
    can_send: Arc<AtomicBool>,
    threshold: u64, // Backpressure threshold for monitoring
}

impl EventDrivenSender {
    /// Create a new event-driven sender with the specified threshold
    pub fn new(data_channel: Arc<WebRTCDataChannel>, threshold: u64) -> Self {
        let sender = Self {
            data_channel: data_channel.clone(),
            pending_frames: Arc::new(Mutex::new(VecDeque::new())),
            can_send: Arc::new(AtomicBool::new(true)),
            threshold,
        };

        // Set up WebRTC native event handling
        data_channel.set_buffered_amount_low_threshold(threshold);

        let can_send_clone = sender.can_send.clone();
        let pending_clone = sender.pending_frames.clone();
        let dc_clone = data_channel.clone();

        // EVENT-DRIVEN: Only wake up when buffer space available
        data_channel.on_buffered_amount_low(Some(Box::new(move || {
            can_send_clone.store(true, Ordering::Release);

            // Drain pending frames when space becomes available (batched)
            let to_send = {
                let mut pending = pending_clone.lock().unwrap();
                let batch_size = std::cmp::min(pending.len(), 16); // Max 16 frames per batch
                pending.drain(..batch_size).collect::<Vec<_>>()
            };

            if !to_send.is_empty() {
                let dc = dc_clone.clone();
                let can_send_for_batch = can_send_clone.clone();

                tokio::spawn(async move {
                    for frame in to_send {
                        match dc.send(frame).await {
                            Ok(_) => continue,
                            Err(_) => {
                                // On failure, mark as unable to send
                                can_send_for_batch.store(false, Ordering::Release);
                                break;
                            }
                        }
                    }
                });
            }
        })));

        sender
    }

    /// Send with zero-polling natural backpressure
    /// Returns immediately - either sends or queues for later
    pub async fn send_with_natural_backpressure(&self, frame: Bytes) -> Result<(), String> {
        // Fast path: send immediately if buffer has space
        if self.can_send.load(Ordering::Acquire) {
            match self.data_channel.send(frame.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    // Check if it's a buffer full error
                    if e.contains("buffer") || e.contains("full") || e.contains("would block") {
                        // Buffer became full, switch to queueing mode
                        self.can_send.store(false, Ordering::Release);
                        // Fall through to queueing
                    } else {
                        return Err(e); // Real error, not buffer full
                    }
                }
            }
        }

        // Slow path: queue for later when buffer drains
        {
            let mut pending = self.pending_frames.lock().unwrap();
            pending.push_back(frame);

            // Prevent unbounded growth
            if pending.len() > 1000 {
                // Drop oldest frames if queue grows too large
                pending.pop_front();
                warn!("Event-driven sender queue full, dropping old frame");
            }
        }

        Ok(()) // Queued successfully - no blocking!
    }

    /// Get queue depth for monitoring
    pub fn queue_depth(&self) -> usize {
        self.pending_frames.lock().unwrap().len()
    }

    /// Check if sender can send immediately (useful for monitoring)
    pub fn can_send_immediate(&self) -> bool {
        self.can_send.load(Ordering::Acquire)
    }

    /// Check if queue depth exceeds threshold (for monitoring/alerting)
    pub fn is_over_threshold(&self) -> bool {
        self.queue_depth() as u64 > self.threshold
    }

    /// Get the configured threshold for monitoring
    pub fn get_threshold(&self) -> u64 {
        self.threshold
    }
}
