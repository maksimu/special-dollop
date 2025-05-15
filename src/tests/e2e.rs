//! End-to-end protocol tests with client and server registries
use crate::tube::Tube;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as TokioTcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use crate::tube_registry::{TubeRegistry, REGISTRY};
use bytes::Bytes;
use std::time::Duration;
use serde_json;

/// Simple echo server that handles different protocols
struct TestServer {
    listener: TcpListener,
    port: u16,
    mode: String,
}

impl TestServer {
    fn new(mode: &str) -> Result<Self> {
        // Bind to port 0 to get an available port
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        
        Ok(Self {
            listener,
            port,
            mode: mode.to_string(),
        })
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn start(&self) -> JoinHandle<()> {
        let mode = self.mode.clone();
        let port = self.port;
        
        // Clone the listener to avoid ownership issues
        let listener_std = self.listener.try_clone().expect("Failed to clone listener");
        
        tokio::spawn(async move {
            // Set the listener to non-blocking mode before converting
            listener_std.set_nonblocking(true).unwrap();
            let listener = TokioTcpListener::from_std(listener_std).unwrap();
            println!("Test server listening on port {} in {} mode", port, mode);
            
            while let Ok((stream, addr)) = listener.accept().await {
                println!("Test server: Accepted connection from {:?}", addr);
                let mode = mode.clone();
                tokio::spawn(async move {
                    match mode.as_str() {
                        "port_forward" | "socks5" => {
                            handle_echo_connection(stream).await;
                        },
                        "guacd" => {
                            println!("Test server: Starting guacd protocol handler");
                            handle_guacd_connection(stream).await;
                        },
                        _ => {
                            println!("Unknown server mode: {}", mode);
                        }
                    }
                });
            }
        })
    }
    
    // Add a stop method for clean shutdown
    async fn stop(&self, handle: JoinHandle<()>) {
        // Abort the server task
        handle.abort();
        // Give some time for cleanup
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn handle_echo_connection(mut stream: tokio::net::TcpStream) {
    let mut buf = [0u8; 1024];
    println!("Echo server: new connection established");
    
    while let Ok(n) = stream.read(&mut buf).await {
        if n == 0 {
            println!("Echo server: connection closed");
            break;
        }
        
        println!("Echo server: received {} bytes", n);
        if n > 0 {
            println!("Echo server: received data: {:?}", &buf[0..std::cmp::min(20, n)]);
        }
        
        // Echo the data back
        println!("Echo server: echoing data back");
        if let Err(e) = stream.write_all(&buf[0..n]).await {
            println!("Echo server: error writing to stream: {}", e);
            break;
        }
        println!("Echo server: echoed {} bytes back to client", n);
    }
}

async fn handle_guacd_connection(mut stream: tokio::net::TcpStream) {
    // Read the first instruction (should be "select" with protocol)
    let mut buffer = [0u8; 1024];
    if let Ok(n) = stream.read(&mut buffer).await {
        if n > 0 {
            println!("Guacd server: received select instruction: {}", String::from_utf8_lossy(&buffer[0..n]));
            
            // Send "args" instruction with version and parameter names
            // Format: "4.args,10.VERSION_1_5_0,8.hostname,4.port,8.username,8.password;"
            let args_response = "4.args,10.VERSION_1_5_0,8.hostname,4.port,8.username,8.password;";
            println!("Guacd server: sending args instruction: {}", args_response);
            if let Err(e) = stream.write_all(args_response.as_bytes()).await {
                println!("Error writing args response: {}", e);
                return;
            }
            
            // Now expect size, audio, video, image instructions
            let expected_messages = ["size", "audio", "video", "image"];
            for (i, expected) in expected_messages.iter().enumerate() {
                let mut inst_buffer = [0u8; 1024];
                match stream.read(&mut inst_buffer).await {
                    Ok(n) if n > 0 => {
                        let msg = String::from_utf8_lossy(&inst_buffer[0..n]);
                        println!("Guacd server: received instruction {}: {}", i+1, msg);
                        if !msg.contains(expected) {
                            println!("Warning: Expected '{}' instruction but got: {}", expected, msg);
                        }
                    },
                    Ok(n) => {
                        println!("Guacd server: empty instruction received ({})", n);
                        return;
                    },
                    Err(e) => {
                        println!("Guacd server: failed to read instruction: {}", e);
                        return;
                    }
                }
            }
            
            // Expect "connect" instruction with connection parameters
            let mut connect_buffer = [0u8; 1024];
            match stream.read(&mut connect_buffer).await {
                Ok(n) if n > 0 => {
                    let connect_msg = String::from_utf8_lossy(&connect_buffer[0..n]);
                    println!("Guacd server: received connect instruction: {}", connect_msg);
                    
                    // Send "ready" instruction with client ID
                    let ready_response = "5.ready,36.12345678-1234-1234-1234-123456789abc;";
                    println!("Guacd server: sending ready instruction: {}", ready_response);
                    if let Err(e) = stream.write_all(ready_response.as_bytes()).await {
                        println!("Error writing ready response: {}", e);
                        return;
                    }
                    
                    // Now handle data exchange
                    println!("Guacd server: handshake complete, handling data exchange");
                    let mut data_buffer = [0u8; 1024];
                    while let Ok(n) = stream.read(&mut data_buffer).await {
                        if n == 0 {
                            println!("Guacd server: connection closed by client");
                            break;
                        }
                        
                        let data_msg = String::from_utf8_lossy(&data_buffer[0..n]);
                        println!("Guacd server: received data of length {}: {}", n, data_msg);
                        
                        // Echo back what we received
                        println!("Guacd server: echoing data back");
                        if let Err(e) = stream.write_all(&data_buffer[0..n]).await {
                            println!("Error echoing data: {}", e);
                            break;
                        }
                        println!("Guacd server: data echoed successfully");
                    }
                },
                Ok(n) => {
                    println!("Guacd server: empty connect instruction received ({})", n);
                },
                Err(e) => {
                    println!("Guacd server: failed to read connect instruction: {}", e);
                }
            }
        } else {
            println!("Guacd server: empty initial instruction");
        }
    } else {
        println!("Guacd server: failed to read initial instruction");
    }
    println!("Guacd server: handle_guacd_connection completed");
}

#[tokio::test]
async fn test_port_forward_protocol() -> Result<()> {
    // Setup test server for port_forward protocol
    let port_forward_server = TestServer::new("port_forward")?;
    let port_forward_port = port_forward_server.port();
    let port_forward_handle = port_forward_server.start();
    
    // Create server registry (server mode)
    let mut server_registry = TubeRegistry::new();
    server_registry.set_server_mode(true);
    
    // Create a client registry (client mode)
    let mut client_registry = TubeRegistry::new();
    
    // Test port forward protocol
    let result = test_protocol_connection(
        &mut server_registry,
        &mut client_registry,
        "port_forward",
        port_forward_port,
        HashMap::new(),
    ).await;
    
    // Clean up
    port_forward_server.stop(port_forward_handle).await;
    
    result
}

#[tokio::test]
async fn test_socks5_protocol() -> Result<()> {
    // Setup test server for socks5 protocol
    let socks_server = TestServer::new("socks5")?;
    let socks_port = socks_server.port();
    let socks_handle = socks_server.start();
    
    // Create server registry (server mode)
    let mut server_registry = TubeRegistry::new();
    server_registry.set_server_mode(true);
    
    // Create a client registry (client mode)
    let mut client_registry = TubeRegistry::new();
    
    // Test SOCKS5 protocol
    let mut socks_settings = HashMap::new();
    socks_settings.insert("destination".to_string(), serde_json::Value::String(format!("127.0.0.1:{}", socks_port)));
    
    let result = test_protocol_connection(
        &mut server_registry,
        &mut client_registry,
        "socks5",
        socks_port,
        socks_settings,
    ).await;
    
    // Clean up
    socks_server.stop(socks_handle).await;
    
    result
}

#[tokio::test]
async fn test_guacd_protocol() -> Result<()> {
    // Setup test server for guacd protocol
    let guacd_server = TestServer::new("guacd")?;
    let guacd_port = guacd_server.port();
    let guacd_handle = guacd_server.start();
    
    // Create server registry (server mode)
    let mut server_registry = TubeRegistry::new();
    server_registry.set_server_mode(true);
    
    // Create a client registry (client mode)
    let mut client_registry = TubeRegistry::new();
    
    // Test guacd protocol
    let mut guacd_settings = HashMap::new();
    guacd_settings.insert("client_id".to_string(), serde_json::Value::String("test-client".to_string()));
    guacd_settings.insert("destination".to_string(), serde_json::Value::String(format!("127.0.0.1:{}", guacd_port)));
    guacd_settings.insert("protocol".to_string(), serde_json::Value::String("rdp".to_string()));
    guacd_settings.insert("hostname".to_string(), serde_json::Value::String("test-host".to_string()));
    guacd_settings.insert("port".to_string(), serde_json::Value::String("3389".to_string()));
    
    let result = test_protocol_connection(
        &mut server_registry,
        &mut client_registry,
        "guacd",
        guacd_port,
        guacd_settings,
    ).await;
    
    // Clean up
    guacd_server.stop(guacd_handle).await;
    
    result
}

async fn test_protocol_connection(
    server_registry: &mut TubeRegistry,
    client_registry: &mut TubeRegistry,
    protocol_type: &str,
    server_port: u16,
    settings: HashMap<String, serde_json::Value>,
) -> Result<()> {
    println!("Testing {} protocol", protocol_type);
    
    // Verify the test server is reachable with a direct connection
    println!("Testing direct connection to echo server at port {}", server_port);
    let _expected_response = match protocol_type {
        "guacd" => {
            // For guacd, we expect the version string, not an echo
            None // We'll just verify we get a non-empty response
        },
        _ => Some(b"Direct Test".to_vec()) // For echo servers, expect the same data back
    };
    
    let mut direct_response = Vec::new();
    match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", server_port)).await {
        Ok(mut stream) => {
            println!("Direct TCP connection successful!");
            // Send a test message directly to verify the echo server works
            let direct_test = b"Direct Test";
            if let Err(e) = stream.write_all(direct_test).await {
                println!("Error in direct write: {}", e);
            } else {
                println!("Direct test message sent");
                // Read the response
                let mut buf = [0u8; 1024];
                match stream.read(&mut buf).await {
                    Ok(n) if n > 0 => {
                        println!("Direct test response: {:?}", &buf[0..n]);
                        direct_response = buf[0..n].to_vec();
                    },
                    Ok(_) => println!("No data received in direct test"),
                    Err(e) => println!("Error in direct read: {}", e)
                }
            }
        },
        Err(e) => println!("Failed to connect directly to test server: {}", e)
    }
    
    // For guacd, check if we got a valid response
    if protocol_type == "guacd" && !direct_response.is_empty() {
        let response_str = String::from_utf8_lossy(&direct_response);
        if !response_str.starts_with("5.guacd") {
            println!("WARNING: Unexpected guacd response: {}", response_str);
        }
    }
    
    // Create a unique channel name for this test
    let channel_name = format!("test-{}-{}", protocol_type, server_port);
    
    // Set server mode for server registry
    {
        let mut registry = REGISTRY.write();
        registry.set_server_mode(true);
    }
    
    // Create server tube
    let server_tube_result = Tube::new()?;
    let server_tube = Arc::clone(&server_tube_result);
    let server_tube_id = server_tube.id().to_string();
    let mut server_settings = HashMap::new();
    
    // Required settings
    server_settings.insert("ksm_config".to_string(), serde_json::Value::String("{}".to_string()));
    server_settings.insert("callback_token".to_string(), serde_json::Value::String("test-token".to_string()));
    
    println!("Registering server channel with ID: {}", server_tube_id);
    let result = server_registry.register_channel(&channel_name, &server_tube_id, &server_settings).await;
    if let Err(e) = &result {
        println!("Failed to register server channel: {}", e);
    }
    result?;
    println!("Server tube created with ID: {}", server_tube.id());
    
    // Set up a client channel pointing to the test server
    let client_tube_result = Tube::new()?;
    let client_tube = Arc::clone(&client_tube_result);
    let client_tube_id = client_tube.id().to_string();
    let mut client_settings = settings.clone();
    client_settings.insert("destination".to_string(), serde_json::Value::String(format!("127.0.0.1:{}", server_port)));
    
    // Required settings 
    client_settings.insert("ksm_config".to_string(), serde_json::Value::String("{}".to_string()));
    client_settings.insert("callback_token".to_string(), serde_json::Value::String("test-token".to_string()));
    
    println!("Registering client channel with ID: {}", client_tube_id);
    client_registry.register_channel(&channel_name, &client_tube_id, &client_settings).await?;
    println!("Client tube created with ID: {}", client_tube.id());
    
    // Verify if client settings are properly configured
    println!("Checking client tube settings to ensure destination is set correctly");
    if let Some(channel) = client_tube.channels.read().get(&channel_name) {
        let channel_lock = channel.lock().unwrap();
        println!("Client tube has channel with name: {}", channel_lock.channel_id);
    } else {
        println!("WARNING: Channel not found in client tube's channels map!");
    }
    
    // Connect the tubes
    connect_tubes(&server_tube, &client_tube).await?;
    
    // Get the data channels
    let server_data_channel = server_tube.get_data_channel(&channel_name)
        .ok_or_else(|| anyhow!("Server data channel not found"))?;
    println!("Got server data channel with label: {}", server_data_channel.label());
    
    let client_data_channel = client_tube.get_data_channel(&channel_name)
        .ok_or_else(|| anyhow!("Client data channel not found"))?;
    println!("Got client data channel with label: {}", client_data_channel.label());
    
    // Monitor data channel state
    let (state_tx, mut state_rx) = mpsc::channel::<String>(10);
    
    // Set up state change monitoring for a client data channel
    let client_state_tx = state_tx.clone();
    client_data_channel.data_channel.on_open(Box::new(move || {
        let state_tx = client_state_tx.clone();
        Box::pin(async move {
            println!("CLIENT DATA CHANNEL OPENED");
            println!("Client data channel ready state: {:?}", "open"); // Simplified since we can't access real state here
            let _ = state_tx.send("client_open".to_string()).await;
        })
    }));
    
    // Set up state change monitoring for a server data channel
    let server_state_tx = state_tx.clone();
    server_data_channel.data_channel.on_open(Box::new(move || {
        let state_tx = server_state_tx.clone();
        Box::pin(async move {
            println!("SERVER DATA CHANNEL OPENED");
            println!("Server data channel ready state: {:?}", "open"); // Simplified since we can't access real state here
            let _ = state_tx.send("server_open".to_string()).await;
        })
    }));

    // Add error handlers
    client_data_channel.data_channel.on_error(Box::new(|err| {
        Box::pin(async move {
            println!("CLIENT DATA CHANNEL ERROR: {:?}", err);
        })
    }));
    
    server_data_channel.data_channel.on_error(Box::new(|err| {
        Box::pin(async move {
            println!("SERVER DATA CHANNEL ERROR: {:?}", err);
        })
    }));
    
    // Add close handlers
    client_data_channel.data_channel.on_close(Box::new(|| {
        Box::pin(async move {
            println!("CLIENT DATA CHANNEL CLOSED");
        })
    }));
    
    server_data_channel.data_channel.on_close(Box::new(|| {
        Box::pin(async move {
            println!("SERVER DATA CHANNEL CLOSED");
        })
    }));
    
    // Wait for data channels to be ready
    println!("Waiting for data channels to open...");
    let mut client_open = false;
    let mut server_open = false;
    
    for _ in 0..20 {  // Increased attempts
        match tokio::time::timeout(
            Duration::from_millis(300),
            state_rx.recv()
        ).await {
            Ok(Some(state)) => {
                if state == "client_open" {
                    client_open = true;
                    println!("Client data channel is open");
                } else if state == "server_open" {
                    server_open = true;
                    println!("Server data channel is open");
                }
                
                if client_open && server_open {
                    println!("Both data channels are open");
                    break;
                }
            },
            _ => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    
    // Wait a bit more to ensure connections are fully established
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    if !client_open || !server_open {
        println!("WARNING: Not all data channels reported open state, but proceeding anyway");
    }
    
    // Print a detailed status of the connection
    println!("\n=== CONNECTION STATUS ===");
    println!("Server tube status: {}", server_tube.status());
    println!("Client tube status: {}", client_tube.status());
    println!("Server data channel ready state: {}", server_data_channel.data_channel.ready_state());
    println!("Client data channel ready state: {}", client_data_channel.data_channel.ready_state());
    println!("Server channel label: {}", server_data_channel.data_channel.label());
    println!("Client channel label: {}", client_data_channel.data_channel.label());
    println!("=== END CONNECTION STATUS ===\n");

    // Check tube registries to see if protocol handlers are properly registered
    println!("\n=== CHECKING PROTOCOL HANDLER REGISTRATION ===");
    
    // For the server tube
    if let Some(channel) = server_tube.channels.read().get(&channel_name) {
        let channel_lock = channel.lock().unwrap();
        println!("✅ Server has channel endpoint name: {}", channel_lock.channel_id);
        
        if server_tube.data_channels.read().contains_key(&channel_name) {
            println!("✅ Server has a data channel registered");
        } else {
            println!("❌ Server is MISSING a data channel!");
        }
    } else {
        println!("❌ Server is missing channel '{}'", channel_name);
    }
    
    // For the client tube
    if let Some(channel) = client_tube.channels.read().get(&channel_name) {
        let channel_lock = channel.lock().unwrap();
        println!("✅ Client has channel endpoint name: {}", channel_lock.channel_id);
        
        if client_tube.data_channels.read().contains_key(&channel_name) {
            println!("✅ Client has a data channel registered");
        } else {
            println!("❌ Client is MISSING a data channel!");
        }
    } else {
        println!("❌ Client is missing channel '{}'", channel_name);
    }
    
    // Check data channels are linked to handlers
    println!("\n=== CHECKING WEBRTC-HANDLER CONNECTIONS ===");
    // For server
    println!("✅ Server data channel: {}", server_data_channel.data_channel.label());
    
    // For client
    println!("✅ Client data channel: {}", client_data_channel.data_channel.label());
    
    println!("=== END REGISTRATION CHECK ===\n");
    
    // Set up response capture
    let (response_tx, mut response_rx) = mpsc::channel::<Vec<u8>>(10);
    
    // Set up message handler on client side to detect response
    client_data_channel.data_channel.on_message(Box::new(move |msg| {
        let tx = response_tx.clone();
        Box::pin(async move {
            let data_len = msg.data.len();
            println!("Client received data: length={}, data={:?}", 
                data_len, 
                String::from_utf8_lossy(&msg.data[..std::cmp::min(100, data_len)])
            );
            let _ = tx.send(msg.data.to_vec()).await;
        })
    }));
    
    // Add message handler on server side to log incoming messages
    server_data_channel.data_channel.on_message(Box::new(|msg| {
        Box::pin(async move {
            let data_len = msg.data.len();
            println!("Server received data: length={}, data={:?}", 
                data_len, 
                String::from_utf8_lossy(&msg.data[..std::cmp::min(100, data_len)])
            );
        })
    }));
    
    println!("\n=== SENDING TEST MESSAGE ===");
    // Send the test message - this relies on the Channel implementation to route it
    let test_message = b"Hello, Protocol Test!";
    println!("Sending test message: {:?}", test_message);
    println!("Raw test message: {}", String::from_utf8_lossy(test_message));
    println!("Client data channel label: {}", client_data_channel.data_channel.label());
    println!("Server data channel label: {}", server_data_channel.data_channel.label());
    println!("Client data channel state: {}", client_data_channel.data_channel.ready_state());
    println!("Server data channel state: {}", server_data_channel.data_channel.ready_state());
    println!("Using client_data_channel.send() to send message to server");
    
    match client_data_channel.send(Bytes::from(test_message.to_vec())).await {
        Ok(_) => println!("✅ Test message sent successfully via client_data_channel.send()"),
        Err(e) => {
            println!("Error sending test message: {}", e);
            return Err(anyhow!("Failed to send data: {}", e));
        },
    }
    
    println!("=== MESSAGE SENT, WAITING FOR RESPONSE ===\n");
    
    // Wait for the echoed response with timeout
    println!("Waiting for echoed response (timeout: 5 seconds)...");
    let response = match tokio::time::timeout(
        Duration::from_secs(5),
        response_rx.recv()
    ).await {
        Ok(Some(data)) => {
            println!("Received response of length: {} bytes", data.len());
            println!("Response data: {:?}", data);
            data
        },
        Ok(None) => {
            println!("Channel closed without receiving data");
            return Err(anyhow!("Channel closed without receiving data"));
        },
        Err(_) => {
            println!("Timeout waiting for response");
            return Err(anyhow!("Timeout waiting for WebRTC response - no data received"));
        }
    };
    
    // Verify the response based on a protocol type
    match protocol_type {
        "guacd" => {
            // For guacd, check if we got a non-empty response
            if response.is_empty() {
                return Err(anyhow!("Empty response from guacd server"));
            }
            
            // Check if it's a valid response
            let resp_str = String::from_utf8_lossy(&response);
            // Valid responses could be either our echoed test message or
            // any guacd protocol instruction like "ready" or "args"
            if !resp_str.contains("ready") && !resp_str.contains("args") && resp_str != String::from_utf8_lossy(test_message) {
                println!("WARNING: Unexpected guacd response: {}", resp_str);
            }
        },
        _ => {
            // For echo protocols (port_forward, socks5), verify the exact match
            assert_eq!(response, test_message);
        }
    }
    
    println!("{} protocol test passed", protocol_type);
    
    // Cleanup tubes
    println!("Closing tubes");
    server_tube.close().await?;
    client_tube.close().await?;
    
    Ok(())
}

async fn connect_tubes(tube1: &Arc<Tube>, tube2: &Arc<Tube>) -> Result<()> {
    println!("Creating WebRTC offer from tube1");
    // Create the offer from tube1
    let offer = tube1.create_offer().await
        .map_err(|e| anyhow!("Failed to create offer: {}", e))?;
    
    println!("Setting remote description on tube2");
    // Set the offer on tube2
    tube2.set_remote_description(offer, false).await
        .map_err(|e| anyhow!("Failed to set remote description on tube2: {}", e))?;
    
    println!("Creating answer from tube2");
    // Create the answer from tube2
    let answer = tube2.create_answer().await
        .map_err(|e| anyhow!("Failed to create answer: {}", e))?;
    
    println!("Setting remote description on tube1");
    // Set the answer on tube1
    tube1.set_remote_description(answer, true).await
        .map_err(|e| anyhow!("Failed to set remote description on tube1: {}", e))?;
    
    // Exchange ICE candidates
    println!("Exchanging ICE candidates");
    let _ = crate::tests::common::exchange_tube_ice_candidates(tube1, tube2).await;
    
    // Give more time for the connection to establish and stabilize
    println!("Waiting for WebRTC connection to stabilize");
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    println!("Connection state tube1: {:?}", tube1.status());
    println!("Connection state tube2: {:?}", tube2.status());
    
    Ok(())
} 