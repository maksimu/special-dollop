//! Adaptive Channel Pool for WebRTC Data Channel Management
//!
//! This module provides a fill-then-overflow strategy for managing WebRTC data channels:
//! 1. Primary channel handles all traffic until its buffer is full
//! 2. When primary is saturated, traffic spills to overflow channel(s)
//! 3. Overflow channels are created on-demand when needed
//! 4. Idle overflow channels are closed after a timeout (keeping one standby)
//!
//! # Architecture
//!
//! ```text
//! +------------------------------------------------------------------+
//! |                     AdaptiveChannelPool                          |
//! |                                                                  |
//! |  +-------------+   buffer full   +-------------+                 |
//! |  |   Primary   | --------------> |  Overflow   | (created on     |
//! |  |   Channel   |                 |  Channel    |  demand)        |
//! |  |  (always)   | <-------------- |  (standby)  |                 |
//! |  +-------------+  buffer drained +-------------+                 |
//! |        |                               |                         |
//! |        |  idle timeout (5s)            v                         |
//! |        |                        [Close if idle]                  |
//! |        |                        [Keep 1 standby]                 |
//! |        |                                                         |
//! |        +------------- Normal traffic --------------------------> |
//! +------------------------------------------------------------------+
//! ```

// Allow dead_code at module level - items are used at runtime through Option<AdaptiveChannelPool>
// indirection that Rust's static analysis cannot trace. Pool is conditionally created when
// ADAPTIVE_CHANNELS capability is enabled.
#![allow(dead_code)]
#![allow(clippy::type_complexity)]

use bytes::Bytes;
use log::{debug, info, warn};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::webrtc_data_channel::{WebRTCDataChannel, STANDARD_BUFFER_THRESHOLD};

/// Statistics for a single overflow channel
/// Used by get_overflow_channel_stats() for monitoring/dashboards
#[derive(Debug, Clone)]
#[allow(dead_code)] // Constructed by get_stats(), exposed via get_overflow_channel_stats()
pub struct OverflowChannelStats {
    /// Channel label/identifier
    pub label: String,
    /// Total bytes sent through this channel
    pub bytes_sent: u64,
    /// Total messages sent through this channel
    pub messages_sent: u64,
    /// When this channel was created
    pub created_at: Instant,
    /// Last time this channel was used for sending
    pub last_used: Instant,
    /// Current buffered amount (bytes waiting to be sent)
    pub buffered_amount: u64,
    /// Whether this channel is currently the active overflow target
    pub is_active: bool,
}

/// Pool-level statistics for monitoring
/// Exposed via get_stats() for dashboards and debugging
#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    /// Total overflow channels created since pool initialization
    pub total_channels_created: u64,
    /// Total overflow channels closed since pool initialization
    #[allow(dead_code)] // Updated in cleanup_idle_channels, read via get_stats()
    pub total_channels_closed: u64,
    /// Number of times traffic spilled from primary to overflow
    pub overflow_events: u64,
    /// Current number of active overflow channels (excluding primary)
    pub active_overflow_count: usize,
    /// Total bytes sent through primary channel
    pub primary_bytes_sent: u64,
    /// Total bytes sent through all overflow channels
    pub overflow_bytes_sent: u64,
    /// Peak concurrent overflow channels
    pub peak_overflow_count: usize,
    /// Total time spent in overflow mode (milliseconds)
    pub total_overflow_time_ms: u64,
}

/// Configuration for the adaptive channel pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Buffer threshold to trigger overflow (bytes)
    pub buffer_threshold: u64,
    /// How long an overflow channel can be idle before closing (seconds)
    #[allow(dead_code)] // Used by cleanup_idle_channels when start_cleanup_task is called
    pub idle_timeout_secs: u64,
    /// Maximum number of overflow channels to keep open
    pub max_overflow_channels: usize,
    /// Minimum overflow channels to keep ready (standby)
    #[allow(dead_code)] // Used by cleanup_idle_channels when start_cleanup_task is called
    pub min_standby_channels: usize,
    /// How often to check for idle channels (seconds)
    #[allow(dead_code)] // Used by start_cleanup_task
    pub cleanup_interval_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            buffer_threshold: STANDARD_BUFFER_THRESHOLD,
            idle_timeout_secs: 5,
            max_overflow_channels: 4,
            min_standby_channels: 1,
            cleanup_interval_secs: 2,
        }
    }
}

/// An overflow channel with tracking metadata
struct OverflowChannel {
    channel: Arc<WebRTCDataChannel>,
    bytes_sent: AtomicU64,
    messages_sent: AtomicU64,
    #[allow(dead_code)] // Used by get_stats() for monitoring
    created_at: Instant,
    last_used: RwLock<Instant>,
    is_active: AtomicBool,
}

impl OverflowChannel {
    fn new(channel: Arc<WebRTCDataChannel>) -> Self {
        let now = Instant::now();
        Self {
            channel,
            bytes_sent: AtomicU64::new(0),
            messages_sent: AtomicU64::new(0),
            created_at: now,
            last_used: RwLock::new(now),
            is_active: AtomicBool::new(false),
        }
    }

    #[allow(dead_code)] // Used by get_overflow_channel_stats() for monitoring
    async fn get_stats(&self) -> OverflowChannelStats {
        OverflowChannelStats {
            label: self.channel.label(),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            created_at: self.created_at,
            last_used: *self.last_used.read().await,
            buffered_amount: self.channel.buffered_amount().await,
            is_active: self.is_active.load(Ordering::Relaxed),
        }
    }

    async fn send(&self, data: Bytes) -> Result<(), String> {
        let len = data.len() as u64;
        let result = self.channel.send(data).await;

        if result.is_ok() {
            self.bytes_sent.fetch_add(len, Ordering::Relaxed);
            self.messages_sent.fetch_add(1, Ordering::Relaxed);
            *self.last_used.write().await = Instant::now();
        }

        result
    }

    #[allow(dead_code)] // Used by cleanup_idle_channels when start_cleanup_task is called
    async fn is_idle(&self, timeout: Duration) -> bool {
        let last_used = *self.last_used.read().await;
        last_used.elapsed() > timeout
    }

    async fn buffered_amount(&self) -> u64 {
        self.channel.buffered_amount().await
    }
}

/// Adaptive channel pool that manages primary + overflow channels
pub struct AdaptiveChannelPool {
    /// Primary channel (always present)
    primary: Arc<WebRTCDataChannel>,
    /// Overflow channels (created on demand)
    overflow_channels: RwLock<Vec<Arc<OverflowChannel>>>,
    /// Pool configuration
    config: PoolConfig,
    /// Whether we're currently in overflow mode
    in_overflow_mode: AtomicBool,
    /// When we entered overflow mode (for stats)
    overflow_mode_start: RwLock<Option<Instant>>,
    /// Pool-level statistics
    stats: RwLock<PoolStats>,
    /// Channel ID for logging
    channel_id: String,
    /// Factory for creating new overflow channels
    channel_factory: RwLock<
        Option<
            Box<
                dyn Fn() -> futures::future::BoxFuture<
                        'static,
                        Result<Arc<WebRTCDataChannel>, String>,
                    > + Send
                    + Sync,
            >,
        >,
    >,
    /// Cleanup task handle
    cleanup_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
    /// Primary channel stats
    primary_bytes_sent: AtomicU64,
    primary_messages_sent: AtomicU64,
}

impl AdaptiveChannelPool {
    /// Create a new adaptive channel pool with the primary channel
    pub fn new(primary: Arc<WebRTCDataChannel>, config: PoolConfig, channel_id: String) -> Self {
        Self {
            primary,
            overflow_channels: RwLock::new(Vec::new()),
            config,
            in_overflow_mode: AtomicBool::new(false),
            overflow_mode_start: RwLock::new(None),
            stats: RwLock::new(PoolStats::default()),
            channel_id,
            channel_factory: RwLock::new(None),
            cleanup_task: RwLock::new(None),
            primary_bytes_sent: AtomicU64::new(0),
            primary_messages_sent: AtomicU64::new(0),
        }
    }

    /// Set the factory function for creating overflow channels
    #[allow(dead_code)] // Called when configuring pool for dynamic channel creation
    pub async fn set_channel_factory<F>(&self, factory: F)
    where
        F: Fn() -> futures::future::BoxFuture<'static, Result<Arc<WebRTCDataChannel>, String>>
            + Send
            + Sync
            + 'static,
    {
        *self.channel_factory.write().await = Some(Box::new(factory));
    }

    /// Start the background cleanup task for idle overflow channels
    #[allow(dead_code)] // Called after pool creation to enable idle channel cleanup
    pub async fn start_cleanup_task(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        let interval = Duration::from_secs(pool.config.cleanup_interval_secs);

        let task = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            loop {
                interval_timer.tick().await;
                pool.cleanup_idle_channels().await;
            }
        });

        *self.cleanup_task.write().await = Some(task);
        debug!(
            "AdaptiveChannelPool({}): Started cleanup task (interval: {}s)",
            self.channel_id, self.config.cleanup_interval_secs
        );
    }

    /// Send data through the pool, using overflow if primary is saturated
    pub async fn send(&self, data: Bytes) -> Result<(), String> {
        let data_len = data.len() as u64;

        // Check primary channel buffer
        let primary_buffered = self.primary.buffered_amount().await;

        if primary_buffered < self.config.buffer_threshold {
            // Primary has space - use it
            self.exit_overflow_mode().await;

            let result = self.primary.send(data).await;
            if result.is_ok() {
                self.primary_bytes_sent
                    .fetch_add(data_len, Ordering::Relaxed);
                self.primary_messages_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.write().await.primary_bytes_sent += data_len;
            }
            return result;
        }

        // Primary is full - enter overflow mode
        self.enter_overflow_mode().await;

        // Find or create an overflow channel with space
        let overflow = self.get_available_overflow().await?;

        let result = overflow.send(data).await;
        if result.is_ok() {
            self.stats.write().await.overflow_bytes_sent += data_len;
        }

        result
    }

    /// Get an overflow channel with available buffer space, creating one if needed
    async fn get_available_overflow(&self) -> Result<Arc<OverflowChannel>, String> {
        let channels = self.overflow_channels.read().await;

        // First, try to find an existing channel with space
        for channel in channels.iter() {
            let buffered = channel.buffered_amount().await;
            if buffered < self.config.buffer_threshold {
                channel.is_active.store(true, Ordering::Relaxed);
                return Ok(Arc::clone(channel));
            }
        }

        // All overflow channels are full - need to create a new one
        drop(channels); // Release read lock before acquiring write lock

        self.create_overflow_channel().await
    }

    /// Create a new overflow channel
    async fn create_overflow_channel(&self) -> Result<Arc<OverflowChannel>, String> {
        let mut channels = self.overflow_channels.write().await;

        // Check if we're at the limit
        if channels.len() >= self.config.max_overflow_channels {
            return Err(format!(
                "Maximum overflow channels reached ({})",
                self.config.max_overflow_channels
            ));
        }

        // Use the factory to create a new channel
        let factory = self.channel_factory.read().await;
        let factory = factory.as_ref().ok_or_else(|| {
            "No channel factory configured - cannot create overflow channel".to_string()
        })?;

        let new_dc = factory().await?;
        let overflow = Arc::new(OverflowChannel::new(new_dc));
        overflow.is_active.store(true, Ordering::Relaxed);

        channels.push(Arc::clone(&overflow));

        // Update stats
        let mut stats = self.stats.write().await;
        stats.total_channels_created += 1;
        stats.active_overflow_count = channels.len();
        if channels.len() > stats.peak_overflow_count {
            stats.peak_overflow_count = channels.len();
        }

        info!(
            "AdaptiveChannelPool({}): Created overflow channel (total: {})",
            self.channel_id,
            channels.len()
        );

        Ok(overflow)
    }

    /// Clean up idle overflow channels, keeping minimum standby
    async fn cleanup_idle_channels(&self) {
        let idle_timeout = Duration::from_secs(self.config.idle_timeout_secs);
        let mut channels = self.overflow_channels.write().await;

        // Don't clean up if we're at or below minimum standby
        if channels.len() <= self.config.min_standby_channels {
            return;
        }

        let mut to_remove = Vec::new();

        for (idx, channel) in channels.iter().enumerate() {
            // Skip if this would bring us below minimum
            if channels.len() - to_remove.len() <= self.config.min_standby_channels {
                break;
            }

            if channel.is_idle(idle_timeout).await {
                to_remove.push(idx);
            }
        }

        // Remove idle channels (in reverse order to preserve indices)
        for idx in to_remove.into_iter().rev() {
            let removed = channels.remove(idx);

            // Close the WebRTC data channel
            if let Err(e) = removed.channel.close().await {
                warn!(
                    "AdaptiveChannelPool({}): Failed to close idle overflow channel: {}",
                    self.channel_id, e
                );
            }

            let mut stats = self.stats.write().await;
            stats.total_channels_closed += 1;
            stats.active_overflow_count = channels.len();

            debug!(
                "AdaptiveChannelPool({}): Closed idle overflow channel (remaining: {})",
                self.channel_id,
                channels.len()
            );
        }
    }

    /// Enter overflow mode (primary buffer is full)
    async fn enter_overflow_mode(&self) {
        if !self.in_overflow_mode.swap(true, Ordering::AcqRel) {
            // Just entered overflow mode
            *self.overflow_mode_start.write().await = Some(Instant::now());
            self.stats.write().await.overflow_events += 1;

            debug!(
                "AdaptiveChannelPool({}): Entered overflow mode (primary buffer full)",
                self.channel_id
            );
        }
    }

    /// Exit overflow mode (primary buffer has space again)
    async fn exit_overflow_mode(&self) {
        if self.in_overflow_mode.swap(false, Ordering::AcqRel) {
            // Just exited overflow mode - record duration
            if let Some(start) = self.overflow_mode_start.write().await.take() {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.stats.write().await.total_overflow_time_ms += duration_ms;

                debug!(
                    "AdaptiveChannelPool({}): Exited overflow mode (was in overflow for {}ms)",
                    self.channel_id, duration_ms
                );
            }

            // Mark all overflow channels as inactive
            let channels = self.overflow_channels.read().await;
            for channel in channels.iter() {
                channel.is_active.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Get current pool statistics
    #[allow(dead_code)] // API method for monitoring - called via Channel::get_pool_stats()
    pub async fn get_stats(&self) -> PoolStats {
        let mut stats = self.stats.read().await.clone();
        stats.active_overflow_count = self.overflow_channels.read().await.len();
        stats.primary_bytes_sent = self.primary_bytes_sent.load(Ordering::Relaxed);
        stats
    }

    /// Get statistics for each overflow channel
    #[allow(dead_code)] // API method for monitoring - called via Channel::get_overflow_channel_stats()
    pub async fn get_overflow_channel_stats(&self) -> Vec<OverflowChannelStats> {
        let channels = self.overflow_channels.read().await;
        let mut stats = Vec::with_capacity(channels.len());

        for channel in channels.iter() {
            stats.push(channel.get_stats().await);
        }

        stats
    }

    /// Check if currently in overflow mode
    pub fn is_in_overflow_mode(&self) -> bool {
        self.in_overflow_mode.load(Ordering::Acquire)
    }

    /// Get the number of active overflow channels
    #[allow(dead_code)] // API method for monitoring
    pub async fn overflow_channel_count(&self) -> usize {
        self.overflow_channels.read().await.len()
    }

    /// Get primary channel buffered amount
    #[allow(dead_code)] // API method for monitoring
    pub async fn primary_buffered_amount(&self) -> u64 {
        self.primary.buffered_amount().await
    }

    /// Manually trigger cleanup (useful for testing)
    #[allow(dead_code)] // Testing utility
    pub async fn force_cleanup(&self) {
        self.cleanup_idle_channels().await;
    }

    /// Close all overflow channels and stop cleanup task
    #[allow(dead_code)] // Called during channel shutdown
    pub async fn shutdown(&self) {
        // Stop cleanup task
        if let Some(task) = self.cleanup_task.write().await.take() {
            task.abort();
        }

        // Close all overflow channels
        let mut channels = self.overflow_channels.write().await;
        for channel in channels.drain(..) {
            if let Err(e) = channel.channel.close().await {
                warn!(
                    "AdaptiveChannelPool({}): Error closing overflow channel during shutdown: {}",
                    self.channel_id, e
                );
            }
        }

        // Update stats
        self.stats.write().await.active_overflow_count = 0;

        info!(
            "AdaptiveChannelPool({}): Shutdown complete",
            self.channel_id
        );
    }
}

impl Drop for AdaptiveChannelPool {
    fn drop(&mut self) {
        // Cancel cleanup task if still running
        if let Ok(mut task_guard) = self.cleanup_task.try_write() {
            if let Some(task) = task_guard.take() {
                task.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_config_defaults() {
        let config = PoolConfig::default();
        assert_eq!(config.buffer_threshold, STANDARD_BUFFER_THRESHOLD);
        assert_eq!(config.idle_timeout_secs, 5);
        assert_eq!(config.max_overflow_channels, 4);
        assert_eq!(config.min_standby_channels, 1);
    }

    #[test]
    fn test_pool_stats_default() {
        let stats = PoolStats::default();
        assert_eq!(stats.total_channels_created, 0);
        assert_eq!(stats.overflow_events, 0);
        assert_eq!(stats.active_overflow_count, 0);
    }
}
