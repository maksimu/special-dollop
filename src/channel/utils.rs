// Utility functions for Channel implementation

use anyhow::Result;
use bytes::Bytes;
use crate::protocol::ControlMessage;
use crate::error::ChannelError;

use super::core::Channel;

// Helper method to handle ping timeout check
pub async fn handle_ping_timeout(channel: &mut Channel) -> Result<(), ChannelError> {
    if channel.ping_attempt > 5 {
        log::error!("Endpoint {}: Too many ping timeouts ({}/5)", 
               channel.channel_id, channel.ping_attempt);
        channel.is_connected = false;
        channel.should_exit = true;
        return Err(ChannelError::Timeout(
            format!("Too many ping timeouts for endpoint {}", channel.channel_id)
        ));
    }

    if channel.is_connected {
       log::debug!("Endpoint {}: Send ping request", channel.channel_id);
        
        // Build ping payload
        let buffer = Bytes::copy_from_slice(&0u32.to_be_bytes()[..]);
        
        channel.send_control_message(ControlMessage::Ping, &buffer).await?;
        channel.ping_attempt += 1;
    }
    
    Ok(())
}
