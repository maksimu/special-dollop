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

// Error message constants to avoid repeated allocations in hot paths
const QUEUE_FULL_ERROR: &str = "Queue full - backpressure required (loss-intolerant protocol)";
const DATACHANNEL_CLOSED_ERROR: &str = "DataChannel closed";

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
    /// Notification for when data channel opens - allows multiple waiters without callback conflicts
    pub(crate) open_notify: Arc<tokio::sync::Notify>,
    /// Flag indicating if data channel is open - set once and never reset
    pub(crate) is_open: Arc<AtomicBool>,

    /// Early message buffer - captures messages arriving before handlers are ready.
    /// This is critical for on_data_channel callbacks where messages can arrive
    /// before setup_channel_for_data_channel completes (~100ms race window).
    /// Uses lock-free SegQueue for zero-contention message capture.
    pub(crate) early_message_buffer: Arc<SegQueue<Bytes>>,
    /// Count of messages in early buffer (for logging/debugging)
    pub(crate) early_message_count: Arc<AtomicUsize>,
    /// Flag indicating if early buffering is active (set to false once real handler takes over)
    pub(crate) early_buffering_active: Arc<AtomicBool>,

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
            open_notify: Arc::clone(&self.open_notify),
            is_open: Arc::clone(&self.is_open),
            early_message_buffer: Arc::clone(&self.early_message_buffer),
            early_message_count: Arc::clone(&self.early_message_count),
            early_buffering_active: Arc::clone(&self.early_buffering_active),

            #[cfg(test)]
            test_send_hook: Arc::clone(&self.test_send_hook),
        }
    }
}

impl WebRTCDataChannel {
    pub fn new(data_channel: Arc<RTCDataChannel>) -> Self {
        let open_notify = Arc::new(tokio::sync::Notify::new());
        let is_open = Arc::new(AtomicBool::new(false));

        // Early message buffering - captures messages before real handler is set up
        let early_message_buffer = Arc::new(SegQueue::new());
        let early_message_count = Arc::new(AtomicUsize::new(0));
        let early_buffering_active = Arc::new(AtomicBool::new(true));

        // Check if already open (for received data channels that may already be open)
        let already_open = data_channel.ready_state()
            == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open;
        if already_open {
            is_open.store(true, Ordering::Release);
            open_notify.notify_waiters();
        }

        // Set up on_open callback to notify waiters (this is the ONLY place on_open is set)
        let is_open_for_callback = Arc::clone(&is_open);
        let open_notify_for_callback = Arc::clone(&open_notify);
        data_channel.on_open(Box::new(move || {
            is_open_for_callback.store(true, Ordering::Release);
            open_notify_for_callback.notify_waiters();
            Box::pin(async {})
        }));

        // CRITICAL: Set up early message buffering IMMEDIATELY.
        // This captures messages that arrive before setup_channel_for_data_channel
        // completes (there can be a ~100ms race window in on_data_channel callbacks).
        let early_buffer_for_callback = Arc::clone(&early_message_buffer);
        let early_count_for_callback = Arc::clone(&early_message_count);
        let early_active_for_callback = Arc::clone(&early_buffering_active);
        let label_for_log = data_channel.label().to_string();
        data_channel.on_message(Box::new(move |msg| {
            let buffer = Arc::clone(&early_buffer_for_callback);
            let count = Arc::clone(&early_count_for_callback);
            let active = Arc::clone(&early_active_for_callback);
            let label = label_for_log.clone();
            let data = Bytes::copy_from_slice(&msg.data);

            Box::pin(async move {
                // Only buffer if early buffering is still active
                if active.load(Ordering::Acquire) {
                    let msg_count = count.fetch_add(1, Ordering::AcqRel) + 1;
                    buffer.push(data);
                    debug!(
                        "[EARLY_BUFFER] Captured early message #{} ({} bytes) on channel '{}' before handler ready",
                        msg_count, buffer.len(), label
                    );
                }
                // If early_buffering_active is false, this callback should have been
                // replaced by the real handler - but if not, the message is dropped
                // (this shouldn't happen in normal operation)
            })
        }));

        Self {
            data_channel,
            is_closing: Arc::new(AtomicBool::new(false)),
            buffered_amount_low_threshold: Arc::new(AtomicU64::new(0)),
            on_buffered_amount_low_callback: Arc::new(std::sync::Mutex::new(None)),
            threshold_monitor: Arc::new(AtomicBool::new(false)),
            open_notify,
            is_open,
            early_message_buffer,
            early_message_count,
            early_buffering_active,

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
                                debug!(
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

    /// Wait for the data channel to be open, with an optional timeout.
    /// Returns Ok(true) if channel opened, Ok(false) if closed/timeout, Err if closing.
    /// This is essential for server-mode channels to wait before accepting TCP connections.
    ///
    /// This uses a simple polling approach combined with the shared notification to be
    /// robust against callback conflicts. Even if callbacks are overwritten elsewhere,
    /// we'll still detect the open state via polling.
    pub async fn wait_for_channel_open(&self, timeout: Option<Duration>) -> Result<bool, String> {
        let timeout_duration = timeout.unwrap_or(Duration::from_secs(10));
        let poll_interval = Duration::from_millis(50);
        let deadline = tokio::time::Instant::now() + timeout_duration;
        let label = self.data_channel.label().to_string();

        debug!(
            "Waiting for data channel to open: {} (timeout: {:?})",
            label, timeout_duration
        );

        loop {
            let current_state = self.data_channel.ready_state();

            // Check if already closed (error case)
            if self.is_closing.load(Ordering::Acquire) {
                warn!("wait_for_channel_open CLOSING: channel={}", label);
                return Err("Data channel is closing".to_string());
            }

            // Check if open via our flag (set by on_open callback)
            if self.is_open.load(Ordering::Acquire) {
                return Ok(true);
            }

            // Also check native state (in case callback was overwritten)
            if current_state == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open
            {
                // Update our flag to match
                debug!(
                    "Data channel opened (detected via native state polling): {}",
                    label
                );
                self.is_open.store(true, Ordering::Release);
                self.open_notify.notify_waiters();
                return Ok(true);
            }

            // Check for timeout
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    "Data channel did not open within timeout ({:?}), final state: {:?} (channel: {})",
                    timeout_duration, current_state, label
                );
                return Ok(false);
            }

            // Use select to wait for either notification or poll interval
            // This combines event-driven and polling for robustness
            tokio::select! {
                _ = self.open_notify.notified() => {
                    // Notification received, check state again
                    continue;
                }
                _ = tokio::time::sleep(poll_interval) => {
                    // Poll interval, check state again
                    continue;
                }
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

    /// Take ownership of all early-buffered messages and disable early buffering.
    ///
    /// CRITICAL: This must be called by setup_channel_for_data_channel AFTER
    /// setting the real on_message handler. The correct order is:
    /// 1. Set new on_message handler (replaces early buffer callback)
    /// 2. Call take_early_messages() to drain buffer
    /// 3. Forward returned messages to the channel
    ///
    /// This order ensures no messages are lost:
    /// - Messages arriving after step 1 go directly to new handler
    /// - Messages that arrived before step 1 are returned by step 2
    ///
    /// After this call:
    /// - early_buffering_active is set to false
    /// - All buffered messages are returned
    ///
    /// # Returns
    /// Vec of early messages in arrival order, empty if none were buffered
    pub fn take_early_messages(&self) -> Vec<Bytes> {
        // Disable early buffering FIRST (atomic release)
        self.early_buffering_active.store(false, Ordering::Release);

        // Drain all buffered messages
        let mut messages = Vec::new();
        while let Some(msg) = self.early_message_buffer.pop() {
            messages.push(msg);
        }

        let count = self.early_message_count.load(Ordering::Acquire);
        if !messages.is_empty() {
            log::info!(
                "[EARLY_BUFFER] Drained {} early messages from channel '{}' (total captured: {})",
                messages.len(),
                self.label(),
                count
            );
        }

        messages
    }
}

/// Retry state for exponential backoff
/// Uses atomic timestamp (nanoseconds since epoch) for lock-free access
struct RetryState {
    /// Number of retry attempts
    attempts: AtomicUsize,
    /// Timestamp of last retry attempt (nanoseconds since Unix epoch, 0 = None)
    last_retry_ns: AtomicU64,
}

impl RetryState {
    fn new() -> Self {
        Self {
            attempts: AtomicUsize::new(0),
            last_retry_ns: AtomicU64::new(0), // 0 = None
        }
    }

    // Note: get_backoff_delay() reserved for future use when implementing
    // actual retry with backoff delays. Currently retries happen via queue.

    /// Record a retry attempt (lock-free)
    fn record_retry(&self) {
        self.attempts.fetch_add(1, Ordering::Release);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        self.last_retry_ns.store(now_ns, Ordering::Release);
    }

    /// Reset retry state on success (lock-free)
    fn reset(&self) {
        self.attempts.store(0, Ordering::Release);
        self.last_retry_ns.store(0, Ordering::Release); // 0 = None
    }

    /// Get current attempt count (lock-free)
    fn get_attempts(&self) -> usize {
        self.attempts.load(Ordering::Acquire)
    }

    /// Get elapsed time since last retry (lock-free)
    fn get_last_retry_elapsed(&self) -> Option<Duration> {
        let last_ns = self.last_retry_ns.load(Ordering::Acquire);
        if last_ns == 0 {
            return None;
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        if now_ns >= last_ns {
            Some(Duration::from_nanos(now_ns - last_ns))
        } else {
            None // Clock went backwards
        }
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

    // Queue monitoring for alerts (lock-free atomics)
    /// Timestamp when queue first exceeded 75% threshold (nanoseconds since Unix epoch, 0 = None)
    high_queue_start_ns: Arc<AtomicU64>,
    /// Count of frames that were blocked (queue full) - for metrics
    frames_blocked: Arc<AtomicUsize>,

    // Retry state for exponential backoff
    retry_state: Arc<RetryState>,
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
            high_queue_start_ns: Arc::new(AtomicU64::new(0)), // 0 = None
            frames_blocked: Arc::new(AtomicUsize::new(0)),
            retry_state: Arc::new(RetryState::new()),
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
                // Note: retry_state not accessible here (would need Arc in closure)
                // Retry logic handled at send_with_natural_backpressure level

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
    pub async fn send_with_natural_backpressure(&self, frame: Bytes) -> Result<(), &'static str> {
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
                            debug!(
                                "WebRTC permanently closed, failing send (frame_size: {} bytes)",
                                frame_len
                            );
                        }
                        return Err(DATACHANNEL_CLOSED_ERROR);
                    }

                    // Temporary failure (buffer full, congestion) - queue for retry
                    // Record retry attempt for exponential backoff tracking
                    self.retry_state.record_retry();
                    let attempts = self.retry_state.get_attempts();

                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!(
                            "WebRTC send failed temporarily (frame_size: {} bytes, retry_attempt: {}), queueing for retry. Error: {}",
                            frame_len, attempts, e
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
        // CRITICAL: Never drop frames for loss-intolerant protocols (RDP/SSH/SFTP)
        // Return error to trigger backpressure instead (caller pauses reads)
        if current_queue_size >= 10000 {
            // Track blocked frames for metrics
            self.frames_blocked.fetch_add(1, Ordering::Relaxed);

            warn!(
                "EventDrivenSender queue FULL - backpressure required (queue_size: {}, threshold: {}, frame_bytes: {})",
                current_queue_size,
                self.threshold,
                frame_len
            );
            // Return error to trigger backpressure - caller will pause reads
            // This prevents frame loss for loss-intolerant protocols
            return Err(QUEUE_FULL_ERROR);
        }

        // Queue monitoring: Track sustained high queue pressure for alerts (lock-free)
        const HIGH_QUEUE_THRESHOLD: usize = 7500; // 75% of max capacity
        const SUSTAINED_PRESSURE_DURATION: Duration = Duration::from_secs(30);

        if current_queue_size > HIGH_QUEUE_THRESHOLD {
            // Track when queue first exceeded threshold (lock-free compare-and-swap)
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;

            // Only set if currently 0 (None) - lock-free initialization
            let _ = self.high_queue_start_ns.compare_exchange(
                0,
                now_ns,
                Ordering::AcqRel,
                Ordering::Acquire,
            );

            // Check if sustained pressure for alert threshold (lock-free read)
            let start_ns = self.high_queue_start_ns.load(Ordering::Acquire);
            if start_ns != 0 {
                let elapsed = Duration::from_nanos(now_ns.saturating_sub(start_ns));
                if elapsed >= SUSTAINED_PRESSURE_DURATION {
                    warn!(
                        "Sustained queue pressure detected: {}/10000 frames ({:.1}% full) for {:?} - network may be degraded",
                        current_queue_size,
                        (current_queue_size as f64 / 10000.0) * 100.0,
                        elapsed
                    );
                    // Record metrics for sustained pressure (non-blocking)
                    // Note: conversation_id not available here, will be tracked at channel level
                }
            }

            // Log warning at 75% threshold
            // Re-check queue size to avoid stale warnings if it dropped during processing
            let final_queue_size = self.queue_size.load(Ordering::Acquire);
            if final_queue_size > HIGH_QUEUE_THRESHOLD {
                warn!(
                    "EventDrivenSender queue critically high: {}/10000 frames ({:.1}% full) - backpressure active",
                    final_queue_size,
                    (final_queue_size as f64 / 10000.0) * 100.0
                );
            }
        } else {
            // Queue below threshold - reset tracking (lock-free)
            self.high_queue_start_ns.store(0, Ordering::Release); // 0 = None

            // Log at 50% threshold (every 500 frames for monitoring)
            if current_queue_size > 5000
                && current_queue_size.is_multiple_of(500)
                && unlikely!(crate::logger::is_verbose_logging())
            {
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

        // Reset retry state on successful queue (frame will be sent when buffer drains)
        self.retry_state.reset();

        Ok(()) // Queued successfully - no blocking!
    }

    /// Get retry statistics for monitoring (lock-free)
    #[allow(dead_code)] // Reserved for future metrics API
    pub fn get_retry_stats(&self) -> (usize, Option<Duration>) {
        let attempts = self.retry_state.get_attempts();
        let last_retry = self.retry_state.get_last_retry_elapsed();
        (attempts, last_retry)
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

    /// Get count of frames that were blocked (queue full) - for metrics
    #[allow(dead_code)] // Reserved for future metrics API
    pub fn get_frames_blocked(&self) -> usize {
        self.frames_blocked.load(Ordering::Acquire)
    }

    /// Check if queue has been high for sustained period (for alerts) - lock-free
    #[allow(dead_code)] // Reserved for future metrics API
    pub fn has_sustained_pressure(&self) -> bool {
        let start_ns = self.high_queue_start_ns.load(Ordering::Acquire);
        if start_ns == 0 {
            return false;
        }
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        if now_ns >= start_ns {
            Duration::from_nanos(now_ns - start_ns) >= Duration::from_secs(30)
        } else {
            false // Clock went backwards
        }
    }
}
