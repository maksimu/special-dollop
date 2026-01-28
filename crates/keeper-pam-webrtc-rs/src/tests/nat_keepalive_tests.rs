//! Tests for NAT keepalive functionality
//!
//! Verifies that keepalive interval is correctly configured to prevent
//! NAT timeout failures seen in production logs.

use crate::resource_manager::RESOURCE_MANAGER;
use std::time::Duration;

#[test]
fn test_nat_keepalive_interval_configured() {
    // CRITICAL TEST: Verify keepalive interval is 60s (not 300s!)
    // This prevents the "disconnected after 36 minutes" failures from logs

    let limits = RESOURCE_MANAGER.get_limits();

    println!("\n=== NAT Keepalive Configuration ===");
    println!("Interval: {:?}", limits.ice_keepalive_interval);
    println!("Enabled: {}", limits.ice_keepalive_enabled);
    println!("===================================\n");

    // Assert 60 second interval (critical for NAT table survival)
    assert_eq!(
        limits.ice_keepalive_interval,
        Duration::from_secs(60),
        "Keepalive MUST be 60s to prevent NAT timeout failures!"
    );

    assert!(
        limits.ice_keepalive_enabled,
        "Keepalive MUST be enabled to prevent NAT timeouts!"
    );

    // Verify it's NOT the old problematic 300s value
    assert_ne!(
        limits.ice_keepalive_interval,
        Duration::from_secs(300),
        "Old 300s interval caused 30-minute disconnections!"
    );

    println!("✓ NAT keepalive correctly configured at 60s");
    println!("✓ This prevents NAT table expiry at 30 minutes");
}

#[test]
fn test_keepalive_frequency() {
    // Verify keepalive fires frequently enough
    let limits = RESOURCE_MANAGER.get_limits();
    let interval_secs = limits.ice_keepalive_interval.as_secs();

    // NAT tables typically expire at 30 minutes (1800 seconds)
    // With 60s keepalive, we get 30 keepalives in 30 minutes = SAFE
    let keepalives_per_30_min = 1800 / interval_secs;

    println!("\n=== Keepalive Frequency Analysis ===");
    println!("Interval: {}s", interval_secs);
    println!("Keepalives per 30min: {}", keepalives_per_30_min);
    println!("====================================\n");

    assert!(
        keepalives_per_30_min >= 20,
        "Should have at least 20 keepalives per 30min for safety (got {})",
        keepalives_per_30_min
    );

    println!("✓ Keepalive frequency is sufficient for NAT survival");
}
