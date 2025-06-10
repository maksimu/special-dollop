// Frame handling functionality for Channel

use super::core::Channel;
use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame, CTRL_NO_LEN};
use crate::{debug_hot_path, warn_hot_path};
use anyhow::{anyhow, Result};
use bytes::Bytes;
use tracing::{debug, error}; // Import centralized hot path macros

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

    /// Prefetch multiple connection lookups for batch processing
    /// Currently preparatory - will be used when we implement frame batching
    #[inline(always)]
    #[allow(dead_code)] // Preparatory for future batch processing optimization
    pub fn prefetch_multiple_connections(conns: &DashMap<u32, Conn>, conn_nos: &[u32]) {
        for &conn_no in conn_nos.iter().take(4) {
            // Prefetch up to 4 connections
            prefetch_connection_lookup(conns, conn_no);
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
    debug_hot_path!(
        channel_id = %channel.channel_id,
        conn_no = frame.connection_no,
        payload_len = frame.payload.len(),
        "handle_incoming_frame received frame"
    );

    if tracing::enabled!(tracing::Level::DEBUG) && frame.payload.len() <= 100 {
        debug!(channel_id = %channel.channel_id, payload = ?frame.payload, "Frame payload");
    } else if tracing::enabled!(tracing::Level::DEBUG) && frame.payload.len() > 100 {
        let first_bytes = &frame.payload[..std::cmp::min(50, frame.payload.len())];
        debug!(channel_id = %channel.channel_id, first_bytes = ?first_bytes, "Large frame first bytes");
    }

    // **BRANCH PREDICTION OPTIMIZATION**: Connection 1 is ULTRA HOT PATH (90%+ of data)
    if likely(frame.connection_no == 1) {
        // **ULTRA HOT PATH**: Connection 1 - main data traffic (most optimized)
        let conn_no = frame.connection_no;
        debug_hot_path!(
            channel_id = %channel.channel_id,
            conn_no = conn_no,
            "🔥 ULTRA HOT PATH: Connection 1 main traffic"
        );

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
        // **CONTROL PATH**: Connection 0 - control messages
        debug_hot_path!(channel_id = %channel.channel_id, "Handling control frame");
        handle_control(channel, frame).await?;
    } else if frame.connection_no > 1 {
        // **WARM PATH**: Other connections - short-lived traffic
        let conn_no = frame.connection_no;
        debug_hot_path!(
            channel_id = %channel.channel_id,
            conn_no = conn_no,
            "Routing short-lived connection frame"
        );

        // **OPTIMIZED**: Regular data path for other connections
        forward_to_protocol(channel, conn_no, frame.payload).await?;
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
    if unlikely(frame.payload.len() < CTRL_NO_LEN) {
        return Err(anyhow!("Malformed control frame"));
    }

    let code = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
    let cmd = ControlMessage::try_from(code)?;
    let data_bytes = frame.payload.slice(CTRL_NO_LEN..);

    // Log the control message for debugging
    debug_hot_path!(
        channel_id = %channel.channel_id,
        message_type = ?cmd,
        "Processing control message"
    );

    // Use the channel's control message handling methods
    match channel.process_control_message(cmd, &data_bytes).await {
        Ok(_) => {
            debug_hot_path!(
                channel_id = %channel.channel_id,
                message_type = ?cmd,
                "Successfully processed control message"
            );
            Ok(())
        }
        Err(e) => {
            error!(
                channel_id = %channel.channel_id,
                message_type = ?cmd,
                error = %e,
                "Error processing control message"
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
    let payload_len = payload.len(); // Store length before moving

    debug_hot_path!(
        channel_id = %channel.channel_id,
        conn_no = conn_no,
        payload_len = payload_len,
        "Forwarding bytes via lock-free channel"
    );

    if tracing::enabled!(tracing::Level::DEBUG) && payload_len > 0 && payload_len <= 100 {
        debug!(channel_id = %channel.channel_id, payload = ?payload, "Payload for backend");
    } else if tracing::enabled!(tracing::Level::DEBUG) && payload_len > 100 {
        let first_bytes = payload.slice(..std::cmp::min(50, payload_len));
        debug!(channel_id = %channel.channel_id, first_bytes = ?first_bytes, "Large payload first bytes");
    }

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

    // **BRANCH PREDICTION**: Connection exists is the most likely case
    let connection_exists = {
        // **OPTIMIZATION**: For 2-connection pattern, optimize lookups
        if likely(conn_no <= 1) {
            // **FAST PATH**: Connections 0 and 1 are persistent, very likely to exist
            likely(channel.conns.contains_key(&conn_no))
        } else {
            // **VARIABLE PATH**: Short-lived connections, existence is less predictable
            channel.conns.contains_key(&conn_no)
        }
    };

    if connection_exists {
        // **HOT PATH**: Connection exists - most common case
        let send_result = if let Some(conn_ref) = channel.conns.get(&conn_no) {
            // Send data to the connection's dedicated task (lock-free!)
            match conn_ref
                .data_tx
                .send(crate::models::ConnectionMessage::Data(payload))
            {
                Ok(_) => {
                    debug_hot_path!(
                        channel_id = %channel.channel_id,
                        conn_no = conn_no,
                        payload_len = payload_len,
                        "Successfully queued bytes for backend task"
                    );
                    Some(Ok(()))
                }
                Err(_) => {
                    // **COLD PATH**: Channel closed - rare error case
                    warn_hot_path!(
                        channel_id = %channel.channel_id,
                        conn_no = conn_no,
                        "Backend task is dead, closing connection"
                    );
                    Some(Err(anyhow!(
                        "Backend task for connection {} is dead",
                        conn_no
                    )))
                }
            }
        } else {
            None
        };

        // Handle the result after dropping all DashMap references
        match send_result {
            Some(Ok(())) => return Ok(()),
            Some(Err(e)) => {
                // Now we can safely call close_backend without borrow issues
                channel
                    .close_backend(conn_no, CloseConnectionReason::ConnectionLost)
                    .await?;
                return Err(e);
            }
            None => {
                // Connection disappeared between contains_key and get
            }
        }
    }

    // **COLD PATH**: Connection not found - should be rare in normal operation
    #[cold]
    fn log_connection_not_found(channel_id: &str, conn_no: u32) {
        warn_hot_path!(
            channel_id = %channel_id,
            conn_no = conn_no,
            "Connection not found for forwarding data, data lost"
        );
    }

    log_connection_not_found(&channel.channel_id, conn_no);
    Ok(())
}
