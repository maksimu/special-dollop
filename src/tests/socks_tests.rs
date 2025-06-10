//! SOCKS5 functionality tests
use crate::buffer_pool::BufferPool;
use crate::models::NetworkAccessChecker;
use crate::runtime::get_runtime;
use crate::tube_protocol::{ControlMessage, Frame, CONN_NO_LEN, PORT_LENGTH};
use bytes::{BufMut, BytesMut};
// IP types and Duration are not directly used in tests

#[test]
fn test_network_access_checker_basic() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let allowed_hosts = vec![
            "localhost".to_string(),      // Real hostname that resolves
            "*.google.com".to_string(),   // Real wildcard domain
            "192.168.1.0/24".to_string(), // IP network (no DNS needed)
        ];
        let allowed_ports = vec![80, 443];

        let checker = NetworkAccessChecker::new(allowed_hosts, allowed_ports);

        // Test allowed hosts (using resolve_if_allowed instead of is_host_allowed)
        assert!(checker.resolve_if_allowed("localhost").await.is_some());
        assert!(checker
            .resolve_if_allowed("definitely-not-real-domain-12345.com")
            .await
            .is_none());

        // Test wildcard matching with real Google domains
        assert!(checker
            .resolve_if_allowed("mail.google.com")
            .await
            .is_some());
        assert!(checker
            .resolve_if_allowed("drive.google.com")
            .await
            .is_some());
        assert!(checker.resolve_if_allowed("google.com").await.is_none()); // *.google.com doesn't match google.com itself
        assert!(checker.resolve_if_allowed("fakegoogle.com").await.is_none());

        // Test IP network matching (no DNS needed)
        assert!(checker.resolve_if_allowed("192.168.1.100").await.is_some());
        assert!(checker.resolve_if_allowed("192.168.1.1").await.is_some());
        assert!(checker.resolve_if_allowed("192.168.2.100").await.is_none());
        assert!(checker.resolve_if_allowed("10.0.0.1").await.is_none());

        // Test port checking
        assert!(checker.is_port_allowed(80));
        assert!(checker.is_port_allowed(443));
        assert!(!checker.is_port_allowed(21));
        assert!(!checker.is_port_allowed(3389));
    });
}

#[test]
fn test_network_access_checker_empty_rules() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Test with empty rules (should allow everything)
        let allowed_hosts = vec![];
        let allowed_ports = vec![];

        let checker = NetworkAccessChecker::new(allowed_hosts, allowed_ports);

        // With empty rules, everything should be allowed (but still needs to resolve)
        assert!(checker.resolve_if_allowed("localhost").await.is_some());
        // Non-existent domains will still fail DNS resolution even with empty rules
        // assert!(checker.resolve_if_allowed("malicious.com").await.is_some());
        assert!(checker.resolve_if_allowed("127.0.0.1").await.is_some()); // Direct IP works

        // Empty port list should allow all ports
        assert!(checker.is_port_allowed(80));
        assert!(checker.is_port_allowed(21));
        assert!(checker.is_port_allowed(3389));
    });
}

#[test]
fn test_network_access_checker_ipv6() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Test IPv6 network support
        let allowed_hosts = vec![
            "2001:db8::/32".to_string(), // IPv6 CIDR network
            "::1/128".to_string(),       // IPv6 localhost
        ];
        let allowed_ports = vec![80, 443];

        let checker = NetworkAccessChecker::new(allowed_hosts, allowed_ports);

        // Test IPv6 CIDR matching
        assert!(checker.resolve_if_allowed("2001:db8::1").await.is_some());
        assert!(checker
            .resolve_if_allowed("2001:db8:1234::abcd")
            .await
            .is_some());
        assert!(checker.resolve_if_allowed("2001:db9::1").await.is_none()); // Different network

        // Test IPv6 exact match
        assert!(checker.resolve_if_allowed("::1").await.is_some());
        assert!(checker.resolve_if_allowed("::2").await.is_none());
    });
}

#[test]
fn test_network_access_checker_dns_caching() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Test DNS caching behavior
        let allowed_hosts = vec!["127.0.0.0/8".to_string()]; // Allow localhost network
        let allowed_ports = vec![80];

        let checker = NetworkAccessChecker::new(allowed_hosts, allowed_ports);

        // First call should resolve and cache
        let result1 = checker.resolve_if_allowed("localhost").await;
        assert!(result1.is_some());

        // Second call should use cache (faster)
        let result2 = checker.resolve_if_allowed("localhost").await;
        assert!(result2.is_some());

        // Results should be equivalent
        // (We can't test timing without more complex infrastructure)
    });
}

#[test]
fn test_socks5_open_connection_message_parsing() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Create a SOCKS5 OpenConnection message
        let connection_no: u32 = 42;
        let target_host = "example.com";
        let target_port: u16 = 80;

        let mut payload = BytesMut::new();

        // Add connection number
        payload.put_u32(connection_no);

        // Add target host length and host
        payload.put_u32(target_host.len() as u32);
        payload.extend_from_slice(target_host.as_bytes());

        // Add target port
        payload.put_u16(target_port);

        let frame = Frame::new_control_with_pool(ControlMessage::OpenConnection, &payload, &pool);

        // Verify frame structure
        assert_eq!(frame.connection_no, 0); // Control frame
        assert_eq!(frame.payload[0], 0);
        assert_eq!(frame.payload[1], ControlMessage::OpenConnection as u8);

        // Verify payload contains our SOCKS5 data
        let control_msg_size = 2; // u16 for control message type
        let socks_data = &frame.payload[control_msg_size..];

        // Parse back the data to verify
        let mut cursor = std::io::Cursor::new(socks_data);
        use bytes::Buf; // Import the trait

        let parsed_conn_no = cursor.get_u32();
        assert_eq!(parsed_conn_no, connection_no);

        let host_len = cursor.get_u32() as usize;
        assert_eq!(host_len, target_host.len());

        let mut host_bytes = vec![0u8; host_len];
        cursor.copy_to_slice(&mut host_bytes);
        let parsed_host = String::from_utf8(host_bytes).unwrap();
        assert_eq!(parsed_host, target_host);

        let parsed_port = cursor.get_u16();
        assert_eq!(parsed_port, target_port);
    });
}

#[test]
fn test_socks5_malformed_messages() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Test message too short for connection number
        let short_payload = vec![0, 1]; // Only 2 bytes, need 4 for conn_no
        let short_frame =
            Frame::new_control_with_pool(ControlMessage::OpenConnection, &short_payload, &pool);

        // Verify frame was created (error handling happens in protocol layer)
        assert_eq!(short_frame.payload.len(), 2 + short_payload.len()); // control msg + payload

        // Test message missing host length
        let mut incomplete_payload = BytesMut::new();
        incomplete_payload.put_u32(42); // connection number
                                        // Missing host length and data

        let incomplete_frame = Frame::new_control_with_pool(
            ControlMessage::OpenConnection,
            &incomplete_payload,
            &pool,
        );

        assert_eq!(incomplete_frame.payload.len(), 2 + 4); // control msg + conn_no only

        // Test message with host length but missing host data
        let mut partial_payload = BytesMut::new();
        partial_payload.put_u32(42); // connection number
        partial_payload.put_u32(10); // host length = 10
        partial_payload.extend_from_slice(b"short"); // Only 5 bytes, need 10

        let partial_frame =
            Frame::new_control_with_pool(ControlMessage::OpenConnection, &partial_payload, &pool);

        // Frame creation succeeds, parsing errors handled in protocol layer
        assert!(partial_frame.payload.len() > 2);
    });
}

#[test]
fn test_socks5_edge_cases() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Test with empty host name
        let mut empty_host_payload = BytesMut::new();
        empty_host_payload.put_u32(1); // connection number
        empty_host_payload.put_u32(0); // host length = 0
                                       // No host data
        empty_host_payload.put_u16(80); // port

        let empty_host_frame = Frame::new_control_with_pool(
            ControlMessage::OpenConnection,
            &empty_host_payload,
            &pool,
        );

        assert!(empty_host_frame.payload.len() >= 2 + 4 + 4 + 2); // control + conn + host_len + port

        // Test with maximum host length
        let long_host = "a".repeat(255); // 255 character hostname
        let mut long_host_payload = BytesMut::new();
        long_host_payload.put_u32(2);
        long_host_payload.put_u32(long_host.len() as u32);
        long_host_payload.extend_from_slice(long_host.as_bytes());
        long_host_payload.put_u16(443);

        let long_host_frame =
            Frame::new_control_with_pool(ControlMessage::OpenConnection, &long_host_payload, &pool);

        assert!(long_host_frame.payload.len() >= 2 + 4 + 4 + 255 + 2);

        // Test with port 0 (invalid)
        let mut zero_port_payload = BytesMut::new();
        zero_port_payload.put_u32(3);
        zero_port_payload.put_u32(9); // "localhost".len()
        zero_port_payload.extend_from_slice(b"localhost");
        zero_port_payload.put_u16(0); // Invalid port

        let zero_port_frame =
            Frame::new_control_with_pool(ControlMessage::OpenConnection, &zero_port_payload, &pool);

        // Frame creation succeeds, validation happens in protocol layer
        assert!(zero_port_frame.payload.len() > 2);

        // Test with maximum port (65535)
        let mut max_port_payload = BytesMut::new();
        max_port_payload.put_u32(4);
        max_port_payload.put_u32(9);
        max_port_payload.extend_from_slice(b"localhost");
        max_port_payload.put_u16(65535);

        let max_port_frame =
            Frame::new_control_with_pool(ControlMessage::OpenConnection, &max_port_payload, &pool);

        assert!(max_port_frame.payload.len() > 2);
    });
}

#[test]
fn test_socks5_network_checker_integration() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Test integration between SOCKS5 parsing and NetworkAccessChecker
        let allowed_hosts = vec![
            "localhost".to_string(),    // Real hostname that resolves
            "*.google.com".to_string(), // Real wildcard domain
            "10.0.0.0/8".to_string(),   // IP network (no DNS needed)
            "127.0.0.1".to_string(),    // Direct IP (no DNS needed)
        ];
        let allowed_ports = vec![80, 443, 8080];

        let checker = NetworkAccessChecker::new(allowed_hosts, allowed_ports);

        // Test allowed combinations using resolve_if_allowed (actual production method)
        assert!(checker.resolve_if_allowed("localhost").await.is_some());
        assert!(checker.is_port_allowed(80));

        // Use real Google subdomain that actually resolves
        assert!(checker
            .resolve_if_allowed("mail.google.com")
            .await
            .is_some());
        assert!(checker.is_port_allowed(443));

        // Test IP network matching (no DNS resolution needed)
        assert!(checker.resolve_if_allowed("10.1.2.3").await.is_some());
        assert!(checker.is_port_allowed(8080));

        // Test direct IP matching (no DNS resolution needed)
        assert!(checker.resolve_if_allowed("127.0.0.1").await.is_some());

        // Test disallowed combinations
        assert!(checker
            .resolve_if_allowed("definitely-not-a-real-domain-12345.com")
            .await
            .is_none());
        assert!(!checker.is_port_allowed(21)); // FTP

        assert!(checker.resolve_if_allowed("192.168.1.1").await.is_none()); // Not in 10.0.0.0/8
        assert!(!checker.is_port_allowed(3389)); // RDP

        // Test edge cases
        assert!(checker.resolve_if_allowed("google.com").await.is_none()); // *.google.com doesn't match root
                                                                           // Skip the nested subdomain test since it requires real DNS resolution
    });
}

#[test]
fn test_socks5_constants() {
    // Verify our protocol constants are correct
    assert_eq!(CONN_NO_LEN, 4); // Connection number is 4 bytes (u32)
    assert_eq!(PORT_LENGTH, 2); // Port is 2 bytes (u16)

    // Verify control message values (actual values from enum)
    assert_eq!(ControlMessage::OpenConnection as u16, 101);
    assert_eq!(ControlMessage::CloseConnection as u16, 102);
    assert_eq!(ControlMessage::Ping as u16, 1);
    assert_eq!(ControlMessage::Pong as u16, 2);
    assert_eq!(ControlMessage::ConnectionOpened as u16, 103);
    assert_eq!(ControlMessage::SendEOF as u16, 104);
}

#[test]
fn test_socks5_success_response() {
    // Test the SOCKS5 success response constant
    use crate::channel::protocol::SOCKS5_SUCCESS_RESPONSE;

    // SOCKS5 success response format:
    // [VER=0x05, REP=0x00, RSV=0x00, ATYP=0x01, BND.ADDR=0x00000000, BND.PORT=0x0000]
    assert_eq!(SOCKS5_SUCCESS_RESPONSE.len(), 10);
    assert_eq!(SOCKS5_SUCCESS_RESPONSE[0], 0x05); // SOCKS version 5
    assert_eq!(SOCKS5_SUCCESS_RESPONSE[1], 0x00); // Success response
    assert_eq!(SOCKS5_SUCCESS_RESPONSE[2], 0x00); // Reserved
    assert_eq!(SOCKS5_SUCCESS_RESPONSE[3], 0x01); // IPv4 address type
                                                  // Bytes 4-7: BND.ADDR (0x00000000)
    assert_eq!(&SOCKS5_SUCCESS_RESPONSE[4..8], &[0x00, 0x00, 0x00, 0x00]);
    // Bytes 8-9: BND.PORT (0x0000)
    assert_eq!(&SOCKS5_SUCCESS_RESPONSE[8..10], &[0x00, 0x00]);
}

#[test]
fn test_udp_control_messages() {
    // Test the UDP control message constants
    use crate::tube_protocol::ControlMessage;

    // Verify UDP control message values
    assert_eq!(ControlMessage::UdpAssociate as u16, 201);
    assert_eq!(ControlMessage::UdpAssociateOpened as u16, 202);
    assert_eq!(ControlMessage::UdpPacket as u16, 203);
    assert_eq!(ControlMessage::UdpAssociateClosed as u16, 204);

    // Test conversion from u16
    assert_eq!(
        ControlMessage::try_from(201).unwrap(),
        ControlMessage::UdpAssociate
    );
    assert_eq!(
        ControlMessage::try_from(202).unwrap(),
        ControlMessage::UdpAssociateOpened
    );
    assert_eq!(
        ControlMessage::try_from(203).unwrap(),
        ControlMessage::UdpPacket
    );
    assert_eq!(
        ControlMessage::try_from(204).unwrap(),
        ControlMessage::UdpAssociateClosed
    );
}

#[test]
fn test_udp_associate_message() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Create a UDP ASSOCIATE message
        let connection_no: u32 = 42;
        let relay_port: u16 = 1080;

        let mut payload = BytesMut::new();

        // Add connection number
        payload.put_u32(connection_no);

        // Add relay port
        payload.put_u16(relay_port);

        let frame = Frame::new_control_with_pool(ControlMessage::UdpAssociate, &payload, &pool);

        // Verify frame structure
        assert_eq!(frame.connection_no, 0); // Control frame
        assert_eq!(frame.payload[0], 0);
        assert_eq!(frame.payload[1], ControlMessage::UdpAssociate as u8);

        // Verify payload contains our UDP associate data
        let control_msg_size = 2; // u16 for control message type
        let udp_data = &frame.payload[control_msg_size..];

        // Parse back the data to verify
        let mut cursor = std::io::Cursor::new(udp_data);
        use bytes::Buf; // Import the trait

        let parsed_conn_no = cursor.get_u32();
        assert_eq!(parsed_conn_no, connection_no);

        let parsed_port = cursor.get_u16();
        assert_eq!(parsed_port, relay_port);
    });
}

#[test]
fn test_udp_packet_message() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Create a UDP packet message
        let connection_no: u32 = 42;
        let client_ip = std::net::Ipv4Addr::new(127, 0, 0, 1);
        let client_port: u16 = 12345;
        let dest_host = "example.com";
        let dest_port: u16 = 53;
        let udp_payload = b"DNS query data";

        let mut payload = BytesMut::new();

        // Connection number
        payload.put_u32(connection_no);

        // Client address (IPv4)
        payload.put_u8(0x01); // IPv4 type
        payload.extend_from_slice(&client_ip.octets());
        payload.put_u16(client_port);

        // Destination host length + host
        payload.put_u32(dest_host.len() as u32);
        payload.extend_from_slice(dest_host.as_bytes());

        // Destination port
        payload.put_u16(dest_port);

        // UDP payload
        payload.extend_from_slice(udp_payload);

        let frame = Frame::new_control_with_pool(ControlMessage::UdpPacket, &payload, &pool);

        // Verify frame structure
        assert_eq!(frame.connection_no, 0); // Control frame
        assert_eq!(frame.payload[1], ControlMessage::UdpPacket as u8);

        // Verify we can parse it back
        let control_msg_size = 2;
        let udp_data = &frame.payload[control_msg_size..];

        // Calculate expected size: conn_no(4) + addr_type(1) + ipv4(4) + port(2) + host_len(4) + host + dest_port(2) + payload
        let expected_size = 4 + 1 + 4 + 2 + 4 + dest_host.len() + 2 + udp_payload.len();
        assert_eq!(
            udp_data.len(),
            expected_size,
            "UDP data length {} should equal expected size {}",
            udp_data.len(),
            expected_size
        );
    });
}

#[test]
fn test_udp_response_handling() {
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // Test the response packet format
        let connection_no: u32 = 42;
        let client_ip = std::net::Ipv4Addr::new(127, 0, 0, 1);
        let client_port: u16 = 12345;
        let source_ip = std::net::Ipv4Addr::new(8, 8, 8, 8);
        let source_port: u16 = 53;
        let response_payload = b"DNS response data";

        // Create response message as it would be formatted for SOCKS5 UDP
        let mut socks5_response = BytesMut::new();

        // SOCKS5 UDP response format:
        // RSV(2) + FRAG(1) + ATYP(1) + SRC.ADDR(4) + SRC.PORT(2) + DATA
        socks5_response.put_u16(0); // Reserved
        socks5_response.put_u8(0); // Fragment
        socks5_response.put_u8(0x01); // IPv4
        socks5_response.extend_from_slice(&source_ip.octets());
        socks5_response.put_u16(source_port);
        socks5_response.extend_from_slice(response_payload);

        // Create tunnel message with client routing info
        let mut tunnel_payload = BytesMut::new();
        tunnel_payload.put_u32(connection_no);
        tunnel_payload.put_u8(0x01); // IPv4 client
        tunnel_payload.extend_from_slice(&client_ip.octets());
        tunnel_payload.put_u16(client_port);
        tunnel_payload.extend_from_slice(&socks5_response);

        let frame = Frame::new_control_with_pool(ControlMessage::UdpPacket, &tunnel_payload, &pool);

        // Verify the frame was created correctly
        assert_eq!(frame.connection_no, 0); // Control frame
        assert_eq!(frame.payload[1], ControlMessage::UdpPacket as u8);

        // Verify we can parse the complete structure
        let control_msg_size = 2;
        let packet_data = &frame.payload[control_msg_size..];

        // Should contain: conn_no(4) + client_addr_type(1) + client_ip(4) + client_port(2) +
        //                 socks5_response(2+1+1+4+2+response_len)
        let expected_size = 4 + 1 + 4 + 2 + (2 + 1 + 1 + 4 + 2 + response_payload.len());
        assert_eq!(packet_data.len(), expected_size);

        // Parse back and verify components
        let mut cursor = std::io::Cursor::new(packet_data);
        use bytes::Buf;

        let parsed_conn_no = cursor.get_u32();
        assert_eq!(parsed_conn_no, connection_no);

        let client_addr_type = cursor.get_u8();
        assert_eq!(client_addr_type, 0x01); // IPv4

        let client_addr_bytes = [
            cursor.get_u8(),
            cursor.get_u8(),
            cursor.get_u8(),
            cursor.get_u8(),
        ];
        assert_eq!(client_addr_bytes, client_ip.octets());

        let parsed_client_port = cursor.get_u16();
        assert_eq!(parsed_client_port, client_port);

        // Verify SOCKS5 response format
        let rsv = cursor.get_u16();
        assert_eq!(rsv, 0);

        let frag = cursor.get_u8();
        assert_eq!(frag, 0);

        let atyp = cursor.get_u8();
        assert_eq!(atyp, 0x01);

        let src_addr_bytes = [
            cursor.get_u8(),
            cursor.get_u8(),
            cursor.get_u8(),
            cursor.get_u8(),
        ];
        assert_eq!(src_addr_bytes, source_ip.octets());

        let parsed_src_port = cursor.get_u16();
        assert_eq!(parsed_src_port, source_port);

        // Remaining should be the response payload
        let remaining_data = &packet_data[cursor.position() as usize..];
        assert_eq!(remaining_data, response_payload);
    });
}

#[test]
fn test_udp_association_lifecycle() {
    // Test the complete UDP association lifecycle
    use crate::tube_protocol::ControlMessage;

    // Test that all UDP control messages have correct values
    assert_eq!(ControlMessage::UdpAssociate as u16, 201);
    assert_eq!(ControlMessage::UdpAssociateOpened as u16, 202);
    assert_eq!(ControlMessage::UdpPacket as u16, 203);
    assert_eq!(ControlMessage::UdpAssociateClosed as u16, 204);

    // Test message sequence for a complete UDP association
    let runtime = get_runtime();
    runtime.block_on(async {
        let pool = BufferPool::default();

        // 1. UdpAssociate request
        let conn_no: u32 = 1;
        let relay_port: u16 = 1080;

        let mut associate_payload = BytesMut::new();
        associate_payload.put_u32(conn_no);
        associate_payload.put_u16(relay_port);

        let associate_frame =
            Frame::new_control_with_pool(ControlMessage::UdpAssociate, &associate_payload, &pool);

        assert_eq!(
            associate_frame.payload[1],
            ControlMessage::UdpAssociate as u8
        );

        // 2. UdpAssociateOpened response
        let opened_port: u16 = 5432;

        let mut opened_payload = BytesMut::new();
        opened_payload.put_u32(conn_no);
        opened_payload.put_u16(opened_port);

        let opened_frame = Frame::new_control_with_pool(
            ControlMessage::UdpAssociateOpened,
            &opened_payload,
            &pool,
        );

        assert_eq!(
            opened_frame.payload[1],
            ControlMessage::UdpAssociateOpened as u8
        );

        // 3. UdpPacket data flow (tested above)

        // 4. UdpAssociateClosed
        let mut closed_payload = BytesMut::new();
        closed_payload.put_u32(conn_no);

        let closed_frame = Frame::new_control_with_pool(
            ControlMessage::UdpAssociateClosed,
            &closed_payload,
            &pool,
        );

        assert_eq!(
            closed_frame.payload[1],
            ControlMessage::UdpAssociateClosed as u8
        );
    });
}

#[test]
fn test_udp_socket_address_parsing() {
    // Test socket address parsing for UDP associations
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    // Test IPv4 addresses
    let ipv4_addr: SocketAddr = "8.8.8.8:53".parse().unwrap();
    assert_eq!(
        ipv4_addr.ip(),
        std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))
    );
    assert_eq!(ipv4_addr.port(), 53);

    let localhost_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    assert_eq!(
        localhost_addr.ip(),
        std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
    );
    assert_eq!(localhost_addr.port(), 12345);

    // Test IPv6 addresses
    let ipv6_addr: SocketAddr = "[::1]:8080".parse().unwrap();
    assert_eq!(
        ipv6_addr.ip(),
        std::net::IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1))
    );
    assert_eq!(ipv6_addr.port(), 8080);

    // Test corporate network addresses
    let corporate_dns: SocketAddr = "10.0.0.1:53".parse().unwrap();
    assert_eq!(
        corporate_dns.ip(),
        std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
    );
    assert_eq!(corporate_dns.port(), 53);
}
