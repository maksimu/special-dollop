use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use bytes::Bytes;
use webrtc::data_channel::RTCDataChannel;

// Async-first wrapper for data channel functionality
pub struct WebRTCDataChannel {
    pub data_channel: Arc<RTCDataChannel>,
    is_closing: Arc<AtomicBool>,
    buffered_amount_low_threshold: Arc<Mutex<u64>>,
    on_buffered_amount_low_callback: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync + 'static>>>>,
    threshold_monitor: Arc<AtomicBool>,
}

impl Clone for WebRTCDataChannel {
    fn clone(&self) -> Self {
        WebRTCDataChannel {
            data_channel: Arc::clone(&self.data_channel),
            is_closing: Arc::clone(&self.is_closing),
            buffered_amount_low_threshold: Arc::clone(&self.buffered_amount_low_threshold),
            on_buffered_amount_low_callback: Arc::clone(&self.on_buffered_amount_low_callback),
            threshold_monitor: Arc::clone(&self.threshold_monitor),
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
        }
    }

    /// Create a fake data channel for testing or initialization
    pub fn dummy() -> Self {
        panic!("WebRTCDataChannel::dummy should only be used for testing or initialization and should not be called directly")
    }

    /// Set the buffered amount low threshold
    pub fn set_buffered_amount_low_threshold(&self, threshold: u64) {
        let mut guard = self.buffered_amount_low_threshold.lock().unwrap();
        *guard = threshold;

        // Log the threshold change
        log::debug!("Set buffered amount low threshold to {} bytes", threshold);

        // Start monitoring if we're already above the threshold
        if threshold > 0 {
            let threshold_monitor = self.threshold_monitor.clone();
            let dc = self.clone();

            // Spawn a task to check the current buffer status
            tokio::spawn(async move {
                // Get the current buffer amount
                let current = dc.data_channel.buffered_amount().await as u64;

                // If we're already above the threshold, start monitoring
                if current > threshold && !threshold_monitor.load(Ordering::Acquire) {
                    dc.start_threshold_monitor(current).await;
                }
            });
        }
    }

    /// Get the current buffered amount low threshold
    pub fn get_buffered_amount_low_threshold(&self) -> u64 {
        let guard = self.buffered_amount_low_threshold.lock().unwrap();
        *guard
    }

    /// Set the callback to be called when the buffered amount drops below the threshold
    pub fn on_buffered_amount_low(&self, callback: Option<Box<dyn Fn() + Send + Sync + 'static>>) {
        // Check is_some() before moving the callback
        let has_callback = callback.is_some();

        // Now move it into the mutex
        let mut guard = self.on_buffered_amount_low_callback.lock().unwrap();
        *guard = callback;

        log::debug!("Set buffered amount low callback: {}", has_callback);
    }

    /// Start monitoring the buffer level in the background with improved efficiency
    async fn start_threshold_monitor(&self, initial_amount: u64) {
        // Only start if not already monitoring
        if self.threshold_monitor.swap(true, Ordering::AcqRel) {
            return; // Already monitoring
        }

        // Get the threshold value
        let threshold = {
            let guard = self.buffered_amount_low_threshold.lock().unwrap();
            *guard
        };

        // If the threshold is 0, or we're already below it, don't start monitoring
        if threshold == 0 || initial_amount <= threshold {
            self.threshold_monitor.store(false, Ordering::Release);
            return;
        }

        log::debug!("Starting buffer threshold monitor: current={}, threshold={}", 
                   initial_amount, threshold);

        // Clone what we need for the task
        let dc = self.clone();

        // Spawn a task to monitor the buffer
        tokio::spawn(async move {
            // Start with a short interval and exponentially back off as the buffer drains more slowly
            let mut check_interval = Duration::from_millis(20);
            let max_interval = Duration::from_millis(200);
            let mut last_amount = initial_amount;
            let mut consecutive_checks_with_no_progress = 0;

            loop {
                // Exit early if the channel is closing
                if dc.is_closing.load(Ordering::Acquire) {
                    break;
                }

                // Check the current buffer amount
                let current = dc.data_channel.buffered_amount().await as u64;
                let threshold = {
                    let guard = dc.buffered_amount_low_threshold.lock().unwrap();
                    *guard
                };

                // Calculate drain rate to adapt polling frequency
                let drained = if current < last_amount {
                    last_amount - current
                } else {
                    0
                };

                // If draining is very slow or stalled, increase a check interval
                if drained < 1024 { // Less than 1KB has been drained since the last check
                    consecutive_checks_with_no_progress += 1;

                    // Exponentially back off to reduce CPU usage
                    if consecutive_checks_with_no_progress > 3 {
                        check_interval = std::cmp::min(check_interval * 2, max_interval);
                    }
                } else {
                    // Reset counter on progress
                    consecutive_checks_with_no_progress = 0;

                    // Decrease the interval if draining quickly
                    if drained > 64 * 1024 && check_interval > Duration::from_millis(20) {
                        check_interval = Duration::from_millis(20);
                    }
                }

                last_amount = current;

                // If we've dropped below the threshold, trigger callback
                if current <= threshold {
                    log::debug!("Buffer amount is now below threshold: {} <= {}", 
                               current, threshold);

                    // Get and call the callback if set
                    let callback_guard = dc.on_buffered_amount_low_callback.lock().unwrap();
                    if let Some(ref callback) = *callback_guard {
                        callback();
                    }

                    // We're done monitoring until the next call to send
                    break;
                }

                // Periodically log buffer levels
                if consecutive_checks_with_no_progress % 10 == 0 && consecutive_checks_with_no_progress > 0 {
                    log::debug!("Buffer draining slowly: amount={}, threshold={}, check_interval={}ms",
                              current, threshold, check_interval.as_millis());
                }

                // Wait before checking again
                tokio::time::sleep(check_interval).await;
            }

            // Mark monitoring as done
            dc.threshold_monitor.store(false, Ordering::Release);
            log::debug!("Buffer threshold monitor stopped");
        });
    }

    pub async fn send(&self, data: Bytes) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Channel is closing".to_string());
        }

        let before_send = self.data_channel.buffered_amount().await as u64;

        // Send data with detailed error handling
        let result = self.data_channel
            .send(&data)
            .await
            .map(|_| ())
            .map_err(|e| format!("Failed to send data: {}", e));

        // After sending, check if we need to start monitoring the buffer
        if result.is_ok() {
            // Get the current threshold
            let threshold = {
                let guard = self.buffered_amount_low_threshold.lock().unwrap();
                *guard
            };

            // Only start monitoring if the threshold is non-zero
            if threshold > 0 {
                // Get the current buffered amount after sending
                let after_send = self.data_channel.buffered_amount().await as u64;

                // If we've crossed above the threshold, start monitoring
                if before_send <= threshold && after_send > threshold {
                    log::debug!("Buffer crossed threshold after send: {} > {}", after_send, threshold);
                    self.start_threshold_monitor(after_send).await;
                }
            }
        }

        result
    }

    pub async fn buffered_amount(&self) -> u64 {
        // Early return if the channel is closing
        if self.is_closing.load(Ordering::Acquire) {
            return 0;
        }

        self.data_channel.buffered_amount().await as u64
    }

    pub async fn wait_for_channel_open(&self, timeout: Option<Duration>) -> Result<bool, String> {
        let timeout_duration = timeout.unwrap_or(Duration::from_secs(10)); // Reduced from 30 s to 10 s
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(50); // Reduced from 100 ms to 50 ms

        while start.elapsed() < timeout_duration {
            // Check for closing early
            if self.is_closing.load(Ordering::Acquire) {
                return Err("Data channel is closing".to_string());
            }

            // Check if already open (the most common case first)
            if self.data_channel.ready_state()
                == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open
            {
                return Ok(true);
            }

            // Check if the channel failed (the second most common case)
            if self.data_channel.ready_state()
                == webrtc::data_channel::data_channel_state::RTCDataChannelState::Closed
            {
                return Ok(false);
            }

            // Wait before the next check with a shorter interval
            tokio::time::sleep(check_interval).await;
        }

        Ok(false)
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
                log::warn!("Data channel close operation timed out, forcing abandonment");
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