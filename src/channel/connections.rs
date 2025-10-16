// Connection management functionality for Channel

use crate::buffer_pool::BufferPool;
use crate::channel::guacd_parser::{
    GuacdInstruction, GuacdParser, OpcodeAction, PeekError, SpecialOpcode,
};
use crate::channel::types::ActiveProtocol;
use crate::models::Conn;
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely; // Branch prediction optimization
use crate::webrtc_data_channel::{EventDrivenSender, STANDARD_BUFFER_THRESHOLD};
use anyhow::Result;
use bytes::{Buf, BufMut, BytesMut};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

use super::core::Channel;

/// Determine if a connection event should be logged
/// Returns true if verbose logging is enabled OR if it's a critical/disconnect event
#[inline(always)]
fn should_log_connection(is_critical: bool) -> bool {
    crate::logger::is_verbose_logging() || is_critical
}

/// Last backpressure log timestamp (rate limiting)
static LAST_BACKPRESSURE_LOG: AtomicU64 = AtomicU64::new(0);

// Open a backend connection to a given address
pub async fn open_backend(
    channel: &mut Channel,
    conn_no: u32,
    addr: SocketAddr,
    active_protocol: ActiveProtocol,
) -> Result<()> {
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Endpoint {}: Opening connection {} to {} for protocol {:?} (Channel ServerMode: {})",
            channel.channel_id, conn_no, addr, active_protocol, channel.server_mode
        );
    }

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
            if unlikely!(should_log_connection(false)) {
                debug!(
                    "Marked as primary Guacd data connection. (channel_id: {})",
                    channel.channel_id
                );
            }
        } else if *primary_conn_no_guard != Some(conn_no) {
            // This case would be unusual - opening a new Guacd connection when one (potentially different conn_no) is already primary.
            // For now, log it. Depending on design, there might be an error or a secondary stream.
            if unlikely!(should_log_connection(false)) {
                debug!("Opening additional Guacd connection; primary already set. (channel_id: {}, existing_primary: {:?})", channel.channel_id, *primary_conn_no_guard);
            }
        }
    }

    // Connect to the backend - measure connection time for latency visibility
    let connect_start = std::time::Instant::now();
    let stream = TcpStream::connect(addr).await?;
    let connect_duration_ms = connect_start.elapsed().as_millis() as f64;

    // **CRITICAL: Disable Nagle's algorithm for low latency + high throughput**
    // This prevents batching delays while still allowing kernel-level optimizations
    stream.set_nodelay(true)?;

    // Log backend connection latency for connection leg visibility
    let backend_type = if active_protocol == ActiveProtocol::Guacd {
        "Gateway<->Guacd"
    } else {
        "Gateway<->Target"
    };

    if unlikely!(should_log_connection(false)) {
        debug!(
            "Backend connection established (TCP_NODELAY) | channel_id: {} | {}: {:.1}ms | addr: {}",
            channel.channel_id, backend_type, connect_duration_ms, addr
        );
    }

    if unlikely!(should_log_connection(false)) {
        debug!(
            "PRE-CALL to setup_outbound_task (channel_id: {}, conn_no: {}, backend_addr: {}, active_protocol: {:?}, server_mode: {})",
            channel.channel_id,
            conn_no,
            addr,
            active_protocol,
            channel.server_mode
        );
    }
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

    // TRACE: Ultra-verbose task lifecycle logging (only in verbose mode)
    if unlikely!(crate::logger::is_verbose_logging()) {
        log::trace!(
            "ENTERING setup_outbound_task function (channel_id: {}, conn_no: {}, active_protocol: {:?}, server_mode: {})",
            channel_id_for_task,
            conn_no,
            active_protocol,
            is_channel_server_mode
        );
    }

    if active_protocol == ActiveProtocol::Guacd {
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Channel({}): Performing Guacd handshake for conn_no {}",
                channel_id_for_task, conn_no
            );
        }

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
                if unlikely!(should_log_connection(false)) {
                    debug!(
                        "Channel({}): Guacd handshake successful for conn_no {}",
                        channel_id_clone, conn_no
                    );
                }
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
                reusable_control_buf.put_u8(CloseConnectionReason::GuacdError as u8);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                // **OPTIMIZED**: Use event-driven sending for handshake error
                // NOTE: In handshake context, event_sender is not available, use dc directly
                let send_start = std::time::Instant::now();
                match dc.send(encoded_frame.clone()).await {
                    Ok(_) => {
                        let send_latency = send_start.elapsed();
                        crate::metrics::METRICS_COLLECTOR.record_message_sent(
                            &channel_id_clone,
                            encoded_frame.len() as u64,
                            Some(send_latency),
                        );
                    }
                    Err(_) => {
                        crate::metrics::METRICS_COLLECTOR
                            .record_error(&channel_id_clone, "handshake_error_send_failed");
                    }
                }
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
                reusable_control_buf.put_u8(CloseConnectionReason::GuacdError as u8);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
                // **OPTIMIZED**: Use event-driven sending for handshake timeout
                // NOTE: In handshake context, event_sender is not available, use dc directly
                let send_start = std::time::Instant::now();
                match dc.send(encoded_frame.clone()).await {
                    Ok(_) => {
                        let send_latency = send_start.elapsed();
                        crate::metrics::METRICS_COLLECTOR.record_message_sent(
                            &channel_id_clone,
                            encoded_frame.len() as u64,
                            Some(send_latency),
                        );
                    }
                    Err(_) => {
                        crate::metrics::METRICS_COLLECTOR
                            .record_error(&channel_id_clone, "handshake_timeout_send_failed");
                    }
                }
                return Err(anyhow::anyhow!("Guacd handshake timed out"));
            }
        }
    }

    if unlikely!(crate::logger::is_verbose_logging()) {
        log::trace!(
            "PRE-SPAWN (outer scope) in setup_outbound_task (channel_id: {}, conn_no: {}, active_protocol: {:?}, server_mode: {})",
            channel.channel_id,
            conn_no,
            active_protocol,
            is_channel_server_mode
        );
    }

    // Create channel for backend task (client→guacd direction)
    let (data_tx, data_rx) = mpsc::unbounded_channel::<crate::models::ConnectionMessage>();

    // Clone data_tx for use in outbound task (to send sync auto-responses back to guacd)
    let data_tx_for_sync = data_tx.clone();

    // Create StreamHalf wrapper for backend_writer (needed for AsyncReadWrite trait)
    let stream_half = crate::models::StreamHalf {
        reader: None,
        writer: backend_writer,
    };

    // Start backend task FIRST (handles client→guacd writes, including our sync responses)
    let backend_task = tokio::spawn(crate::models::backend_task_runner(
        Box::new(stream_half),
        data_rx,
        conn_no,
        channel_id_for_task.clone(),
    ));

    let outbound_handle = tokio::spawn(async move {
        // TRACE: Task lifecycle logging (ultra-verbose, only in verbose mode)
        if unlikely!(crate::logger::is_verbose_logging()) {
            log::trace!(
                "setup_outbound_task TASK SPAWNED (channel_id: {}, conn_no: {}, active_protocol: {:?}, server_mode: {})",
                channel_id_for_task,
                conn_no,
                active_protocol,
                is_channel_server_mode
            );
        }

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
                    // TRACE: Ultra-verbose send tracking (suppressed unless verbose mode)
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        log::trace!(
                            "Event-driven send successful (0ms latency) (channel_id: {}, conn_no: {}, context: {}, queue_depth: {}, can_send_immediate: {}, is_over_threshold: {}, threshold: {})",
                            channel_id_local,
                            conn_no_local,
                            context_msg,
                            event_sender.queue_depth(),
                            event_sender.can_send_immediate(),
                            event_sender.is_over_threshold(),
                            event_sender.get_threshold()
                        );
                    }
                    Ok(())
                }
                Err(e) => {
                    // Only log if the error is not related to a closed connection
                    if !e.to_string().contains("DataChannel is not opened")
                        && !e.to_string().contains("Channel is closing")
                    {
                        error!("Event-driven send failed (channel_id: {}, conn_no: {}, context: {}, error: {})", channel_id_local, conn_no_local, context_msg, e);
                    }
                    Err(())
                }
            }
        }

        // Original task logic starts here

        let mut reader = backend_reader;
        let mut eof_sent = false;
        let mut loop_iterations = 0;

        let mut main_read_buffer = buffer_pool.acquire();
        let mut encode_buffer = buffer_pool.acquire();

        // **SCIENTIFICALLY DERIVED VALUES FROM WEBRTC-RS SOURCE + PROTOCOL ANALYSIS**
        // WebRTC-rs internals: RECEIVE_MTU = 8KB (webrtc-data/src/data_channel/mod.rs)
        // Threshold: STANDARD_BUFFER_THRESHOLD = 8KB (event fires when buffer < 8KB free)
        // Guacamole protocol analysis:
        //   - SSH/telnet: 90% instructions < 100 bytes (key, mouse, sync)
        //   - RDP/VNC: Mixed - small copy (64B) + large img (1-16KB PNG tiles)
        // Strategy: Per-read flush makes batch size irrelevant for SSH (always flushes immediately)
        //           while allowing RDP to batch efficiently within one screen update burst

        const MAX_READ_SIZE: usize = 8 * 1024; // 8KB - matches WebRTC RECEIVE_MTU and threshold (prevents 2x rate mismatch)
        const GUACD_BATCH_SIZE: usize = 16 * 1024; // 16KB - optimal for RDP tile batching, SSH flushes immediately anyway
        const LARGE_INSTRUCTION_THRESHOLD: usize = 32 * 1024; // 32KB - bypass batching for rare huge blob/img instructions

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

        // **BOLD WARNING: ENTERING HOT PATH - BACKEND→WEBRTC MAIN LOOP**
        // **NO STRING ALLOCATIONS, NO UNNECESSARY OBJECT CREATION**
        // **USE BORROWED DATA, BUFFER POOLS, AND ZERO-COPY TECHNIQUES**

        // Zombie prevention: Track backpressure stall iterations
        let mut backpressure_stall_counter = 0;
        const BACKPRESSURE_STALL_LIMIT: usize = 100; // 100 * 10ms = 1 second between channel state checks

        loop {
            loop_iterations += 1;

            // **CRITICAL: TCP BACKPRESSURE - Check queue depth before reading more data**
            let queue_depth = event_sender.queue_depth();
            const MAX_QUEUE_FRAMES: usize = 10000; // Match the queue size in EventDrivenSender
            const BACKPRESSURE_THRESHOLD: usize = MAX_QUEUE_FRAMES / 2; // 50% = 5,000 frames (adjusted for faster drain rate)

            // Log queue status periodically for monitoring
            if unlikely!(should_log_connection(false)) && loop_iterations % 1000 == 0 {
                debug!(
                    "Queue status check (channel_id: {}, conn_no: {}, queue_depth: {}/{}, fill: {:.1}%)",
                    channel_id_for_task, conn_no, queue_depth, MAX_QUEUE_FRAMES,
                    (queue_depth as f64 / MAX_QUEUE_FRAMES as f64) * 100.0
                );
            }

            // If queue is > 50% full (5,000 frames), pause reading to prevent overflow
            if queue_depth > BACKPRESSURE_THRESHOLD {
                backpressure_stall_counter += 1;

                // ZOMBIE PREVENTION: Check if data channel closed (efficient - once per second, not every 10ms)
                if backpressure_stall_counter >= BACKPRESSURE_STALL_LIMIT {
                    let channel_state = dc.ready_state();

                    if channel_state != "Open" {
                        warn!(
                            "Data channel closed during backpressure - exiting zombie task (channel_id: {}, conn_no: {}, queue: {}, state: {})",
                            channel_id_for_task, conn_no, queue_depth, channel_state
                        );
                        break; // Exit zombie task immediately
                    }
                    backpressure_stall_counter = 0; // Reset for next interval
                }

                // Rate-limited logging (once every 5 seconds max)
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let last = LAST_BACKPRESSURE_LOG.load(Ordering::Relaxed);

                if now - last >= 5 {
                    LAST_BACKPRESSURE_LOG.store(now, Ordering::Relaxed);
                    debug!(
                        "Backpressure active: Queue filling up (channel_id: {}, conn_no: {}, queue: {}/{} = {:.1}%)",
                        channel_id_for_task, conn_no, queue_depth, MAX_QUEUE_FRAMES,
                        (queue_depth as f64 / MAX_QUEUE_FRAMES as f64) * 100.0
                    );
                }

                // Brief pause to let queue drain
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue; // Skip this read iteration, check queue again
            } else {
                // Reset stall counter when queue is healthy
                backpressure_stall_counter = 0;
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
                            if unlikely!(should_log_connection(false)) {
                                debug!("OUTBOUND: About to validate and detect special opcodes (channel_id: {}, conn_no: {}, slice_len: {}, first_bytes: {:?})",
                                       channel_id_for_task, conn_no, current_slice.len(), &current_slice[..std::cmp::min(50, current_slice.len())]);
                            }
                            match GuacdParser::validate_and_detect_special(current_slice) {
                                Ok((instruction_len, action)) => {
                                    #[cfg(feature = "profiling")]
                                    {
                                        let parse_duration = parse_start.elapsed();
                                        if parse_duration.as_micros() > 100 {
                                            debug!(
                                                "Channel({}): Slow Guacd validate: {}μs",
                                                channel_id_for_task,
                                                parse_duration.as_micros()
                                            );
                                        }
                                    }

                                    // Log opcode action for verbose debugging
                                    if unlikely!(should_log_connection(false)) {
                                        debug!("Opcode action dispatched by expandable system (channel_id: {}, conn_no: {}, action: {:?}, instruction_len: {})",
                                           channel_id_for_task, conn_no, action, instruction_len);
                                    }

                                    // Dispatch based on opcode action
                                    match action {
                                        OpcodeAction::CloseConnection => {
                                            warn!("Error opcode detected - preparing to close connection (channel_id: {}, conn_no: {}, expected_opcode: {})", channel_id_for_task, conn_no, crate::channel::guacd_parser::ERROR_OPCODE);

                                            // **COLD PATH**: Error opcode detected
                                            match GuacdParser::peek_instruction(current_slice) {
                                                Ok(error_instr) => {
                                                    error!("Guacd sent error opcode - closing connection (channel_id: {}, conn_no: {}, opcode: {}, expected_opcode: {}, args: {:?})", channel_id_for_task, conn_no, error_instr.opcode, crate::channel::guacd_parser::ERROR_OPCODE, error_instr.args);
                                                }
                                                Err(_) => {
                                                    error!("Guacd sent error opcode but failed to parse args - closing connection (channel_id: {}, conn_no: {}, expected_opcode: {})", channel_id_for_task, conn_no, crate::channel::guacd_parser::ERROR_OPCODE);
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
                                            temp_buf_for_control
                                                .put_u8(CloseConnectionReason::GuacdError as u8);

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
                                        OpcodeAction::ServerSync => {
                                            // GUACD KEEPALIVE AUTO-RESPONSE
                                            // Guacd sends sync every ~5s as keepalive ping, client must respond within 15s
                                            // When browser is backgrounded (throttled to 1 FPS), response is delayed 10-16s → timeout
                                            // We intercept and respond immediately to prevent false "User is not responding" disconnects

                                            // **CRITICAL FIX: Flush batch buffer BEFORE handling sync**
                                            // Bug (commit 196ba77): sync handler would continue without flushing batch,
                                            // causing keystroke echoes to wait ~1 second for next read/sync
                                            if let Some(ref mut batch_buffer) = guacd_batch_buffer {
                                                if !batch_buffer.is_empty() {
                                                    encode_buffer.clear();
                                                    let bytes_written =
                                                        Frame::encode_data_frame_from_slice(
                                                            &mut encode_buffer,
                                                            conn_no,
                                                            &batch_buffer[..],
                                                        );
                                                    let batch_frame_bytes = encode_buffer
                                                        .split_to(bytes_written)
                                                        .freeze();
                                                    if send_with_event_backpressure(
                                                        batch_frame_bytes,
                                                        conn_no,
                                                        &event_sender,
                                                        &channel_id_for_task,
                                                        "pre-sync batch flush",
                                                    )
                                                    .await
                                                    .is_err()
                                                    {
                                                        close_conn_and_break = true;
                                                        break;
                                                    }
                                                    batch_buffer.clear();
                                                }
                                            }

                                            if let Ok(peeked_instr) =
                                                GuacdParser::peek_instruction(current_slice)
                                            {
                                                if peeked_instr.opcode == "sync"
                                                    && !peeked_instr.args.is_empty()
                                                {
                                                    let timestamp = peeked_instr.args[0];

                                                    if unlikely!(should_log_connection(false)) {
                                                        debug!(
                                                            "KEEPALIVE: Detected guacd sync, auto-responding immediately (channel_id: {}, conn_no: {}, timestamp: {})",
                                                            channel_id_for_task, conn_no, timestamp
                                                        );
                                                    }

                                                    // Send sync response back to guacd IMMEDIATELY (write directly to backend)
                                                    let sync_response =
                                                        GuacdParser::guacd_encode_instruction(
                                                            &GuacdInstruction::new(
                                                                "sync".to_string(),
                                                                vec![timestamp.to_string()],
                                                            ),
                                                        );

                                                    if let Err(e) = data_tx_for_sync.send(
                                                        crate::models::ConnectionMessage::Data(
                                                            sync_response,
                                                        ),
                                                    ) {
                                                        error!(
                                                            "Failed to send sync auto-response to guacd: {} (channel_id: {}, conn_no: {})",
                                                            e, channel_id_for_task, conn_no
                                                        );
                                                    } else if unlikely!(should_log_connection(
                                                        false
                                                    )) {
                                                        debug!(
                                                            "KEEPALIVE: Sent sync auto-response to guacd (channel_id: {}, timestamp: {}, prevents 15s timeout)",
                                                            channel_id_for_task, timestamp
                                                        );
                                                    }
                                                }
                                            }

                                            // Still forward the original sync to client (for frame timing/display sync)
                                            // This is intentional - both our auto-response AND client's eventual response are fine
                                            // Guacd will accept the first response (ours) and ignore duplicates
                                            let instruction_slice =
                                                &current_slice[..instruction_len];
                                            let data_frame = Frame::new_data_with_pool(
                                                conn_no,
                                                instruction_slice,
                                                &buffer_pool,
                                            );
                                            let encoded_data =
                                                data_frame.encode_with_pool(&buffer_pool);

                                            if send_with_event_backpressure(
                                                encoded_data,
                                                conn_no,
                                                &event_sender,
                                                &channel_id_for_task,
                                                "Guacd sync forward to client",
                                            )
                                            .await
                                            .is_err()
                                            {
                                                error!(
                                                    "Channel({}): Conn {}: Failed to forward sync to client",
                                                    channel_id_for_task, conn_no
                                                );
                                            }

                                            // Consume the instruction from buffer
                                            consumed_offset += instruction_len;
                                            continue; // Process next instruction
                                        }
                                        OpcodeAction::ProcessSpecial(opcode) => {
                                            // Note: Disconnect/close events use CloseConnection action (line 532) with warn! logging
                                            // SpecialOpcode is for Size and other non-critical opcodes
                                            if unlikely!(should_log_connection(false)) {
                                                debug!("OUTBOUND: Special opcode detected - dispatching to handler (channel_id: {}, conn_no: {}, opcode_name: {}, opcode: {:?})", channel_id_for_task, conn_no, opcode.as_str(), opcode);
                                            }

                                            // Dispatch to appropriate special handler
                                            match opcode {
                                                SpecialOpcode::Size => {
                                                    // Parse the full instruction for details and send to Python
                                                    if let Ok(peeked_instr) =
                                                        GuacdParser::peek_instruction(current_slice)
                                                    {
                                                        if peeked_instr.args.len() >= 2 {
                                                            if unlikely!(should_log_connection(
                                                                false
                                                            )) {
                                                                debug!("OUTBOUND: Server size instruction (actual session size) - sending to signal system (channel_id: {}, conn_no: {}, layer: {}, width: {}, height: {})", channel_id_for_task, conn_no, peeked_instr.args[0], peeked_instr.args.get(1).unwrap_or(&"unknown"), peeked_instr.args.get(2).unwrap_or(&"unknown"));
                                                            }

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
                                                                // LOCK-FREE: Iterate over tubes (DashMap)
                                                                let registry =
                                                                    &crate::tube_registry::REGISTRY;

                                                                // Find which tube contains this channel
                                                                let mut found_tube_id = None;
                                                                for entry in registry.tubes().iter()
                                                                {
                                                                    let (tube_id, tube) = (
                                                                        entry.key(),
                                                                        entry.value(),
                                                                    );
                                                                    let channels_guard = tube
                                                                        .active_channels
                                                                        .read()
                                                                        .await;
                                                                    if channels_guard.contains_key(
                                                                        &channel_id_clone,
                                                                    ) {
                                                                        found_tube_id =
                                                                            Some(tube_id.clone());
                                                                        if unlikely!(
                                                                            should_log_connection(
                                                                                false
                                                                            )
                                                                        ) {
                                                                            debug!("OUTBOUND: Found tube containing this channel (channel_id: {}, tube_id: {})", channel_id_clone, tube_id);
                                                                        }
                                                                        break;
                                                                    }
                                                                }

                                                                if let Some(tube_id) = found_tube_id
                                                                {
                                                                    if let Some(signal_sender) =
                                                                        registry.get_signal_sender(
                                                                            &tube_id,
                                                                        )
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
                                                                            debug!("OUTBOUND: Failed to send actual size signal to Python (tube_id: {}, channel_id: {}, error: {})", tube_id, channel_id_clone, e);
                                                                        } else if unlikely!(
                                                                            should_log_connection(
                                                                                false
                                                                            )
                                                                        ) {
                                                                            debug!("OUTBOUND: Successfully sent actual size signal to Python (tube_id: {}, channel_id: {})", tube_id, channel_id_clone);
                                                                        }
                                                                    } else {
                                                                        debug!("OUTBOUND: No signal sender found for tube (tube_id: {})", tube_id);
                                                                    }
                                                                } else {
                                                                    debug!("OUTBOUND: Could not find tube containing this channel");
                                                                }
                                                            });
                                                        } else if unlikely!(should_log_connection(
                                                            false
                                                        )) {
                                                            debug!("OUTBOUND: Size instruction with insufficient args - skipping signal (channel_id: {}, opcode_name: {})", channel_id_for_task, SpecialOpcode::Size.as_str());
                                                        }
                                                    } else if unlikely!(should_log_connection(
                                                        false
                                                    )) {
                                                        debug!("OUTBOUND: Failed to parse size instruction - skipping signal (channel_id: {}, opcode_name: {})", channel_id_for_task, SpecialOpcode::Size.as_str());
                                                    }
                                                }
                                                SpecialOpcode::Error => {
                                                    // This should not happen as Error maps to CloseConnection
                                                    unreachable!("Error opcode should map to CloseConnection action");
                                                } // Add more handlers as needed:
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
                                    temp_buf_for_control
                                        .put_u8(CloseConnectionReason::ProtocolError as u8);
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

                        // **CRITICAL: PER-READ FLUSH - Prevents SSH latency accumulation**
                        // After processing all complete instructions from THIS TCP read, flush the batch immediately.
                        // This is the key to making large batch sizes work for both protocols:
                        //   - SSH: One keystroke = one TCP read → flushes 150 bytes immediately (instant!)
                        //   - RDP: Screen update = one TCP read with many tiles → batches efficiently, then flushes
                        // Without per-read flush: SSH keystrokes would wait for 16KB accumulation = MASSIVE lag
                        // With per-read flush: Batch size becomes "maximum within one read burst", not "target to wait for"
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
                                    "per-read flush",
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

                        // Encode directly from temp_read_buffer (which was filled by read_buf)
                        let bytes_written = Frame::encode_data_frame_from_slice(
                            &mut encode_buffer,
                            conn_no,
                            &temp_read_buffer[..],
                        );

                        let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();

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
                                "Failed to send PortForward/SOCKS5 data with event-driven backpressure - closing connection (channel_id: {}, conn_no: {})", channel_id_for_task, conn_no
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
        if unlikely!(should_log_connection(true)) {
            // Critical: connection closing
            debug!(
                "Endpoint {}: Backend→WebRTC task for connection {} exited",
                channel_id_for_task, conn_no
            );
        }
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
                debug!("Failed to send connection closure signal; channel might be shutting down. (channel_id: {}, conn_no: {}, error: {:?})", channel_id_for_task, conn_no, e
                );
            }
        } else if unlikely!(should_log_connection(true)) {
            // Critical: disconnect event
            debug!(
                "Sent connection closure signal to Channel run loop. (channel_id: {}, conn_no: {})",
                channel_id_for_task, conn_no
            );
        }
    });

    // Create connection struct with our pre-created backend task and data_tx channel
    // Note: outbound_handle is the to_webrtc task (guacd→client)
    let conn = Conn {
        data_tx,                    // Channel for sending data to guacd (including sync responses)
        backend_task,               // Task that writes client data to guacd
        to_webrtc: outbound_handle, // Task that reads guacd data and sends to client
    };

    channel.conns.insert(conn_no, conn);

    if unlikely!(should_log_connection(false)) {
        debug!(
            "Endpoint {}: Connection {} added to registry",
            channel.channel_id, conn_no
        );
    }

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
                                "Invalid instruction length peeked ({}) vs buffer len ({}). Opcode: '{}'. Buffer (approx): {:?} (channel_id: {}, conn_no: {})",
                                instruction_total_len, *current_buffer_len, peeked_instr.opcode, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)], channel_id, conn_no
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
                        error!(
                            "Error during handshake (channel_id: {}, conn_no: {}, error: {})",
                            channel_id, conn_no, err_msg
                        );
                        return Err(anyhow::anyhow!(err_msg));
                    }
                }
            }; // peek_result is dropped here

            // Now we can safely mutate handshake_buffer
            if let Some((instruction, advance_len, expected_opcode_check)) = process_result {
                handshake_buffer.advance(advance_len);
                *current_buffer_len -= advance_len;

                if instruction.opcode == "error" {
                    error!("Guacd sent error during handshake (channel_id: {}, error_opcode: {}, expected_opcode: {}, error_args: {:?})", channel_id, instruction.opcode, expected_opcode, instruction.args);
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
                    error!("Unexpected Guacd opcode (channel_id: {}, expected_opcode: {}, received_opcode: {}, received_args: {:?})", channel_id, expected_opcode, instruction.opcode, instruction.args);
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
                    error!("EOF during Guacd handshake (channel_id: {}, expected_opcode: {}, buffer_len: {})", channel_id, expected_opcode, *current_buffer_len);
                    return Err(anyhow::anyhow!("EOF during Guacd handshake while waiting for '{}' (incomplete data in buffer)", expected_opcode));
                }
                Ok(n_read) => {
                    if handshake_buffer.capacity() < *current_buffer_len + n_read {
                        handshake_buffer
                            .reserve(*current_buffer_len + n_read - handshake_buffer.capacity());
                    }
                    handshake_buffer.put_slice(&temp_read_buf[..n_read]);
                    *current_buffer_len += n_read;
                    if unlikely!(should_log_connection(false)) {
                        debug!("Read more data for handshake, waiting for '{}' (channel_id: {}, conn_no: {}, bytes_read: {}, new_buffer_len: {})", expected_opcode, channel_id, conn_no, n_read, *current_buffer_len);
                    }
                }
                Err(e) => {
                    error!("Read error waiting for Guacd instruction (channel_id: {}, expected_opcode: {}, error: {})", channel_id, expected_opcode, e);
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
                // Only split on backslash if it's NOT Azure AD format
                if username.starts_with("AzureAD\\") || username.starts_with(".\\AzureAD\\") {
                    if unlikely!(should_log_connection(false)) {
                        debug!("Azure AD format detected - setting security to aad (channel_id: {}, conn_no: {}, username: {})", channel_id, conn_no, username);
                    }
                    guacd_params_locked.insert("security".to_string(), "aad".to_string());
                } else if let Some(pos) = username.find('\\') {
                    let domain = &username[..pos];
                    let user = &username[pos + 1..];
                    if unlikely!(should_log_connection(false)) {
                        debug!("Traditional domain found - splitting (channel_id: {}, conn_no: {}, domain: {}, username: {})", channel_id, conn_no, domain, user);
                    }
                    guacd_params_locked.insert("username".to_string(), user.to_string());
                    guacd_params_locked.insert("domain".to_string(), domain.to_string());
                }
            }
        }
    }

    let protocol_name_from_params = guacd_params_locked.get("protocol").cloned().unwrap_or_else(|| {
        warn!("Guacd 'protocol' missing in guacd_params, defaulting to 'rdp' for select fallback. (channel_id: {})", channel_id);
        "rdp".to_string()
    });

    let join_connection_id_key = "connectionid";
    let join_connection_id_opt = guacd_params_locked.get(join_connection_id_key).cloned();
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Checked for join connection ID in guacd_params (channel_id: {}, key_looked_up: {})",
            channel_id, join_connection_id_key
        );
    }

    let select_arg: String;
    if let Some(id_to_join) = &join_connection_id_opt {
        if unlikely!(should_log_connection(false)) {
            debug!("Guacd Handshake: Preparing to join existing session. (channel_id: {}, session_to_join: {})", channel_id, id_to_join);
        }
        select_arg = id_to_join.clone();
    } else {
        if unlikely!(should_log_connection(false)) {
            debug!("Guacd Handshake: Preparing for new session with protocol. (channel_id: {}, protocol: {})", channel_id, protocol_name_from_params);
        }
        select_arg = protocol_name_from_params;
    }

    let readonly_param_key = "readonly";
    let readonly_param_value_from_map = guacd_params_locked.get(readonly_param_key).cloned();
    if unlikely!(should_log_connection(false)) {
        debug!("Initial 'readonly' value from guacd_params_locked for join attempt. (channel_id: {}, readonly_param_value_from_map: {:?})", channel_id, readonly_param_value_from_map);
    }

    let readonly_str_for_join =
        readonly_param_value_from_map.unwrap_or_else(|| "false".to_string());
    if unlikely!(should_log_connection(false)) {
        debug!("Effective 'readonly_str_for_join' (after unwrap_or_else) for join attempt. (channel_id: {}, readonly_str_for_join: {})", channel_id, readonly_str_for_join);
    }

    let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Final 'is_readonly' boolean for join attempt. (channel_id: {}, is_readonly_bool: {})",
            channel_id, is_readonly
        );
    }

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
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: Sending 'select' (channel_id: {}, instruction: {:?})",
            channel_id, select_instruction
        );
    }
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&select_instruction))
        .await?;
    writer.flush().await?;

    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: Waiting for 'args' (channel_id: {})",
            channel_id
        );
    }
    let args_instruction = read_expected_instruction_stateless(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "args",
    )
    .await?;
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: Received 'args' from Guacd server (channel_id: {}, received_args: {:?})",
            channel_id, args_instruction.args
        );
    }

    const EXPECTED_GUACD_VERSION: &str = "VERSION_1_5_0";
    let connect_version_arg = args_instruction.args.first().cloned().unwrap_or_else(|| {
        warn!(
            "'args' instruction missing version, defaulting to {} (channel_id: {}, conn_no: {})",
            EXPECTED_GUACD_VERSION, channel_id, conn_no
        );
        EXPECTED_GUACD_VERSION.to_string()
    });
    if connect_version_arg != EXPECTED_GUACD_VERSION {
        warn!("Guacd version mismatch. Expected: '{}', Received: '{}'. Proceeding with received version for connect. (channel_id: {}, conn_no: {})", EXPECTED_GUACD_VERSION, connect_version_arg, channel_id, conn_no);
    }

    let mut connect_args: Vec<String> = Vec::new();
    connect_args.push(connect_version_arg);

    if join_connection_id_opt.is_some() {
        info!(
            "Guacd Handshake: Preparing 'connect' for JOINING session. (channel_id: {})",
            channel_id
        );
        let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
        if unlikely!(should_log_connection(false)) {
            debug!("Readonly status for join. (channel_id: {}, requested_readonly_param: {}, is_readonly_for_connect: {})", channel_id, readonly_str_for_join, is_readonly);
        }

        for (idx, arg_name_from_guacd) in args_instruction.args.iter().enumerate() {
            if idx == 0 {
                continue;
            }

            let is_readonly_arg_name_literal = "read-only";
            let is_current_arg_readonly_keyword =
                arg_name_from_guacd == is_readonly_arg_name_literal;

            if unlikely!(should_log_connection(false)) {
                debug!("Looping for connect_args (join). Comparing '{}' with '{}' (channel_id: {}, conn_no: {}, current_arg_name_from_guacd: {}, is_readonly_param_from_config: {}, is_current_arg_the_readonly_keyword: {})", arg_name_from_guacd, is_readonly_arg_name_literal, channel_id, conn_no, arg_name_from_guacd, is_readonly, is_current_arg_readonly_keyword);
            }

            if is_current_arg_readonly_keyword {
                let value_to_push = if is_readonly {
                    "true".to_string()
                } else {
                    "".to_string()
                };
                if unlikely!(should_log_connection(false)) {
                    debug!("Pushing to connect_args for 'read-only' keyword (channel_id: {}, conn_no: {}, arg_name_being_processed: {}, is_readonly_flag_for_push: {}, value_being_pushed_for_readonly_arg: {})", channel_id, conn_no, arg_name_from_guacd, is_readonly, value_to_push);
                }
                connect_args.push(value_to_push);
            } else {
                connect_args.push("".to_string());
            }
        }
    } else {
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd Handshake: Preparing 'connect' for NEW session. (channel_id: {})",
                channel_id
            );
        }

        let parse_mimetypes = |mimetype_str: &str| -> Vec<String> {
            if mimetype_str.is_empty() {
                return Vec::new();
            }
            serde_json::from_str::<Vec<String>>(mimetype_str)
                .unwrap_or_else(|e| {
                    if unlikely!(should_log_connection(false)) {
                        debug!("Failed to parse mimetype string '{}' as JSON array, splitting by comma as fallback. (channel_id: {}, conn_no: {}, error: {})", mimetype_str, channel_id, conn_no, e);
                    }
                    mimetype_str.split(',').map(String::from).filter(|s| !s.is_empty()).collect()
                })
        };

        let size_parts: Vec<String> = width_for_new
            .split(',')
            .chain(height_for_new.split(','))
            .chain(dpi_for_new.split(','))
            .map(String::from)
            .collect();
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd Handshake (new): Sending 'size' (channel_id: {})",
                channel_id
            );
        }

        // **HANDSHAKE SIZE INSTRUCTION DETECTION**: Log for debugging (no Python signal)
        let size_instruction = GuacdInstruction::new("size".to_string(), size_parts.clone());
        if size_parts.len() >= 2 && unlikely!(should_log_connection(false)) {
            debug!("HANDSHAKE: Client initial size instruction (debug only - not sent to signal system) (channel_id: {}, conn_no: {}, layer: {}, width: {}, height: {}, dpi: {})", channel_id, conn_no, "0", size_parts.first().map(|s| s.as_str()).unwrap_or("1024"), size_parts.get(1).map(|s| s.as_str()).unwrap_or("768"), size_parts.get(2).map(|s| s.as_str()).unwrap_or("96"));
        }

        writer
            .write_all(&GuacdParser::guacd_encode_instruction(&size_instruction))
            .await?;
        writer.flush().await?;

        let audio_mimetypes = parse_mimetypes(&audio_mimetypes_str_for_new);
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd Handshake (new): Sending 'audio' (channel_id: {})",
                channel_id
            );
        }
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("audio".to_string(), audio_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let video_mimetypes = parse_mimetypes(&video_mimetypes_str_for_new);
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd Handshake (new): Sending 'video' (channel_id: {})",
                channel_id
            );
        }
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("video".to_string(), video_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let image_mimetypes = parse_mimetypes(&image_mimetypes_str_for_new);
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd Handshake (new): Sending 'image' (channel_id: {})",
                channel_id
            );
        }
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
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: Sending 'connect' (channel_id: {})",
            channel_id
        );
    }
    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: params (channel_id: {}, instruction: {:?})",
            channel_id, connect_instruction
        );
    }
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&connect_instruction))
        .await?;
    writer.flush().await?;

    if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd Handshake: Waiting for 'ready' (channel_id: {})",
            channel_id
        );
    }
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
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Guacd handshake completed. (channel_id: {}, guacd_client_id: {})",
                channel_id, client_id_from_ready
            );
        }
    } else if unlikely!(should_log_connection(false)) {
        debug!(
            "Guacd handshake completed. No client ID received with 'ready'. (channel_id: {})",
            channel_id
        );
    }
    buffer_pool.release(handshake_buffer);
    Ok(())
}
