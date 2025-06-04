// Frame handling functionality for Channel

use anyhow::{anyhow, Result};
use crate::tube_protocol::{Frame, ControlMessage, CloseConnectionReason, CTRL_NO_LEN};
use tracing::{debug, error};
use super::core::Channel;
use bytes::Bytes;
use crate::{debug_hot_path, warn_hot_path}; // Import centralized hot path macros

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
    
    match frame.connection_no {
        0 => {
            debug_hot_path!(channel_id = %channel.channel_id, "Handling control frame");
            handle_control(channel, frame).await?;
        }
        conn_no => {
            // All non-control frames go to the lock-free protocol handler
            debug_hot_path!(
                channel_id = %channel.channel_id,
                conn_no = conn_no,
                "Routing frame to lock-free protocol handler"
            );
            forward_to_protocol(channel, conn_no, frame.payload).await?;
        }
    }
    
    Ok(())
}

// Handle control frames
pub async fn handle_control(channel: &mut Channel, frame: Frame) -> Result<()> {
    if frame.payload.len() < CTRL_NO_LEN {
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
        },
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

    // **COMPLETELY LOCK-FREE**: DashMap provides efficient concurrent access
    let send_result = if let Some(conn_ref) = channel.conns.get(&conn_no) {
        // Send data to the connection's dedicated task (lock-free!)
        match conn_ref.data_tx.send(crate::models::ConnectionMessage::Data(payload)) {
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
                // The Channel is closed, meaning the backend task died
                warn_hot_path!(
                    channel_id = %channel.channel_id,
                    conn_no = conn_no,
                    "Backend task is dead, closing connection"
                );
                Some(Err(anyhow!("Backend task for connection {} is dead", conn_no)))
            }
        }
    } else {
        warn_hot_path!(
            channel_id = %channel.channel_id,
            conn_no = conn_no,
            "Connection not found for forwarding data, data lost"
        );
        None
    };
    
    // Handle the result after dropping all DashMap references
    match send_result {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => {
            // Now we can safely call close_backend without borrow issues
            channel.close_backend(conn_no, CloseConnectionReason::ConnectionLost).await?;
            Err(e)
        }
        None => Ok(()),
    }
}