use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use crate::models::ProtocolHandler;
use log::debug;
use bytes::{Bytes, BytesMut};
use crate::models::EMPTY_BYTES;
use crate::webrtc_data_channel::WebRTCDataChannel;
use std::collections::HashSet;
use crate::buffer_pool::BufferPool;
use std::str;
use serde_json;
use crate::GuacdParser;

// Default guacd server port
const DEFAULT_GUACD_PORT: u16 = 4822;

pub struct GuacdHandler {
    guacd_connection: Option<TcpStream>,
    client_id: String,
    connection_settings: HashMap<String, serde_json::Value>,
    state: GuacdState,
    parser: GuacdParser,
    // Active connections
    active_connections: HashSet<u32>,
    // WebRTC channel for sending data
    webrtc: Option<WebRTCDataChannel>,
    // Buffer pool for memory management
    buffer_pool: Option<BufferPool>,
}

#[derive(Debug)]
enum GuacdState {
    Initial,
    Connected,
    Handshaking,
    Ready,
    Failed(String),
}

impl GuacdHandler {
    pub fn new(client_id: String, connection_settings: HashMap<String, serde_json::Value>) -> Self {
        // Use client_id to generate a unique connection number or get it from settings
        let _connection_no = if let Some(conn_no_str) = connection_settings.get("connection_no").and_then(|v| v.as_str()) {
            conn_no_str.parse::<u32>().unwrap_or(1)
        } else {
            // Use a hash of the client_id or another method to generate a unique number
            // This is just a simple example - in production you'd want a better way to generate unique IDs
            let mut hash = 0u32;
            for byte in client_id.bytes() {
                hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
            }
            if hash == 0 {
                // Connection 0 is reserved for control messages
                1
            } else {
                hash
            }
        };

        // Create a buffer pool
        let buffer_pool = Some(BufferPool::default());
        
        // Create parser with buffer pool
        let parser = if let Some(pool) = &buffer_pool {
            GuacdParser::with_buffer_pool(pool.clone())
        } else {
            GuacdParser::new()
        };

        Self {
            guacd_connection: None,
            client_id,
            connection_settings,
            state: GuacdState::Initial,
            parser,
            active_connections: HashSet::new(),
            webrtc: None,
            buffer_pool,
        }
    }

    fn initialize(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            debug!("GuacdHandler: Initializing handler with client_id: {}", self.client_id);
            // Print the settings we have
            debug!("GuacdHandler: Connection settings:");
            for (key, value) in &self.connection_settings {
                debug!("  - {}: {}", key, value);
            }
            
            // Connect to guacd server during initialization
            self.connect_to_guacd().await
        })
    }

    async fn connect_to_guacd(&mut self) -> Result<()> {
        let host = self.connection_settings.get("host")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "localhost".to_string());
            
        let port = self.connection_settings.get("port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16)
            .unwrap_or(DEFAULT_GUACD_PORT);
        
        // Try to get destination from settings (for testing)
        let destination = self.connection_settings.get("destination")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        
        let addr = if let Some(dest) = destination {
            debug!("GuacdHandler: Using destination from settings: {}", dest);
            dest
        } else {
            debug!("GuacdHandler: Using host:port from settings: {}:{}", host, port);
            format!("{}:{}", host, port)
        };
        
        debug!("GuacdHandler: Connecting to guacd at {}", addr);
        
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                debug!("GuacdHandler: Connected to guacd successfully");
                self.guacd_connection = Some(stream);
                self.state = GuacdState::Connected;
                debug!("GuacdHandler: Starting handshake");
                match self.perform_handshake().await {
                    Ok(_) => {
                        debug!("GuacdHandler: Handshake completed successfully");
                        Ok(())
                    },
                    Err(e) => {
                        debug!("GuacdHandler: Handshake failed: {}", e);
                        self.state = GuacdState::Failed(format!("Handshake failed: {}", e));
                        Err(anyhow!("Failed to perform handshake: {}", e))
                    }
                }
            }
            Err(e) => {
                debug!("GuacdHandler: Failed to connect to guacd: {}", e);
                self.state = GuacdState::Failed(format!("Failed to connect to guacd: {}", e));
                Err(anyhow!("Failed to connect to guacd: {}", e))
            }
        }
    }

    async fn perform_handshake(&mut self) -> Result<()> {
        debug!("Performing handshake");
        if let Some(ref mut conn) = self.guacd_connection {
            self.state = GuacdState::Handshaking;

            // Basic handshake process based on guacamole protocol
            let protocol = self.connection_settings.get("protocol")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "rdp".to_string());

            // 1. Send 'select' instruction with protocol
            let handshake = GuacdParser::guacd_encode("select", &[&protocol], self.buffer_pool.as_ref());
            conn.write_all(&handshake).await?;

            // 2. Read args instruction response
            let mut buffer = if let Some(pool) = &self.buffer_pool {
                pool.acquire()
            } else {
                BytesMut::with_capacity(4096)
            };

            let n = conn.read_buf(&mut buffer).await?;

            if n > 0 {
                // Process handshake response with the parser
                match self.parser.receive(0, &buffer[..n]) {
                    Ok(_) => {
                        // Get the instruction from the parser
                        if let Some(instruction) = self.parser.get_instruction() {
                            debug!("Received instruction: {:?}", instruction);

                            if instruction.opcode != "args" {
                                log::error!("Expected 'args' instruction, received: {}", instruction.opcode);

                                // Return buffer to pool if using one
                                if let Some(pool) = &self.buffer_pool {
                                    pool.release(buffer);
                                }

                                self.state = GuacdState::Failed(format!("Expected 'args' instruction, received: {}", instruction.opcode));
                                return Err(anyhow!("Expected 'args' instruction"));
                            }

                            let expected_version = "VERSION_1_5_0";
                            let guacd_version = &instruction.args[0];

                            if guacd_version != expected_version {
                                log::info!("Guacd Protocol Version Number mismatch. Expected: {}, Received: {}", 
                                           expected_version, guacd_version);
                            }

                            // 3. Send size, audio, video settings if needed
                            // These could be configurable through connection_settings
                            conn.write_all(&GuacdParser::guacd_encode("size", &[], self.buffer_pool.as_ref())).await?;
                            conn.write_all(&GuacdParser::guacd_encode("audio", &[], self.buffer_pool.as_ref())).await?;
                            conn.write_all(&GuacdParser::guacd_encode("video", &[], self.buffer_pool.as_ref())).await?;
                            conn.write_all(&GuacdParser::guacd_encode("image", &[], self.buffer_pool.as_ref())).await?;

                            // 4. Map connection_settings to the requested arguments
                            let mut connection_args: Vec<String> = Vec::new();

                            // First arg is always the version
                            connection_args.push(expected_version.to_string());

                            // Map the rest of the arguments based on the args instruction
                            for i in 1..instruction.args.len() {
                                let arg_name = instruction.args[i].replace("-", "");
                                let value = self.connection_settings.get(&arg_name)
                                    .and_then(|v| match v {
                                        serde_json::Value::String(s) => Some(s.clone()),
                                        serde_json::Value::Number(n) => Some(n.to_string()),
                                        serde_json::Value::Bool(b) => Some(b.to_string()),
                                        _ => None
                                    })
                                    .unwrap_or_else(|| "".to_string());
                                connection_args.push(value);
                            }

                            // 5. Send connect instruction with connection arguments
                            let connect_args: Vec<&str> = connection_args.iter().map(|s| s.as_str()).collect();
                            conn.write_all(&GuacdParser::guacd_encode("connect", &connect_args, self.buffer_pool.as_ref())).await?;

                            // Return buffer to pool if using one
                            if let Some(pool) = &self.buffer_pool {
                                pool.release(buffer);
                            }

                            // 6. Read ready instruction with client ID
                            buffer = if let Some(pool) = &self.buffer_pool {
                                pool.acquire()
                            } else {
                                BytesMut::with_capacity(4096)
                            };

                            let n = conn.read_buf(&mut buffer).await?;

                            if n > 0 {
                                match self.parser.receive(0, &buffer[..n]) {
                                    Ok(_) => {
                                        if let Some(instruction) = self.parser.get_instruction() {
                                            if instruction.opcode != "ready" {
                                                log::error!("Expected 'ready' instruction, received: {}", instruction.opcode);

                                                // Return buffer to pool if using one
                                                if let Some(pool) = &self.buffer_pool {
                                                    pool.release(buffer);
                                                }

                                                self.state = GuacdState::Failed(format!("Expected 'ready' instruction, received: {}", instruction.opcode));
                                                return Err(anyhow!("Expected 'ready' instruction"));
                                            }

                                            // Store client ID if provided
                                            if !instruction.args.is_empty() {
                                                self.client_id = instruction.args[0].clone();
                                            }

                                            log::info!("Handshake completed. Connection established with client id: {}", self.client_id);
                                            self.state = GuacdState::Ready;

                                            // Return buffer to pool if using one
                                            if let Some(pool) = &self.buffer_pool {
                                                pool.release(buffer);
                                            }

                                            Ok(())
                                        } else {
                                            // Return buffer to pool if using one
                                            if let Some(pool) = &self.buffer_pool {
                                                pool.release(buffer);
                                            }

                                            self.state = GuacdState::Failed("Missing 'ready' instruction".to_string());
                                            Err(anyhow!("Missing 'ready' instruction"))
                                        }
                                    },
                                    Err(e) => {
                                        // Return buffer to pool if using one
                                        if let Some(pool) = &self.buffer_pool {
                                            pool.release(buffer);
                                        }

                                        self.state = GuacdState::Failed(format!("Failed to parse 'ready' response: {}", e));
                                        Err(anyhow!("Failed to parse 'ready' response: {}", e))
                                    }
                                }
                            } else {
                                // Return buffer to pool if using one
                                if let Some(pool) = &self.buffer_pool {
                                    pool.release(buffer);
                                }

                                self.state = GuacdState::Failed("Empty 'ready' response".to_string());
                                Err(anyhow!("Empty 'ready' response"))
                            }
                        } else {
                            // Return buffer to pool if using one
                            if let Some(pool) = &self.buffer_pool {
                                pool.release(buffer);
                            }

                            self.state = GuacdState::Failed("Missing 'args' instruction".to_string());
                            Err(anyhow!("Missing 'args' instruction"))
                        }
                    },
                    Err(e) => {
                        // Return buffer to pool if using one
                        if let Some(pool) = &self.buffer_pool {
                            pool.release(buffer);
                        }

                        self.state = GuacdState::Failed(format!("Failed to parse 'args' response: {}", e));
                        Err(anyhow!("Failed to parse 'args' response: {}", e))
                    }
                }
            } else {
                // Return buffer to pool if using one
                if let Some(pool) = &self.buffer_pool {
                    pool.release(buffer);
                }

                self.state = GuacdState::Failed("Empty response from guacd".to_string());
                Err(anyhow!("Empty response from guacd"))
            }
        } else {
            self.state = GuacdState::Failed("Not connected to guacd".to_string());
            Err(anyhow!("Not connected to guacd"))
        }
    }
}

impl ProtocolHandler for GuacdHandler {
    fn initialize(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            debug!("GuacdHandler: Initializing handler with client_id: {}", self.client_id);
            // Print the settings we have
            debug!("GuacdHandler: Connection settings:");
            for (key, value) in &self.connection_settings {
                debug!("  - {}: {}", key, value);
            }
            
            // Connect to guacd server during initialization
            self.connect_to_guacd().await
        })
    }

    fn process_client_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            debug!("GuacdHandler: process_client_data received {} bytes on connection {}", data.len(), conn_no);
            debug!("GuacdHandler: First 20 bytes of data: {:?}", &data[0..std::cmp::min(20, data.len())]);
            
            // Log the data as a string if it appears to be text
            if let Ok(text) = str::from_utf8(data) {
                debug!("GuacdHandler: Data as text: {}", text);
            }
            
            // Get a buffer from the pool if available
            let buffer = if let Some(pool) = &self.buffer_pool {
                let mut buf = pool.acquire();
                debug!("GuacdHandler: Got buffer from pool, extending with {} bytes", data.len());
                buf.extend_from_slice(data);
                buf.freeze()
            } else {
                debug!("GuacdHandler: No buffer pool, copying data ({} bytes)", data.len());
                Bytes::copy_from_slice(data)
            };
            
            // Process the Guacamole protocol data
            if let GuacdState::Ready = self.state {
                debug!("GuacdHandler: Connection state is Ready, proceeding to parse");
                // Parse the guacd protocol data
                match self.parser.receive(conn_no, data) {
                    Ok(_) => {
                        debug!("GuacdHandler: Successfully parsed client data");
                        
                        // Check for special instructions that need handling
                        let mut has_instruction = false;
                        while let Some(instruction) = self.parser.get_instruction() {
                            has_instruction = true;
                            debug!("GuacdHandler: Processing client instruction: {}", instruction.opcode);
                            debug!("GuacdHandler: Instruction args: {:?}", instruction.args);
                            
                            match instruction.opcode.as_str() {
                                "disconnect" => {
                                    // Handle disconnect from the client
                                    debug!("GuacdHandler: Client requested disconnect");
                                    if let Some(mut conn) = self.guacd_connection.take() {
                                        let _ = conn.shutdown().await;
                                    }
                                    self.state = GuacdState::Initial;
                                    return Ok(EMPTY_BYTES.clone());
                                },
                                "error" => {
                                    // Handle error from the client
                                    let error_message = instruction.args.first()
                                        .cloned()
                                        .unwrap_or_else(|| "Unknown error".to_string());

                                    debug!("GuacdHandler: Client error: {}", error_message);
                                    if let Some(mut conn) = self.guacd_connection.take() {
                                        let _ = conn.shutdown().await;
                                    }
                                    self.state = GuacdState::Failed(error_message.clone());
                                    return Ok(EMPTY_BYTES.clone());
                                },
                                _ => {
                                    debug!("GuacdHandler: Passing through client instruction: {}", instruction.opcode);
                                } // Other instructions are passed through to guacd
                            }
                        }

                        // If no instruction was parsed, but we have data, log this as unusual
                        if !has_instruction && data.len() > 0 {
                            debug!("GuacdHandler: No instruction parsed from data, but data is non-empty");
                        }

                        // Forward data to guacd
                        if let Some(ref mut conn) = self.guacd_connection {
                            debug!("GuacdHandler: Forwarding {} bytes to guacd", buffer.len());
                            debug!("GuacdHandler: Data being forwarded: {}", String::from_utf8_lossy(&buffer));
                            
                            match conn.write_all(&buffer).await {
                                Ok(_) => debug!("GuacdHandler: Successfully wrote data to guacd connection"),
                                Err(e) => {
                                    debug!("GuacdHandler: Error writing to guacd connection: {}", e);
                                    // Return buffer to pool if using one
                                    if let Some(pool) = &self.buffer_pool {
                                        pool.release(buffer.into());
                                    }
                                    return Err(anyhow!("Error writing to guacd: {}", e));
                                }
                            }
                            
                            match conn.flush().await {
                                Ok(_) => debug!("GuacdHandler: Successfully flushed guacd connection"),
                                Err(e) => {
                                    debug!("GuacdHandler: Error flushing guacd connection: {}", e);
                                    // Return buffer to pool if using one
                                    if let Some(pool) = &self.buffer_pool {
                                        pool.release(buffer.into());
                                    }
                                    return Err(anyhow!("Error flushing guacd connection: {}", e));
                                }
                            }
                            
                            // Read response from guacd
                            debug!("GuacdHandler: Reading response from guacd with 10 second timeout");
                            let mut response_buffer = BytesMut::with_capacity(4096);
                            match tokio::time::timeout(
                                tokio::time::Duration::from_secs(10), // Increased timeout
                                conn.read_buf(&mut response_buffer)
                            ).await {
                                Ok(Ok(n)) if n > 0 => {
                                    debug!("GuacdHandler: Received {} bytes from guacd", n);
                                    // Log the raw bytes for debugging
                                    debug!("GuacdHandler: First 50 bytes of raw response: {:?}", 
                                           &response_buffer[..std::cmp::min(50, n)]);
                                    debug!("GuacdHandler: Response from guacd: {}", String::from_utf8_lossy(&response_buffer[..n]));
                                    
                                    // Return the response directly
                                    let response = Bytes::copy_from_slice(&response_buffer[..n]);
                                    
                                    // Return buffer to pool if using one
                                    if let Some(pool) = &self.buffer_pool {
                                        debug!("GuacdHandler: Releasing buffer back to pool");
                                        // Release back to the pool
                                        pool.release(buffer.into());
                                    }
                                    
                                    debug!("GuacdHandler: Returning response to client: {} bytes", response.len());
                                    return Ok(response);
                                },
                                Ok(Ok(0)) => {
                                    debug!("GuacdHandler: Empty response from guacd");
                                },
                                Ok(Err(e)) => {
                                    debug!("GuacdHandler: Error reading from guacd: {}", e);
                                },
                                Err(_) => {
                                    debug!("GuacdHandler: Timeout waiting for response from guacd (10 seconds)");
                                }
                                _ => {
                                    // This wildcard pattern catches any remaining cases
                                    debug!("GuacdHandler: Unexpected pattern in match statement");
                                }
                            }
                        } else {
                            log::warn!("GuacdHandler: No guacd connection available to forward data");
                            debug!("GuacdHandler: No guacd connection available to forward data");
                            
                            // Return buffer to pool if using one
                            if let Some(pool) = &self.buffer_pool {
                                pool.release(buffer.into());
                            }
                            
                            return Err(anyhow!("No guacd connection available"));
                        }

                        // Return buffer to pool if using one
                        if let Some(pool) = &self.buffer_pool {
                            debug!("GuacdHandler: Releasing buffer back to pool");
                            // Release back to the pool
                            pool.release(buffer.into());
                        }

                        // No immediate response, guacd will respond separately
                        debug!("GuacdHandler: No immediate response, returning empty bytes");
                        Ok(EMPTY_BYTES.clone())
                    },
                    Err(e) => {
                        log::error!("GuacdHandler: Failed to parse client data: {}", e);
                        debug!("GuacdHandler: Failed to parse client data: {}", e);
                        
                        // Return buffer to pool if using one
                        if let Some(pool) = &self.buffer_pool {
                            debug!("GuacdHandler: Releasing buffer back to pool after parse error");
                            // Release back to the pool
                            pool.release(buffer.into());
                        }
                        
                        Err(anyhow!("Failed to parse client data: {}", e))
                    }
                }
            } else {
                log::warn!("GuacdHandler: Connection not in Ready state: {:?}", self.state);
                debug!("GuacdHandler: Connection not in Ready state: {:?}", self.state);
                
                // Return buffer to pool if using one
                if let Some(pool) = &self.buffer_pool {
                    debug!("GuacdHandler: Releasing buffer back to pool due to not ready state");
                    // Release back to the pool
                    pool.release(buffer.into());
                }
                
                Err(anyhow!("GuaCD connection not in Ready state"))
            }
        })
    }

    fn process_server_data<'a>(&'a mut self, conn_no: u32, data: &'a [u8]) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            debug!("GuacdHandler: process_server_data received {} bytes on connection {}", data.len(), conn_no);
            
            // Parse the guacd server data
            match self.parser.receive(conn_no, data) {
                Ok(_) => {
                    debug!("GuacdHandler: Successfully parsed server data");
                    
                    // Check for special instructions that need handling
                    let mut has_disconnect = false;
                    let mut has_error = false;
                    
                    while let Some(instruction) = self.parser.get_instruction() {
                        debug!("GuacdHandler: Processing server instruction: {}", instruction.opcode);
                        
                        match instruction.opcode.as_str() {
                            "disconnect" => {
                                // Handle disconnect from server
                                debug!("GuacdHandler: Server requested disconnect");
                                has_disconnect = true;
                                if let Some(mut conn) = self.guacd_connection.take() {
                                    let _ = conn.shutdown().await;
                                }
                                self.state = GuacdState::Initial;
                            },
                            "error" => {
                                // Handle error from the server
                                has_error = true;
                                let error_message = instruction.args.first()
                                    .cloned()
                                    .unwrap_or_else(|| "Unknown error".to_string());
                                
                                debug!("GuacdHandler: Server error: {}", error_message);
                                self.state = GuacdState::Failed(error_message);
                            },
                            _ => {
                                debug!("GuacdHandler: Passing through server instruction: {}", instruction.opcode);
                            } // Other instructions are passed through to the client
                        }
                    }
                    
                    if has_disconnect || has_error {
                        debug!("GuacdHandler: Returning empty bytes due to disconnect/error");
                        Ok(EMPTY_BYTES.clone())
                    } else {
                        // Forward the data to the client with the correct connection number
                        if let Some(pool) = &self.buffer_pool {
                            debug!("GuacdHandler: Creating response from pool with {} bytes", data.len());
                            Ok(pool.create_bytes(data))
                        } else {
                            debug!("GuacdHandler: Copying response data ({} bytes)", data.len());
                            Ok(Bytes::copy_from_slice(data))
                        }
                    }
                },
                Err(e) => {
                    log::error!("GuacdHandler: Failed to parse server data: {}", e);
                    Err(anyhow!("Failed to parse server data: {}", e))
                }
            }
        })
    }

    fn handle_command<'a>(&'a mut self, command: &'a str, _params: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            match command {
                "reconnect" => {
                    // Attempt to reconnect to guacd server
                    self.connect_to_guacd().await
                }
                "disconnect" => {
                    // Close the guacd connection
                    if let Some(mut conn) = self.guacd_connection.take() {
                        let _ = conn.shutdown().await;
                    }
                    self.state = GuacdState::Initial;
                    Ok(())
                }
                _ => Ok(())
            }
        })
    }
    
    fn handle_new_connection(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Add the connection to the active set
            self.active_connections.insert(conn_no);
            
            // If first connection, try to ensure we're connected to guacd
            if self.active_connections.len() == 1 && self.guacd_connection.is_none() {
                self.connect_to_guacd().await?;
            }
            
            debug!("GuaCD handler: new connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_eof(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            debug!("GuaCD handler: EOF on connection {}", conn_no);
            Ok(())
        })
    }
    
    fn handle_close(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Remove from active connections
            self.active_connections.remove(&conn_no);
            
            // If no more connections, close the guacd connection
            if self.active_connections.is_empty() && self.guacd_connection.is_some() {
                if let Some(mut conn) = self.guacd_connection.take() {
                    let _ = conn.shutdown().await;
                }
                self.state = GuacdState::Initial;
            }
            
            debug!("GuaCD handler: closed connection {}", conn_no);
            Ok(())
        })
    }
    
    fn set_webrtc_channel(&mut self, channel: WebRTCDataChannel) {
        debug!("GuacdHandler: Setting WebRTC data channel");
        self.webrtc = Some(channel);
        debug!("GuacdHandler: WebRTC data channel set successfully");
    }

    fn on_webrtc_channel_state_change(&mut self, state: &str) -> BoxFuture<'_, Result<()>> {
        let state_copy = state.to_string(); // Copy the state to avoid lifetime issues
        Box::pin(async move {
            debug!("Guacd handler: WebRTC channel state changed to {}", state_copy);
            
            if state_copy == "open" {
                if let Some(webrtc) = &self.webrtc {
                    if webrtc.ready_state() == "open" {
                        debug!("Guacd handler: WebRTC channel is open");
                        
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
            // Guacd handler currently doesn't queue messages, but we could add that
            // functionality in the future if needed. For now, return Ok.
            debug!("Guacd handler: process_pending_messages called (no-op for this handler)");
            Ok(())
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            // Close the guacd connection
            if let Some(mut conn) = self.guacd_connection.take() {
                let _ = conn.shutdown().await;
            }
            
            // Clear active connections
            self.active_connections.clear();
            
            // Reset state
            self.state = GuacdState::Initial;
            
            // Clean up parser
            self.parser.cleanup();
            
            Ok(())
        })
    }

    fn status(&self) -> String {
        match self.state {
            GuacdState::Initial => format!("GuaCD: Initial - Active connections: {}", self.active_connections.len()),
            GuacdState::Connected => format!("GuaCD: Connected - Active connections: {}", self.active_connections.len()),
            GuacdState::Handshaking => format!("GuaCD: Handshaking - Active connections: {}", self.active_connections.len()),
            GuacdState::Ready => format!("GuaCD: Ready - Active connections: {}", self.active_connections.len()),
            GuacdState::Failed(ref reason) => format!("GuaCD: Failed ({}) - Active connections: {}", reason, self.active_connections.len()),
        }
    }
} 