//! Protocol frame and message tests
use crate::buffer_pool::BufferPool;
use crate::runtime::get_runtime;
use crate::tube_protocol::{try_parse_frame, ControlMessage, Frame};

#[test]
fn test_protocol_frame_handling() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create a buffer pool for the test
        let pool = BufferPool::default();

        // Test ping/pong control message parsing
        let ping_data = vec![0, 0, 0, 42]; // Connection number 42
        let ping_frame = Frame::new_control_with_pool(ControlMessage::Ping, &ping_data, &pool);

        // Verify frame construction
        assert_eq!(
            ping_frame.connection_no, 0,
            "Control frames should have connection_no = 0"
        );

        // The control message is stored as an u16 in the payload
        // ControlMessage::Ping has value 1, so the u16 bytes would be [0, 1]
        assert_eq!(ping_frame.payload[0], 0);
        assert_eq!(ping_frame.payload[1], ControlMessage::Ping as u8);

        // Verify that ping data is properly included
        assert_eq!(&ping_frame.payload[2..], &ping_data);

        // Test frame encoding/decoding
        let encoded = ping_frame.encode_with_pool(&pool);
        let encoded_slice = encoded.as_ref(); // Convert Bytes to &[u8] for testing

        // Verify frame encoding format
        assert_eq!(encoded_slice[0], 0); // connection_no (4 bytes)
        assert_eq!(encoded_slice[1], 0);
        assert_eq!(encoded_slice[2], 0);
        assert_eq!(encoded_slice[3], 0);

        // Test timestamp (skipping exact value)

        // Test payload length (4 bytes)
        let payload_len = (ping_frame.payload.len() as u32).to_be_bytes();
        assert_eq!(encoded_slice[12], payload_len[0]);
        assert_eq!(encoded_slice[13], payload_len[1]);
        assert_eq!(encoded_slice[14], payload_len[2]);
        assert_eq!(encoded_slice[15], payload_len[3]);
    });
}

#[test]
fn test_frame_parsing() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create a buffer pool for the test
        let pool = BufferPool::default();

        // Test basic frame parsing functionality
        let data_payload = b"Hello, WebRTC!".to_vec();
        let data_frame = Frame::new_data_with_pool(1, &data_payload, &pool);

        // Verify frame construction
        assert_eq!(data_frame.connection_no, 1);
        assert_eq!(data_frame.payload, data_payload);

        // Test encoding
        let encoded = data_frame.encode_with_pool(&pool);
        let mut bytes = bytes::BytesMut::from(&encoded[..]);

        // Test parsing
        let parsed_frame = try_parse_frame(&mut bytes).expect("Should parse valid frame");

        // Verify parsed frame matches original
        assert_eq!(parsed_frame.connection_no, data_frame.connection_no);
        assert_eq!(parsed_frame.payload, data_frame.payload);
    });
}

#[test]
fn test_protocol_ping_pong() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create a buffer pool for the test
        let pool = BufferPool::default();

        // Create a ping control message
        let conn_no: u32 = 0; // Control connection
        let ping_data = conn_no.to_be_bytes().to_vec();

        // Create a ping frame
        let ping_frame = Frame::new_control_with_pool(ControlMessage::Ping, &ping_data, &pool);

        // Verify the frame has the correct control message type
        assert_eq!(ping_frame.payload[0], 0);
        assert_eq!(ping_frame.payload[1], ControlMessage::Ping as u8);

        // Verify the connection number is 0 (for control messages)
        assert_eq!(ping_frame.connection_no, 0);

        // Test encoding/decoding roundtrip
        let encoded = ping_frame.encode_with_pool(&pool);
        let mut bytes = bytes::BytesMut::from(&encoded[..]);

        // Use the proper frame parsing function
        let decoded_frame = try_parse_frame(&mut bytes).expect("Should parse a valid frame");

        // Verify the decoded frame matches the original
        assert_eq!(decoded_frame.connection_no, ping_frame.connection_no);
        assert_eq!(decoded_frame.payload, ping_frame.payload);

        // Create a pong response with latency information
        let receive_latency = 15u64; // 15 ms simulated processing time
        let mut pong_data = conn_no.to_be_bytes().to_vec();
        pong_data.extend_from_slice(&receive_latency.to_be_bytes());

        let pong_frame = Frame::new_control_with_pool(ControlMessage::Pong, &pong_data, &pool);

        // Verify pong frame
        assert_eq!(pong_frame.payload[0], 0);
        assert_eq!(pong_frame.payload[1], ControlMessage::Pong as u8);

        // Verify pong contains expected data
        assert_eq!(pong_frame.payload.len(), 2 + 4 + 8); // control msg + conn_no + latency

        // Test basic frame parsing for pong
        let pong_encoded = pong_frame.encode_with_pool(&pool);
        let mut pong_bytes = bytes::BytesMut::from(&pong_encoded[..]);
        let parsed_pong = try_parse_frame(&mut pong_bytes).expect("Should parse pong frame");

        assert_eq!(parsed_pong.connection_no, 0);
        assert_eq!(parsed_pong.payload, pong_frame.payload);

        // Note: In our simplified system, we rely on WebRTC native stats
        // instead of custom ConnectionStats tracking
        println!("Frame parsing test completed - custom stats replaced with WebRTC native stats");
    });
}
