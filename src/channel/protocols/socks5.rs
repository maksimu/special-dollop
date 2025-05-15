use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use crate::models::{ProtocolHandler, NetworkAccessChecker};
use bytes::{Bytes, BytesMut, BufMut};
use log::debug;
use crate::webrtc_data_channel::WebRTCDataChannel;
use crate::buffer_pool::BufferPool;
use serde_json;
use crate::tube_and_channel_helpers;

const SOCKS5_VERSION: u8 = 5;

// SOCKS5 Command Codes
const CONNECT: u8 = 1;
const BIND: u8 = 2;
const UDP_ASSOCIATE: u8 = 3;

// SOCKS5 Address Types
const IPV4: u8 = 1;
const DOMAIN_NAME: u8 = 3;
const IPV6: u8 = 4;

// SOCKS5 Reply Codes
const SUCCEEDED: u8 = 0;
const GENERAL_FAILURE: u8 = 1;
const CONNECTION_NOT_ALLOWED: u8 = 2;
const NETWORK_UNREACHABLE: u8 = 3;
const HOST_UNREACHABLE: u8 = 4;
const CONNECTION_REFUSED: u8 = 5;
const TTL_EXPIRED: u8 = 6;
const COMMAND_NOT_SUPPORTED: u8 = 7;
const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 8;

// SOCKS5 Auth Methods
const NO_AUTH: u8 = 0;
const NO_ACCEPTABLE_METHODS: u8 = 0xFF;

// Connection state for server mode
#[derive(Debug, PartialEq, Eq)]
enum LocalServerState {
    Initial,              // Waiting for client greeting
    AuthNegotiated,       // Authentication method selected
    ConnectionRequested,  // Client has sent a connection request
    Connected,            // Connection established
    Failed(String),       // Connection failed with an error
}

pub struct SOCKS5Handler {
    active_connections: Arc<Mutex<HashMap<u32, TcpStream>>>,
    network_checker: Option<NetworkAccessChecker>,
    server_state: LocalServerState,
    current_connection: Option<u32>,
    next_connection_id: u32,
    // Buffer for protocol frames
    buffer: BytesMut,
    webrtc_channel: Option<WebRTCDataChannel>,
    // Buffer pool for efficient memory usage
    buffer_pool: BufferPool,
}

impl SOCKS5Handler {
    pub fn new(
        settings: HashMap<String, serde_json::Value>,
    ) -> Self {
        // Use NetworkAccessChecker for checking hosts and ports
        let network_checker = tube_and_channel_helpers::parse_network_rules_from_settings(&settings);
            
        Self {
            active_connections: Arc::new(Mutex::new(HashMap::new())),
            network_checker,
            server_state: LocalServerState::Initial,
            current_connection: None,
            next_connection_id: 1,
            buffer: BytesMut::new(),
            webrtc_channel: None,
            buffer_pool: BufferPool::default(),
        }
    }

    // ===== Server Mode Methods =====
    // These handle a local SOCKS5 server accepting client connections

    /// Process the initial client greeting and authentication negotiation
    async fn handle_client_greeting(&mut self, data: &[u8]) -> Result<Bytes> {
        if data.len() < 2 {
            self.server_state = LocalServerState::Failed("Invalid SOCKS request (too short)".to_string());
            return Err(anyhow!("Invalid SOCKS request: too short"));
        }

        let socks_version = data[0];
        let n_methods = data[1] as usize;

        if socks_version != SOCKS5_VERSION {
            self.server_state = LocalServerState::Failed("Invalid SOCKS version".to_string());
            // Return error response as per SOCKS5 spec
            return Ok(Bytes::from(vec![SOCKS5_VERSION, 0xFF]));
        }

        if data.len() < 2 + n_methods {
            self.server_state = LocalServerState::Failed("Invalid SOCKS request (not enough methods)".to_string());
            return Err(anyhow!("Invalid SOCKS request: not enough method bytes"));
        }

        let method_ids = &data[2..2 + n_methods];
        
        // Currently, we only support NO_AUTH (0)
        let selected_method = if method_ids.contains(&NO_AUTH) {
            NO_AUTH
        } else {
            NO_ACCEPTABLE_METHODS
        };

        // Return the selected method
        let mut buf = self.buffer_pool.acquire();
        buf.clear();
        buf.put_u8(SOCKS5_VERSION);
        buf.put_u8(selected_method);
        
        if selected_method == NO_AUTH {
            self.server_state = LocalServerState::AuthNegotiated;
        } else {
            self.server_state = LocalServerState::Failed("No acceptable auth methods".to_string());
        }

        Ok(buf.freeze())
    }

    /// Process the client's connection request
    async fn handle_connection_request(&mut self, data: &[u8]) -> Result<Bytes> {
        if data.len() < 4 {
            self.server_state = LocalServerState::Failed("Invalid connection request (too short)".to_string());
            return Ok(self.build_error_response(GENERAL_FAILURE));
        }

        let version = data[0];
        let cmd = data[1];
        let _reserved = data[2]; // Reserved byte should be 0
        let address_type = data[3];

        if version != SOCKS5_VERSION {
            self.server_state = LocalServerState::Failed("Invalid SOCKS version in request".to_string());
            return Ok(self.build_error_response(GENERAL_FAILURE));
        }

        if cmd != CONNECT {
            // We only support the CONNECT command
            self.server_state = LocalServerState::Failed("Command not supported".to_string());
            return Ok(self.build_error_response(COMMAND_NOT_SUPPORTED));
        }

        // Parse the destination address based on the address type
        let (host, port) = match address_type {
            IPV4 => {
                if data.len() < 10 {
                    self.server_state = LocalServerState::Failed("Invalid IPv4 address".to_string());
                    return Ok(self.build_error_response(GENERAL_FAILURE));
                }
                let ip = Ipv4Addr::new(data[4], data[5], data[6], data[7]);
                let port = u16::from_be_bytes([data[8], data[9]]);
                (ip.to_string(), port)
            }
            DOMAIN_NAME => {
                if data.len() < 5 {
                    self.server_state = LocalServerState::Failed("Invalid domain name".to_string());
                    return Ok(self.build_error_response(GENERAL_FAILURE));
                }
                let len = data[4] as usize;
                if data.len() < 5 + len + 2 {
                    self.server_state = LocalServerState::Failed("Invalid domain name".to_string());
                    return Ok(self.build_error_response(GENERAL_FAILURE));
                }
                let domain = String::from_utf8_lossy(&data[5..5 + len]).to_string();
                let port_idx = 5 + len;
                let port = u16::from_be_bytes([data[port_idx], data[port_idx + 1]]);
                (domain, port)
            }
            IPV6 => {
                if data.len() < 22 {
                    self.server_state = LocalServerState::Failed("Invalid IPv6 address".to_string());
                    return Ok(self.build_error_response(GENERAL_FAILURE));
                }
                let segments = [
                    u16::from_be_bytes([data[4], data[5]]),
                    u16::from_be_bytes([data[6], data[7]]),
                    u16::from_be_bytes([data[8], data[9]]),
                    u16::from_be_bytes([data[10], data[11]]),
                    u16::from_be_bytes([data[12], data[13]]),
                    u16::from_be_bytes([data[14], data[15]]),
                    u16::from_be_bytes([data[16], data[17]]),
                    u16::from_be_bytes([data[18], data[19]]),
                ];
                let ip = Ipv6Addr::new(
                    segments[0], segments[1], segments[2], segments[3],
                    segments[4], segments[5], segments[6], segments[7],
                );
                let port = u16::from_be_bytes([data[20], data[21]]);
                (ip.to_string(), port)
            }
            _ => {
                self.server_state = LocalServerState::Failed("Address type not supported".to_string());
                return Ok(self.build_error_response(ADDRESS_TYPE_NOT_SUPPORTED));
            }
        };

        // Check if host and port are allowed
        if !self.is_host_allowed(&host) {
            self.server_state = LocalServerState::Failed(format!("Host not allowed: {}", host));
            return Ok(self.build_error_response(CONNECTION_NOT_ALLOWED));
        }

        if !self.is_port_allowed(port) {
            self.server_state = LocalServerState::Failed(format!("Port not allowed: {}", port));
            return Ok(self.build_error_response(CONNECTION_NOT_ALLOWED));
        }

        // Store target details
        self.server_state = LocalServerState::ConnectionRequested;

        // The WebRTC channel will handle the actual connection establishment
        // For now, we return a success response to the client
        let conn_id = self.next_connection_id;
        self.next_connection_id += 1;
        
        // Build success response
        // VER, REP, RSV, ATYP, BND.ADDR, BND.PORT
        let mut response = vec![SOCKS5_VERSION, SUCCEEDED, 0, IPV4];
        // Use a generic IP address for the bound address (0.0.0.0)
        response.extend_from_slice(&[0, 0, 0, 0]);
        response.extend_from_slice(&port.to_be_bytes()); // Use the same port
        
        self.server_state = LocalServerState::Connected;
        self.current_connection = Some(conn_id);
        
        Ok(Bytes::from(response))
    }

    // ===== Utility Methods =====

    // Build a standard SOCKS5 error response
    fn build_error_response(&self, error_code: u8) -> Bytes {
        let mut response = self.buffer_pool.acquire();
        response.clear();
        response.put_u8(SOCKS5_VERSION);
        response.put_u8(error_code);
        response.put_u8(0x00); // RSV
        response.put_u8(0x01); // ATYP: IPv4
        response.put_u32(0);   // IP: 0.0.0.0
        response.put_u16(0);   // PORT: 0
        response.freeze()
    }

    // Check if a host is allowed by network rules
    fn is_host_allowed(&self, host: &str) -> bool {
        if let Some(checker) = &self.network_checker {
            checker.is_host_allowed(host)
        } else {
            // If no network checker is set, allow all hosts
            true
        }
    }

    // Check if port is allowed by network rules
    fn is_port_allowed(&self, port: u16) -> bool {
        if let Some(checker) = &self.network_checker {
            checker.is_port_allowed(port)
        } else {
            // If no network checker is set, allow all ports
            true
        }
    }

    // Generate the connection open payload for sending over WebRTC tunnel
    // This format matches what the Python code sends in the open_connection method
    pub fn build_connection_open_data(&self, conn_id: u32, target_host: String, target_port: u16,) -> Option<Bytes> {
        let mut buf = self.buffer_pool.acquire();
        buf.clear();

        // Connection ID (4 bytes)
        buf.put_u32(conn_id);

        // Host length (4 bytes) and host bytes
        let host_bytes = target_host.as_bytes();
        buf.put_u32(host_bytes.len() as u32);
        buf.extend_from_slice(host_bytes);

        // Port (2 bytes)
        buf.put_u16(target_port);

        Some(buf.freeze())
    }
}

impl ProtocolHandler for SOCKS5Handler {
    fn initialize(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            self.server_state = LocalServerState::Initial;
            Ok(())
        })
    }

    fn process_client_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            // Add new data to the buffer
            self.buffer.extend_from_slice(data);
            
            // Process according to current state
            match self.server_state {
                LocalServerState::Initial => {
                    // Handle initial SOCKS5 greeting
                    self.handle_client_greeting(&data).await
                }
                LocalServerState::AuthNegotiated => {
                    // Handle connection request
                    self.handle_connection_request(&data).await
                }
                LocalServerState::ConnectionRequested => {
                    // Handle connection request
                    self.handle_connection_request(&data).await
                }
                LocalServerState::Connected => {
                    // Forward data to the server via WebRTC
                    if let Some(webrtc) = &self.webrtc_channel {
                        // Create a data frame with the received data
                        use crate::protocol::Frame;
                        let frame = Frame::new_data_with_pool(conn_no, data, &self.buffer_pool);
                        let encoded = frame.encode_with_pool(&self.buffer_pool);
                        
                        debug!("SOCKS5 handler: Forwarding {} bytes to server for connection {}", 
                              data.len(), conn_no);
                        webrtc.send(encoded).await.map_err(|e| anyhow!("Failed to send data: {}", e))?;
                        
                        // Return empty bytes as we've already forwarded the data
                        Ok(Bytes::new())
                    } else {
                        debug!("SOCKS5 handler: WebRTC channel not available for connection {}", conn_no);
                        Ok(Bytes::copy_from_slice(data))
                    }
                }
                LocalServerState::Failed(ref reason) => {
                    // Failed state - return error
                    Err(anyhow!("Connection {} failed: {}", conn_no,reason))
                }
            }
        })
    }

    fn process_server_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            // Send server data back through the WebRTC channel
            if let Some(webrtc) = &self.webrtc_channel {
                // Create a data frame with the received data
                use crate::protocol::Frame;
                let frame = Frame::new_data_with_pool(conn_no, data, &self.buffer_pool);
                let encoded = frame.encode_with_pool(&self.buffer_pool);
                
                // Send the data through the WebRTC channel
                debug!("SOCKS5 handler: Forwarding {} bytes from server for connection {}", 
                      data.len(), conn_no);
                webrtc.send(encoded).await.map_err(|e| anyhow!("Failed to send data: {}", e))?;
                
                // Return empty bytes as we've already forwarded the data
                Ok(Bytes::new())
            } else {
                debug!("SOCKS5 handler: WebRTC channel not available for connection {}", conn_no);
                
                // If no WebRTC channel is available, just return the data as-is
                Ok(Bytes::copy_from_slice(data))
            }
        })
    }

    fn handle_command<'a>(&'a mut self, command: &'a str, params: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match command {
                "set_state" => {
                    if let Ok(state_str) = std::str::from_utf8(params) {
                        match state_str {
                            "initial" => self.server_state = LocalServerState::Initial,
                            "authenticated" => self.server_state = LocalServerState::AuthNegotiated,
                            "conn_requested" => self.server_state = LocalServerState::ConnectionRequested,
                            "connected" => self.server_state = LocalServerState::Connected,
                            _ => {}
                        }
                    }
                    Ok(())
                }
                "disconnect" => {
                    // Reset state
                    self.server_state = LocalServerState::Initial;
                    self.current_connection = None;
                    Ok(())
                }
                _ => Ok(())
            }
        })
    }
    
    fn handle_new_connection(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Register the connection
            self.current_connection = Some(conn_no);
            debug!("SOCKS5 handler: new connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_eof(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Mark EOF received
            debug!("SOCKS5 handler: EOF on connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_close(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Clean up the connection
            if self.current_connection == Some(conn_no) {
                self.current_connection = None;
                self.server_state = LocalServerState::Initial;
            }
            debug!("SOCKS5 handler: closed connection {}", conn_no);
            Ok(())
        })
    }
    
    fn set_webrtc_channel(&mut self, channel: WebRTCDataChannel) {
        self.webrtc_channel = Some(channel);
    }

    fn on_webrtc_channel_state_change(&mut self, state: &str) -> BoxFuture<'_, Result<()>> {
        let state_copy = state.to_string(); // Copy the state to avoid lifetime issues
        Box::pin(async move {
            debug!("SOCKS5 handler: WebRTC channel state changed to {}", state_copy);
            
            if state_copy == "open" {
                if let Some(webrtc) = &self.webrtc_channel {
                    if webrtc.ready_state() == "open" {
                        debug!("SOCKS5 handler: WebRTC channel is open");
                        
                        // Could process any pending messages here if we implemented queuing
                        // for this handler in the future
                    }
                }
            }
            
            Ok(())
        })
    }

    fn process_pending_messages(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // SOCKS5 handler currently doesn't queue messages, but we could add that
            // functionality in the future if needed. For now, just return Ok.
            debug!("SOCKS5 handler: process_pending_messages called (no-op for this handler)");
            Ok(())
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            // Reset to the initial state
            self.server_state = LocalServerState::Initial;
            self.current_connection = None;
            Ok(())
        })
    }

    fn status(&self) -> String {
        let state = match self.server_state {
            LocalServerState::Initial => "Initial",
            LocalServerState::AuthNegotiated => "Authenticated",
            LocalServerState::ConnectionRequested => "Connection Requested",
            LocalServerState::Connected => "Connected",
            LocalServerState::Failed(ref reason) => &format!("Failed: {}", reason),
        };
        
        let conn_info = if let Some(conn) = self.current_connection {
            format!("Connection: {}", conn)
        } else {
            "No active connection".to_string()
        };
        
        format!("SOCKS5 Handler: {} - {}", state, conn_info)
    }
} 