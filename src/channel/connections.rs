// Connection management functionality for Channel

use anyhow::Result;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;
use crate::models::{Conn, ConnectionStats, StreamHalf};
use crate::tube_protocol::{Frame, ControlMessage, CloseConnectionReason};
use crate::channel::types::ActiveProtocol;
use crate::channel::guacd_parser::{GuacdInstruction, GuacdParser, PeekError};
use tracing::{debug, error, info, warn, trace};
use tokio::time::timeout;
use crate::buffer_pool::BufferPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use bytes::{Buf, BufMut, BytesMut};
use crate::channel::core::ProtocolLogicState;
use crate::channel::connect_as::decrypt_connect_as_payload;

use super::core::Channel;

const CONNECT_AS_DETAILS_LEN_FIELD_BYTES: usize = 2;
const CONNECT_AS_PUBLIC_KEY_BYTES: usize = 65; // As per Python: 65-byte public key
const CONNECT_AS_NONCE_BYTES: usize = 12;    // As per Python: 12 byte nonce

// Helper function to convert camelCase to kebab-case
fn camel_to_kebab(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch.to_lowercase().next().unwrap_or(ch));
        }
    }
    result
}

// Open a backend connection to a given address
pub async fn open_backend(channel: &mut Channel, conn_no: u32, addr: SocketAddr, active_protocol: ActiveProtocol) -> Result<()> {
    debug!(
        "Endpoint {}: Opening connection {} to {} for protocol {:?} (Channel ServerMode: {})", 
        channel.channel_id, conn_no, addr, active_protocol, channel.server_mode
    );

    // Check if the connection already exists
    if channel.conns.lock().await.contains_key(&conn_no) {
        warn!("Endpoint {}: Connection {} already exists", channel.channel_id, conn_no);
        return Ok(());
    }

    // Connect to the backend
    let stream = TcpStream::connect(addr).await?;
    trace!( 
        "Channel({}): conn_no {}: PRE-CALL to setup_outbound_task for backend address {}. ActiveProtocol: {:?}, Channel ServerMode: {}",
        channel.channel_id, conn_no, addr, active_protocol, channel.server_mode
    );
    setup_outbound_task(channel, conn_no, stream, active_protocol).await?;

    Ok(())
}

// Set up a task to read from the backend and send to WebRTC
pub async fn setup_outbound_task(channel: &mut Channel, conn_no: u32, stream: TcpStream, active_protocol: ActiveProtocol) -> Result<()> {
    let (mut backend_reader, mut backend_writer) = stream.into_split();

    let dc = channel.webrtc.clone();
    let channel_id_for_task = channel.channel_id.clone(); 
    let pending_messages = channel.pending_messages.clone();
    let buffer_pool = channel.buffer_pool.clone(); 
    let buffer_low_threshold = super::core::BUFFER_LOW_THRESHOLD;
    let is_channel_server_mode = channel.server_mode; 

    trace!( 
        "Channel({}): conn_no {}: ENTERING setup_outbound_task function. ActiveProtocol: {:?}, Captured ServerMode: {}",
        channel_id_for_task, 
        conn_no,
        active_protocol,
        is_channel_server_mode 
    );

    if active_protocol == ActiveProtocol::Guacd {
        debug!("Channel({}): Performing Guacd handshake for conn_no {}", channel_id_for_task, conn_no);
        
        let channel_id_clone = channel_id_for_task.clone(); // Already have channel_id_for_task
        let guacd_params_clone = channel.guacd_params.clone(); 
        let buffer_pool_clone = buffer_pool.clone(); // Use the already cloned buffer_pool
        let handshake_timeout_duration = channel.timeouts.guacd_handshake;

        match timeout(handshake_timeout_duration, perform_guacd_handshake(
            &mut backend_reader, 
            &mut backend_writer, 
            &channel_id_clone, 
            conn_no, 
            guacd_params_clone,
            buffer_pool_clone 
        )).await {
            Ok(Ok(_)) => { 
                debug!("Channel({}): Guacd handshake successful for conn_no {}", channel_id_clone, conn_no);
            }
            Ok(Err(e)) => {
                error!("Channel({}): Guacd handshake failed for conn_no {}: {}", channel_id_clone, conn_no, e);
                let close_payload = conn_no.to_be_bytes();
                let mut temp_control_payload_buf = buffer_pool.acquire(); // Use task's buffer_pool
                temp_control_payload_buf.clear();
                temp_control_payload_buf.extend_from_slice(&close_payload);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut temp_control_payload_buf
                );
                buffer_pool.release(temp_control_payload_buf);
                let _ = dc.send(close_frame.encode_with_pool(&buffer_pool)).await; // Use task's dc and buffer_pool
                return Err(e);
            }
            Err(_) => { 
                error!("Channel({}): Guacd handshake timed out for conn_no {}", channel_id_clone, conn_no);
                let close_payload = conn_no.to_be_bytes();
                let mut temp_control_payload_buf = buffer_pool.acquire(); // Use task's buffer_pool
                temp_control_payload_buf.clear();
                temp_control_payload_buf.extend_from_slice(&close_payload);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut temp_control_payload_buf
                );
                buffer_pool.release(temp_control_payload_buf);
                let _ = dc.send(close_frame.encode_with_pool(&buffer_pool)).await; // Use task's dc and buffer_pool
                return Err(anyhow::anyhow!("Guacd handshake timed out"));
            }
        }
    }

    let channel_id_for_log_after_spawn = channel.channel_id.clone(); 

    trace!(
        "Channel({}): conn_no {}: PRE-SPAWN (outer scope) in setup_outbound_task. ActiveProtocol: {:?}, is_channel_server_mode_for_this_task: {}",
        channel.channel_id, 
        conn_no,
        active_protocol,
        is_channel_server_mode 
    );

    let outbound_handle = tokio::spawn(async move {
        // This is the very first log inside the spawned task
        trace!( 
            "Channel({}): conn_no {}: setup_outbound_task TASK SPAWNED. ActiveProtocol: {:?}, is_channel_server_mode_in_task: {}",
            channel_id_for_task, conn_no, active_protocol, is_channel_server_mode
        );
        
        // Clone Arcs for the helper function
        let dc_clone_for_helper = dc.clone(); // dc is Arc<WebRTCDataChannel>
        let pending_messages_clone_for_helper = pending_messages.clone(); // pending_messages is Arc<Mutex<...>>

        // Define an async helper function for sending or queueing a frame
        // It returns Ok(()) if the operation was successful (sent or queued),
        // and Err(()) if a direct send failed, indicating the connection should be closed.
        async fn try_send_or_queue_frame(
            frame_to_send: bytes::Bytes,
            conn_no_local: u32,
            data_channel: &crate::webrtc_data_channel::WebRTCDataChannel,
            pending_messages_local: &Arc<Mutex<HashMap<u32, std::collections::VecDeque<bytes::Bytes>>>>,
            buffer_low_threshold_local: u64,
            channel_id_local: &str,
            active_protocol_local: ActiveProtocol,
            context_msg: &str
        ) -> Result<(), ()> {
            let buffered_amount = data_channel.buffered_amount().await;
            if buffered_amount >= buffer_low_threshold_local {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(
                        "Endpoint {}: Buffer amount high ({} bytes), QUEUEING {} for connection {} (channel_id: {}, active_protocol: {:?}). Message size: {}",
                        channel_id_local, buffered_amount, context_msg, conn_no_local, channel_id_local, active_protocol_local, frame_to_send.len()
                    );
                }
                let mut pending_guard = pending_messages_local.lock().await;
                if let Some(queue) = pending_guard.get_mut(&conn_no_local) {
                    queue.push_back(frame_to_send);
                }
                Ok(())
            } else {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(
                        "Endpoint {}: Buffer amount low ({} bytes), SENDING DIRECTLY {} for connection {} (channel_id: {}, active_protocol: {:?}). Message size: {}",
                        channel_id_local, buffered_amount, context_msg, conn_no_local, channel_id_local, active_protocol_local, frame_to_send.len()
                    );
                }
                if let Err(e) = data_channel.send(frame_to_send).await {
                    error!(
                        "Endpoint {}: Failed to send {} for connection {}: {}",
                        channel_id_local, context_msg, conn_no_local, e
                    );
                    Err(())
                } else {
                    Ok(())
                }
            }
        }
        
        // Original task logic starts here
        trace!( 
            "Channel({}): conn_no {}: setup_outbound_task ORIGINAL LOGIC START.",
            channel_id_for_task, conn_no
        );

        let mut reader = backend_reader;
        let mut eof_sent = false;
        let mut loop_iterations = 0; 
        
        let mut main_read_buffer = buffer_pool.acquire(); 
        let mut encode_buffer = buffer_pool.acquire(); 
        const MAX_READ_SIZE: usize = 8 * 1024; 
        const GUACD_BATCH_SIZE: usize = 16 * 1024; // Batch up to 16KB of Guacd instructions
        const LARGE_INSTRUCTION_THRESHOLD: usize = MAX_READ_SIZE; // If a single instruction is this big, send it directly

        // **BOLD WARNING: HOT PATH - NO STRING/OBJECT ALLOCATIONS ALLOWED IN MAIN LOOP**
        // **USE BUFFER POOL FOR ALL ALLOCATIONS**
        let mut temp_read_buffer = buffer_pool.acquire();
        if active_protocol != ActiveProtocol::Guacd {
            temp_read_buffer.clear();
            if temp_read_buffer.capacity() < MAX_READ_SIZE {
                temp_read_buffer.reserve(MAX_READ_SIZE - temp_read_buffer.capacity());
            }
        }
        
        // Batch buffer for Guacd instructions
        let mut guacd_batch_buffer = if active_protocol == ActiveProtocol::Guacd {
            Some(buffer_pool.acquire())
        } else {
            None
        };

        trace!( 
            "Channel({}): conn_no {}: setup_outbound_task BEFORE main loop.",
            channel_id_for_task, conn_no
        );
        
        // **BOLD WARNING: ENTERING HOT PATH - BACKEND→WEBRTC MAIN LOOP**
        // **NO STRING ALLOCATIONS, NO UNNECESSARY OBJECT CREATION**
        // **USE BORROWED DATA, BUFFER POOLS, AND ZERO-COPY TECHNIQUES**
        loop { 
            loop_iterations += 1;
            if tracing::enabled!(tracing::Level::TRACE) {
                trace!( 
                    "Channel({}): conn_no {}: setup_outbound_task loop iteration {}, main_read_buffer.len(): {}",
                    channel_id_for_task, conn_no, loop_iterations, main_read_buffer.len()
                );
            }

            if main_read_buffer.capacity() - main_read_buffer.len() < MAX_READ_SIZE / 2 { 
                 main_read_buffer.reserve(MAX_READ_SIZE);
            }
            
            // Ensure temp_read_buffer has enough capacity if it's going to be used
            // For Guacd, we read directly into main_read_buffer, so temp_read_buffer is not used for the read.
            if active_protocol != ActiveProtocol::Guacd {
                temp_read_buffer.clear();
                if temp_read_buffer.capacity() < MAX_READ_SIZE {
                    temp_read_buffer.reserve(MAX_READ_SIZE - temp_read_buffer.capacity());
                }
            }
            
            if tracing::enabled!(tracing::Level::TRACE) {
                trace!( 
                    "Channel({}): conn_no {}: setup_outbound_task PRE-READ from backend (active_protocol: {:?})",
                    channel_id_for_task, conn_no, active_protocol
                );
            }
            
            // **ZERO-COPY READ: Use buffer pool buffer directly**
            // For Guacd, read directly into main_read_buffer to append.
            // For others, use temp_read_buffer for a single pass.
            let n_read = if active_protocol == ActiveProtocol::Guacd {
                // Ensure main_read_buffer has enough capacity *before* the read_buf call
                // This is slightly different from its previous position but more direct for this path.
                if main_read_buffer.capacity() - main_read_buffer.len() < MAX_READ_SIZE {
                    main_read_buffer.reserve(MAX_READ_SIZE); 
                }
                match reader.read_buf(&mut main_read_buffer).await { // Read appends to main_read_buffer
                    Ok(n) => n,
                    Err(e) => {
                        error!("Endpoint {}: Read error on connection {} (Guacd path): {}", channel_id_for_task, conn_no, e);
                        break;
                    }
                }
            } else {
                match reader.read_buf(&mut temp_read_buffer).await { // read_buf clears and fills temp_read_buffer
                    Ok(n) => n,
                    Err(e) => {
                        error!("Endpoint {}: Read error on connection {} (Non-Guacd path): {}", channel_id_for_task, conn_no, e);
                        break;
                    }
                }
            };
            
            match n_read {
                0 => {
                    if tracing::enabled!(tracing::Level::TRACE) {
                        trace!( 
                            "Channel({}): conn_no {}: setup_outbound_task POST-READ 0 bytes (EOF). EOF_sent: {}",
                            channel_id_for_task, conn_no, eof_sent
                        );
                    }

                    if !eof_sent {
                        let eof_frame = Frame::new_control_with_pool(
                            ControlMessage::SendEOF,
                            &conn_no.to_be_bytes(),
                            &buffer_pool
                        );
                        let encoded = eof_frame.encode_with_pool(&buffer_pool);
                        let _ = dc.send(encoded).await;
                        eof_sent = true;
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    } else {
                        break;
                    }
                    continue; 
                }
                _ => {
                    if tracing::enabled!(tracing::Level::TRACE) {
                        trace!( 
                            "Channel({}): conn_no {}: setup_outbound_task POST-READ {} bytes from backend. EOF_sent: {}",
                            channel_id_for_task, conn_no, n_read, eof_sent
                        );
                    }
                    
                    eof_sent = false;
                    let mut close_conn_and_break = false;

                    if active_protocol == ActiveProtocol::Guacd {
                        // **BOLD WARNING: GUACD PARSING HOT PATH**
                        // **DO NOT CREATE STRINGS OR ALLOCATE OBJECTS UNNECESSARILY**
                        // **USE is_error_opcode FLAG TO AVOID PARSING ERROR INSTRUCTIONS**
                    
                        
                        let mut consumed_offset = 0;
                        loop {
                            if consumed_offset >= main_read_buffer.len() {
                                break; 
                            }
                            let current_slice = &main_read_buffer[consumed_offset..main_read_buffer.len()];
                            
                            #[cfg(feature = "profiling")]
                            let parse_start = std::time::Instant::now();
                            
                            // **ULTRA-FAST PATH: Only validate format and check for error**
                            match GuacdParser::validate_and_check_error(current_slice) {
                                Ok((instruction_len, is_error)) => {
                                    #[cfg(feature = "profiling")]
                                    {
                                        let parse_duration = parse_start.elapsed();
                                        if parse_duration.as_micros() > 100 {
                                            warn!(
                                                "Channel({}): Slow Guacd validate: {}μs",
                                                channel_id_for_task, parse_duration.as_micros()
                                            );
                                        }
                                    }
                                    
                                    if is_error {
                                        // Only parse the full instruction if it's an error
                                        match GuacdParser::peek_instruction(current_slice) {
                                            Ok(error_instr) => {
                                                error!(
                                                    "Channel({}): Conn {}: Guacd sent error opcode. Args: {:?}. Closing connection.",
                                                    channel_id_for_task, conn_no, error_instr.args
                                                );
                                            }
                                            Err(_) => {
                                                error!(
                                                    "Channel({}): Conn {}: Guacd sent error opcode but failed to parse args. Closing connection.",
                                                    channel_id_for_task, conn_no
                                                );
                                            }
                                        }
                                        
                                        // Common error handling for Guacd "error" opcode
                                        let close_payload_bytes = conn_no.to_be_bytes();
                                        let mut temp_buf_for_control = buffer_pool.acquire();
                                        temp_buf_for_control.clear();
                                        temp_buf_for_control.extend_from_slice(&close_payload_bytes);
                                        let close_frame = Frame::new_control_with_buffer(
                                            ControlMessage::CloseConnection,
                                            &mut temp_buf_for_control
                                        );
                                        buffer_pool.release(temp_buf_for_control);
                                        if let Err(send_err) = dc.send(close_frame.encode_with_pool(&buffer_pool)).await {
                                            error!(
                                                "Channel({}): Conn {}: Failed to send CloseConnection frame for Guacd error: {}",
                                                channel_id_for_task, conn_no, send_err
                                            );
                                        }
                                        close_conn_and_break = true;
                                        break; 
                                    }

                                    // Batch Guacd instructions for efficiency
                                    if let Some(ref mut batch_buffer) = guacd_batch_buffer {
                                        let instruction_data = &current_slice[..instruction_len];
                                        
                                        if instruction_data.len() >= LARGE_INSTRUCTION_THRESHOLD {
                                            // If large, first flush any existing batch
                                            if !batch_buffer.is_empty() {
                                                encode_buffer.clear();
                                                let bytes_written = Frame::encode_data_frame_from_slice(
                                                    &mut encode_buffer, conn_no, &batch_buffer[..]);
                                                let batch_frame_bytes = encode_buffer.split_to(bytes_written).freeze();
                                                if try_send_or_queue_frame(batch_frame_bytes, conn_no, &dc_clone_for_helper, &pending_messages_clone_for_helper, buffer_low_threshold, &channel_id_for_task, active_protocol, "(pre-large) batch").await.is_err() {
                                                    close_conn_and_break = true;
                                                }
                                                batch_buffer.clear();
                                                if close_conn_and_break { break; }
                                            }

                                            // Now send the large instruction directly
                                            encode_buffer.clear();
                                            let bytes_written = Frame::encode_data_frame_from_slice(
                                                &mut encode_buffer, conn_no, instruction_data);
                                            let large_frame_bytes = encode_buffer.split_to(bytes_written).freeze();
                                            if try_send_or_queue_frame(large_frame_bytes, conn_no, &dc_clone_for_helper, &pending_messages_clone_for_helper, buffer_low_threshold, &channel_id_for_task, active_protocol, "large instruction").await.is_err() {
                                                close_conn_and_break = true;
                                            }
                                            // No need to add to batch_buffer if sent directly

                                        } else { // Instruction is not large, proceed with normal batching
                                            if batch_buffer.len() + instruction_data.len() > GUACD_BATCH_SIZE && !batch_buffer.is_empty() {
                                                encode_buffer.clear();
                                                let bytes_written = Frame::encode_data_frame_from_slice(
                                                    &mut encode_buffer, conn_no, &batch_buffer[..]);
                                                let batch_frame_bytes = encode_buffer.split_to(bytes_written).freeze();
                                                if try_send_or_queue_frame(batch_frame_bytes, conn_no, &dc_clone_for_helper, &pending_messages_clone_for_helper, buffer_low_threshold, &channel_id_for_task, active_protocol, "batch").await.is_err() {
                                                    close_conn_and_break = true;
                                                }
                                                batch_buffer.clear();
                                                if close_conn_and_break { break; }
                                            }
                                            batch_buffer.extend_from_slice(instruction_data);
                                        }
                                        if close_conn_and_break { break; }
                                    }
                                    consumed_offset += instruction_len;
                                }
                                Err(PeekError::Incomplete) => {
                                    break; 
                                }
                                Err(e) => { // Other PeekErrors
                                    error!(
                                        "Channel({}): Conn {}: Error peeking/parsing Guacd instruction: {:?}. Buffer content (approx): {:?}. Closing connection.",
                                        channel_id_for_task, conn_no, e, &main_read_buffer[..std::cmp::min(main_read_buffer.len(), 100)] 
                                    );
                                    let close_payload_bytes = conn_no.to_be_bytes();
                                    let mut temp_buf_for_control = buffer_pool.acquire();
                                    temp_buf_for_control.clear();
                                    temp_buf_for_control.extend_from_slice(&close_payload_bytes);
                                    let close_frame = Frame::new_control_with_buffer(
                                        ControlMessage::CloseConnection,
                                        &mut temp_buf_for_control
                                    );
                                    buffer_pool.release(temp_buf_for_control);
                                    if let Err(send_err) = dc.send(close_frame.encode_with_pool(&buffer_pool)).await {
                                        error!(
                                            "Channel({}): Conn {}: Failed to send CloseConnection frame for Guacd parsing error: {}",
                                            channel_id_for_task, conn_no, send_err
                                        );
                                    }
                                    close_conn_and_break = true;
                                    break; 
                                }
                            }
                        } // End of inner Guacd processing loop
                        
                        // Flush any remaining batched Guacd data
                        if let Some(ref mut batch_buffer) = guacd_batch_buffer {
                            if !batch_buffer.is_empty() && !close_conn_and_break {
                                encode_buffer.clear();
                                let bytes_written = Frame::encode_data_frame_from_slice(
                                    &mut encode_buffer,
                                    conn_no,
                                    &batch_buffer[..],
                                );
                                let final_batch_frame_bytes = encode_buffer.split_to(bytes_written).freeze();
                                if try_send_or_queue_frame(final_batch_frame_bytes, conn_no, &dc_clone_for_helper, &pending_messages_clone_for_helper, buffer_low_threshold, &channel_id_for_task, active_protocol, "final batch").await.is_err() {
                                    close_conn_and_break = true; // This will be checked after the Guacd block
                                }
                                batch_buffer.clear();
                            }
                        }
                        
                        if close_conn_and_break { // If Guacd processing decided to close
                            main_read_buffer.clear(); 
                        } else if consumed_offset > 0 {
                            main_read_buffer.advance(consumed_offset);
                        }

                    } else { // Not Guacd protocol (e.g., PortForward, SOCKS5)
                        // **BOLD WARNING: ZERO-COPY HOT PATH FOR PORT FORWARDING**
                        // **ENCODE DIRECTLY FROM READ BUFFER - NO COPIES**
                        // **SEND DIRECTLY - NO INTERMEDIATE VECTOR**
                        encode_buffer.clear();
                        
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                "Channel({}): conn_no {}: PortForward/SOCKS5 zero-copy encode. Read {} bytes into temp_read_buffer",
                                channel_id_for_task, conn_no, n_read
                            );
                        }
                        
                        // Encode directly from temp_read_buffer (which was filled by read_buf)
                        let bytes_written = Frame::encode_data_frame_from_slice(
                            &mut encode_buffer,
                            conn_no,
                            &temp_read_buffer[..],
                        );
                        
                        let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();
                        
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                "Channel({}): conn_no {}: PortForward/SOCKS5 POST-ENCODE. Bytes written: {}",
                                channel_id_for_task, conn_no, bytes_written
                            );
                        }
                        
                        // **PERFORMANCE: Send directly without intermediate storage**
                        let buffered_amount = dc.buffered_amount().await;
                        if buffered_amount >= buffer_low_threshold {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!( 
                                    "Endpoint {}: Buffer amount high ({} bytes), QUEUEING message for connection {} (channel_id: {}, active_protocol: {:?}). Message size: {}",
                                    channel_id_for_task, buffered_amount, conn_no, channel_id_for_task, active_protocol, encoded_frame_bytes.len()
                                );
                            }
                            let mut pending_guard = pending_messages.lock().await;
                            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                                queue.push_back(encoded_frame_bytes);
                            }
                        } else {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!( 
                                    "Endpoint {}: Buffer amount low ({} bytes), SENDING DIRECTLY message for connection {} (channel_id: {}, active_protocol: {:?}). Message size: {}",
                                    channel_id_for_task, buffered_amount, conn_no, channel_id_for_task, active_protocol, encoded_frame_bytes.len()
                                );
                            }
                            if let Err(e) = dc.send(encoded_frame_bytes).await {
                                error!(
                                    "Endpoint {}: Failed to send data for connection {}: {}",
                                    channel_id_for_task, conn_no, e
                                );
                                close_conn_and_break = true;
                            }
                        }
                    }
                    
                    if close_conn_and_break { break; }
                }
            }
        }
        debug!("Endpoint {}: Backend→WebRTC task for connection {} exited", channel_id_for_task, conn_no);
        buffer_pool.release(main_read_buffer);
        buffer_pool.release(encode_buffer);
        buffer_pool.release(temp_read_buffer);
        
        // Release the batch buffer if it was used
        if let Some(batch_buffer) = guacd_batch_buffer {
            buffer_pool.release(batch_buffer);
        }
        
        let mut pending_guard = pending_messages.lock().await;
        pending_guard.remove(&conn_no);
    });

    trace!( 
        "Channel({}): conn_no {}: setup_outbound_task tokio::spawn COMPLETE (outer scope). Outbound handle created.",
        channel_id_for_log_after_spawn, 
        conn_no
    );
            
    let stream_half = StreamHalf {
        reader: None,
        writer: backend_writer,
    };
    
    channel.conns.lock().await.insert(conn_no, Conn {
        backend: Box::new(stream_half),
        to_webrtc: outbound_handle,
        stats: ConnectionStats::default(),
    });
    
    debug!("Endpoint {}: Connection {} added to registry", channel.channel_id, conn_no); 

    Ok(())
}

// --- Helper function for Guacd Handshake ---
// A stateless GuacdParser that manages its own BytesMut buffer for reading from the socket,
// then passes slices of this buffer to GuacdParser::peek_instruction and GuacdParser::parse_instruction_content.
pub(crate) async fn perform_guacd_handshake<R, W>(
    reader: &mut R,
    writer: &mut W,
    channel_id: &str,
    conn_no: u32,
    guacd_params_arc: Arc<Mutex<HashMap<String, String>>>,
    buffer_pool: BufferPool, 
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + ?Sized,
    W: AsyncWriteExt + Unpin + Send + ?Sized,
{
    let mut handshake_buffer = buffer_pool.acquire(); 
    let mut current_handshake_buffer_len = 0;

    async fn read_expected_instruction_stateless<'a, SHelper>(
        reader: &'a mut SHelper,
        handshake_buffer: &'a mut BytesMut,
        current_buffer_len: &'a mut usize, 
        channel_id: &'a str,
        conn_no: u32,
        expected_opcode: &'a str
    ) -> Result<GuacdInstruction>
    where 
        SHelper: AsyncRead + Unpin + Send + ?Sized, 
    {
        loop {
            // Process peek result and extract what we need
            let process_result = {
                let peek_result = GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len]);
                
                match peek_result {
                    Ok(peeked_instr) => {
                        let instruction_total_len = peeked_instr.total_length_in_buffer;
                        if instruction_total_len == 0 || instruction_total_len > *current_buffer_len { 
                            error!(
                                target: "guac_protocol", channel_id=%channel_id, conn_no,
                                "Invalid instruction length peeked ({}) vs buffer len ({}). Opcode: '{}'. Buffer (approx): {:?}",
                                instruction_total_len, *current_buffer_len, peeked_instr.opcode, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)]
                            );
                            return Err(anyhow::anyhow!("Peeked instruction length is invalid or exceeds buffer."));
                        }
                        let content_slice = &handshake_buffer[..instruction_total_len - 1]; 
                        
                        let instruction = GuacdParser::parse_instruction_content(content_slice).map_err(|e| 
                            anyhow::anyhow!("Handshake: Conn {}: Failed to parse peeked Guacd instruction (opcode: '{}'): {}. Content: {:?}", conn_no, peeked_instr.opcode, e, content_slice)
                        )?;
                        
                        // Extract what we need from peeked_instr before it goes out of scope
                        let expected_opcode_check = peeked_instr.opcode == expected_opcode;
                        
                        // Return the instruction and advance amount
                        Some((instruction, instruction_total_len, expected_opcode_check))
                    }
                    Err(PeekError::Incomplete) => {
                        // Need more data
                        None
                    }
                    Err(err) => { 
                        let err_msg = format!("Error peeking Guacd instruction while expecting '{}': {:?}. Buffer content (approx): {:?}", expected_opcode, err, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)]);
                        error!(target: "guac_protocol", channel_id=%channel_id, conn_no, %err_msg);
                        return Err(anyhow::anyhow!(err_msg));
                    }
                }
            }; // peek_result is dropped here
            
            // Now we can safely mutate handshake_buffer
            if let Some((instruction, advance_len, expected_opcode_check)) = process_result {
                handshake_buffer.advance(advance_len);
                *current_buffer_len -= advance_len;

                if instruction.opcode == "error" { 
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, error_opcode=%instruction.opcode, error_args=?instruction.args, expected_opcode=%expected_opcode, "Guacd sent error during handshake");
                    return Err(anyhow::anyhow!("Guacd sent error '{}' ({:?}) during handshake (expected '{}')", instruction.opcode, instruction.args, expected_opcode));
                }
                return if expected_opcode_check {
                    Ok(instruction)
                } else {
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, expected_opcode=%expected_opcode, received_opcode=%instruction.opcode, received_args=?instruction.args, "Unexpected Guacd opcode");
                    Err(anyhow::anyhow!("Expected Guacd opcode '{}', got '{}' with args {:?}", expected_opcode, instruction.opcode, instruction.args))
                }
            }
            
            // Handle the incomplete case - need to read more data
            let mut temp_read_buf = [0u8; 1024];
            match reader.read(&mut temp_read_buf).await {
                Ok(0) => {
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, expected_opcode=%expected_opcode, buffer_len = *current_buffer_len, "EOF during Guacd handshake");
                    return Err(anyhow::anyhow!("EOF during Guacd handshake while waiting for '{}' (incomplete data in buffer)", expected_opcode));
                }
                Ok(n_read) => {
                    if handshake_buffer.capacity() < *current_buffer_len + n_read {
                        handshake_buffer.reserve(*current_buffer_len + n_read - handshake_buffer.capacity());
                    }
                    handshake_buffer.put_slice(&temp_read_buf[..n_read]);
                    *current_buffer_len += n_read;
                    trace!(target: "guac_protocol", channel_id=%channel_id, conn_no, bytes_read=n_read, new_buffer_len=*current_buffer_len, "Read more data for handshake, waiting for '{}'", expected_opcode);
                }
                Err(e) => {
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, expected_opcode=%expected_opcode, error=%e, "Read error waiting for Guacd instruction");
                    return Err(e.into());
                }
            }
        }
    }
    let guacd_params_locked = guacd_params_arc.lock().await;

    let protocol_name_from_params = guacd_params_locked.get("protocol").cloned().unwrap_or_else(|| {
        warn!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd 'protocol' missing in guacd_params, defaulting to 'rdp' for select fallback.");
        "rdp".to_string()
    });
    
    let join_connection_id_key = "connectionid";
    let join_connection_id_opt = guacd_params_locked.get(join_connection_id_key).cloned(); 
    trace!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?join_connection_id_opt, key_looked_up=%join_connection_id_key, "Checked for join connection ID in guacd_params");

    let select_arg: String;
    if let Some(id_to_join) = &join_connection_id_opt {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, session_to_join=%id_to_join, "Guacd Handshake: Preparing to join existing session.");
        select_arg = id_to_join.clone();
    } else {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, protocol=%protocol_name_from_params, "Guacd Handshake: Preparing for new session with protocol.");
        select_arg = protocol_name_from_params;
    }
    
    let readonly_param_key = "readonly";
    let readonly_param_value_from_map = guacd_params_locked.get(readonly_param_key).cloned();
    trace!(target: "guac_protocol_debug", channel_id=%channel_id, conn_no, readonly_param_value_from_map = ?readonly_param_value_from_map, "Initial 'readonly' value from guacd_params_locked for join attempt.");

    let readonly_str_for_join = readonly_param_value_from_map.unwrap_or_else(|| "false".to_string());
    trace!(target: "guac_protocol_debug", channel_id=%channel_id, conn_no, readonly_str_for_join = %readonly_str_for_join, "Effective 'readonly_str_for_join' (after unwrap_or_else) for join attempt.");
        
    let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
    trace!(target: "guac_protocol_debug", channel_id=%channel_id, conn_no, is_readonly_bool = %is_readonly, "Final 'is_readonly' boolean for join attempt.");

    let width_for_new = guacd_params_locked.get("width").cloned().unwrap_or_else(|| "1024".to_string());
    let height_for_new = guacd_params_locked.get("height").cloned().unwrap_or_else(|| "768".to_string());
    let dpi_for_new = guacd_params_locked.get("dpi").cloned().unwrap_or_else(|| "96".to_string());
    let audio_mimetypes_str_for_new = guacd_params_locked.get("audio").cloned().unwrap_or_default();
    let video_mimetypes_str_for_new = guacd_params_locked.get("video").cloned().unwrap_or_default();
    let image_mimetypes_str_for_new = guacd_params_locked.get("image").cloned().unwrap_or_default();
    
    let connect_params_for_new_conn: HashMap<String,String> = if join_connection_id_opt.is_none() {
        guacd_params_locked.clone() 
    } else {
        HashMap::new() 
    };
    drop(guacd_params_locked); 
    
    let select_instruction = GuacdInstruction::new("select".to_string(), vec![select_arg.clone()]);
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, instruction=?select_instruction, "Guacd Handshake: Sending 'select'");
    writer.write_all(&GuacdParser::guacd_encode_instruction(&select_instruction)).await?;
    writer.flush().await?;
    
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Waiting for 'args'");
    let args_instruction = read_expected_instruction_stateless(reader, &mut handshake_buffer, &mut current_handshake_buffer_len, channel_id, conn_no, "args").await?;
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, received_args=?args_instruction.args, "Guacd Handshake: Received 'args' from Guacd server");

    const EXPECTED_GUACD_VERSION: &str = "VERSION_1_5_0";
    let connect_version_arg = args_instruction.args.get(0).cloned().unwrap_or_else(|| {
        warn!(target: "guac_protocol", channel_id=%channel_id, conn_no, "'args' instruction missing version, defaulting to {}", EXPECTED_GUACD_VERSION);
        EXPECTED_GUACD_VERSION.to_string()
    });
    if connect_version_arg != EXPECTED_GUACD_VERSION {
        warn!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd version mismatch. Expected: '{}', Received: '{}'. Proceeding with received version for connect.", EXPECTED_GUACD_VERSION, connect_version_arg);
    }

    let mut connect_args: Vec<String> = Vec::new();
    connect_args.push(connect_version_arg); 

    if join_connection_id_opt.is_some() {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Preparing 'connect' for JOINING session.");
        let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true"); 
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, requested_readonly_param=%readonly_str_for_join, is_readonly_for_connect=is_readonly, "Readonly status for join.");

        for (idx, arg_name_from_guacd) in args_instruction.args.iter().enumerate() {
            if idx == 0 { continue; } 
            
            let is_readonly_arg_name_literal = "read-only";
            let is_current_arg_readonly_keyword = arg_name_from_guacd == is_readonly_arg_name_literal;
            
            trace!(target: "guac_protocol", channel_id=%channel_id, conn_no, 
                   current_arg_name_from_guacd=%arg_name_from_guacd, 
                   is_readonly_param_from_config=%is_readonly, 
                   is_current_arg_the_readonly_keyword=is_current_arg_readonly_keyword, 
                   "Looping for connect_args (join). Comparing '{}' with '{}'", 
                   arg_name_from_guacd, is_readonly_arg_name_literal);

            if is_current_arg_readonly_keyword { 
                let value_to_push = if is_readonly { "true".to_string() } else { "".to_string() };
                debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, 
                       arg_name_being_processed = %arg_name_from_guacd, 
                       is_readonly_flag_for_push = %is_readonly,
                       value_being_pushed_for_readonly_arg = %value_to_push,
                       "Pushing to connect_args for 'read-only' keyword");
                connect_args.push(value_to_push); 
            } else {
                connect_args.push("".to_string()); 
            }
        }
    } else {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Preparing 'connect' for NEW session.");
        
        let parse_mimetypes = |mimetype_str: &str| -> Vec<String> {
            if mimetype_str.is_empty() { return Vec::new(); }
            serde_json::from_str::<Vec<String>>(mimetype_str)
                .unwrap_or_else(|e| {
                    debug!(target:"guac_protocol", channel_id=%channel_id, conn_no, error=%e, "Failed to parse mimetype string '{}' as JSON array, splitting by comma as fallback.", mimetype_str);
                    mimetype_str.split(',').map(String::from).filter(|s| !s.is_empty()).collect()
                })
        };

        let size_parts: Vec<String> = width_for_new.split(',').chain(height_for_new.split(',')).chain(dpi_for_new.split(',')).map(String::from).collect();
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?size_parts, "Guacd Handshake (new): Sending 'size'");
        writer.write_all(&GuacdParser::guacd_encode_instruction(&GuacdInstruction::new("size".to_string(), size_parts))).await?;
        writer.flush().await?;

        let audio_mimetypes = parse_mimetypes(&audio_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?audio_mimetypes, "Guacd Handshake (new): Sending 'audio'");
        writer.write_all(&GuacdParser::guacd_encode_instruction(&GuacdInstruction::new("audio".to_string(), audio_mimetypes))).await?;
        writer.flush().await?;

        let video_mimetypes = parse_mimetypes(&video_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?video_mimetypes, "Guacd Handshake (new): Sending 'video'");
        writer.write_all(&GuacdParser::guacd_encode_instruction(&GuacdInstruction::new("video".to_string(), video_mimetypes))).await?;
        writer.flush().await?;

        let image_mimetypes = parse_mimetypes(&image_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?image_mimetypes, "Guacd Handshake (new): Sending 'image'");
        writer.write_all(&GuacdParser::guacd_encode_instruction(&GuacdInstruction::new("image".to_string(), image_mimetypes))).await?;
        writer.flush().await?;
        
        for arg_name_from_guacd in args_instruction.args.iter().skip(1) { 
            let param_value = connect_params_for_new_conn.get(arg_name_from_guacd.replace('_', "-").as_str()) 
                                .or_else(|| connect_params_for_new_conn.get(arg_name_from_guacd.as_str()))
                                .or_else(|| {
                                    // Try to find the camelCase version of the parameter
                                    connect_params_for_new_conn.keys()
                                        .find(|k| camel_to_kebab(k) == *arg_name_from_guacd)
                                        .and_then(|k| connect_params_for_new_conn.get(k))
                                })
                                .cloned()
                                .unwrap_or_else(String::new);
            connect_args.push(param_value);
        }
    }
    
    let connect_instruction = GuacdInstruction::new("connect".to_string(), connect_args.clone());
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, instruction=?connect_instruction, "Guacd Handshake: Sending 'connect'");
    writer.write_all(&GuacdParser::guacd_encode_instruction(&connect_instruction)).await?;
    writer.flush().await?;

    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Waiting for 'ready'");
    let ready_instruction = read_expected_instruction_stateless(reader, &mut handshake_buffer, &mut current_handshake_buffer_len, channel_id, conn_no, "ready").await?;
    if let Some(client_id_from_ready) = ready_instruction.args.get(0) {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, guacd_client_id=%client_id_from_ready, "Guacd handshake completed.");
    } else {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd handshake completed. No client ID received with 'ready'.");
    }
    buffer_pool.release(handshake_buffer); 
    Ok(())
}

// Process an inbound message (typically from WebRTC, to be sent to a backend connection)
pub async fn process_inbound_message(channel: &mut Channel, frame: Frame) -> Result<()> {
    // **BOLD WARNING: HOT PATH - WebRTC→BACKEND MESSAGE PROCESSING**
    // **NO UNNECESSARY STRING/OBJECT ALLOCATIONS**
    // **USE BUFFER POOLS AND BORROWED DATA**
    
    let conn_no = frame.connection_no;
    
    if frame.connection_no == 0 { // Control message
        // Buffer for operations that might need it (like Pong payload)
        let mut temp_payload_buffer = channel.buffer_pool.acquire();

        if frame.payload.len() >= crate::tube_protocol::CTRL_NO_LEN { 
            let mut payload_cursor = std::io::Cursor::new(&frame.payload);
            let control_code = payload_cursor.get_u16();
            match ControlMessage::try_from(control_code) {
                Ok(ctrl_msg) => {
                    debug!("Channel({}): Received control message: {:?} (frame.conn_no=0, target identified in payload if applicable)", channel.channel_id, ctrl_msg);
                    match ctrl_msg {
                        ControlMessage::OpenConnection => {
                            // Payload format: target_conn_no (u32), then protocol-specific data
                            if payload_cursor.remaining() >= 4 { // Minimum for target_conn_no
                                let target_conn_no = payload_cursor.get_u32();
                                info!("Channel({}): Received OpenConnection request for target_conn_no {}", channel.channel_id, target_conn_no);

                                let open_result = match channel.active_protocol {
                                    ActiveProtocol::Guacd => {
                                        let mut effective_guacd_host = channel.guacd_host.clone();
                                        let mut effective_guacd_port = channel.guacd_port;

                                        // ConnectAs logic
                                        if (channel.connect_as_settings.allow_supply_user || channel.connect_as_settings.allow_supply_host) &&
                                           channel.connect_as_settings.gateway_private_key.is_some() {
                                            
                                            debug!("Channel({}): Attempting ConnectAs for Guacd target_conn_no {}", channel.channel_id, target_conn_no);

                                            // Check if enough data for connect_as fields
                                            // connect_as_payload_len (u16) + public_key (65) + nonce (12)
                                            const MIN_CONNECT_AS_HEADER_LEN: usize = CONNECT_AS_DETAILS_LEN_FIELD_BYTES + CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES;
                                            
                                            if payload_cursor.remaining() >= MIN_CONNECT_AS_HEADER_LEN {
                                                let connect_as_payload_len = payload_cursor.get_u16() as usize;
                                                
                                                // **PERFORMANCE: Use buffer pool for crypto operations**
                                                let mut crypto_buffer = channel.buffer_pool.acquire();
                                                crypto_buffer.clear();
                                                crypto_buffer.resize(CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES + connect_as_payload_len, 0);
                                                
                                                payload_cursor.copy_to_slice(&mut crypto_buffer[..]);
                                                
                                                let client_public_key_bytes = &crypto_buffer[..CONNECT_AS_PUBLIC_KEY_BYTES];
                                                let nonce_bytes = &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES..CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES];
                                                let encrypted_data_bytes = &crypto_buffer[CONNECT_AS_PUBLIC_KEY_BYTES + CONNECT_AS_NONCE_BYTES..];

                                                if payload_cursor.remaining() >= connect_as_payload_len {
                                                    match decrypt_connect_as_payload(
                                                        channel.connect_as_settings.gateway_private_key.as_ref().unwrap(), // Safe due to is_some() check
                                                        client_public_key_bytes,
                                                        nonce_bytes,
                                                        encrypted_data_bytes,
                                                    ) {
                                                        Ok(decrypted_payload) => {
                                                            info!("Channel({}): Successfully decrypted ConnectAs payload for target_conn_no {}", channel.channel_id, target_conn_no);
                                                            let mut guacd_params_locked = channel.guacd_params.lock().await;

                                                            if channel.connect_as_settings.allow_supply_user {
                                                                if let Some(user_details) = decrypted_payload.user {
                                                                    debug!("Channel({}): Applying ConnectAs user details: {:?}", channel.channel_id, user_details);
                                                                    if let Some(val) = user_details.username { guacd_params_locked.insert("username".to_string(), val); }
                                                                    if let Some(val) = user_details.password { guacd_params_locked.insert("password".to_string(), val); }
                                                                    if let Some(val) = user_details.private_key { guacd_params_locked.insert("private-key".to_string(), val); } // Guacamole uses 'private-key'
                                                                    if let Some(val) = user_details.private_key_passphrase { guacd_params_locked.insert("passphrase".to_string(), val); } // Guacamole uses 'passphrase'
                                                                    if let Some(val) = user_details.domain { guacd_params_locked.insert("domain".to_string(), val); }
                                                                    if let Some(val) = user_details.connect_database { guacd_params_locked.insert("database".to_string(), val); } // Common for SQL
                                                                    if let Some(val) = user_details.distinguished_name { guacd_params_locked.insert("user-dn".to_string(), val); } // Example for LDAP/X.500
                                                                }
                                                            }

                                                            if channel.connect_as_settings.allow_supply_host {
                                                                if let Some(host) = decrypted_payload.host {
                                                                    debug!("Channel({}): ConnectAs supplied host: {}", channel.channel_id, host);
                                                                    effective_guacd_host = Some(host);
                                                                }
                                                                if let Some(port) = decrypted_payload.port {
                                                                    debug!("Channel({}): ConnectAs supplied port: {}", channel.channel_id, port);
                                                                    effective_guacd_port = Some(port);
                                                                }
                                                            }
                                                            // Guacd params are updated, next steps will use them
                                                        }
                                                        Err(e) => {
                                                            error!("Channel({}): Failed to decrypt or parse ConnectAs payload for target_conn_no {}: {}", channel.channel_id, target_conn_no, e);
                                                            // Fall through to use default guacd_host/port or error if not configured
                                                            // Or explicitly return error here if ConnectAs was mandatory.
                                                            // For now, let's make it an error that prevents connection.
                                                            return Err(anyhow::anyhow!("ConnectAs decryption/parsing failed: {}", e));
                                                        }
                                                    }
                                                    channel.buffer_pool.release(crypto_buffer);
                                                } else {
                                                    channel.buffer_pool.release(crypto_buffer);
                                                    warn!("Channel({}): ConnectAs payload too short for encrypted data (expected {}, got {}) for target_conn_no {}",
                                                        channel.channel_id, connect_as_payload_len, payload_cursor.remaining(), target_conn_no);
                                                    return Err(anyhow::anyhow!("ConnectAs payload too short for encrypted data"));
                                                }
                                            } else {
                                                 warn!("Channel({}): ConnectAs payload too short for headers (key, nonce) for target_conn_no {}", channel.channel_id, target_conn_no);
                                                 return Err(anyhow::anyhow!("ConnectAs payload too short for headers"));
                                            }
                                        } // End of ConnectAs logic block

                                        // Proceed with effective host/port
                                        if let (Some(host), Some(port)) = (effective_guacd_host.as_deref(), effective_guacd_port) {
                                            match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
                                                Ok(mut addrs) => {
                                                    if let Some(socket_addr) = addrs.next() {
                                                        debug!("Channel({}): Guacd OpenConnection for target_conn_no {} to pre-configured {}:{} (resolved to {}).", 
                                                            channel.channel_id, target_conn_no, host, port, socket_addr);
                                                        open_backend(channel, target_conn_no, socket_addr, ActiveProtocol::Guacd).await
                                                    } else {
                                                        Err(anyhow::anyhow!("Could not resolve pre-configured Guacd host {}:{} to any SocketAddr", host, port))
                                                    }
                                                }
                                                Err(e) => Err(anyhow::anyhow!("DNS lookup failed for pre-configured Guacd host {}:{}: {}", host, port, e)),
                                            }
                                        } else {
                                            Err(anyhow::anyhow!("Guacd host/port not configured on channel for OpenConnection"))
                                        }
                                    }
                                    ActiveProtocol::PortForward => {
                                        if let ProtocolLogicState::PortForward(pf_state) = &channel.protocol_state {
                                            if let (Some(host), Some(port)) = (pf_state.target_host.as_deref(), pf_state.target_port) {
                                                match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
                                                    Ok(mut addrs) => {
                                                        if let Some(socket_addr) = addrs.next() {
                                                            debug!("Channel({}): PortForward OpenConnection for target_conn_no {} to pre-configured {}:{} (resolved to {}).", 
                                                                channel.channel_id, target_conn_no, host, port, socket_addr);
                                                            open_backend(channel, target_conn_no, socket_addr, ActiveProtocol::PortForward).await
                                                        } else {
                                                            Err(anyhow::anyhow!("Could not resolve pre-configured PortForward host {}:{} to any SocketAddr", host, port))
                                                        }
                                                    }
                                                    Err(e) => Err(anyhow::anyhow!("DNS lookup failed for pre-configured PortForward host {}:{}: {}", host, port, e)),
                                                }
                                            } else {
                                                Err(anyhow::anyhow!("PortForward target host/port not configured in channel protocol state"))
                                            }
                                        } else {
                                            Err(anyhow::anyhow!("Channel is in PortForward mode, but protocol state is not PortForward")) // Should not happen
                                        }
                                    }
                                    ActiveProtocol::Socks5 => {
                                        // Payload: host_len (u16), host (String), port (u16)
                                        // We are deferring SOCKS5 specific payload parsing for now.
                                        // This branch will effectively fail or use placeholder logic.
                                        warn!("Channel({}): SOCKS5 OpenConnection payload parsing is not fully implemented for target_conn_no {}. Deferring detailed parsing.", channel.channel_id, target_conn_no);
                                        // For now, let's return an error to indicate it's not supported in this path yet,
                                        // rather than trying to connect to a default/unspecified SOCKS5 target.
                                        Err(anyhow::anyhow!("SOCKS5 OpenConnection payload parsing deferred"))
                                    }
                                };

                                if let Err(e) = open_result {
                                    error!("Channel({}): Failed to process OpenConnection for target_conn_no {}: {}. Sending CloseConnection back.", 
                                        channel.channel_id, target_conn_no, e);
                                    temp_payload_buffer.clear(); 
                                    temp_payload_buffer.put_u32(target_conn_no); // The conn_no that failed
                                    temp_payload_buffer.put_u16(CloseConnectionReason::ConnectionFailed as u16);
                                    if let Err(send_err) = channel.send_control_message(ControlMessage::CloseConnection, &temp_payload_buffer).await {
                                        error!("Channel({}): Failed to send CloseConnection for failed OpenConnection {}: {}", channel.channel_id, target_conn_no, send_err);
                                    }
                                } else {
                                    // If open_backend succeeded, send ConnectionOpened
                                    info!("Channel({}): Successfully processed OpenConnection for target_conn_no {}, sending ConnectionOpened.", channel.channel_id, target_conn_no);
                                    temp_payload_buffer.clear();
                                    temp_payload_buffer.put_u32(target_conn_no);
                                    if let Err(e) = channel.send_control_message(ControlMessage::ConnectionOpened, &temp_payload_buffer).await {
                                        error!("Channel({}): Error sending ConnectionOpened for conn_no {}: {}", channel.channel_id, target_conn_no, e);
                                    }
                                }
                            } else {
                                warn!("Channel({}): OpenConnection payload too short (min 4 bytes for target_conn_no required). Actual: {}", channel.channel_id, payload_cursor.remaining());
                            }
                        }
                        ControlMessage::CloseConnection => {
                            if payload_cursor.remaining() >= 4 { // Need at least 4 bytes for target_conn_no
                                let target_conn_no = payload_cursor.get_u32();
                                let reason = if payload_cursor.remaining() >= 2 {
                                    match CloseConnectionReason::try_from(payload_cursor.get_u16()) {
                                        Ok(r) => r,
                                        Err(_) => {
                                            warn!("Channel({}): Invalid CloseConnectionReason code in payload for conn_no {}. Defaulting to Normal.", channel.channel_id, target_conn_no);
                                            CloseConnectionReason::Normal
                                        }
                                    }
                                } else {
                                    CloseConnectionReason::Normal
                                };
                                // Drop conns_map before calling channel.close_backend
                                // Explicitly drop for clarity, though scope ending would also work if structured differently
                                // let mut conns_map = channel.conns.lock().await; // This was the original problematic line
                                // drop(conns_map); // Ensure lock is released

                                info!("Channel({}): Received CloseConnection control message for target_conn_no {} with reason {:?}", channel.channel_id, target_conn_no, reason);
                                if let Err(e) = channel.close_backend(target_conn_no, reason).await {
                                    error!("Channel({}): Error closing backend connection {}: {}", channel.channel_id, target_conn_no, e);
                                }
                            } else {
                                warn!("Channel({}): CloseConnection payload too short to contain target_conn_no.", channel.channel_id);
                            }
                        }
                        ControlMessage::SendEOF => {
                            if payload_cursor.remaining() >= 4 { // Need at least 4 bytes for target_conn_no
                                let target_conn_no = payload_cursor.get_u32();
                                info!("Channel({}): Received SendEOF control message for target_conn_no {}", channel.channel_id, target_conn_no);
                                
                                // Lock needed here to access specific connection
                                let mut conns_map_guard = channel.conns.lock().await;
                                if let Some(conn_to_eof) = conns_map_guard.get_mut(&target_conn_no) {
                                    if let Err(e) = crate::models::AsyncReadWrite::shutdown(&mut conn_to_eof.backend).await {
                                        error!("Channel({}): Error sending EOF to backend for conn_no {}: {}", channel.channel_id, target_conn_no, e);
                                    } else {
                                        debug!("Channel({}): Successfully sent EOF/shutdown to backend for conn_no {}", channel.channel_id, target_conn_no);
                                    }
                                } else {
                                    warn!("Channel({}): SendEOF for unknown connection {}", channel.channel_id, target_conn_no);
                                }
                                // conns_map_guard is dropped here
                            } else {
                                warn!("Channel({}): SendEOF payload too short to contain target_conn_no.", channel.channel_id);
                            }
                        }
                        ControlMessage::ConnectionOpened => {
                            if payload_cursor.remaining() >= 4 { // conn_no
                                let target_conn_no_opened = payload_cursor.get_u32();
                                info!("Channel({}): Received ConnectionOpened for conn_no {}", channel.channel_id, target_conn_no_opened);
                                
                                {
                                    let mut conns_map = channel.conns.lock().await;
                                    if let Some(conn) = conns_map.get_mut(&target_conn_no_opened) {
                                        conn.stats.remote_connected = true; // Mark as fully connected
                                        debug!("Channel({}): Marked conn_no {} stats.remote_connected = true.", channel.channel_id, target_conn_no_opened);
                                    } else {
                                        warn!("Channel({}): Received ConnectionOpened for unknown conn_no {}", channel.channel_id, target_conn_no_opened);
                                        // If conn not found, no point trying to send pending messages for it.
                                        // Release buffer and return from this control message case.
                                        channel.buffer_pool.release(temp_payload_buffer);
                                        return Ok(()); 
                                    }
                                } // conns_map lock is dropped here
                                
                                // Now that the lock on conns_map is released, we can call send_pending_messages.
                                debug!("Channel({}): Triggering send_pending_messages for newly opened conn_no {}.", channel.channel_id, target_conn_no_opened);
                                if let Err(e) = send_pending_messages(channel, target_conn_no_opened).await {
                                    error!("Channel({}): Error in send_pending_messages for conn_no {}: {}", channel.channel_id, target_conn_no_opened, e);
                                }
                            } else {
                                warn!("Channel({}): ConnectionOpened payload too short for conn_no.", channel.channel_id);
                            }
                        }
                        ControlMessage::Ping => {
                            temp_payload_buffer.clear();
                            if payload_cursor.remaining() > 0 {
                                let position = payload_cursor.position() as usize;
                                temp_payload_buffer.extend_from_slice(&frame.payload[position..]);
                            }
                            // Drop conns_map before calling channel.send_control_message
                            // let mut conns_map = channel.conns.lock().await; // Original problematic line
                            // drop(conns_map); // Ensure lock is released

                            debug!("Channel({}): Received Ping, sending Pong. Pong payload len: {}", channel.channel_id, temp_payload_buffer.len());
                            if let Err(e) = channel.send_control_message(ControlMessage::Pong, &temp_payload_buffer).await {
                                error!("Channel({}): Error sending Pong: {}", channel.channel_id, e);
                            }
                        }
                        ControlMessage::Pong => {
                            if payload_cursor.remaining() >= 4 { // conn_no
                                let conn_no_pong = payload_cursor.get_u32();
                                
                                if conn_no_pong == 0 {
                                    // Handle channel-level Pong (conn_no = 0)
                                    let mut channel_ping_time = channel.channel_ping_sent_time.lock().await;
                                    if let Some(ping_sent_time_ms) = channel_ping_time.take() { // .take() resets to None
                                        let current_time_ms = crate::tube_protocol::now_ms();
                                        if current_time_ms >= ping_sent_time_ms {
                                            let rtt = current_time_ms - ping_sent_time_ms;
                                            let mut rtt_vec = channel.round_trip_latency.lock().await;
                                            rtt_vec.push(rtt);
                                            if rtt_vec.len() > 100 { // Keep a sliding window
                                                rtt_vec.remove(0);
                                            }
                                            debug!("Channel({}): Channel PONG received. RTT: {}ms. Channel ping time reset.", channel.channel_id, rtt);
                                        } else {
                                            warn!("Channel({}): Channel PONG received with current_time < ping_sent_time. Clock skew?", channel.channel_id);
                                        }
                                    } else {
                                        debug!("Channel({}): Channel PONG received, but no channel_ping_sent_time was set. Ignoring RTT.", channel.channel_id);
                                    }
                                } else {
                                    // Handle data connection Pong (conn_no > 0)
                                    let mut locked_conns = channel.conns.lock().await;
                                    if let Some(conn_stats) = locked_conns.get_mut(&conn_no_pong).map(|c| &mut c.stats) {
                                        conn_stats.message_counter = 0; // Reset message counter
                                        if let Some(ping_sent_time_ms) = conn_stats.ping_time.take() { // .take() resets it to None
                                            let current_time_ms = crate::tube_protocol::now_ms();
                                            if current_time_ms >= ping_sent_time_ms {
                                                let rtt = current_time_ms - ping_sent_time_ms;
                                                // Store individual connection RTT if needed, or just log
                                                // For now, primary RTT tracking is via channel.round_trip_latency for overall health
                                                debug!("Channel({}): Pong for conn_no {} received. RTT: {}ms. Ping time reset.", channel.channel_id, conn_no_pong, rtt);

                                                // Simplified transfer latency update
                                                if conn_stats.receive_latency_count > 0 {
                                                    let receive_avg = conn_stats.receive_latency_avg();
                                                    if rtt >= receive_avg {
                                                        conn_stats.transfer_latency_sum += rtt - receive_avg;
                                                        conn_stats.transfer_latency_count += 1;
                                                    }
                                                }
                                            } else {
                                                warn!("Channel({}): Pong for conn_no {} received with current_time < ping_sent_time. Clock skew?", channel.channel_id, conn_no_pong);
                                            }
                                        } else {
                                            debug!("Channel({}): Pong for conn_no {} received, but no ping_time was set. Ignoring RTT.", channel.channel_id, conn_no_pong);
                                        }
                                    } else {
                                        warn!("Channel({}): Pong for unknown data connection_no {}", channel.channel_id, conn_no_pong);
                                    }
                                }
                            } else {
                                warn!("Channel({}): Pong payload too short to contain conn_no.", channel.channel_id);
                            }
                        }
                    }
                }
                Err(_) => {
                    warn!("Channel({}): Unknown control message code: {} (frame.conn_no=0)", channel.channel_id, control_code);
                }
            }
        } else {
            warn!("Channel({}): Control message payload too short ({} bytes) to contain code (frame.conn_no=0)", channel.channel_id, frame.payload.len());
        }
        channel.buffer_pool.release(temp_payload_buffer); // Release buffer used for pong or other ops
    } else { // Data message for connection_no > 0
        let mut conns_map = channel.conns.lock().await;
        if let Some(conn) = conns_map.get_mut(&conn_no) {
            if frame.payload.is_empty() {
                debug!("Channel({}): Received empty data payload for conn_no {}", channel.channel_id, conn_no);
            } else {
                debug!("Channel({}): Forwarding data for conn_no {}. Payload size: {}", channel.channel_id, conn_no, frame.payload.len());
                // TODO: Consider if there are scenarios where this write should be non-blocking or handled in a separate task. A potential future performance tuning if needed
                // For now, direct await is simplest.
                if let Err(e) = conn.backend.write_all(&frame.payload).await {
                    error!("Channel({}): Failed to write to backend for conn_no {}: {}", channel.channel_id, conn_no, e);
                    // Optionally, close connection or send error back via WebRTC
                    // For now, just logging. This error will propagate up.
                    return Err(e.into());
                }
                // Flush might be important depending on the backend's expectations
                if let Err(e) = conn.backend.flush().await {
                     error!("Channel({}): Failed to flush backend writer for conn_no {}: {}", channel.channel_id, conn_no, e);
                     return Err(e.into());
                }
            }
        } else {
            warn!("Channel({}): Message for unknown connection {} in process_inbound_message", channel.channel_id, conn_no);
        }
    }
    Ok(())
}


// Send any pending messages for a given connection
pub async fn send_pending_messages(channel: &mut Channel, conn_no: u32) -> Result<()> {
    // **BOLD WARNING: HOT PATH - SENDING QUEUED MESSAGES**
    // **NO STRING/OBJECT ALLOCATIONS IN THE LOOP**
    // **MESSAGES ARE ALREADY ENCODED AS Bytes**
    
    debug!("Channel({}): Attempting to send pending messages for conn_no {}", channel.channel_id, conn_no);
    let mut pending_guard = channel.pending_messages.lock().await;
    
    if let Some(queue) = pending_guard.get_mut(&conn_no) {
        if queue.is_empty() {
            debug!("Channel({}): No pending messages for conn_no {}.", channel.channel_id, conn_no);
            return Ok(());
        }
        
        let mut messages_sent_count = 0;
        // Check the buffer amount before starting, and periodically inside the loop if sending many messages.
        // BUFFER_LOW_THRESHOLD is defined in core.rs, consider making it accessible or passing it.
        // For now, using a local constant or assuming Channel.webrtc has the threshold knowledge.
        // The WebRTCDataChannel already has set_buffered_amount_low_threshold and on_buffered_amount_low callback.
        // The check here is to proactively avoid over-sending if we are about to send a batch.
        
        // Strategy: Peek at message, clone for sending, pop only on successful send.
        // This ensures messages aren't lost on transient send errors.
        // Cloning Bytes is cheap (increments a ref count) but peeking first is crucial for error handling.
        while let Some(message_bytes_ref) = queue.front() { // Peek at the front message
            if message_bytes_ref.is_empty() {
                queue.pop_front(); // Remove empty message
                continue; // Skip empty messages
            }

            // Clone here is necessary because we need to release the borrow on queue
            // before .await, and we might put it back.
            let message_bytes_cloned = message_bytes_ref.clone();

            let current_buffered_amount = channel.webrtc.buffered_amount().await;
            if current_buffered_amount >= super::core::BUFFER_LOW_THRESHOLD { 
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(
                        "Channel({}): Buffer amount {} for conn_no {} is high. Pausing sending of pending messages.",
                        channel.channel_id, current_buffered_amount, conn_no
                    );
                }
                // Message is still at the front, no need to push_front here.
                break; // Stop sending if buffer is too full
            }

            match channel.webrtc.send(message_bytes_cloned.clone()).await { // Send the cloned message
                Ok(_) => {
                    messages_sent_count += 1;
                    queue.pop_front(); // Successfully sent, now remove from queue
                }
                Err(e) => {
                    error!(
                        "Channel({}): Failed to send pending message for conn_no {}: {}. Message remains at the front of the queue.",
                        channel.channel_id, conn_no, e
                    );
                    // Message is still at the front of queue because we only peeked.
                    // Stop processing this queue on error to avoid repeated attempts on a potentially problematic message or state.
                    break; 
                }
            }
            // Optional: Add a small yield or limit the number of messages sent in one go if this loop becomes too tight.
        }
        if messages_sent_count > 0 {
            debug!("Channel({}): Sent {} pending messages for conn_no {}. Remaining in queue: {}", 
                channel.channel_id, messages_sent_count, conn_no, queue.len());
        }
    } else {
        debug!("Channel({}): No pending message queue found for conn_no {}.", channel.channel_id, conn_no);
    }
    
    Ok(())
}
