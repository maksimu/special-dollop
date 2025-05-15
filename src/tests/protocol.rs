//! Protocol frame and message tests
use crate::models::ConnectionStats;
use crate::protocol::{ControlMessage, Frame, now_ms, try_parse_frame};
use crate::runtime::get_runtime;
use crate::buffer_pool::BufferPool;

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
        assert_eq!(ping_frame.connection_no, 0, "Control frames should have connection_no = 0");

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
fn test_latency_tracking() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create connection stats and verify functionality
        let mut stats = ConnectionStats::default();

        // Verify default values
        assert_eq!(stats.receive_size, 0);
        assert_eq!(stats.transfer_size, 0);
        assert_eq!(stats.receive_latency_sum, 0);
        assert_eq!(stats.receive_latency_count, 0);
        assert_eq!(stats.transfer_latency_sum, 0);
        assert_eq!(stats.transfer_latency_count, 0);
        assert_eq!(stats.ping_time, None);
        assert_eq!(stats.message_counter, 0);

        // Test latency tracking
        let now = now_ms();

        // Simulate a ping sent 100 ms ago
        stats.ping_time = Some(now - 100);

        // Record some receive latency
        stats.receive_latency_sum += 40;
        stats.receive_latency_count += 1;

        // Calculate ping-pong latency
        let ping_time = stats.ping_time.unwrap();
        let latency = now.saturating_sub(ping_time);

        // Calculate transfer latency (total latency minus receive latency)
        let receive_latency_avg = stats.receive_latency_sum / stats.receive_latency_count;
        let transfer_latency = latency.saturating_sub(receive_latency_avg);

        stats.transfer_latency_sum += transfer_latency;
        stats.transfer_latency_count += 1;

        // Verify values
        assert_eq!(stats.receive_latency_avg(), 40);
        assert!(stats.transfer_latency_avg() > 0);
        assert!(stats.transfer_latency_avg() < 100);
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
        let timestamp = now_ms();
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

        // Create empty connection stats
        let mut stats = ConnectionStats::default();
        stats.ping_time = Some(timestamp);

        // Update stats with pong response
        if pong_frame.payload.len() >= 12 { // 2 bytes for the control message + 4 bytes for conn_no + 8 bytes for u64 latency
            let receive_latency_bytes: [u8; 8] = pong_frame.payload[6..14].try_into().unwrap();
            let receive_latency = u64::from_be_bytes(receive_latency_bytes);

            stats.receive_latency_sum += receive_latency;
            stats.receive_latency_count += 1;
        }

        // Verify stats were updated correctly
        assert_eq!(stats.receive_latency_count, 1);
        assert_eq!(stats.receive_latency_sum, receive_latency);

        // Test average calculation methods
        assert_eq!(stats.receive_latency_avg(), receive_latency);

        // Create a new stats object with zero counts to test edge cases
        let empty_stats = ConnectionStats::default();
        assert_eq!(empty_stats.receive_latency_avg(), 0);
        assert_eq!(empty_stats.transfer_latency_avg(), 0);
    });
}
