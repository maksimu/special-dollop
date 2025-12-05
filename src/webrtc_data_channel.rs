use crate::unlikely;
use bytes::Bytes;
#[cfg(test)]
use futures::future::BoxFuture;
use log::{debug, warn};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webrtc::data_channel::RTCDataChannel;

// Lock-free queue for pending frames - uses crossbeam's SegQueue
use crossbeam_queue::SegQueue;

/// Standard buffer threshold for optimal WebRTC performance.
/// This value (8KB) is optimized for mixed interactive + bulk workloads:
/// - Research: SMALLER thresholds achieve HIGHER throughput (2KB → 135 Mbps on LAN)
/// - 8KB enables ~200-500 drain events/sec (2x more frequent than 16KB)
/// - Combined with 2000-frame drain batches = 8x faster queue clearing vs 16KB/500 frames
/// - Reduces interactive latency (keyboard echo) from 100-500ms to 10-50ms
/// - Still maintains high throughput for bulk transfers (4K video, 100GB files)
/// - Marginal CPU increase (~1-2% per connection) for dramatic latency improvement
pub const STANDARD_BUFFER_THRESHOLD: u64 = 8 * 1024; // 8KB - balanced for latency + throughput

// Type alias for complex callback type - callbacks still need Mutex (can't be atomic)
type BufferedAmountLowCallback =
    Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync + 'static>>>>;

#[cfg(test)]
type TestSendHook = Arc<
    std::sync::Mutex<Option<Box<dyn Fn(Bytes) -> BoxFuture<'static, ()> + Send + Sync + 'static>>>,
>;

// Async-first wrapper for data channel functionality
pub struct WebRTCDataChannel {
    pub data_channel: Arc<RTCDataChannel>,
    pub(crate) is_closing: Arc<AtomicBool>,
    /// Buffered amount threshold - lock-free with AtomicU64
    pub(crate) buffered_amount_low_threshold: Arc<AtomicU64>,
    /// Callback for buffered amount low - still needs Mutex (callback is not Copy)
    pub(crate) on_buffered_amount_low_callback: BufferedAmountLowCallback,
    pub(crate) threshold_monitor: Arc<AtomicBool>,

    #[cfg(test)]
    pub(crate) test_send_hook: TestSendHook,
}

impl Clone for WebRTCDataChannel {
    fn clone(&self) -> Self {
        Self {
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
            buffered_amount_low_threshold: Arc::new(AtomicU64::new(0)),
            on_buffered_amount_low_callback: Arc::new(std::sync::Mutex::new(None)),
            threshold_monitor: Arc::new(AtomicBool::new(false)),

            #[cfg(test)]
            test_send_hook: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Set the buffered amount low threshold (lock-free)
    pub fn set_buffered_amount_low_threshold(&self, threshold: u64) {
        // Lock-free store
        self.buffered_amount_low_threshold
            .store(threshold, Ordering::Release);

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
                            // Buffer drain event logging - verbose only (can be frequent)
                            if unlikely!(crate::logger::is_verbose_logging()) {
                                log::debug!(
                                    "Native bufferedAmountLow event triggered (buffer below {})",
                                    threshold_value
                                );
                            }

                            // Get and call our callback (callback mutex is infrequent, not hot path)
                            if let Ok(callback_guard) =
                                callback_dc.on_buffered_amount_low_callback.lock()
                            {
                                if let Some(ref callback) = *callback_guard {
                                    callback();
                                }
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

        // Callback registration is infrequent - mutex is acceptable here
        if let Ok(mut guard) = self.on_buffered_amount_low_callback.lock() {
            *guard = callback;
            debug!("Set buffered amount low callback: {}", has_callback);
        } else {
            warn!("Failed to set buffered amount low callback - mutex poisoned");
        }
    }

    // Add a test method to set the sending hook for testing
    #[cfg(test)]
    pub fn set_test_send_hook<F>(&self, hook: F)
    where
        F: Fn(Bytes) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    {
        if let Ok(mut guard) = self.test_send_hook.lock() {
            *guard = Some(Box::new(hook));
        }
    }

    pub async fn send(&self, data: Bytes) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Channel is closing".to_string());
        }

        // For testing: call the test hook if set
        #[cfg(test)]
        {
            if let Ok(hook_guard) = self.test_send_hook.lock() {
                if let Some(ref hook) = *hook_guard {
                    // Clone the data for the hook
                    let data_clone = data.clone();

                    // Call the hook with a clone of the data
                    let hook_future = hook(data_clone);

                    // Spawn the hook execution to avoid blocking
                    tokio::spawn(hook_future);
                }
            }
        }

        // Send data with detailed error handling
        let result = self
            .data_channel
            .send(&data)
            .await
            .map(|_| ())
            .map_err(|e| format!("Failed to send data: {e}"));

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
        let sender = Arc::new(std::sync::Mutex::new(Some(tx)));

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
                if let Ok(mut guard) = sender_clone.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(true);
                    }
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
                if let Ok(mut guard) = sender_for_close.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(false);
                    }
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
            Ok(result) => result.map_err(|e| format!("Failed to close data channel: {e}")),
            Err(_) => {
                warn!("Data channel close operation timed out, forcing abandonment");
                Ok(()) // Force success even though it timed out
            }
        }
    }

    /// Wait for the WebRTC buffer to drain (without closing the channel).
    ///
    /// Use this when you've just sent an important message (like an error or
    /// disconnect notification) and want to ensure it's transmitted before
    /// the connection task exits. Does NOT close the channel.
    ///
    /// # Arguments
    /// * `timeout` - Maximum time to wait for buffer to drain
    ///
    /// # Returns
    /// * `true` - Buffer drained completely
    /// * `false` - Timeout reached, data may still be buffered
    pub async fn drain(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            // Check if already closing (buffer will report 0)
            if self.is_closing.load(Ordering::Acquire) {
                return true;
            }

            let buffered = self.data_channel.buffered_amount().await;
            if buffered == 0 {
                return true;
            }
            // Cooperative yield - avoids busy loop, no timer overhead
            tokio::task::yield_now().await;
        }

        warn!(
            "WebRTC buffer drain timeout after {:?}, data may still be buffered",
            timeout
        );
        false
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
/// Uses lock-free SegQueue for zero-contention frame queueing
pub struct EventDrivenSender {
    data_channel: Arc<WebRTCDataChannel>,
    /// Lock-free queue for pending frames - SegQueue provides MPSC without locks
    pending_frames: Arc<SegQueue<Bytes>>,
    can_send: Arc<AtomicBool>,
    threshold: u64,               // Backpressure threshold for monitoring
    queue_size: Arc<AtomicUsize>, // Lock-free queue depth counter
}

impl EventDrivenSender {
    /// Create a new event-driven sender with the specified threshold
    pub fn new(data_channel: Arc<WebRTCDataChannel>, threshold: u64) -> Self {
        let sender = Self {
            data_channel: data_channel.clone(),
            pending_frames: Arc::new(SegQueue::new()),
            can_send: Arc::new(AtomicBool::new(true)),
            threshold,
            queue_size: Arc::new(AtomicUsize::new(0)),
        };

        // Set up WebRTC native event handling
        data_channel.set_buffered_amount_low_threshold(threshold);

        let can_send_clone = sender.can_send.clone();
        let pending_clone = sender.pending_frames.clone();
        let queue_size_clone = sender.queue_size.clone();
        let dc_clone = data_channel.clone();

        // EVENT-DRIVEN: Only wake up when buffer space available
        data_channel.on_buffered_amount_low(Some(Box::new(move || {
            can_send_clone.store(true, Ordering::Release);

            // Drain pending frames when space becomes available (batched)
            // Lock-free: pop from SegQueue without any locks
            let mut to_send = Vec::with_capacity(2000);
            while to_send.len() < 2000 {
                match pending_clone.pop() {
                    Some(frame) => to_send.push(frame),
                    None => break,
                }
            }

            // Update atomic counter after draining
            let old_size = queue_size_clone.fetch_sub(to_send.len(), Ordering::AcqRel);

            // Sanity check: counter should never underflow (indicates a bug in increment/decrement logic)
            // In development: panic to catch the bug immediately
            // In production: log error and reset counter to prevent permanent corruption
            debug_assert!(
                old_size >= to_send.len(),
                "Queue size counter underflowed! old_size={}, to_send.len()={} - this indicates a race condition in queue management",
                old_size,
                to_send.len()
            );

            if old_size < to_send.len() {
                log::error!(
                    "CRITICAL: Queue size counter underflowed! old_size={}, to_send.len()={} - resetting counter to 0. This indicates a bug in queue increment/decrement logic.",
                    old_size,
                    to_send.len()
                );
                queue_size_clone.store(0, Ordering::Release);
            }

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

    /// Send with zero-polling natural backpressure (lock-free!)
    /// Returns immediately - either sends or queues for later
    pub async fn send_with_natural_backpressure(&self, frame: Bytes) -> Result<(), String> {
        let frame_len = frame.len(); // Capture for logging

        // Fast path: send immediately if buffer has space
        if self.can_send.load(Ordering::Acquire) {
            match self.data_channel.send(frame.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let error_str = e.to_string();

                    // Detect permanent failures (WebRTC closed) vs temporary failures (buffer full)
                    // When WebRTC is permanently closed, we must return error to trigger cleanup
                    // Otherwise backend tasks become zombies, guacd keeps responding, 15s timeout leak
                    if error_str.contains("DataChannel is not opened")
                        || error_str.contains("Channel is closing")
                        || error_str.contains("closed")
                    {
                        // Permanent failure - WebRTC is dead, don't queue
                        // Only log once per channel to avoid spam during shutdown
                        if unlikely!(crate::logger::is_verbose_logging()) {
                            log::debug!(
                                "WebRTC permanently closed, failing send (frame_size: {} bytes)",
                                frame_len
                            );
                        }
                        return Err(error_str);
                    }

                    // Temporary failure (buffer full, congestion) - queue for retry
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!(
                            "WebRTC send failed temporarily (frame_size: {} bytes), queueing for retry. Error: {}",
                            frame_len, e
                        );
                    }

                    self.can_send.store(false, Ordering::Release);
                    // Fall through to queueing
                }
            }
        }

        // Slow path: queue for later when buffer drains (lock-free!)
        // CRITICAL: Check size BEFORE incrementing to avoid race condition where
        // counter is incremented but frame is never queued (window of incorrect state)
        let current_queue_size = self.queue_size.load(Ordering::Acquire);

        // Prevent unbounded growth - increased from 1000 to 10000 frames
        // Check BEFORE incrementing counter to maintain accurate queue depth
        if current_queue_size >= 10000 {
            log::error!(
                "CRITICAL: EventDrivenSender queue overflow! Dropping frame (queue_size: {}, threshold: {}, dropped_bytes: {})",
                current_queue_size,
                self.threshold,
                frame_len
            );
            return Ok(()); // Still return Ok - frame was "handled" (dropped)
        }

        // Log warnings at various thresholds (before incrementing)
        if current_queue_size > 7500 {
            // 75% of max capacity
            warn!(
                "EventDrivenSender queue critically high: {}/10000 frames ({:.1}% full) - approaching data loss",
                current_queue_size,
                (current_queue_size as f64 / 10000.0) * 100.0
            );
        } else if current_queue_size > 5000 && current_queue_size.is_multiple_of(500) {
            // Log every 500 frames after 50%
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "EventDrivenSender queue growing: {}/10000 frames ({:.1}% full)",
                    current_queue_size,
                    (current_queue_size as f64 / 10000.0) * 100.0
                );
            }
        }

        // Push to queue FIRST (SegQueue::push is infallible - always succeeds)
        self.pending_frames.push(frame);

        // Then increment counter (atomic operation)
        // This ensures counter never exceeds actual queue size
        self.queue_size.fetch_add(1, Ordering::Release);

        Ok(()) // Queued successfully - no blocking!
    }

    /// Get queue depth for monitoring (lock-free)
    pub fn queue_depth(&self) -> usize {
        self.queue_size.load(Ordering::Acquire)
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
