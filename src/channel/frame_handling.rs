// Frame handling functionality for Channel

use anyhow::{anyhow, Result};
use crate::tube_protocol::{Frame, ControlMessage, CloseConnectionReason, CTRL_NO_LEN};
use tracing::{debug, error, warn};
use super::core::Channel;
use bytes::Bytes;
use tokio::io::AsyncWriteExt;

// Central dispatcher for incoming frames
// **BOLD WARNING: HOT PATH - CALLED FOR EVERY INCOMING FRAME**
// **NO STRING ALLOCATIONS IN DEBUG LOGS UNLESS ENABLED**
pub async fn handle_incoming_frame(channel: &mut Channel, frame: Frame) -> Result<()> {
    if tracing::enabled!(tracing::Level::DEBUG) {
        debug!("Channel({}): handle_incoming_frame received frame on connection {}, payload length: {}, timestamp: {}", 
                 channel.channel_id, frame.connection_no, frame.payload.len(), frame.timestamp_ms);
        
        if frame.payload.len() <= 100 {
            debug!("Channel({}): Frame payload: {:?}", channel.channel_id, frame.payload);
        } else {
            debug!("Channel({}): First 50 bytes of frame payload: {:?}", 
                     channel.channel_id, &frame.payload[..std::cmp::min(50, frame.payload.len())]);
        }
    }
    
    match frame.connection_no {
        0 => {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!("Channel({}): Handling control frame", channel.channel_id);
            }
            handle_control(channel, frame).await?;
        }
        conn_no => {
            // All non-control frames go to the single protocol handler
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!("Channel({}): Routing frame to protocol handler for connection {}", 
                         channel.channel_id, conn_no);
            }
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
    if tracing::enabled!(tracing::Level::DEBUG) {
        debug!("Channel({}): Processing control message: {:?}", channel.channel_id, cmd);
    }
    
    // Use the channel's control message handling methods
    match channel.process_control_message(cmd, &data_bytes).await {
        Ok(_) => {
            if tracing::enabled!(tracing::Level::DEBUG) {
                debug!("Channel({}): Successfully processed control message: {:?}", 
                       channel.channel_id, cmd);
            }
            Ok(())
        },
        Err(e) => {
            error!("Channel({}): Error processing control message {:?}: {}", 
                   channel.channel_id, cmd, e);
            Err(e)
        }
    }
}

// Forward data to the appropriate protocol handler
// **BOLD WARNING: HOT PATH - CALLED FOR EVERY DATA FRAME**
// **NO STRING ALLOCATIONS IN DEBUG LOGS UNLESS ENABLED**
// **WARNING: LOCK ACQUISITION ON EVERY CALL - CONSIDER LOCK-FREE ALTERNATIVES**
async fn forward_to_protocol(channel: &mut Channel, conn_no: u32, payload: Bytes) -> Result<()> {
    if tracing::enabled!(tracing::Level::DEBUG) {
        debug!("Channel({}): Forwarding {} bytes for connection {} directly to backend TCP", 
               channel.channel_id, payload.len(), conn_no);
        
        if payload.len() > 0 {
            if payload.len() <= 100 {
                debug!("Channel({}): Payload for backend: {:?}", channel.channel_id, payload);
            } else {
                debug!("Channel({}): First 50 bytes of payload for backend: {:?}", 
                     channel.channel_id, &payload.slice(..std::cmp::min(50, payload.len())));
            }
        }
    }

    let mut conns_guard = channel.conns.lock().await;
    if let Some(conn) = conns_guard.get_mut(&conn_no) {
        let backend_writer = conn.backend.as_mut();

        match backend_writer.write_all(payload.as_ref()).await {
            Ok(_) => {
                if let Err(flush_err) = backend_writer.flush().await {
                    warn!("Channel({}): Error flushing backend for conn_no {}: {}. Client might have disconnected.", channel.channel_id, conn_no, flush_err);
                }
                if tracing::enabled!(tracing::Level::DEBUG) {
                    debug!("Channel({}): Successfully wrote {} bytes to backend for conn_no {}", channel.channel_id, payload.len(), conn_no);
                }
            }
            Err(e) => {
                error!("Channel({}): Error writing to backend for conn_no {}: {}. Closing connection.", channel.channel_id, conn_no, e);
                drop(conns_guard);
                channel.close_backend(conn_no, CloseConnectionReason::ConnectionLost).await?;
                return Err(e.into());
            }
        }
    } else {
        drop(conns_guard);
        warn!("Channel({}): Connection {} not found for forwarding data, data lost.", channel.channel_id, conn_no);
    }
    
    Ok(())
}
