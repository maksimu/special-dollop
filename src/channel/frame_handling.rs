// Frame handling functionality for Channel

use super::core::{Channel, ProtocolLogicState, PythonHandlerMessage};
use super::types::ActiveProtocol;
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame, CTRL_NO_LEN};
use crate::unlikely; // Branch prediction optimization
use anyhow::{anyhow, Result};
use bytes::Bytes;
use log::{debug, error, warn};

// Memory prefetching optimizations
#[cfg(target_arch = "x86_64")]
mod prefetch_optimizations {
    use crate::models::Conn;
    use dashmap::DashMap;
    use std::arch::x86_64::*;

    /// Prefetch memory for DashMap lookup to improve cache performance
    #[inline(always)]
    pub fn prefetch_connection_lookup(conns: &DashMap<u32, Conn>, conn_no: u32) {
        // Calculate approximate hash bucket location for prefetch
        // This is a heuristic - actual DashMap implementation may vary
        let hash = conn_no as usize;
        let ptr = conns as *const _ as *const u8;

        unsafe {
            // Prefetch the likely memory location
            _mm_prefetch(
                ptr.add(hash * 64) as *const i8, // Approximate bucket location
                _MM_HINT_T0,                     // Prefetch to L1 cache
            );

            // Prefetch next cache line as well for better coverage
            _mm_prefetch(ptr.add(hash * 64 + 64) as *const i8, _MM_HINT_T0);
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod prefetch_optimizations {
    use crate::models::Conn;
    use dashmap::DashMap;

    /// No-op prefetch for non-x86_64 architectures
    #[inline(always)]
    pub fn prefetch_connection_lookup(_conns: &DashMap<u32, Conn>, _conn_no: u32) {
        // No prefetch on non-x86_64
    }

    /// No-op batch prefetch for non-x86_64 architectures
    /// Currently preparatory - will be used when we implement frame batching
    #[inline(always)]
    #[allow(dead_code)] // Preparatory for future batch processing optimization
    pub fn prefetch_multiple_connections(_conns: &DashMap<u32, Conn>, _conn_nos: &[u32]) {
        // No prefetch on non-x86_64
    }
}

use prefetch_optimizations::prefetch_connection_lookup;

// Branch prediction hints for hot/cold paths
#[inline(always)]
fn likely(condition: bool) -> bool {
    #[cold]
    fn cold() {}

    if !condition {
        cold();
    }
    condition
}

#[inline(always)]
fn unlikely(condition: bool) -> bool {
    #[cold]
    fn cold() {}

    if condition {
        cold();
    }
    condition
}

// Central dispatcher for incoming frames
// **BOLD WARNING: HOT PATH - CALLED FOR EVERY INCOMING FRAME**
// **NO STRING ALLOCATIONS IN DEBUG LOGS UNLESS ENABLED**
pub async fn handle_incoming_frame(channel: &mut Channel, frame: Frame) -> Result<()> {
    // **BRANCH PREDICTION OPTIMIZATION**: Connection 1 is ULTRA HOT PATH (90%+ of data)
    if likely(frame.connection_no == 1) {
        // **ULTRA HOT PATH**: Connection 1 - main data traffic (most optimized)

        // **HYPER-OPTIMIZED**: Inline everything for Connection 1
        #[inline(always)]
        async fn forward_connection1_ultra_fast(
            channel: &mut Channel,
            payload: Bytes,
        ) -> Result<()> {
            forward_to_protocol(channel, 1, payload).await
        }

        forward_connection1_ultra_fast(channel, frame.payload).await?;
    } else if frame.connection_no == 0 {
        // **CONTROL PATH**: Connection 0 - control messages (infrequent, OK to log)
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Handling control frame (channel_id: {})",
                channel.channel_id
            );
        }
        handle_control(channel, frame).await?;
    } else if frame.connection_no > 1 {
        // **WARM PATH**: Other connections - short-lived traffic
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Routing short-lived connection frame (channel_id: {}, conn_no: {})",
                channel.channel_id, frame.connection_no
            );
        }

        // **OPTIMIZED**: Regular data path for other connections
        forward_to_protocol(channel, frame.connection_no, frame.payload).await?;
    } else {
        // **ERROR PATH**: Should never happen
        return Err(anyhow::anyhow!(
            "Invalid connection number: {}",
            frame.connection_no
        ));
    }

    Ok(())
}

// Handle control frames (COLD PATH - infrequent)
#[cold]
pub async fn handle_control(channel: &mut Channel, frame: Frame) -> Result<()> {
    // ALWAYS log control messages with error!() to ensure visibility
    if unlikely(frame.payload.len() < CTRL_NO_LEN) {
        error!("DEBUG_CONTROL_ENTRY: Malformed control frame - too short!");
        return Err(anyhow!("Malformed control frame"));
    }

    let code = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
    let cmd = ControlMessage::try_from(code)?;
    let data_bytes = frame.payload.slice(CTRL_NO_LEN..);

    // Control messages are infrequent - only log in verbose mode
    if unlikely!(crate::logger::is_verbose_logging()) {
        debug!(
            "Processing control message (channel_id: {}, message_type: {:?})",
            channel.channel_id, cmd
        );
    }

    // Use the channel's control message handling methods
    match channel.process_control_message(cmd, &data_bytes).await {
        Ok(_) => {
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Successfully processed control message (channel_id: {}, message_type: {:?})",
                    channel.channel_id, cmd
                );
            }
            Ok(())
        }
        Err(e) => {
            error!(
                "Error processing control message (channel_id: {}, message_type: {:?}, error: {})",
                channel.channel_id, cmd, e
            );
            Err(e)
        }
    }
}

// Lock-free data forwarding using dedicated channels per connection
// **BOLD WARNING: HOT PATH - CALLED FOR EVERY DATA FRAME**
// **COMPLETELY LOCK-FREE: Uses channel communication instead of mutex!**
#[inline(always)] // Force inlining for maximum performance
async fn forward_to_protocol(channel: &mut Channel, conn_no: u32, payload: Bytes) -> Result<()> {
    // **PythonHandler FAST PATH**: Route directly to Python, bypassing connection lookup
    if channel.active_protocol == ActiveProtocol::PythonHandler {
        return forward_to_python_handler(channel, conn_no, payload).await;
    }

    let payload_len = payload.len(); // Store length before moving

    // **HANDLER PATH**: Check if this connection is managed by a built-in protocol handler
    {
        // DashMap provides lock-free concurrent access
        if let Some(sender_ref) = channel.handler_senders.get(&conn_no) {
            // Clone sender and drop the DashMap reference before await
            let sender = sender_ref.value().clone();
            drop(sender_ref);

            // Forward to protocol handler instead of normal TCP connection
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!(
                    "Forwarding to protocol handler (channel_id: {}, conn_no: {}, payload_len: {})",
                    channel.channel_id, conn_no, payload_len
                );
            }

            return match sender.send(payload).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    error!(
                        "Failed to forward frame to handler, removing sender (channel_id: {}, conn_no: {}, error: {})",
                        channel.channel_id, conn_no, e
                    );
                    // Remove dead sender
                    channel.handler_senders.remove(&conn_no);
                    Err(anyhow!("Handler channel closed for connection {}", conn_no))
                }
            };
        }
    }

    // HOT PATH: Only log in verbose mode (suppress 1000s/sec spam with video workloads)
    if unlikely!(crate::logger::is_verbose_logging()) {
        debug!(
            "Forwarding bytes via lock-free channel (channel_id: {}, conn_no: {}, payload_len: {})",
            channel.channel_id, conn_no, payload_len
        );

        if payload_len > 0 && payload_len <= 100 {
            debug!(
                "Payload for backend (channel_id: {}, payload: {:?})",
                channel.channel_id, payload
            );
        } else if payload_len > 100 {
            let first_bytes = payload.slice(..std::cmp::min(50, payload_len));
            debug!(
                "Large payload first bytes (channel_id: {}, first_bytes: {:?})",
                channel.channel_id, first_bytes
            );
        }
    } // Close the is_verbose_logging() block

    // Skip inbound special instruction detection - user only wants outbound and handshake sizes
    // **COMPLETELY LOCK-FREE**: DashMap provides efficient concurrent access
    // **MEMORY OPTIMIZATION**: Smart prefetching for 2-connection pattern (always enabled)
    if likely(conn_no == 1) {
        // **HOT PATH**: Connection 1 is main traffic - always prefetch
        prefetch_connection_lookup(&channel.conns, conn_no);
    } else if conn_no == 0 {
        // **CONTROL PATH**: Connection 0 is control channel - lighter prefetch
        prefetch_connection_lookup(&channel.conns, conn_no);
    }
    // For conn_no > 1: Short-lived connections, skip prefetch to avoid cache pollution

    // **HOT PATH OPTIMIZATION**: Single lookup eliminates race conditions and improves performance
    // **FAST PATH**: Connections 0 and 1 are persistent, very likely to exist
    let send_result = if likely(conn_no <= 1) {
        // Use likely() hint for branch prediction on persistent connections
        channel.conns.get(&conn_no)
    } else {
        // **VARIABLE PATH**: Short-lived connections, no branch prediction hint needed
        channel.conns.get(&conn_no)
    }
    .map(|conn_ref| {
        // **HOT PATH**: Connection exists - most common case
        // Send data to the connection's dedicated task (lock-free!)
        conn_ref
            .data_tx
            .send(crate::models::ConnectionMessage::Data(payload))
    });

    match send_result {
        Some(Ok(_)) => {
            return Ok(());
        }
        Some(Err(_)) => {
            // **COLD PATH**: Channel closed - rare error case
            warn!(
                "Backend task is dead, closing connection (channel_id: {}, conn_no: {})",
                channel.channel_id, conn_no
            );
            // Connection reference is dropped, safe to call close_backend
            channel
                .close_backend(
                    conn_no,
                    CloseConnectionReason::ConnectionLost,
                    Some("Backend task is dead"),
                )
                .await?;
            return Err(anyhow!("Backend task for connection {} is dead", conn_no));
        }
        None => {
            // **COLD PATH**: Connection not found - fall through to logging
        }
    }

    // **COLD PATH**: Connection not found - should be rare in normal operation
    #[cold]
    fn log_connection_not_found(channel_id: &str, conn_no: u32) {
        warn!(
            "Connection not found for forwarding data, data lost (channel_id: {}, conn_no: {})",
            channel_id, conn_no
        );
    }

    log_connection_not_found(&channel.channel_id, conn_no);
    Ok(())
}

/// Forward data to Python handler for PythonHandler protocol mode
/// **HOT PATH for PythonHandler mode**
#[inline(always)]
async fn forward_to_python_handler(
    channel: &mut Channel,
    conn_no: u32,
    payload: Bytes,
) -> Result<()> {
    // Verify the connection is registered (was confirmed via ConnectionOpened)
    let is_registered = if let ProtocolLogicState::PythonHandler(ref state) = channel.protocol_state
    {
        state.active_connections.contains(&conn_no)
    } else {
        false
    };

    if !is_registered {
        // Data arrived for unregistered connection - this can happen if:
        // 1. Data arrives before ConnectionOpened (race condition)
        // 2. Connection was never opened or was closed
        // Log and drop the data rather than crashing
        warn!(
            "Data received for unregistered PythonHandler connection (channel_id: {}, conn_no: {}), dropping {} bytes",
            channel.channel_id, conn_no, payload.len()
        );
        return Ok(());
    }

    if let Some(ref tx) = channel.python_handler_tx {
        // Send data message to Python handler
        if tx
            .send(PythonHandlerMessage::Data { conn_no, payload })
            .await
            .is_err()
        {
            // Channel closed - Python handler is gone
            warn!(
                "Python handler channel closed, cannot forward data (channel_id: {}, conn_no: {})",
                channel.channel_id, conn_no
            );
            return Err(anyhow!("Python handler channel closed"));
        }
        Ok(())
    } else {
        // Should not happen - PythonHandler mode requires python_handler_tx
        error!(
            "PythonHandler mode but python_handler_tx is None (channel_id: {})",
            channel.channel_id
        );
        Err(anyhow!("PythonHandler mode missing python_handler_tx"))
    }
}
