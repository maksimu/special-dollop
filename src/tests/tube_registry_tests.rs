#![cfg(test)]
use crate::tube::Tube;
use crate::runtime::get_runtime;
use std::sync::Arc;
use std::collections::HashMap;
use anyhow::{anyhow, Result};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use std::time::Duration;
use bytes::Bytes;
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, error, warn};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use crate::tube_registry::{SignalMessage, TubeRegistry};
use tracing_subscriber::{EnvFilter};

// Helper to create a default tube for testing
// This helper will create a tube that is NOT automatically added to the global REGISTRY
// as tests should manage their own registry instances.
fn new_test_tube_without_registry_add() -> Result<Arc<Tube>> {
    let id = uuid::Uuid::new_v4().to_string();
    let runtime = get_runtime();
    Ok(Arc::new(Tube {
        id,
        peer_connection: Arc::new(tokio::sync::Mutex::new(None)),
        data_channels: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        control_channel: Arc::new(tokio::sync::RwLock::new(None)),
        channel_shutdown_signals: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        active_channels: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        is_server_mode_context: false, // Default to client context for this helper
        status: Arc::new(tokio::sync::RwLock::new(crate::tube_and_channel_helpers::TubeStatus::New)),
        runtime,
        original_conversation_id: None,
    }))
}

#[test]
fn test_registry_new() {
    let _registry = TubeRegistry::new();
    // Verifies that new doesn't panic and basic fields are initialized (implicitly)
}

#[tokio::test]
async fn test_add_and_get_tube() {
    let mut registry = TubeRegistry::new();
    let tube = new_test_tube_without_registry_add().expect("Failed to create test tube");
    let tube_id = tube.id();

    registry.add_tube(Arc::clone(&tube));

    let retrieved_tube = registry.get_by_tube_id(&tube_id);
    assert!(retrieved_tube.is_some(), "Tube should be found by ID");
    assert_eq!(retrieved_tube.unwrap().id(), tube_id, "Retrieved tube ID should match original");
}

#[tokio::test]
async fn test_remove_tube() {
    let mut registry = TubeRegistry::new();
    let tube = new_test_tube_without_registry_add().expect("Failed to create test tube");
    let tube_id = tube.id();

    registry.add_tube(Arc::clone(&tube));
    assert!(registry.get_by_tube_id(&tube_id).is_some(), "Tube should be present before removal");

    registry.remove_tube(&tube_id);
    assert!(registry.get_by_tube_id(&tube_id).is_none(), "Tube should be gone after removal");
    assert!(registry.get_signal_channel(&tube_id).is_none(), "Signal channel should be removed");
}

#[tokio::test]
async fn test_associate_and_get_by_conversation_id() -> Result<()> {
    let mut registry = TubeRegistry::new();
    let tube = new_test_tube_without_registry_add()?;
    let tube_id = tube.id();
    let conversation_id = "conv_123";

    registry.add_tube(Arc::clone(&tube));
    let _ = registry.associate_conversation(&tube_id, conversation_id);

    let retrieved_tube = registry.get_by_conversation_id(conversation_id);
    assert!(retrieved_tube.is_some(), "Tube should be found by conversation ID");
    assert_eq!(retrieved_tube.unwrap().id(), tube_id, "Retrieved tube ID should match");

    registry.remove_tube(&tube_id);
    assert!(registry.get_by_conversation_id(conversation_id).is_none(), "Conversation mapping should be gone after tube removal");

    Ok(())
}

#[tokio::test]
async fn test_all_tube_ids() -> Result<()> {
    let mut registry = TubeRegistry::new();
    let tube1 = new_test_tube_without_registry_add()?;
    let tube2 = new_test_tube_without_registry_add()?;
    let tube1_id = tube1.id();
    let tube2_id = tube2.id();

    registry.add_tube(Arc::clone(&tube1));
    registry.add_tube(Arc::clone(&tube2));

    let ids = registry.all_tube_ids_sync();
    assert_eq!(ids.len(), 2, "Should have two tube IDs");
    assert!(ids.contains(&tube1_id), "List should contain tube1's ID");
    assert!(ids.contains(&tube2_id), "List should contain tube2's ID");
    Ok(())
}

#[tokio::test]
async fn test_find_tubes() -> Result<()> {
    let mut registry = TubeRegistry::new();
    let tube1 = new_test_tube_without_registry_add()?;
    let tube2 = new_test_tube_without_registry_add()?;
    let tube3 = new_test_tube_without_registry_add()?;

    let tube1_id_str = tube1.id();
    let conv_id_for_tube3 = format!("conv_for_{}", tube3.id());

    registry.add_tube(Arc::clone(&tube1));
    registry.add_tube(Arc::clone(&tube2));
    registry.add_tube(Arc::clone(&tube3));
    let _ = registry.associate_conversation(&tube3.id(), &conv_id_for_tube3);

    if tube1_id_str.len() >= 5 {
        let partial_id1 = &tube1_id_str[0..5];
        let found_tubes1 = registry.find_tubes(partial_id1);
        assert_eq!(found_tubes1.len(), 1, "Should find tube1 by partial ID: {}", partial_id1);
        assert_eq!(found_tubes1[0], tube1_id_str);
    }

    if conv_id_for_tube3.len() >= 10 {
        let partial_conv_id3 = &conv_id_for_tube3[0..10];
        let found_tubes2 = registry.find_tubes(partial_conv_id3);
        assert_eq!(found_tubes2.len(), 1, "Should find tube3 by partial conversation ID: {}", partial_conv_id3);
        assert_eq!(found_tubes2[0], tube3.id());
    }
    
    let found_tubes_none = registry.find_tubes("nonexistent_term");
    assert!(found_tubes_none.is_empty(), "Should find no tubes for a nonexistent term");

    Ok(())
}

#[tokio::test]
async fn test_signal_channels() -> Result<()> {
    let mut registry = TubeRegistry::new();
    let tube = new_test_tube_without_registry_add()?;
    let tube_id = tube.id();

    let mut receiver = registry.register_signal_channel(&tube_id);

    let test_signal = SignalMessage {
        tube_id: tube_id.clone(),
        kind: "test_kind".to_string(),
        data: "test_data".to_string(),
        conversation_id: "conv_signal_123".to_string(),
    };

    registry.send_signal(test_signal.clone())?;

    match tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await {
        Ok(Some(received_signal)) => {
            assert_eq!(received_signal.tube_id, test_signal.tube_id);
            assert_eq!(received_signal.kind, test_signal.kind);
            assert_eq!(received_signal.data, test_signal.data);
            assert_eq!(received_signal.conversation_id, test_signal.conversation_id);
        }
        Ok(None) => panic!("Signal channel closed prematurely (received None from recv() within timeout)"),
        Err(_elapsed_error) => panic!("Timeout waiting for signal message"),
    }

    registry.remove_signal_channel(&tube_id);
    assert!(registry.get_signal_channel(&tube_id).is_none(), "Signal channel should be removed");

    Ok(())
}

#[tokio::test]
async fn test_set_and_get_server_mode() {
    let mut registry = TubeRegistry::new();
    assert!(!registry.is_server_mode(), "Should default to not server mode");
    registry.set_server_mode(true);
    assert!(registry.is_server_mode(), "Should be in server mode after setting true");
    registry.set_server_mode(false);
    assert!(!registry.is_server_mode(), "Should be in client mode after setting false");
}

// Use the special test mode KSM config string
const TEST_MODE_KSM_CONFIG: &str = "TEST_MODE_KSM_CONFIG";

const TEST_CALLBACK_TOKEN: &str = "test_callback_token_e2e_registry";

// Helper to run a simple TCP Ack Server for the test
async fn run_ack_server(mut kill_signal_rx: tokio::sync::oneshot::Receiver<()>) -> Result<std::net::SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let actual_addr = listener.local_addr()?;
    info!("[AckServer] Listening on {}", actual_addr);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut kill_signal_rx => {
                    info!("[AckServer] Shutdown signal received.");
                    break;
                }
                Ok((mut socket, client_addr)) = listener.accept() => {
                    info!("[AckServer] Accepted connection from {}", client_addr);
                    tokio::spawn(async move {
                        let (mut reader, mut writer) = socket.split();
                        let mut buffer = vec![0; 1024];
                        loop {
                            let n = match reader.read(&mut buffer).await {
                                Ok(0) => {
                                    info!("[AckServer] Client {} disconnected.", client_addr);
                                    break;
                                }
                                Ok(n) => n,
                                Err(e) => {
                                    error!("[AckServer] Failed to read from socket from {}: {:?}", client_addr, e);
                                    break;
                                }
                            };
                            let received_data = buffer[..n].to_vec();
                            info!("[AckServer] Received from {}: '{}'", client_addr, String::from_utf8_lossy(&received_data));
                            let mut response = received_data.clone();
                            response.extend_from_slice(b" ack");
                            if writer.write_all(&response).await.is_err() {
                                error!("[AckServer] Failed to write ack to socket for {}", client_addr);
                                break;
                            }
                            info!("[AckServer] Sent ack back to {}", client_addr);
                        }
                    });
                }
                else => {
                    info!("[AckServer] Listener closed or other error, shutting down.");
                    break; 
                }
            }
        }
    });
    Ok(actual_addr)
}

async fn perform_tube_signaling(
    server_tube_id: &str,
    server_registry: &mut TubeRegistry,
    signal_rx1: &mut UnboundedReceiver<SignalMessage>,
    client_tube_id: &str,
    client_registry: &mut TubeRegistry,
    signal_rx2: &mut UnboundedReceiver<SignalMessage>,
    client_answer_sdp: String,
) -> Result<(), String> {
    let server_tube = server_registry.get_by_tube_id(server_tube_id)
        .ok_or_else(|| format!("Server tube {} not found in registry", server_tube_id))?;
    let client_tube = client_registry.get_by_tube_id(client_tube_id)
        .ok_or_else(|| format!("Client tube {} not found in registry", client_tube_id))?;

    // Use server_registry to set a remote description, which handles base64 decoding
    server_registry.set_remote_description(server_tube_id, &client_answer_sdp, true).await
        .map_err(|e| format!("ServerTube ({}) set_remote_description (answer) error via registry: {}", server_tube.id(), e))?
        .map_or(Ok(()), |_| Err(format!("ServerTube ({}) set_remote_description (answer) via registry returned unexpected Some(answer)", server_tube.id())))?;

    info!("[Signaling] ServerTube ({}) set remote answer from client via registry.", server_tube.id());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut tube1_ice_candidates_finished = false;
    let mut tube2_ice_candidates_finished = false;
    let mut attempts = 0;
    let max_attempts = 70;

    loop {
        if (tube1_ice_candidates_finished && tube2_ice_candidates_finished) || attempts >= max_attempts {
            info!("[Signaling] ICE candidate exchange loop finishing. ServerDone={}, ClientDone={}, Attempts={}",
                  tube1_ice_candidates_finished, tube2_ice_candidates_finished, attempts);
            break;
        }
        attempts += 1;

        tokio::select! {
            Some(signal) = signal_rx1.recv(), if !tube1_ice_candidates_finished => {
                if signal.kind == "icecandidate" {
                    info!("[SignalingDebug] ServerTube ({}) received ICE signal. Data: '{}', isEmpty: {}", server_tube.id(), signal.data, signal.data.is_empty());
                    if signal.data.is_empty() {
                        info!("[Signaling] ServerTube ({}) indicated all ICE candidates gathered.", server_tube.id());
                        tube1_ice_candidates_finished = true;
                    } else {
                        info!("[Signaling] ServerTube ({}) sending ICE to ClientTube ({}): {}", server_tube.id(), client_tube.id(), signal.data);
                        if let Err(e) = client_tube.add_ice_candidate(signal.data).await {
                            warn!("[Signaling] ClientTube ({}) failed to add ICE candidate from ServerTube ({}): {}", client_tube.id(), server_tube.id(), e);
                        }
                    }
                }
            }
            Some(signal) = signal_rx2.recv(), if !tube2_ice_candidates_finished => {
                if signal.kind == "icecandidate" {
                    info!("[SignalingDebug] ClientTube ({}) received ICE signal. Data: '{}', isEmpty: {}", client_tube.id(), signal.data, signal.data.is_empty());
                    if signal.data.is_empty() {
                        info!("[Signaling] ClientTube ({}) indicated all ICE candidates gathered.", client_tube.id());
                        tube2_ice_candidates_finished = true;
                    } else {
                        info!("[Signaling] ClientTube ({}) sending ICE to ServerTube ({}): {}", client_tube.id(), server_tube.id(), signal.data);
                        if let Err(e) = server_tube.add_ice_candidate(signal.data).await {
                            warn!("[Signaling] ServerTube ({}) failed to add ICE candidate from ClientTube ({}): {}", server_tube.id(), client_tube.id(), e);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if attempts % 5 == 0 {
                    let state1 = server_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
                    let state2 = client_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
                    info!("[Signaling] Tube states @ attempt {}: ServerT={:?}, ClientT={:?}", attempts, state1, state2);
                }
            }
        }

        let state1 = server_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
        let state2 = client_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
        if state1 == RTCPeerConnectionState::Connected && state2 == RTCPeerConnectionState::Connected {
            info!("[Signaling] Both tubes connected during ICE exchange!");
            return Ok(());
        }
    }

    for _ in 0..10 {
        let state1 = server_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
        let state2 = client_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified);
        info!("[Signaling] Final check: ServerTube ({}) state: {:?}, ClientTube ({}) state: {:?}", server_tube.id(), state1, client_tube.id(), state2);
        if state1 == RTCPeerConnectionState::Connected && state2 == RTCPeerConnectionState::Connected {
            info!("[Signaling] Both tubes connected after ICE loop!");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Err(format!("ICE connection failed to establish. ServerT ({}). ClientT ({}). Final states after {} attempts: ServerT={:?}, ClientT={:?}",
        server_tube.id(), client_tube.id(), attempts,
        server_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified),
        client_tube.peer_connection().await.map(|pc| pc.peer_connection.connection_state()).unwrap_or(RTCPeerConnectionState::Unspecified)
    ))
}

// Helper function for test logging setup within this module
fn setup_test_logging() {
    static LOGGING_INIT: std::sync::Once = std::sync::Once::new();
    LOGGING_INIT.call_once(|| {
        let filter = EnvFilter::builder()
            .with_default_directive(tracing::metadata::LevelFilter::TRACE.into())
            .from_env_lossy(); 

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_target(true)
            .with_level(true)
            .with_test_writer()
            .try_init()
            .ok();
    });
}

#[tokio::test]
async fn test_registry_e2e_server_client_echo() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    setup_test_logging(); // Call the setup

    let _runtime = get_runtime();

    // Start the external Ack Server
    let (ack_server_kill_tx, ack_server_kill_rx_for_server_task) = tokio::sync::oneshot::channel();
    let (ack_server_actual_kill_tx_for_test, ack_server_kill_rx_for_test_scope) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if ack_server_kill_rx_for_test_scope.await.is_ok() {
            let _ = ack_server_kill_tx.send(());
        }
    });
    let ack_server_addr = run_ack_server(ack_server_kill_rx_for_server_task).await?;
    info!("[E2E_Test] External Ack server running on {}", ack_server_addr);

    // Create registry instances
    let mut server_registry = TubeRegistry::new();
    let mut client_registry = TubeRegistry::new();

    // Signal channels for WebRTC
    let (server_signal_tx, mut server_signal_rx) = mpsc::unbounded_channel();
    let (client_signal_tx, mut client_signal_rx) = mpsc::unbounded_channel();

    // Server Tube Settings
    let server_conversation_id = "e2e_conv_server_proxied_1";
    let mut server_settings = HashMap::new();
    server_settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));
    server_settings.insert("local_listen_addr".to_string(), serde_json::json!("127.0.0.1:0")); // Server tube listens here
    
    let server_response = server_registry.create_tube(
        server_conversation_id,
        server_settings.clone(),
        None,
        true,
        TEST_CALLBACK_TOKEN,
        TEST_MODE_KSM_CONFIG,
        server_signal_tx,
    ).await?;
    let server_offer = server_response.get("offer").cloned().ok_or_else(|| anyhow!("Server offer missing"))?;
    let server_tube_id = server_response.get("tube_id").cloned().ok_or_else(|| anyhow!("Server tube_id missing"))?;
    let server_tube_local_tcp_addr = server_response.get("actual_local_listen_addr").cloned()
        .ok_or_else(|| anyhow!("Server tube's actual_local_listen_addr missing"))?;
    info!("[E2E_Test] Server tube {} created. Will listen for local TCP on {}. Offer generated.", server_tube_id, server_tube_local_tcp_addr);

    // Client Tube Settings
    let client_conversation_id = "e2e_conv_client_proxied_1"; 
    let mut client_settings = HashMap::new();
    client_settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));
    // Parse the server address to extract host and port separately
    let binding = ack_server_addr.to_string();
    let addr_parts: Vec<&str> = binding.split(':').collect();
    let host = addr_parts[0];
    let port = addr_parts[1];

    client_settings.insert("target_host".to_string(), serde_json::json!(host));
    client_settings.insert("target_port".to_string(), serde_json::json!(port));

    let client_response = client_registry.create_tube(
        client_conversation_id,
        client_settings.clone(),
        Some(server_offer.clone()), // Client mode, with server's offer
        true,  // trickle_ice
        TEST_CALLBACK_TOKEN,
        TEST_MODE_KSM_CONFIG,
        client_signal_tx,
    ).await?;
    let client_answer_sdp = client_response.get("answer").cloned().ok_or_else(|| anyhow!("Client answer missing"))?;
    let client_tube_id = client_response.get("tube_id").cloned().ok_or_else(|| anyhow!("Client tube_id missing"))?;
    info!("[E2E_Test] Client tube {} created. Will connect to AckServer at {}. Answer generated.", client_tube_id, ack_server_addr);

    // Perform WebRTC Signaling
    info!("[E2E_Test] Starting WebRTC signaling...");
    perform_tube_signaling(
        &server_tube_id, &mut server_registry, &mut server_signal_rx, 
        &client_tube_id, &mut client_registry, &mut client_signal_rx, 
        client_answer_sdp
    ).await.map_err(|e| anyhow!("WebRTC signaling failed: {}", e))?;
    info!("[E2E_Test] WebRTC signaling complete.");

    // Wait for data channels to open (optional, good practice)
    let server_tube_from_reg = server_registry.get_by_tube_id(&server_tube_id).ok_or_else(|| anyhow!("Failed to get server tube from registry"))?;
    let client_tube_from_reg = client_registry.get_by_tube_id(&client_tube_id).ok_or_else(|| anyhow!("Failed to get client tube from registry"))?;
    let server_dc = server_tube_from_reg.get_data_channel(server_conversation_id).await.ok_or_else(|| anyhow!("Server DC not found"))?;
    let client_dc = client_tube_from_reg.get_data_channel(server_conversation_id).await.ok_or_else(|| anyhow!(format!("Client DC (expected label '{}') not found after processing server's offer", server_conversation_id)))?;
    
    tokio::time::timeout(Duration::from_secs(10), server_dc.wait_for_channel_open(None)).await??;
    info!("[E2E_Test] Server DC '{}' is open.", server_dc.label());
    tokio::time::timeout(Duration::from_secs(10), client_dc.wait_for_channel_open(None)).await??;
    info!("[E2E_Test] Client DC '{}' is open.", client_dc.label());

    // Simulate External Client connecting to Server Tube's local TCP endpoint
    info!("[E2E_Test] Simulating external client connecting to ServerTube's local TCP endpoint: {}", server_tube_local_tcp_addr);
    let mut external_client_stream = TcpStream::connect(&server_tube_local_tcp_addr).await
        .map_err(|e| anyhow!("External client failed to connect to ServerTube TCP {}: {}", server_tube_local_tcp_addr, e))?;
    info!("[E2E_Test] External client connected. Sending initial message.");

    let initial_message_content = "Hello Proxied World!";
    let initial_bytes = Bytes::from(initial_message_content);
    external_client_stream.write_all(&initial_bytes).await
        .map_err(|e| anyhow!("External client failed to write: {}", e))?;
    info!("[E2E_Test] External client sent: '{}'", initial_message_content);
    
    // The server tube reads this, sends CONTROL_OPEN_CONNECTION, then the data via WebRTC.
    // The client tube receives CONTROL_OPEN_CONNECTION, connects to AckServer, then forwards WebRTC data to AckServer.
    // AckServer acknowledges it, client tube reads ack, sends it via WebRTC to server tube.
    // Server tube receives acked data, writes to external_client_stream

    info!("[E2E_Test] Waiting for final acked message via external_client_stream (from ServerTube's on_message)...");
    
    // Also, check if the external client received the acked message directly
    let mut external_client_buffer = vec![0; 1024];
    match tokio::time::timeout(Duration::from_secs(5), external_client_stream.read(&mut external_client_buffer)).await {
        Ok(Ok(0)) => return Err(anyhow!("[E2E_Test] External client stream closed prematurely (EOF) before receiving ack.").into()),
        Ok(Ok(n)) => { // Catches n > 0 since n=0 is handled above
            let received_on_external_client = &external_client_buffer[..n];
            let expected_acked_content = format!("{} ack", initial_message_content);
            info!("[E2E_Test] External client received: '{}'", String::from_utf8_lossy(received_on_external_client));
            assert_eq!(String::from_utf8_lossy(received_on_external_client), expected_acked_content, "Final acked message mismatch on external client stream");
            info!("[E2E_Test] SUCCESS! External client received expected acked message directly.");
        }
        Ok(Err(e)) => return Err(anyhow!("[E2E_Test] Error reading from external client stream: {}", e).into()),
        Err(_) => return Err(anyhow!("[E2E_Test] Timeout waiting for external client to receive acked message.").into()),
    }

    info!("[E2E_Test] Shutting down Ack server...");
    let _ = ack_server_actual_kill_tx_for_test.send(()); 
    tokio::time::sleep(Duration::from_millis(200)).await; // Give time for the server to shut down
    
    server_registry.close_tube(&server_tube_id).await?;
    client_registry.close_tube(&client_tube_id).await?;
    info!("[E2E_Test] Test finished.");

    Ok(())
} 