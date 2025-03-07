use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;

// Cached API instance for reuse
static API: once_cell::sync::Lazy<webrtc::api::API> = once_cell::sync::Lazy::new(|| {
    APIBuilder::new().build()
});

// Utility for formatting ICE candidates as strings with preallocated capacity
pub fn format_ice_candidate(candidate: &webrtc::ice_transport::ice_candidate::RTCIceCandidate) -> String {
    // Pre-allocate a reasonably sized string to avoid reallocations
    let mut result = String::with_capacity(128);

    result.push_str("candidate:");
    result.push_str(&candidate.foundation);
    result.push(' ');
    result.push_str(&candidate.component.to_string());
    result.push(' ');
    result.push_str(&candidate.protocol.to_string().to_lowercase());
    result.push(' ');
    result.push_str(&candidate.priority.to_string());
    result.push(' ');
    result.push_str(&candidate.address);
    result.push(' ');
    result.push_str(&candidate.port.to_string());
    result.push_str(" typ ");
    result.push_str(&candidate.typ.to_string().to_lowercase());

    if !candidate.related_address.is_empty() {
        result.push_str(" raddr ");
        result.push_str(&candidate.related_address);
        result.push_str(" rport ");
        result.push_str(&candidate.related_port.to_string());
    }

    result
}

// Helper function to create a WebRTC peer connection with cached API
pub async fn create_peer_connection(config: Option<RTCConfiguration>) -> webrtc::error::Result<RTCPeerConnection> {
    // Use cached API instance
    API.new_peer_connection(config.unwrap_or_default()).await
}

// Helper function to create a data channel with optimized settings
pub async fn create_data_channel(
    peer_connection: &RTCPeerConnection,
    label: &str,
) -> webrtc::error::Result<Arc<RTCDataChannel>> {
    let config = RTCDataChannelInit {
        ordered: Some(true),        // Guarantee message order
        max_retransmits: Some(0),   // Don't retransmit lost messages
        max_packet_life_time: None, // No timeout for packets
        protocol: None,             // No specific protocol
        negotiated: None,           // Let WebRTC handle negotiation
    };

    peer_connection.create_data_channel(label, Some(config)).await
}

// Async-first wrapper for core WebRTC operations
pub struct WebRTCPeerConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub trickle_ice: bool,
    is_closing: Arc<AtomicBool>,
}

impl WebRTCPeerConnection {
    pub async fn new(config: Option<RTCConfiguration>, trickle_ice: bool) -> Result<Self, String> {
        // Create the peer connection
        let peer_connection = create_peer_connection(config).await
            .map_err(|e| format!("Failed to create peer connection: {}", e))?;

        Ok(Self {
            peer_connection: Arc::new(peer_connection),
            trickle_ice,
            is_closing: Arc::new(AtomicBool::new(false)),
        })
    }

    // Common helper function for offer/answer creation with ICE gathering
    async fn create_session_description(
        &self,
        is_offer: bool,
    ) -> webrtc::error::Result<String> {
        // Create the appropriate description based on type
        let description = if is_offer {
            self.peer_connection.create_offer(None).await?
        } else {
            self.peer_connection.create_answer(None).await?
        };

        // Store the description by setting local description first
        self.peer_connection.set_local_description(description.clone()).await?;

        // For non-trickle, wait for gathering to complete
        if !self.trickle_ice {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

            self.peer_connection.on_ice_gathering_state_change(Box::new(move |s| {
                let tx = Arc::clone(&tx);
                Box::pin(async move {
                    if s == RTCIceGathererState::Complete {
                        // Take the sender in a minimal lock scope
                        let sender = {
                            let mut guard = tx.lock().await;
                            guard.take()
                        };

                        // Send outside the lock
                        if let Some(sender) = sender {
                            let _ = sender.send(());
                        }
                    }
                })
            }));

            // Wait for ICE gathering to complete with a shorter timeout to avoid hanging
            if let Ok(Ok(())) = tokio::time::timeout(Duration::from_secs(10), rx).await {
                if let Some(desc) = self.peer_connection.local_description().await {
                    return Ok(desc.sdp);
                }
            }
        }

        // For trickle ICE or if gathering completion failed, return initial SDP
        Ok(description.sdp)
    }

    pub async fn create_offer(&self) -> Result<String, String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        self.create_session_description(true).await
            .map_err(|e| format!("Failed to create offer: {}", e))
    }

    pub async fn create_answer(&self) -> Result<String, String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        self.create_session_description(false).await
            .map_err(|e| format!("Failed to create answer: {}", e))
    }

    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp)
        } else {
            webrtc::peer_connection::sdp::session_description::RTCSessionDescription::offer(sdp)
        }
            .map_err(|e| format!("Failed to create session description: {}", e))?;

        self.peer_connection.set_remote_description(desc).await
            .map_err(|e| format!("Failed to set remote description: {}", e))
    }

    pub async fn add_ice_candidate(&self, candidate: String) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid: None,
            sdp_mline_index: None,
            username_fragment: None,
        };

        self.peer_connection.add_ice_candidate(init).await
            .map_err(|e| format!("Failed to add ICE candidate: {}", e))
    }

    pub fn connection_state(&self) -> String {
        // Fast path for closing state
        if self.is_closing.load(Ordering::Acquire) {
            return "Closed".to_string();
        }

        format!("{:?}", self.peer_connection.connection_state())
    }

    pub async fn close(&self) -> Result<(), String> {
        // Avoid duplicate close operations
        if self.is_closing.swap(true, Ordering::AcqRel) {
            return Ok(());  // Already closing or closed
        }

        // First, clear all callbacks to prevent more mDNS lookups
        self.peer_connection.on_ice_candidate(Box::new(|_| Box::pin(async {})));
        self.peer_connection.on_ice_gathering_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection.on_data_channel(Box::new(|_| Box::pin(async {})));
        self.peer_connection.on_peer_connection_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection.on_signaling_state_change(Box::new(|_| Box::pin(async {})));

        // Then close the connection with a timeout to avoid hanging
        match tokio::time::timeout(Duration::from_secs(5), self.peer_connection.close()).await {
            Ok(result) => result.map_err(|e| format!("Failed to close peer connection: {}", e)),
            Err(_) => {
                log::warn!("Close operation timed out, forcing abandonment");
                Ok(()) // Force success even though it timed out
            }
        }
    }
}

// Async-first wrapper for data channel functionality
pub struct WebRTCDataChannel {
    pub data_channel: Arc<RTCDataChannel>,
    is_closing: Arc<AtomicBool>,
}


impl Clone for WebRTCDataChannel {
    fn clone(&self) -> Self {
        WebRTCDataChannel {
            data_channel: Arc::clone(&self.data_channel),
            is_closing: Arc::clone(&self.is_closing),
        }
    }
}

impl WebRTCDataChannel {
    pub fn new(data_channel: Arc<RTCDataChannel>) -> Self {
        Self {
            data_channel,
            is_closing: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn send(&self, data: &[u8]) -> Result<(), String> {
        // Early return if channel is closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Data channel is closing".to_string());
        }

        // Create Bytes from the data slice
        // Note: We could use Bytes::from_static for static data, but we don't know
        // if the input slice is actually static, so we have to copy
        let bytes = Bytes::copy_from_slice(data);

        // Send with a reasonable default timeout
        tokio::time::timeout(Duration::from_secs(5), self.data_channel.send(&bytes))
            .await
            .map_err(|_| "Send operation timed out".to_string())?
            .map(|_| ())
            .map_err(|e| format!("Failed to send data: {}", e))
    }

    // Optimized version with explicit timeout
    pub async fn send_with_timeout(&self, data: &[u8], timeout: Duration) -> Result<(), String> {
        // Early return if channel is closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Data channel is closing".to_string());
        }

        // Create Bytes from the data slice
        // Note: We could use Bytes::from_static for static data, but we don't know
        // if the input slice is actually static, so we have to copy
        let bytes = Bytes::copy_from_slice(data);

        tokio::time::timeout(timeout, self.data_channel.send(&bytes))
            .await
            .map_err(|_| "Send operation timed out".to_string())?
            .map(|_| ())
            .map_err(|e| format!("Failed to send data: {}", e))
    }

    pub async fn buffered_amount(&self) -> u64 {
        // Early return if channel is closing
        if self.is_closing.load(Ordering::Acquire) {
            return 0;
        }

        self.data_channel.buffered_amount().await as u64
    }

    pub async fn wait_for_channel_open(&self, timeout: Option<Duration>) -> Result<bool, String> {
        let timeout_duration = timeout.unwrap_or(Duration::from_secs(10)); // Reduced from 30s to 10s
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(50); // Reduced from 100ms to 50ms

        while start.elapsed() < timeout_duration {
            // Check for closing early
            if self.is_closing.load(Ordering::Acquire) {
                return Err("Data channel is closing".to_string());
            }

            // Check if already open (most common case first)
            if self.data_channel.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Open {
                return Ok(true);
            }

            // Check if channel failed (second most common case)
            if self.data_channel.ready_state() == webrtc::data_channel::data_channel_state::RTCDataChannelState::Closed {
                return Ok(false);
            }

            // Wait before next check with a shorter interval
            tokio::time::sleep(check_interval).await;
        }

        Ok(false)
    }

    pub async fn close(&self) -> Result<(), String> {
        // Avoid duplicate close operations
        if self.is_closing.swap(true, Ordering::AcqRel) {
            return Ok(());  // Already closing or closed
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