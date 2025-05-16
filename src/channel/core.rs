// Core Channel implementation

use anyhow::Result;
use bytes::BytesMut;
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use log::debug;
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use crate::error::ChannelError;
use crate::protocol::{Frame, ControlMessage, CloseConnectionReason, try_parse_frame, CONN_NO_LEN};
use crate::models::{Conn, TunnelTimeouts, NetworkAccessChecker, StreamHalf, ConnectionStats};
use crate::runtime::get_runtime;
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::buffer_pool::{BufferPool, BufferPoolConfig};

// Import from sibling modules
use super::frame_handling::handle_incoming_frame;
use super::utils::handle_ping_timeout;

// Define the buffer thresholds
pub(crate) const BUFFER_LOW_THRESHOLD: u64 = 14 * 1024 * 1024; // 14MB for responsive UI

/// Channel instance. Owns the data‑channel and a map of active back‑end TCP streams.
pub struct Channel {
    pub(crate) webrtc: WebRTCDataChannel,
    pub(crate) conns: HashMap<u32, Conn>,
    pub(crate) next_conn_no: u32,
    pub(crate) rx_from_dc: mpsc::UnboundedReceiver<Bytes>,
    pub(crate) channel_id: String,
    pub(crate) timeouts: TunnelTimeouts,
    pub(crate) network_checker: Option<NetworkAccessChecker>,
    pub(crate) ping_attempt: u32,
    pub(crate) round_trip_latency: Vec<u64>,
    pub(crate) is_connected: bool,
    pub(crate) should_exit: bool,
    pub(crate) server_mode: bool, // If true, acts as a client-side channel that creates servers; if false, acts as a server-side channel
    // Server-related fields
    pub(crate) local_client_server: Option<Arc<TcpListener>>,
    pub(crate) local_client_server_task: Option<JoinHandle<()>>,
    // Channel for server to register new connections
    pub(crate) local_client_server_conn_tx: Option<mpsc::Sender<(u32, OwnedWriteHalf, JoinHandle<()>)>>,
    pub(crate) local_client_server_conn_rx: Option<mpsc::Receiver<(u32, OwnedWriteHalf, JoinHandle<()>)>>,
    // Protocol handlers (for SOCKS, guacd)
    pub(crate) protocol_handlers: HashMap<String, Box<dyn crate::models::ProtocolHandler + Send + Sync>>,
    // Protocol connection map: maps connection_no -> protocol_type
    pub(crate) protocol_connections: HashMap<u32, String>,
    // Pending message queues for each connection
    pub(crate) pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>>,
    // Buffer pool for efficient buffer management
    pub(crate) buffer_pool: BufferPool,
}

// Implement Clone for Channel
impl Clone for Channel {
    fn clone(&self) -> Self {
        // Create new channels for server connections
        let (server_conn_tx, server_conn_rx) = mpsc::channel(32);
        
        Self {
            webrtc: self.webrtc.clone(),
            conns: HashMap::new(), // We don't clone connections
            next_conn_no: self.next_conn_no,
            rx_from_dc: mpsc::unbounded_channel().1, // Create a new channel
            channel_id: self.channel_id.clone(),
            timeouts: self.timeouts.clone(),
            network_checker: self.network_checker.clone(),
            ping_attempt: self.ping_attempt,
            round_trip_latency: self.round_trip_latency.clone(),
            is_connected: self.is_connected,
            should_exit: self.should_exit,
            server_mode: self.server_mode, // Copy the server mode setting
            local_client_server: None, // We don't clone the server
            local_client_server_task: None, // We don't clone server task
            local_client_server_conn_tx: Some(server_conn_tx),
            local_client_server_conn_rx: Some(server_conn_rx),
            protocol_handlers: HashMap::new(), // We don't clone protocol handlers
            protocol_connections: HashMap::new(), // We don't clone protocol connections
            pending_messages: Arc::new(Mutex::new(HashMap::<u32, VecDeque<Bytes>>::new())), // New empty pending messages map
            buffer_pool: BufferPool::default(), // Create a new buffer pool
        }
    }
}

impl Channel {
    pub fn new(
        webrtc: WebRTCDataChannel,
        rx_from_dc: mpsc::UnboundedReceiver<Bytes>,
        channel_id: String,
        timeouts: Option<TunnelTimeouts>,
        network_checker: Option<NetworkAccessChecker>,
    ) -> Self {
        // Create a channel for the server to register connections
        let (server_conn_tx, server_conn_rx) = mpsc::channel(32); // Buffer for 32 connections
        
        // Set up the bufferedAmountLowThreshold on the data channel
        webrtc.set_buffered_amount_low_threshold(BUFFER_LOW_THRESHOLD);
        
        // Create a buffer pool sized appropriately for this channel
        let buffer_pool_config = BufferPoolConfig {
            buffer_size: 32 * 1024, // 32KB default buffer size for network operations
            max_pooled: 64,         // Keep up to 64 buffers in the pool
            resize_on_return: true, // Resize buffers when returning to the pool
        };
        let buffer_pool = BufferPool::new(buffer_pool_config);
        
        let pending_messages: Arc<Mutex<HashMap<u32, VecDeque<Bytes>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_messages_clone = pending_messages.clone();
        let webrtc_clone = webrtc.clone();
        let channel_id_clone = channel_id.clone();
        let buffer_pool_clone = buffer_pool.clone();

        // Set up the callback for when the buffer drops below the threshold
        webrtc.on_buffered_amount_low(Some(Box::new(move || {
            let pending_messages = pending_messages_clone.clone();
            let dc = webrtc_clone.clone();
            let ep_name = channel_id_clone.clone();
            let pool = buffer_pool_clone.clone();

            // Spawn a task to process pending messages
            tokio::spawn(async move {
               debug!("Endpoint {}: Buffer amount is now below threshold, processing pending messages", ep_name);

                // Create a channel instance just for processing messages
                let (_, rx) = mpsc::unbounded_channel();
                let mut channel = Channel::new(
                    dc.clone(),
                    rx,
                    ep_name.clone(),
                    None, // No timeouts needed for message processing
                    None, // No network checking needed
                );

                // Set the pending_messages field to use our shared collection
                channel.pending_messages = pending_messages;
                // Use the same buffer pool
                channel.buffer_pool = pool;

                // Process pending messages
                if let Err(e) = channel.process_pending_messages().await {
                    log::error!("Endpoint {}: Error processing pending messages: {}", ep_name, e);
                }
            });
        })));
        
        Self {
            webrtc,
            conns: HashMap::new(),
            next_conn_no: 1,
            rx_from_dc,
            channel_id,
            timeouts: timeouts.unwrap_or_default(),
            network_checker,
            ping_attempt: 0,
            round_trip_latency: Vec::new(),
            is_connected: true,
            should_exit: false,
            server_mode: false, // Default to server-side mode
            local_client_server: None,
            local_client_server_task: None,
            local_client_server_conn_tx: Some(server_conn_tx),
            local_client_server_conn_rx: Some(server_conn_rx),
            protocol_handlers: HashMap::new(),
            protocol_connections: HashMap::new(),
            pending_messages,
            buffer_pool,
        }
    }

    pub async fn run(mut self) -> Result<(), ChannelError> {
        let mut buf = BytesMut::with_capacity(64 * 1024);

        // Take the receiver channel for server connections
        let mut server_conn_rx = self.local_client_server_conn_rx.take();

        // Main processing loop - reads from WebRTC and dispatches frames
        while !self.should_exit {
            // Process any complete frames in the buffer
            while let Some(frame) = try_parse_frame(&mut buf) {
                debug!("Channel({}): Received frame from WebRTC, connection {}, payload size: {}", 
                        self.channel_id, frame.connection_no, frame.payload.len());
                
                if let Err(e) = handle_incoming_frame(&mut self, frame).await {
                    log::error!("Channel({}): Error handling frame: {}", self.channel_id, e);
                }
            }

            // Check for any new connections from the server
            if let Some(ref mut rx) = server_conn_rx {
                if let Ok(Some((conn_no, writer, task))) = tokio::time::timeout(Duration::from_millis(1), rx.recv()).await {
                   debug!("Endpoint {}: Registering connection {} from server", 
                        self.channel_id, conn_no);
                    
                    // Create a stream half
                    let stream_half = StreamHalf {
                        reader: None,
                        writer,
                    };
                    
                    // Create a connection
                    let conn = Conn {
                        backend: Box::new(stream_half),
                        to_webrtc: task,
                        stats: ConnectionStats::default(),
                    };
                    
                    // Store in our registry
                    self.conns.insert(conn_no, conn);
                }
            }

            // Wait for more data from WebRTC
            debug!("Channel({}): Waiting for data from WebRTC...", self.channel_id);
            match tokio::time::timeout(self.timeouts.read, self.rx_from_dc.recv()).await {
                Ok(Some(chunk)) => {
                    debug!("Channel({}): Received {} bytes from WebRTC", 
                            self.channel_id, chunk.len());
                    
                    // Log the first few bytes to help with debugging
                    if chunk.len() > 0 {
                        debug!("Channel({}): First few bytes: {:?}", 
                                self.channel_id, &chunk[..std::cmp::min(20, chunk.len())]);
                    }
                    
                    // Add the data to our buffer
                    buf.extend_from_slice(&chunk);
                    debug!("Channel({}): Buffer size after adding chunk: {} bytes", 
                            self.channel_id, buf.len());
                    
                    // Process any messages waiting in connection queues
                    self.process_pending_messages().await?;
                }
                Ok(None) => {
                    log::info!("Endpoint {}: WebRTC data channel closed", self.channel_id);
                    break;
                }
                Err(_) => {
                    // Handle read timeout - send ping to check connection
                    handle_ping_timeout(&mut self).await?;
                }
            }
        }

        // Clean up all connections
        self.cleanup_all_connections().await?;

        Ok(())
    }

    // Method to handle client mode operation - waits for events from server
    pub async fn wait_for_events(mut self) -> Result<(), ChannelError> {
        let mut buf = BytesMut::with_capacity(64 * 1024);

        // Main processing loop - reads from WebRTC and handles events
        while !self.should_exit {
            // Process any complete frames in the buffer
            while let Some(frame) = try_parse_frame(&mut buf) {
                // Process frame through the appropriate handler
                if let Err(e) = handle_incoming_frame(&mut self, frame).await {
                    log::error!("Channel({}): Error handling frame: {}", self.channel_id, e);
                }
            }

            // Wait for more data from WebRTC
            match tokio::time::timeout(self.timeouts.read, self.rx_from_dc.recv()).await {
                Ok(Some(chunk)) => {
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => {
                    log::info!("Endpoint {}: WebRTC data channel closed", self.channel_id);
                    break;
                }
                Err(_) => {
                    // Handle read timeout - send ping to check connection
                    handle_ping_timeout(&mut self).await?;
                }
            }
        }

        // Clean up all connections
        self.cleanup_all_connections().await?;

        Ok(())
    }

    // New method to process pending messages for all connections with better prioritization
    pub(crate) async fn process_pending_messages(&mut self) -> Result<()> {
        // Lock the pending messages map
        let mut pending_guard = self.pending_messages.lock().await;
        
        // If no pending messages, return early
        if pending_guard.iter().all(|(_, queue)| queue.is_empty()) {
            return Ok(());
        }
        
        debug!("Endpoint {}: Processing pending messages for {} connections", 
                   self.channel_id, pending_guard.len());
        
        // Process a batch of messages while staying under the buffer threshold
        let max_to_process = 100; // Maximum total messages to process in one batch
        let mut total_processed = 0;
        
        // First, calculate message counts across all connections
        let total_messages: usize = pending_guard.values().map(|q| q.len()).sum();
        
        if total_messages > 0 {
           debug!("Endpoint {}: Total of {} pending messages across all connections", 
                       self.channel_id, total_messages);
        }
        
        // Create a list of connection numbers that have messages
        let conn_nos: Vec<u32> = pending_guard.keys()
            .filter(|&conn_no| !pending_guard[conn_no].is_empty())
            .copied()
            .collect();
        
        // Process messages in a round-robin fashion across connections to ensure fairness
        // This prevents a single busy connection from starving others
        let mut current_index = 0;
        
        // Get a buffer for potential repackaging operations
        let encode_buffer = self.buffer_pool.acquire();
        
        // Get buffer threshold one time
        let buffer_threshold = BUFFER_LOW_THRESHOLD;
        
        while total_processed < max_to_process && !conn_nos.is_empty() {
            let conn_index = current_index % conn_nos.len();
            let conn_no = conn_nos[conn_index];
            
            // Check if this connection has any messages
            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                let message_to_send = if let Some(message) = queue.front() {
                    message.clone()
                } else {
                    // Move to the next connection if the queue is empty
                    current_index += 1;
                    continue;
                };
                
                // Try to send the message - the WebRTC implementation will handle buffering internally
                if let Err(e) = self.webrtc.send(message_to_send).await {
                    log::error!("Endpoint {}: Failed to send queued message for connection {}: {}", 
                        self.channel_id, conn_no, e);
                    break;
                }

                // Now we can mutably borrow to remove the item
                queue.pop_front();
                total_processed += 1;
                
                // Every 10 messages, check if we should continue or wait for buffer to drain
                if total_processed % 10 == 0 {
                    let current_buffer = self.webrtc.buffered_amount().await;
                    if current_buffer >= buffer_threshold {
                        debug!("Endpoint {}: Buffer threshold reached during processing, pausing after {} messages", 
                            self.channel_id, total_processed);
                        break;
                    }
                }
            }
            
            // Move to the next connection
            current_index += 1;
        }
        
        // Return the buffer to the pool
        self.buffer_pool.release(encode_buffer);
        
        // Log results
        if total_processed > 0 {
           debug!("Endpoint {}: Processed {} pending messages in this batch", 
                       self.channel_id, total_processed);
        }
        
        Ok(())
    }

    // Helper method to clean up all connections
    pub(crate) async fn cleanup_all_connections(&mut self) -> Result<()> {
        for conn_no in self.conns.keys().copied().collect::<Vec<_>>() {
            if conn_no != 0 {
                self.close_backend(conn_no, CloseConnectionReason::Normal).await?;
            }
        }
        Ok(())
    }

    // Control message helper
    pub(crate) async fn send_control_message(&mut self, message: ControlMessage, data: &[u8]) -> Result<()> {
        // Create a control frame using our buffer pool
        let frame = Frame::new_control_with_pool(message, data, &self.buffer_pool);
        
        // Encode using our buffer pool to avoid allocations
        let encoded = frame.encode_with_pool(&self.buffer_pool);

        // Control messages are prioritized and should be sent immediately
        // Get the current buffer amount
        let buffered_amount = self.webrtc.buffered_amount().await;
        
        if buffered_amount >= BUFFER_LOW_THRESHOLD {
           debug!(
                "Endpoint {}: Control message buffer full ({} bytes), but sending control message for {}",
                self.channel_id, buffered_amount, message
            );
        }
        
        // Always try to send control messages immediately, regardless of buffer state
        // WebRTC library will handle the backpressure internally
        self.webrtc.send(encoded).await.map_err(|e| anyhow::anyhow!("{}", e))?;

        Ok(())
    }
    
    // Method to handle received message bytes from a data channel
    pub async fn receive_message(&self, bytes: Bytes) -> Result<()> {
        // Use the buffer pool instead of allocating a new BytesMut
        let mut buffer = self.buffer_pool.acquire();
        buffer.clear();
        buffer.extend_from_slice(&bytes);
        
        // Process frames one at a time to avoid holding locks across await points
        while let Some(frame) = try_parse_frame(&mut buffer) {
            debug!("Channel({}): Processing frame from message, connection {}, payload size: {}", 
                    self.channel_id, frame.connection_no, frame.payload.len());
            
            // We need a mutable reference for handle_incoming_frame
            // Using a separate task to avoid cloning the entire channel
            let frame_clone = frame;
            let channel_id = self.channel_id.clone();
            let pending_messages = self.pending_messages.clone();
            let webrtc = self.webrtc.clone();
            let protocol_connections = self.protocol_connections.clone();
            let buffer_pool = self.buffer_pool.clone();
            
            // Get these fields individually to avoid cloning the entire channel
            let is_server_mode = self.server_mode;
            let timeouts = self.timeouts.clone();
            let network_checker = self.network_checker.clone();
            
            // Spawn a task to handle this frame independently
            tokio::spawn(async move {
                // Create a lightweight channel instance just for this frame
                let (_, rx) = mpsc::unbounded_channel();
                let mut frame_channel = Channel::new(
                    webrtc.clone(),
                    rx,
                    channel_id.clone(),
                    Some(timeouts),
                    network_checker,
                );
                
                // Connect to the shared state
                frame_channel.pending_messages = pending_messages;
                frame_channel.buffer_pool = buffer_pool;
                frame_channel.server_mode = is_server_mode;
                
                // Rebuild protocol connections mapping (lightweight copy)
                for (conn_no, proto_type) in protocol_connections {
                    frame_channel.protocol_connections.insert(conn_no, proto_type);
                }
                
                // Process the frame
                if let Err(e) = handle_incoming_frame(&mut frame_channel, frame_clone).await {
                    log::error!("Channel({}): Error handling frame from message: {}", channel_id, e);
                }
            });
        }
        
        // Return the buffer to the pool
        self.buffer_pool.release(buffer);
        
        Ok(())
    }

    // Async close_backend implementation
    pub(crate) async fn close_backend(&mut self, conn_no: u32, reason: CloseConnectionReason) -> Result<()> {
        log::info!(
        "Endpoint {}: Closing connection {} (reason: {:?})",
        self.channel_id, conn_no, reason
        );

        // Send close notification
        let mut buffer = BytesMut::with_capacity(CONN_NO_LEN + 2);
        buffer.extend_from_slice(&conn_no.to_be_bytes());
        buffer.extend_from_slice(&(reason as u16).to_be_bytes());
        let buffer = buffer.freeze();

        self.send_control_message(ControlMessage::CloseConnection, &buffer).await?;

        // Check if this is a protocol connection
        if let Some(protocol_type) = self.protocol_connections.remove(&conn_no) {
            // Notify the protocol handler
            if let Some(handler) = self.protocol_handlers.get_mut(&protocol_type) {
                handler.handle_close(conn_no).await?;
            }
        }

        // Remove and clean up connection
        if let Some(mut conn) = self.conns.remove(&conn_no) {
            // Shutdown the socket using the trait method explicitly
            let _ = crate::models::AsyncReadWrite::shutdown(&mut conn.backend).await;

            // Abort the task
            conn.to_webrtc.abort();

            // Wait for a task to complete (with timeout)
            match tokio::time::timeout(self.timeouts.close_connection, conn.to_webrtc).await {
                Ok(_) => {
                   debug!(
                    "Endpoint {}: Successfully closed backend task for connection {}",
                    self.channel_id, conn_no
                );
                }
                Err(_) => {
                    log::warn!(
                    "Endpoint {}: Timeout waiting for backend task to close for connection {}",
                    self.channel_id, conn_no
                );
                }
            }
        }

        // Remove any pending messages for this connection
        {
            let mut pending_guard = self.pending_messages.lock().await;
            pending_guard.remove(&conn_no);
        }

        // Special case: of closing connection 0, signal exit
        if conn_no == 0 {
            self.should_exit = true;
        }

        Ok(())
    }
}

// Ensure all resources are properly cleaned up
impl Drop for Channel {
    fn drop(&mut self) {
        // Set the should_exit flag to stop any loops
        self.should_exit = true;

        // Ensure server tasks are aborted
        if let Some(task) = &self.local_client_server_task {
            task.abort();
        }

        // Global runtime can handle the async cleanup
        let runtime = get_runtime();
        let webrtc = self.webrtc.clone();
        let channel_id = self.channel_id.clone();
        let conns = self.conns.keys().copied().collect::<Vec<_>>();
        let buffer_pool = self.buffer_pool.clone();

        runtime.spawn(async move {
            for conn_no in conns {
                // Create a buffer for the close message
                let mut close_buffer = buffer_pool.acquire();
                close_buffer.clear();
                
                // Add connection number and reason
                close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                close_buffer.extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());
                
                // Create and send the frame
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut close_buffer
                );
                
                let encoded = close_frame.encode_with_pool(&buffer_pool);
                let _ = webrtc.send(encoded).await;
                
                // Return buffer to pool
                buffer_pool.release(close_buffer);
            }

            log::info!("Endpoint {}: All resources cleaned up", channel_id);
        });
    }
}
