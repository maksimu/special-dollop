// Server functionality for the Channel implementation

use crate::tube_protocol::{ControlMessage, Frame};
use crate::unlikely;
use crate::webrtc_data_channel::{EventDrivenSender, WebRTCDataChannel, STANDARD_BUFFER_THRESHOLD};
use anyhow::{anyhow, Result};
use bytes::BufMut;
use log::{debug, error, info, warn};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::core::Channel;
use super::socks5::{self, SOCKS5_AUTH_FAILED, SOCKS5_FAIL, SOCKS5_VERSION};
use super::types::ActiveProtocol;

impl Channel {
    /// Start a server listening on the given address for the given protocol
    /// Returns the actual SocketAddr it bound to.
    pub async fn start_server(&mut self, addr_str: &str) -> Result<SocketAddr, anyhow::Error> {
        if !self.server_mode {
            return Err(anyhow!("Cannot start server in client mode"));
        }

        let parsed_addr = addr_str
            .parse::<SocketAddr>()
            .map_err(|e| anyhow!("Invalid server address '{}': {}", addr_str, e))?;

        // Create the TCP listener with SO_REUSEADDR for immediate reuse after shutdown
        let domain = if parsed_addr.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
            .map_err(|e| anyhow!("Failed to create socket: {}", e))?;

        socket
            .set_reuse_address(true)
            .map_err(|e| anyhow!("Failed to set SO_REUSEADDR: {}", e))?;

        // On Unix systems, also set SO_REUSEPORT for more immediate reuse
        #[cfg(unix)]
        socket
            .set_reuse_port(true)
            .map_err(|e| anyhow!("Failed to set SO_REUSEPORT: {}", e))?;

        socket
            .set_nonblocking(true)
            .map_err(|e| anyhow!("Failed to set socket as non-blocking: {}", e))?;

        socket
            .bind(&parsed_addr.into())
            .map_err(|e| anyhow!("Failed to bind to {}: {}", parsed_addr, e))?;

        socket
            .listen(128)
            .map_err(|e| anyhow!("Failed to listen on {}: {}", parsed_addr, e))?;

        let listener = TcpListener::from_std(socket.into())
            .map_err(|e| anyhow!("Failed to convert socket to TcpListener: {}", e))?;

        let actual_addr = listener
            .local_addr()
            .map_err(|e| anyhow!("Failed to get local address after bind: {}", e))?;

        debug!(
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
                                if reader.read_exact(&mut buf).await.is_ok() {
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
                                        if let Err(e) = socks5::send_socks5_response(
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
                                    if let Err(e) = socks5::handle_socks5_connection(
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

        // Explicitly drop the listener reference to ensure immediate socket release
        if let Some(listener) = self.local_client_server.take() {
            drop(listener);
        }

        // Give the system a brief moment to fully release the socket
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        info!("Endpoint {}: Server stopped", self.channel_id);
        Ok(())
    }
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

        // Use 64KB max read size - maximum safe size under webrtc-rs limits
        // 64KB payload + 17 byte frame overhead = 65,553 bytes total
        // Safely under webrtc-rs hard limit (~96KB message size)
        // Larger frames = fewer frames = less overhead = higher throughput
        // Tested: 98KB+ frames fail with "outbound packet larger than maximum message size"
        const MAX_READ_SIZE: usize = 64 * 1024;

        // Create EventDrivenSender for proper backpressure management
        // This prevents overwhelming WebRTC buffers and provides natural flow control
        let event_sender =
            EventDrivenSender::new(Arc::new(dc_clone.clone()), STANDARD_BUFFER_THRESHOLD);

        // Add a print statement to see the conn_no being used
        if unlikely!(crate::logger::is_verbose_logging()) {
            debug!(
                "Server-side TCP reader task started for conn_no: {} with EventDrivenSender",
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
                    if unlikely!(crate::logger::is_verbose_logging()) {
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
                    if let Err(e) = event_sender
                        .send_with_natural_backpressure(
                            eof_frame.encode_with_pool(&buffer_pool_clone),
                        )
                        .await
                    {
                        error!(
                            "Channel({}): Failed to send EOF via EventDrivenSender for conn_no {}: {}",
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

                    if unlikely!(crate::logger::is_verbose_logging()) {
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

                    if unlikely!(crate::logger::is_verbose_logging()) {
                        debug!("Encoded frame with conn_no {} and {} bytes payload into {} total bytes",
                            conn_no, n, bytes_written);
                    }

                    // Freeze to get a Bytes instance we can send
                    let encoded_frame_bytes = encode_buffer.split_to(bytes_written).freeze();

                    // Use EventDrivenSender for proper backpressure management
                    let send_start = std::time::Instant::now();
                    match event_sender
                        .send_with_natural_backpressure(encoded_frame_bytes.clone())
                        .await
                    {
                        Ok(_) => {
                            let send_latency = send_start.elapsed();

                            // Record metrics for message sent (using channel_id as conversation_id)
                            crate::metrics::METRICS_COLLECTOR.record_message_sent(
                                &channel_id_clone,
                                encoded_frame_bytes.len() as u64,
                                Some(send_latency),
                            );

                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("Successfully sent data frame via EventDrivenSender for conn_no {}", conn_no);
                            }
                        }
                        Err(e) => {
                            // Record error metrics
                            crate::metrics::METRICS_COLLECTOR
                                .record_error(&channel_id_clone, "webrtc_send_failed");

                            if unlikely!(crate::logger::is_verbose_logging()) {
                                debug!("Failed to send data frame via EventDrivenSender for conn_no {}: {}", conn_no, e);
                            }
                            break;
                        }
                    }
                }
                Ok(_) => { /* n == 0 but not EOF, should not happen with read_buf if it doesn't return Ok(0) */
                }
                Err(e) => {
                    if unlikely!(crate::logger::is_verbose_logging()) {
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
        if unlikely!(crate::logger::is_verbose_logging()) {
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
