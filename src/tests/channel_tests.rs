// Tests for the Channel module
#![cfg(test)]
use crate::buffer_pool::{BufferPool, BufferPoolConfig};
use crate::channel::Channel;
use crate::tube_protocol::{ControlMessage, Frame};
use anyhow::Result;
use bytes::{Bytes, BytesMut};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test]
async fn test_buffer_pool_usage() {
    // This test verifies that the buffer pool is used efficiently

    // Create a custom buffer pool for testing
    let buffer_pool_config = BufferPoolConfig {
        buffer_size: 8 * 1024, // 8KB buffer size
        max_pooled: 10,        // Keep up to 10 buffers in the pool (increased for pre-warming)
        resize_on_return: true,
    };
    let buffer_pool = BufferPool::new(buffer_pool_config);

    // Verify the initial buffer pool state - now pre-warmed for performance
    let initial_count = buffer_pool.count();
    assert!(
        initial_count > 0,
        "Buffer pool should be pre-warmed with buffers"
    );
    assert!(
        initial_count <= 8,
        "Buffer pool should pre-warm with at most 8 buffers"
    );

    // Create a frame using the pool
    let test_data = b"test data for buffer pool";
    let frame = Frame::new_data_with_pool(1, test_data, &buffer_pool);

    // Encode it and manually return the buffer to the pool
    {
        let mut buffer = BytesMut::with_capacity(64);
        frame.encode_into(&mut buffer);
        buffer_pool.release(buffer);
    }

    // Verify pool usage - count should be at least the initial count
    let count_after_use = buffer_pool.count();
    assert!(
        count_after_use >= initial_count,
        "Buffer pool should maintain or increase count after releasing buffers"
    );

    // Create another frame and encode, then release
    {
        let frame2 = Frame::new_data_with_pool(2, b"more test data", &buffer_pool);
        let mut buffer = BytesMut::with_capacity(64);
        frame2.encode_into(&mut buffer);
        buffer_pool.release(buffer);
    }

    // The buffer pool count should not exceed max_pooled
    assert!(
        buffer_pool.count() <= 10,
        "Buffer pool should not exceed max_pooled"
    );
}

/// Test to verify data flow in server mode (local TCP -> Channel -> WebRTC)
#[tokio::test]
async fn test_server_mode_data_flow() -> Result<()> {
    // Set up a WebRTCDataChannel with the test hook to capture sent frames
    let (frame_tx, mut frame_rx) = mpsc::channel::<Bytes>(100);
    let webrtc = crate::tests::common_tests::create_test_webrtc_data_channel().await;

    // Set the test hook to capture frames sent to WebRTC
    webrtc.set_test_send_hook(move |data| {
        let frame_tx = frame_tx.clone();
        Box::pin(async move {
            let _ = frame_tx.send(data).await;
        })
    });

    // Create a Channel with server_mode=true
    let (_tx_to_channel, rx_from_dc) = mpsc::unbounded_channel::<Bytes>();
    let mut settings = HashMap::new();
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

    let mut channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: webrtc.clone(),
        rx_from_dc,
        channel_id: "test_server_mode".to_string(),
        timeouts: None,              // default timeouts
        protocol_settings: settings, // protocol_settings
        server_mode: true,           // server_mode=true
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await?;

    // Start server listening on localhost with a random port
    let server_addr = "127.0.0.1:0"; // Let OS assign a port
    channel.start_server(server_addr).await?;

    // Get the actual bound address
    let bound_addr = if let Some(ref listener) = channel.local_client_server {
        let addr = listener.local_addr()?;
        println!("Server bound to: {}", addr);
        addr
    } else {
        panic!("Failed to get server listener address");
    };

    // Start the channel running in a separate task
    let channel_handle = tokio::spawn(async move {
        if let Err(e) = channel.run().await {
            eprintln!("Channel run error: {}", e);
        }
    });

    // Give the channel time to initialize
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect to the local server as a client
    let mut client = TcpStream::connect(bound_addr).await?;

    // Give time for the connection to be established
    tokio::time::sleep(Duration::from_millis(200)).await;

    // First, capture and verify the OpenConnection control frame
    let control_frame_data =
        receive_with_timeout(&mut frame_rx, Duration::from_millis(500)).await?;
    let control_frame = extract_frame_from_bytes(&control_frame_data)?;

    println!(
        "Captured control frame: conn_no={}, payload.len={}",
        control_frame.connection_no,
        control_frame.payload.len()
    );

    // The control frame should have connection number 0 (for control messages)
    assert_eq!(
        control_frame.connection_no, 0,
        "Control frame should have connection number 0"
    );

    // Verify this is an OpenConnection control message
    // Payload structure for OpenConnection: [u16: control_code][u32: assigned_connection_no]
    assert!(
        control_frame.payload.len() >= 6,
        "OpenConnection payload is too short. Expected at least 6 bytes."
    );

    let control_code_val = u16::from_be_bytes([control_frame.payload[0], control_frame.payload[1]]);
    assert_eq!(
        control_code_val,
        ControlMessage::OpenConnection as u16,
        "Control frame should be an OpenConnection message"
    );

    let conn_bytes = [
        control_frame.payload[2],
        control_frame.payload[3],
        control_frame.payload[4],
        control_frame.payload[5],
    ];
    let assigned_conn_no = u32::from_be_bytes(conn_bytes);
    println!(
        "OpenConnection assigned connection number: {}",
        assigned_conn_no
    );
    assert!(
        assigned_conn_no > 0,
        "Assigned connection number should be greater than 0"
    );

    // Now send test data from client to server
    let test_data = b"Hello from TCP client!";
    client.write_all(test_data).await?;
    client.flush().await?;

    // Give time for data to be processed
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now capture and verify the actual data frame
    let data_frame_bytes = receive_with_timeout(&mut frame_rx, Duration::from_millis(500)).await?;
    let data_frame = extract_frame_from_bytes(&data_frame_bytes)?;

    // Print info about the captured data frame
    println!(
        "Captured data frame: conn_no={}, payload.len={}",
        data_frame.connection_no,
        data_frame.payload.len()
    );

    // Verify the data frame - the connection number should match the one assigned in OpenConnection
    assert_eq!(
        data_frame.connection_no, assigned_conn_no,
        "Data frame connection number should match the assigned one from OpenConnection"
    );

    // Check that the payload matches our test data
    assert_eq!(
        &data_frame.payload[..],
        test_data,
        "Data frame payload should match the test data sent by the client"
    );

    // Clean up
    client.shutdown().await?;
    channel_handle.abort();

    Ok(())
}

/// Test to verify data flow in client mode (WebRTC -> Channel -> Remote TCP)
#[tokio::test]
async fn test_client_mode_data_flow() -> Result<()> {
    // Set up a mock TCP server to represent the remote host
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0").await?,
    };
    let server_addr = listener.local_addr()?;
    println!("Mock TCP server listening on: {}", server_addr);

    // Create a channel to receive data on the mock TCP server
    let (tcp_data_tx, mut tcp_data_rx) = mpsc::channel::<Vec<u8>>(10);

    // Spawn a task to handle connections to our mock server
    let server_handle = tokio::spawn(async move {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("Failed to accept connection");
        println!("Connection accepted from: {}", socket.peer_addr().unwrap());

        let mut buf = [0u8; 1024];
        loop {
            match socket.read(&mut buf).await {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    println!("TCP server received {} bytes", n);
                    tcp_data_tx
                        .send(buf[..n].to_vec())
                        .await
                        .expect("Failed to send to channel");
                }
                Err(e) => {
                    eprintln!("TCP server read error: {}", e);
                    break;
                }
            }
        }
    });

    // Set up the Channel in client mode
    let webrtc = crate::tests::common_tests::create_test_webrtc_data_channel().await;
    let (tx_to_channel, rx_from_dc) = mpsc::unbounded_channel::<Bytes>();

    // Configure the channel to connect to our mock TCP server
    let mut settings = HashMap::new();
    settings.insert(
        "target_host".to_string(),
        serde_json::json!(server_addr.ip().to_string()),
    );
    settings.insert(
        "target_port".to_string(),
        serde_json::json!(server_addr.port()),
    );
    settings.insert("conversationType".to_string(), serde_json::json!("tunnel"));

    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: webrtc.clone(),
        rx_from_dc,
        channel_id: "test_client_mode".to_string(),
        timeouts: None,              // default timeouts
        protocol_settings: settings, // protocol_settings
        server_mode: false,          // server_mode=false
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await?;

    // Start the channel running in a separate task
    let channel_handle = tokio::spawn(async move {
        if let Err(e) = channel.run().await {
            eprintln!("Channel run error: {}", e);
        }
    });

    // Give the channel time to initialize
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create a data frame as if coming from WebRTC
    // First create the OpenConnection control message to establish the connection
    let conn_no: u32 = 1; // Use connection #1
    let mut open_data = Vec::new();
    open_data.extend_from_slice(&conn_no.to_be_bytes());

    // Get a buffer pool for frame creation
    let buffer_pool = BufferPool::default();

    // Send the OpenConnection control message using new_control_with_pool
    let open_frame =
        Frame::new_control_with_pool(ControlMessage::OpenConnection, &open_data, &buffer_pool);
    let encoded_open = open_frame.encode_with_pool(&buffer_pool);
    tx_to_channel
        .send(encoded_open)
        .expect("Failed to send OpenConnection");

    // Give time for the connection to be established
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now send actual data as if coming from WebRTC
    let test_data = b"Hello from WebRTC!";
    let data_frame = Frame::new_data_with_pool(conn_no, test_data, &buffer_pool);
    let encoded_data = data_frame.encode_with_pool(&buffer_pool);
    tx_to_channel
        .send(encoded_data)
        .expect("Failed to send data frame");

    // Give time for data to be processed
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify that the mock TCP server received the expected data
    let received_data = receive_with_timeout(&mut tcp_data_rx, Duration::from_millis(500)).await?;

    println!("TCP server received data: {:?}", received_data);
    assert_eq!(
        received_data, test_data,
        "TCP server should receive the test data"
    );

    // Clean up
    channel_handle.abort();
    server_handle.abort();

    Ok(())
}

// Helper to extract a Frame from bytes
fn extract_frame_from_bytes(data: &Bytes) -> Result<Frame> {
    // This is a simplified parser for test purposes
    let mut buffer = BytesMut::from(&data[..]);

    // Use the crate's frame parsing logic if available
    match crate::tube_protocol::try_parse_frame(&mut buffer) {
        Some(frame) => Ok(frame),
        None => anyhow::bail!("Failed to parse frame from bytes"),
    }
}

// Helper to receive with timeout
async fn receive_with_timeout<T>(
    receiver: &mut mpsc::Receiver<T>,
    timeout_duration: Duration,
) -> Result<T> {
    match timeout(timeout_duration, receiver.recv()).await {
        Ok(Some(data)) => Ok(data),
        Ok(None) => anyhow::bail!("Channel closed"),
        Err(_) => anyhow::bail!("Timeout waiting for data"),
    }
}
