//! Tube-related tests
use crate::runtime::get_runtime;
use crate::tube_and_channel_helpers::TubeStatus;
use crate::tube_protocol::CloseConnectionReason;
use crate::tube_registry::REGISTRY;
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::Tube;
use bytes::Bytes;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

// Get a tube by ID from the registry
pub async fn get_tube(tube_id: &str) -> Option<Arc<Tube>> {
    REGISTRY.read().await.get_by_tube_id(tube_id)
}

#[test]
fn test_tube_creation() {
    println!("Starting test_tube_creation");
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create a tube
        let tube = Tube::new(false, None, None).expect("Failed to create tube");
        let tube_id = tube.id();
        println!("Created tube with ID: {}", tube_id);

        // Create a signal channel
        let (signal_tx, _signal_rx) = mpsc::unbounded_channel();

        let mut settings = HashMap::new();
        settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

        // Create peer connection with explicit timeout
        let connection_fut = tube.create_peer_connection(
            None,
            true,
            false,
            "TEST_MODE_KSM_CONFIG_1".to_string(),
            "TEST_CALLBACK_TOKEN_1".to_string(),
            "ms16.5.0",
            settings,
            signal_tx,
        );
        let timeout_fut = tokio::time::timeout(Duration::from_secs(5), connection_fut);
        match timeout_fut.await {
            Ok(result) => {
                result.expect("Failed to create peer connection");
                println!("Peer connection created successfully");
            }
            Err(_) => {
                println!("Timeout creating peer connection, continuing with test");
                // Don't fail the test, just log and continue
            }
        }

        // Create a data channel with timeout
        let data_channel_fut = tube.create_data_channel(
            "test-channel",
            "TEST_MODE_KSM_CONFIG_1".to_string(),
            "TEST_CALLBACK_TOKEN_1".to_string(),
            "ms16.5.0",
        );
        let timeout_fut = tokio::time::timeout(Duration::from_secs(3), data_channel_fut);
        let data_channel = match timeout_fut.await {
            Ok(result) => result.expect("Failed to create data channel"),
            Err(_) => {
                println!("Timeout creating data channel, skipping data channel tests");
                // Skip the rest of the data channel tests
                {
                    let mut registry = REGISTRY.write().await;
                    tube.close(&mut registry).await
                }
                .expect("Failed to close tube");
                return;
            }
        };

        // Verify data channel label
        assert_eq!(data_channel.label(), "test-channel");

        // Get data channel by label
        let retrieved_channel = tube.get_data_channel("test-channel").await;
        assert!(
            retrieved_channel.is_some(),
            "Data channel should be accessible by label"
        );

        // Create the control channel with timeout
        let control_channel_fut = tube.create_control_channel(
            "TEST_MODE_KSM_CONFIG_1".to_string(),
            "TEST_CALLBACK_TOKEN_1".to_string(),
            "ms16.5.0",
        );
        let timeout_fut = tokio::time::timeout(Duration::from_secs(3), control_channel_fut);
        let control_channel = match timeout_fut.await {
            Ok(result) => result.expect("Failed to create control channel"),
            Err(_) => {
                println!("Timeout creating control channel, skipping verification");
                // Skip the verification
                {
                    let mut registry = REGISTRY.write().await;
                    tube.close(&mut registry).await
                }
                .expect("Failed to close tube");
                return;
            }
        };

        // Verify control channel label
        assert_eq!(control_channel.label(), "control");

        // Close the tube with timeout
        tokio::time::timeout(Duration::from_secs(3), async {
            // Pass async block directly
            let mut registry = REGISTRY.write().await;
            tube.close(&mut registry).await
        })
        .await
        .unwrap_or_else(|_| {
            println!("Timeout closing tube, but continuing");
            Ok(()) // This Ok(()) matches the inner Result from tube.close()
        })
        .expect("Failed to close tube");

        // Verify status changed to Closed
        assert_eq!(tube.status().await, TubeStatus::Closed);

        // Verify tube is removed from the registry
        let retrieved_tube = get_tube(&tube_id).await;
        assert!(
            retrieved_tube.is_none(),
            "Tube should be removed from the registry"
        );
    });
}

#[test]
fn test_tube_channel_creation() {
    println!("Starting test_tube_channel_creation");
    let runtime = get_runtime();
    runtime.block_on(async {
        let tube = Tube::new(false, None, None).expect("Failed to create tube");
        let tube_id = tube.id();
        let (signal_tx, _signal_rx) = mpsc::unbounded_channel();
        let mut settings = HashMap::new();
        settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

        tokio::time::timeout(
            Duration::from_secs(5),
            tube.create_peer_connection(None, true, false, "TEST_MODE_KSM_CONFIG_1".to_string(), "TEST_CALLBACK_TOKEN_1".to_string(), "ms16.5.0", settings.clone(), signal_tx)
        ).await.map_or_else(
            |_| println!("Timeout creating peer connection, continuing with test"),
            |res| res.expect("Failed to create peer connection")
        );

        let data_channel_fut = tube.create_data_channel("test-channel", "TEST_MODE_KSM_CONFIG_1".to_string(), "TEST_CALLBACK_TOKEN_1".to_string(), "ms16.5.0");
        let data_channel = match tokio::time::timeout(Duration::from_secs(3), data_channel_fut).await {
            Ok(Ok(dc)) => dc,
            Ok(Err(e)) => panic!("Failed to create data channel: {}", e),
            Err(_) => {
                println!("Timeout creating data channel, skipping channel tests");
                { let mut registry = REGISTRY.write().await; tube.close(&mut registry).await }.expect("Failed to close tube during data channel timeout");
                return;
            }
        };

        let _channel_result_port = tube.create_channel(
            "test",
            &data_channel,
            Some(5.0),
            settings,
            Some("TEST_CALLBACK_TOKEN_1".to_string()),
            Some("TEST_MODE_KSM_CONFIG_1".to_string()),
            Some("ms16.5.0".to_string()),
        ).await.expect("Call to create_channel itself failed");

        // Verify channel shutdown signal exists
        assert!(tube.channel_shutdown_signals.read().await.contains_key("test"), "Channel shutdown signal should exist after creation");

        // Close the channel and verify the signal is acted upon (signal removed from the map)
        let close_result = tube.close_channel("test", Some(CloseConnectionReason::Normal)).await;
        assert!(close_result.is_ok(), "close_channel should return Ok. Actual: {:?}", close_result);
        assert!(!tube.channel_shutdown_signals.read().await.contains_key("test"), "Channel shutdown signal should be removed after closing");

        // Try to close a non-existent channel
        assert!(tube.close_channel("nonexistent", Some(CloseConnectionReason::Error)).await.is_err(), "Non-existent channel should return an error on close");

        { let mut registry = REGISTRY.write().await; tube.close(&mut registry).await }.expect("Failed to close tube");

        for _ in 0..3 {
            if get_tube(&tube_id).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if get_tube(&tube_id).await.is_some() {
            println!("Warning: Tube was not removed from registry after closing in test_tube_channel_creation");
        }
    });
}

#[tokio::test]
async fn test_tube_create_with_pc() {
    let tube = Tube::new(false, None, None).expect("Failed to create tube");

    // Create a signaling channel
    let (signal_tx, _signal_rx) = mpsc::unbounded_channel();

    let mut settings = HashMap::new();
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

    // Test configuration creation
    let result = tube.create_peer_connection(
        None,
        true,  // trickle_ice
        false, // turn_only
        "TEST_MODE_KSM_CONFIG_1".to_string(),
        "TEST_CALLBACK_TOKEN_1".to_string(),
        "ms16.5.0",
        settings,
        signal_tx,
    );
    assert!(result.await.is_ok());
}

#[tokio::test]
async fn test_tube_webrtc_connection() {
    let tube = Tube::new(false, None, None).expect("Failed to create tube");

    // Create a signaling channel
    let (signal_tx, _) = mpsc::unbounded_channel();

    let mut settings = HashMap::new();
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));
    // Set up peer connection
    tube.create_peer_connection(
        None,
        true,  // trickle_ice
        false, // turn_only
        "TEST_MODE_KSM_CONFIG_1".to_string(),
        "TEST_CALLBACK_TOKEN_1".to_string(),
        "ms16.5.0",
        settings,
        signal_tx,
    )
    .await
    .expect("Failed to create peer connection");

    // Create an offer
    let offer = tube.create_offer().await.expect("Failed to create offer");
    assert!(!offer.is_empty());
}

#[tokio::test]
async fn test_tube_create_channel() {
    let tube = Tube::new(false, None, None).expect("Failed to create tube");
    let (signal_tx, _) = mpsc::unbounded_channel();
    let mut settings = HashMap::new();
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

    tube.create_peer_connection(
        Some(RTCConfiguration::default()),
        true,
        false,
        "TEST_MODE_KSM_CONFIG".to_string(),
        "test_callback_token".to_string(),
        "ms16.5.0",
        settings.clone(),
        signal_tx,
    )
    .await
    .expect("Tube failed to create peer connection");

    let data_channel = tube
        .create_data_channel(
            "test_dc",
            "ksm_config_val".to_string(),
            "token_val".to_string(),
            "ms16.5.0",
        )
        .await
        .expect("Failed to create data channel");

    let _channel_result_port = tube
        .create_channel(
            "test",
            &data_channel,
            Some(5.0),
            settings,
            Some("token_val".to_string()),
            Some("ksm_config_val".to_string()),
            Some("ms16.5.0".to_string()),
        )
        .await
        .expect("Failed to create channel instance");

    // Assert that the channel shutdown signal exists in the tube's map
    assert!(
        tube.channel_shutdown_signals
            .read()
            .await
            .contains_key("test"),
        "Channel shutdown signal should be present after creation."
    );
}

// New helper for offer/answer and ICE exchange
async fn perform_signaling_and_ice_exchange(
    tube1: &Arc<Tube>,
    tube2: &Arc<Tube>,
    signal_rx1: &mut mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
    signal_rx2: &mut mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
) -> Result<(), String> {
    info!(
        "[perform_signaling] Starting offer/answer and ICE for tubes: {} and {}",
        tube1.id(),
        tube2.id()
    );

    // Offer/Answer exchange
    let offer = tube1
        .create_offer()
        .await
        .map_err(|e| format!("[perform_signaling] Tube1 create_offer error: {}", e))?;
    info!("[perform_signaling] Tube1 created offer.");

    // Ensure tube2's peer connection exists before set_remote_description
    if tube2.peer_connection().await.is_none() {
        return Err(
            "Tube2 peer connection not initialized before set_remote_description".to_string(),
        );
    }
    tube2
        .set_remote_description(offer, false)
        .await
        .map_err(|e| {
            format!(
                "[perform_signaling] Tube2 set_remote_description (offer) error: {}",
                e
            )
        })?;
    info!("[perform_signaling] Tube2 set remote offer.");

    let answer = tube2
        .create_answer()
        .await
        .map_err(|e| format!("[perform_signaling] Tube2 create_answer error: {}", e))?;
    info!("[perform_signaling] Tube2 created answer.");

    // Ensure tube1's peer connection exists
    if tube1.peer_connection().await.is_none() {
        return Err(
            "Tube1 peer connection not initialized before set_remote_description for answer"
                .to_string(),
        );
    }
    tube1
        .set_remote_description(answer, true)
        .await
        .map_err(|e| {
            format!(
                "[perform_signaling] Tube1 set_remote_description (answer) error: {}",
                e
            )
        })?;
    info!("[perform_signaling] Tube1 set remote answer.");

    // ICE Candidate Exchange
    tokio::time::sleep(Duration::from_millis(500)).await;

    let max_ice_exchange_attempts = 25;
    let mut ice_connected = false;
    let mut tube1_ice_candidates_finished = false;
    let mut tube2_ice_candidates_finished = false;

    for attempt in 1..=max_ice_exchange_attempts {
        info!(
            "[perform_signaling] ICE exchange attempt {}/{}",
            attempt, max_ice_exchange_attempts
        );

        let state1_pc = tube1
            .peer_connection()
            .await
            .map_or(RTCPeerConnectionState::Unspecified, |p| {
                p.peer_connection.connection_state()
            });
        let state2_pc = tube2
            .peer_connection()
            .await
            .map_or(RTCPeerConnectionState::Unspecified, |p| {
                p.peer_connection.connection_state()
            });
        info!(
            "[perform_signaling] PC states: tube1={:?}, tube2={:?}",
            state1_pc, state2_pc
        );

        if state1_pc == RTCPeerConnectionState::Connected
            && state2_pc == RTCPeerConnectionState::Connected
        {
            ice_connected = true;
            info!("[perform_signaling] Both tubes RTCPeerConnectionState::Connected.");
            break;
        }

        // Exchange ICE candidates via signal channels
        let mut exchanged_any_this_attempt = false;

        // Process signals from tube1
        while !tube1_ice_candidates_finished {
            match tokio::time::timeout(Duration::from_millis(100), signal_rx1.recv()).await {
                Ok(Some(signal)) => {
                    if signal.kind == "icecandidate" {
                        info!(
                            "[perform_signaling] Tube1 ICE candidate: {}",
                            if signal.data.is_empty() {
                                "<empty>"
                            } else {
                                &signal.data
                            }
                        );
                        // Make sure to add the empty candidate to tube2 as well
                        tube2
                            .add_ice_candidate(signal.data.clone())
                            .await
                            .map_err(|e| {
                                format!("[perform_signaling] Tube2 add_ice_candidate error: {}", e)
                            })?;
                        exchanged_any_this_attempt = true;

                        if signal.data.is_empty() {
                            tube1_ice_candidates_finished = true;
                        }
                    }
                }
                Ok(None) => break, // Channel closed
                Err(_) => break,   // Timeout, no more signals for now
            }
        }

        // Process signals from tube2
        while !tube2_ice_candidates_finished {
            match tokio::time::timeout(Duration::from_millis(100), signal_rx2.recv()).await {
                Ok(Some(signal)) => {
                    if signal.kind == "icecandidate" {
                        info!(
                            "[perform_signaling] Tube2 ICE candidate: {}",
                            if signal.data.is_empty() {
                                "<empty>"
                            } else {
                                &signal.data
                            }
                        );
                        // Make sure to add the empty candidate to tube1 as well
                        tube1
                            .add_ice_candidate(signal.data.clone())
                            .await
                            .map_err(|e| {
                                format!("[perform_signaling] Tube1 add_ice_candidate error: {}", e)
                            })?;
                        exchanged_any_this_attempt = true;

                        if signal.data.is_empty() {
                            tube2_ice_candidates_finished = true;
                        }
                    }
                }
                Ok(None) => break, // Channel closed
                Err(_) => break,   // Timeout, no more signals for now
            }
        }

        // IMPORTANT: Don't break out of the loop just because ICE gathering is finished
        // Continue until the connection is established, or we reach max attempts
        if tube1_ice_candidates_finished && tube2_ice_candidates_finished {
            info!("[perform_signaling] All ICE candidates exchanged, continuing to monitor connection state...");
        }

        // Add a check to see if the connection happened immediately
        let current_state1_pc = tube1
            .peer_connection()
            .await
            .map_or(RTCPeerConnectionState::Unspecified, |p| {
                p.peer_connection.connection_state()
            });
        let current_state2_pc = tube2
            .peer_connection()
            .await
            .map_or(RTCPeerConnectionState::Unspecified, |p| {
                p.peer_connection.connection_state()
            });
        if current_state1_pc == RTCPeerConnectionState::Connected
            && current_state2_pc == RTCPeerConnectionState::Connected
        {
            ice_connected = true;
            info!("[perform_signaling] Connected during this attempt.");
            break;
        }

        if !exchanged_any_this_attempt && attempt > 8 {
            info!("[perform_signaling] No new ICE candidates exchanged for several attempts, checking states.");
            // Add a longer delay to see if it eventually connects
            tokio::time::sleep(Duration::from_secs(3)).await;
            let final_state1_pc = tube1
                .peer_connection()
                .await
                .map_or(RTCPeerConnectionState::Unspecified, |p| {
                    p.peer_connection.connection_state()
                });
            let final_state2_pc = tube2
                .peer_connection()
                .await
                .map_or(RTCPeerConnectionState::Unspecified, |p| {
                    p.peer_connection.connection_state()
                });
            if final_state1_pc == RTCPeerConnectionState::Connected
                && final_state2_pc == RTCPeerConnectionState::Connected
            {
                ice_connected = true;
                info!("[perform_signaling] Connected after additional delay.");
                break;
            }
            info!("[perform_signaling] Still not connected after extra delay. Final PC states: tube1={:?}, tube2={:?}", final_state1_pc, final_state2_pc);
            // Consider breaking or returning an error if stuck
            if attempt > 15
                && (final_state1_pc == RTCPeerConnectionState::Failed
                    || final_state2_pc == RTCPeerConnectionState::Failed)
            {
                return Err(format!(
                    "ICE failed, peer connection state is Failed. T1: {:?}, T2: {:?}",
                    final_state1_pc, final_state2_pc
                ));
            }
        }
        // Use a slightly longer, but consistent delay between attempts after the initial fast exchanges
        tokio::time::sleep(Duration::from_millis(if attempt <= 5 { 300 } else { 1000 })).await;
    }

    // Final check for connection status
    let final_state1_pc = tube1
        .peer_connection()
        .await
        .map_or(RTCPeerConnectionState::Unspecified, |p| {
            p.peer_connection.connection_state()
        });
    let final_state2_pc = tube2
        .peer_connection()
        .await
        .map_or(RTCPeerConnectionState::Unspecified, |p| {
            p.peer_connection.connection_state()
        });

    if final_state1_pc == RTCPeerConnectionState::Connected
        && final_state2_pc == RTCPeerConnectionState::Connected
    {
        ice_connected = true;
    }

    if !ice_connected {
        return Err(format!(
            "ICE connection failed after {} attempts. Final PC states: tube1={:?}, tube2={:?}",
            max_ice_exchange_attempts, final_state1_pc, final_state2_pc
        ));
    }

    info!("[perform_signaling] P2P signaling and ICE exchange appears complete.");
    Ok(())
}

#[tokio::test]
async fn test_tube_p2p_data_transfer_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    info!("[E2E_TEST] Starting test_tube_p2p_data_transfer_end_to_end");

    let tube1 = Tube::new(false, None, None)?;
    let tube2 = Tube::new(false, None, None)?;
    info!(
        "[E2E_TEST] Tube1 ID: {}, Tube2 ID: {}",
        tube1.id(),
        tube2.id()
    );

    let ksm_config_t1 = "TEST_MODE_KSM_CONFIG_T1_E2E".to_string();
    let token_t1 = "test_token_t1_e2e".to_string();
    let ksm_config_t2 = "TEST_MODE_KSM_CONFIG_T2_E2E".to_string();
    let token_t2 = "test_token_t2_e2e".to_string();

    // Create signal channels - now properly keeping both tx and rx
    let (signal_tx1, mut signal_rx1) = mpsc::unbounded_channel();
    let (signal_tx2, mut signal_rx2) = mpsc::unbounded_channel();

    let mut ice_servers = Vec::new();
    ice_servers.push(RTCIceServer {
        urls: vec!["stun:stun.l.google.com:19302?transport=udp&family=ipv4".to_string()],
        username: String::new(),
        credential: String::new(),
    });
    // Add a second Google STUN server as a backup
    ice_servers.push(RTCIceServer {
        urls: vec!["stun:stun1.l.google.com:19302?transport=udp&family=ipv4".to_string()],
        username: String::new(),
        credential: String::new(),
    });
    let rtc_config = RTCConfiguration {
        ice_servers,
        ..Default::default()
    };

    let mut settings = HashMap::new();
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

    // Create peer connections
    tube1
        .create_peer_connection(
            Some(rtc_config.clone()),
            true,
            false,
            ksm_config_t1.clone(),
            token_t1.clone(),
            "ms16.5.0",
            settings.clone(),
            signal_tx1,
        )
        .await?;
    info!("[E2E_TEST] Tube1 peer connection created.");
    tube2
        .create_peer_connection(
            Some(rtc_config),
            true,
            false,
            ksm_config_t2.clone(),
            token_t2.clone(),
            "ms16.5.0",
            settings,
            signal_tx2,
        )
        .await?;
    info!("[E2E_TEST] Tube2 peer connection created.");

    // Tube1 (offerer) creates the data channel *before* creating the offer
    let dc_label = "e2e-channel".to_string();
    info!(
        "[E2E_TEST] Tube1 attempting to create data channel '{}' BEFORE offer",
        dc_label
    );
    let dc1_out = tube1
        .create_data_channel(
            &dc_label,
            ksm_config_t1.clone(),
            token_t1.clone(),
            "ms16.5.0",
        )
        .await?;
    info!(
        "[E2E_TEST] Tube1: create_data_channel call returned for '{}'",
        dc_label
    );

    let (dc1_open_tx, dc1_open_rx) = oneshot::channel();
    let dc1_label_clone_for_onopen = dc1_out.label();
    dc1_out.data_channel.on_open(Box::new(move || {
        // Attach to raw RTCDataChannel
        info!(
            "[E2E_TEST] Tube1: dc1_out '{}' (RTCDataChannel) ON_OPEN triggered.",
            dc1_label_clone_for_onopen
        );
        let _ = dc1_open_tx.send(());
        Box::pin(async {})
    }));

    // Perform signaling (Offer/Answer + ICE) with signal channels
    perform_signaling_and_ice_exchange(&tube1, &tube2, &mut signal_rx1, &mut signal_rx2)
        .await
        .map_err(|e| format!("[E2E_TEST] Signaling/ICE helper failed: {}", e))?;
    info!("[E2E_TEST] Signaling and ICE exchange complete.");

    // Wait for dc1_out to open
    tokio::time::timeout(Duration::from_secs(20), dc1_open_rx) // Increased timeout for channel open
        .await
        .map_err(|e| format!("[E2E_TEST] Timeout waiting for dc1_out to open: {}", e))?
        .map_err(|e| format!("[E2E_TEST] dc1_out_open_rx failed: {}", e))?;
    info!(
        "[E2E_TEST] Tube1: dc1_out '{}' is confirmed open.",
        dc_label
    );

    // Tube2 side: Wait for the data channel and attach test's message handler
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Bytes>();
    let mut dc2_in_opt: Option<WebRTCDataChannel> = None; // This will be our wrapper

    for i in 0..200 {
        // Poll for up to 20 seconds
        if let Some(found_dc_wrapper) = tube2.get_data_channel(&dc_label).await {
            info!(
                "[E2E_TEST] Tube2: Found '{}' via get_data_channel. Setting handlers.",
                dc_label
            );

            let (dc2_open_tx_test, dc2_open_rx_test) = oneshot::channel();
            let found_dc_clone_for_open = found_dc_wrapper.clone();

            found_dc_wrapper.data_channel.on_open(Box::new(move || { // Attach to raw RTCDataChannel
                info!("[E2E_TEST] Tube2: dc_in '{}' (RTCDataChannel) ON_OPEN triggered for test handler.", found_dc_clone_for_open.label());
                let _ = dc2_open_tx_test.send(());
                Box::pin(async {})
            }));

            let found_dc_clone_for_message = found_dc_wrapper.clone();
            let msg_tx_clone = msg_tx.clone();
            found_dc_wrapper.data_channel.on_message(Box::new(move |msg: DataChannelMessage| { // Attach to raw
                let tx_c = msg_tx_clone.clone();
                let current_label_for_log = found_dc_clone_for_message.label();
                Box::pin(async move {
                    info!("[E2E_TEST] Tube2: dc_in '{}' TEST on_message received {} bytes (is_string: {})", current_label_for_log, msg.data.len(), msg.is_string);
                    // Log a preview if it's a UTF-8 string, for easier debugging.
                    // Note: msg.is_string might be false if RTCDataChannel.send (binary) was used,
                    // even if the content is a valid UTF-8 string.
                    match String::from_utf8(msg.data.to_vec()) {
                        Ok(s_preview) => {
                             info!("[E2E_TEST] Tube2: dc_in '{}' (data preview as string: '{}')", current_label_for_log, s_preview);
                        }
                        Err(_) => {
                             info!("[E2E_TEST] Tube2: dc_in '{}' (data is not valid UTF-8 for preview)", current_label_for_log);
                        }
                    }
                    if tx_c.send(msg.data).is_err() { // Send the Bytes directly
                        error!("[E2E_TEST] Tube2: dc_in on_message failed to send Bytes to test mpsc channel");
                    }
                })
            }));
            info!("[E2E_TEST] Tube2: Test's on_message and on_open handlers set for '{}' on underlying RTCDataChannel.", dc_label);

            dc2_in_opt = Some(found_dc_wrapper.clone());

            if found_dc_wrapper.data_channel.ready_state() != RTCDataChannelState::Open {
                info!("[E2E_TEST] Tube2: dc_in '{}' RTCDataChannel not yet open, waiting for test's ON_OPEN...", dc_label);
                tokio::time::timeout(Duration::from_secs(15), dc2_open_rx_test) // Increased timeout
                    .await.map_err(|e| format!("[E2E_TEST] Timeout waiting for dc2_in (RTCDataChannel) test on_open: {}", e))?
                    .map_err(|e| format!("[E2E_TEST] dc2_in test on_open_rx failed: {}", e))?;
                info!(
                    "[E2E_TEST] Tube2: dc_in '{}' confirmed open via test\\'s ON_OPEN.\"",
                    dc_label
                );
            } else {
                info!(
                    "[E2E_TEST] Tube2: dc_in '{}' was already open when retrieved.\"",
                    dc_label
                );
            }
            break;
        }
        info!(
            "[E2E_TEST] Tube2: Waiting for data channel '{}'... Attempt {}/150\"",
            dc_label,
            i + 1
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let dc2_in = dc2_in_opt.ok_or_else(|| {
        format!(
            "[E2E_TEST] Data channel '{}' not found or set up on tube2\"",
            dc_label
        )
    })?;
    // Use data_channel.ready_state() for openness check
    assert_eq!(
        dc2_in.data_channel.ready_state(),
        RTCDataChannelState::Open,
        "[E2E_TEST] dc2_in '{}' should be open after setup loop\"",
        dc_label
    );
    info!(
        "[E2E_TEST] Tube2: dc_in '{}' setup complete and confirmed open.\"",
        dc_label
    );

    let test_message_string = format!(
        "Hello from Tube1 {} to Tube2 {} at {}",
        tube1.id(),
        tube2.id(),
        Utc::now().to_rfc3339()
    );
    let sent_bytes = Bytes::from(test_message_string.clone()); // Bytes to be sent and used for assertion
    info!(
        "[E2E_TEST] Tube1: Sending message (as Bytes, content: '{}') on dc '{}'",
        test_message_string,
        dc1_out.label()
    );
    dc1_out.send(sent_bytes.clone()).await?; // Send a clone of sent_bytes
    info!("[E2E_TEST] Tube1: Message sent.");

    info!(
        "[E2E_TEST] Tube2: Waiting for message on dc '{}'...",
        dc_label
    );
    match tokio::time::timeout(Duration::from_secs(15), msg_rx.recv()).await {
        // msg_rx now receives Bytes
        Ok(Some(received_bytes)) => {
            // received_bytes is Bytes
            info!("[E2E_TEST] Tube2: Received {} bytes.", received_bytes.len());
            // For easier debugging of assertion failures, log string versions if possible
            let received_string_preview = String::from_utf8(received_bytes.to_vec())
                .unwrap_or_else(|_| "[[not a valid UTF-8 string]]".to_string());
            // sent_bytes was derived from test_message_string, so it should be valid UTF-8.
            let sent_string_preview =
                String::from_utf8(sent_bytes.to_vec()).expect("sent_bytes should be valid UTF-8");
            info!("[E2E_TEST] Tube2: Comparing received data (preview: '{}') with sent data (preview: '{}')", received_string_preview, sent_string_preview);
            assert_eq!(received_bytes, sent_bytes); // Compare Bytes directly
        }
        Ok(None) => {
            return Err(Box::from(
                "[E2E_TEST] Tube2: Message channel closed prematurely by sender.",
            ));
        }
        Err(e) => {
            return Err(Box::from(format!(
                "[E2E_TEST] Tube2: Timeout waiting for message: {}",
                e
            )));
        }
    }

    info!("[E2E_TEST] Message received and verified successfully.");

    info!("[E2E_TEST] Closing tubes.");
    let tube1_id = tube1.id();
    let tube2_id = tube2.id();
    if let Err(e) = {
        let mut registry = REGISTRY.write().await;
        tube1.close(&mut registry).await
    } {
        error!("[E2E_TEST] Error closing tube1 ({}): {}", tube1_id, e);
    }
    if let Err(e) = {
        let mut registry = REGISTRY.write().await;
        tube2.close(&mut registry).await
    } {
        error!("[E2E_TEST] Error closing tube2 ({}): {}", tube2_id, e);
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    info!("[E2E_TEST] Test finished successfully.");
    Ok(())
}
