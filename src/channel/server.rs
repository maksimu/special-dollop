// Server functionality for the Channel implementation

use anyhow::Result;
use std::sync::Arc;
use std::net::SocketAddr;
use log::{debug, info};
use tokio::net::TcpListener;
use tokio::io::AsyncReadExt;
use crate::protocol::{Frame, ControlMessage, CloseConnectionReason};

use super::core::Channel;

impl Channel {
    // Start a local server to accept incoming connections
    pub async fn start_server(&mut self, host: &str, port: u16) -> Result<u16> {
        if self.local_client_server.is_some() {
            return Ok(port); // Server already running
        }

        let addr = format!("{}:{}", host, port).parse::<SocketAddr>()?;
        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                log::error!(
                "Endpoint {}: Failed to bind to {}:{}: {}",
                self.channel_id, host, port, e
            );
                return Err(anyhow::anyhow!("Failed to bind: {}", e));
            }
        };

        // Record the actually bound port
        let actual_port = listener.local_addr()?.port();
        
        // Get the connection channel sender
        let conn_tx = match self.local_client_server_conn_tx.clone() {
            Some(tx) => tx,
            None => return Err(anyhow::anyhow!("Connection channel not available")),
        };
        
        // Set up the server task
        let server = Arc::new(listener);
        let server_clone = server.clone();
        let channel_id_clone = self.channel_id.clone(); // Clone before moving into the task
        let webrtc_clone = self.webrtc.clone();
        let buffer_pool_clone = self.buffer_pool.clone(); // Share the buffer pool with the server task
        
        let task = tokio::spawn(async move {
            info!("Endpoint {}: Server started on port {}", channel_id_clone, actual_port);
            
            let mut next_conn_no = 1;
            
            loop {
                match server_clone.accept().await {
                    Ok((stream, addr)) => {
                       debug!("Endpoint {}: Accepted connection from {}", channel_id_clone, addr);
                        
                        // Assign a connection number
                        let conn_no: u32 = next_conn_no;
                        next_conn_no += 1;
                        
                        // Split the stream
                        let (reader, writer) = stream.into_split();
                        
                        // Create a buffer for connection information
                        let mut buffer = buffer_pool_clone.acquire();
                        buffer.clear();
                        
                        // Add a connection number
                        buffer.extend_from_slice(&conn_no.to_be_bytes());
                        
                        // Add address information
                        buffer.extend_from_slice(format!("{}:{}", addr.ip(), addr.port()).as_bytes());
                        
                        // Create the control message frame
                        let frame = Frame::new_control_with_buffer(
                            ControlMessage::OpenConnection, 
                            &mut buffer
                        );
                        
                        // Encode the frame with our pool
                        let encoded = frame.encode_with_pool(&buffer_pool_clone);
                        
                        if let Err(e) = webrtc_clone.send(encoded).await {
                            log::error!("Endpoint {}: Failed to send OpenConnection: {}", 
                                channel_id_clone, e);
                            continue;
                        }
                        
                        // Spawn a task to read from the stream and send to WebRTC
                        let dc = webrtc_clone.clone();
                        let task_endpoint = channel_id_clone.clone();
                        let task_buffer_pool = buffer_pool_clone.clone();
                        
                        let task = tokio::spawn(async move {
                            let mut reader = reader;
                            
                            // Get buffers from the pool
                            let mut read_buffer = task_buffer_pool.acquire();
                            let mut encode_buffer = task_buffer_pool.acquire();
                            
                            // Ensure we have enough capacity
                            if read_buffer.capacity() < 16 * 1024 {
                                read_buffer.reserve(16 * 1024);
                            }
                            
                            // Main read loop
                            loop {
                                // Clear buffer for reuse
                                read_buffer.clear();
                                
                                match reader.read_buf(&mut read_buffer).await {
                                    Ok(0) => {
                                        // EOF - send SendEOF control message
                                        let eof_frame = Frame::new_control_with_pool(
                                            ControlMessage::SendEOF,
                                            &conn_no.to_be_bytes(),
                                            &task_buffer_pool
                                        );
                                        let encoded = eof_frame.encode_with_pool(&task_buffer_pool);
                                        let _ = dc.send(encoded).await;
                                        break;
                                    }
                                    Ok(n) if n > 0 => {
                                        // Create a data frame with the read portion of the buffer
                                        let frame = Frame::new_data_with_pool(
                                            conn_no, 
                                            &read_buffer[..n],
                                            &task_buffer_pool
                                        );
                                        
                                        // Encode directly into our encode buffer
                                        let bytes_written = frame.encode_into(&mut encode_buffer);
                                        let encoded = encode_buffer.split_to(bytes_written).freeze();
                                        
                                        if let Err(e) = dc.send(encoded).await {
                                            log::error!(
                                                "Endpoint {}: Failed to send data: {}",
                                                task_endpoint, e
                                            );
                                            break;
                                        }
                                    }
                                    Ok(_) => {
                                        // Zero bytes read but not EOF, keep looping
                                        continue;
                                    }
                                    Err(e) => {
                                        log::error!(
                                            "Endpoint {}: Read error: {}",
                                            task_endpoint, e
                                        );
                                        break;
                                    }
                                }
                            }
                            
                            // Send a close connection message
                            let mut close_buffer = task_buffer_pool.acquire();
                            close_buffer.clear();
                            close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                            close_buffer.extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());
                            
                            let close_frame = Frame::new_control_with_buffer(
                                ControlMessage::CloseConnection, 
                                &mut close_buffer
                            );
                            
                            let encoded = close_frame.encode_with_pool(&task_buffer_pool);
                            let _ = dc.send(encoded).await;
                            
                            // Return buffers to the pool
                            task_buffer_pool.release(read_buffer);
                            task_buffer_pool.release(encode_buffer);
                            task_buffer_pool.release(close_buffer);
                            
                           debug!("Endpoint {}: Stream reader task for connection {} exited", 
                                task_endpoint, conn_no);
                        });
                        
                        // Send the connection to the main channel
                        if let Err(e) = conn_tx.send((conn_no, writer, task)).await {
                            log::error!("Endpoint {}: Failed to register connection: {}", 
                                channel_id_clone, e);
                            
                            // If we can't register, send a close message
                            let mut close_buffer = buffer_pool_clone.acquire();
                            close_buffer.clear();
                            close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                            close_buffer.extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());
                            
                            let close_frame = Frame::new_control_with_buffer(
                                ControlMessage::CloseConnection, 
                                &mut close_buffer
                            );
                            
                            let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                            let _ = webrtc_clone.send(encoded).await;
                            
                            // Return buffer to pool
                            buffer_pool_clone.release(close_buffer);
                        }
                        
                        // Return our main connection buffer to the pool
                        buffer_pool_clone.release(buffer);
                    }
                    Err(e) => {
                        log::error!("Endpoint {}: Error accepting connection: {}", channel_id_clone, e);
                        // Brief pause to avoid spinning on errors
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });
        
        self.local_client_server = Some(server);
        self.local_client_server_task = Some(task);
        
        Ok(actual_port)
    }

    // Stop the server
    pub async fn stop_server(&mut self) -> Result<()> {
        if let Some(task) = self.local_client_server_task.take() {
            task.abort();

            // Give it some time to clean up
            match tokio::time::timeout(std::time::Duration::from_secs(5), task).await {
                Ok(_) => {
                   debug!("Endpoint {}: Server task shutdown cleanly", self.channel_id);
                }
                Err(_) => {
                    log::warn!("Endpoint {}: Server task did not shutdown in time", self.channel_id);
                }
            }
        }

        self.local_client_server = None;
        info!("Endpoint {}: Server stopped", self.channel_id);
        Ok(())
    }
}
