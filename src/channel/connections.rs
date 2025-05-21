// Connection management functionality for Channel

use anyhow::Result;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use crate::models::{Conn, ConnectionStats, StreamHalf};
use crate::tube_protocol::Frame;
use crate::channel::types::ActiveProtocol;
use crate::channel::guacd_parser::GuacdParser;
use bytes::Bytes;
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
                    // Data handling - got data in read_buffer
                    eof_sent = false;

                    // We use a view of just the filled part
                    let current_payload_bytes_slice = &read_buffer[0..n];
                    
                    let mut frames_to_send_payloads: Vec<Bytes> = Vec::new();

                    if active_protocol == ActiveProtocol::Guacd {
                        if let Some(parser) = &mut task_guacd_parser {
                            if let Err(e) = parser.receive(conn_no, current_payload_bytes_slice) {
                                error!("Channel({}): GuacdParser receive error conn_no {}: {}", channel_id, conn_no, e);
                                // On parser error, might be good to break and initiate close
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
                                        let close_frame = Frame::new_control_with_pool(crate::tube_protocol::ControlMessage::CloseConnection, &close_payload, &buffer_pool);
                                        if let Err(e) = dc.send(close_frame.encode_with_pool(&buffer_pool)).await {
                                            error!("Channel({}): Failed to send CloseConnection for Guacd error on conn_no {}: {}", channel_id, conn_no, e);
                                        }
                                        close_conn_and_break = true;
                                        break; // Stop processing further instructions from this batch for this conn_no
                                    }
                                    frames_to_send_payloads.push(GuacdParser::guacd_encode_instruction(&instruction));
                                }
                                if close_conn_and_break {
                                    break; // Break from the outer read loop for this connection
                                }
                            }
                        } else {
                            error!("Channel({}): GuacdParser missing for conn_no {} but protocol is Guacd.", channel_id, conn_no);
                            frames_to_send_payloads.push(Bytes::copy_from_slice(current_payload_bytes_slice)); // Fallback: send raw
                        }
                    } else {
                        // For PortForward, SOCKS5 data phase - forward raw data
                        frames_to_send_payloads.push(Bytes::copy_from_slice(current_payload_bytes_slice));
                    }

                    for payload_bytes_to_send in frames_to_send_payloads {
                        if payload_bytes_to_send.is_empty() { continue; }

                        let frame = Frame::new_data_with_pool(conn_no, &payload_bytes_to_send, &buffer_pool);
                        encode_buffer.clear();
                        let bytes_written = frame.encode_into(&mut encode_buffer);
                        let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();

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
