use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};

use super::core::Channel;
use crate::tube_protocol::{ControlMessage, CONN_NO_LEN};

// UDP Association Management
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

/// Tracks an active UDP association for response handling
#[derive(Debug)]
pub(crate) struct UdpAssociation {
    pub(crate) socket: Arc<UdpSocket>,
    // Used indirectly through closure capture in response_task, but the compiler can't detect it
    #[allow(dead_code)]
    pub(crate) client_addr: std::net::SocketAddr,
    pub(crate) conn_no: u32,
    pub(crate) last_activity: std::time::Instant,
    pub(crate) response_task: tokio::task::JoinHandle<()>,
}

/// Global UDP association manager
pub(crate) type UdpAssociations = Arc<Mutex<HashMap<std::net::SocketAddr, UdpAssociation>>>;

impl Channel {
    /// Handle a UDP associate request from server side
    pub(crate) async fn handle_udp_associate(&mut self, data: &[u8]) -> Result<()> {
        use bytes::Buf;

        if data.len() < CONN_NO_LEN + 2 {
            return Err(anyhow!("UdpAssociate message too short"));
        }

        let mut cursor = std::io::Cursor::new(data);
        let conn_no = cursor.get_u32();
        let relay_port = cursor.get_u16();

        debug!(
            "Client received UdpAssociate for connection {} with relay port {} (channel_id: {})",
            conn_no, relay_port, self.channel_id
        );

        // Create UDP socket for this association using dual-stack binding
        let udp_socket = crate::models::dual_stack::bind_udp_localhost(0).await?;
        let local_addr = udp_socket.local_addr()?;

        info!(
            "Channel({}): UDP association {} bound to {}",
            self.channel_id, conn_no, local_addr
        );

        // Store UDP socket in connections map (we'll need a new connection type for UDP)
        // For now, we'll use a special marker in the connection map

        // Send UdpAssociateOpened response
        let mut response_data = self.buffer_pool.acquire();
        response_data.clear();
        response_data.extend_from_slice(&conn_no.to_be_bytes());
        response_data.extend_from_slice(&local_addr.port().to_be_bytes());

        self.send_control_message(ControlMessage::UdpAssociateOpened, &response_data)
            .await?;
        self.buffer_pool.release(response_data);

        // Set up UDP forwarding task
        let webrtc_clone = self.webrtc.clone();
        let buffer_pool_clone = self.buffer_pool.clone();
        let channel_id_clone = self.channel_id.clone();

        let udp_receiver_task = tokio::spawn(async move {
            let mut buf = [0u8; 65536];

            loop {
                match udp_socket.recv_from(&mut buf).await {
                    Ok((len, peer_addr)) => {
                        debug!(
                            "Channel({}): Received {} bytes from {} for UDP association {}",
                            channel_id_clone, len, peer_addr, conn_no
                        );

                        // Forward UDP packet through the tunnel
                        if let Err(e) = forward_udp_packet_to_tunnel(
                            &buf[..len],
                            peer_addr,
                            conn_no,
                            &webrtc_clone,
                            &buffer_pool_clone,
                        )
                        .await
                        {
                            error!(
                                "Channel({}): Error forwarding UDP packet: {}",
                                channel_id_clone, e
                            );
                        }
                    }
                    Err(e) => {
                        error!(
                            "Channel({}): UDP socket error for association {}: {}",
                            channel_id_clone, conn_no, e
                        );
                        break;
                    }
                }
            }

            debug!(
                "Channel({}): UDP task for association {} exited",
                channel_id_clone, conn_no
            );
        });

        // RAII FIX: Store task handle for proper cleanup
        self.udp_receiver_tasks
            .lock()
            .await
            .insert(conn_no, udp_receiver_task);

        debug!(
            "Channel({}): UDP receiver task tracked for connection {} (RAII)",
            self.channel_id, conn_no
        );

        Ok(())
    }

    /// Handle UDP associate opened response  
    pub(crate) async fn handle_udp_associate_opened(&mut self, data: &[u8]) -> Result<()> {
        use bytes::Buf;

        if data.len() < CONN_NO_LEN + 2 {
            return Err(anyhow!("UdpAssociateOpened message too short"));
        }

        let mut cursor = std::io::Cursor::new(data);
        let conn_no = cursor.get_u32();
        let client_port = cursor.get_u16();

        debug!("Server received UdpAssociateOpened for connection {} with client port {} (channel_id: {})",
               conn_no, client_port, self.channel_id);

        // In server mode, this confirms the UDP association is ready
        // We might need to notify the SOCKS5 client that UDP relay is ready

        Ok(())
    }

    /// Handle UDP packet forwarding
    pub(crate) async fn handle_udp_packet(&mut self, data: &[u8]) -> Result<()> {
        use bytes::Buf;

        if data.len() < CONN_NO_LEN + 1 + 4 + 4 + 2 {
            return Err(anyhow!("UdpPacket message too short"));
        }

        let mut cursor = std::io::Cursor::new(data);
        let conn_no = cursor.get_u32();

        // Parse client address type and address
        let client_addr_type = cursor.get_u8();
        let client_addr = match client_addr_type {
            0x01 => {
                // IPv4
                if cursor.remaining() < 4 {
                    return Err(anyhow!("UdpPacket too short for IPv4 address"));
                }
                let addr = [
                    cursor.get_u8(),
                    cursor.get_u8(),
                    cursor.get_u8(),
                    cursor.get_u8(),
                ];
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(addr))
            }
            0x04 => {
                // IPv6
                if cursor.remaining() < 16 {
                    return Err(anyhow!("UdpPacket too short for IPv6 address"));
                }
                let mut addr = [0u8; 16];
                for byte in &mut addr {
                    *byte = cursor.get_u8();
                }
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(addr))
            }
            _ => {
                return Err(anyhow!(
                    "Unsupported client address type: {}",
                    client_addr_type
                ));
            }
        };

        let client_port = cursor.get_u16();
        let client_socket_addr = std::net::SocketAddr::new(client_addr, client_port);

        // Parse destination host
        if cursor.remaining() < 4 {
            return Err(anyhow!("UdpPacket too short for host length"));
        }
        let host_len = cursor.get_u32() as usize;

        if cursor.remaining() < host_len + 2 {
            return Err(anyhow!("UdpPacket too short for host and port"));
        }

        let mut host_bytes = vec![0u8; host_len];
        cursor.copy_to_slice(&mut host_bytes);
        let dest_host = String::from_utf8(host_bytes)?;
        let dest_port = cursor.get_u16();

        // Remaining data is the UDP payload
        let payload = &data[cursor.position() as usize..];

        debug!("Processing UDP packet for connection {}: client={}, dest={}:{}, payload={} bytes (channel_id: {})",
               conn_no, client_socket_addr, dest_host, dest_port, payload.len(), self.channel_id);

        // Network access check if configured
        if let Some(ref checker) = self.network_checker {
            match checker.resolve_if_allowed(&dest_host).await {
                Some(resolved_ips) => {
                    if !checker.is_port_allowed(dest_port) {
                        error!(
                            "UDP packet to {}:{} port not allowed (channel_id: {})",
                            dest_host, dest_port, self.channel_id
                        );
                        return Ok(()); // Drop the packet silently
                    }

                    // Use first resolved IP
                    if let Some(&dest_ip) = resolved_ips.first() {
                        let dest_socket_addr = std::net::SocketAddr::new(dest_ip, dest_port);

                        // Forward UDP packet to destination
                        match forward_udp_packet_to_destination(
                            payload,
                            dest_socket_addr,
                            client_socket_addr,
                            conn_no,
                            self,
                        )
                        .await
                        {
                            Ok(_) => {
                                debug!("UDP packet forwarded successfully to {}", dest_socket_addr);
                            }
                            Err(e) => {
                                error!(
                                    "Failed to forward UDP packet to {}: {}",
                                    dest_socket_addr, e
                                );
                            }
                        }
                    } else {
                        error!("No IP addresses resolved for UDP destination {}", dest_host);
                    }
                }
                None => {
                    error!(
                        "UDP packet to {} not allowed or unresolvable (channel_id: {})",
                        dest_host, self.channel_id
                    );
                }
            }
        } else {
            // No network checker - allow all destinations
            match tokio::net::lookup_host(format!("{dest_host}:{dest_port}")).await {
                Ok(mut addrs) => {
                    if let Some(dest_socket_addr) = addrs.next() {
                        match forward_udp_packet_to_destination(
                            payload,
                            dest_socket_addr,
                            client_socket_addr,
                            conn_no,
                            self,
                        )
                        .await
                        {
                            Ok(_) => {
                                debug!("UDP packet forwarded successfully to {}", dest_socket_addr);
                            }
                            Err(e) => {
                                error!(
                                    "Failed to forward UDP packet to {}: {}",
                                    dest_socket_addr, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to resolve UDP destination {}:{}: {}",
                        dest_host, dest_port, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Handle UDP associate closed
    pub(crate) async fn handle_udp_associate_closed(&mut self, data: &[u8]) -> Result<()> {
        use bytes::Buf;

        if data.len() < CONN_NO_LEN {
            return Err(anyhow!("UdpAssociateClosed message too short"));
        }

        let mut cursor = std::io::Cursor::new(data);
        let conn_no = cursor.get_u32();

        debug!(
            "UDP association {} closed (channel_id: {})",
            conn_no, self.channel_id
        );

        // Clean up UDP associations related to this connection
        self.cleanup_udp_associations_for_connection(conn_no)
            .await?;

        Ok(())
    }

    /// Clean up UDP associations for a specific connection - OPTIMIZED with reverse index
    pub(crate) async fn cleanup_udp_associations_for_connection(
        &mut self,
        conn_no: u32,
    ) -> Result<()> {
        // **FAST O(1) LOOKUP**: Get all destination addresses for this connection
        let dest_addrs_to_remove = {
            let mut conn_index = self.udp_conn_index.lock().unwrap();
            conn_index.remove(&conn_no).unwrap_or_default()
        };

        if dest_addrs_to_remove.is_empty() {
            debug!(
                "Channel({}): No UDP associations found for connection {}",
                self.channel_id, conn_no
            );
            return Ok(());
        }

        // **FAST O(k) CLEANUP**: Remove only the specific associations (k = number of associations for this conn)
        let mut associations = self.udp_associations.lock().await;
        for dest_addr in &dest_addrs_to_remove {
            if let Some(association) = associations.remove(dest_addr) {
                association.response_task.abort();
                debug!(
                    "Channel({}): Cleaned up UDP association for {} (connection {})",
                    self.channel_id, dest_addr, conn_no
                );
            }
        }

        // RAII FIX: Clean up client-side UDP receiver task if present
        let mut receiver_tasks = self.udp_receiver_tasks.lock().await;
        if let Some(receiver_task) = receiver_tasks.remove(&conn_no) {
            receiver_task.abort();
            debug!(
                "Channel({}): Aborted UDP receiver task for connection {} (RAII cleanup)",
                self.channel_id, conn_no
            );
        }

        debug!(
            "Channel({}): Cleaned up {} UDP associations for connection {}",
            self.channel_id,
            dest_addrs_to_remove.len(),
            conn_no
        );

        Ok(())
    }
}

/// Forward a UDP packet to the tunnel (used by client-side UDP associations)
pub(crate) async fn forward_udp_packet_to_tunnel(
    packet_data: &[u8],
    peer_addr: std::net::SocketAddr,
    conn_no: u32,
    webrtc: &crate::webrtc_data_channel::WebRTCDataChannel,
    buffer_pool: &crate::buffer_pool::BufferPool,
) -> Result<()> {
    use bytes::BufMut;

    // Create UdpPacket message for the tunnel (client-to-server direction)
    let mut udp_data = buffer_pool.acquire();
    udp_data.clear();

    // Connection number
    udp_data.put_u32(conn_no);

    // Client address (for return packets)
    match peer_addr.ip() {
        std::net::IpAddr::V4(ipv4) => {
            udp_data.put_u8(0x01); // IPv4
            udp_data.extend_from_slice(&ipv4.octets());
        }
        std::net::IpAddr::V6(ipv6) => {
            udp_data.put_u8(0x04); // IPv6
            udp_data.extend_from_slice(&ipv6.octets());
        }
    }
    udp_data.put_u16(peer_addr.port());

    // Parse SOCKS5 UDP relay packet format (RFC 1928 Section 7)
    // Format: [RSV:2][FRAG:1][ATYP:1][DST.ADDR:var][DST.PORT:2][DATA:var]

    if packet_data.len() < 10 {
        return Err(anyhow::anyhow!("Invalid SOCKS5 UDP packet: too short"));
    }

    let mut cursor = 0;

    // Skip reserved (2 bytes) and fragment (1 byte)
    cursor += 3;

    let atyp = packet_data[cursor];
    cursor += 1;

    let (destination_host, destination_port, udp_payload_start) = match atyp {
        0x01 => {
            // IPv4 address (4 bytes)
            if packet_data.len() < cursor + 6 {
                return Err(anyhow::anyhow!(
                    "Invalid SOCKS5 UDP packet: incomplete IPv4"
                ));
            }
            let ip = std::net::Ipv4Addr::new(
                packet_data[cursor],
                packet_data[cursor + 1],
                packet_data[cursor + 2],
                packet_data[cursor + 3],
            );
            cursor += 4;
            let port = u16::from_be_bytes([packet_data[cursor], packet_data[cursor + 1]]);
            cursor += 2;
            (ip.to_string(), port, cursor)
        }
        0x03 => {
            // Domain name (1 byte length + domain)
            if packet_data.len() < cursor + 1 {
                return Err(anyhow::anyhow!(
                    "Invalid SOCKS5 UDP packet: missing domain length"
                ));
            }
            let domain_len = packet_data[cursor] as usize;
            cursor += 1;
            if packet_data.len() < cursor + domain_len + 2 {
                return Err(anyhow::anyhow!(
                    "Invalid SOCKS5 UDP packet: incomplete domain"
                ));
            }
            let domain = String::from_utf8(packet_data[cursor..cursor + domain_len].to_vec())
                .map_err(|_| anyhow::anyhow!("Invalid UTF-8 in domain name"))?;
            cursor += domain_len;
            let port = u16::from_be_bytes([packet_data[cursor], packet_data[cursor + 1]]);
            cursor += 2;
            (domain, port, cursor)
        }
        0x04 => {
            // IPv6 address (16 bytes)
            if packet_data.len() < cursor + 18 {
                return Err(anyhow::anyhow!(
                    "Invalid SOCKS5 UDP packet: incomplete IPv6"
                ));
            }
            let ip = std::net::Ipv6Addr::from([
                packet_data[cursor],
                packet_data[cursor + 1],
                packet_data[cursor + 2],
                packet_data[cursor + 3],
                packet_data[cursor + 4],
                packet_data[cursor + 5],
                packet_data[cursor + 6],
                packet_data[cursor + 7],
                packet_data[cursor + 8],
                packet_data[cursor + 9],
                packet_data[cursor + 10],
                packet_data[cursor + 11],
                packet_data[cursor + 12],
                packet_data[cursor + 13],
                packet_data[cursor + 14],
                packet_data[cursor + 15],
            ]);
            cursor += 16;
            let port = u16::from_be_bytes([packet_data[cursor], packet_data[cursor + 1]]);
            cursor += 2;
            (ip.to_string(), port, cursor)
        }
        _ => return Err(anyhow::anyhow!("Unsupported SOCKS5 address type: {}", atyp)),
    };

    // Destination host length and host
    let host_bytes = destination_host.as_bytes();
    udp_data.put_u32(host_bytes.len() as u32);
    udp_data.extend_from_slice(host_bytes);

    // Destination port
    udp_data.put_u16(destination_port);

    // UDP payload (actual data starts at udp_payload_start)
    udp_data.extend_from_slice(&packet_data[udp_payload_start..]);

    let frame = crate::tube_protocol::Frame::new_control_with_pool(
        ControlMessage::UdpPacket,
        &udp_data,
        buffer_pool,
    );
    let encoded = frame.encode_with_pool(buffer_pool);
    buffer_pool.release(udp_data);

    webrtc
        .send(encoded)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send UDP packet: {}", e))?;

    Ok(())
}

/// Forward a UDP packet to its destination (used by server-side UDP forwarding)
pub(crate) async fn forward_udp_packet_to_destination(
    payload: &[u8],
    dest_addr: std::net::SocketAddr,
    client_addr: std::net::SocketAddr,
    conn_no: u32,
    channel: &Channel,
) -> Result<()> {
    let udp_associations = channel.udp_associations.clone();
    let udp_conn_index = channel.udp_conn_index.clone();

    // Check if we already have an association for this destination
    let mut associations = udp_associations.lock().await;

    if let Some(association) = associations.get_mut(&dest_addr) {
        // Update last activity and reuse existing socket
        association.last_activity = std::time::Instant::now();

        // Send packet using existing socket
        let bytes_sent = association.socket.send_to(payload, dest_addr).await?;
        debug!(
            "Sent {} bytes to {} using existing association for UDP connection {}",
            bytes_sent, dest_addr, conn_no
        );
    } else {
        // Create new UDP socket and association using dual-stack binding
        let socket = Arc::new(crate::models::dual_stack::bind_udp_dual_stack(0).await?);

        // Send the packet immediately
        let bytes_sent = socket.send_to(payload, dest_addr).await?;
        debug!(
            "Sent {} bytes to {} using new association for UDP connection {}",
            bytes_sent, dest_addr, conn_no
        );

        // Set up response listening task
        let socket_clone = socket.clone();
        let webrtc_clone = channel.webrtc.clone();
        let buffer_pool_clone = channel.buffer_pool.clone();
        let channel_id_clone = channel.channel_id.clone();
        let udp_associations_clone = udp_associations.clone();
        let udp_conn_index_for_task = udp_conn_index.clone(); // FIX: Clone before async move

        let response_task = tokio::spawn(async move {
            let mut response_buf = [0u8; 65536];
            let socket_timeout = std::time::Duration::from_secs(300); // 5-minute timeout

            debug!(
                "Channel({}): Starting UDP response listener for {}",
                channel_id_clone, dest_addr
            );

            loop {
                match tokio::time::timeout(
                    socket_timeout,
                    socket_clone.recv_from(&mut response_buf),
                )
                .await
                {
                    Ok(Ok((len, response_from))) => {
                        if response_from == dest_addr {
                            debug!("Channel({}): Received {} byte response from {} for UDP connection {}", 
                                   channel_id_clone, len, dest_addr, conn_no);

                            // Forward response back through a tunnel
                            if let Err(e) = forward_udp_response_to_tunnel(
                                &response_buf[..len],
                                client_addr,
                                dest_addr,
                                conn_no,
                                &webrtc_clone,
                                &buffer_pool_clone,
                            )
                            .await
                            {
                                error!(
                                    "Channel({}): Error forwarding UDP response: {}",
                                    channel_id_clone, e
                                );
                            }

                            // Update last activity
                            {
                                let mut associations = udp_associations_clone.lock().await;
                                if let Some(assoc) = associations.get_mut(&dest_addr) {
                                    assoc.last_activity = std::time::Instant::now();
                                }
                            }
                        } else {
                            warn!("Channel({}): Received UDP packet from unexpected source {} (expected {})", 
                                  channel_id_clone, response_from, dest_addr);
                        }
                    }
                    Ok(Err(e)) => {
                        error!(
                            "Channel({}): UDP socket error for {}: {}",
                            channel_id_clone, dest_addr, e
                        );
                        break;
                    }
                    Err(_) => {
                        // Timeout - check if association should be cleaned up
                        let should_cleanup = {
                            let associations = udp_associations_clone.lock().await;
                            if let Some(assoc) = associations.get(&dest_addr) {
                                assoc.last_activity.elapsed() > std::time::Duration::from_secs(300)
                            } else {
                                true // Association was already removed
                            }
                        };

                        if should_cleanup {
                            debug!(
                                "Channel({}): UDP association for {} timed out, cleaning up",
                                channel_id_clone, dest_addr
                            );
                            break;
                        }
                    }
                }
            }

            // Cleanup association AND reverse index
            {
                let mut associations = udp_associations_clone.lock().await;
                if let Some(removed_association) = associations.remove(&dest_addr) {
                    // Clean up reverse index too
                    {
                        let mut conn_index = udp_conn_index_for_task.lock().unwrap();
                        if let Some(addr_set) = conn_index.get_mut(&removed_association.conn_no) {
                            addr_set.remove(&dest_addr);
                            if addr_set.is_empty() {
                                conn_index.remove(&removed_association.conn_no);
                            }
                        }
                    }
                    debug!(
                        "Channel({}): Cleaned up UDP association for {} and reverse index",
                        channel_id_clone, dest_addr
                    );
                }
            }
        });

        // Create and store the association
        let association = UdpAssociation {
            socket,
            client_addr,
            conn_no,
            last_activity: std::time::Instant::now(),
            response_task,
        };

        associations.insert(dest_addr, association);

        // **UPDATE REVERSE INDEX**: Add dest_addr to conn_no's set
        {
            let mut conn_index = udp_conn_index.lock().unwrap();
            conn_index.entry(conn_no).or_default().insert(dest_addr);
        }

        debug!(
            "Channel({}): Created new UDP association for {} with client {} (conn_no: {})",
            channel.channel_id, dest_addr, client_addr, conn_no
        );
    }

    Ok(())
}

/// Forward a UDP response packet back to the client through the tunnel
pub(crate) async fn forward_udp_response_to_tunnel(
    response_payload: &[u8],
    client_addr: std::net::SocketAddr,
    source_addr: std::net::SocketAddr,
    conn_no: u32,
    webrtc: &crate::webrtc_data_channel::WebRTCDataChannel,
    buffer_pool: &crate::buffer_pool::BufferPool,
) -> Result<()> {
    use bytes::BufMut;

    // Create SOCKS5 UDP response packet
    let mut response_data = buffer_pool.acquire();
    response_data.clear();

    // SOCKS5 UDP packet format for response:
    // +----+------+------+----------+----------+----------+
    // |RSV | FRAG | ATYP | SRC.ADDR | SRC.PORT |   DATA   |
    // +----+------+------+----------+----------+----------+
    // | 2  |  1   |  1   | Variable |    2     | Variable |

    // Reserved (2 bytes)
    response_data.put_u16(0);

    // Fragment (1 byte) - 0 for no fragmentation
    response_data.put_u8(0);

    // Address type and source address
    match source_addr.ip() {
        std::net::IpAddr::V4(ipv4) => {
            response_data.put_u8(0x01); // IPv4
            response_data.extend_from_slice(&ipv4.octets());
        }
        std::net::IpAddr::V6(ipv6) => {
            response_data.put_u8(0x04); // IPv6
            response_data.extend_from_slice(&ipv6.octets());
        }
    }

    // Source port
    response_data.put_u16(source_addr.port());

    // Response payload
    response_data.extend_from_slice(response_payload);

    // Create a tunnel message with client address info for routing
    let mut tunnel_data = buffer_pool.acquire();
    tunnel_data.clear();

    // Connection number
    tunnel_data.put_u32(conn_no);

    // Client address for routing response
    match client_addr.ip() {
        std::net::IpAddr::V4(ipv4) => {
            tunnel_data.put_u8(0x01); // IPv4
            tunnel_data.extend_from_slice(&ipv4.octets());
        }
        std::net::IpAddr::V6(ipv6) => {
            tunnel_data.put_u8(0x04); // IPv6
            tunnel_data.extend_from_slice(&ipv6.octets());
        }
    }
    tunnel_data.put_u16(client_addr.port());

    // SOCKS5-formatted response data
    tunnel_data.extend_from_slice(&response_data);

    let frame = crate::tube_protocol::Frame::new_control_with_pool(
        ControlMessage::UdpPacket,
        &tunnel_data,
        buffer_pool,
    );
    let encoded = frame.encode_with_pool(buffer_pool);

    buffer_pool.release(response_data);
    buffer_pool.release(tunnel_data);

    webrtc
        .send(encoded)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send UDP response: {}", e))?;

    Ok(())
}
