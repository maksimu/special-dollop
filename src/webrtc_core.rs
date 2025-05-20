use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use log::{debug, info, warn};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use tokio::sync::mpsc::UnboundedSender;
use crate::tube_registry::SignalMessage;

// Cached API instance for reuse
static API: once_cell::sync::Lazy<webrtc::api::API> =
    once_cell::sync::Lazy::new(|| APIBuilder::new().build());

// Utility for formatting ICE candidates as strings with the pre-allocated capacity
pub fn format_ice_candidate(
    candidate: &RTCIceCandidate,
) -> String {
    // Use a single format! macro for better efficiency
    if candidate.related_address.is_empty() {
        format!(
            "candidate:{} {} {} {} {} {} typ {}",
            candidate.foundation,
            candidate.component,
            candidate.protocol.to_string().to_lowercase(),
            candidate.priority,
            candidate.address,
            candidate.port,
            candidate.typ.to_string().to_lowercase()
        )
    } else {
        format!(
            "candidate:{} {} {} {} {} {} typ {} raddr {} rport {}",
            candidate.foundation,
            candidate.component,
            candidate.protocol.to_string().to_lowercase(),
            candidate.priority,
            candidate.address,
            candidate.port,
            candidate.typ.to_string().to_lowercase(),
            candidate.related_address,
            candidate.related_port
        )
    }
}

// Helper function to create a WebRTC peer connection with cached API
pub async fn create_peer_connection(
    config: Option<RTCConfiguration>,
) -> webrtc::error::Result<RTCPeerConnection> {
    // Use the configuration as provided or default
    let actual_config = config.unwrap_or_default();
    
    // Use the cached API instance instead of creating a new one each time
    API.new_peer_connection(actual_config).await
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

    peer_connection
        .create_data_channel(label, Some(config))
        .await
}

// Async-first wrapper for core WebRTC operations
pub struct WebRTCPeerConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    trickle_ice: bool,
    is_closing: Arc<AtomicBool>,
    ksm_config: String,
    answer_sent: Arc<AtomicBool>,
    pending_ice_candidates: Arc<Mutex<Vec<String>>>,
    signal_sender: Option<UnboundedSender<SignalMessage>>,
}

impl WebRTCPeerConnection {
    pub async fn new(
        config: Option<RTCConfiguration>, 
        trickle_ice: bool, 
        turn_only: bool,
        ksm_config: String,
        signal_sender: Option<UnboundedSender<SignalMessage>>,
    ) -> Result<Self, String> {
        // Use the provided configuration or default
        let mut actual_config = config.unwrap_or_default();
        
        // Apply ICE transport policy settings based on the turn_only flag
        if turn_only {
            // If turn_only, force use of relay candidates only
            actual_config.ice_transport_policy = webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay;
        } else {
            // Otherwise use all candidates
            actual_config.ice_transport_policy = webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All;
        }
        
        // Create peer connection
        let peer_connection = create_peer_connection(Some(actual_config.clone())).await
            .map_err(|e| format!("Failed to create peer connection: {}", e))?;
        
        // Store the closing state and signal channel
        let is_closing = Arc::new(AtomicBool::new(false));
        let pending_ice_candidates = Arc::new(Mutex::new(Vec::new()));
        
        // Create an Arc<RTCPeerConnection> first
        let pc_arc = Arc::new(peer_connection);
        let is_closing_clone = Arc::clone(&is_closing);
        
        // Set up a connection state change handler
        pc_arc.on_peer_connection_state_change(Box::new(move |state| {
            let is_closing = Arc::clone(&is_closing_clone);
            Box::pin(async move {
                debug!("Peer connection state changed to {}", state);
                
                // Update the closing flag if needed
                if state == RTCPeerConnectionState::Closed || state == RTCPeerConnectionState::Failed {
                    is_closing.store(true, Ordering::Release);
                }
            })
        }));
        
        // No longer setting up ICE candidate handler here - this will be done in setup_ice_candidate_handler
        // to avoid duplicate handlers
        
        // Return the new WebRTCPeerConnection struct
        Ok(Self {
            peer_connection: pc_arc,
            trickle_ice,
            is_closing,
            ksm_config,
            answer_sent: Arc::new(AtomicBool::new(false)),
            pending_ice_candidates,
            signal_sender,
        })
    }

    // Method to set up ICE candidate handler with channel-based signaling
    pub fn setup_ice_candidate_handler(
        &self,
        tube_id: String,
        conversation_id: String,
    ) {
        // Handle ICE candidates only when using trickle ICE
        if !self.trickle_ice {
            debug!("Not setting up ICE candidate handler - trickle ICE is disabled for tube {} conv {}", tube_id, conversation_id);
            return;
        }
        info!("[ICE_HANDLER_SETUP] Setting up ICE candidate handler for tube {} conv {} (trickle_ice: {})", tube_id, conversation_id, self.trickle_ice);
        
        // Create a clone of self-reference for sending candidates
        let self_ref = self.clone();
        let tube_id_clone = tube_id.clone();
        let conversation_id_clone = conversation_id.clone();
        
        // Remove any existing handlers first to avoid duplicates
        self.peer_connection.on_ice_candidate(Box::new(|_| Box::pin(async {})));
        
        // Set up handler for ICE candidates
        self.peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            // CLONE FOR LOGGING - ensure tube_id_clone and conversation_id_clone are fresh for this scope if needed
            let tube_id_for_log = tube_id_clone.clone(); 
            log::info!("[ICE_CALLBACK_DEBUG] on_ice_candidate triggered for tube_id: {}. Candidate is: {:?}", tube_id_for_log, candidate);

            let self_clone = self_ref.clone();
            let tube_id_clone2 = tube_id_clone.clone(); 
            let conversation_id_clone2 = conversation_id_clone.clone();
            
            Box::pin(async move {
                if let Some(c) = candidate {
                    // Convert the ICE candidate to a string representation
                    let candidate_str = format_ice_candidate(&c);
                    debug!("New ICE candidate for tube {}: {}", tube_id_clone2, candidate_str);
                    
                    // Store the candidate for later use if an answer hasn't been sent
                    if !self_clone.answer_sent.load(Ordering::Acquire) {
                        let mut candidates_lock = self_clone.pending_ice_candidates.lock().unwrap();
                        candidates_lock.push(candidate_str.clone());
                        debug!("Stored ICE candidate for tube {} (answer not sent yet)", tube_id_clone2);
                    }
                    
                    // Send it to the signal channel if an answer has been sent, or (always if trickle ICE is on for this handler)
                    // The original logic was: if self_clone.answer_sent.load(Ordering::Acquire)
                    // For trickle ICE, we generally send candidates as they arrive after offer/answer exchange has begun.
                    // The `answer_sent` flag is more about flushing pending candidates when an answer is *sent by this peer*.
                    // If we are the offerer, we send candidates once set_remote_description(answer) is done.
                    // If we are the answerer, we send candidates once set_local_description(answer) is done (which sets answer_sent).
                    // Let's assume for now if trickle_ice is on for the PC, we always try to send.
                    // The `answer_sent` logic for flushing *pending* candidates remains in `send_answer`.
                    self_clone.send_ice_candidate(
                        &candidate_str,
                        &tube_id_clone2,
                        &conversation_id_clone2,
                    );
                } else {
                    // Candidate is None, which means ICE gathering is complete for this transport
                    debug!("All ICE candidates gathered for tube {} (received None). Sending empty candidate signal.", tube_id_clone2);
                    self_clone.send_ice_candidate(
                        "", // Empty string to signify end of candidates
                        &tube_id_clone2,
                        &conversation_id_clone2,
                    );
                }
            })
        }));
    }

    // Set or update the signal channel
    pub fn set_signal_channel(&mut self, signal_sender: UnboundedSender<SignalMessage>) {
        self.signal_sender = Some(signal_sender);
    }

    // Method to send an ICE candidate using the signal channel
    pub fn send_ice_candidate(&self, 
                             candidate: &str, 
                             tube_id: &str,
                             conversation_id: &str) {
        // Only proceed if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            // Create the ICE candidate message - use one-time allocation with format!
            // The data field of SignalMessage is just a String. We'll send the candidate string directly.
            
            // Prepare the signaling message
            let message = SignalMessage {
                tube_id: tube_id.to_string(),
                kind: "icecandidate".to_string(),
                data: candidate.to_string(), // Send the candidate string directly
                conversation_id: conversation_id.to_string(),
            };
            
            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!("Failed to send ICE candidate through signal channel: {}", e);
            }
        } else {
            debug!("No signal channel available to send ICE candidate");
        }
    }
    
    // Method to send answer to router and flush-buffered candidates
    pub fn send_answer(&self,
                       answer_sdp: &str,
                       conversation_id: &str,
                       tube_id: &str) {
        // Only send it if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            // Create and serialize the answer in one step
            let message = SignalMessage {
                tube_id: tube_id.to_string(),
                kind: "answer".to_string(),
                data: answer_sdp.to_string(), // Send the answer SDP string directly
                conversation_id: conversation_id.to_string(),
            };
            
            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!("Failed to send answer through signal channel: {}", e);
                return; // Don't process candidates if we can't send the answer
            }
        } else {
            debug!("No signal channel available to send answer");
            return; // Don't process candidates if we have no signal channel
        }
        
        // Set the answer_sent flag
        self.answer_sent.store(true, Ordering::Release);
        
        // Take the buffered candidates with a single lock operation
        let pending_candidates = {
            let mut lock = self.pending_ice_candidates.lock().unwrap();
            std::mem::take(&mut *lock)
        };
        
        // Send any buffered candidates
        if !pending_candidates.is_empty() {
            debug!("Sending {} buffered ICE candidates", pending_candidates.len());
            for candidate in pending_candidates {
                self.send_ice_candidate(&candidate, tube_id, conversation_id);
            }
        }
    }
    
    // Clone implementation for WebRTCPeerConnection
    pub fn clone(&self) -> Self {
        Self {
            peer_connection: Arc::clone(&self.peer_connection),
            trickle_ice: self.trickle_ice,
            is_closing: Arc::clone(&self.is_closing),
            ksm_config: self.ksm_config.clone(),
            answer_sent: Arc::clone(&self.answer_sent),
            pending_ice_candidates: Arc::clone(&self.pending_ice_candidates),
            signal_sender: self.signal_sender.clone(),
        }
    }

    // Create an offer (returns SDP string)
    pub async fn create_offer(&self) -> Result<String, String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Check the current signaling state
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before create_offer: {:?}", current_state);
        
        // Validate that we can create an offer in the current state
        match current_state {
            webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer => {
                return Err("Cannot create offer when already have local offer".to_string());
            },
            _ => {}
        }

        // Create the offer
        let offer = self.peer_connection
            .create_offer(None)
            .await
            .map_err(|e| format!("Failed to create offer: {}", e))?;
            
        // Convert to a string
        offer.sdp
            .parse()
            .map_err(|e| format!("Failed to parse offer SDP: {}", e))
    }

    // Create an answer (returns SDP string)
    pub async fn create_answer(&self) -> Result<String, String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Check the current signaling state
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before create_answer: {:?}", current_state);
        
        // Validate that we can create an answer in the current state
        match current_state {
            webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer => {},
            _ => {
                return Err(format!(
                    "Cannot create answer when in state {:?} - must have remote offer",
                    current_state
                ));
            }
        }

        // Create the answer
        let answer = self.peer_connection
            .create_answer(None)
            .await
            .map_err(|e| format!("Failed to create answer: {}", e))?;
            
        // Convert to a string
        answer.sdp
            .parse()
            .map_err(|e| format!("Failed to parse answer SDP: {}", e))
    }

    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            session_description::RTCSessionDescription::answer(sdp)
        } else {
            session_description::RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {}", e))?;

        // Check the current signaling state before setting the remote description
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before set_remote_description: {:?}", current_state);
        
        // Validate that the signaling state transition is valid
        let valid_transition = match (current_state, is_answer) {
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer, true) => true,
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer, false) => false, // Invalid transition
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false) => true,
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true) => false, // Invalid transition
            _ => true, // Allow other transitions
        };
        
        if !valid_transition {
            return Err(format!("Invalid proposed signaling state transition from {:?} applying {}", 
                        current_state, if is_answer { "local answer" } else { "local offer" }));
        }

        // Set the remote description
        self.peer_connection
            .set_remote_description(desc)
            .await
            .map_err(|e| format!("Failed to set remote description: {}", e))
    }

    pub async fn add_ice_candidate(&self, candidate_str: String) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }
        
        // Create the RTCIceCandidateInit
        let candidate_init = RTCIceCandidateInit {
            candidate: candidate_str,
            ..Default::default()
        };
        
        // Add a candidate to the peer connection
        self.peer_connection
            .add_ice_candidate(candidate_init)
            .await
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
            return Ok(()); // Already closing or closed
        }

        // First, clear all callbacks to prevent more mDNS lookups
        self.peer_connection
            .on_ice_candidate(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_ice_gathering_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_data_channel(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_peer_connection_state_change(Box::new(|_| Box::pin(async {})));
        self.peer_connection
            .on_signaling_state_change(Box::new(|_| Box::pin(async {})));

        // Then close the connection with a timeout to avoid hanging
        match tokio::time::timeout(Duration::from_secs(5), self.peer_connection.close()).await {
            Ok(result) => result.map_err(|e| format!("Failed to close peer connection: {}", e)),
            Err(_) => {
                warn!("Close operation timed out, forcing abandonment");
                Ok(()) // Force success even though it timed out
            }
        }
    }

    // Method to set up a simple connection state monitor for open/close events only
    pub fn setup_connection_state_monitor(&self) {
        // Set up the connection state change handler
        self.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            
            Box::pin(async move {
                let state_str = format!("{:?}", state);
                debug!("Connection state changed: {}", state_str);
                
                // Only handle Connected and Closed states for router reporting
                match state {
                    RTCPeerConnectionState::Connected => {
                        // Report connection open to router if available
                        debug!("Sending connection open callback to router");
                    },
                    RTCPeerConnectionState::Closed => {
                        info!("Connection is closed");
                        // No need to report a closed state here as it's handled by the close() method
                    },
                    _ => {}
                }
            })
        }));
    }

    // Add method to set local description for better state management
    pub async fn set_local_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            session_description::RTCSessionDescription::answer(sdp)
        } else {
            session_description::RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {}", e))?;

        // Check the current signaling state before setting the local description
        let current_state = self.peer_connection.signaling_state();
        debug!("Current signaling state before set_local_description: {:?}", current_state);
        
        // Validate that the signaling state transition is valid
        let valid_transition = match (current_state, is_answer) {
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer, true) => true,
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer, false) => false, // Invalid transition
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false) => true,
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true) => false, // Invalid transition
            _ => true, // Allow other transitions
        };
        
        if !valid_transition {
            return Err(format!("Invalid proposed signaling state transition from {:?} applying {}", 
                        current_state, if is_answer { "local answer" } else { "local offer" }));
        }

        // Set the local description
        self.peer_connection
            .set_local_description(desc)
            .await
            .map_err(|e| format!("Failed to set local description: {}", e))
    }
    
    // Get all gathered ICE candidates
    pub fn get_ice_candidates(&self) -> Vec<String> {
        let candidates = self.pending_ice_candidates.lock().unwrap();
        candidates.clone()
    }
}
