// SOCKS5 server functionality extracted from server.rs

use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::unlikely;
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut};
use log::{debug, error, info, warn};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Constants for SOCKS5 protocol
pub(crate) const SOCKS5_VERSION: u8 = 0x05;
pub(crate) const SOCKS5_AUTH_METHOD_NONE: u8 = 0x00;
pub(crate) const SOCKS5_AUTH_FAILED: u8 = 0xFF;
pub(crate) const SOCKS5_CMD_CONNECT: u8 = 0x01;
pub(crate) const SOCKS5_CMD_UDP: u8 = 0x03;
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
        SOCKS5_CMD_UDP => {
            // UDP ASSOCIATE - handle separately
            return handle_socks5_udp_associate(
                reader,
                writer,
                addr_type,
                conn_no,
                webrtc,
                buffer_pool,
                channel_id,
            )
            .await;
        }
        _ => {
            error!(
                "Channel({}): Unsupported SOCKS command: {}",
                channel_id, cmd
            );
            send_socks5_response(&mut writer, 0x07, &[0, 0, 0, 0], 0, &buffer_pool).await?; // Command isn't supported
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

/// Send a SOCKS5 response to the client with IPv6 address
pub(crate) async fn send_socks5_response_ipv6(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    rep: u8,
    addr: &[u8; 16], // IPv6 address as 16 bytes
    port: u16,
    buffer_pool: &crate::buffer_pool::BufferPool,
) -> Result<()> {
    let mut response = buffer_pool.acquire();
    response.clear();
    response.put_u8(SOCKS5_VERSION);
    response.put_u8(rep);
    response.put_u8(0x00); // Reserved
    response.put_u8(SOCKS5_ATYP_IPV6); // IPv6 address type
    response.extend_from_slice(addr); // 16 bytes for IPv6
    response.extend_from_slice(&port.to_be_bytes());

    writer.write_all(&response).await?;
    buffer_pool.release(response);
    Ok(())
}

/// Handle a SOCKS5 UDP ASSOCIATE request
pub(crate) async fn handle_socks5_udp_associate(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    addr_type: u8,
    conn_no: u32,
    webrtc: WebRTCDataChannel,
    buffer_pool: crate::buffer_pool::BufferPool,
    channel_id: String,
) -> Result<()> {
    // Parse the client's desired UDP relay address (usually 0.0.0.0:0 for "any")
    let _client_udp_host = match addr_type {
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
                "Channel({}): Unsupported address type for UDP: {}",
                channel_id, addr_type
            );
            send_socks5_response(&mut writer, 0x08, &[0, 0, 0, 0], 0, &buffer_pool).await?;
            return Err(anyhow!("Unsupported address type for UDP"));
        }
    };

    // Read the client's desired UDP port
    let mut port_buf = [0u8; 2];
    reader.read_exact(&mut port_buf).await?;
    let _client_udp_port = u16::from_be_bytes(port_buf);

    // Create UDP socket for this association using dual-stack binding
    let udp_socket = crate::models::dual_stack::bind_udp_localhost(0).await?;
    let udp_local_addr = udp_socket.local_addr()?;

    info!(
        "Channel({}): UDP ASSOCIATE created on {}",
        channel_id, udp_local_addr
    );

    // Send success response with the UDP relay address
    let local_ip = udp_local_addr.ip();
    let local_port = udp_local_addr.port();

    match local_ip {
        std::net::IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            send_socks5_response(
                &mut writer,
                0x00, // Success
                &octets,
                local_port,
                &buffer_pool,
            )
            .await?;
        }
        std::net::IpAddr::V6(ipv6) => {
            let octets = ipv6.octets();
            send_socks5_response_ipv6(
                &mut writer,
                0x00, // Success
                &octets,
                local_port,
                &buffer_pool,
            )
            .await?;
        }
    }

    // Send UdpAssociate message to tunnel
    let mut udp_data = buffer_pool.acquire();
    udp_data.clear();
    udp_data.extend_from_slice(&conn_no.to_be_bytes());
    udp_data.extend_from_slice(&local_port.to_be_bytes());

    let frame = Frame::new_control_with_pool(ControlMessage::UdpAssociate, &udp_data, &buffer_pool);
    let encoded = frame.encode_with_pool(&buffer_pool);
    buffer_pool.release(udp_data);

    webrtc
        .send(encoded)
        .await
        .map_err(|e| anyhow!("Failed to send UdpAssociate: {}", e))?;

    // Set up UDP packet forwarding task
    let udp_socket = Arc::new(udp_socket);
    let webrtc_clone = webrtc.clone();
    let buffer_pool_clone = buffer_pool.clone();
    let channel_id_clone = channel_id.clone();

    let mut udp_task = tokio::spawn(async move {
        let mut buf = [0u8; 65536]; // Maximum UDP packet size

        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((len, peer_addr)) => {
                    debug!(
                        "Channel({}): UDP packet from {} ({} bytes)",
                        channel_id_clone, peer_addr, len
                    );

                    // Process SOCKS5 UDP packet format and forward through tunnel
                    if let Err(e) = process_udp_packet(
                        &buf[..len],
                        peer_addr,
                        conn_no,
                        &webrtc_clone,
                        &buffer_pool_clone,
                        &channel_id_clone,
                    )
                    .await
                    {
                        error!(
                            "Channel({}): Error processing UDP packet: {}",
                            channel_id_clone, e
                        );
                    }
                }
                Err(e) => {
                    error!("Channel({}): UDP recv error: {}", channel_id_clone, e);
                    break;
                }
            }
        }

        debug!(
            "Channel({}): UDP task for connection {} exited",
            channel_id_clone, conn_no
        );
    });

    // Monitor TCP connection for close
    let channel_id_for_tcp_task = channel_id.clone();
    let mut tcp_monitor_task = tokio::spawn(async move {
        let mut buf = [0u8; 1];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    debug!(
                        "Channel({}): TCP connection closed, terminating UDP association",
                        channel_id_for_tcp_task
                    );
                    break;
                }
                Ok(_) => {
                    // Ignore any data on TCP connection during UDP association
                }
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        _ = &mut udp_task => {
            tcp_monitor_task.abort();
        }
        _ = &mut tcp_monitor_task => {
            udp_task.abort();
        }
    }

    // Send UdpAssociateClosed message
    let close_frame = Frame::new_control_with_pool(
        ControlMessage::UdpAssociateClosed,
        &conn_no.to_be_bytes(),
        &buffer_pool,
    );
    let encoded = close_frame.encode_with_pool(&buffer_pool);
    let send_start = std::time::Instant::now();
    match webrtc.send(encoded.clone()).await {
        Ok(_) => {
            let send_latency = send_start.elapsed();
            crate::metrics::METRICS_COLLECTOR.record_message_sent(
                &channel_id,
                encoded.len() as u64,
                Some(send_latency),
            );
        }
        Err(_) => {
            crate::metrics::METRICS_COLLECTOR
                .record_error(&channel_id, "udp_associate_close_send_failed");
        }
    }

    Ok(())
}

/// Process a UDP packet received from a client
pub(crate) async fn process_udp_packet(
    packet_data: &[u8],
    peer_addr: std::net::SocketAddr,
    conn_no: u32,
    webrtc: &WebRTCDataChannel,
    buffer_pool: &crate::buffer_pool::BufferPool,
    channel_id: &str,
) -> Result<()> {
    // SOCKS5 UDP packet format:
    // +----+------+------+----------+----------+----------+
    // |RSV | FRAG | ATYP | DST.ADDR | DST.PORT |   DATA   |
    // +----+------+------+----------+----------+----------+
    // | 2  |  1   |  1   | Variable |    2     | Variable |
    // +----+------+------+----------+----------+----------+

    if packet_data.len() < 4 {
        return Err(anyhow!("UDP packet too short"));
    }

    let mut cursor = packet_data;
    let rsv = cursor.get_u16(); // Reserved, should be 0
    let frag = cursor.get_u8(); // Fragment, should be 0
    let atyp = cursor.get_u8(); // Address type

    if rsv != 0 {
        warn!(
            "Channel({}): Non-zero RSV in UDP packet: {}",
            channel_id, rsv
        );
    }

    if frag != 0 {
        warn!(
            "Channel({}): Fragmented UDP packets not supported: {}",
            channel_id, frag
        );
        return Err(anyhow!("Fragmented UDP packets not supported"));
    }

    // Parse destination address
    let dest_host = match atyp {
        SOCKS5_ADDR_TYPE_IPV4 => {
            if cursor.len() < 4 {
                return Err(anyhow!("UDP packet too short for IPv4 address"));
            }
            let addr = [cursor[0], cursor[1], cursor[2], cursor[3]];
            cursor.advance(4);
            format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
        }
        SOCKS5_ATYP_DOMAIN => {
            if cursor.is_empty() {
                return Err(anyhow!("UDP packet too short for domain length"));
            }
            let domain_len = cursor.get_u8() as usize;
            if cursor.len() < domain_len {
                return Err(anyhow!("UDP packet too short for domain"));
            }
            let domain_bytes = &cursor[..domain_len];
            cursor.advance(domain_len);
            String::from_utf8(domain_bytes.to_vec())?
        }
        SOCKS5_ATYP_IPV6 => {
            if cursor.len() < 16 {
                return Err(anyhow!("UDP packet too short for IPv6 address"));
            }
            let addr: [u8; 16] = cursor[..16].try_into()?;
            cursor.advance(16);
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
            return Err(anyhow!("Unsupported address type in UDP packet: {}", atyp));
        }
    };

    // Parse destination port
    if cursor.len() < 2 {
        return Err(anyhow!("UDP packet too short for port"));
    }
    let dest_port = cursor.get_u16();

    // Remaining data is the actual UDP payload
    let udp_payload = cursor;

    debug!(
        "Channel({}): UDP packet to {}:{} ({} bytes payload)",
        channel_id,
        dest_host,
        dest_port,
        udp_payload.len()
    );

    // Create UdpPacket message for tunnel
    let mut udp_data = buffer_pool.acquire();
    udp_data.clear();

    // Connection number
    udp_data.put_u32(conn_no);

    // Client address (for return packets)
    match peer_addr.ip() {
        std::net::IpAddr::V4(ipv4) => {
            udp_data.put_u8(SOCKS5_ADDR_TYPE_IPV4);
            udp_data.extend_from_slice(&ipv4.octets());
        }
        std::net::IpAddr::V6(ipv6) => {
            udp_data.put_u8(SOCKS5_ATYP_IPV6);
            udp_data.extend_from_slice(&ipv6.octets());
        }
    }
    udp_data.put_u16(peer_addr.port());

    // Destination host length + host
    let host_bytes = dest_host.as_bytes();
    udp_data.put_u32(host_bytes.len() as u32);
    udp_data.extend_from_slice(host_bytes);

    // Destination port
    udp_data.put_u16(dest_port);

    // UDP payload
    udp_data.extend_from_slice(udp_payload);

    let frame = Frame::new_control_with_pool(ControlMessage::UdpPacket, &udp_data, buffer_pool);
    let encoded = frame.encode_with_pool(buffer_pool);
    buffer_pool.release(udp_data);

    webrtc
        .send(encoded)
        .await
        .map_err(|e| anyhow!("Failed to send UDP packet: {}", e))?;

    Ok(())
}
