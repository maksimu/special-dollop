use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tracing::{debug, info, warn};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use crate::tube_registry::SignalMessage;

// Constants for SCTP max message size negotiation
const DEFAULT_MAX_MESSAGE_SIZE: u32 = 262144; // 256KB - Common default for WebRTC
const OUR_MAX_MESSAGE_SIZE: u32 = 65536;      // 64KB - Safe limit for webrtc-rs

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
        max_retransmits: Some(0),   // No retransmits
        max_packet_life_time: None, // No timeout for packets
        protocol: None,             // No specific protocol
        negotiated: None,           // Let WebRTC handle negotiation
    };

    debug!(
        "Creating data channel '{}' with config: ordered={:?}, reliable delivery", 
        label, config.ordered
    );

    peer_connection
        .create_data_channel(label, Some(config))
        .await
}

// Async-first wrapper for core WebRTC operations
pub struct WebRTCPeerConnection {
    pub peer_connection: Arc<RTCPeerConnection>,
    pub(crate) trickle_ice: bool,
    is_closing: Arc<AtomicBool>,
    ksm_config: String,
    answer_sent: Arc<AtomicBool>,
    pending_ice_candidates: Arc<Mutex<Vec<String>>>,
    pub(crate) signal_sender: Option<UnboundedSender<SignalMessage>>,
    pub tube_id: String,
}

impl WebRTCPeerConnection {
    // Helper function to validate signaling state transitions
    fn validate_signaling_state_transition(
        current_state: webrtc::peer_connection::signaling_state::RTCSignalingState,
        is_answer: bool,
        is_local: bool,
    ) -> Result<(), String> {
        let operation = match (is_local, is_answer) {
            (true, true) => "local answer",
            (true, false) => "local offer", 
            (false, true) => "remote answer",
            (false, false) => "remote offer",
        };
        
        let valid_transition = match (current_state, is_local, is_answer) {
            // Local descriptions
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer, true, true) => true,  // Local answer after remote offer
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer, true, false) => false, // Local offer after local offer (invalid)
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true, false) => true,          // Local offer from stable
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, true, true) => false,          // Local answer from stable (invalid)
            
            // Remote descriptions  
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer, false, true) => true,  // Remote answer after local offer
            (webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer, false, false) => false, // Remote offer after remote offer (invalid)
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false, false) => true,         // Remote offer from stable
            (webrtc::peer_connection::signaling_state::RTCSignalingState::Stable, false, true) => false,         // Remote answer from stable (invalid)
            
            _ => true, // Allow other transitions
        };
        
        if !valid_transition {
            return Err(format!(
                "Invalid signaling state transition from {:?} applying {}", 
                current_state, operation
            ));
        }
        
        Ok(())
    }
    
    pub async fn new(
        config: Option<RTCConfiguration>, 
        trickle_ice: bool, 
        turn_only: bool,
        ksm_config: String,
        signal_sender: Option<UnboundedSender<SignalMessage>>,
        tube_id: String,
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
                debug!(target: "webrtc_state", state = ?state, "Peer connection state changed");
                
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
            tube_id,
        })
    }

    // Method to set up ICE candidate handler with channel-based signaling
    pub fn setup_ice_candidate_handler(
        &self,
    ) {
        // Handle ICE candidates only when using trickle ICE
        if !self.trickle_ice {
            debug!(target: "webrtc_ice", tube_id = %self.tube_id, "Not setting up ICE candidate handler - trickle ICE is disabled");
            return;
        }
        info!(target: "webrtc_ice", tube_id = %self.tube_id, "Setting up ICE candidate handler");
        
        // Create a clone of self-reference for sending candidates
        let self_ref = self.clone();
        
        // Remove any existing handlers first to avoid duplicates
        self.peer_connection.on_ice_candidate(Box::new(|_| Box::pin(async {})));
        
        // Set up handler for ICE candidates
        self.peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            info!(target: "webrtc_ice_internal", tube_id = %self_ref.tube_id, ?candidate, "on_ice_candidate triggered");

            let self_clone = self_ref.clone();
            
            Box::pin(async move {
                if let Some(c) = candidate {
                    // Convert the ICE candidate to a string representation
                    let candidate_str = format_ice_candidate(&c);
                    info!(target: "webrtc_ice", tube_id = %self_clone.tube_id, candidate = %candidate_str, "ICE candidate gathered");
                    debug!(target: "webrtc_ice_verbose", tube_id = %self_clone.tube_id, candidate = %candidate_str, "New ICE candidate details");
                    
                    // Store the candidate for later use if an answer hasn't been sent
                    if !self_clone.answer_sent.load(Ordering::Acquire) {
                        let mut candidates_lock = self_clone.pending_ice_candidates.lock().unwrap();
                        candidates_lock.push(candidate_str.clone());
                        debug!(target: "webrtc_ice", tube_id = %self_clone.tube_id, "Stored ICE candidate (trickle_ice: {}, answer_sent: {})", self_clone.trickle_ice, self_clone.answer_sent.load(Ordering::Acquire));
                    }
                    
                    self_clone.send_ice_candidate(
                        &candidate_str,
                    );
                } else {
                    debug!(target: "webrtc_ice", tube_id = %self_clone.tube_id, "All ICE candidates gathered (received None). Sending empty candidate signal.");
                    self_clone.send_ice_candidate(
                        "", 
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
                            ) {
        // Only proceed if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            // Create the ICE candidate message - use one-time allocation with format!
            // The data field of SignalMessage is just a String. We'll send the candidate string directly.
            
            // Prepare the signaling message
            let message = SignalMessage {
                tube_id: self.tube_id.clone(),
                kind: "icecandidate".to_string(),
                data: candidate.to_string(), // Send the candidate string directly
                conversation_id: self.tube_id.clone(), // Using tube_id as conversation_id for SignalMessage
            };
            
            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!(target: "webrtc_signal", tube_id = %self.tube_id, error = %e, "Failed to send ICE candidate signal");
            }
        } else {
            warn!(target: "webrtc_signal", tube_id = %self.tube_id, "Signal sender not available for ICE candidate");
        }
    }
    
    // Method to send answer to router and flush-buffered candidates
    pub fn send_answer(&self,
                       answer_sdp: &str,
                      ) {
        // Only send it if we have a signal channel
        if let Some(sender) = &self.signal_sender {
            // Create and serialize the answer in one step
            let message = SignalMessage {
                tube_id: self.tube_id.clone(),
                kind: "answer".to_string(),
                data: answer_sdp.to_string(), // Send the answer SDP string directly
                conversation_id: self.tube_id.clone(), // Using tube_id as conversation_id for SignalMessage
            };
            
            // Try to send it, but don't fail if the channel is closed
            if let Err(e) = sender.send(message) {
                warn!(target: "webrtc_signal", tube_id = %self.tube_id, error = %e, "Failed to send answer signal");
                return; // Don't process candidates if we can't send the answer
            }
        } else {
            warn!(target: "webrtc_signal", tube_id = %self.tube_id, "Signal sender not available for answer");
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
            debug!(target: "webrtc_ice", count = pending_candidates.len(), "Sending buffered ICE candidates");
            for candidate in pending_candidates {
                self.send_ice_candidate(&candidate);
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
            tube_id: self.tube_id.clone(),
        }
    }

    pub(crate) async fn create_description_with_checks(&self, is_offer: bool) -> Result<String, String> {
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        let current_state = self.peer_connection.signaling_state();
        let sdp_type_str = if is_offer { "offer" } else { "answer" };
        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, state = ?current_state, "Current signaling state before create_{}", sdp_type_str);

        if is_offer {
            // Offer-specific signaling state validation
            match current_state {
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveLocalOffer => {
                    return if !self.trickle_ice {
                        if let Some(desc) = self.peer_connection.local_description().await {
                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Already have local offer and non-trickle, returning existing SDP");
                            Ok(desc.sdp)
                        } else {
                            Err("Cannot create offer: already have local offer but failed to retrieve it (non-trickle)".to_string())
                        }
                    } else {
                        Err("Cannot create offer when already have local offer (trickle ICE)".to_string())
                    }
                },
                _ => {} // Other states are generally fine for creating an offer
            }
        } else {
            // Answer-specific signaling state validation
            match current_state {
                webrtc::peer_connection::signaling_state::RTCSignalingState::HaveRemoteOffer => {} // This is the expected state
                _ => {
                    return Err(format!(
                        "Cannot create answer when in state {:?} - must have remote offer",
                        current_state
                    ));
                }
            }
        }

        self.generate_sdp_and_maybe_gather_ice(is_offer).await
    }

    async fn generate_sdp_and_maybe_gather_ice(&self, is_offer: bool) -> Result<String, String> {
        let sdp_type_str = if is_offer { "offer" } else { "answer" };

        let sdp_obj = if is_offer {
            self.peer_connection
                .create_offer(None)
                .await
                .map_err(|e| format!("Failed to create initial {}: {}", sdp_type_str, e))?
        } else {
            self.peer_connection
                .create_answer(None)
                .await
                .map_err(|e| format!("Failed to create initial {}: {}", sdp_type_str, e))?
        };

        if !self.trickle_ice {
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Non-trickle ICE: gathering candidates before returning {}.", sdp_type_str);

            let initial_desc = if is_offer {
                RTCSessionDescription::offer(sdp_obj.sdp.clone())
            } else {
                RTCSessionDescription::answer(sdp_obj.sdp.clone())
            }
            .map_err(|e| format!("Failed to create RTCSessionDescription for initial {}: {}", sdp_type_str, e))?;
            
            self.peer_connection
                .set_local_description(initial_desc)
                .await
                .map_err(|e| format!("Failed to set initial local description for {} (non-trickle): {}", sdp_type_str, e))?;
            
            let (tx, rx) = oneshot::channel();
            let tx_arc = Arc::new(Mutex::new(Some(tx))); // Wrap sender in Arc<Mutex<Option<T>>>
            
            let pc_clone = Arc::clone(&self.peer_connection); 
            let tube_id_clone = self.tube_id.clone(); 
            let sdp_type_str_clone = sdp_type_str.to_string(); // Clone for closure
            let captured_tx_arc = Arc::clone(&tx_arc); // Clone Arc for closure

            self.peer_connection.on_ice_gathering_state_change(Box::new(move |state: RTCIceGathererState| {
                let tx_for_handler = Arc::clone(&captured_tx_arc); // Clone Arc for the async block
                let pc_on_gather = Arc::clone(&pc_clone); 
                let tube_id_log = tube_id_clone.clone(); 
                let sdp_type_log = sdp_type_str_clone.clone(); // Clone for async block logging
                Box::pin(async move {
                    debug!(target: "webrtc_ice", tube_id = %tube_id_log, new_state = ?state, "ICE gathering state changed (non-trickle {})", sdp_type_log);
                    if state == RTCIceGathererState::Complete {
                        if let Some(sender) = tx_for_handler.lock().unwrap().take() { // Use the Arc<Mutex<Option<Sender>>>
                            let _ = sender.send(());
                        }
                        // Clear the handler after completion by setting a no-op one.
                        pc_on_gather.on_ice_gathering_state_change(Box::new(|_| Box::pin(async {})));
                    }
                })
            }));

            match tokio::time::timeout(Duration::from_secs(15), rx).await {
                Ok(Ok(_)) => {
                    debug!(target: "webrtc_ice", tube_id = %self.tube_id, "ICE gathering complete for non-trickle {}.", sdp_type_str);
                    if let Some(final_desc) = self.peer_connection.local_description().await {
                        let mut sdp_str = final_desc.sdp;
                        
                        // Add max-message-size to answer SDP for non-trickle ICE only
                        if !is_offer && !sdp_str.contains("a=max-message-size") {
                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Answer SDP missing max-message-size, attempting to add it");
                            
                            // Extract max-message-size from the offer (remote description)
                            let max_message_size = if let Some(remote_desc) = self.peer_connection.remote_description().await {
                                debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Remote description found, searching for max-message-size");
                                
                                // Extract the max-message-size from the remote offer
                                let offer_sdp = &remote_desc.sdp;
                                if let Some(pos) = offer_sdp.find("a=max-message-size:") {
                                    let start = pos + "a=max-message-size:".len();
                                    if let Some(end) = offer_sdp[start..].find('\r').or_else(|| offer_sdp[start..].find('\n')) {
                                        if let Ok(size) = offer_sdp[start..start + end].trim().parse::<u32>() {
                                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Successfully extracted max-message-size from offer: {}", size);
                                            size
                                        } else {
                                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Failed to parse max-message-size value from offer");
                                            DEFAULT_MAX_MESSAGE_SIZE // Default if parsing fails
                                        }
                                    } else {
                                        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No line ending found after max-message-size in offer");
                                        DEFAULT_MAX_MESSAGE_SIZE // Default if no line ending
                                    }
                                } else {
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No max-message-size found in remote offer SDP");
                                    DEFAULT_MAX_MESSAGE_SIZE // Default if isn't found in offer
                                }
                            } else {
                                debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "No remote description available");
                                DEFAULT_MAX_MESSAGE_SIZE // Default if no remote description
                            };

                            // Use the minimum of the client's requested size and our maximum
                            let our_max = OUR_MAX_MESSAGE_SIZE;
                            let negotiated_size = max_message_size.min(our_max);
                            
                            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Negotiating max-message-size: client_requested={} ({}KB), our_max={} ({}KB), negotiated={} ({}KB)", 
                                   max_message_size, max_message_size/1024, our_max, our_max/1024, negotiated_size, negotiated_size/1024);

                            // Find the position to insert after sctp-port
                            if let Some(sctp_pos) = sdp_str.find("a=sctp-port:") {
                                // Find the end of the sctp-port line
                                if let Some(line_end) = sdp_str[sctp_pos..].find('\n') {
                                    let insert_pos = sctp_pos + line_end + 1;
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Found sctp-port at position {}", sctp_pos);
                                    debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Inserting 'a=max-message-size:{}' at position {}", negotiated_size, insert_pos);
                                    sdp_str.insert_str(insert_pos, &format!("a=max-message-size:{}\r\n", negotiated_size));
                                    info!(target: "webrtc_sdp", tube_id = %self.tube_id, 
                                          "Successfully added max-message-size={} ({}KB) to answer SDP (client requested: {} ({}KB), our max: {} ({}KB))",
                                          negotiated_size, negotiated_size/1024, max_message_size, max_message_size/1024, our_max, our_max/1024);
                                }
                            }
                        }
                        
                        Ok(sdp_str)
                    } else {
                        Err(format!("Failed to get local description after gathering for {}", sdp_type_str))
                    }
                }
                Ok(Err(_)) => {
                    Err(format!("ICE gathering was cancelled for {}", sdp_type_str))
                }
                Err(_) => {
                    Err(format!("ICE gathering timeout for {}", sdp_type_str))
                }
            }
        } else {
            // Trickle ICE: return the SDP immediately.
            // The calling Tube will set the local description if this is an offer/answer being created by self.
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Trickle ICE: returning {} immediately.", sdp_type_str);
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, "Initial {} SDP", sdp_obj.sdp.clone());
            
            // For trickle ICE, do not modify the SDP
            Ok(sdp_obj.sdp)
        }
    }

    // Create an offer (returns SDP string)
    pub async fn create_offer(&self) -> Result<String, String> {
        self.create_description_with_checks(true).await
    }

    // Create an answer (returns SDP string)
    pub async fn create_answer(&self) -> Result<String, String> {
        self.create_description_with_checks(false).await
    }

    pub async fn set_remote_description(&self, sdp: String, is_answer: bool) -> Result<(), String> {
        // Check if closing
        if self.is_closing.load(Ordering::Acquire) {
            return Err("Connection is closing".to_string());
        }

        debug!(target: "webrtc_sdp", tube_id = %self.tube_id, 
               "set_remote_description called with {} (length: {} bytes)", 
               if is_answer { "answer" } else { "offer" }, sdp.len());
        
        // Check if the offer contains max-message-size
        if !is_answer && sdp.contains("a=max-message-size:") {
            debug!(target: "webrtc_sdp", tube_id = %self.tube_id, 
                   "Incoming offer contains max-message-size attribute");
        }

        // Create SessionDescription based on type
        let desc = if is_answer {
            RTCSessionDescription::answer(sdp)
        } else {
            RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {}", e))?;

        // Check the current signaling state before setting the remote description
        let current_state = self.peer_connection.signaling_state();
        debug!(target: "webrtc_sdp", ?current_state, "Current signaling state before set_remote_description");
        
        // Validate the signaling state transition
        Self::validate_signaling_state_transition(current_state, is_answer, false)?;

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

        // First, clear all callbacks 
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
                // The timeout elapsed.
                warn!(target: "webrtc_lifecycle", tube_id = %self.tube_id, "Close operation timed out for peer connection. The underlying webrtc-rs close() did not complete in 5 seconds.");
                // Return an error instead of Ok(())
                Err(format!("Peer connection close operation timed out for tube {}", self.tube_id))
            }
        }
    }

    // Method to set up a simple connection state monitor for open/close events only
    pub fn setup_connection_state_monitor(&self) {
        // Set up the connection state change handler
        let tube_id_clone = self.tube_id.clone();

        self.peer_connection.on_peer_connection_state_change(Box::new(move |state| {
            let tube_id_for_log = tube_id_clone.clone();
            Box::pin(async move {
                let state_str = format!("{:?}", state);
                debug!(target: "webrtc_state", tube_id = %tube_id_for_log, state = %state_str, "Connection state changed");

                match state {
                    RTCPeerConnectionState::Connected => {
                        debug!(target: "webrtc_state_report", tube_id = %tube_id_for_log, "Sending connection open callback to router");
                    },
                    RTCPeerConnectionState::Closed |
                    RTCPeerConnectionState::Failed |
                    RTCPeerConnectionState::Disconnected => {
                        info!(target: "webrtc_lifecycle", tube_id = %tube_id_for_log, state = %state_str, "Connection ended");
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
            RTCSessionDescription::answer(sdp)
        } else {
            RTCSessionDescription::offer(sdp)
        }
        .map_err(|e| format!("Failed to create session description: {}", e))?;

        // Check the current signaling state before setting the local description
        let current_state = self.peer_connection.signaling_state();
        debug!(target: "webrtc_sdp", ?current_state, "Current signaling state before set_local_description");
        
        // Validate the signaling state transition
        Self::validate_signaling_state_transition(current_state, is_answer, true)?;

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

