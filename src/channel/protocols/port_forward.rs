use anyhow::Result;
use futures::future::BoxFuture;
use crate::models::ProtocolHandler;
use crate::protocol::{Frame, ControlMessage};
use bytes::BytesMut;
use log::{debug, info, warn};
use bytes::Bytes;
use crate::webrtc_data_channel::WebRTCDataChannel;
use std::collections::{HashSet, VecDeque};
use crate::buffer_pool::BufferPool;

/// Structure for pending messages that couldn't be sent due to an unavailable WebRTC channel
struct PendingMessage {
    conn_no: u32,
    data: Vec<u8>,
}

/// A simple port forwarding protocol handler that just passes data through
pub struct PortForwardProtocolHandler {
    // Store multiple active connections
    active_connections: HashSet<u32>,
    // Buffer for incomplete frames
    buffer: BytesMut,
    // WebRTC data channel for sending data
    webrtc: Option<WebRTCDataChannel>,
    // Buffer pool for efficient memory usage
    buffer_pool: BufferPool,
    // Queue for pending messages when WebRTC channel isn't available
    pending_messages: VecDeque<PendingMessage>,
    // Maximum number of pending messages to store (prevent memory leak)
    max_pending_messages: usize,
}

impl PortForwardProtocolHandler {
    pub fn new() -> Self {
        Self { 
            active_connections: HashSet::new(),
            buffer: BytesMut::with_capacity(8192),
            webrtc: None,
            buffer_pool: BufferPool::default(),
            pending_messages: VecDeque::new(),
            max_pending_messages: 100, // Reasonable default
        }
    }

    // Helper function to queue a message when WebRTC channel is unavailable
    fn queue_message(&mut self, conn_no: u32, data: &[u8]) {
        // Only queue if we haven't reached the maximum (to prevent memory leaks)
        if self.pending_messages.len() < self.max_pending_messages {
            debug!("Port forward handler: Queueing message for connection {} ({} bytes)", 
                  conn_no, data.len());
            self.pending_messages.push_back(PendingMessage {
                conn_no,
                data: data.to_vec(),
            });
        } else {
            warn!("Port forward handler: Dropped message for connection {} due to queue limit", conn_no);
        }
    }
}

impl ProtocolHandler for PortForwardProtocolHandler {
    fn initialize(&mut self) -> BoxFuture<'_, Result<()>> {
        // No initialization needed for simple forwarding
        Box::pin(async { Ok(()) })
    }

    fn process_client_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            // Add new data to the buffer
            self.buffer.extend_from_slice(data);
            
            // For port forwarding, we need to pass this data to the connection's destination
            // First, create a frame to send to the destination
            let frame = Frame::new_data_with_pool(conn_no, data, &self.buffer_pool);
            let encoded_frame = frame.encode_with_pool(&self.buffer_pool);
            
            // Send the data through the WebRTC channel if available
            if let Some(webrtc) = &self.webrtc {
                // Check WebRTC state before sending
                let ready_state = webrtc.ready_state();
                if ready_state.to_lowercase() == "open" {
                    debug!("Port forward handler: Forwarding {} bytes for connection {} (client->server)", 
                        data.len(), conn_no);
                    
                    match webrtc.send(encoded_frame).await {
                        Ok(_) => {
                            debug!("Port forward handler: Successfully forwarded client data for connection {}", conn_no);
                        },
                        Err(e) => {
                            // If send fails, queue the message for later
                            warn!("Port forward handler: Failed to send data: {}, queueing for later", e);
                            self.queue_message(conn_no, data);
                        }
                    }
                } else {
                    warn!("Port forward handler: WebRTC channel not in open state ({}), queueing message for connection {}", 
                        ready_state, conn_no);
                    self.queue_message(conn_no, data);
                }
            } else {
                debug!("Port forward handler: WebRTC channel not available for connection {}, queueing message", conn_no);
                self.queue_message(conn_no, data);
            }
            
            // Return empty bytes as we've already forwarded the data
            Ok(Bytes::new())
        })
    }

    fn process_server_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            // For port forwarding, we need to wrap the response in a data frame 
            // to be sent back to the client via WebRTC
            if let Some(webrtc) = &self.webrtc {
                // Create a data frame with the received data
                let frame = Frame::new_data_with_pool(conn_no, data, &self.buffer_pool);
                let encoded = frame.encode_with_pool(&self.buffer_pool);
                
                // Check WebRTC state before sending
                let ready_state = webrtc.ready_state();
                if ready_state.to_lowercase() == "open" {
                    // Send the data through the WebRTC channel
                    debug!("Port forward handler: Forwarding {} bytes from server for connection {} (server->client)", 
                          data.len(), conn_no);
                    
                    match webrtc.send(encoded).await {
                        Ok(_) => {
                            debug!("Port forward handler: Successfully forwarded server data for connection {}", conn_no);
                        },
                        Err(e) => {
                            // If send fails, queue the message for later
                            warn!("Port forward handler: Failed to send server data: {}, queueing for later", e);
                            self.queue_message(conn_no, data);
                        }
                    }
                } else {
                    warn!("Port forward handler: WebRTC channel not in open state ({}), queueing server message for connection {}", 
                        ready_state, conn_no);
                    self.queue_message(conn_no, data);
                }
            } else {
                debug!("Port forward handler: WebRTC channel not available for connection {}, queueing server message", conn_no);
                self.queue_message(conn_no, data);
            }
            
            // Return empty bytes as we've already forwarded the data
            Ok(Bytes::new())
        })
    }

    fn handle_command<'a>(&'a mut self, command: &'a str, params: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match command {
                "raw_control" => {
                    // Send a control message directly
                    if params.len() >= 2 {
                        let control_code = u16::from_be_bytes([params[0], params[1]]);
                        if let Ok(_) = ControlMessage::try_from(control_code) {
                            // Process control message if needed
                            debug!("Processing control message: {}", control_code);
                        }
                    }
                    Ok(())
                }
                "disconnect" => {
                    // Handle disconnect - no additional cleanup needed
                    Ok(())
                }
                _ => Ok(()) // Ignore other commands
            }
        })
    }
    
    fn handle_new_connection(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Register this connection as active
            self.active_connections.insert(conn_no);
            debug!("Port forward handler: new connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_eof(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            debug!("Port forward handler: EOF on connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_close(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Remove this connection from the active set
            self.active_connections.remove(&conn_no);
            debug!("Port forward handler: closed connection {}", conn_no);
            Ok(())
        })
    }
    
    fn set_webrtc_channel(&mut self, channel: WebRTCDataChannel) {
        debug!("Port forward handler: WebRTC channel set/updated");
        self.webrtc = Some(channel);
        
        // Try to process any pending messages now that we have a channel
        let runtime = crate::runtime::get_runtime();
        let mut handler = self.clone();
        runtime.spawn(async move {
            if let Err(e) = handler.process_pending_messages().await {
                warn!("Port forward handler: Error processing pending messages: {}", e);
            }
        });
    }

    fn on_webrtc_channel_state_change(&mut self, state: &str) -> BoxFuture<'_, Result<()>> {
        // Clone the state string to avoid lifetime issues
        let state_copy = state.to_string();
        
        Box::pin(async move {
            debug!("Port forward handler: WebRTC channel state changed to {}", state_copy);
            
            // If the channel is now open, try to process pending messages
            if state_copy == "open" {
                if let Some(webrtc) = &self.webrtc {
                    if webrtc.ready_state() == "open" {
                        debug!("Port forward handler: WebRTC channel is open, processing pending messages");
                        self.process_pending_messages().await?;
                    }
                }
            } else if state_copy == "closed" || state_copy == "closing" {
                // Channel is closing or closed - no need to try sending
                debug!("Port forward handler: WebRTC channel is {}, pausing message processing", state_copy);
            }
            
            Ok(())
        })
    }

    fn process_pending_messages(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Only process if we have a channel and it's in the 'open' state
            if let Some(webrtc) = &self.webrtc {
                if webrtc.ready_state() == "open" {
                    let pending_count = self.pending_messages.len();
                    if pending_count > 0 {
                        info!("Port forward handler: Processing {} pending messages", pending_count);
                        
                        // Process pending messages until queue is empty, or we encounter an error
                        let mut successful_sends = 0;
                        while let Some(msg) = self.pending_messages.pop_front() {
                            // Create frame and send
                            let frame = Frame::new_data_with_pool(msg.conn_no, &msg.data, &self.buffer_pool);
                            let encoded = frame.encode_with_pool(&self.buffer_pool);
                            
                            match webrtc.send(encoded).await {
                                Ok(_) => {
                                    successful_sends += 1;
                                },
                                Err(e) => {
                                    // If send fails, put the message back at the front of the queue and stop processing
                                    warn!("Port forward handler: Failed to send pending message: {}, will retry later", e);
                                    self.pending_messages.push_front(msg);
                                    break;
                                }
                            }
                        }
                        
                        info!("Port forward handler: Successfully sent {}/{} pending messages", 
                             successful_sends, pending_count);
                    }
                } else {
                    debug!("Port forward handler: WebRTC channel not in 'open' state ({}), skipping pending messages", 
                          webrtc.ready_state());
                }
            } else {
                debug!("Port forward handler: No WebRTC channel available, skipping pending messages");
            }
            
            Ok(())
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<()>> {
        // Clean up active connections
        Box::pin(async move {
            self.active_connections.clear();
            self.pending_messages.clear();
            Ok(())
        })
    }

    fn status(&self) -> String {
        format!("Port Forward Handler: Active (Connections: {}, Pending Messages: {})", 
               self.active_connections.len(), self.pending_messages.len())
    }
}

// Implement Clone for the PortForwardProtocolHandler
impl Clone for PortForwardProtocolHandler {
    fn clone(&self) -> Self {
        Self {
            active_connections: self.active_connections.clone(),
            buffer: BytesMut::with_capacity(8192), // Create a new buffer rather than cloning
            webrtc: self.webrtc.clone(),
            buffer_pool: self.buffer_pool.clone(),
            pending_messages: VecDeque::new(), // Don't clone pending messages
            max_pending_messages: self.max_pending_messages,
        }
    }
} 