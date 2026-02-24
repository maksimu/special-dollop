// Connection management functionality for Channel

use crate::buffer_pool::BufferPool;
use crate::channel::assembler::{
    fragment_frame, DEFAULT_FRAGMENT_THRESHOLD, DEFAULT_MAX_FRAGMENTS,
};
use crate::channel::types::ActiveProtocol;
use crate::models::Conn;
use crate::tube_protocol::{Capabilities, CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely; // Branch prediction optimization
use crate::webrtc_data_channel::{EventDrivenSender, STANDARD_BUFFER_THRESHOLD};
use anyhow::Result;
use bytes::{Buf, BufMut, BytesMut};
use guacr_protocol::{GuacdInstruction, GuacdParser, OpcodeAction, PeekError, SpecialOpcode};
use log::{debug, error, warn};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{timeout, Duration};

use super::core::Channel;

/// Determine if a connection event should be logged
/// Returns true if verbose logging is enabled OR if it's a critical/disconnect event
#[inline(always)]
fn should_log_connection(is_critical: bool) -> bool {
    crate::logger::is_verbose_logging() || is_critical
}

/// Last backpressure log timestamp (rate limiting)
static LAST_BACKPRESSURE_LOG: AtomicU64 = AtomicU64::new(0);

/// Read timeout for cancellation check interval
/// This allows the backend read task to check for cancellation every 500ms
/// instead of waiting for TCP timeout (2-3 seconds)
const READ_CANCELLATION_CHECK_INTERVAL: Duration = Duration::from_millis(500);

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
    let channel_close_reason_arc = channel.channel_close_reason.clone(); // For checking if Python already closed
    let fragmentation_enabled = channel.capabilities.contains(Capabilities::FRAGMENTATION);

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
        let handshake_timeout_duration = channel.timeouts.guacd_handshake;

        match timeout(
            handshake_timeout_duration,
            guacr_guacd::client::perform_guacd_handshake(
                &mut backend_reader,
                &mut backend_writer,
                &channel_id_clone,
                conn_no,
                guacd_params_clone,
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
                let error_str = e.to_string();
                error!(
                    "Channel({}): Guacd handshake failed for conn_no {}: {}",
                    channel_id_clone, conn_no, error_str
                );
                // Reuse a single buffer for both operations to avoid acquire/release cycles
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                reusable_control_buf.put_u8(CloseConnectionReason::GuacdError as u8);
                // Add error message (backward compatible extension)
                let error_bytes = error_str.as_bytes();
                let error_len = error_bytes.len().min(1024) as u16;
                reusable_control_buf.put_u16(error_len);
                reusable_control_buf.extend_from_slice(&error_bytes[..error_len as usize]);
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
                dc.drain(Duration::from_millis(500)).await;
                return Err(e);
            }
            Err(_) => {
                let error_str = "Guacd handshake timed out";
                error!(
                    "Channel({}): {} for conn_no {}",
                    channel_id_clone, error_str, conn_no
                );
                // Reuse a single buffer for both operations to avoid acquire/release cycles
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                reusable_control_buf.put_u8(CloseConnectionReason::GuacdError as u8);
                // Add error message (backward compatible extension)
                let error_bytes = error_str.as_bytes();
                let error_len = error_bytes.len().min(1024) as u16;
                reusable_control_buf.put_u16(error_len);
                reusable_control_buf.extend_from_slice(&error_bytes[..error_len as usize]);
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
                dc.drain(Duration::from_millis(500)).await;
                return Err(anyhow::anyhow!("Guacd handshake timed out"));
            }
        }
    } else if active_protocol == ActiveProtocol::DatabaseProxy {
        // Database Proxy handshake - similar to Guacd but with simplified protocol
        if unlikely!(should_log_connection(false)) {
            debug!(
                "Channel({}): Performing DatabaseProxy handshake for conn_no {}",
                channel_id_for_task, conn_no
            );
        }

        let channel_id_clone = channel_id_for_task.clone();
        let db_params_clone = channel.db_params.clone();
        let buffer_pool_clone = buffer_pool.clone();
        let handshake_timeout_duration = channel.timeouts.guacd_handshake; // Reuse guacd timeout

        // Get the database type from params, default to "auto" for proxy-side detection
        let db_type = {
            let params = db_params_clone.lock().await;
            params
                .get("protocol")
                .cloned()
                .unwrap_or_else(|| "auto".to_string())
        };

        match timeout(
            handshake_timeout_duration,
            perform_database_proxy_handshake(
                &mut backend_reader,
                &mut backend_writer,
                &channel_id_clone,
                conn_no,
                &db_type,
                db_params_clone.clone(),
                buffer_pool_clone,
            ),
        )
        .await
        {
            Ok(Ok(_)) => {
                if unlikely!(should_log_connection(false)) {
                    debug!(
                        "Channel({}): DatabaseProxy handshake successful for conn_no {}",
                        channel_id_clone, conn_no
                    );
                }
            }
            Ok(Err(e)) => {
                let error_str = e.to_string();
                error!(
                    "Channel({}): DatabaseProxy handshake failed for conn_no {}: {}",
                    channel_id_clone, conn_no, error_str
                );
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                reusable_control_buf.put_u8(CloseConnectionReason::ProxyError as u8);
                let error_bytes = error_str.as_bytes();
                let error_len = error_bytes.len().min(1024) as u16;
                reusable_control_buf.put_u16(error_len);
                reusable_control_buf.extend_from_slice(&error_bytes[..error_len as usize]);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
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
                        crate::metrics::METRICS_COLLECTOR.record_error(
                            &channel_id_clone,
                            "db_proxy_handshake_error_send_failed",
                        );
                    }
                }
                dc.drain(Duration::from_millis(500)).await;
                return Err(e);
            }
            Err(_) => {
                let error_str = "DatabaseProxy handshake timed out";
                error!(
                    "Channel({}): {} for conn_no {}",
                    channel_id_clone, error_str, conn_no
                );
                let mut reusable_control_buf = buffer_pool.acquire();
                reusable_control_buf.clear();
                reusable_control_buf.extend_from_slice(&conn_no.to_be_bytes());
                reusable_control_buf.put_u8(CloseConnectionReason::ProxyError as u8);
                let error_bytes = error_str.as_bytes();
                let error_len = error_bytes.len().min(1024) as u16;
                reusable_control_buf.put_u16(error_len);
                reusable_control_buf.extend_from_slice(&error_bytes[..error_len as usize]);
                let close_frame = Frame::new_control_with_buffer(
                    ControlMessage::CloseConnection,
                    &mut reusable_control_buf,
                );
                let encoded_frame = close_frame.encode_with_pool(&buffer_pool);
                buffer_pool.release(reusable_control_buf);
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
                        crate::metrics::METRICS_COLLECTOR.record_error(
                            &channel_id_clone,
                            "db_proxy_handshake_timeout_send_failed",
                        );
                    }
                }
                dc.drain(Duration::from_millis(500)).await;
                return Err(anyhow::anyhow!("DatabaseProxy handshake timed out"));
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

    // Create cancellation token for immediate exit on WebRTC closure
    let cancel_read_task = tokio_util::sync::CancellationToken::new();
    let cancel_token_for_task = cancel_read_task.clone();

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
        // Now with optional fragmentation support for large frames
        #[inline(always)] // Hot path optimization
        async fn send_with_event_backpressure(
            frame_to_send: bytes::Bytes,
            conn_no_local: u32,
            event_sender: &EventDrivenSender,
            channel_id_local: &str,
            context_msg: &str,
            fragmentation_enabled: bool,
        ) -> Result<(), ()> {
            // Check if we need to fragment this frame
            if fragmentation_enabled && frame_to_send.len() > DEFAULT_FRAGMENT_THRESHOLD {
                // Large frame + fragmentation enabled: split into fragments
                if let Some(fragments) = fragment_frame(
                    &frame_to_send,
                    DEFAULT_FRAGMENT_THRESHOLD,
                    DEFAULT_MAX_FRAGMENTS,
                ) {
                    // Send each fragment through backpressure system
                    for (i, fragment) in fragments.into_iter().enumerate() {
                        match event_sender.send_with_natural_backpressure(fragment).await {
                            Ok(_) => {
                                if unlikely!(crate::logger::is_verbose_logging()) {
                                    log::trace!(
                                        "Fragment {}/{} sent (channel_id: {}, conn_no: {}, context: {})",
                                        i + 1,
                                        frame_to_send.len().div_ceil(DEFAULT_FRAGMENT_THRESHOLD - 9),
                                        channel_id_local,
                                        conn_no_local,
                                        context_msg
                                    );
                                }
                            }
                            Err(e) => {
                                if !e.contains("DataChannel is not opened")
                                    && !e.contains("Channel is closing")
                                {
                                    error!(
                                        "Fragment send failed (channel_id: {}, conn_no: {}, fragment: {}, error: {})",
                                        channel_id_local, conn_no_local, i, e
                                    );
                                }
                                return Err(());
                            }
                        }
                    }
                    return Ok(());
                }
                // If fragment_frame returns None (frame too large), fall through to send as-is
            }

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
        let mut clean_disconnect_received = false; // Track if disconnect opcode was seen
        let mut loop_iterations = 0;

        let mut drain_mode = false; // Guacd: discard data after WebRTC close, wait for guacd EOF
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

        // Orphaned task prevention: Track backpressure stall iterations
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

                // ORPHANED TASK PREVENTION: Check if data channel closed (efficient - once per second, not every 10ms)
                if backpressure_stall_counter >= BACKPRESSURE_STALL_LIMIT {
                    let channel_state = dc.ready_state();

                    if channel_state != "Open" {
                        warn!(
                            "Data channel closed during backpressure - exiting orphaned task (channel_id: {}, conn_no: {}, queue: {}, state: {})",
                            channel_id_for_task, conn_no, queue_depth, channel_state
                        );
                        break; // Exit orphaned task immediately
                    }
                    backpressure_stall_counter = 0; // Reset for next interval
                }

                // Rate-limited logging (once every 5 seconds max)
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
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
            // **CANCELLABLE READ**: Use tokio::select! to allow immediate exit on WebRTC closure
            let n_read = if active_protocol == ActiveProtocol::Guacd {
                // Ensure main_read_buffer has enough capacity *before* the read_buf call
                // This is slightly different from its previous position but more direct for this path.
                if main_read_buffer.capacity() - main_read_buffer.len() < MAX_READ_SIZE {
                    main_read_buffer.reserve(MAX_READ_SIZE);
                }
                tokio::select! {
                    biased;  // Check cancellation first for faster exit

                    _ = cancel_token_for_task.cancelled() => {
                        if unlikely!(should_log_connection(true)) {
                            debug!(
                                "Backend read cancelled due to WebRTC closure (channel_id: {}, conn_no: {})",
                                channel_id_for_task, conn_no
                            );
                        }
                        break;  // Exit immediately
                    }

                    read_result = tokio::time::timeout(
                        READ_CANCELLATION_CHECK_INTERVAL,
                        reader.read_buf(&mut main_read_buffer)
                    ) => {
                        match read_result {
                            Ok(Ok(n)) => n,
                            Ok(Err(e)) => {
                                error!(
                                    "Endpoint {}: Read error on connection {} (Guacd path): {}",
                                    channel_id_for_task, conn_no, e
                                );
                                break;
                            }
                            Err(_timeout) => {
                                // Read timeout - loop continues and checks cancellation
                                // This allows cancellation to be detected within 500ms
                                // instead of waiting for TCP timeout (2-3 seconds)
                                continue;
                            }
                        }
                    }
                }
            } else {
                tokio::select! {
                    biased;  // Check cancellation first for faster exit

                    _ = cancel_token_for_task.cancelled() => {
                        if unlikely!(should_log_connection(true)) {
                            debug!(
                                "Backend read cancelled due to WebRTC closure (channel_id: {}, conn_no: {})",
                                channel_id_for_task, conn_no
                            );
                        }
                        break;  // Exit immediately
                    }

                    read_result = tokio::time::timeout(
                        READ_CANCELLATION_CHECK_INTERVAL,
                        reader.read_buf(&mut temp_read_buffer)
                    ) => {
                        match read_result {
                            Ok(Ok(n)) => n,
                            Ok(Err(e)) => {
                                error!(
                                    "Endpoint {}: Read error on connection {} (Non-Guacd path): {}",
                                    channel_id_for_task, conn_no, e
                                );
                                break;
                            }
                            Err(_timeout) => {
                                // Read timeout - loop continues and checks cancellation
                                // This allows cancellation to be detected within 500ms
                                // instead of waiting for TCP timeout (2-3 seconds)
                                continue;
                            }
                        }
                    }
                }
            };

            match n_read {
                0 => {
                    // EOF detected - guacd closed connection
                    if !eof_sent {
                        // First EOF detection

                        // Check if this is a clean disconnect (disconnect opcode was sent)
                        // or an unexpected EOF (guacd crashed, network failure, auth error without protocol error)
                        if clean_disconnect_received {
                            // Clean disconnect - guacd sent disconnect opcode first
                            // Send SendEOF as half-close signal (existing behavior)
                            let eof_frame = Frame::new_control_with_pool(
                                ControlMessage::SendEOF,
                                &conn_no.to_be_bytes(),
                                &buffer_pool,
                            );
                            let encoded = eof_frame.encode_with_pool(&buffer_pool);
                            let _ = send_with_event_backpressure(
                                encoded,
                                conn_no,
                                &event_sender,
                                &channel_id_for_task,
                                "EOF frame (clean disconnect)",
                                fragmentation_enabled,
                            )
                            .await;
                            eof_sent = true;
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        } else {
                            // Unexpected EOF - guacd closed without sending disconnect/error opcode
                            // This indicates a problem: crash, auth failure, network error, etc.
                            warn!(
                                "Unexpected EOF from guacd - connection closed without disconnect opcode \
                                (channel_id: {}, conn_no: {})",
                                channel_id_for_task, conn_no
                            );

                            // Check if Python already sent a CloseConnection
                            let python_already_closed = channel_close_reason_arc
                                .try_lock()
                                .ok()
                                .and_then(|guard| *guard)
                                .is_some();

                            if !python_already_closed {
                                // Send CloseConnection with ConnectionLost reason
                                let mut temp_buf_for_control = buffer_pool.acquire();
                                temp_buf_for_control.clear();
                                temp_buf_for_control.extend_from_slice(&conn_no.to_be_bytes());
                                temp_buf_for_control
                                    .put_u8(CloseConnectionReason::ConnectionLost as u8);

                                // Add error message
                                let error_msg = "Backend connection closed unexpectedly";
                                let error_bytes = error_msg.as_bytes();
                                let error_len = error_bytes.len().min(1024) as u16;
                                temp_buf_for_control.put_u16(error_len);
                                temp_buf_for_control
                                    .extend_from_slice(&error_bytes[..error_len as usize]);

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
                                    "Unexpected EOF close",
                                    fragmentation_enabled,
                                )
                                .await
                                .is_err()
                                {
                                    error!(
                                        "Channel({}): Conn {}: Failed to send CloseConnection for unexpected EOF",
                                        channel_id_for_task, conn_no
                                    );
                                }

                                // Store close reason
                                if let Ok(mut guard) = channel_close_reason_arc.try_lock() {
                                    *guard = Some(CloseConnectionReason::ConnectionLost);
                                    if unlikely!(should_log_connection(false)) {
                                        debug!(
                                            "Stored ConnectionLost as close reason for unexpected EOF \
                                            (channel_id: {}, conn_no: {})",
                                            channel_id_for_task, conn_no
                                        );
                                    }
                                }

                                // CRITICAL: Drain buffer to ensure CloseConnection transmits.
                                // We wait up to 500ms to allow the CloseConnection control message to be sent
                                // over the network before the connection is torn down. This is necessary because
                                // without draining, the message may remain buffered and never reach the client,
                                // especially if the underlying transport is unreliable or slow. The 500ms timeout
                                // is chosen as a balance between giving the message a reasonable chance to transmit
                                // and not delaying shutdown excessively.
                                dc.drain(Duration::from_millis(500)).await;
                            } else if unlikely!(should_log_connection(false)) {
                                debug!(
                                    "Channel({}): Conn {}: Skipping CloseConnection for unexpected EOF \
                                    (Python already sent with specific reason)",
                                    channel_id_for_task, conn_no
                                );
                            }

                            // Exit immediately - connection is dead
                            break;
                        }
                    } else {
                        // Second EOF after SendEOF was sent - exit
                        break;
                    }
                    continue;
                }
                _ => {
                    eof_sent = false;

                    if drain_mode {
                        // WebRTC is already closed; discard guacd data until it sends EOF
                        main_read_buffer.clear();
                        continue;
                    }

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

                                    // Dispatch based on opcode action
                                    match action {
                                        OpcodeAction::CloseConnection => {
                                            // **COLD PATH**: Error or disconnect opcode detected
                                            // Parse instruction to determine which opcode it is
                                            // Also extract error message for CloseConnection
                                            let guacd_error_message: Option<String> =
                                                match GuacdParser::peek_instruction(current_slice) {
                                                    Ok(instr) => {
                                                        if instr.opcode
                                                            == guacr_protocol::DISCONNECT_OPCODE
                                                        {
                                                            // Guacd sent disconnect instruction - clean connection closure
                                                            clean_disconnect_received = true; // Mark as clean disconnect
                                                            warn!("Guacd sent disconnect instruction - closing connection cleanly (channel_id: {}, conn_no: {})", channel_id_for_task, conn_no);
                                                            Some("Guacd disconnect".to_string())
                                                        } else if instr.opcode
                                                            == guacr_protocol::ERROR_OPCODE
                                                        {
                                                            // Guacd sent error instruction - error condition
                                                            // Extract error message from args (typically args[0] is the error text)
                                                            let error_msg =
                                                                if !instr.args.is_empty() {
                                                                    format!(
                                                                        "Guacd error: {}",
                                                                        instr.args.join(", ")
                                                                    )
                                                                } else {
                                                                    "Guacd error".to_string()
                                                                };
                                                            error!("Guacd sent error opcode - closing connection (channel_id: {}, conn_no: {}, opcode: {}, args: {:?})", channel_id_for_task, conn_no, instr.opcode, instr.args);
                                                            Some(error_msg)
                                                        } else {
                                                            // Unknown close opcode
                                                            warn!("Guacd sent close instruction - closing connection (channel_id: {}, conn_no: {}, opcode: {}, args: {:?})", channel_id_for_task, conn_no, instr.opcode, instr.args);
                                                            Some(format!(
                                                                "Guacd close: {}",
                                                                instr.opcode
                                                            ))
                                                        }
                                                    }
                                                    Err(_) => {
                                                        // Failed to parse - assume error
                                                        error!("Guacd sent close opcode but failed to parse - closing connection (channel_id: {}, conn_no: {})", channel_id_for_task, conn_no);
                                                        Some(
                                                            "Guacd close opcode (parse failed)"
                                                                .to_string(),
                                                        )
                                                    }
                                                };

                                            // Forward the close instruction to the other side before closing
                                            // (could be error or disconnect opcode)
                                            let close_instruction_slice =
                                                &current_slice[..instruction_len];

                                            // Send the close instruction immediately
                                            let data_frame = Frame::new_data_with_pool(
                                                conn_no,
                                                close_instruction_slice,
                                                &buffer_pool,
                                            );
                                            let encoded_data =
                                                data_frame.encode_with_pool(&buffer_pool);

                                            if send_with_event_backpressure(
                                                encoded_data,
                                                conn_no,
                                                &event_sender,
                                                &channel_id_for_task,
                                                "Guacd close instruction forward",
                                                fragmentation_enabled,
                                            )
                                            .await
                                            .is_err()
                                            {
                                                error!(
                                                    "Channel({}): Conn {}: Failed to forward Guacd close instruction",
                                                    channel_id_for_task, conn_no
                                                );
                                            }

                                            // Check if Python already sent a CloseConnection with specific reason
                                            // (e.g., AI_CLOSED). If so, don't send a second one that would overwrite it.
                                            let python_already_closed = channel_close_reason_arc
                                                .try_lock()
                                                .ok()
                                                .and_then(|guard| *guard)
                                                .is_some();

                                            if python_already_closed {
                                                // Python already sent CloseConnection with the correct reason
                                                // (e.g., AI_CLOSED = 15). Don't send a second one with GuacdError.
                                                // This prevents overwriting AI_CLOSED with GuacdError in Vault.
                                                if unlikely!(should_log_connection(false)) {
                                                    debug!(
                                                        "Channel({}): Conn {}: Skipping redundant CloseConnection (Python already sent with specific reason)",
                                                        channel_id_for_task, conn_no
                                                    );
                                                }
                                            } else {
                                                // Send CloseConnection control frame
                                                // Use Normal for clean disconnect opcode, GuacdError for error opcode
                                                let close_reason = if clean_disconnect_received {
                                                    CloseConnectionReason::Normal
                                                } else {
                                                    CloseConnectionReason::GuacdError
                                                };
                                                let mut temp_buf_for_control =
                                                    buffer_pool.acquire();
                                                temp_buf_for_control.clear();
                                                temp_buf_for_control
                                                    .extend_from_slice(&conn_no.to_be_bytes());
                                                temp_buf_for_control.put_u8(close_reason as u8);
                                                // Add error message (backward compatible extension)
                                                if let Some(ref error_msg) = guacd_error_message {
                                                    let error_bytes = error_msg.as_bytes();
                                                    let error_len =
                                                        error_bytes.len().min(1024) as u16;
                                                    temp_buf_for_control.put_u16(error_len);
                                                    temp_buf_for_control.extend_from_slice(
                                                        &error_bytes[..error_len as usize],
                                                    );
                                                }

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
                                                    "Guacd close",
                                                    fragmentation_enabled,
                                                )
                                                .await
                                                .is_err()
                                                {
                                                    error!(
                                                        "Channel({}): Conn {}: Failed to send CloseConnection frame for Guacd close via event-driven system",
                                                        channel_id_for_task, conn_no
                                                    );
                                                }

                                                // Store the close reason so internal_handle_connection_close knows this was a GuacdError
                                                // This allows the tube to be closed with the correct reason instead of generic UpstreamClosed
                                                if let Ok(mut guard) =
                                                    channel_close_reason_arc.try_lock()
                                                {
                                                    *guard =
                                                        Some(CloseConnectionReason::GuacdError);
                                                    if unlikely!(should_log_connection(false)) {
                                                        debug!(
                                                            "Stored GuacdError as close reason for channel (channel_id: {}, conn_no: {})",
                                                            channel_id_for_task, conn_no
                                                        );
                                                    }
                                                }
                                            }

                                            // CRITICAL: Drain buffer to ensure CloseConnection transmits.
                                            // We wait up to 500ms to allow the CloseConnection control message to be sent
                                            // over the network before the connection is torn down. This is necessary because
                                            // without draining, the message may remain buffered and never reach the client,
                                            // especially if the underlying transport is unreliable or slow. The 500ms timeout
                                            // is chosen as a balance between giving the message a reasonable chance to transmit
                                            // and not delaying shutdown excessively.
                                            dc.drain(Duration::from_millis(500)).await;

                                            close_conn_and_break = true;
                                            break;
                                        }
                                        OpcodeAction::ServerSync => {
                                            // Flush batch buffer BEFORE handling sync**
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
                                                        fragmentation_enabled,
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
                                                fragmentation_enabled,
                                            )
                                            .await
                                            .is_err()
                                            {
                                                // WebRTC channel permanently closed - exit task to prevent zombie
                                                // The EventDrivenSender only returns Err for permanent closure,
                                                // temporary failures (buffer full) are queued and return Ok.
                                                debug!(
                                                    "Channel({}): Conn {}: WebRTC channel closed, exiting guacd outbound task",
                                                    channel_id_for_task, conn_no
                                                );
                                                close_conn_and_break = true;
                                                break;
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
                                                }
                                                SpecialOpcode::Disconnect => {
                                                    // This should not happen as Disconnect maps to CloseConnection
                                                    unreachable!("Disconnect opcode should map to CloseConnection action");
                                                } // Add more handlers as needed
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
                                                    fragmentation_enabled,
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
                                                fragmentation_enabled,
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
                                                    fragmentation_enabled,
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
                                    let error_str =
                                        format!("Guacd protocol parsing error: {:?}", e);
                                    error!(
                                        "Channel({}): Conn {}: Error peeking/parsing Guacd instruction: {:?}. Buffer content (approx): {:?}. Closing connection.",
                                        channel_id_for_task, conn_no, e, &main_read_buffer[..std::cmp::min(main_read_buffer.len(), 100)]
                                    );
                                    let mut temp_buf_for_control = buffer_pool.acquire();
                                    temp_buf_for_control.clear();
                                    temp_buf_for_control.extend_from_slice(&conn_no.to_be_bytes());
                                    temp_buf_for_control
                                        .put_u8(CloseConnectionReason::ProtocolError as u8);
                                    // Add error message (backward compatible extension)
                                    let error_bytes = error_str.as_bytes();
                                    let error_len = error_bytes.len().min(1024) as u16;
                                    temp_buf_for_control.put_u16(error_len);
                                    temp_buf_for_control
                                        .extend_from_slice(&error_bytes[..error_len as usize]);
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
                                        fragmentation_enabled,
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
                                    fragmentation_enabled,
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
                            // WebRTC send failed — enter drain mode: keep reading guacd TCP
                            // until it sends EOF, so guacd can process disconnect cleanly
                            main_read_buffer.clear();
                            if let Some(ref mut batch) = guacd_batch_buffer {
                                batch.clear();
                            }
                            drain_mode = true;
                            close_conn_and_break = false;
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
                            fragmentation_enabled,
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
                "Endpoint {}: Backend->WebRTC task for connection {} exited",
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
            // Proper error handling: Connection closure signal failed
            // This means the Channel run loop's receiver is closed (likely during shutdown)
            // The connection will remain in DashMap until Channel drops and RAII cleans it up
            // This is not ideal but acceptable - connection resources are already released by this point
            if !e.to_string().contains("channel closed") {
                warn!(
                    "Connection closure signal failed - receiver closed (channel_id: {}, conn_no: {}). \
                     Connection will remain in map until Channel Drop. Error: {:?}",
                    channel_id_for_task, conn_no, e
                );
            }
            // Note: We cannot remove the connection from DashMap here because we don't have
            // access to it. The connection will be cleaned up when Channel drops via RAII.
            // This is acceptable because:
            // 1. Connection resources (socket, tasks) are already released
            // 2. Only the map entry remains
            // 3. Channel Drop will clean up the map
            // The "proper way" would be to pass a Weak<DashMap> reference, but that adds
            // significant complexity for a rare edge case (shutdown race condition).
        } else if unlikely!(should_log_connection(true)) {
            // Critical: disconnect event
            debug!(
                "Sent connection closure signal to Channel run loop. (channel_id: {}, conn_no: {})",
                channel_id_for_task, conn_no
            );
        }
    });

    // Get next generation for this conn_no - prevents reuse race during cleanup (600ms-2.7s)
    // Use Relaxed ordering since generation is per-conn_no and doesn't need synchronization
    // with other conn_no values
    let generation = channel
        .conn_generations
        .entry(conn_no)
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(1, Ordering::Relaxed);

    // Create connection struct with our pre-created backend task and data_tx channel
    // Note: outbound_handle is the to_webrtc task (guacd→client)
    let conn = Conn {
        data_tx, // Channel for sending data to guacd (including sync responses)
        backend_task: Some(backend_task), // Task that writes client data to guacd
        to_webrtc: Some(outbound_handle), // Task that reads guacd data and sends to client
        cancel_read_task, // Cancellation token for immediate exit on WebRTC closure
        generation, // Increments on each conn_no reuse
        state: Arc::new(std::sync::atomic::AtomicU8::new(
            crate::models::CONN_STATE_ACTIVE,
        )),
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

/// Perform handshake with the database proxy.
///
/// Protocol:
///   1. Gateway → Proxy: select [db_type] (mysql/postgresql/sqlserver)
///   2. Proxy → Gateway: args [target_host, target_port, username, password, database, session_uid]
///   3. Gateway → Proxy: connect [arg_values...]
///   4. Proxy → Gateway: ready [session_id] OR error [message]
///
/// This is a simplified version of the Guacd handshake - no size/audio/video/image instructions.
pub(crate) async fn perform_database_proxy_handshake<R, W>(
    reader: &mut R,
    writer: &mut W,
    channel_id: &str,
    conn_no: u32,
    db_type: &str, // "mysql", "postgresql", "sqlserver"
    db_params: Arc<Mutex<HashMap<String, String>>>,
    buffer_pool: BufferPool,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + ?Sized,
    W: AsyncWriteExt + Unpin + Send + ?Sized,
{
    let mut handshake_buffer = buffer_pool.acquire();
    let mut current_handshake_buffer_len = 0;

    // Helper function to read expected instruction (reuse logic from guacd handshake)
    async fn read_expected_instruction<'a, SHelper>(
        reader: &'a mut SHelper,
        handshake_buffer: &'a mut BytesMut,
        current_buffer_len: &'a mut usize,
        _channel_id: &'a str,
        _conn_no: u32,
        expected_opcode: &'a str,
    ) -> Result<GuacdInstruction>
    where
        SHelper: AsyncRead + Unpin + Send + ?Sized,
    {
        loop {
            let process_result = {
                let peek_result =
                    GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len]);

                match peek_result {
                    Ok(peeked_instr) => {
                        let instruction_total_len = peeked_instr.total_length_in_buffer;
                        if instruction_total_len == 0 || instruction_total_len > *current_buffer_len
                        {
                            return Err(anyhow::anyhow!(
                                "Peeked instruction length is invalid or exceeds buffer."
                            ));
                        }
                        let content_slice = &handshake_buffer[..instruction_total_len - 1];

                        let instruction = GuacdParser::parse_instruction_content(content_slice)
                            .map_err(|e| {
                                anyhow::anyhow!(
                                    "DatabaseProxy Handshake: Failed to parse instruction: {}",
                                    e
                                )
                            })?;

                        let expected_opcode_check = peeked_instr.opcode == expected_opcode;
                        Some((instruction, instruction_total_len, expected_opcode_check))
                    }
                    Err(PeekError::Incomplete) => None,
                    Err(err) => {
                        return Err(anyhow::anyhow!(
                            "Error peeking instruction while expecting '{}': {:?}",
                            expected_opcode,
                            err
                        ));
                    }
                }
            };

            if let Some((instruction, advance_len, expected_opcode_check)) = process_result {
                handshake_buffer.advance(advance_len);
                *current_buffer_len -= advance_len;

                if instruction.opcode == "error" {
                    return Err(anyhow::anyhow!(
                        "Proxy sent error '{}' ({:?}) during handshake",
                        instruction.opcode,
                        instruction.args
                    ));
                }
                return if expected_opcode_check {
                    Ok(instruction)
                } else {
                    Err(anyhow::anyhow!(
                        "Expected opcode '{}', got '{}'",
                        expected_opcode,
                        instruction.opcode
                    ))
                };
            }

            // Need more data
            let mut temp_read_buf = [0u8; 1024];
            match reader.read(&mut temp_read_buf).await {
                Ok(0) => {
                    // EOF received - check if there's any remaining instruction in buffer
                    // (especially an error instruction that arrived just before connection close)
                    if *current_buffer_len > 0 {
                        if let Ok(peeked) =
                            GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len])
                        {
                            if peeked.total_length_in_buffer <= *current_buffer_len {
                                // Try to parse the instruction
                                let content_slice =
                                    &handshake_buffer[..peeked.total_length_in_buffer - 1];
                                if let Ok(instruction) =
                                    GuacdParser::parse_instruction_content(content_slice)
                                {
                                    if instruction.opcode == "error" {
                                        // Extract the actual error from proxy
                                        let error_msg = instruction
                                            .args
                                            .first()
                                            .map(|s| s.as_str())
                                            .unwrap_or("Unknown proxy error");
                                        let error_code = instruction
                                            .args
                                            .get(1)
                                            .map(|s| s.as_str())
                                            .unwrap_or("");
                                        return Err(anyhow::anyhow!(
                                            "Proxy error: {} (code: {})",
                                            error_msg,
                                            error_code
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    return Err(anyhow::anyhow!(
                        "EOF during DatabaseProxy handshake while waiting for '{}'",
                        expected_opcode
                    ));
                }
                Ok(n_read) => {
                    if handshake_buffer.capacity() < *current_buffer_len + n_read {
                        handshake_buffer
                            .reserve(*current_buffer_len + n_read - handshake_buffer.capacity());
                    }
                    handshake_buffer.put_slice(&temp_read_buf[..n_read]);
                    *current_buffer_len += n_read;
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
    }

    // Lock params once for reading
    let db_params_locked = db_params.lock().await;

    // Step 1: Send select with database type
    let select_instruction = GuacdInstruction::new("select".to_string(), vec![db_type.to_string()]);
    if unlikely!(should_log_connection(false)) {
        debug!(
            "DatabaseProxy Handshake: Sending 'select' (channel_id: {}, db_type: {})",
            channel_id, db_type
        );
    }
    let encoded_select = GuacdParser::guacd_encode_instruction(&select_instruction);
    writer.write_all(&encoded_select).await?;
    writer.flush().await?;

    // Step 2: Receive args from proxy
    let args_instruction = read_expected_instruction(
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
            "DatabaseProxy Handshake: Received 'args' (channel_id: {}, args: {:?})",
            channel_id, args_instruction.args
        );
    }

    // Step 3: Build connect args from params
    // Mapping from proxy arg names to guacd_params keys
    let mut connect_args: Vec<String> = Vec::new();

    for arg_name in &args_instruction.args {
        let value = match arg_name.as_str() {
            "target_host" => db_params_locked
                .get("hostname")
                .or_else(|| db_params_locked.get("target_host"))
                .cloned()
                .unwrap_or_default(),
            "target_port" => db_params_locked
                .get("port")
                .or_else(|| db_params_locked.get("target_port"))
                .cloned()
                .unwrap_or_default(),
            "username" => db_params_locked
                .get("username")
                .cloned()
                .unwrap_or_default(),
            "password" => db_params_locked
                .get("password")
                .cloned()
                .unwrap_or_default(),
            "database" => db_params_locked
                .get("database")
                .cloned()
                .unwrap_or_default(),
            "session_uid" => db_params_locked
                .get("session_uid")
                .or_else(|| db_params_locked.get("sessionUid"))
                .or_else(|| db_params_locked.get("conversation_id"))
                .cloned()
                .unwrap_or_default(),
            _ => {
                // Try to find the key directly in params
                db_params_locked.get(arg_name).cloned().unwrap_or_default()
            }
        };
        connect_args.push(value);
    }

    drop(db_params_locked);

    // Step 4: Send connect
    let connect_instruction = GuacdInstruction::new("connect".to_string(), connect_args);
    if unlikely!(should_log_connection(false)) {
        debug!(
            "DatabaseProxy Handshake: Sending 'connect' (channel_id: {})",
            channel_id
        );
    }
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&connect_instruction))
        .await?;
    writer.flush().await?;

    // Step 5: Receive ready or error
    let ready_instruction = read_expected_instruction(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "ready",
    )
    .await?;

    if let Some(session_id) = ready_instruction.args.first() {
        if unlikely!(should_log_connection(false)) {
            debug!(
                "DatabaseProxy handshake completed (channel_id: {}, session_id: {})",
                channel_id, session_id
            );
        }
    } else if unlikely!(should_log_connection(false)) {
        debug!(
            "DatabaseProxy handshake completed (channel_id: {})",
            channel_id
        );
    }

    buffer_pool.release(handshake_buffer);
    Ok(())
}
