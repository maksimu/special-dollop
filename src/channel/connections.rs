// Connection management functionality for Channel

use crate::buffer_pool::BufferPool;
use crate::channel::guacd_parser::{GuacdInstruction, GuacdParser, PeekError};
use crate::channel::types::ActiveProtocol;
use crate::models::{Conn, StreamHalf};
use crate::trace_hot_path;
use crate::tube_protocol::{ControlMessage, Frame};
use anyhow::Result;
use bytes::{Buf, BufMut, BytesMut};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{debug, error, info, trace, warn}; // Import centralized hot path macros

use super::core::Channel;

// Open a backend connection to a given address
pub async fn open_backend(
    channel: &mut Channel,
    conn_no: u32,
    addr: SocketAddr,
    active_protocol: ActiveProtocol,
) -> Result<()> {
    debug!(
        "Endpoint {}: Opening connection {} to {} for protocol {:?} (Channel ServerMode: {})",
        channel.channel_id, conn_no, addr, active_protocol, channel.server_mode
    );

    // Check if the connection already exists
    if channel.conns.contains_key(&conn_no) {
        warn!(
            "Endpoint {}: Connection {} already exists",
            channel.channel_id, conn_no
        );
        return Ok(());
    }

    // If this is a Guacd connection, try to set it as the primary one if not already set.
    if active_protocol == ActiveProtocol::Guacd {
        let mut primary_conn_no_guard = channel.primary_guacd_conn_no.lock().await;
        if primary_conn_no_guard.is_none() {
            *primary_conn_no_guard = Some(conn_no);
            info!(target: "connection_lifecycle", channel_id = %channel.channel_id, conn_no, "Marked as primary Guacd data connection.");
        } else if *primary_conn_no_guard != Some(conn_no) {
            // This case would be unusual - opening a new Guacd connection when one (potentially different conn_no) is already primary.
            // For now, log it. Depending on design, there might be an error or a secondary stream.
            debug!(target: "connection_lifecycle", channel_id = %channel.channel_id, conn_no, existing_primary = ?*primary_conn_no_guard, "Opening additional Guacd connection; primary already set.");
        }
    }

    // Connect to the backend
    let stream = TcpStream::connect(addr).await?;
    trace_hot_path!(
        channel_id = %channel.channel_id,
        conn_no = conn_no,
        backend_addr = %addr,
        active_protocol = ?active_protocol,
        server_mode = channel.server_mode,
        "PRE-CALL to setup_outbound_task"
    );
    setup_outbound_task(channel, conn_no, stream, active_protocol).await?;

    Ok(())
}

// Set up a task to read from the backend and send to WebRTC
pub async fn setup_outbound_task(
    channel: &mut Channel,
    conn_no: u32,
    stream: TcpStream,
    active_protocol: ActiveProtocol,
) -> Result<()> {
    let (mut backend_reader, mut backend_writer) = stream.into_split();

    let dc = channel.webrtc.clone();
    let channel_id_for_task = channel.channel_id.clone();
    let conn_closed_tx_for_task = channel.conn_closed_tx.clone(); // Clone the sender for the task
    let pending_messages = channel.pending_messages.clone();
    let buffer_pool = channel.buffer_pool.clone();
    let buffer_low_threshold = super::core::BUFFER_LOW_THRESHOLD;
    let is_channel_server_mode = channel.server_mode;

    trace_hot_path!(
        channel_id = %channel_id_for_task,
        conn_no = conn_no,
        active_protocol = ?active_protocol,
        server_mode = is_channel_server_mode,
        "ENTERING setup_outbound_task function"
    );

    if active_protocol == ActiveProtocol::Guacd {
        debug!(
            "Channel({}): Performing Guacd handshake for conn_no {}",
            channel_id_for_task, conn_no
        );

        let channel_id_clone = channel_id_for_task.clone(); // Already have channel_id_for_task
        let guacd_params_clone = channel.guacd_params.clone();
        let buffer_pool_clone = buffer_pool.clone(); // Use the already cloned buffer_pool
        let handshake_timeout_duration = channel.timeouts.guacd_handshake;

        match timeout(
            handshake_timeout_duration,
            perform_guacd_handshake(
                &mut backend_reader,
                &mut backend_writer,
                &channel_id_clone,
                conn_no,
                guacd_params_clone,
                buffer_pool_clone,
            ),
        )
        .await
        {
            Ok(Ok(_)) => {
                debug!(
                    "Channel({}): Guacd handshake successful for conn_no {}",
                    channel_id_clone, conn_no
                );
            }
            Ok(Err(e)) => {
                error!(
                    "Channel({}): Guacd handshake failed for conn_no {}: {}",
                    channel_id_clone, conn_no, e
                );
                // Reuse a single buffer for both operations to avoid acquire/release cycles
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                let _ = dc.send(encoded_frame).await;
                return Err(e);
            }
            Err(_) => {
                error!(
                    "Channel({}): Guacd handshake timed out for conn_no {}",
                    channel_id_clone, conn_no
                );
                // Reuse a single buffer for both operations to avoid acquire/release cycles
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                let _ = dc.send(encoded_frame).await;
                return Err(anyhow::anyhow!("Guacd handshake timed out"));
            }
        }
    }

    let channel_id_for_log_after_spawn = channel.channel_id.clone();

    trace_hot_path!(
        channel_id = %channel.channel_id,
        conn_no = conn_no,
        active_protocol = ?active_protocol,
        server_mode = is_channel_server_mode,
        "PRE-SPAWN (outer scope) in setup_outbound_task"
    );

    let outbound_handle = tokio::spawn(async move {
        // This is the very first log inside the spawned task
        trace_hot_path!(
            channel_id = %channel_id_for_task,
            conn_no = conn_no,
            active_protocol = ?active_protocol,
            server_mode = is_channel_server_mode,
            "setup_outbound_task TASK SPAWNED"
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
            pending_messages_local: &Arc<
                Mutex<HashMap<u32, std::collections::VecDeque<bytes::Bytes>>>,
            >,
            buffer_low_threshold_local: u64,
            channel_id_local: &str,
            active_protocol_local: ActiveProtocol,
            context_msg: &str,
        ) -> Result<(), ()> {
            let buffered_amount = data_channel.buffered_amount().await;
            if buffered_amount >= buffer_low_threshold_local {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!(
                        channel_id = %channel_id_local,
                        buffered_amount = buffered_amount,
                        conn_no = conn_no_local,
                        active_protocol = ?active_protocol_local,
                        message_size = frame_to_send.len(),
                        context = context_msg,
                        "Buffer high, queueing message"
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
                        channel_id = %channel_id_local,
                        buffered_amount = buffered_amount,
                        conn_no = conn_no_local,
                        active_protocol = ?active_protocol_local,
                        message_size = frame_to_send.len(),
                        context = context_msg,
                        "Buffer low, sending directly"
                    );
                }
                if let Err(e) = data_channel.send(frame_to_send).await {
                    error!(
                        channel_id = %channel_id_local,
                        conn_no = conn_no_local,
                        context = context_msg,
                        error = %e,
                        "Failed to send message"
                    );
                    Err(())
                } else {
                    Ok(())
                }
            }
        }

        // Original task logic starts here
        trace_hot_path!(
            channel_id = %channel_id_for_task,
            conn_no = conn_no,
            "setup_outbound_task ORIGINAL LOGIC START"
        );

        let mut reader = backend_reader;
        let mut eof_sent = false;
        let mut loop_iterations = 0;

        let mut main_read_buffer = buffer_pool.acquire();
        let mut encode_buffer = buffer_pool.acquire();
        const MAX_READ_SIZE: usize = 8 * 1024;
        const GUACD_BATCH_SIZE: usize = 16 * 1024; // Batch up to 16KB of Guacd instructions
        const LARGE_INSTRUCTION_THRESHOLD: usize = MAX_READ_SIZE; // If a single instruction is this big, send it directly

        // **BOLD WARNING: HOT PATH - NO STRING/OBJECT ALLOCATIONS ALLOWED IN THE MAIN LOOP**
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

        trace_hot_path!(
            channel_id = %channel_id_for_task,
            conn_no = conn_no,
            "setup_outbound_task BEFORE main loop"
        );

        // **BOLD WARNING: ENTERING HOT PATH - BACKEND→WEBRTC MAIN LOOP**
        // **NO STRING ALLOCATIONS, NO UNNECESSARY OBJECT CREATION**
        // **USE BORROWED DATA, BUFFER POOLS, AND ZERO-COPY TECHNIQUES**
        loop {
            loop_iterations += 1;
            trace_hot_path!(
                channel_id = %channel_id_for_task,
                conn_no = conn_no,
                loop_iteration = loop_iterations,
                buffer_len = main_read_buffer.len(),
                "setup_outbound_task loop iteration"
            );

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

            trace_hot_path!(
                channel_id = %channel_id_for_task,
                conn_no = conn_no,
                active_protocol = ?active_protocol,
                "setup_outbound_task PRE-READ from backend"
            );

            // **ZERO-COPY READ: Use buffer pool buffer directly**
            // For Guacd, read directly into main_read_buffer to append.
            // For others, use temp_read_buffer for a single pass.
            let n_read = if active_protocol == ActiveProtocol::Guacd {
                // Ensure main_read_buffer has enough capacity *before* the read_buf call
                // This is slightly different from its previous position but more direct for this path.
                if main_read_buffer.capacity() - main_read_buffer.len() < MAX_READ_SIZE {
                    main_read_buffer.reserve(MAX_READ_SIZE);
                }
                match reader.read_buf(&mut main_read_buffer).await {
                    // Read then append to main_read_buffer
                    Ok(n) => n,
                    Err(e) => {
                        error!(
                            "Endpoint {}: Read error on connection {} (Guacd path): {}",
                            channel_id_for_task, conn_no, e
                        );
                        break;
                    }
                }
            } else {
                match reader.read_buf(&mut temp_read_buffer).await {
                    // read_buf clears and fills temp_read_buffer
                    Ok(n) => n,
                    Err(e) => {
                        error!(
                            "Endpoint {}: Read error on connection {} (Non-Guacd path): {}",
                            channel_id_for_task, conn_no, e
                        );
                        break;
                    }
                }
            };

            match n_read {
                0 => {
                    trace_hot_path!(
                        channel_id = %channel_id_for_task,
                        conn_no = conn_no,
                        eof_sent = eof_sent,
                        "setup_outbound_task POST-READ 0 bytes (EOF)"
                    );

                    if !eof_sent {
                        let eof_frame = Frame::new_control_with_pool(
                            ControlMessage::SendEOF,
                            &conn_no.to_be_bytes(),
                            &buffer_pool,
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
                    trace_hot_path!(
                        channel_id = %channel_id_for_task,
                        conn_no = conn_no,
                        bytes_read = n_read,
                        eof_sent = eof_sent,
                        "setup_outbound_task POST-READ bytes from backend"
                    );

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
                            let current_slice =
                                &main_read_buffer[consumed_offset..main_read_buffer.len()];

                            #[cfg(feature = "profiling")]
                            let parse_start = std::time::Instant::now();

                            // **ULTRA-FAST PATH: Only validate a format and check for error**
                            match GuacdParser::validate_and_check_error(current_slice) {
                                Ok((instruction_len, is_error)) => {
                                    #[cfg(feature = "profiling")]
                                    {
                                        let parse_duration = parse_start.elapsed();
                                        if parse_duration.as_micros() > 100 {
                                            warn!(
                                                "Channel({}): Slow Guacd validate: {}μs",
                                                channel_id_for_task,
                                                parse_duration.as_micros()
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
                                        temp_buf_for_control
                                            .extend_from_slice(&close_payload_bytes);
                                        let close_frame = Frame::new_control_with_buffer(
                                            ControlMessage::CloseConnection,
                                            &mut temp_buf_for_control,
                                        );
                                        buffer_pool.release(temp_buf_for_control);
                                        if let Err(send_err) = dc
                                            .send(close_frame.encode_with_pool(&buffer_pool))
                                            .await
                                        {
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
                                                let bytes_written =
                                                    Frame::encode_data_frame_from_slice(
                                                        &mut encode_buffer,
                                                        conn_no,
                                                        &batch_buffer[..],
                                                    );
                                                let batch_frame_bytes =
                                                    encode_buffer.split_to(bytes_written).freeze();
                                                if try_send_or_queue_frame(
                                                    batch_frame_bytes,
                                                    conn_no,
                                                    &dc_clone_for_helper,
                                                    &pending_messages_clone_for_helper,
                                                    buffer_low_threshold,
                                                    &channel_id_for_task,
                                                    active_protocol,
                                                    "(pre-large) batch",
                                                )
                                                .await
                                                .is_err()
                                                {
                                                    close_conn_and_break = true;
                                                }
                                                batch_buffer.clear();
                                                if close_conn_and_break {
                                                    break;
                                                }
                                            }

                                            // Now send the large instruction directly
                                            encode_buffer.clear();
                                            let bytes_written = Frame::encode_data_frame_from_slice(
                                                &mut encode_buffer,
                                                conn_no,
                                                instruction_data,
                                            );
                                            let large_frame_bytes =
                                                encode_buffer.split_to(bytes_written).freeze();
                                            if try_send_or_queue_frame(
                                                large_frame_bytes,
                                                conn_no,
                                                &dc_clone_for_helper,
                                                &pending_messages_clone_for_helper,
                                                buffer_low_threshold,
                                                &channel_id_for_task,
                                                active_protocol,
                                                "large instruction",
                                            )
                                            .await
                                            .is_err()
                                            {
                                                close_conn_and_break = true;
                                            }
                                            // No need to add to batch_buffer if sent directly
                                        } else {
                                            // Instruction is not large, proceed with normal batching
                                            if batch_buffer.len() + instruction_data.len()
                                                > GUACD_BATCH_SIZE
                                                && !batch_buffer.is_empty()
                                            {
                                                encode_buffer.clear();
                                                let bytes_written =
                                                    Frame::encode_data_frame_from_slice(
                                                        &mut encode_buffer,
                                                        conn_no,
                                                        &batch_buffer[..],
                                                    );
                                                let batch_frame_bytes =
                                                    encode_buffer.split_to(bytes_written).freeze();
                                                if try_send_or_queue_frame(
                                                    batch_frame_bytes,
                                                    conn_no,
                                                    &dc_clone_for_helper,
                                                    &pending_messages_clone_for_helper,
                                                    buffer_low_threshold,
                                                    &channel_id_for_task,
                                                    active_protocol,
                                                    "batch",
                                                )
                                                .await
                                                .is_err()
                                                {
                                                    close_conn_and_break = true;
                                                }
                                                batch_buffer.clear();
                                                if close_conn_and_break {
                                                    break;
                                                }
                                            }
                                            batch_buffer.extend_from_slice(instruction_data);
                                        }
                                        if close_conn_and_break {
                                            break;
                                        }
                                    }
                                    consumed_offset += instruction_len;
                                }
                                Err(PeekError::Incomplete) => {
                                    break;
                                }
                                Err(e) => {
                                    // Other PeekErrors
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
                                        &mut temp_buf_for_control,
                                    );
                                    buffer_pool.release(temp_buf_for_control);
                                    if let Err(send_err) =
                                        dc.send(close_frame.encode_with_pool(&buffer_pool)).await
                                    {
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
                                let final_batch_frame_bytes =
                                    encode_buffer.split_to(bytes_written).freeze();
                                if try_send_or_queue_frame(
                                    final_batch_frame_bytes,
                                    conn_no,
                                    &dc_clone_for_helper,
                                    &pending_messages_clone_for_helper,
                                    buffer_low_threshold,
                                    &channel_id_for_task,
                                    active_protocol,
                                    "final batch",
                                )
                                .await
                                .is_err()
                                {
                                    close_conn_and_break = true; // This will be checked after the Guacd block
                                }
                                batch_buffer.clear();
                            }
                        }

                        if close_conn_and_break {
                            // If Guacd processing decided to close
                            main_read_buffer.clear();
                        } else if consumed_offset > 0 {
                            main_read_buffer.advance(consumed_offset);
                        }
                    } else {
                        // Not Guacd protocol (e.g., PortForward, SOCKS5)
                        // **BOLD WARNING: ZERO-COPY HOT PATH FOR PORT FORWARDING**
                        // **ENCODE DIRECTLY FROM READ BUFFER - NO COPIES**
                        // **SEND DIRECTLY - NO INTERMEDIATE VECTOR**
                        encode_buffer.clear();

                        trace_hot_path!(
                            channel_id = %channel_id_for_task,
                            conn_no = conn_no,
                            bytes_read = n_read,
                            "PortForward/SOCKS5 zero-copy encode"
                        );

                        // Encode directly from temp_read_buffer (which was filled by read_buf)
                        let bytes_written = Frame::encode_data_frame_from_slice(
                            &mut encode_buffer,
                            conn_no,
                            &temp_read_buffer[..],
                        );

                        let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();

                        trace_hot_path!(
                            channel_id = %channel_id_for_task,
                            conn_no = conn_no,
                            bytes_written = bytes_written,
                            "PortForward/SOCKS5 POST-ENCODE"
                        );

                        // **PERFORMANCE: Send directly without intermediate storage**
                        let buffered_amount = dc.buffered_amount().await;
                        if buffered_amount >= buffer_low_threshold {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!(
                                    channel_id = %channel_id_for_task,
                                    buffered_amount = buffered_amount,
                                    conn_no = conn_no,
                                    active_protocol = ?active_protocol,
                                    message_size = encoded_frame_bytes.len(),
                                    "Buffer high, queueing message"
                                );
                            }
                            let mut pending_guard = pending_messages.lock().await;
                            if let Some(queue) = pending_guard.get_mut(&conn_no) {
                                queue.push_back(encoded_frame_bytes);
                            }
                        } else {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!(
                                    channel_id = %channel_id_for_task,
                                    buffered_amount = buffered_amount,
                                    conn_no = conn_no,
                                    active_protocol = ?active_protocol,
                                    message_size = encoded_frame_bytes.len(),
                                    "Buffer low, sending directly"
                                );
                            }
                            if let Err(e) = dc.send(encoded_frame_bytes).await {
                                error!(
                                    channel_id = %channel_id_for_task,
                                    conn_no = conn_no,
                                    error = %e,
                                    "Failed to send data for connection"
                                );
                                close_conn_and_break = true;
                            }
                        }
                    }

                    if close_conn_and_break {
                        break;
                    }
                }
            }
        }
        debug!(
            "Endpoint {}: Backend→WebRTC task for connection {} exited",
            channel_id_for_task, conn_no
        );
        buffer_pool.release(main_read_buffer);
        buffer_pool.release(encode_buffer);
        buffer_pool.release(temp_read_buffer);

        // Release the batch buffer if it was used
        if let Some(batch_buffer) = guacd_batch_buffer {
            buffer_pool.release(batch_buffer);
        }

        let mut pending_guard = pending_messages.lock().await;
        pending_guard.remove(&conn_no);

        // Signal that this connection task has exited
        if let Err(e) = conn_closed_tx_for_task.send((conn_no, channel_id_for_task.clone())) {
            // If sending fails, it likely means the Channel's run loop (receiver) has already terminated.
            // This is not necessarily an error in this context, so a warning or trace might be appropriate.
            warn!(target: "connection_lifecycle", channel_id = %channel_id_for_task, conn_no, error = %e, "Failed to send connection closure signal; channel might be shutting down.");
        } else {
            debug!(target: "connection_lifecycle", channel_id = %channel_id_for_task, conn_no, "Sent connection closure signal to Channel run loop.");
        }
    });

    trace_hot_path!(
        channel_id = %channel_id_for_log_after_spawn,
        conn_no = conn_no,
        "setup_outbound_task tokio::spawn COMPLETE (outer scope). Outbound handle created"
    );

    let stream_half = StreamHalf {
        reader: None,
        writer: backend_writer,
    };

    // Create lock-free connection with a dedicated backend task
    let conn = Conn::new_with_backend(
        Box::new(stream_half),
        outbound_handle,
        conn_no,
        channel.channel_id.clone(),
    )
    .await;

    channel.conns.insert(conn_no, conn);

    debug!(
        "Endpoint {}: Connection {} added to registry",
        channel.channel_id, conn_no
    );

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
        expected_opcode: &'a str,
    ) -> Result<GuacdInstruction>
    where
        SHelper: AsyncRead + Unpin + Send + ?Sized,
    {
        loop {
            // Process a peek result and extract what we need
            let process_result = {
                let peek_result =
                    GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len]);

                match peek_result {
                    Ok(peeked_instr) => {
                        let instruction_total_len = peeked_instr.total_length_in_buffer;
                        if instruction_total_len == 0 || instruction_total_len > *current_buffer_len
                        {
                            error!(
                                target: "guac_protocol", channel_id=%channel_id, conn_no,
                                "Invalid instruction length peeked ({}) vs buffer len ({}). Opcode: '{}'. Buffer (approx): {:?}",
                                instruction_total_len, *current_buffer_len, peeked_instr.opcode, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)]
                            );
                            return Err(anyhow::anyhow!(
                                "Peeked instruction length is invalid or exceeds buffer."
                            ));
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
                    return Err(anyhow::anyhow!(
                        "Guacd sent error '{}' ({:?}) during handshake (expected '{}')",
                        instruction.opcode,
                        instruction.args,
                        expected_opcode
                    ));
                }
                return if expected_opcode_check {
                    Ok(instruction)
                } else {
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, expected_opcode=%expected_opcode, received_opcode=%instruction.opcode, received_args=?instruction.args, "Unexpected Guacd opcode");
                    Err(anyhow::anyhow!(
                        "Expected Guacd opcode '{}', got '{}' with args {:?}",
                        expected_opcode,
                        instruction.opcode,
                        instruction.args
                    ))
                };
            }

            // Handle the incomplete case - read more data
            let mut temp_read_buf = [0u8; 1024];
            match reader.read(&mut temp_read_buf).await {
                Ok(0) => {
                    error!(target: "guac_protocol", channel_id=%channel_id, conn_no, expected_opcode=%expected_opcode, buffer_len = *current_buffer_len, "EOF during Guacd handshake");
                    return Err(anyhow::anyhow!("EOF during Guacd handshake while waiting for '{}' (incomplete data in buffer)", expected_opcode));
                }
                Ok(n_read) => {
                    if handshake_buffer.capacity() < *current_buffer_len + n_read {
                        handshake_buffer
                            .reserve(*current_buffer_len + n_read - handshake_buffer.capacity());
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

    let readonly_str_for_join =
        readonly_param_value_from_map.unwrap_or_else(|| "false".to_string());
    trace!(target: "guac_protocol_debug", channel_id=%channel_id, conn_no, readonly_str_for_join = %readonly_str_for_join, "Effective 'readonly_str_for_join' (after unwrap_or_else) for join attempt.");

    let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
    trace!(target: "guac_protocol_debug", channel_id=%channel_id, conn_no, is_readonly_bool = %is_readonly, "Final 'is_readonly' boolean for join attempt.");

    let width_for_new = guacd_params_locked
        .get("width")
        .cloned()
        .unwrap_or_else(|| "1024".to_string());
    let height_for_new = guacd_params_locked
        .get("height")
        .cloned()
        .unwrap_or_else(|| "768".to_string());
    let dpi_for_new = guacd_params_locked
        .get("dpi")
        .cloned()
        .unwrap_or_else(|| "96".to_string());
    let audio_mimetypes_str_for_new = guacd_params_locked
        .get("audio")
        .cloned()
        .unwrap_or_default();
    let video_mimetypes_str_for_new = guacd_params_locked
        .get("video")
        .cloned()
        .unwrap_or_default();
    let image_mimetypes_str_for_new = guacd_params_locked
        .get("image")
        .cloned()
        .unwrap_or_default();

    let connect_params_for_new_conn: HashMap<String, String> = if join_connection_id_opt.is_none() {
        guacd_params_locked.clone()
    } else {
        HashMap::new()
    };
    drop(guacd_params_locked);

    let select_instruction = GuacdInstruction::new("select".to_string(), vec![select_arg.clone()]);
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, instruction=?select_instruction, "Guacd Handshake: Sending 'select'");
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&select_instruction))
        .await?;
    writer.flush().await?;

    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Waiting for 'args'");
    let args_instruction = read_expected_instruction_stateless(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "args",
    )
    .await?;
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
            if idx == 0 {
                continue;
            }

            let is_readonly_arg_name_literal = "read-only";
            let is_current_arg_readonly_keyword =
                arg_name_from_guacd == is_readonly_arg_name_literal;

            trace!(target: "guac_protocol", channel_id=%channel_id, conn_no,
                   current_arg_name_from_guacd=%arg_name_from_guacd,
                   is_readonly_param_from_config=%is_readonly,
                   is_current_arg_the_readonly_keyword=is_current_arg_readonly_keyword,
                   "Looping for connect_args (join). Comparing '{}' with '{}'",
                   arg_name_from_guacd, is_readonly_arg_name_literal);

            if is_current_arg_readonly_keyword {
                let value_to_push = if is_readonly {
                    "true".to_string()
                } else {
                    "".to_string()
                };
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
            if mimetype_str.is_empty() {
                return Vec::new();
            }
            serde_json::from_str::<Vec<String>>(mimetype_str)
                .unwrap_or_else(|e| {
                    debug!(target:"guac_protocol", channel_id=%channel_id, conn_no, error=%e, "Failed to parse mimetype string '{}' as JSON array, splitting by comma as fallback.", mimetype_str);
                    mimetype_str.split(',').map(String::from).filter(|s| !s.is_empty()).collect()
                })
        };

        let size_parts: Vec<String> = width_for_new
            .split(',')
            .chain(height_for_new.split(','))
            .chain(dpi_for_new.split(','))
            .map(String::from)
            .collect();
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?size_parts, "Guacd Handshake (new): Sending 'size'");
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("size".to_string(), size_parts),
            ))
            .await?;
        writer.flush().await?;

        let audio_mimetypes = parse_mimetypes(&audio_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?audio_mimetypes, "Guacd Handshake (new): Sending 'audio'");
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("audio".to_string(), audio_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let video_mimetypes = parse_mimetypes(&video_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?video_mimetypes, "Guacd Handshake (new): Sending 'video'");
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("video".to_string(), video_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let image_mimetypes = parse_mimetypes(&image_mimetypes_str_for_new);
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?image_mimetypes, "Guacd Handshake (new): Sending 'image'");
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("image".to_string(), image_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        // Pre-normalize config keys once for efficient lookup
        let normalized_config_map: HashMap<String, String> = connect_params_for_new_conn
            .iter()
            .map(|(key, value)| {
                let normalized_key = key.replace(&['-', '_'][..], "").to_ascii_lowercase();
                (normalized_key, value.clone())
            })
            .collect();

        for arg_name_from_guacd in args_instruction.args.iter().skip(1) {
            // Normalize the guacd parameter name by removing hyphens/underscores and converting to lowercase
            let normalized_guacd_param = arg_name_from_guacd
                .replace(&['-', '_'][..], "")
                .to_ascii_lowercase();

            // Look up the parameter value using the normalized key
            let param_value = normalized_config_map
                .get(&normalized_guacd_param)
                .cloned()
                .unwrap_or_else(String::new);

            connect_args.push(param_value);
        }
    }

    let connect_instruction = GuacdInstruction::new("connect".to_string(), connect_args.clone());
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, instruction=?connect_instruction, "Guacd Handshake: Sending 'connect'");
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&connect_instruction))
        .await?;
    writer.flush().await?;

    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Waiting for 'ready'");
    let ready_instruction = read_expected_instruction_stateless(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "ready",
    )
    .await?;
    if let Some(client_id_from_ready) = ready_instruction.args.get(0) {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, guacd_client_id=%client_id_from_ready, "Guacd handshake completed.");
    } else {
        info!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd handshake completed. No client ID received with 'ready'.");
    }
    buffer_pool.release(handshake_buffer);
    Ok(())
}
