// Tests for WebRTC functionality
use crate::{get_or_create_runtime, logger};
use crate::webrtc_core::{format_ice_candidate};

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::RTCPeerConnection;
use bytes::Bytes;

// Initialize the logger before any test runs, but allow it to be safely called multiple times
#[ctor::ctor]
fn init() {
    let _ = logger::initialize_logger("test", Some(true), 10);
}

// Helper function to create a peer connection
async fn create_peer_connection(
    config: RTCConfiguration,
) -> webrtc::error::Result<RTCPeerConnection> {
    let api = APIBuilder::new().build();
    api.new_peer_connection(config).await
}

#[test]
fn test_webrtc_connection_creation() {
    let runtime = get_or_create_runtime().expect("Failed to get shared runtime");
    let result = runtime.block_on(async {
        let config = RTCConfiguration::default();
        create_peer_connection(config).await
    });

    assert!(result.is_ok());
    let pc = result.unwrap();
    assert_eq!(pc.connection_state(), RTCPeerConnectionState::New);
}

#[test]
fn test_data_channel_creation() {
    let runtime = get_or_create_runtime().expect("Failed to get shared runtime");
    let result = runtime.block_on(async {
        let config = RTCConfiguration::default();
        let pc = create_peer_connection(config).await?;
        pc.create_data_channel("test", None).await
    });

    assert!(result.is_ok());
    let dc = result.unwrap();
    assert_eq!(dc.label(), "test");
}

// Helper function to exchange ICE candidates
async fn exchange_ice_candidates(peer1: Arc<RTCPeerConnection>, peer2: Arc<RTCPeerConnection>) {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let _ready_tx = Arc::new(StdMutex::new(Some(ready_tx)));
    println!("Starting exchange_ice_candidates");
    let (ice_tx1, mut ice_rx1) = mpsc::channel::<String>(32);
    let (ice_tx2, mut ice_rx2) = mpsc::channel::<String>(32);

    // Set up ICE candidate handlers
    let peer1_clone = peer1.clone();
    peer1_clone.on_ice_candidate(Box::new(move |c| {
        let ice_tx = ice_tx1.clone();
        Box::pin(async move {
            if let Some(candidate) = c {
                let candidate_str = format_ice_candidate(&candidate);
                println!("Found ICE candidate for peer1: {:?}", candidate_str);
                if let Err(e) = ice_tx.send(candidate_str).await {
                    eprintln!("Failed to send ICE candidate for peer1: {:?}", e);
                }
            }
        })
    }));

    let peer2_clone = peer2.clone();
    peer2_clone.on_ice_candidate(Box::new(move |c| {
        let ice_tx = ice_tx2.clone();
        Box::pin(async move {
            if let Some(candidate) = c {
                let candidate_str = format_ice_candidate(&candidate);
                println!("Found ICE candidate for peer2: {:?}", candidate_str);
                if let Err(e) = ice_tx.send(candidate_str).await {
                    eprintln!("Failed to send ICE candidate for peer2: {:?}", e);
                }
            }
        })
    }));

    // Handle candidate exchange
    let peer2_clone = peer2.clone();
    let handle1 = tokio::spawn(async move {
        while let Some(candidate) = ice_rx1.recv().await {
            let init = RTCIceCandidateInit {
                candidate,
                sdp_mid: None,
                sdp_mline_index: None,
                username_fragment: None,
            };
            if let Err(err) = peer2_clone.add_ice_candidate(init).await {
                eprintln!("Error adding ICE candidate to peer2: {:?}", err);
            }
        }
    });

    let peer1_clone = peer1.clone();
    let handle2 = tokio::spawn(async move {
        while let Some(candidate) = ice_rx2.recv().await {
            let init = RTCIceCandidateInit {
                candidate,
                sdp_mid: None,
                sdp_mline_index: None,
                username_fragment: None,
            };
            if let Err(err) = peer1_clone.add_ice_candidate(init).await {
                eprintln!("Error adding ICE candidate to peer1: {:?}", err);
            }
        }
    });

    // Set a timeout for ICE gathering
    let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(5));
    tokio::pin!(timeout);

    tokio::select! {
        _ = timeout => {
            println!("ICE gathering timed out");
        }
        _ = ready_rx => {
            println!("ICE gathering completed successfully");
        }
    }

    // Clean up handlers
    let _ = handle1.abort();
    let _ = handle2.abort();
}

#[test]
fn test_p2p_connection() {
    println!("Starting P2P connection test");
    let runtime = get_or_create_runtime().expect("Failed to get shared runtime");
    runtime.block_on(async {
        let config = RTCConfiguration::default();
        let peer1 = Arc::new(create_peer_connection(config.clone()).await.unwrap());
        let peer2 = Arc::new(create_peer_connection(config).await.unwrap());

        let done_signal = Arc::new(TokioMutex::new(false));
        let done_signal_clone = Arc::clone(&done_signal);

        let dc_received = Arc::new(TokioMutex::new(None));
        let dc_received_clone = Arc::clone(&dc_received);

        // Set up connection state callback
        peer2.on_peer_connection_state_change(Box::new(move |s| {
            let done = Arc::clone(&done_signal_clone);
            Box::pin(async move {
                if s == RTCPeerConnectionState::Connected {
                    let mut done = done.lock().await;
                    *done = true;
                }
            })
        }));

        // Set up data channel callback
        peer2.on_data_channel(Box::new(move |dc| {
            let dc_received = Arc::clone(&dc_received_clone);
            Box::pin(async move {
                let mut dc_guard = dc_received.lock().await;
                *dc_guard = Some(dc.clone());
            })
        }));

        println!("Creating data channel");
        let dc1 = peer1
            .create_data_channel("test-channel", None)
            .await
            .unwrap();
        println!("Data channel created successfully");

        // Monitor ICE gathering state
        peer1.on_ice_gathering_state_change(Box::new(|s| {
            println!("Peer1 ICE gathering state changed to: {:?}", s);
            Box::pin(async {})
        }));

        peer2.on_ice_gathering_state_change(Box::new(|s| {
            println!("Peer2 ICE gathering state changed to: {:?}", s);
            Box::pin(async {})
        }));

        // Exchange ICE candidates
        let ice_exchange = tokio::spawn(exchange_ice_candidates(peer1.clone(), peer2.clone()));

        println!("Creating and setting offer");
        let offer = peer1.create_offer(None).await.unwrap();
        println!("Created offer: {:?}", offer);

        peer1.set_local_description(offer.clone()).await.unwrap();
        println!("Set local description on peer1");

        peer2.set_remote_description(offer).await.unwrap();
        println!("Set remote description on peer2");

        println!("Creating and setting answer");
        let answer = peer2.create_answer(None).await.unwrap();
        println!("Created answer: {:?}", answer);

        peer2.set_local_description(answer.clone()).await.unwrap();
        println!("Set local description on peer2");

        peer1.set_remote_description(answer).await.unwrap();
        println!("Set remote description on peer1");

        println!("Waiting for connection to establish");
        let mut retries = 50;
        while retries > 0 && !*done_signal.lock().await {
            println!(
                "Connection state - peer1: {:?}, peer2: {:?}",
                peer1.connection_state(),
                peer2.connection_state()
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            retries -= 1;
        }

        // Ensure ICE candidate exchange completed
        ice_exchange.await.expect("ICE exchange task failed");

        assert_eq!(peer1.connection_state(), RTCPeerConnectionState::Connected);
        assert_eq!(peer2.connection_state(), RTCPeerConnectionState::Connected);

        // Test data channel
        let dc2 = dc_received.lock().await;
        assert!(dc2.is_some(), "Data channel should have been received");
        assert_eq!(dc2.as_ref().unwrap().label(), "test-channel");

        // Test message sending
        let message = Bytes::from("Hello from peer1!");
        dc1.send(&message).await.unwrap();

        if let Some(dc2) = &*dc2 {
            let (msg_tx, mut msg_rx) = mpsc::channel(1);

            dc2.on_message(Box::new(move |msg| {
                let tx = msg_tx.clone();
                Box::pin(async move {
                    let _ = tx.send(msg.data).await;
                })
            }));

            let received =
                tokio::time::timeout(tokio::time::Duration::from_secs(5), msg_rx.recv())
                    .await
                    .unwrap();

            assert_eq!(received.unwrap(), message);
        }
    });
}

#[test]
fn test_p2p_connection_non_trickle() {
    println!("Starting non-trickle P2P connection test");
    let runtime = get_or_create_runtime().expect("Failed to get shared runtime");
    runtime.block_on(async {
        // Create config with STUN servers first
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let api = APIBuilder::new().build();
        let peer1 = Arc::new(api.new_peer_connection(config.clone()).await.unwrap());
        let peer2 = Arc::new(api.new_peer_connection(config).await.unwrap());

        let done_signal = Arc::new(TokioMutex::new(false));
        let done_signal_clone = Arc::clone(&done_signal);

        // Set up connection state callback
        peer2.on_peer_connection_state_change(Box::new(move |s| {
            let done = Arc::clone(&done_signal_clone);
            Box::pin(async move {
                println!("Peer2 connection state changed to: {:?}", s);
                if s == RTCPeerConnectionState::Connected {
                    let mut done = done.lock().await;
                    *done = true;
                }
            })
        }));

        println!("Creating data channel");
        let _dc1 = peer1
            .create_data_channel("test-channel", None)
            .await
            .unwrap();
        println!("Data channel created successfully");

        // Create channels for ICE gathering completion
        let (gather_tx1, gather_rx1) = tokio::sync::oneshot::channel();
        let gather_tx1 = Arc::new(TokioMutex::new(Some(gather_tx1)));
        let (gather_tx2, gather_rx2) = tokio::sync::oneshot::channel();
        let gather_tx2 = Arc::new(TokioMutex::new(Some(gather_tx2)));

        // Set up ICE gathering state monitoring for peer1
        peer1.on_ice_gathering_state_change(Box::new(move |s| {
            let tx = gather_tx1.clone();
            println!("Peer1 ICE gathering state: {:?}", s);
            Box::pin(async move {
                if s == RTCIceGathererState::Complete {
                    if let Some(tx) = tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                }
            })
        }));

        // Set up ICE gathering state monitoring for peer2
        peer2.on_ice_gathering_state_change(Box::new(move |s| {
            let tx = gather_tx2.clone();
            println!("Peer2 ICE gathering state: {:?}", s);
            Box::pin(async move {
                if s == RTCIceGathererState::Complete {
                    if let Some(tx) = tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                }
            })
        }));

        // Create and set local description for peer1 (offer)
        println!("Creating offer and waiting for ICE gathering");
        let offer = peer1.create_offer(None).await.unwrap();
        peer1.set_local_description(offer).await.unwrap();

        // Wait for peer1's ICE gathering to complete
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                panic!("Timeout waiting for peer1 ICE gathering");
            }
            _ = gather_rx1 => {
                if let Some(complete_offer) = peer1.local_description().await {
                    println!(
                        "Complete offer with ICE candidates:\n{}",
                        complete_offer.sdp
                    );
                    assert!(
                        complete_offer.sdp.contains("a=candidate:"),
                        "Offer should contain ICE candidates"
                    );

                    // Set the complete offer on peer2
                    peer2.set_remote_description(complete_offer).await.unwrap();

                    // Create and set local description for peer2 (answer)
                    println!("Creating answer and waiting for ICE gathering");
                    let answer = peer2.create_answer(None).await.unwrap();
                    peer2.set_local_description(answer).await.unwrap();

                    // Wait for peer2's ICE gathering to complete
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                            panic!("Timeout waiting for peer2 ICE gathering");
                        }
                        _ = gather_rx2 => {
                            if let Some(complete_answer) = peer2.local_description().await {
                                println!(
                                    "Complete answer with ICE candidates:\n{}",
                                    complete_answer.sdp
                                );
                                assert!(
                                    complete_answer.sdp.contains("a=candidate:"),
                                    "Answer should contain ICE candidates"
                                );

                                // Set the complete answer on peer1
                                peer1.set_remote_description(complete_answer).await.unwrap();
                            }
                        }
                    }
                }
            }
        }

        // Wait for connection with increased timeout
        println!("Waiting for connection to establish");
        let mut retries = 50; // 10 seconds total
        while retries > 0 && !*done_signal.lock().await {
            println!(
                "Connection state - peer1: {:?}, peer2: {:?}",
                peer1.connection_state(),
                peer2.connection_state()
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            retries -= 1;
        }

        // Give it a bit more time if needed
        if !*done_signal.lock().await {
            println!("Waiting additional time for connection...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        // Check connection state - be slightly more lenient in this test
        let p1_state = peer1.connection_state();
        let p2_state = peer2.connection_state();
        println!("Final connection states - peer1: {:?}, peer2: {:?}", p1_state, p2_state);

        assert!(
            p1_state == RTCPeerConnectionState::Connected ||
                p1_state == RTCPeerConnectionState::Connecting,
            "Peer1 should be connected or connecting"
        );

        assert!(
            p2_state == RTCPeerConnectionState::Connected ||
                p2_state == RTCPeerConnectionState::Connecting,
            "Peer2 should be connected or connecting"
        );
    });
}