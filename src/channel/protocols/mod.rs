// Protocol handlers for channel communication
mod socks5;
pub mod guacd;
mod port_forward;
pub mod guacd_parser;

pub use socks5::SOCKS5Handler;
pub use guacd::GuacdHandler;
// Re-export types for use elsewhere
pub use guacd_parser::{GuacdParser, InstructionObject, GuacdError};

use crate::models::{is_guacd_session, ConversationType, ProtocolHandler};
use std::collections::HashMap;
use std::str::FromStr;
use anyhow::{anyhow, Result};
use log::debug;
use self::port_forward::PortForwardProtocolHandler;

// Factory method to create protocol handlers
pub fn create_protocol_handler(
    server_mode: bool,
    settings: HashMap<String, serde_json::Value>,
) -> Result<Box<dyn ProtocolHandler + Send + Sync>> {
    // Extract the conversation type string from settings with a default
    let conversation_type_str = settings
        .get("conversationType")
        .and_then(|v| v.as_str())
        .unwrap_or("tunnel");
    
    let conversation_type = match ConversationType::from_str(conversation_type_str) {
        Ok(ct) => ct,
        Err(_) => {
            debug!("Invalid conversation type: {}, defaulting to Tunnel", conversation_type_str);
            ConversationType::Tunnel
        }
    };
    
    debug!("Creating protocol handler for: {}", conversation_type);
    
    match conversation_type {
        ConversationType::Tunnel => {
            if server_mode && (settings.contains_key("allowed_hosts") || settings.contains_key("allowed_ports")){
                // SOCKS5 handler for the local server
                let handler = SOCKS5Handler::new(settings);
                Ok(Box::new(handler) as Box<dyn ProtocolHandler + Send + Sync>)
            } else {
                // Use the port-forwarding handler
                let handler = PortForwardProtocolHandler::new();
                Ok(Box::new(handler) as Box<dyn ProtocolHandler + Send + Sync>)
            }

        }
        _ => {
            if is_guacd_session(&conversation_type){
                let client_id = settings.get("client_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| anyhow!("Missing client_id in settings for guacd handler"))?;

                let handler = GuacdHandler::new(client_id, settings);
                Ok(Box::new(handler) as Box<dyn ProtocolHandler + Send + Sync>)
            } else {
                Err(anyhow!("Unknown protocol type: {}", conversation_type_str))
            }
        }
    }
} 