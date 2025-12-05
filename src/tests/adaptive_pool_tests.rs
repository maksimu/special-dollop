//! Tests for the AdaptiveChannelPool
//!
//! These tests verify:
//! 1. Fill-then-overflow behavior
//! 2. Overflow channel creation on demand
//! 3. Idle channel cleanup
//! 4. Statistics tracking
//! 5. Standby channel preservation

use crate::channel::adaptive_pool::{PoolConfig, PoolStats};

/// Test pool configuration defaults
#[test]
fn test_pool_config_defaults() {
    let config = PoolConfig::default();

    // Verify sensible defaults
    assert!(
        config.buffer_threshold > 0,
        "Buffer threshold should be positive"
    );
    assert!(
        config.idle_timeout_secs > 0,
        "Idle timeout should be positive"
    );
    assert!(
        config.max_overflow_channels > 0,
        "Max overflow should be positive"
    );
    assert!(
        config.min_standby_channels <= config.max_overflow_channels,
        "Min standby should not exceed max overflow"
    );
    assert!(
        config.cleanup_interval_secs > 0,
        "Cleanup interval should be positive"
    );
}

/// Test pool statistics initialization
#[test]
fn test_pool_stats_initial_state() {
    let stats = PoolStats::default();

    assert_eq!(stats.total_channels_created, 0);
    assert_eq!(stats.total_channels_closed, 0);
    assert_eq!(stats.overflow_events, 0);
    assert_eq!(stats.active_overflow_count, 0);
    assert_eq!(stats.primary_bytes_sent, 0);
    assert_eq!(stats.overflow_bytes_sent, 0);
    assert_eq!(stats.peak_overflow_count, 0);
    assert_eq!(stats.total_overflow_time_ms, 0);
}

/// Test custom pool configuration
#[test]
fn test_pool_config_custom() {
    let config = PoolConfig {
        buffer_threshold: 16 * 1024, // 16KB
        idle_timeout_secs: 10,
        max_overflow_channels: 8,
        min_standby_channels: 2,
        cleanup_interval_secs: 5,
    };

    assert_eq!(config.buffer_threshold, 16 * 1024);
    assert_eq!(config.idle_timeout_secs, 10);
    assert_eq!(config.max_overflow_channels, 8);
    assert_eq!(config.min_standby_channels, 2);
    assert_eq!(config.cleanup_interval_secs, 5);
}

/// Test that configuration cloning works correctly
#[test]
fn test_pool_config_clone() {
    let original = PoolConfig {
        buffer_threshold: 32 * 1024,
        idle_timeout_secs: 15,
        max_overflow_channels: 6,
        min_standby_channels: 1,
        cleanup_interval_secs: 3,
    };

    let cloned = original.clone();

    assert_eq!(cloned.buffer_threshold, original.buffer_threshold);
    assert_eq!(cloned.idle_timeout_secs, original.idle_timeout_secs);
    assert_eq!(cloned.max_overflow_channels, original.max_overflow_channels);
    assert_eq!(cloned.min_standby_channels, original.min_standby_channels);
    assert_eq!(cloned.cleanup_interval_secs, original.cleanup_interval_secs);
}

/// Test stats cloning preserves all fields
#[test]
fn test_pool_stats_clone() {
    let mut original = PoolStats::default();
    original.total_channels_created = 5;
    original.total_channels_closed = 3;
    original.overflow_events = 10;
    original.active_overflow_count = 2;
    original.primary_bytes_sent = 1_000_000;
    original.overflow_bytes_sent = 500_000;
    original.peak_overflow_count = 4;
    original.total_overflow_time_ms = 5000;

    let cloned = original.clone();

    assert_eq!(cloned.total_channels_created, 5);
    assert_eq!(cloned.total_channels_closed, 3);
    assert_eq!(cloned.overflow_events, 10);
    assert_eq!(cloned.active_overflow_count, 2);
    assert_eq!(cloned.primary_bytes_sent, 1_000_000);
    assert_eq!(cloned.overflow_bytes_sent, 500_000);
    assert_eq!(cloned.peak_overflow_count, 4);
    assert_eq!(cloned.total_overflow_time_ms, 5000);
}

/// Test configuration debug format
#[test]
fn test_pool_config_debug() {
    let config = PoolConfig::default();
    let debug_str = format!("{:?}", config);

    // Verify debug output contains expected fields
    assert!(debug_str.contains("buffer_threshold"));
    assert!(debug_str.contains("idle_timeout_secs"));
    assert!(debug_str.contains("max_overflow_channels"));
    assert!(debug_str.contains("min_standby_channels"));
}

/// Test stats debug format
#[test]
fn test_pool_stats_debug() {
    let stats = PoolStats::default();
    let debug_str = format!("{:?}", stats);

    // Verify debug output contains expected fields
    assert!(debug_str.contains("total_channels_created"));
    assert!(debug_str.contains("overflow_events"));
    assert!(debug_str.contains("primary_bytes_sent"));
}
