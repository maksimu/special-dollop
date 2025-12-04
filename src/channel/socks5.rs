// SOCKS5 server functionality extracted from server.rs

use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely;
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use bytes::BufMut;
use log::{debug, error};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Constants for SOCKS5 protocol
pub(crate) const SOCKS5_VERSION: u8 = 0x05;
pub(crate) const SOCKS5_AUTH_METHOD_NONE: u8 = 0x00;
pub(crate) const SOCKS5_AUTH_FAILED: u8 = 0xFF;
pub(crate) const SOCKS5_CMD_CONNECT: u8 = 0x01;
pub(crate) const SOCKS5_ADDR_TYPE_IPV4: u8 = 0x01;
pub(crate) const SOCKS5_ATYP_DOMAIN: u8 = 0x03;
pub(crate) const SOCKS5_ATYP_IPV6: u8 = 0x04;
pub(crate) const SOCKS5_FAIL: u8 = 0x01;

/// Handle a SOCKS5 client connection
pub(crate) async fn handle_socks5_connection(
    stream: TcpStream,
    conn_no: u32,
    conn_tx: tokio::sync::mpsc::Sender<(
        u32,
        tokio::net::tcp::OwnedWriteHalf,
        tokio::task::JoinHandle<()>,
    )>,
    webrtc: WebRTCDataChannel,
    buffer_pool: crate::buffer_pool::BufferPool,
    channel_id: String,
) -> Result<()> {
    // Split the stream
    let (mut reader, mut writer) = stream.into_split();

    // ===== Step 1: Handle initial greeting and authentication method negotiation =====
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf).await?;

    let socks_version = buf[0];
    let num_methods = buf[1];

    if socks_version != SOCKS5_VERSION {
        error!(
            "Channel({}): Invalid SOCKS version: {}",
            channel_id, socks_version
        );
        writer
            .write_all(&[SOCKS5_VERSION, SOCKS5_AUTH_FAILED])
            .await?;
        return Err(anyhow!("Invalid SOCKS version"));
    }

    // Read authentication methods
    let mut methods = vec![0u8; num_methods as usize];
    reader.read_exact(&mut methods).await?;

    // Check if no authentication is supported
    let selected_method = if methods.contains(&SOCKS5_AUTH_METHOD_NONE) {
        SOCKS5_AUTH_METHOD_NONE
    } else {
        // No supported authentication method
        SOCKS5_AUTH_FAILED
    };

    // Send selected method
    writer.write_all(&[SOCKS5_VERSION, selected_method]).await?;

    if selected_method == SOCKS5_AUTH_FAILED {
        return Err(anyhow!("No supported authentication method"));
    }

    // ===== Step 2: Handle connection request =====
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;

    let version = buf[0];
    let cmd = buf[1];
    let _reserved = buf[2];
    let addr_type = buf[3];

    if version != SOCKS5_VERSION {
        error!(
            "Channel({}): Invalid SOCKS version in request: {}",
            channel_id, version
        );
        send_socks5_response(&mut writer, SOCKS5_FAIL, &[0, 0, 0, 0], 0, &buffer_pool).await?;
        return Err(anyhow!("Invalid SOCKS version in request"));
    }

    match cmd {
        SOCKS5_CMD_CONNECT => {
            // TCP CONNECT - continue with existing logic
        }
        _ => {
            // UDP ASSOCIATE (0x03) and other commands not supported
            error!(
                "Channel({}): Unsupported SOCKS command: {}",
                channel_id, cmd
            );
            send_socks5_response(&mut writer, 0x07, &[0, 0, 0, 0], 0, &buffer_pool).await?; // Command not supported
            return Err(anyhow!("Unsupported SOCKS command"));
        }
    }

    // Parse the destination address
    let dest_host = match addr_type {
        SOCKS5_ADDR_TYPE_IPV4 => {
            let mut addr = [0u8; 4];
            reader.read_exact(&mut addr).await?;
            format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
        }
        SOCKS5_ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            reader.read_exact(&mut len).await?;
            let domain_len = len[0] as usize;

            let mut domain = vec![0u8; domain_len];
            reader.read_exact(&mut domain).await?;

            String::from_utf8(domain)?
        }
        SOCKS5_ATYP_IPV6 => {
            let mut addr = [0u8; 16];
            reader.read_exact(&mut addr).await?;
            // Format IPv6 address
            format!(
                "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
                ((addr[0] as u16) << 8) | (addr[1] as u16),
                ((addr[2] as u16) << 8) | (addr[3] as u16),
                ((addr[4] as u16) << 8) | (addr[5] as u16),
                ((addr[6] as u16) << 8) | (addr[7] as u16),
                ((addr[8] as u16) << 8) | (addr[9] as u16),
                ((addr[10] as u16) << 8) | (addr[11] as u16),
                ((addr[12] as u16) << 8) | (addr[13] as u16),
                ((addr[14] as u16) << 8) | (addr[15] as u16)
            )
        }
        _ => {
            error!(
                "Channel({}): Unsupported address type: {}",
                channel_id, addr_type
            );
            send_socks5_response(&mut writer, 0x08, &[0, 0, 0, 0], 0, &buffer_pool).await?; // Address type isn't supported
            return Err(anyhow!("Unsupported address type"));
        }
    };

    // Read port
    let mut port_buf = [0u8; 2];
    reader.read_exact(&mut port_buf).await?;
    let dest_port = u16::from_be_bytes(port_buf);

    debug!(
        "Channel({}): SOCKS5 connection to {}:{}",
        channel_id, dest_host, dest_port
    );

    // ===== Step 3: Send OpenConnection message to the tunnel =====
    // Build and send the OpenConnection message
    // **PERFORMANCE: Use buffer pool for zero-copy**
    let mut open_data = buffer_pool.acquire();
    open_data.clear();

    // Connection number
    open_data.extend_from_slice(&conn_no.to_be_bytes());

    // Host length + host
    let host_bytes = dest_host.as_bytes();
    open_data.extend_from_slice(&(host_bytes.len() as u32).to_be_bytes());
    open_data.extend_from_slice(host_bytes);

    // Port (PORT_LENGTH = 2 bytes for standard u16)
    open_data.extend_from_slice(&dest_port.to_be_bytes());

    // Create and send the control message
    let frame =
        Frame::new_control_with_pool(ControlMessage::OpenConnection, &open_data, &buffer_pool);
    let encoded = frame.encode_with_pool(&buffer_pool);

    buffer_pool.release(open_data);

    webrtc
        .send(encoded)
        .await
        .map_err(|e| anyhow!("Failed to send OpenConnection: {}", e))?;

    // ===== Step 4: Set up a task to read from a client and forward to tunnel =====
    let dc = webrtc.clone();
    let endpoint_name = channel_id.clone();
    let buffer_pool_clone = buffer_pool.clone();

    let read_task = tokio::spawn(async move {
        let mut read_buffer = buffer_pool_clone.acquire();
        let mut encode_buffer = buffer_pool_clone.acquire();

        // Use 64KB max read size - maximum safe size under webrtc-rs limits
        // Matches server.rs and connections.rs for consistent performance
        // 64KB is the hard limit (OUR_MAX_MESSAGE_SIZE in webrtc_core.rs)
        const MAX_READ_SIZE: usize = 64 * 1024;

        // **BOLD WARNING: ENTERING HOT PATH - TCP→WEBRTC FORWARDING LOOP**
        // **NO STRING ALLOCATIONS, NO UNNECESSARY OBJECT CREATION**
        // **USE BUFFER POOLS AND ZERO-COPY TECHNIQUES**
        loop {
            read_buffer.clear();
            if read_buffer.capacity() < MAX_READ_SIZE {
                read_buffer.reserve(MAX_READ_SIZE - read_buffer.capacity());
            }

            // Limit read size to prevent SCTP issues
            let max_to_read = std::cmp::min(read_buffer.capacity(), MAX_READ_SIZE);

            // Correctly create a mutable slice for reading
            let ptr = read_buffer.chunk_mut().as_mut_ptr();
            let current_chunk_len = read_buffer.chunk_mut().len();
            let slice_len = std::cmp::min(current_chunk_len, max_to_read);
            let read_slice = unsafe { std::slice::from_raw_parts_mut(ptr, slice_len) };

            match reader.read(read_slice).await {
                Ok(0) => {
                    // EOF
                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!(
                            "Channel({}): Client connection {} closed",
                            endpoint_name, conn_no
                        );
                    }

                    // Send EOF to tunnel
                    let eof_frame = Frame::new_control_with_pool(
                        ControlMessage::SendEOF,
                        &conn_no.to_be_bytes(),
                        &buffer_pool_clone,
                    );
                    let encoded = eof_frame.encode_with_pool(&buffer_pool_clone);
                    let send_start = std::time::Instant::now();
                    match dc.send(encoded.clone()).await {
                        Ok(_) => {
                            let send_latency = send_start.elapsed();
                            crate::metrics::METRICS_COLLECTOR.record_message_sent(
                                &endpoint_name,
                                encoded.len() as u64,
                                Some(send_latency),
                            );
                        }
                        Err(_) => {
                            crate::metrics::METRICS_COLLECTOR
                                .record_error(&endpoint_name, "socks5_eof_send_failed");
                        }
                    }

                    // Then close the connection
                    let mut close_buffer = buffer_pool_clone.acquire();
                    close_buffer.clear();
                    close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                    close_buffer.put_u8(CloseConnectionReason::Normal as u8);

                    let close_frame = Frame::new_control_with_pool(
                        ControlMessage::CloseConnection,
                        &close_buffer,
                        &buffer_pool_clone,
                    );

                    let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                    let send_start = std::time::Instant::now();
                    match dc.send(encoded.clone()).await {
                        Ok(_) => {
                            let send_latency = send_start.elapsed();
                            crate::metrics::METRICS_COLLECTOR.record_message_sent(
                                &endpoint_name,
                                encoded.len() as u64,
                                Some(send_latency),
                            );
                        }
                        Err(_) => {
                            crate::metrics::METRICS_COLLECTOR
                                .record_error(&endpoint_name, "socks5_close_send_failed");
                        }
                    }

                    break;
                }
                Ok(n) => {
                    // Advance the buffer by the number of bytes read
                    unsafe {
                        read_buffer.advance_mut(n);
                    }

                    // Data from a client
                    encode_buffer.clear();

                    // Create a data frame
                    let frame =
                        Frame::new_data_with_pool(conn_no, &read_buffer[0..n], &buffer_pool_clone);
                    let bytes_written = frame.encode_into(&mut encode_buffer);
                    let encoded = encode_buffer.split_to(bytes_written).freeze();

                    let send_start = std::time::Instant::now();
                    match dc.send(encoded.clone()).await {
                        Ok(_) => {
                            let send_latency = send_start.elapsed();

                            // Record metrics for message sent (using channel_id as conversation_id)
                            crate::metrics::METRICS_COLLECTOR.record_message_sent(
                                &endpoint_name,
                                encoded.len() as u64,
                                Some(send_latency),
                            );
                        }
                        Err(e) => {
                            // Record error metrics
                            crate::metrics::METRICS_COLLECTOR
                                .record_error(&endpoint_name, "socks5_send_failed");

                            error!(
                                "Channel({}): Failed to send data to tunnel: {}",
                                endpoint_name, e
                            );
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Channel({}): Error reading from client: {}",
                        endpoint_name, e
                    );
                    break;
                }
            }
        }

        // Return buffers to the pool
        buffer_pool_clone.release(read_buffer);
        buffer_pool_clone.release(encode_buffer);

        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Channel({}): Client read task for connection {} exited",
                endpoint_name, conn_no
            );
        }
    });

    // ===== Step 5: Send a deferred SOCKS5 success response =====
    // The actual response will be sent when we receive ConnectionOpened
    // But the task will continue to run, forwarding data

    // Send the reader task and writer to the channel
    conn_tx
        .send((conn_no, writer, read_task))
        .await
        .map_err(|e| anyhow!("Failed to send connection to channel: {}", e))?;

    Ok(())
}

/// Send a SOCKS5 response to the client
pub(crate) async fn send_socks5_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    rep: u8,
    addr: &[u8],
    port: u16,
    buffer_pool: &crate::buffer_pool::BufferPool,
) -> Result<()> {
    // **PERFORMANCE: Use buffer pool for zero-copy response**
    let mut response = buffer_pool.acquire();
    response.clear();
    response.put_u8(SOCKS5_VERSION);
    response.put_u8(rep);
    response.put_u8(0x00); // Reserved
    response.put_u8(SOCKS5_ADDR_TYPE_IPV4); // Address type
    response.extend_from_slice(addr);
    response.extend_from_slice(&port.to_be_bytes());

    writer.write_all(&response).await?;
    buffer_pool.release(response);
    Ok(())
}
