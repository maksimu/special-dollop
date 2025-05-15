// Frame handling functionality for Channel

use std::net::ToSocketAddrs;
use anyhow::{anyhow, Context as AnyhowContext, Result};
use crate::protocol::{Frame, ControlMessage, CloseConnectionReason, CTRL_NO_LEN, CONN_NO_LEN, now_ms};
use log::debug;
use tokio::io::AsyncWriteExt;
use super::core::Channel;

// Central dispatcher for incoming frames
pub async fn handle_incoming_frame(channel: &mut Channel, frame: Frame) -> Result<()> {
    debug!("Channel({}): handle_incoming_frame received frame on connection {}, payload length: {}, timestamp: {}", 
             channel.channel_id, frame.connection_no, frame.payload.len(), frame.timestamp_ms);
    
    // For small frames, log the contents for debugging
    if frame.payload.len() <= 100 {
        debug!("Channel({}): Frame payload: {:?}", channel.channel_id, frame.payload);
    } else {
        debug!("Channel({}): First 50 bytes of frame payload: {:?}", 
                 channel.channel_id, &frame.payload[..std::cmp::min(50, frame.payload.len())]);
    }
    
    match frame.connection_no {
        0 => {
            // Control channel
            debug!("Channel({}): Handling control frame", channel.channel_id);
            handle_control(channel, frame).await?;
        }
        conn_no => {
            // Check if this is a protocol connection
            if let Some(protocol_type) = channel.protocol_connections.get(&conn_no).cloned() {
                // Process through protocol handler
                debug!("Channel({}): Routing frame to protocol handler: {} for connection {}", 
                         channel.channel_id, protocol_type, conn_no);
                forward_to_protocol(channel, conn_no, &protocol_type, &frame.payload).await?;
            } else {
                // Regular TCP connection
                debug!("Channel({}): Routing frame to TCP backend connection {}", 
                         channel.channel_id, conn_no);
                forward_to_backend(channel, frame).await?;
            }
        }
    }
    
    Ok(())
}

// Handle control frames
pub async fn handle_control(channel: &mut Channel, frame: Frame) -> Result<()> {
    if frame.payload.len() < CTRL_NO_LEN {
        return Err(anyhow::anyhow!("Malformed control frame"));
    }

    let code = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
    let cmd = ControlMessage::try_from(code)?;
    
    // Create a stable copy of the data slice
    let data_bytes = frame.payload.slice(CTRL_NO_LEN..);
    let data = data_bytes.as_ref();

    match cmd {
        ControlMessage::Ping => {
           debug!("Endpoint {}: Received ping", channel.channel_id);
            // immediately echo back – echo the payload
            let pong = Frame::new_control_with_pool(ControlMessage::Pong, data, &channel.buffer_pool);
            channel.webrtc.send(pong.encode_with_pool(&channel.buffer_pool)).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        ControlMessage::Pong => {
           debug!("Endpoint {}: Received pong", channel.channel_id);

            if data.len() >= CONN_NO_LEN {
                let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

                if let Some(conn) = channel.conns.get_mut(&conn_no) {
                    conn.stats.message_counter = 0;

                    // Calculate round-trip latency if we have ping time
                    if let Some(ping_time) = conn.stats.ping_time {
                        let now = now_ms();
                        let latency = now.saturating_sub(ping_time);
                        channel.round_trip_latency.push(latency);

                        // Keep only recent latency values (last 20)
                        if channel.round_trip_latency.len() > 20 {
                            channel.round_trip_latency.remove(0);
                        }

                       debug!(
                            "Endpoint {}: Round trip latency: {} ms",
                            channel.channel_id,
                            latency
                        );

                        conn.stats.ping_time = None;
                    }
                }
            }

            // Reset ping attempts on any pong
            channel.ping_attempt = 0;
        }
        ControlMessage::OpenConnection => {
            // data contains: connection_no(4) + host data
            if data.len() < 4 {
                return Err(anyhow::anyhow!("OpenConnection too short"));
            }

            let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            debug!("Channel({}): Received OpenConnection for connection {}", channel.channel_id, conn_no);

            // Check if connection already exists
            if channel.conns.contains_key(&conn_no) {
                debug!("Channel({}): Connection {} already exists", channel.channel_id, conn_no);
                // Send a connection-opened message
                let opened = Frame::new_control_with_pool(
                    ControlMessage::ConnectionOpened,
                    &conn_no.to_be_bytes(),
                    &channel.buffer_pool
                );
                channel.webrtc.send(opened.encode_with_pool(&channel.buffer_pool)).await
                    .map_err(|e| anyhow::anyhow!("Failed to send ConnectionOpened: {}", e))?;
                return Ok(());
            }

            // Check if this is a protocol that should be handled specially
            // The protocol is determined by the connection settings, not the URL
            let protocol_type = if channel.server_mode {
                // In server mode, check registered protocols
                if let Some(protocol) = channel.protocol_handlers.keys().next() {
                    Some(protocol.clone())
                } else {
                    None
                }
            } else {
                None
            };

            // If we have a protocol handler for this connection
            if let Some(protocol_name) = protocol_type {
                debug!("Channel({}): Using protocol handler '{}' for connection {}", 
                       channel.channel_id, protocol_name, conn_no);

                return if let Some(handler) = channel.protocol_handlers.get_mut(&protocol_name) {
                    debug!("Channel({}): Found protocol handler for '{}', registering connection {}", 
                           channel.channel_id, protocol_name, conn_no);

                    // This is a protocol connection - register it
                    channel.protocol_connections.insert(conn_no, protocol_name.clone());

                    // Notify protocol handler about new connection
                    match handler.handle_new_connection(conn_no).await {
                        Ok(_) => {
                            debug!("Channel({}): Protocol handler accepted connection {}", 
                                   channel.channel_id, conn_no);
                        },
                        Err(e) => {
                            log::error!("Channel({}): Protocol handler rejected connection {}: {}", 
                                     channel.channel_id, conn_no, e);
                            return Err(e);
                        }
                    }

                    // Send a connection-opened message
                    debug!("Channel({}): Sending ConnectionOpened message for protocol connection {}", 
                           channel.channel_id, conn_no);
                    let opened = Frame::new_control_with_pool(
                        ControlMessage::ConnectionOpened,
                        &conn_no.to_be_bytes(),
                        &channel.buffer_pool
                    );

                    match channel.webrtc.send(opened.encode_with_pool(&channel.buffer_pool)).await {
                        Ok(_) => {
                            debug!("Channel({}): Successfully sent ConnectionOpened for connection {}", 
                                   channel.channel_id, conn_no);
                        },
                        Err(e) => {
                            log::error!("Channel({}): Failed to send ConnectionOpened for connection {}: {}", 
                                     channel.channel_id, conn_no, e);
                            return Err(anyhow!("Failed to send ConnectionOpened: {}", e));
                        }
                    }

                    log::info!("Endpoint {}: Opened protocol connection {} using handler {}", 
                        channel.channel_id, conn_no, protocol_name);
                    Ok(())
                } else {
                    log::error!("Channel({}): Protocol handler '{}' not found for connection {}", 
                             channel.channel_id, protocol_name, conn_no);

                    // Send a close connection message since we can't handle this protocol
                    channel.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                    Ok(())
                }
            }

            // Normal (non-protocol) connection handling
            // Parse host and port from the remainder of the data
            let target_str = if data.len() > 4 {
                std::str::from_utf8(&data[4..])?.trim()
            } else {
                // Default target if none specified (for tests)
                "127.0.0.1:0"
            };
            
            debug!("Channel({}): Normal connection to target: {}", channel.channel_id, target_str);

            // Check if a target is allowed if we have a network checker
            if let Some(checker) = &channel.network_checker {
                let (host, port_str) = target_str.split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("Invalid target format, expected host:port"))?;

                let port = port_str.parse::<u16>()
                    .context("Failed to parse port number")?;

                if !checker.is_host_allowed(host) {
                    log::error!("Endpoint {}: Host not allowed: {}", channel.channel_id, host);
                    channel.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                    return Ok(());
                }

                if !checker.is_port_allowed(port) {
                    log::error!("Endpoint {}: Port not allowed: {}", channel.channel_id, port);
                    channel.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                    return Ok(());
                }
            }

            // Resolve and connect
            let mut addrs = target_str.to_socket_addrs()?;
            if let Some(addr) = addrs.next() {
                // Set up timeout for connection
                match tokio::time::timeout(
                    channel.timeouts.open_connection,
                    super::connections::open_backend(channel, conn_no, addr)
                ).await {
                    Ok(result) => {
                        match result {
                            Ok(_) => {
                                // Connection success - send an opened message
                                let opened = Frame::new_control_with_pool(
                                    ControlMessage::ConnectionOpened,
                                    &conn_no.to_be_bytes(),
                                    &channel.buffer_pool
                                );
                                channel.webrtc.send(opened.encode_with_pool(&channel.buffer_pool)).await.map_err(|e| anyhow::anyhow!("{}", e))?;
                                log::info!("Endpoint {}: Opened connection {}", channel.channel_id, conn_no);
                            }
                            Err(e) => {
                                log::error!(
                                    "Endpoint {}: Failed to open connection {}: {}",
                                    channel.channel_id, conn_no, e
                                );
                                // Notify client of failure
                                channel.close_backend(conn_no, CloseConnectionReason::ConnectionFailed).await?;
                            }
                        }
                    }
                    Err(_) => {
                        log::error!(
                            "Endpoint {}: Connection {} timed out",
                            channel.channel_id, conn_no
                        );
                        channel.close_backend(conn_no, CloseConnectionReason::Timeout).await?;
                    }
                }
            } else {
                log::error!(
                    "Endpoint {}: Could not resolve address: {}",
                    channel.channel_id, target_str
                );
            }
        }
        ControlMessage::CloseConnection => {
            if data.len() < 4 {
                return Ok(());
            }

            let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

            // Get reason if provided
            let reason = if data.len() >= 6 {
                let reason_code = u16::from_be_bytes([data[4], data[5]]);
                match reason_code {
                    0 => CloseConnectionReason::Normal,
                    1 => CloseConnectionReason::ConnectionFailed,
                    2 => CloseConnectionReason::ConnectionLost,
                    3 => CloseConnectionReason::Timeout,
                    4 => CloseConnectionReason::AIClosed,
                    _ => CloseConnectionReason::Normal,
                }
            } else {
                CloseConnectionReason::Normal
            };

            channel.close_backend(conn_no, reason).await?;
        }
        ControlMessage::SendEOF => {
            if data.len() < 4 {
                return Ok(());
            }

            let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);

            // Check if this is a protocol connection
            if let Some(protocol_type) = channel.protocol_connections.get(&conn_no) {
                if let Some(handler) = channel.protocol_handlers.get_mut(protocol_type) {
                    // Notify protocol handler about EOF
                    handler.handle_eof(conn_no).await?;
                }
            } else if let Some(c) = channel.conns.get_mut(&conn_no) {
                let _ = crate::models::AsyncReadWrite::shutdown(&mut c.backend).await;
               debug!("Endpoint {}: Sent EOF to connection {}", channel.channel_id, conn_no);
            }
        }
        ControlMessage::ConnectionOpened => {
            // This is typically sent by us, but we'll handle it in case the remote sends it
            if data.len() >= 4 {
                let conn_no = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
               debug!("Endpoint {}: Connection {} opened", channel.channel_id, conn_no);
            }
        }
    }

    Ok(())
}

// Forward data to a TCP backend
async fn forward_to_backend(channel: &mut Channel, frame: Frame) -> Result<()> {
    let conn_no = frame.connection_no;
    
    if let Some(conn) = channel.conns.get_mut(&conn_no) {
        // Update statistics
        let now = now_ms();
        let receive_latency = now.saturating_sub(frame.timestamp_ms);
        conn.stats.receive_latency_sum += receive_latency;
        conn.stats.receive_latency_count += 1;
        conn.stats.receive_size += frame.payload.len();

        // Send data to the backend
        conn.backend.write_all(&frame.payload).await?;
        conn.backend.flush().await?;

       debug!(
            "Endpoint {}: Forwarded {} bytes to backend connection {}",
            channel.channel_id,
            frame.payload.len(),
            conn_no
        );
    } else {
        log::warn!(
            "Endpoint {}: Received data for unknown connection {}",
            channel.channel_id,
            conn_no
        );
    }

    Ok(())
}

// Forward data to a protocol handler
async fn forward_to_protocol(channel: &mut Channel, conn_no: u32, protocol_type: &str, data: &[u8]) -> Result<()> {
    debug!("Channel({}): forward_to_protocol for {} connection {}, data length: {}", 
             channel.channel_id, protocol_type, conn_no, data.len());
    debug!("Channel({}): First 20 bytes of data for protocol: {:?}", 
             channel.channel_id, &data[0..std::cmp::min(20, data.len())]);
    
    if let Some(handler) = channel.protocol_handlers.get_mut(protocol_type) {
        debug!("Channel({}): Found protocol handler for {}, calling process_client_data", 
                 channel.channel_id, protocol_type);
        // Process data through the protocol handler
        let response = match handler.process_client_data(conn_no, data).await {
            Ok(res) => {
                debug!("Channel({}): Protocol handler processed data successfully, response length: {}", 
                         channel.channel_id, res.len());
                if !res.is_empty() {
                    debug!("Channel({}): First 50 bytes of protocol response: {:?}", 
                       channel.channel_id, &res[..std::cmp::min(50, res.len())]);
                } else {
                    debug!("Channel({}): Protocol handler returned EMPTY response", channel.channel_id);
                }
                res
            },
            Err(e) => {
                debug!("Channel({}): Protocol handler error: {}", channel.channel_id, e);
                return Err(e);
            }
        };
        
        // If there's a response, send it back
        if !response.is_empty() {
            debug!("Channel({}): Sending protocol handler response back to client, length: {}", 
                     channel.channel_id, response.len());
            // Create and send the frame
            let frame = Frame::new_data_with_pool(conn_no, &response, &channel.buffer_pool);
            let encoded = frame.encode_with_pool(&channel.buffer_pool);
            
            debug!("Channel({}): WebRTC datachannel status before send: {}", 
                     channel.channel_id, channel.webrtc.ready_state());
                     
            match channel.webrtc.send(encoded).await {
                Ok(_) => debug!("Channel({}): WebRTC send successful", channel.channel_id),
                Err(e) => {
                    debug!("Channel({}): WebRTC send error: {}", channel.channel_id, e);
                    return Err(anyhow!("WebRTC send error: {}", e));
                }
            }
        } else {
            debug!("Channel({}): Protocol handler returned empty response (normal for async handlers)", 
                     channel.channel_id);
            // Check handler status to help with debugging
            let status = handler.status();
            debug!("Channel({}): Protocol handler status: {}", channel.channel_id, status);
        }
        
       debug!(
            "Endpoint {}: Forwarded {} bytes to protocol handler {} for connection {}",
            channel.channel_id,
            data.len(),
            protocol_type,
            conn_no
        );
    } else {
        debug!("Channel({}): Protocol handler {} not found for connection {}", 
                 channel.channel_id, protocol_type, conn_no);
        log::warn!(
            "Endpoint {}: Protocol handler {} not found for connection {}",
            channel.channel_id,
            protocol_type,
            conn_no
        );
    }
    
    Ok(())
}
