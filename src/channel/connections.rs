// Connection management functionality for Channel

use crate::buffer_pool::BufferPool;
use crate::channel::guacd_parser::{
    GuacdInstruction, GuacdParser, OpcodeAction, PeekError, SpecialOpcode,
};
use crate::channel::types::ActiveProtocol;
use crate::models::{Conn, StreamHalf};
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely; // Import branch prediction macros
use crate::webrtc_data_channel::{EventDrivenSender, STANDARD_BUFFER_THRESHOLD};
use crate::{trace_hot_path, trace_ultra_hot_path};
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
use tracing::{debug, error, info, trace, warn};

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
            debug!(target: "connection_lifecycle", channel_id = %channel.channel_id, conn_no, "Marked as primary Guacd data connection.");
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
    let buffer_pool = channel.buffer_pool.clone();
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
                reusable_control_buf
                    .extend_from_slice(&(CloseConnectionReason::GuacdError as u16).to_be_bytes());
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                // **OPTIMIZED**: Use event-driven sending for handshake error
                // NOTE: In handshake context, event_sender is not available, use dc directly
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
                reusable_control_buf
                    .extend_from_slice(&(CloseConnectionReason::GuacdError as u16).to_be_bytes());
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                // **OPTIMIZED**: Use event-driven sending for handshake timeout
                // NOTE: In handshake context, event_sender is not available, use dc directly
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

        // Create event-driven sender for zero-polling backpressure
        let event_sender = EventDrivenSender::new(Arc::new(dc.clone()), STANDARD_BUFFER_THRESHOLD);

        // **OPTIMIZED EVENT-DRIVEN HELPER** - Zero polling, instant backpressure
        #[inline(always)] // Hot path optimization
        async fn send_with_event_backpressure(
            frame_to_send: bytes::Bytes,
            conn_no_local: u32,
            event_sender: &EventDrivenSender,
            channel_id_local: &str,
            context_msg: &str,
        ) -> Result<(), ()> {
            // **FAST PATH**: Event-driven sending with native WebRTC backpressure
            match event_sender
                .send_with_natural_backpressure(frame_to_send)
                .await
            {
                Ok(_) => {
                    trace_hot_path!(
                        channel_id = %channel_id_local,
                        conn_no = conn_no_local,
                        context = context_msg,
                        queue_depth = event_sender.queue_depth(),
                        can_send_immediate = event_sender.can_send_immediate(),
                        is_over_threshold = event_sender.is_over_threshold(),
                        threshold = event_sender.get_threshold(),
                        "Event-driven send successful (0ms latency)"
                    );
                    Ok(())
                }
                Err(e) => {
                    // Only log if the error is not related to a closed connection
                    if !e.to_string().contains("DataChannel is not opened")
                        && !e.to_string().contains("Channel is closing")
                    {
                        error!(
                            channel_id = %channel_id_local,
                            conn_no = conn_no_local,
                            context = context_msg,
                            error = %e,
                            "Event-driven send failed"
                        );
                    }
                    Err(())
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
                        // **OPTIMIZED**: Use event-driven sending instead of polling
                        let _ = send_with_event_backpressure(
                            encoded,
                            conn_no,
                            &event_sender,
                            &channel_id_for_task,
                            "EOF frame",
                        )
                        .await;
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

                            // **ULTRA-FAST PATH: Validate format and detect special opcodes**
                            trace_ultra_hot_path!(target: "guac_opcode_debug",
                                   channel_id = %channel_id_for_task,
                                   conn_no = conn_no,
                                   slice_len = current_slice.len(),
                                   first_bytes = ?&current_slice[..std::cmp::min(50, current_slice.len())],
                                   "OUTBOUND: About to validate and detect special opcodes");
                            match GuacdParser::validate_and_detect_special(current_slice) {
                                Ok((instruction_len, action)) => {
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

                                    // Log opcode action for trace-level debugging
                                    trace_ultra_hot_path!(target: "guac_opcode_dispatch",
                                       channel_id = %channel_id_for_task,
                                       conn_no = conn_no,
                                       action = ?action,
                                       instruction_len = instruction_len,
                                       "Opcode action dispatched by expandable system");

                                    // Dispatch based on opcode action
                                    match action {
                                        OpcodeAction::CloseConnection => {
                                            warn!(target: "guac_error_handling",
                                                  channel_id = %channel_id_for_task,
                                                  conn_no = conn_no,
                                                  expected_opcode = %crate::channel::guacd_parser::ERROR_OPCODE,
                                                  "Error opcode detected - preparing to close connection");

                                            // **COLD PATH**: Error opcode detected
                                            match GuacdParser::peek_instruction(current_slice) {
                                                Ok(error_instr) => {
                                                    error!(target: "guac_error_handling",
                                                        channel_id = %channel_id_for_task,
                                                        conn_no = conn_no,
                                                        opcode = %error_instr.opcode,
                                                        expected_opcode = %crate::channel::guacd_parser::ERROR_OPCODE,
                                                        args = ?error_instr.args,
                                                        "Guacd sent error opcode - closing connection");
                                                }
                                                Err(_) => {
                                                    error!(target: "guac_error_handling",
                                                        channel_id = %channel_id_for_task,
                                                        conn_no = conn_no,
                                                        expected_opcode = %crate::channel::guacd_parser::ERROR_OPCODE,
                                                        "Guacd sent error opcode but failed to parse args - closing connection");
                                                }
                                            }

                                            // Forward the error instruction to the other side before closing
                                            // Extract the instruction data for forwarding
                                            let error_instruction_slice =
                                                &current_slice[..instruction_len];

                                            // Send the error instruction immediately
                                            let data_frame = Frame::new_data_with_pool(
                                                conn_no,
                                                error_instruction_slice,
                                                &buffer_pool,
                                            );
                                            let encoded_data =
                                                data_frame.encode_with_pool(&buffer_pool);

                                            if send_with_event_backpressure(
                                                encoded_data,
                                                conn_no,
                                                &event_sender,
                                                &channel_id_for_task,
                                                "Guacd error instruction forward",
                                            )
                                            .await
                                            .is_err()
                                            {
                                                error!(
                                                    "Channel({}): Conn {}: Failed to forward Guacd error instruction",
                                                    channel_id_for_task, conn_no
                                                );
                                            }

                                            // Now send CloseConnection with GuacdError reason
                                            let mut temp_buf_for_control = buffer_pool.acquire();
                                            temp_buf_for_control.clear();
                                            temp_buf_for_control
                                                .extend_from_slice(&conn_no.to_be_bytes());
                                            temp_buf_for_control.extend_from_slice(
                                                &(CloseConnectionReason::GuacdError as u16)
                                                    .to_be_bytes(),
                                            );

                                            let close_frame = Frame::new_control_with_buffer(
                                                ControlMessage::CloseConnection,
                                                &mut temp_buf_for_control,
                                            );
                                            buffer_pool.release(temp_buf_for_control);
                                            let encoded_close_frame =
                                                close_frame.encode_with_pool(&buffer_pool);
                                            if send_with_event_backpressure(
                                                encoded_close_frame,
                                                conn_no,
                                                &event_sender,
                                                &channel_id_for_task,
                                                "Guacd error close",
                                            )
                                            .await
                                            .is_err()
                                            {
                                                error!(
                                                    "Channel({}): Conn {}: Failed to send CloseConnection frame for Guacd error via event-driven system",
                                                    channel_id_for_task, conn_no
                                                );
                                            }
                                            close_conn_and_break = true;
                                            break;
                                        }
                                        OpcodeAction::ProcessSpecial(opcode) => {
                                            debug!(target: "guac_special_opcodes",
                                                  channel_id = %channel_id_for_task,
                                                  conn_no = conn_no,
                                                  opcode_name = %opcode.as_str(),
                                                  opcode = ?opcode,
                                                  "OUTBOUND: Special opcode detected - dispatching to handler");

                                            // Dispatch to appropriate special handler
                                            match opcode {
                                                SpecialOpcode::Size => {
                                                    // Parse the full instruction for details and send to Python
                                                    if let Ok(peeked_instr) =
                                                        GuacdParser::peek_instruction(current_slice)
                                                    {
                                                        if peeked_instr.args.len() >= 2 {
                                                            debug!(target: "guac_size_instruction_outbound",
                                                                  channel_id = %channel_id_for_task,
                                                                  conn_no = conn_no,
                                                                  layer = %peeked_instr.args[0],
                                                                  width = %peeked_instr.args.get(1).unwrap_or(&"unknown"),
                                                                  height = %peeked_instr.args.get(2).unwrap_or(&"unknown"),
                                                                  "OUTBOUND: Server size instruction (actual session size) - sending to signal system");

                                                            // Send it to the Python signal system
                                                            let channel_id_clone =
                                                                channel_id_for_task.clone();
                                                            let raw_instruction = GuacdParser::guacd_encode_instruction(&GuacdInstruction::new(
                                                                 peeked_instr.opcode.to_string(),
                                                                 peeked_instr.args.iter().map(|s| s.to_string()).collect()
                                                             ));
                                                            let raw_instruction_str =
                                                                std::str::from_utf8(
                                                                    &raw_instruction,
                                                                )
                                                                .unwrap_or("")
                                                                .to_string();

                                                            tokio::spawn(async move {
                                                                let registry =
                                                                    crate::tube_registry::REGISTRY
                                                                        .read()
                                                                        .await;

                                                                // Find which tube contains this channel
                                                                let mut found_tube_id = None;
                                                                for (tube_id, tube) in
                                                                    &registry.tubes_by_id
                                                                {
                                                                    let channels_guard = tube
                                                                        .active_channels
                                                                        .read()
                                                                        .await;
                                                                    if channels_guard.contains_key(
                                                                        &channel_id_clone,
                                                                    ) {
                                                                        found_tube_id =
                                                                            Some(tube_id.clone());
                                                                        debug!(target: "guac_size_instruction_outbound",
                                                                               channel_id = %channel_id_clone,
                                                                               tube_id = %tube_id,
                                                                               "OUTBOUND: Found tube containing this channel");
                                                                        break;
                                                                    }
                                                                }

                                                                if let Some(tube_id) = found_tube_id
                                                                {
                                                                    if let Some(signal_sender) =
                                                                        registry
                                                                            .signal_channels
                                                                            .get(&tube_id)
                                                                    {
                                                                        let signal_msg = crate::tube_registry::SignalMessage {
                                                                            tube_id: tube_id.clone(),
                                                                            kind: "guacd_instruction".to_string(),
                                                                            data: raw_instruction_str,
                                                                            conversation_id: channel_id_clone.clone(),
                                                                            progress_flag: Some(2), // PROGRESS - ongoing data transfer/instruction processing
                                                                            progress_status: Some("OK".to_string()), // Successful instruction forwarding
                                                                            is_ok: Some(true), // Successful instruction forwarding
                                                                        };

                                                                        if let Err(e) =
                                                                            signal_sender
                                                                                .send(signal_msg)
                                                                        {
                                                                            warn!(target: "guac_size_instruction_outbound",
                                                                                  tube_id = %tube_id,
                                                                                  channel_id = %channel_id_clone,
                                                                                  error = %e,
                                                                                  "OUTBOUND: Failed to send actual size signal to Python");
                                                                        } else {
                                                                            debug!(target: "guac_size_instruction_outbound",
                                                                                  tube_id = %tube_id,
                                                                                  channel_id = %channel_id_clone,
                                                                                  "OUTBOUND: Successfully sent actual size signal to Python");
                                                                        }
                                                                    } else {
                                                                        warn!(target: "guac_size_instruction_outbound",
                                                                              tube_id = %tube_id,
                                                                              "OUTBOUND: No signal sender found for tube");
                                                                    }
                                                                } else {
                                                                    warn!(target: "guac_size_instruction_outbound",
                                                                          "OUTBOUND: Could not find tube containing this channel");
                                                                }
                                                            });
                                                        } else {
                                                            debug!(target: "guac_special_opcodes",
                                                                  channel_id = %channel_id_for_task,
                                                                  opcode_name = %SpecialOpcode::Size.as_str(),
                                                                  "OUTBOUND: Size instruction with insufficient args - skipping signal");
                                                        }
                                                    } else {
                                                        debug!(target: "guac_special_opcodes",
                                                              channel_id = %channel_id_for_task,
                                                              opcode_name = %SpecialOpcode::Size.as_str(),
                                                              "OUTBOUND: Failed to parse size instruction - skipping signal");
                                                    }
                                                }
                                                SpecialOpcode::Error => {
                                                    // This should not happen as Error maps to CloseConnection
                                                    unreachable!("Error opcode should map to CloseConnection action");
                                                } // Add more handlers as needed:
                                                  // SpecialOpcode::Disconnect => {
                                                  //     info!(target: "guac_special_opcodes",
                                                  //           channel_id = %channel_id_for_task,
                                                  //           opcode_name = %SpecialOpcode::Disconnect.as_str(),
                                                  //           "Dispatching to disconnect handler");
                                                  //     handle_disconnect_instruction(...).await;
                                                  // }
                                            }
                                        }
                                        OpcodeAction::Normal => {
                                            // Normal instruction - continue to batching
                                        }
                                    }

                                    // Batch Guacd instructions for efficiency
                                    if let Some(ref mut batch_buffer) = guacd_batch_buffer {
                                        let instruction_data = &current_slice[..instruction_len];

                                        // **BRANCH PREDICTION**: Large instructions are uncommon (~5%)
                                        if unlikely!(
                                            instruction_data.len() >= LARGE_INSTRUCTION_THRESHOLD
                                        ) {
                                            // **COLD PATH**: If large, first flush any existing batch
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
                                                if send_with_event_backpressure(
                                                    batch_frame_bytes,
                                                    conn_no,
                                                    &event_sender,
                                                    &channel_id_for_task,
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
                                            if send_with_event_backpressure(
                                                large_frame_bytes,
                                                conn_no,
                                                &event_sender,
                                                &channel_id_for_task,
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
                                                if send_with_event_backpressure(
                                                    batch_frame_bytes,
                                                    conn_no,
                                                    &event_sender,
                                                    &channel_id_for_task,
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
                                    let mut temp_buf_for_control = buffer_pool.acquire();
                                    temp_buf_for_control.clear();
                                    temp_buf_for_control.extend_from_slice(&conn_no.to_be_bytes());
                                    temp_buf_for_control.extend_from_slice(
                                        &(CloseConnectionReason::ProtocolError as u16)
                                            .to_be_bytes(),
                                    );
                                    let close_frame = Frame::new_control_with_buffer(
                                        ControlMessage::CloseConnection,
                                        &mut temp_buf_for_control,
                                    );
                                    buffer_pool.release(temp_buf_for_control);
                                    // **OPTIMIZED**: Use event-driven sending for parsing error
                                    let encoded_close_frame =
                                        close_frame.encode_with_pool(&buffer_pool);
                                    if send_with_event_backpressure(
                                        encoded_close_frame,
                                        conn_no,
                                        &event_sender,
                                        &channel_id_for_task,
                                        "Guacd parsing error close",
                                    )
                                    .await
                                    .is_err()
                                    {
                                        error!(
                                            "Channel({}): Conn {}: Failed to send CloseConnection frame for Guacd parsing error via event-driven system",
                                            channel_id_for_task, conn_no
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
                                if send_with_event_backpressure(
                                    final_batch_frame_bytes,
                                    conn_no,
                                    &event_sender,
                                    &channel_id_for_task,
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

                        // **PERFORMANCE: Send with event-driven backpressure - zero polling!**
                        if send_with_event_backpressure(
                            encoded_frame_bytes,
                            conn_no,
                            &event_sender,
                            &channel_id_for_task,
                            "PortForward/SOCKS5 data",
                        )
                        .await
                        .is_err()
                        {
                            error!(
                                channel_id = %channel_id_for_task,
                                conn_no = conn_no,
                                "Failed to send PortForward/SOCKS5 data with event-driven backpressure - closing connection"
                            );
                            close_conn_and_break = true;
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

        // Signal that this connection task has exited
        if let Err(e) = conn_closed_tx_for_task.send((conn_no, channel_id_for_task.clone())) {
            // Only log if the error is not related to an expected channel closure
            if !e.to_string().contains("channel closed") {
                warn!(target: "connection_lifecycle",
                    channel_id = %channel_id_for_task,
                    conn_no,
                    error = %e,
                    "Failed to send connection closure signal; channel might be shutting down."
                );
            }
        } else {
            debug!(target: "connection_lifecycle",
                channel_id = %channel_id_for_task,
                conn_no,
                "Sent connection closure signal to Channel run loop."
            );
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
    let mut guacd_params_locked = guacd_params_arc.lock().await;

    // --- RDP username/domain splitting logic ---
    if let Some(protocol) = guacd_params_locked.get("protocol") {
        if protocol.eq_ignore_ascii_case("rdp") {
            if let Some(username) = guacd_params_locked.get("username").cloned() {
                if let Some(pos) = username.find('\\') {
                    let domain = &username[..pos];
                    let user = &username[pos + 1..];
                    info!(target: "guac_protocol",  channel_id=%channel_id, conn_no, domain=&domain, username=&user, "Domain found in username");
                    guacd_params_locked.insert("username".to_string(), user.to_string());
                    guacd_params_locked.insert("domain".to_string(), domain.to_string());
                }
            }
        }
    }

    let protocol_name_from_params = guacd_params_locked.get("protocol").cloned().unwrap_or_else(|| {
        warn!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd 'protocol' missing in guacd_params, defaulting to 'rdp' for select fallback.");
        "rdp".to_string()
    });

    let join_connection_id_key = "connectionid";
    let join_connection_id_opt = guacd_params_locked.get(join_connection_id_key).cloned();
    trace!(target: "guac_protocol", channel_id=%channel_id, conn_no, ?join_connection_id_opt, key_looked_up=%join_connection_id_key, "Checked for join connection ID in guacd_params");

    let select_arg: String;
    if let Some(id_to_join) = &join_connection_id_opt {
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, session_to_join=%id_to_join, "Guacd Handshake: Preparing to join existing session.");
        select_arg = id_to_join.clone();
    } else {
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, protocol=%protocol_name_from_params, "Guacd Handshake: Preparing for new session with protocol.");
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
    let connect_version_arg = args_instruction.args.first().cloned().unwrap_or_else(|| {
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
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Preparing 'connect' for NEW session.");

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

        // **HANDSHAKE SIZE INSTRUCTION DETECTION**: Log for debugging (no Python signal)
        let size_instruction = GuacdInstruction::new("size".to_string(), size_parts.clone());
        if size_parts.len() >= 2 {
            debug!(target: "guac_size_instruction_handshake",
                  channel_id = %channel_id,
                  conn_no = conn_no,
                  layer = "0", // Handshake size is always for default layer
                  width = %size_parts.first().map(|s| s.as_str()).unwrap_or("1024"),
                  height = %size_parts.get(1).map(|s| s.as_str()).unwrap_or("768"),
                  dpi = %size_parts.get(2).map(|s| s.as_str()).unwrap_or("96"),
                  "HANDSHAKE: Client initial size instruction (debug only - not sent to signal system)");
        }

        writer
            .write_all(&GuacdParser::guacd_encode_instruction(&size_instruction))
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
    debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd Handshake: Sending 'connect'");
    trace!(target: "guac_protocol", channel_id=%channel_id, conn_no, instruction=?connect_instruction, "Guacd Handshake: params");
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
    if let Some(client_id_from_ready) = ready_instruction.args.first() {
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, guacd_client_id=%client_id_from_ready, "Guacd handshake completed.");
    } else {
        debug!(target: "guac_protocol", channel_id=%channel_id, conn_no, "Guacd handshake completed. No client ID received with 'ready'.");
    }
    buffer_pool.release(handshake_buffer);
    Ok(())
}
