// Server functionality for the Channel implementation

use crate::tube_protocol::{CloseConnectionReason, ControlMessage, Frame};
use crate::webrtc_data_channel::WebRTCDataChannel;
use anyhow::{anyhow, Result};
use bytes::BufMut;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use super::core::Channel;
use super::types::ActiveProtocol;

// Constants for SOCKS5 protocol
const SOCKS5_VERSION: u8 = 0x05;
const SOCKS5_AUTH_METHOD_NONE: u8 = 0x00;
// const SOCKS5_AUTH_USER_PW: u8 = 0x02;
const SOCKS5_AUTH_FAILED: u8 = 0xFF;
const SOCKS5_CMD_CONNECT: u8 = 0x01;
// const SOCKS5_CMD_BIND: u8 = 0x02;
// const SOCKS5_CMD_UDP: u8 = 0x03;
const SOCKS5_ADDR_TYPE_IPV4: u8 = 0x01;
const SOCKS5_ATYP_DOMAIN: u8 = 0x03;
const SOCKS5_ATYP_IPV6: u8 = 0x04;
const SOCKS5_FAIL: u8 = 0x01;

impl Channel {
    /// Start a server listening on the given address for the given protocol
    /// Returns the actual SocketAddr it bound to.
    pub async fn start_server(&mut self, addr_str: &str) -> Result<SocketAddr, anyhow::Error> {
        if self.server_mode == false {
            return Err(anyhow!("Cannot start server in client mode"));
        }

        let parsed_addr = addr_str
            .parse::<SocketAddr>()
            .map_err(|e| anyhow!("Invalid server address '{}': {}", addr_str, e))?;

        // Create the TCP listener
        let listener = TcpListener::bind(parsed_addr)
            .await
            .map_err(|e| anyhow!("Failed to bind to {}: {}", parsed_addr, e))?;

        let actual_addr = listener
            .local_addr()
            .map_err(|e| anyhow!("Failed to get local address after bind: {}", e))?;

        info!(
            "Channel({}): Server listening on {}",
            self.channel_id, actual_addr
        );

        // Store the listener for cleanup
        self.local_client_server = Some(Arc::new(listener));
        self.actual_listen_addr = Some(actual_addr);

        // Spawn a task to accept connections
        let (tx, _rx) = oneshot::channel();
        let channel_id = self.channel_id.clone();
        let webrtc = self.webrtc.clone();
        let active_protocol = self.active_protocol;
        let buffer_pool = self.buffer_pool.clone();
        let connection_tx = self.local_client_server_conn_tx.clone();
        let should_exit = self.should_exit.clone();
        let listener_clone = self.local_client_server.clone().unwrap();

        let server_task = tokio::spawn(async move {
            // Signal that we're ready
            let _ = tx.send(());

            let mut next_conn_no = 1;

            while !should_exit.load(std::sync::atomic::Ordering::Relaxed) {
                // Accept a new connection
                match listener_clone.accept().await {
                    Ok((stream, peer_addr)) => {
                        debug!("Channel({}): New connection from {}", channel_id, peer_addr);

                        // Check if peer_addr is localhost
                        let is_localhost = match peer_addr.ip() {
                            std::net::IpAddr::V4(ip) => ip.is_loopback(),
                            std::net::IpAddr::V6(ip) => ip.is_loopback(),
                        };

                        if !is_localhost {
                            if active_protocol == ActiveProtocol::Socks5 {
                                // Split the stream to send a rejection response
                                let (mut reader, mut writer) = stream.into_split();

                                // Read the initial greeting to determine the SOCKS version
                                let mut buf = [0u8; 2];
                                if let Ok(_) = reader.read_exact(&mut buf).await {
                                    // Send connection didn't allow response
                                    let version = buf[0];
                                    if version == SOCKS5_VERSION {
                                        // For SOCKS5, send auth failed first
                                        if let Err(e) = writer
                                            .write_all(&[SOCKS5_VERSION, SOCKS5_AUTH_FAILED])
                                            .await
                                        {
                                            error!("Channel({}): Failed to send SOCKS5 auth failure: {}", channel_id, e);
                                        }
                                    } else {
                                        // For other versions, send general failure
                                        if let Err(e) = send_socks5_response(
                                            &mut writer,
                                            SOCKS5_FAIL,
                                            &[0, 0, 0, 0],
                                            0,
                                            &buffer_pool,
                                        )
                                        .await
                                        {
                                            error!("Channel({}): Failed to send SOCKS5 failure response: {}", channel_id, e);
                                        }
                                    }
                                }
                            }

                            error!(
                                "Channel({}): Connection from non-localhost address rejected",
                                channel_id
                            );
                            continue; // Continue to the next connection attempt
                        }

                        // Handle based on protocol
                        match active_protocol {
                            ActiveProtocol::Socks5 => {
                                let conn_tx_clone = connection_tx.clone().unwrap();
                                let webrtc_clone = webrtc.clone();
                                let buffer_pool_clone = buffer_pool.clone();
                                let task_channel_id = channel_id.clone();
                                let error_log_channel_id = channel_id.clone();
                                let current_conn_no = next_conn_no;
                                next_conn_no += 1;

                                tokio::spawn(async move {
                                    if let Err(e) = handle_socks5_connection(
                                        stream,
                                        current_conn_no,
                                        conn_tx_clone,
                                        webrtc_clone,
                                        buffer_pool_clone,
                                        task_channel_id,
                                    )
                                    .await
                                    {
                                        error!(
                                            "Channel({}): SOCKS5 connection error: {}",
                                            error_log_channel_id, e
                                        );
                                    }
                                });
                            }
                            ActiveProtocol::PortForward | ActiveProtocol::Guacd => {
                                let conn_tx_clone = connection_tx.clone().unwrap();
                                let webrtc_clone = webrtc.clone();
                                let buffer_pool_clone = buffer_pool.clone();
                                let task_channel_id = channel_id.clone();
                                let error_log_channel_id = channel_id.clone();
                                let current_conn_no = next_conn_no;
                                next_conn_no += 1;

                                tokio::spawn(async move {
                                    if let Err(e) = handle_generic_server_connection(
                                        stream,
                                        current_conn_no,
                                        active_protocol,
                                        conn_tx_clone,
                                        webrtc_clone,
                                        buffer_pool_clone,
                                        task_channel_id,
                                    )
                                    .await
                                    {
                                        error!("Channel({}): Generic server connection error for {:?}: {}", error_log_channel_id, active_protocol, e);
                                    }
                                });
                            }
                        }
                    }
                    Err(e) => {
                        error!("Channel({}): Error accepting connection: {}", channel_id, e);
                    }
                }
            }

            debug!("Channel({}): Server task exiting", channel_id);
        });

        self.local_client_server_task = Some(server_task);

        Ok(actual_addr)
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
                    warn!(
                        "Endpoint {}: Server task did not shutdown in time",
                        self.channel_id
                    );
                }
            }
        }

        self.local_client_server = None;
        info!("Endpoint {}: Server stopped", self.channel_id);
        Ok(())
    }
}

/// Handle a SOCKS5 client connection
async fn handle_socks5_connection(
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

    if cmd != SOCKS5_CMD_CONNECT {
        error!(
            "Channel({}): Unsupported SOCKS command: {}",
            channel_id, cmd
        );
        send_socks5_response(&mut writer, 0x07, &[0, 0, 0, 0], 0, &buffer_pool).await?; // Command isn't supported
        return Err(anyhow!("Unsupported SOCKS command"));
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

    // Port
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

        // Use 8KB max read size to stay under SCTP limits
        const MAX_READ_SIZE: usize = 8 * 1024;

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
                    if tracing::enabled!(tracing::Level::DEBUG) {
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
                    let _ = dc.send(encoded).await;

                    // Then close the connection
                    let mut close_buffer = buffer_pool_clone.acquire();
                    close_buffer.clear();
                    close_buffer.extend_from_slice(&conn_no.to_be_bytes());
                    close_buffer
                        .extend_from_slice(&(CloseConnectionReason::Normal as u16).to_be_bytes());

                    let close_frame = Frame::new_control_with_pool(
                        ControlMessage::CloseConnection,
                        &close_buffer,
                        &buffer_pool_clone,
                    );

                    let encoded = close_frame.encode_with_pool(&buffer_pool_clone);
                    let _ = dc.send(encoded).await;

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

                    if let Err(e) = dc.send(encoded).await {
                        error!(
                            "Channel({}): Failed to send data to tunnel: {}",
                            endpoint_name, e
                        );
                        break;
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

        if tracing::enabled!(tracing::Level::DEBUG) {
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
async fn send_socks5_response(
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

/// Handle a generic server-side accepted TCP connection (for PortForward, Guacd server mode)
async fn handle_generic_server_connection(
    stream: TcpStream,
    conn_no: u32,
    active_protocol: ActiveProtocol,
    conn_tx: tokio::sync::mpsc::Sender<(
        u32,
        tokio::net::tcp::OwnedWriteHalf,
        tokio::task::JoinHandle<()>,
    )>,
    webrtc: WebRTCDataChannel,
    buffer_pool: crate::buffer_pool::BufferPool,
    channel_id: String,
) -> Result<()> {
    debug!(
        "Channel({}): New generic {:?} connection {}",
        channel_id, active_protocol, conn_no
    );

    // 1. Send OpenConnection control message over WebRTC to the other side.
    // The payload of OpenConnection should just be the conn_no for now.
    // The other side, upon receiving this, will know it needs to prepare for a new stream on this conn_no.
    // For PortForward server mode, the other side will connect to its pre-configured target.
    // For Guacd server mode, the other side will expect Guacamole data on this conn_no from a Guacd client (which is this accepted stream).

    let open_conn_payload = conn_no.to_be_bytes();
    let open_frame = Frame::new_control_with_pool(
        ControlMessage::OpenConnection,
        &open_conn_payload,
        &buffer_pool,
    );
    webrtc
        .send(open_frame.encode_with_pool(&buffer_pool))
        .await
        .map_err(|e| {
            anyhow!(
                "Failed to send OpenConnection for new server stream {}: {}",
                conn_no,
                e
            )
        })?;

    debug!(
        "Channel({}): Sent OpenConnection for new server stream {} ({:?})",
        channel_id, conn_no, active_protocol
    );

    // 2. Split the accepted TCP stream.
    let (mut reader, writer) = stream.into_split();

    // 3. Spawn a task to read from this accepted TCP stream and send data frames over WebRTC.
    let dc_clone = webrtc.clone();
    let buffer_pool_clone = buffer_pool.clone();
    let channel_id_clone = channel_id.clone();

    let read_task = tokio::spawn(async move {
        let mut read_buffer = buffer_pool_clone.acquire();
        let mut encode_buffer = buffer_pool_clone.acquire();

        // Use 8KB max read size to stay under SCTP limits
        const MAX_READ_SIZE: usize = 8 * 1024;

        // Add a print statement to see the conn_no being used
        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!(
                "Server-side TCP reader task started for conn_no: {}",
                conn_no
            );
        }

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
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(
                            "Server-side TCP reader received EOF for conn_no: {}",
                            conn_no
                        );
                        debug!(
                            "Channel({}): Client on server port (conn_no {}) sent EOF.",
                            channel_id_clone, conn_no
                        );
                    }
                    let eof_payload = conn_no.to_be_bytes();
                    let eof_frame = Frame::new_control_with_pool(
                        ControlMessage::SendEOF,
                        &eof_payload,
                        &buffer_pool_clone,
                    );
                    if let Err(e) = dc_clone
                        .send(eof_frame.encode_with_pool(&buffer_pool_clone))
                        .await
                    {
                        error!(
                            "Channel({}): Failed to send EOF for conn_no {}: {}",
                            channel_id_clone, conn_no, e
                        );
                    }
                    break;
                }
                Ok(n) if n > 0 => {
                    // Advance the buffer by the number of bytes read
                    unsafe {
                        read_buffer.advance_mut(n);
                    }

                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(
                            "Server-side TCP reader received {} bytes for conn_no: {}",
                            n, conn_no
                        );

                        // Print part of the data for debugging
                        if n <= 100 {
                            debug!("Data: {:?}", &read_buffer[0..n]);
                        } else {
                            debug!("Data (first 50 bytes): {:?}", &read_buffer[0..50]);
                        }
                    }

                    let current_payload_bytes_slice = &read_buffer[0..n];

                    // Directly create a single data frame with the connection number
                    let data_frame = Frame::new_data_with_pool(
                        conn_no,
                        current_payload_bytes_slice,
                        &buffer_pool_clone,
                    );

                    // Encode it into a separate buffer
                    encode_buffer.clear();
                    let bytes_written = data_frame.encode_into(&mut encode_buffer);

                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!("Encoded frame with conn_no {} and {} bytes payload into {} total bytes",
                            conn_no, n, bytes_written);
                    }

                    // Freeze to get a Bytes instance we can send
                    let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();

                    // Send it directly without checking the buffer amount
                    match dc_clone.send(encoded_frame_bytes).await {
                        Ok(_) => {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!("Successfully sent data frame for conn_no {}", conn_no);
                            }
                        }
                        Err(e) => {
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                debug!("Failed to send data frame for conn_no {}: {}", conn_no, e);
                            }
                            break;
                        }
                    }
                }
                Ok(_) => { /* n == 0 but not EOF, should not happen with read_buf if it doesn't return Ok(0) */
                }
                Err(e) => {
                    if tracing::enabled!(tracing::Level::DEBUG) {
                        debug!(
                            "Error reading from client on server port (conn_no {}): {}",
                            conn_no, e
                        );
                    }
                    error!(
                        "Channel({}): Error reading from client on server port (conn_no {}): {}",
                        channel_id_clone, conn_no, e
                    );
                    break; // Exit read task
                }
            }
        }
        buffer_pool_clone.release(read_buffer);
        buffer_pool_clone.release(encode_buffer);
        // Add a print statement to see the conn_no being used
        if tracing::enabled!(tracing::Level::DEBUG) {
            debug!("Server-side TCP reader task for conn_no {} exited", conn_no);
            debug!(
                "Channel({}): Server-side TCP reader task for conn_no {} ({:?}) exited.",
                channel_id_clone, conn_no, active_protocol
            );
        }
    });

    // 4. Send the writer half and the reader_task handle to the main Channel loop via conn_tx.
    conn_tx
        .send((conn_no, writer, read_task))
        .await
        .map_err(|e| {
            anyhow!(
                "Failed to send new server connection {} to channel: {}",
                conn_no,
                e
            )
        })?;

    Ok(())
}
