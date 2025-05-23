// Connection management functionality for Channel

use anyhow::Result;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use crate::models::{Conn, ConnectionStats, StreamHalf};
use crate::tube_protocol::Frame;
use crate::channel::types::ActiveProtocol;
use crate::channel::guacd_parser::GuacdParser;
use bytes::{Bytes, BufMut};
use tracing::{debug, error, warn};

use super::core::Channel;

// Open a backend connection to a given address
pub async fn open_backend(channel: &mut Channel, conn_no: u32, addr: SocketAddr, active_protocol: ActiveProtocol) -> Result<()> {
    debug!("Endpoint {}: Opening connection {} to {} for protocol {:?}", channel.channel_id, conn_no, addr, active_protocol);

    // Check if the connection already exists
    if channel.conns.lock().await.contains_key(&conn_no) {
        warn!("Endpoint {}: Connection {} already exists", channel.channel_id, conn_no);
        return Ok(());
    }

    // Connect to the backend
    let stream = TcpStream::connect(addr).await?;
    
    // Set up the outbound task for this connection
    setup_outbound_task(channel, conn_no, stream, active_protocol).await?;

    Ok(())
}

// Set up a task to read from the backend and send to WebRTC
pub async fn setup_outbound_task(channel: &mut Channel, conn_no: u32, stream: TcpStream, active_protocol: ActiveProtocol) -> Result<()> {
    // Split the stream into a reader and writer
    let (backend_reader, backend_writer) = stream.into_split();

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

    let mut task_guacd_parser = if active_protocol == ActiveProtocol::Guacd {
        Some(GuacdParser::new())
    } else {
        None
    };

    let outbound_handle = tokio::spawn(async move {
        let mut reader = backend_reader;
        let mut eof_sent = false;
        
        // Get a single reusable buffer from the pool
        let mut read_buffer = buffer_pool.acquire();
        
        // Use a smaller read size to ensure we don't exceed SCTP limits
        // Frame overhead is approximately 17 bytes (4 + 8 + 4 + 1)
        // To stay safely under 16KB SCTP limit, use 8KB reads
        const MAX_READ_SIZE: usize = 8 * 1024; // 8KB
        
        // Initialize buffer with our max read size
        if read_buffer.capacity() < MAX_READ_SIZE {
            read_buffer.reserve(MAX_READ_SIZE);
        }
        
        // Create a second buffer for encoding frames
        let mut encode_buffer = buffer_pool.acquire();

        loop {
            // Clear the buffer for reuse (but keep capacity)
            read_buffer.clear();
            
            // Ensure we have exactly MAX_READ_SIZE capacity
            if read_buffer.capacity() < MAX_READ_SIZE {
                read_buffer.reserve(MAX_READ_SIZE - read_buffer.capacity());
            }
            
            // Limit the read to MAX_READ_SIZE to control message sizes
            let max_to_read = std::cmp::min(read_buffer.capacity(), MAX_READ_SIZE);
            
            // Correctly create a mutable slice for reading
            let ptr = read_buffer.chunk_mut().as_mut_ptr();
            let current_chunk_len = read_buffer.chunk_mut().len();
            let slice_len = std::cmp::min(current_chunk_len, max_to_read);
            let read_slice = unsafe { std::slice::from_raw_parts_mut(ptr, slice_len) };

            match reader.read(read_slice).await {
                Ok(0) => {
                    // EOF handling
                    if !eof_sent {
                        // Create EOF control message
                        let eof_frame = Frame::new_control_with_pool(
                            crate::tube_protocol::ControlMessage::SendEOF,
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
                    // Advance the buffer by the number of bytes read
                    unsafe {
                        read_buffer.advance_mut(n);
                    }
                    
                    // Data handling - got data in read_buffer
                    eof_sent = false;

                    // We use a view of just the filled part
                    let current_payload_bytes_slice = &read_buffer[0..n];
                    
                    let mut frames_to_send: Vec<Bytes> = Vec::new(); // Changed name for clarity

                    if active_protocol == ActiveProtocol::Guacd {
                        if let Some(parser) = &mut task_guacd_parser {
                            if let Err(e) = parser.receive(conn_no, current_payload_bytes_slice) {
                                error!("Channel({}): GuacdParser receive error conn_no {}: {}", channel_id, conn_no, e);
                                break; 
                            } else {
                                let mut close_conn_and_break = false;
                                while let Some(instruction) = parser.get_instruction() {
                                    debug!("Channel({}): Parsed Guacd instruction from backend for conn_no {}: {:?}", channel_id, conn_no, instruction.opcode);
                                    if instruction.opcode == "disconnect" || instruction.opcode == "error" {
                                        warn!(
                                            "Channel({}): Guacd instruction '{}' received from backend for conn_no {}. Args: {:?}. Closing connection.",
                                            channel_id, instruction.opcode, conn_no, instruction.args
                                        );
                                        let close_payload = conn_no.to_be_bytes();
                                        let mut temp_control_payload_buf = buffer_pool.acquire();
                                        temp_control_payload_buf.clear();
                                        temp_control_payload_buf.extend_from_slice(&close_payload);
                                        let close_frame = Frame::new_control_with_buffer(crate::tube_protocol::ControlMessage::CloseConnection, &mut temp_control_payload_buf);
                                        buffer_pool.release(temp_control_payload_buf); 

                                        if let Err(e) = dc.send(close_frame.encode_with_pool(&buffer_pool)).await {
                                            error!("Channel({}): Failed to send CloseConnection for Guacd error on conn_no {}: {}", channel_id, conn_no, e);
                                        }
                                        close_conn_and_break = true;
                                        break; 
                                    }
                                    frames_to_send.push(GuacdParser::guacd_encode_instruction(&instruction));
                                }
                                if close_conn_and_break {
                                    break; 
                                }
                            }
                        } else {
                            error!("Channel({}): Guacd protocol active for conn_no {} but GuacdParser is missing. This is a critical error. Terminating task for this connection.", channel_id, conn_no);
                            break; // Critical error, terminate the task for this connection
                        }
                    } else {
                        // For PortForward, SOCKS5 data phase - use direct encoding to avoid intermediate Frame/Bytes copy
                        encode_buffer.clear(); // encode_buffer is from the pool
                        let bytes_written = Frame::encode_data_frame_from_slice(
                            &mut encode_buffer,
                            conn_no,
                            current_payload_bytes_slice,
                        );
                        frames_to_send.push(encode_buffer.split_to(bytes_written).freeze());
                    }

                    for encoded_frame_bytes in frames_to_send {
                        if encoded_frame_bytes.is_empty() { continue; }

                        // Log frame sizes for debugging SCTP issues
                        // Note: For the direct encoding path, we don't have separate payload_len easily here
                        // We log the total encoded_frame_bytes.len()
                        if encoded_frame_bytes.len() > 4096 {
                            debug!(
                                "Channel({}): Sending large frame for conn_no {}: encoded_size={} bytes",
                                channel_id, conn_no, encoded_frame_bytes.len()
                            );
                        }

                        let buffered_amount = dc.buffered_amount().await;
                        if buffered_amount >= buffer_low_threshold {
                            debug!(
                                "Endpoint {}: Buffer amount high ({} bytes), queueing message for connection {}",
                                channel_id, buffered_amount, conn_no
                            );
                            
                            // Add to pending messages queue
                            let mut pending_guard = pending_messages.lock().await;
                            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                                queue.push_back(encoded_frame_bytes);
                                
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
                            if let Err(e) = dc.send(encoded_frame_bytes).await {
                                error!(
                                    "Endpoint {}: Failed to send data for connection {}: {}",
                                    channel_id, conn_no, e
                                );
                                break;
                            }
                        }
                    }
                }
                Ok(_) => {
                    // Zero-length read, but not EOF - keep looping
                    continue;
                }
                Err(e) => {
                    error!(
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
        writer: backend_writer,
    };
    
    channel.conns.lock().await.insert(conn_no, Conn {
        backend: Box::new(stream_half),
        to_webrtc: outbound_handle,
        stats: ConnectionStats::default(),
    });
    
    debug!("Endpoint {}: Connection {} added to registry", channel_id_for_log, conn_no);

    Ok(())
}
