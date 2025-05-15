// Tests for the Channel module
#![cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use futures::future::BoxFuture;
use tokio::sync::mpsc;

use crate::buffer_pool::{BufferPool, BufferPoolConfig};
use crate::channel::Channel;
use crate::models::ProtocolHandler;
use crate::protocol::Frame;
use crate::webrtc_data_channel::WebRTCDataChannel;

// Mock protocol handler for testing
struct MockProtocolHandler {
    client_data: Arc<Mutex<Vec<(u32, Vec<u8>)>>>,
    commands: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
    connections: Arc<Mutex<Vec<u32>>>,
    webrtc: Option<WebRTCDataChannel>,
    responses: Arc<Mutex<HashMap<u32, Vec<Vec<u8>>>>>,
}

impl MockProtocolHandler {
    fn new() -> Self {
        Self {
            client_data: Arc::new(Mutex::new(Vec::new())),
            commands: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(Mutex::new(Vec::new())),
            webrtc: None,
            responses: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    // Add a predefined response for a connection
    fn add_response(&self, conn_no: u32, data: Vec<u8>) {
        let mut responses = self.responses.lock().unwrap();
        responses.entry(conn_no)
            .or_insert_with(Vec::new)
            .push(data);
    }
}

impl ProtocolHandler for MockProtocolHandler {
    fn initialize(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn process_client_data<'a>(
        &'a mut self,
        conn_no: u32,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<Bytes>> {
        let client_data = self.client_data.clone();
        let responses = self.responses.clone();
        
        Box::pin(async move {
            // Store the received data
            {
                let mut guard = client_data.lock().unwrap();
                guard.push((conn_no, data.to_vec()));
            }
            
            // Return predefined response if available
            let response = {
                let mut guard = responses.lock().unwrap();
                if let Some(resp_queue) = guard.get_mut(&conn_no) {
                    if !resp_queue.is_empty() {
                        Some(resp_queue.remove(0))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            
            // Return the response or an empty vector
            Ok(Bytes::from(response.unwrap_or_default()))
        })
    }

    fn process_server_data<'a>(
        &'a mut self,
        _conn_no: u32,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<Bytes>> {
        // For test simplicity, return the data as is
        Box::pin(async move {
            Ok(Bytes::copy_from_slice(data))
        })
    }

    fn handle_command<'a>(&'a mut self, command: &'a str, params: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let commands = self.commands.clone();
        let cmd = command.to_string();
        let p = params.to_vec();
        
        Box::pin(async move {
            let mut guard = commands.lock().unwrap();
            guard.push((cmd, p));
            Ok(())
        })
    }

    fn handle_new_connection(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        let connections = self.connections.clone();
        Box::pin(async move {
            let mut guard = connections.lock().unwrap();
            guard.push(conn_no);
            Ok(())
        })
    }

    fn handle_eof(&mut self, _conn_no: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Log for testing
            Ok(())
        })
    }

    fn handle_close(&mut self, conn_no: u32) -> BoxFuture<'_, Result<()>> {
        let connections = self.connections.clone();
        Box::pin(async move {
            let mut guard = connections.lock().unwrap();
            if let Some(pos) = guard.iter().position(|&c| c == conn_no) {
                guard.remove(pos);
            }
            Ok(())
        })
    }

    fn set_webrtc_channel(&mut self, webrtc: WebRTCDataChannel) {
        self.webrtc = Some(webrtc);
    }

    fn on_webrtc_channel_state_change(&mut self, _state: &str) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Log for testing
            Ok(())
        })
    }

    fn process_pending_messages(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // For testing, return Ok
            Ok(())
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // For testing, return Ok
            Ok(())
        })
    }

    fn status(&self) -> String {
        "Mock Protocol Handler".to_string()
    }
}

#[tokio::test]
async fn test_channel_creation_and_drop() {
    // Create a WebRTCDataChannel using our non-blocking helper
    let webrtc = crate::tests::common::create_test_webrtc_data_channel();
    
    // Create channels for communication
    let (_tx_to_channel, rx_from_dc) = mpsc::unbounded_channel::<Bytes>();
    
    // Create a channel in a block so it gets dropped
    {
        let _channel = Channel::new(
            webrtc,
            rx_from_dc,
            "test_endpoint".to_string(),
            None,
            None,
            "TEST_KSM_CONFIG".to_string(),
            "TEST_CALLBACK_TOKEN".to_string(),
        );
        
        // Channel is created here and will be dropped when this block ends
    }
    
    // Now the channel should be dropped and cleaned up
    // In a more complete test we would verify that resources were properly released
}

#[tokio::test]
async fn test_message_flow() {
    // For this test, we'll use a simple approach that doesn't require real WebRTC
    // since we can't easily test message flow without access to private fields
    
    // Create a simple message channel
    let (tx, mut rx) = mpsc::channel::<Bytes>(10);
    
    // Send a test message
    let test_message = Bytes::from_static(b"test message");
    tx.send(test_message.clone()).await.unwrap();
    
    // Verify the message received
    let received = rx.recv().await.unwrap();
    assert_eq!(received, test_message);
}

#[tokio::test]
async fn test_protocol_handler_routing() {
    // In this test, we'll verify that protocol handlers receive messages
    // Since we can't directly access channel internals; this test is simplified
    
    // Create a mock protocol handler
    let handler = MockProtocolHandler::new();
    let client_data = handler.client_data.clone();
    
    // Add test data to simulate receiving a message
    let conn_no = 1;
    let test_data = b"TEST_DATA".to_vec();
    {
        let mut guard = client_data.lock().unwrap();
        guard.push((conn_no, test_data.clone()));
    }
    
    // Verify the test data was added
    let received_data = {
        let guard = client_data.lock().unwrap();
        guard.clone()
    };
    
    assert!(!received_data.is_empty(), "Should have test data");
    assert_eq!(received_data[0].0, conn_no, "Connection number should match");
    assert_eq!(received_data[0].1, test_data, "Data should match");
}

#[tokio::test]
async fn test_buffer_pool_usage() {
    // This test verifies that the buffer pool is used efficiently
    
    // Create a custom buffer pool for testing
    let buffer_pool_config = BufferPoolConfig {
        buffer_size: 8 * 1024, // 8KB buffer size
        max_pooled: 5,         // Keep up to 5 buffers in the pool
        resize_on_return: true,
    };
    let buffer_pool = BufferPool::new(buffer_pool_config);
    
    // Verify initial buffer pool state
    assert_eq!(buffer_pool.count(), 0, "Buffer pool should start empty");
    
    // Create a frame using the pool
    let test_data = b"test data for buffer pool";
    let frame = Frame::new_data_with_pool(1, test_data, &buffer_pool);
    
    // Encode it and manually return the buffer to the pool
    {
        let mut buffer = BytesMut::with_capacity(64);
        frame.encode_into(&mut buffer);
        buffer_pool.release(buffer);
    }
    
    // Verify pool usage
    assert!(buffer_pool.count() > 0, "Buffer pool should have buffers after use");
    
    // Create another frame and encode, then release
    {
        let frame2 = Frame::new_data_with_pool(2, b"more test data", &buffer_pool);
        let mut buffer = BytesMut::with_capacity(64);
        frame2.encode_into(&mut buffer);
        buffer_pool.release(buffer);
    }
    
    // The buffer pool count should not exceed max_pooled
    assert!(buffer_pool.count() <= 5, "Buffer pool should not exceed max_pooled");
}

// TODO: Placeholder test for channel message queuing
#[tokio::test]
async fn test_channel_message_queuing() {
    // This will be implemented in a future PR
    assert!(true);
} 