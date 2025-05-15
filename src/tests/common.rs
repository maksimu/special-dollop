//! Common test utilities and setup
#![cfg(test)]
use crate::webrtc_core::format_ice_candidate;
use crate::webrtc_data_channel::WebRTCDataChannel;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;

// Helper function to create a test WebRTC data channel
pub fn create_test_webrtc_data_channel() -> WebRTCDataChannel {
    // Create an unreusable, but properly structured, data channel for testing
    let data_channel = RTCDataChannel::default();
    
    // Wrap it in the WebRTCDataChannel implementation
    WebRTCDataChannel::new(Arc::new(data_channel))
}

// Helper function to create a peer connection
pub async fn create_peer_connection(
    config: RTCConfiguration,
) -> webrtc::error::Result<RTCPeerConnection> {
    let api = APIBuilder::new().build();
    api.new_peer_connection(config).await
}

// Helper function to exchange ICE candidates
pub async fn exchange_ice_candidates(peer1: Arc<RTCPeerConnection>, peer2: Arc<RTCPeerConnection>) {
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

// Helper function to exchange ICE candidates between tubes
pub async fn exchange_tube_ice_candidates(tube1: &Arc<crate::tube::Tube>, tube2: &Arc<crate::tube::Tube>) -> anyhow::Result<()> {
    println!("Starting ICE candidate exchange between tubes: {} and {}", tube1.id(), tube2.id());
    
    // Get peer connections (now properly awaiting the futures)
    let pc1_option = tube1.peer_connection().await;
    let pc2_option = tube2.peer_connection().await;

    if let (Some(pc1), Some(pc2)) = (pc1_option, pc2_option) {
        println!("Got peer connections for both tubes");
        
        // Collect ICE candidates from both peers
        let (tx1, mut rx1) = mpsc::channel(100);
        let (tx2, mut rx2) = mpsc::channel(100);
        
        // Track when candidate gathering is complete
        let (gathering_complete_tx1, gathering_complete_rx1) = tokio::sync::oneshot::channel();
        let (gathering_complete_tx2, gathering_complete_rx2) = tokio::sync::oneshot::channel();
        let gathering_complete_tx1 = Arc::new(StdMutex::new(Some(gathering_complete_tx1)));
        let gathering_complete_tx2 = Arc::new(StdMutex::new(Some(gathering_complete_tx2)));

        // Set up ICE candidate handlers
        let gathering_tx1 = gathering_complete_tx1.clone();
        pc1.peer_connection.on_ice_candidate(Box::new(move |c| {
            let tx = tx1.clone();
            let gathering_tx = gathering_tx1.clone();
            Box::pin(async move {
                if let Some(c) = c {
                    let candidate_str = format_ice_candidate(&c);
                    println!("Tube1 generated ICE candidate: {}", candidate_str);
                    let _ = tx.send(candidate_str).await;
                } else {
                    println!("Tube1 finished gathering ICE candidates");
                    // Signal completion of candidate gathering
                    if let Some(tx) = gathering_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        }));

        let gathering_tx2 = gathering_complete_tx2.clone();
        pc2.peer_connection.on_ice_candidate(Box::new(move |c| {
            let tx = tx2.clone();
            let gathering_tx = gathering_tx2.clone();
            Box::pin(async move {
                if let Some(c) = c {
                    let candidate_str = format_ice_candidate(&c);
                    println!("Tube2 generated ICE candidate: {}", candidate_str);
                    let _ = tx.send(candidate_str).await;
                } else {
                    println!("Tube2 finished gathering ICE candidates");
                    // Signal completion of candidate gathering
                    if let Some(tx) = gathering_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        }));

        // Wait for ICE candidates to be gathered (with increased timeout)
        println!("Waiting for ICE candidates to be gathered (up to 20 seconds)");
        let timeout_duration = tokio::time::Duration::from_secs(20);
        
        // Flush the current queued up candidates first
        process_candidates(&tube1, &tube2, &mut rx1, &mut rx2, 20).await;
        
        // Now wait for complete ICE gathering with increased timeout
        let _ = tokio::time::timeout(
            timeout_duration.clone(), 
            futures::future::join_all(vec![
                tokio::spawn(async move { let _ = gathering_complete_rx1.await; }),
                tokio::spawn(async move { let _ = gathering_complete_rx2.await; })
            ])
        ).await;

        println!("Processing and exchanging all available ICE candidates...");
        
        // Process candidates from both sides with increased limits
        process_candidates(&tube1, &tube2, &mut rx1, &mut rx2, 50).await;
        
        // Wait a bit for candidates to be processed
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        
        Ok(())
    } else {
        anyhow::bail!("Failed to get peer connections for ICE exchange")
    }
}

// Helper function to process candidates bidirectionally
async fn process_candidates(
    tube1: &Arc<crate::tube::Tube>,
    tube2: &Arc<crate::tube::Tube>,
    rx1: &mut mpsc::Receiver<String>,
    rx2: &mut mpsc::Receiver<String>,
    max_candidates: usize
) {
    // Process candidates from tube1 to tube2
    let mut count1 = 0;
    println!("Processing candidates from tube1 to tube2");
    
    while count1 < max_candidates {
        match tokio::time::timeout(tokio::time::Duration::from_millis(200), rx1.recv()).await {
            Ok(Some(candidate)) => {
                println!("Adding ICE candidate from tube1 to tube2: {}", candidate);
                if let Err(e) = tube2.add_ice_candidate(candidate.clone()).await {
                    println!("Error adding ICE candidate to tube2: {}", e);
                } else {
                    count1 += 1;
                }
            },
            _ => break,
        }
    }

    // Process candidates from tube2 to tube1
    let mut count2 = 0;
    println!("Processing candidates from tube2 to tube1");
    
    while count2 < max_candidates {
        match tokio::time::timeout(tokio::time::Duration::from_millis(200), rx2.recv()).await {
            Ok(Some(candidate)) => {
                println!("Adding ICE candidate from tube2 to tube1: {}", candidate);
                if let Err(e) = tube1.add_ice_candidate(candidate.clone()).await {
                    println!("Error adding ICE candidate to tube1: {}", e);
                } else {
                    count2 += 1;
                }
            },
            _ => break,
        }
    }

    println!("Exchanged {} candidates from tube1 to tube2", count1);
    println!("Exchanged {} candidates from tube2 to tube1", count2);
}
