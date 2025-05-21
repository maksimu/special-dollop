// Utility functions for Channel implementation

use anyhow::Result;
use crate::tube_protocol::{ControlMessage, Frame};
use crate::error::ChannelError;
use tracing::{debug, error};

use super::core::Channel;

// Helper method to handle ping timeout check
pub(crate) async fn handle_ping_timeout(channel: &mut Channel) -> Result<(), ChannelError> {
    channel.ping_attempt += 1;
    if channel.ping_attempt > 10 {
        error!(
            target: "channel_health", 
            channel_id = %channel.channel_id, 
            ping_attempts = channel.ping_attempt, 
            "Too many ping timeouts, closing channel"
        );
        channel.close_backend(0, crate::tube_protocol::CloseConnectionReason::Timeout).await?;
        return Err(ChannelError::Timeout(
            format!("Too many ping timeouts for endpoint {}", channel.channel_id)
        ));
    }

    if channel.is_connected {
       debug!(target: "channel_health", channel_id = %channel.channel_id, ping_attempt = channel.ping_attempt, "Send ping request");
        let timestamp = crate::channel::protocol::now_ms();
        let timestamp_bytes = timestamp.to_be_bytes();  // Convert to big endian bytes
        let length = timestamp_bytes.len() as u32;      // Get the length
        let length_bytes = length.to_be_bytes();
        // Combine the bytes into a single Vec
        let mut combined = Vec::new();
        combined.extend_from_slice(&length_bytes);
        combined.extend_from_slice(&timestamp_bytes);
        // Convert length to bytes

        // Now you can use both timestamp_bytes and length_bytes as needed
        // Build ping payload
        let frame = Frame::new_control_with_pool(
            ControlMessage::Ping,
            &*combined,
            &channel.buffer_pool
        );
        let encoded = frame.encode_with_pool(&channel.buffer_pool);
        channel.webrtc.send(encoded).await.map_err(|e| ChannelError::WebRTCError(e.to_string()))?;
    }
    
    Ok(())
}
