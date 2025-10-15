use crate::tube_registry::SignalMessage;
use crate::webrtc_core::WebRTCPeerConnection;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::sleep;

/// Test helper to create a WebRTC peer connection for testing
async fn create_test_peer_connection() -> WebRTCPeerConnection {
    let (tx, _rx) = unbounded_channel();

    WebRTCPeerConnection::new(
        None,     // Use default config
        true,     // trickle_ice
        false,    // turn_only
        Some(tx), // signal_sender
        "test_tube_123".to_string(),
        Some("test_conversation".to_string()),
        None,                           // ksm_config (no credential refresh in tests)
        "test-client-v1.0".to_string(), // client_version
        None,                           // turn_credentials_created_at (not tracked in tests)
    )
    .await
    .expect("Failed to create test peer connection")
}

#[tokio::test]
async fn test_keepalive_start_and_stop() {
    let pc = create_test_peer_connection().await;

    // Start keepalive
    let _ = pc.start_keepalive().await;

    // Verify task is running
    assert!(
        pc.is_keepalive_running(),
        "Keepalive task should be running"
    );

    // Stop keepalive
    let _ = pc.stop_keepalive().await;

    // Verify task is stopped
    assert!(
        !pc.is_keepalive_running(),
        "Keepalive task should be stopped"
    );
}

#[tokio::test]
async fn test_keepalive_lifecycle() {
    let (tx, _rx) = unbounded_channel();

    let pc = WebRTCPeerConnection::new(
        None,
        true,
        false,
        Some(tx),
        "test_tube_keepalive".to_string(),
        Some("test_conversation".to_string()),
        None,                           // ksm_config (no credential refresh in tests)
        "test-client-v1.0".to_string(), // client_version
        None,                           // turn_credentials_created_at (not tracked in tests)
    )
    .await
    .expect("Failed to create peer connection");

    // Test keepalive lifecycle
    assert!(
        !pc.is_keepalive_running(),
        "Keepalive should not be running initially"
    );

    let _ = pc.start_keepalive().await;
    assert!(
        pc.is_keepalive_running(),
        "Keepalive should be running after start"
    );

    let _ = pc.stop_keepalive().await;
    assert!(
        !pc.is_keepalive_running(),
        "Keepalive should be stopped after stop"
    );
}

#[tokio::test]
async fn test_activity_tracking() {
    let pc = create_test_peer_connection().await;

    // Get initial activity time
    let initial_time = pc.get_last_activity();

    // Wait a bit
    sleep(Duration::from_millis(100)).await;

    // Update activity
    pc.update_activity();

    // Verify activity was updated
    let updated_time = pc.get_last_activity();

    assert!(
        updated_time > initial_time,
        "Activity timestamp should be updated"
    );
}

#[tokio::test]
async fn test_ice_restart_creation() {
    let pc = create_test_peer_connection().await;

    // Test ICE restart
    let result = pc.restart_ice().await;

    // Should succeed - webrtc-rs allows creating offers without remote description
    assert!(
        result.is_ok(),
        "ICE restart should succeed for new connection"
    );

    // Verify the result contains SDP data
    let sdp = result.unwrap();
    assert!(!sdp.is_empty(), "ICE restart should return non-empty SDP");
    assert!(sdp.contains("v=0"), "SDP should be valid (contain version)");
}

#[tokio::test]
async fn test_connection_state_detection() {
    let pc = create_test_peer_connection().await;

    // Test should_restart_ice method
    // This will return false for a new connection in "New" state
    let should_restart = pc.should_restart_ice();
    assert!(!should_restart, "Should not restart ICE for new connection");
}

#[tokio::test]
async fn test_activity_timeout_logic() {
    let pc = create_test_peer_connection().await;

    // Set very old activity time (use checked_sub to prevent overflow)
    let old_instant = std::time::Instant::now()
        .checked_sub(Duration::from_secs(3700))
        .unwrap_or_else(|| std::time::Instant::now() - Duration::from_secs(60)); // Fallback to 1 minute ago
    pc.set_last_activity(old_instant);

    // Test should_restart_ice logic which considers activity timeout
    let should_restart = pc.should_restart_ice();

    // Should not restart for new connection even with old activity
    // (connection state is "New", not "Disconnected" or "Failed")
    assert!(
        !should_restart,
        "Should not restart ICE for new connection even with old activity"
    );

    // Update activity and verify it changes
    let old_time = pc.get_last_activity();
    pc.update_activity();
    let new_time = pc.get_last_activity();

    assert!(new_time > old_time, "Activity should be updated");
}

#[tokio::test]
async fn test_network_change_detection() {
    let (tx, _rx) = unbounded_channel();

    let pc = WebRTCPeerConnection::new(
        None,
        true,
        false,
        Some(tx),
        "test_tube_network".to_string(),
        Some("test_conversation".to_string()),
        None,                           // ksm_config (no credential refresh in tests)
        "test-client-v1.0".to_string(), // client_version
        None,                           // turn_credentials_created_at (not tracked in tests)
    )
    .await
    .expect("Failed to create peer connection");

    let _ = pc.start_keepalive().await;

    // Simulate connection failure by checking current state
    // The keepalive loop will detect this and send network_change_detected

    // For testing, we can't easily simulate actual connection state changes,
    // but we can verify the logic exists
    let should_restart = pc.should_restart_ice();

    // Connection should be in "New" state, so shouldn't restart
    assert!(!should_restart);

    let _ = pc.stop_keepalive().await;
}

#[tokio::test]
async fn test_close_cleanup() {
    let pc = create_test_peer_connection().await;

    // Start keepalive
    let _ = pc.start_keepalive().await;

    // Verify it's running
    assert!(pc.is_keepalive_running());

    // Close connection
    let close_result = pc.close().await;

    // Should succeed
    assert!(close_result.is_ok(), "Close should succeed");

    // Keepalive should be stopped
    assert!(
        !pc.is_keepalive_running(),
        "Keepalive should be stopped after close"
    );

    // Connection should be marked as closing
    assert!(
        pc.is_closing.load(Ordering::Acquire),
        "Connection should be marked as closing"
    );
}

#[test]
fn test_resource_limits_configuration() {
    use crate::resource_manager::RESOURCE_MANAGER;

    let limits = RESOURCE_MANAGER.get_limits();

    // Verify keepalive settings are properly configured
    assert!(limits.ice_keepalive_enabled, "Keepalive should be enabled");
    assert_eq!(
        limits.ice_keepalive_interval,
        Duration::from_secs(60),
        "Keepalive interval should be 60s to prevent NAT timeout failures (was 300s)"
    );
    assert_eq!(
        limits.session_timeout,
        Duration::from_secs(3600),
        "Session timeout should be 1 hour"
    );
    assert_eq!(
        limits.turn_credential_refresh_interval,
        Duration::from_secs(600),
        "TURN refresh should be 10 minutes"
    );
    assert_eq!(
        limits.connection_health_check_interval,
        Duration::from_secs(120),
        "Health check should be 2 minutes"
    );
}

/// Integration test that simulates a complete connection scenario
#[tokio::test]
async fn test_complete_connection_scenario() {
    let pc = create_test_peer_connection().await;

    // Start keepalive
    let _ = pc.start_keepalive().await;

    // Simulate some activity
    pc.update_activity();

    // Test connection state query
    let connection_state = pc.connection_state();
    assert!(
        !connection_state.is_empty(),
        "Connection state should not be empty"
    );
    assert!(
        connection_state.contains("New"),
        "New connection should be in 'New' state"
    );

    // Test activity update
    let old_time = pc.get_last_activity();
    pc.update_activity();
    let new_time = pc.get_last_activity();
    assert!(new_time >= old_time, "Activity should be updated or same");

    // Test should_restart_ice logic
    let should_restart = pc.should_restart_ice();
    assert!(
        !should_restart,
        "New connection should not need ICE restart"
    );

    // Cleanup
    let _ = pc.stop_keepalive().await;
    let _ = pc.close().await;
}

/// Test that multiple restarts succeed and track attempts
#[tokio::test]
async fn test_restart_backoff_simulation() {
    // This test simulates the backoff logic that would be implemented
    // in the Python connection manager, but we can test the Rust side

    let pc = create_test_peer_connection().await;

    // Simulate multiple restart attempts
    for attempt in 0..3 {
        let result = pc.restart_ice().await;

        // All should succeed - webrtc-rs allows creating offers
        assert!(
            result.is_ok(),
            "Restart attempt {} should succeed",
            attempt + 1
        );

        // Verify the result contains valid SDP
        let sdp = result.unwrap();
        assert!(!sdp.is_empty(), "Restart SDP should not be empty");
        assert!(sdp.contains("v=0"), "SDP should be valid (contain version)");
    }

    // Test should_restart_ice logic - still shouldn't restart for new connection
    let should_restart = pc.should_restart_ice();
    assert!(
        !should_restart,
        "New connection should not need restart even after multiple restart calls"
    );
}

/// Mock test for signal message creation
#[test]
fn test_signal_message_creation() {
    let message = SignalMessage {
        tube_id: "test_tube".to_string(),
        kind: "keepalive".to_string(),
        data: "ping".to_string(),
        conversation_id: "test_conversation".to_string(),
        progress_flag: Some(1),
        progress_status: Some("KEEPALIVE".to_string()),
        is_ok: Some(true),
    };

    assert_eq!(message.tube_id, "test_tube");
    assert_eq!(message.kind, "keepalive");
    assert_eq!(message.data, "ping");
    assert_eq!(message.progress_flag, Some(1));
}
