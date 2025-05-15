// Connection management functionality for Channel

use anyhow::Result;
use log::debug;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use crate::models::{Conn, ConnectionStats, StreamHalf};
use crate::protocol::Frame;

use super::core::Channel;

// Open a backend connection to a given address
pub async fn open_backend(channel: &mut Channel, conn_no: u32, addr: SocketAddr) -> Result<()> {
    debug!("Endpoint {}: Opening connection {} to {}", channel.channel_id, conn_no, addr);

    // Check if the connection already exists
    if channel.conns.contains_key(&conn_no) {
        log::warn!("Endpoint {}: Connection {} already exists", channel.channel_id, conn_no);
        return Ok(());
    }

    // Connect to the backend
    let stream = TcpStream::connect(addr).await?;
    
    // Set up the outbound task for this connection
    setup_outbound_task(channel, conn_no, stream).await?;

    Ok(())
}

// Set up a task to read from the backend and send to WebRTC
pub async fn setup_outbound_task(channel: &mut Channel, conn_no: u32, stream: TcpStream) -> Result<()> {
    // Split the stream into a reader and writer
    let (reader, writer) = stream.into_split();

    // Create an entry in the pending messages map for this connection
    {
        let mut pending_guard = channel.pending_messages.lock().await;
        pending_guard.insert(conn_no, std::collections::VecDeque::new());
    }

    // Spawn a task that pumps backend→WebRTC
    let dc = channel.webrtc.clone();
    let channel_id = channel.channel_id.clone();
    let channel_id_for_log = channel_id.clone(); // Clone for later use
    let pending_messages = channel.pending_messages.clone();
    let buffer_pool = channel.buffer_pool.clone(); // Share the buffer pool with the task
    let buffer_low_threshold = super::core::BUFFER_LOW_THRESHOLD;

    let outbound_handle = tokio::spawn(async move {
        let mut reader = reader;
        let mut eof_sent = false;
        
        // Get a single reusable buffer from the pool
        let mut read_buffer = buffer_pool.acquire();
        
        // Initialize to a reasonable size (this will grow if needed)
        if read_buffer.capacity() < 16 * 1024 {
            read_buffer.reserve(16 * 1024);
        }
        
        // Create a second buffer for encoding frames
        let mut encode_buffer = buffer_pool.acquire();

        loop {
            // Clear the buffer for reuse (but keep capacity)
            read_buffer.clear();
            
            // Make sure we have room to read into
            read_buffer.reserve(16 * 1024);
            
            // Read directly into a pooled buffer
            match reader.read_buf(&mut read_buffer).await {
                Ok(0) => {
                    // EOF handling
                    if !eof_sent {
                        // Create EOF control message
                        let eof_frame = Frame::new_control_with_pool(
                            crate::protocol::ControlMessage::SendEOF,
                            &conn_no.to_be_bytes(),
                            &buffer_pool
                        );
                        
                        // Encode to bytes
                        let encoded = eof_frame.encode_with_pool(&buffer_pool);
                        
                        let _ = dc.send(encoded).await;
                        eof_sent = true;

                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    } else {
                        break;
                    }
                }
                Ok(n) if n > 0 => {
                    // Data handling - got data in read_buffer
                    eof_sent = false;

                    // Create a data frame with the read portion of the buffer
                    // We use a view of just the filled part
                    let frame = Frame::new_data_with_pool(conn_no, &read_buffer[0..n], &buffer_pool);
                    
                    // Encode the frame reusing our encode buffer
                    let bytes_written = frame.encode_into(&mut encode_buffer);
                    let encoded = encode_buffer.split_to(bytes_written).freeze();

                    // Get the current buffer amount
                    let buffered_amount = dc.buffered_amount().await;
                    
                    // Check if the buffer is high - if so, queue the message instead of sending directly
                    if buffered_amount >= buffer_low_threshold {
                       debug!(
                            "Endpoint {}: Buffer amount high ({} bytes), queueing message for connection {}",
                            channel_id, buffered_amount, conn_no
                        );
                        
                        // Add to pending messages queue
                        let mut pending_guard = pending_messages.lock().await;
                        if let Some(queue) = pending_guard.get_mut(&conn_no) {
                            queue.push_back(encoded);
                            
                            // Log queue size periodically
                            if queue.len() % 100 == 0 {
                               debug!(
                                    "Endpoint {}: Pending queue for connection {} has {} messages",
                                    channel_id, conn_no, queue.len()
                                );
                            }
                        }
                    } else {
                        // Send it directly
                        if let Err(e) = dc.send(encoded).await {
                            log::error!(
                                "Endpoint {}: Failed to send data for connection {}: {}",
                                channel_id, conn_no, e
                            );
                            break;
                        }
                    }
                }
                Ok(_) => {
                    // Zero-length read, but not EOF - keep looping
                    continue;
                }
                Err(e) => {
                    log::error!(
                    "Endpoint {}: Read error on connection {}: {}",
                    channel_id, conn_no, e
                );
                    break;
                }
            }
        }

       debug!("Endpoint {}: Backend→WebRTC task for connection {} exited", channel_id, conn_no);
        
        // Return buffers to the pool before exiting
        buffer_pool.release(read_buffer);
        buffer_pool.release(encode_buffer);
        
        // Clean up pending messages for this connection
        let mut pending_guard = pending_messages.lock().await;
        pending_guard.remove(&conn_no);
    });

    // Store the writer part of the stream for us to write to
    let stream_half = StreamHalf {
        reader: None,
        writer,
    };
    
    // We need to store this connection somewhere to write back to it
    // Create a new Conn instance for this connection
    let conn = Conn {
        backend: Box::new(stream_half),
        to_webrtc: outbound_handle,
        stats: ConnectionStats::default(),
    };
    
    // Store the connection in our registry
    channel.conns.insert(conn_no, conn);
    
    debug!("Endpoint {}: Connection {} added to registry", channel_id_for_log, conn_no);

    Ok(())
}
