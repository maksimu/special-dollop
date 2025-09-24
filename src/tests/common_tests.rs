//! Common test utilities and setup
#![cfg(test)]
use crate::webrtc_core::format_ice_candidate;
use crate::webrtc_data_channel::WebRTCDataChannel;
use log::debug;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Duration;
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;

// Helper function to create a test WebRTC data channel
pub async fn create_test_webrtc_data_channel() -> WebRTCDataChannel {
    println!("create_test_webrtc_data_channel: Setting up mock WebRTC environment");
    let (tx, mut rx) = mpsc::channel::<WebRTCDataChannel>(1);
    let (error_tx, mut error_rx) = mpsc::channel::<String>(1);

    // Spawn an OS thread to perform the setup in its own Tokio runtime
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            match setup_connected_data_channel_pair().await {
                Ok(dc) => {
                    if tx.send(dc).await.is_err() {
                        eprintln!("create_test_webrtc_data_channel: Receiver dropped before channel was sent");
                    }
                    // Don't drop error_tx immediately, let it be dropped naturally
                }
                Err(e) => {
                    let err_msg = format!("create_test_webrtc_data_channel: Failed to setup channel pair: {}", e);
                    eprintln!("{}", err_msg);
                    if error_tx.send(err_msg).await.is_err() {
                         eprintln!("create_test_webrtc_data_channel: Error receiver also dropped");
                    }
                }
            }
        });
    });

    // Asynchronously wait for the result from the spawned thread
    let result = tokio::select! {
        res = rx.recv() => {
            match res {
                Some(dc) => Ok(dc),
                None => Err("Failed to receive WebRTCDataChannel from setup task".to_string())
            }
        },
        err_res = error_rx.recv() => {
            match err_res {
                Some(err) => Err(err),
                None => {
                    // Error channel closed without error - wait for success result
                    match rx.recv().await {
                        Some(dc) => Ok(dc),
                        None => Err("Setup task completed but no result received".to_string())
                    }
                }
            }
        },
        _ = tokio::time::sleep(Duration::from_secs(25)) => Err("Timeout waiting for WebRTCDataChannel setup (increased to 25s)".to_string()),
    };

    match result {
        Ok(dc) => {
            println!("create_test_webrtc_data_channel: Successfully created and received WebRTCDataChannel");
            dc
        }
        Err(e) => {
            panic!("create_test_webrtc_data_channel: Error during setup: {}", e);
        }
    }
}

async fn setup_connected_data_channel_pair() -> anyhow::Result<WebRTCDataChannel> {
    debug!("Setting up connected peer connection and data channel for test");

    let pc1_config = RTCConfiguration::default();
    let pc2_config = RTCConfiguration::default();

    let pc1 = Arc::new(create_peer_connection(pc1_config).await?);
    let pc2 = Arc::new(create_peer_connection(pc2_config).await?);

    let (pc1_ice_candidate_tx, mut pc1_ice_candidate_rx) = mpsc::unbounded_channel();
    let (pc2_ice_candidate_tx, mut pc2_ice_candidate_rx) = mpsc::unbounded_channel();

    // Clone senders for the on_ice_candidate closures
    let pc1_ice_tx_clone = pc1_ice_candidate_tx.clone();
    pc1.on_ice_candidate(Box::new(move |candidate| {
        let tx = pc1_ice_tx_clone.clone(); // Further clone for this specific async block
        Box::pin(async move {
            if let Some(candidate) = candidate {
                if tx.send(candidate).is_err() {
                    debug!("PC1: ICE candidate receiver dropped");
                }
            }
        })
    }));

    let pc2_ice_tx_clone = pc2_ice_candidate_tx.clone();
    pc2.on_ice_candidate(Box::new(move |candidate| {
        let tx = pc2_ice_tx_clone.clone(); // Further clone for this specific async block
        Box::pin(async move {
            if let Some(candidate) = candidate {
                if tx.send(candidate).is_err() {
                    debug!("PC2: ICE candidate receiver dropped");
                }
            }
        })
    }));

    let (dc1_open_tx, dc1_open_rx) = oneshot::channel::<Arc<RTCDataChannel>>();
    let dc1_open_tx = Arc::new(StdMutex::new(Some(dc1_open_tx)));

    let dc1_label = "test_dc";
    let dc1 = pc1.create_data_channel(dc1_label, None).await?;
    let dc1_for_on_open_label = dc1.clone(); // Clone for label
    let dc1_for_on_open_send = dc1.clone(); // Clone for sending
    dc1.on_open(Box::new(move || {
        debug!(
            "PC1: DataChannel '{}' opened",
            dc1_for_on_open_label.label()
        );
        if let Some(tx) = dc1_open_tx.lock().unwrap().take() {
            if tx.send(dc1_for_on_open_send).is_err() {
                // Send the cloned dc
                debug!("PC1: DataChannel open signal receiver dropped");
            }
        }
        Box::pin(async {})
    }));

    let (dc2_open_tx, dc2_open_rx) = oneshot::channel::<Arc<RTCDataChannel>>();
    let dc2_open_tx_arc = Arc::new(StdMutex::new(Some(dc2_open_tx)));

    pc2.on_data_channel(Box::new(move |dc2| {
        let dc2_label_clone = dc2.clone(); // Clone for label
        let dc2_send_clone = dc2.clone(); // Clone for sending
        let dc2_open_tx_clone_for_closure = dc2_open_tx_arc.clone(); // Clone Arc for the on_open closure

        debug!("PC2: Received DataChannel '{}'", dc2_label_clone.label());

        dc2.on_open(Box::new(move || {
            debug!("PC2: DataChannel '{}' opened", dc2_label_clone.label()); // Use cloned dc2 for a label
            if let Some(tx) = dc2_open_tx_clone_for_closure.lock().unwrap().take() {
                if tx.send(dc2_send_clone).is_err() {
                    // Send the other cloned dc2
                    debug!("PC2: DataChannel open signal receiver dropped");
                }
            }
            Box::pin(async {})
        }));
        Box::pin(async {})
    }));

    let offer = pc1.create_offer(None).await?;
    pc1.set_local_description(offer.clone()).await?;
    pc2.set_remote_description(offer).await?;

    let answer = pc2.create_answer(None).await?;
    pc2.set_local_description(answer.clone()).await?;
    pc1.set_remote_description(answer).await?;

    let pc1_drain_ice = async {
        while let Some(candidate) = pc1_ice_candidate_rx.recv().await {
            match candidate.to_json() {
                Ok(ice_init) => {
                    if pc2.add_ice_candidate(ice_init).await.is_err() {
                        debug!("PC1->PC2: Error adding ICE candidate");
                    }
                }
                Err(e) => {
                    debug!("PC1: Failed to convert ICE candidate to JSON: {}", e);
                }
            }
        }
    };

    let pc2_drain_ice = async {
        while let Some(candidate) = pc2_ice_candidate_rx.recv().await {
            match candidate.to_json() {
                Ok(ice_init) => {
                    if pc1.add_ice_candidate(ice_init).await.is_err() {
                        debug!("PC2->PC1: Error adding ICE candidate");
                    }
                }
                Err(e) => {
                    debug!("PC2: Failed to convert ICE candidate to JSON: {}", e);
                }
            }
        }
    };

    let overall_timeout = Duration::from_secs(10);

    tokio::select! {
        biased;
        join_output = async { tokio::join!(dc1_open_rx, dc2_open_rx) } => {
            let (res1, res2) = join_output;
            let opened_dc1 = res1.map_err(|e| anyhow::anyhow!("DC1 open signal failed: {}", e))?;
            let _opened_dc2 = res2.map_err(|e| anyhow::anyhow!("DC2 open signal failed: {}", e))?;
            debug!("Both data channels reported open.");

            // These are the original senders from the outer scope, now safe to drop.
            drop(pc1_ice_candidate_tx);
            drop(pc2_ice_candidate_tx);

            Ok(WebRTCDataChannel::new(opened_dc1))
        }
        _ice_drain_outcome = async { tokio::join!(pc1_drain_ice, pc2_drain_ice) } => {
            debug!("ICE draining tasks completed before data channels opened. This might be okay if DCs open shortly.");
            futures::future::pending::<()>().await;
            anyhow::bail!("ICE drained but DC did not open (should be caught by timeout or DC open arm)");
        }
        _ = tokio::time::sleep(overall_timeout) => {
            anyhow::bail!("Timeout waiting for data channel to open and ICE to complete")
        }
    }
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
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
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
    let timeout = tokio::time::sleep(Duration::from_secs(5));
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
