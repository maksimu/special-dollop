// Handler-based connection management (zero-copy integration with guacr)
//
// This module provides zero-copy integration with guacr protocol handlers using
// the EventCallback pattern. Falls back to channel-based interface for backwards
// compatibility if handlers don't support EventBasedHandler trait yet.

use async_trait::async_trait;
use guacr_handlers::{EventCallback, HandlerError, HandlerEvent, ProtocolHandlerRegistry};

use crate::channel::core::Channel;
use crate::models::ConversationType;
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely; // Branch prediction optimization
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use bytes::{BufMut, Bytes};
use log::{debug, error, info, trace, warn};
use std::sync::Arc;
use tokio::sync::mpsc;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_millis() as u64
}

/// Zero-copy event callback for WebRTC data channel
///
/// Implements the EventCallback trait from guacr-handlers for zero-copy
/// integration. All instructions are forwarded directly to WebRTC with
/// reference-counted Bytes (no copying). Only critical events trigger
/// additional processing.
///
/// Zero-copy event callback for built-in protocol handlers
struct WebRtcEventCallback {
    /// WebRTC data channel for sending instructions
    webrtc: WebRTCDataChannel,
    /// Connection number for framing
    conn_no: u32,
    /// Channel ID for logging
    channel_id: String,
    /// Notify when connection closes
    conn_closed_tx: mpsc::UnboundedSender<(u32, String)>,
    /// Buffer pool for frame encoding
    buffer_pool: crate::buffer_pool::BufferPool,
    /// Channel close reason storage
    channel_close_reason: Arc<tokio::sync::Mutex<Option<CloseConnectionReason>>>,
    /// Send queue for instructions (avoids spawning task per instruction)
    send_queue_tx: mpsc::Sender<Bytes>,
}

#[async_trait]
impl EventCallback for WebRtcEventCallback {
    /// Handle critical events from protocol handler
    ///
    /// This is called for errors, disconnects, and threats. Regular instructions
    /// bypass this method and go directly through send_instruction() for zero-copy.
    fn on_event(&self, event: HandlerEvent) {
        match event {
            HandlerEvent::Error { message, code } => {
                error!(
                    "Handler error (channel: {}, conn: {}): {} (code: {:?})",
                    self.channel_id, self.conn_no, message, code
                );

                // Send CloseConnection control message with error reason
                let mut payload = self.buffer_pool.acquire();
                payload.extend_from_slice(&self.conn_no.to_be_bytes());
                payload.put_u8(CloseConnectionReason::HandlerError as u8);

                let webrtc = self.webrtc.clone();
                let channel_id = self.channel_id.clone();
                let conn_no = self.conn_no;
                let buffer_pool = self.buffer_pool.clone();

                tokio::spawn(async move {
                    let frame = Frame::new_control_with_pool(
                        ControlMessage::CloseConnection,
                        &payload,
                        &buffer_pool,
                    );
                    let encoded = frame.encode_with_pool(&buffer_pool);
                    if let Err(e) = webrtc.send(encoded).await {
                        error!(
                            "Failed to send CloseConnection for handler error (channel: {}, conn: {}): {}",
                            channel_id, conn_no, e
                        );
                    }
                });

                // Notify connection closed
                self.conn_closed_tx
                    .send((self.conn_no, self.channel_id.clone()))
                    .ok();
            }

            HandlerEvent::Disconnect { reason } => {
                info!(
                    "Handler disconnect (channel: {}, conn: {}): {}",
                    self.channel_id, self.conn_no, reason
                );

                // Send CloseConnection control message with normal reason
                let mut payload = self.buffer_pool.acquire();
                payload.extend_from_slice(&self.conn_no.to_be_bytes());
                payload.put_u8(CloseConnectionReason::Normal as u8);

                let webrtc = self.webrtc.clone();
                let channel_id = self.channel_id.clone();
                let conn_no = self.conn_no;
                let buffer_pool = self.buffer_pool.clone();

                tokio::spawn(async move {
                    let frame = Frame::new_control_with_pool(
                        ControlMessage::CloseConnection,
                        &payload,
                        &buffer_pool,
                    );
                    let encoded = frame.encode_with_pool(&buffer_pool);
                    if let Err(e) = webrtc.send(encoded).await {
                        error!(
                            "Failed to send CloseConnection for disconnect (channel: {}, conn: {}): {}",
                            channel_id, conn_no, e
                        );
                    }
                });

                // Notify connection closed
                self.conn_closed_tx
                    .send((self.conn_no, self.channel_id.clone()))
                    .ok();
            }

            HandlerEvent::ThreatDetected { level, description } => {
                error!(
                    "THREAT DETECTED (channel: {}, conn: {}): [{}] {}",
                    self.channel_id, self.conn_no, level, description
                );

                // Immediate termination for security threats
                let mut payload = self.buffer_pool.acquire();
                payload.extend_from_slice(&self.conn_no.to_be_bytes());
                payload.put_u8(CloseConnectionReason::SecurityThreat as u8);

                let webrtc = self.webrtc.clone();
                let channel_id = self.channel_id.clone();
                let conn_no = self.conn_no;
                let buffer_pool = self.buffer_pool.clone();

                tokio::spawn(async move {
                    let frame = Frame::new_control_with_pool(
                        ControlMessage::CloseConnection,
                        &payload,
                        &buffer_pool,
                    );
                    let encoded = frame.encode_with_pool(&buffer_pool);
                    if let Err(e) = webrtc.send(encoded).await {
                        error!(
                            "Failed to send CloseConnection for threat (channel: {}, conn: {}): {}",
                            channel_id, conn_no, e
                        );
                    }
                });

                // Store threat detection as close reason
                let close_reason = self.channel_close_reason.clone();
                tokio::spawn(async move {
                    *close_reason.lock().await = Some(CloseConnectionReason::SecurityThreat);
                });

                // Notify connection closed
                self.conn_closed_tx
                    .send((self.conn_no, self.channel_id.clone()))
                    .ok();
            }

            HandlerEvent::Ready { connection_id } => {
                debug!(
                    "Handler ready (channel: {}, conn: {}, connection_id: {})",
                    self.channel_id, self.conn_no, connection_id
                );
            }

            HandlerEvent::Size { width, height } => {
                trace!(
                    "Handler size change (channel: {}, conn: {}): {}x{}",
                    self.channel_id,
                    self.conn_no,
                    width,
                    height
                );
            }

            HandlerEvent::Instruction(bytes) => {
                // This shouldn't happen in normal flow - send_instruction is the hot path
                warn!(
                    "Received Instruction event (should use send_instruction): {} bytes",
                    bytes.len()
                );
                // Spawn to call async from sync context (rare path, only for HandlerEvent::Instruction)
                let send_queue_tx = self.send_queue_tx.clone();
                let channel_id = self.channel_id.clone();
                let conn_no = self.conn_no;
                tokio::spawn(async move {
                    if let Err(e) = send_queue_tx.send(bytes).await {
                        trace!(
                            "Send queue closed (channel: {}, conn: {}): {}",
                            channel_id,
                            conn_no,
                            e
                        );
                    }
                });
            }
        }
    }

    /// Send instruction to WebRTC client (ZERO-COPY HOT PATH WITH BACKPRESSURE)
    ///
    /// This is the performance-critical path. Bytes is reference-counted (Arc internally),
    /// so no copying occurs. The instruction is queued for sending by a dedicated sender task.
    ///
    /// Uses blocking send().await to provide proper backpressure. If the WebRTC send queue
    /// is full, this awaits here (blocking the handler's forwarding task) rather than:
    /// - Dropping frames (unacceptable for RDP/VNC/database protocols)
    /// - Spawning tasks (which caused the original "one char behind" latency)
    ///
    /// The large channel capacity (4096) ensures backpressure only occurs under severe
    /// network congestion, and the handler's internal buffer prevents debounce starvation.
    async fn send_instruction(&self, instruction: Bytes) -> Result<(), HandlerError> {
        // Send to dedicated sender task with backpressure (blocks if queue full)
        // This is the RIGHT behavior: better to slow down the handler than drop frames
        if let Err(e) = self.send_queue_tx.send(instruction).await {
            // Channel closed - sender task died, connection is closing
            trace!(
                "Send queue closed (expected during shutdown) (channel: {}, conn: {}): {}",
                self.channel_id,
                self.conn_no,
                e
            );
            return Err(HandlerError::ChannelError(format!(
                "Send queue closed: {}",
                e
            )));
        }
        Ok(())
    }
}

/// Invoke a built-in protocol handler with zero-copy integration
///
/// This function uses the EventCallback pattern for zero-copy data sharing.
/// Falls back to channel-based interface if handlers don't implement
/// EventBasedHandler yet (backwards compatible).
///
/// # Zero-Copy Architecture
///
/// Handler -> EventCallback::send_instruction(Bytes) -> Frame -> WebRTC
///                            |
///                            v
///                      No copying (Bytes is Arc)
///
/// # RAII Safety
///
/// - All tasks are spawned with proper cleanup
/// - handler_senders are removed on completion
/// - No resource leaks (compiler enforced)
pub async fn invoke_builtin_handler(
    channel: &Channel,
    conversation_type: &ConversationType,
    registry: &Arc<ProtocolHandlerRegistry>,
    conn_no: u32,
    spawned_task_completion_tx: Arc<tokio::sync::mpsc::UnboundedSender<()>>,
) -> Result<()> {
    let protocol_name = conversation_type.to_string();

    info!(
        "Channel({}): Invoking built-in handler for protocol '{}' (conn_no: {})",
        channel.channel_id, protocol_name, conn_no
    );

    // Get handler from registry
    let handler = registry
        .get(&protocol_name)
        .ok_or_else(|| anyhow!("No handler registered for protocol: {}", protocol_name))?;

    debug!(
        "Channel({}): Found handler for protocol: {}",
        channel.channel_id, protocol_name
    );

    // Get connection parameters
    let params = channel.guacd_params.lock().await.clone();

    debug!(
        "Channel({}): Handler params: {:?}",
        channel.channel_id,
        params.keys().collect::<Vec<_>>()
    );

    // Check if handler supports event-based interface (zero-copy path)
    if handler.as_event_based().is_some() {
        info!(
            "Channel({}): Using event-based zero-copy interface for protocol: {}",
            channel.channel_id, protocol_name
        );

        // Clone the Arc so we can move it into the spawn
        let handler_arc = Arc::clone(&handler);

        // Create send queue for outbound instructions (Handler → WebRTC)
        // This avoids spawning a new task for every instruction (fixes "one char behind" issue)
        // Capacity 4096 matches the SSH handler's internal buffer to prevent bottlenecks
        let (send_queue_tx, mut send_queue_rx) = mpsc::channel::<Bytes>(4096);

        // Spawn dedicated sender task (one task for all instructions)
        let webrtc_for_sender = channel.webrtc.clone();
        let buffer_pool_for_sender = channel.buffer_pool.clone();
        let channel_id_for_sender = channel.channel_id.clone();
        let conn_no_for_sender = conn_no;

        let sender_handle = tokio::spawn(async move {
            debug!(
                "Dedicated sender task started (channel: {}, conn: {})",
                channel_id_for_sender, conn_no_for_sender
            );

            while let Some(instruction) = send_queue_rx.recv().await {
                // Wrap handler output in Frame with connection number
                let frame = Frame {
                    connection_no: conn_no_for_sender,
                    timestamp_ms: now_ms(),
                    payload: instruction,
                };

                // Encode frame (zero-copy: Bytes is refcounted)
                let encoded = frame.encode_with_pool(&buffer_pool_for_sender);

                // Send directly to WebRTC (no spawn - we're already in a dedicated task)
                if let Err(e) = webrtc_for_sender.send(encoded).await {
                    error!(
                        "Failed to send instruction (channel: {}, conn: {}): {}",
                        channel_id_for_sender, conn_no_for_sender, e
                    );
                    // Break on send error - connection is likely dead
                    break;
                }
            }

            debug!(
                "Dedicated sender task ended (channel: {}, conn: {})",
                channel_id_for_sender, conn_no_for_sender
            );
        });

        // Spawn monitor task to track sender task completion
        let sender_completion_tx = Arc::clone(&spawned_task_completion_tx);
        tokio::spawn(async move {
            match sender_handle.await {
                Ok(()) => {
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Handler sender task completed normally");
                    }
                }
                Err(e) => {
                    error!("Handler sender task panicked or was cancelled: {}", e);
                }
            }
            let _ = sender_completion_tx.send(());
        });

        // Create event callback for zero-copy integration
        let callback = Arc::new(WebRtcEventCallback {
            webrtc: channel.webrtc.clone(),
            conn_no,
            channel_id: channel.channel_id.clone(),
            conn_closed_tx: channel.conn_closed_tx.clone(),
            buffer_pool: channel.buffer_pool.clone(),
            channel_close_reason: channel.channel_close_reason.clone(),
            send_queue_tx,
        });

        // Create channel for bidirectional communication (WebRTC → Handler)
        // The handler receives client messages (keyboard, mouse, etc.) through this channel
        let (webrtc_tx, from_client_rx) = mpsc::channel::<Bytes>(128);

        // Store sender so incoming frames can be forwarded to handler
        channel.handler_senders.insert(conn_no, webrtc_tx.clone());

        // Spawn handler task with event callback
        let protocol_for_task = protocol_name.clone();
        let handler_senders = channel.handler_senders.clone();
        let conn_closed_tx_clone = channel.conn_closed_tx.clone();
        let channel_id_clone = channel.channel_id.clone();
        let handler_handle = tokio::spawn(async move {
            debug!(
                "Event-based handler task started for protocol: {}",
                protocol_for_task
            );

            // Get event handler inside the task (where handler_arc lives)
            if let Some(event_handler) = handler_arc.as_event_based() {
                match event_handler
                    .connect_with_events(params, callback, from_client_rx)
                    .await
                {
                    Ok(()) => {
                        info!(
                            "Event-based handler completed successfully: {}",
                            protocol_for_task
                        );
                    }
                    Err(e) => {
                        error!("Event-based handler failed: {} - {}", protocol_for_task, e);
                    }
                }
            }

            // Cleanup: Remove handler sender when task completes
            handler_senders.remove(&conn_no);

            // Notify main Channel run loop that connection has closed
            // This triggers cleanup of backend connection and signals to WebRTC client
            conn_closed_tx_clone.send((conn_no, channel_id_clone)).ok();

            info!(
                "Event-based handler session ended for protocol: {}",
                protocol_for_task
            );
        });

        // Spawn monitor task to track event-based handler task completion
        let handler_completion_tx = Arc::clone(&spawned_task_completion_tx);
        tokio::spawn(async move {
            match handler_handle.await {
                Ok(()) => {
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Event-based handler task completed normally");
                    }
                }
                Err(e) => {
                    error!("Event-based handler task panicked or was cancelled: {}", e);
                }
            }
            let _ = handler_completion_tx.send(());
        });

        return Ok(());
    }

    // Fallback to channel-based interface (for handlers that don't implement EventBasedHandler yet)
    info!(
        "Channel({}): Using channel-based interface (fallback) for protocol: {}",
        channel.channel_id, protocol_name
    );

    // Create channels for bidirectional communication
    // Handler → WebRTC (handler sends messages to client)
    let (handler_to_webrtc, mut handler_rx) = mpsc::channel::<Bytes>(128);

    // WebRTC → Handler (client sends messages to handler)
    let (webrtc_tx, handler_from_webrtc) = mpsc::channel::<Bytes>(128);

    // Store sender so incoming frames can be forwarded to handler
    channel.handler_senders.insert(conn_no, webrtc_tx.clone());

    let protocol_for_handler = protocol_name.clone();
    let protocol_for_outbound = protocol_name.clone();

    // Spawn protocol handler task
    let handler_task = tokio::spawn(async move {
        debug!(
            "Channel-based handler task started for protocol: {}",
            protocol_for_handler
        );

        match handler
            .connect(params, handler_to_webrtc, handler_from_webrtc)
            .await
        {
            Ok(()) => {
                info!(
                    "Channel-based handler completed successfully: {}",
                    protocol_for_handler
                );
                Ok(())
            }
            Err(e) => {
                error!(
                    "Channel-based handler failed: {} - {}",
                    protocol_for_handler, e
                );
                Err(anyhow!("Handler error: {}", e))
            }
        }
    });

    // Clone needed fields for forwarding tasks
    let dc = channel.webrtc.clone();
    let channel_id_for_outbound = channel.channel_id.clone();
    let conn_closed_tx = channel.conn_closed_tx.clone();
    let buffer_pool = channel.buffer_pool.clone();

    // Task 1: Handler → WebRTC (outbound)
    // Read messages from handler and send to WebRTC data channel
    let outbound_task = tokio::spawn(async move {
        debug!(
            "Handler outbound task started (channel: {}, protocol: {})",
            channel_id_for_outbound, protocol_for_outbound
        );

        let mut message_count = 0u64;
        let mut total_bytes = 0u64;

        while let Some(msg) = handler_rx.recv().await {
            message_count += 1;
            let msg_len = msg.len();
            total_bytes += msg_len as u64;

            // Log first 60 chars of message for debugging
            if crate::logger::is_verbose_logging() {
                let msg_preview = String::from_utf8_lossy(&msg[..msg.len().min(60)]);
                trace!(
                    "Handler → WebRTC: message #{} ({} bytes): {}...",
                    message_count,
                    msg_len,
                    msg_preview
                );
            }

            // Wrap handler output in Frame with connection number
            let frame = Frame {
                connection_no: conn_no,
                timestamp_ms: now_ms(),
                payload: msg,
            };

            // Encode frame
            let encoded = frame.encode_with_pool(&buffer_pool);

            // Send to WebRTC data channel
            match dc.send(encoded).await {
                Ok(_) => {
                    if crate::logger::is_verbose_logging() {
                        debug!(
                            "Handler message #{} sent to WebRTC successfully",
                            message_count
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to send handler message #{} to WebRTC: {}",
                        message_count, e
                    );
                    break;
                }
            }
        }

        info!(
            "Handler outbound task ended (total messages: {}, total bytes: {})",
            message_count, total_bytes
        );

        // Notify that connection closed
        conn_closed_tx.send((conn_no, channel_id_for_outbound)).ok();
    });

    // Task 2: WebRTC → Handler (inbound)
    // This is already wired through handler_senders in frame_handling.rs
    debug!("Handler inbound path ready (conn_no: {})", conn_no);

    // Spawn cleanup task that waits for handler completion (RAII)
    let protocol_for_cleanup = protocol_name.clone();
    let handler_senders = channel.handler_senders.clone();
    let cleanup_handle = tokio::spawn(async move {
        // Wait for handler to complete
        let _ = handler_task.await;
        let _ = outbound_task.await;

        // Cleanup: Remove handler sender
        handler_senders.remove(&conn_no);

        info!(
            "Channel-based handler session ended for protocol: {}",
            protocol_for_cleanup
        );
    });

    // Spawn monitor task to track channel-based handler cleanup task completion
    // This ensures both handler_task and outbound_task are tracked
    let cleanup_completion_tx = Arc::clone(&spawned_task_completion_tx);
    tokio::spawn(async move {
        match cleanup_handle.await {
            Ok(()) => {
                if unlikely!(crate::logger::is_verbose_logging()) {
                    debug!("Channel-based handler cleanup task completed normally");
                }
            }
            Err(e) => {
                error!(
                    "Channel-based handler cleanup task panicked or was cancelled: {}",
                    e
                );
            }
        }
        let _ = cleanup_completion_tx.send(());
    });

    // Return immediately to allow ConnectionOpened to be sent
    debug!(
        "Handler spawned successfully for protocol: {}, conn_no: {}",
        protocol_name, conn_no
    );
    Ok(())
}
