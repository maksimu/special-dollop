// src/channel/tests/guacd_handshake_tests.rs
// Create a new file for Guacd handshake tests.

#![cfg(test)]
use bytes::{Buf, BytesMut};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::time::timeout;

use crate::channel::guacd_parser::{
    GuacdInstruction, GuacdParser, PeekError, ARG_SEP, ELEM_SEP, INST_TERM,
};
use crate::webrtc_data_channel::WebRTCDataChannel;

// --- Mock RTCDataChannel for testing ---
use crate::channel::Channel;
use crate::models::TunnelTimeouts;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel as ActualRTCDataChannel;
use webrtc::peer_connection::configuration::RTCConfiguration;

async fn create_test_rtc_data_channel(label: &str) -> Arc<ActualRTCDataChannel> {
    let mut m = MediaEngine::default();
    m.register_default_codecs().unwrap();
    let api = APIBuilder::new().with_media_engine(m).build();
    let config = RTCConfiguration::default();
    let pc = api.new_peer_connection(config).await.unwrap();

    let data_channel_init = RTCDataChannelInit {
        ordered: Some(true),
        max_retransmits: None,
        ..Default::default()
    };
    pc.create_data_channel(label, Some(data_channel_init))
        .await
        .unwrap()
}

async fn create_mock_webrtc_data_channel_for_channel_new() -> WebRTCDataChannel {
    let rtc_dc = create_test_rtc_data_channel("mock_dc_label_for_channel").await;
    WebRTCDataChannel::new(rtc_dc)
}

fn encode_guac_instruction(opcode: &str, args: &[&str]) -> Vec<u8> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(opcode.len().to_string().as_bytes());
    buffer.push(ELEM_SEP);
    buffer.extend_from_slice(opcode.as_bytes());

    for arg in args {
        buffer.push(ARG_SEP);
        buffer.extend_from_slice(arg.len().to_string().as_bytes());
        buffer.push(ELEM_SEP);
        buffer.extend_from_slice(arg.as_bytes());
    }
    buffer.push(INST_TERM);
    buffer
}

/// Helper function to parse instructions from buffer and send response when "connect" is found
async fn parse_and_handle_connect_instruction(
    temp_parse_buffer: &mut BytesMut,
    server_stream: &mut DuplexStream,
    test_name: &str,
) -> Result<bool, String> {
    loop {
        let advance_amount = {
            let peek_result = GuacdParser::peek_instruction(&temp_parse_buffer);
            match peek_result {
                Ok(peeked) => {
                    if peeked.opcode == "connect" {
                        let total_length = peeked.total_length_in_buffer;
                        let content_slice = &temp_parse_buffer[..total_length - 1];
                        match GuacdParser::parse_instruction_content(content_slice) {
                            Ok(connect_instr) => {
                                println!("Server ({}): 'connect' successfully parsed: {:?}. Proceeding to send 'goodbye_cruel_world'.", test_name, connect_instr.args);
                                let unexpected_response =
                                    encode_guac_instruction("goodbye_cruel_world", &["param1"]);
                                server_stream
                                    .write_all(&unexpected_response)
                                    .await
                                    .map_err(|e| {
                                        format!(
                                            "Server ({}): Failed to send unexpected_opcode: {}",
                                            test_name, e
                                        )
                                    })?;
                                println!("Server ({}): Sent goodbye_cruel_world", test_name);
                                server_stream.shutdown().await.ok();
                                return Ok(true); // Connect found and handled
                            }
                            Err(e) => {
                                println!("Server ({}): 'connect' peeked but failed to parse fully: {}. Content: {:?}", test_name, e, content_slice);
                                None
                            }
                        }
                    } else {
                        // Not a "connect" instruction, advance anyway for debugging
                        println!(
                            "Server ({}): Parsed instruction: opcode='{}', args={:?}",
                            test_name, peeked.opcode, peeked.args
                        );
                        Some(peeked.total_length_in_buffer)
                    }
                }
                Err(PeekError::Incomplete) => None,
                Err(e) => {
                    println!("Server ({}): Peek Error: {:?}. Stopping.", test_name, e);
                    None
                }
            }
        };

        if let Some(advance) = advance_amount {
            temp_parse_buffer.advance(advance);
            if temp_parse_buffer.is_empty() {
                break;
            }
        } else {
            break;
        }
    }
    Ok(false) // Connect not found in current buffer
}

async fn recv_and_get_instruction(stream: &mut DuplexStream) -> Result<GuacdInstruction, String> {
    let mut local_buffer = BytesMut::with_capacity(1024);
    let mut read_attempts = 0;
    const MAX_READ_ATTEMPTS: usize = 5;
    // println! (SERVER_RECV_FN_ENTRY): About to enter recv loop.

    loop {
        // println! (SERVER_RECV_FN_LOOP_START): Top of loop, read_attempts: {}.
        if read_attempts >= MAX_READ_ATTEMPTS {
            // println! (SERVER_RECV_FN_MAX_ATTEMPTS): Exceeded max read attempts.
            return Err(
                "Server: Exceeded max read attempts waiting for a complete instruction".to_string(),
            );
        }
        // println! (SERVER_RECV_FN_PEEK_ATTEMPT): Attempting to peek. buffer len: {}. Content (trunc 64): {:?}
        //local_buffer.len(), &local_buffer[..std::cmp::min(local_buffer.len(), 64)]

        // Extract what we need from a peek result before any buffer mutations
        let process_result = {
            let peek_result = GuacdParser::peek_instruction(&local_buffer);
            match peek_result {
                Ok(peeked_instr) => {
                    let total_length = peeked_instr.total_length_in_buffer;
                    let content_slice = &local_buffer[..total_length - 1];
                    let instr = GuacdParser::parse_instruction_content(content_slice)
                        .map_err(|e| format!("Server: Failed to fully parse peeked instruction: {}. Content: {:?}", e, content_slice))?;

                    // Return the instruction and advance amount
                    Some((instr, total_length))
                }
                Err(PeekError::Incomplete) => {
                    // Handle an incomplete case
                    None
                }
                Err(PeekError::InvalidFormat(e)) => {
                    return Err(format!(
                        "Server: Invalid Guacd format in received data: {}",
                        e
                    ));
                }
                Err(PeekError::Utf8Error(e)) => {
                    return Err(format!("Server: UTF-8 error in received Guacd data: {}", e));
                }
            }
        }; // peek_result is dropped here

        // Now we can safely mutate local_buffer
        if let Some((instr, advance_len)) = process_result {
            local_buffer.advance(advance_len);
            return Ok(instr);
        }

        // Handle the incomplete case after the match to avoid borrowing issues
        // println! (SERVER_RECV_FN_PEEK_INCOMPLETE): Peek Incomplete. Current buffer len: {}.
        //local_buffer.len()
        if local_buffer.capacity() == 0 {
            local_buffer.reserve(1024);
        }
        let mut temp_read_buf = [0u8; 512];
        // println! (SERVER_RECV_FN_AWAIT_READ): About to await stream.read().
        match stream.read(&mut temp_read_buf).await {
            Ok(0) => {
                // println! (SERVER_RECV_FN_READ_EOF): Read EOF.
                return Err("Server: EOF while waiting for a complete instruction".to_string());
            }
            Ok(n) => {
                // println! (SERVER_RECV_FN_READ_OK): Read {} bytes. Data (trunc 64): {:?}
                //n, &temp_read_buf[..std::cmp::min(n, 64)]
                if local_buffer.capacity() < local_buffer.len() + n {
                    local_buffer.reserve(local_buffer.len() + n - local_buffer.capacity());
                }
                local_buffer.extend_from_slice(&temp_read_buf[..n]);
            }
            Err(e) => {
                // println! (SERVER_RECV_FN_READ_ERR): Read error: {}.
                //e
                return Err(format!("Server: Read error from stream: {}", e));
            }
        }
        read_attempts += 1;
        // println! (SERVER_RECV_FN_LOOP_END): Bottom of loop, read_attempts now: {}.
        //read_attempts
    }
}

#[tokio::test]
async fn test_guacd_handshake_successful() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    guacd_params.insert("hostname".to_string(), "testserver".to_string());
    guacd_params.insert("port".to_string(), "3389".to_string());
    guacd_params.insert("width".to_string(), "1024".to_string());
    guacd_params.insert("height".to_string(), "768".to_string());
    guacd_params.insert("dpi".to_string(), "96".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    let guacd_params_for_channel = guacd_params.clone();
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params_for_channel),
    );
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_id".to_string(),
        timeouts: Some(TunnelTimeouts::default()),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel");
    let conn_no = 1u32;

    let server_task = tokio::spawn(async move {
        println!("Server (success_new): Waiting for select...");
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server: Failed to get select instruction");
        assert_eq!(received_select.opcode, "select");
        assert_eq!(received_select.args, vec!["rdp"]);
        println!(
            "Server (success_new): Received select: {:?}",
            received_select
        );

        let args_response = encode_guac_instruction(
            "args",
            &[
                "VERSION_1_5_0",
                "hostname",
                "port",
                "username",
                "password",
                "domain",
            ],
        );
        server_stream
            .write_all(&args_response)
            .await
            .expect("Server failed to send args");
        println!("Server (success_new): Sent args");

        println!("Server (success_new): V2: Attempting raw read for all subsequent client data (size, audio, video, image, connect)...");
        let mut diagnostic_buffer = BytesMut::with_capacity(2048);
        let mut read_buf_array = [0u8; 512];
        let mut connect_received = false;

        for i in 0..20 {
            // Try to read multiple times
            match tokio::time::timeout(
                Duration::from_millis(250),
                server_stream.read(&mut read_buf_array),
            )
            .await
            {
                Ok(Ok(0)) => {
                    println!(
                        "Server (success_new): V2: read EOF. Total data: {}. Loop iter: {}",
                        diagnostic_buffer.len(),
                        i
                    );
                    break;
                }
                Ok(Ok(n)) => {
                    diagnostic_buffer.extend_from_slice(&read_buf_array[..n]);
                    println!("Server (success_new): V2: read {} bytes. Total: {}. Loop iter: {}. Data just read: {:?}", n, diagnostic_buffer.len(), i, &read_buf_array[..n]);

                    let mut temp_parse_buffer = diagnostic_buffer.clone();
                    loop {
                        let advance_amount = {
                            let peek_result = GuacdParser::peek_instruction(&temp_parse_buffer);
                            match peek_result {
                                Ok(peeked) => {
                                    let total_length = peeked.total_length_in_buffer;
                                    let content_slice = &temp_parse_buffer[..total_length - 1];
                                    match GuacdParser::parse_instruction_content(content_slice) {
                                        Ok(instr) => {
                                            println!("Server (success_new): V2: Parsed: {{opcode: '{}', args: {:?}}}", instr.opcode, instr.args);
                                            if instr.opcode == "size" {
                                                assert_eq!(instr.args, vec!["1024", "768", "96"]);
                                            } else if instr.opcode == "audio"
                                                || instr.opcode == "video"
                                                || instr.opcode == "image"
                                            {
                                                assert!(instr.args.is_empty());
                                            } else if instr.opcode == "connect" {
                                                assert_eq!(
                                                    instr.args,
                                                    vec![
                                                        "VERSION_1_5_0",
                                                        "testserver",
                                                        "3389",
                                                        "",
                                                        "",
                                                        ""
                                                    ]
                                                );
                                                connect_received = true;
                                                println!("Server (success_new): V2: 'connect' received and validated.");
                                            }
                                        }
                                        Err(e) => {
                                            println!("Server (success_new): V2: Parse error for peeked data: {}. Content: {:?}",e, content_slice);
                                            break; // break inner parse loop, try to get more stream data
                                        }
                                    }
                                    Some(total_length) // Return the amount to advance
                                }
                                Err(PeekError::Incomplete) => None, // Need more data from stream
                                Err(e) => {
                                    println!("Server (success_new): V2: Peek Error: {:?}", e);
                                    None
                                }
                            }
                        };

                        if let Some(advance) = advance_amount {
                            temp_parse_buffer.advance(advance);
                            if temp_parse_buffer.is_empty() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if connect_received {
                        break;
                    } // Exit outer read loop if connect is processed
                }
                Ok(Err(e)) => {
                    println!(
                        "Server (success_new): V2: read error: {}. Loop iter: {}",
                        e, i
                    );
                    break;
                }
                Err(_) => {
                    // Timeout from tokio::time::timeout
                    println!("Server (success_new): V2: read timed out (250ms). Loop iter: {}. Total data: {}. Connect received: {}", i, diagnostic_buffer.len(), connect_received);
                    if connect_received {
                        break;
                    } // Exit if connect was found before timeout
                    if i == 19 {
                        println!("Server (success_new): V2: Max read attempts with timeouts.");
                    }
                }
            }
        }

        if !connect_received {
            panic!("Server (success_new): V2: 'connect' instruction not received/processed. Final buffer ({} bytes): {:?}", diagnostic_buffer.len(), diagnostic_buffer.to_vec());
        }

        println!("Server (success_new): V2: All expected instructions up to 'connect' processed.");
        let ready_response = encode_guac_instruction("ready", &["test-client-id"]);
        server_stream
            .write_all(&ready_response)
            .await
            .expect("Server failed to send ready");
        println!("Server (success_new): Sent ready");
    });

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => {
            println!("Client: Handshake successful");
        }
        Ok(Err(e)) => {
            panic!("Client: Handshake failed: {}", e);
        }
        Err(_) => {
            panic!("Client: Handshake timed out");
        }
    }
    server_task.await.expect("Server task panicked");
}

#[tokio::test]
async fn test_guacd_handshake_join_existing_connection_readonly() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let connection_id_to_join = "existing-session-123".to_string();
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    guacd_params.insert("connectionid".to_string(), connection_id_to_join.clone());
    guacd_params.insert("readonly".to_string(), "true".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_join_id".to_string(),
        timeouts: Some(TunnelTimeouts::default()),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for join test");
    let conn_no = 2u32;
    let server_task = tokio::spawn(async move {
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server: Failed to get select instruction for join");
        assert_eq!(received_select.opcode, "select");
        assert_eq!(received_select.args, vec![connection_id_to_join]);
        println!("Server (join): Received select: {:?}", received_select);
        let server_supported_args = vec!["VERSION_1_5_0", "param-a", "read-only", "param-b"];
        let args_response = encode_guac_instruction("args", &server_supported_args);
        server_stream
            .write_all(&args_response)
            .await
            .expect("Server: Failed to send args for join");
        println!("Server (join): Sent args: {:?}", server_supported_args);
        let received_connect = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server: Failed to get connect instruction for join");
        assert_eq!(received_connect.opcode, "connect");
        assert_eq!(
            received_connect.args,
            vec![
                "VERSION_1_5_0".to_string(),
                "".to_string(),
                "true".to_string(),
                "".to_string()
            ]
        );
        println!("Server (join): Received connect: {:?}", received_connect);
        let ready_response = encode_guac_instruction("ready", &["joined-client-id"]);
        server_stream
            .write_all(&ready_response)
            .await
            .expect("Server: Failed to send ready for join");
        println!("Server (join): Sent ready");
    });
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => println!("Client (join): Handshake successful for join_readonly"),
        Ok(Err(e)) => panic!("Client (join): Handshake failed for join_readonly: {}", e),
        Err(_) => panic!("Client (join): Handshake timed out for join_readonly"),
    }
    server_task
        .await
        .expect("Server task (join_readonly) panicked");
}

#[tokio::test]
async fn test_guacd_handshake_join_existing_connection_not_readonly() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let connection_id_to_join = "existing-session-456".to_string();
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    guacd_params.insert("connectionid".to_string(), connection_id_to_join.clone());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_join_nr_id".to_string(),
        timeouts: Some(TunnelTimeouts::default()),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for join_not_readonly test");
    let conn_no = 3u32;
    let server_task = tokio::spawn(async move {
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (nr): Failed to get select");
        assert_eq!(received_select.opcode, "select");
        assert_eq!(received_select.args, vec![connection_id_to_join]);
        println!("Server (join_nr): Received select: {:?}", received_select);
        let server_supported_args = vec!["VERSION_1_5_0", "param-a", "read-only", "param-b"];
        let args_response = encode_guac_instruction("args", &server_supported_args);
        server_stream
            .write_all(&args_response)
            .await
            .expect("Server (nr): Failed to send args");
        println!("Server (join_nr): Sent args: {:?}", server_supported_args);
        let received_connect = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (nr): Failed to get connect");
        assert_eq!(received_connect.opcode, "connect");
        assert_eq!(
            received_connect.args,
            vec![
                "VERSION_1_5_0".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string()
            ]
        );
        println!("Server (join_nr): Received connect: {:?}", received_connect);
        let ready_response = encode_guac_instruction("ready", &["joined-client-nr-id"]);
        server_stream
            .write_all(&ready_response)
            .await
            .expect("Server (nr): Failed to send ready");
        println!("Server (join_nr): Sent ready");
    });
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => println!("Client (join_nr): Handshake successful"),
        Ok(Err(e)) => panic!("Client (join_nr): Handshake failed: {}", e),
        Err(_) => panic!("Client (join_nr): Handshake timed out"),
    }
    server_task.await.expect("Server task (join_nr) panicked");
}

#[tokio::test]
async fn test_guacd_handshake_failure_wrong_opcode_instead_of_args() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_fail_args_id".to_string(),
        timeouts: Some(TunnelTimeouts::default()),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for fail_args test");
    let conn_no = 4u32;
    let server_task = tokio::spawn(async move {
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (fail_args): Failed to get select");
        assert_eq!(received_select.opcode, "select");
        println!("Server (fail_args): Received select: {:?}", received_select);
        let unexpected_response = encode_guac_instruction("unexpected_opcode", &["param1"]);
        server_stream
            .write_all(&unexpected_response)
            .await
            .expect("Server (fail_args): Failed to send unexpected_opcode");
        println!("Server (fail_args): Sent unexpected_opcode");
        server_stream.shutdown().await.ok();
    });
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => panic!("Client (fail_args): Handshake unexpectedly succeeded"),
        Ok(Err(e)) => {
            let err_string = e.to_string();
            println!(
                "Client (fail_args): Handshake failed as expected: {}",
                err_string
            );
            assert!(err_string.contains("Expected Guacd opcode 'args'") && err_string.contains("got 'unexpected_opcode'"),
                    "Handshake error message validation failed. Expected to find fragments related to wrong opcode. Actual error: {}", err_string);
        }
        Err(_) => panic!("Client (fail_args): Handshake timed out, but expected specific error"),
    }
    server_task.await.expect("Server task (fail_args) panicked");
}

#[tokio::test]
async fn test_guacd_handshake_failure_wrong_opcode_instead_of_ready() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    guacd_params.insert("hostname".to_string(), "testserver_fail_ready".to_string());
    guacd_params.insert("port".to_string(), "3389".to_string());
    guacd_params.insert("width".to_string(), "1024".to_string());
    guacd_params.insert("height".to_string(), "768".to_string());
    guacd_params.insert("dpi".to_string(), "96".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_fail_ready_id".to_string(),
        timeouts: Some(TunnelTimeouts::default()),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for fail_ready test");
    let conn_no = 5u32;
    let server_task = tokio::spawn(async move {
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (fail_ready): Failed to get select");
        assert_eq!(received_select.opcode, "select");
        println!(
            "Server (fail_ready): Received select: {{opcode: '{}', args: {:?}}}",
            received_select.opcode, received_select.args
        );

        let args_response =
            encode_guac_instruction("args", &["VERSION_1_5_0", "hostname", "port", "username"]);
        server_stream
            .write_all(&args_response)
            .await
            .expect("Server (fail_ready): Failed to send args");
        println!("Server (fail_ready): Sent args");

        // MODIFICATION: Bypass recv_and_get_instruction for "size" and go straight to diagnostic raw read.
        println!("Server (fail_ready): DIAGNOSTIC V2: Bypassing 'size' read. Attempting raw read for all subsequent client data (size, audio, video, image, connect)...");

        let mut diagnostic_buffer = BytesMut::with_capacity(2048);
        let mut read_buf_array = [0u8; 512]; // Temporary buffer for each read call

        for i in 0..20 {
            // Increased attempts, shorter timeout per attempt
            match tokio::time::timeout(
                Duration::from_millis(250),
                server_stream.read(&mut read_buf_array),
            )
            .await
            {
                Ok(Ok(0)) => {
                    println!("Server (fail_ready): DIAGNOSTIC V2: read EOF. Total data in diagnostic_buffer: {} bytes. Loop iter: {}", diagnostic_buffer.len(), i);
                    break;
                }
                Ok(Ok(n)) => {
                    diagnostic_buffer.extend_from_slice(&read_buf_array[..n]);
                    println!("Server (fail_ready): DIAGNOSTIC V2: read {} bytes. Total in diagnostic_buffer: {}. Loop iter: {}. Data just read: {:?}", n, diagnostic_buffer.len(), i, &read_buf_array[..n]);

                    // Try to parse "connect" from the accumulated buffer
                    let mut temp_parse_buffer = diagnostic_buffer.clone(); // Clone for parsing attempts
                    match parse_and_handle_connect_instruction(
                        &mut temp_parse_buffer,
                        &mut server_stream,
                        "fail_ready",
                    )
                    .await
                    {
                        Ok(true) => return, // Connect found and handled, end server task
                        Ok(false) => {}     // Connect not found, continue reading
                        Err(e) => {
                            println!(
                                "Server (fail_ready): Error handling connect instruction: {}",
                                e
                            );
                            return;
                        }
                    }
                }
                Ok(Err(e)) => {
                    println!(
                        "Server (fail_ready): DIAGNOSTIC V2: read error: {}. Loop iter: {}",
                        e, i
                    );
                    break;
                }
                Err(_) => {
                    // Timeout from tokio::time::timeout
                    println!("Server (fail_ready): DIAGNOSTIC V2: read timed out (250ms). Loop iter: {}. Total data in diagnostic_buffer: {} bytes.", i, diagnostic_buffer.len());
                    // Check if connect is already in the buffer before breaking the outer loop
                    if diagnostic_buffer
                        .windows(7)
                        .any(|window| window == b"connect")
                    {
                        // Quick check for "connect"
                        println!("Server (fail_ready): DIAGNOSTIC V2: 'connect' substring found on timeout. Attempting final parse.");
                        // Re-attempt parse on timeout, similar to above
                        let mut final_parse_buffer = diagnostic_buffer.clone();
                        match parse_and_handle_connect_instruction(
                            &mut final_parse_buffer,
                            &mut server_stream,
                            "fail_ready",
                        )
                        .await
                        {
                            Ok(true) => return, // Connect found and handled
                            Ok(false) => {}     // Connect not found
                            Err(e) => {
                                println!("Server (fail_ready): Error handling connect instruction on timeout: {}", e);
                            }
                        }
                    }
                }
            }
            // If connect was found and handled, we would have returned. If the loop finishes, connect wasn't fully processed.
        }
        println!("Server (fail_ready): DIAGNOSTIC V2: Exited diagnostic read loop. Final diagnostic_buffer ({} bytes): {:?}", diagnostic_buffer.len(), diagnostic_buffer.to_vec());
        println!(
            "Server (fail_ready): DIAGNOSTIC V2: as string (lossy): {}",
            String::from_utf8_lossy(&diagnostic_buffer)
        );

        // If connect wasn't processed, the test should fail as the client will time out waiting for ready/goodbye.
        // We can let the client timeout or force a server-side panic if connect was expected but not found.
        panic!("Server (fail_ready): DIAGNOSTIC V2: 'connect' instruction not successfully processed in diagnostic loop.");

        // The original logic is now entirely replaced by the diagnostic block for this test case.
    });
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => panic!("Client (fail_ready): Handshake unexpectedly succeeded"),
        Ok(Err(e)) => {
            let err_string = e.to_string();
            println!(
                "Client (fail_ready): Handshake failed as expected. Error: {}",
                err_string
            );
            // The error from perform_guacd_handshake will be about expecting 'ready' but getting something else.
            assert!(err_string.contains("Expected Guacd opcode 'ready',") && err_string.contains("got 'goodbye_cruel_world'"),
                    "Handshake error message validation failed. Expected to find fragments related to wrong opcode. Actual error: {}", err_string);
            println!("Client (fail_ready): Correctly failed due to server sending 'goodbye_cruel_world' instead of 'ready'.");
        }
        Err(elapsed) => {
            // Timeout error from tokio::time::timeout
            panic!("Client (fail_ready): Handshake timed out after {:?}, but expected a specific error from guacd (goodbye_cruel_world). This implies the client might have hung or the server didn't send the expected error.", elapsed);
        }
    }
    server_task
        .await
        .expect("Server task (fail_ready) panicked");
}

#[tokio::test]
async fn test_guacd_handshake_failure_timeout_waiting_for_args() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let mut timeouts = TunnelTimeouts::default();
    timeouts.guacd_handshake = Duration::from_millis(100);
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_timeout_args_id".to_string(),
        timeouts: Some(timeouts),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for timeout_args test");
    let conn_no = 6u32;
    let server_task = tokio::spawn(async move {
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (timeout_args): Failed to get select");
        assert_eq!(received_select.opcode, "select");
        println!(
            "Server (timeout_args): Received select: {:?}",
            received_select
        );
        println!("Server (timeout_args): Intentionally not sending 'args'");
        tokio::time::sleep(Duration::from_millis(200)).await;
        server_stream.shutdown().await.ok();
    });
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => panic!("Client (timeout_args): Handshake unexpectedly succeeded"),
        Ok(Err(e)) => {
            println!(
                "Client (timeout_args): Handshake failed with specific error (e.g. EOF): {}",
                e
            );
            assert!(e
                .to_string()
                .contains("EOF during Guacd handshake while waiting for 'args'"));
        }
        Err(_) => {
            println!("Client (timeout_args): Handshake timed out as expected waiting for 'args'");
        }
    }
    let _ = server_task.await;
}

#[tokio::test]
async fn test_guacd_handshake_failure_timeout_waiting_for_ready() {
    let (client_stream, mut server_stream): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (_dc_tx_for_channel, dc_rx_for_channel) = tokio::sync::mpsc::unbounded_channel();
    let mock_webrtc_dc_for_channel = create_mock_webrtc_data_channel_for_channel_new().await;
    let mut guacd_params = HashMap::new();
    guacd_params.insert("protocol".to_string(), "rdp".to_string());
    guacd_params.insert(
        "hostname".to_string(),
        "testserver_timeout_ready".to_string(),
    );
    guacd_params.insert("port".to_string(), "3389".to_string());
    guacd_params.insert("width".to_string(), "1024".to_string());
    guacd_params.insert("height".to_string(), "768".to_string());
    guacd_params.insert("dpi".to_string(), "96".to_string());
    let mut protocol_settings = HashMap::new();
    protocol_settings.insert("conversationType".to_string(), serde_json::json!("rdp"));
    protocol_settings.insert(
        "guacd".to_string(),
        serde_json::json!({
            "guacd_host": "localhost",
            "guacd_port": 4822
        }),
    );
    protocol_settings.insert(
        "guacd_params".to_string(),
        serde_json::json!(guacd_params.clone()),
    );
    let mut timeouts = TunnelTimeouts::default();
    timeouts.guacd_handshake = Duration::from_millis(100);
    let channel = Channel::new(crate::channel::core::ChannelParams {
        webrtc: mock_webrtc_dc_for_channel,
        rx_from_dc: dc_rx_for_channel,
        channel_id: "test_channel_timeout_ready_id".to_string(),
        timeouts: Some(timeouts),
        protocol_settings,
        server_mode: false,
        callback_token: Some("test_callback_token".to_string()),
        ksm_config: Some("test_ksm_config".to_string()),
        client_version: "ms16.5.0".to_string(),
    })
    .await
    .expect("Failed to create channel for timeout_ready test");
    let conn_no = 7u32;

    let server_task = tokio::spawn(async move {
        println!("Server (timeout_ready): Waiting for select...");
        let received_select = recv_and_get_instruction(&mut server_stream)
            .await
            .expect("Server (timeout_ready): Failed to get select");
        assert_eq!(received_select.opcode, "select");
        println!(
            "Server (timeout_ready): Received select: {:?}",
            received_select
        );

        let args_response = encode_guac_instruction("args", &["VERSION_1_5_0", "hostname", "port"]);
        server_stream
            .write_all(&args_response)
            .await
            .expect("Server (timeout_ready): Failed to send args");
        println!("Server (timeout_ready): Sent args");

        println!("Server (timeout_ready): V2: Attempting raw read for client data (size, audio, video, image, connect)...");
        let mut diagnostic_buffer = BytesMut::with_capacity(2048);
        let mut read_buf_array = [0u8; 512];
        let mut connect_instr_received_and_validated = false;

        for i in 0..20 {
            // Try to read multiple times
            match tokio::time::timeout(
                Duration::from_millis(250),
                server_stream.read(&mut read_buf_array),
            )
            .await
            {
                Ok(Ok(0)) => {
                    println!(
                        "Server (timeout_ready): V2: read EOF. Total data: {}. Loop iter: {}",
                        diagnostic_buffer.len(),
                        i
                    );
                    break;
                }
                Ok(Ok(n)) => {
                    diagnostic_buffer.extend_from_slice(&read_buf_array[..n]);
                    println!(
                        "Server (timeout_ready): V2: read {} bytes. Total: {}. Loop iter: {}",
                        n,
                        diagnostic_buffer.len(),
                        i
                    );

                    let mut temp_parse_buffer = diagnostic_buffer.clone();
                    loop {
                        let advance_amount = {
                            let peek_result = GuacdParser::peek_instruction(&temp_parse_buffer);
                            match peek_result {
                                Ok(peeked) => {
                                    let content_slice =
                                        &temp_parse_buffer[..peeked.total_length_in_buffer - 1];
                                    match GuacdParser::parse_instruction_content(content_slice) {
                                        Ok(instr) => {
                                            println!("Server (timeout_ready): V2: Parsed: {{opcode: '{}', args: {:?}}}", instr.opcode, instr.args);
                                            if instr.opcode == "size" {
                                                assert_eq!(instr.args, vec!["1024", "768", "96"]);
                                            } else if instr.opcode == "audio"
                                                || instr.opcode == "video"
                                                || instr.opcode == "image"
                                            {
                                                assert!(instr.args.is_empty());
                                            } else if instr.opcode == "connect" {
                                                // For this test, a client sends: VERSION_1_5_0, hostname, port
                                                // guacd_params has: "hostname":"testserver_timeout_ready", "port": "3389"
                                                // args sent by server: VERSION_1_5_0, hostname, port
                                                // Expected connect args based on perform_guacd_handshake: VERSION_1_5_0, value_for_hostname, value_for_port
                                                assert_eq!(
                                                    instr.args,
                                                    vec![
                                                        "VERSION_1_5_0".to_string(),
                                                        "testserver_timeout_ready".to_string(),
                                                        "3389".to_string()
                                                    ]
                                                );
                                                connect_instr_received_and_validated = true;
                                                println!("Server (timeout_ready): V2: 'connect' received and validated.");
                                            }
                                        }
                                        Err(e) => {
                                            println!("Server (timeout_ready): V2: Parse error for peeked data: {}. Content: {:?}",e, content_slice);
                                            break;
                                        }
                                    }
                                    Some(peeked.total_length_in_buffer) // Return the amount to advance
                                }
                                Err(PeekError::Incomplete) => None,
                                Err(e) => {
                                    println!("Server (timeout_ready): V2: Peek Error: {:?}", e);
                                    None
                                }
                            }
                        };

                        if let Some(advance) = advance_amount {
                            temp_parse_buffer.advance(advance);
                            if temp_parse_buffer.is_empty() {
                                break;
                            }
                            if connect_instr_received_and_validated {
                                break;
                            } //Optimization: if connect is found, exit inner loop.
                        } else {
                            break;
                        }
                    }
                    if connect_instr_received_and_validated {
                        break;
                    } // Exit outer read loop if connect is processed
                }
                Ok(Err(e)) => {
                    println!(
                        "Server (timeout_ready): V2: read error: {}. Loop iter: {}",
                        e, i
                    );
                    break;
                }
                Err(_) => {
                    // Timeout from tokio::time::timeout
                    println!("Server (timeout_ready): V2: read timed out (250ms). Loop iter: {}. Connect received: {}", i, connect_instr_received_and_validated);
                    if connect_instr_received_and_validated {
                        break;
                    }
                    if i == 19 {
                        println!("Server (timeout_ready): V2: Max read attempts with timeouts.");
                    }
                }
            }
        }

        if !connect_instr_received_and_validated {
            // This might happen if client times out before sending full connect, or stream closes.
            println!("Server (timeout_ready): V2: 'connect' instruction was NOT fully received/validated. Final buffer ({} bytes): {:?}", diagnostic_buffer.len(), String::from_utf8_lossy(&diagnostic_buffer));
        } else {
            println!(
                "Server (timeout_ready): V2: All expected instructions up to 'connect' processed."
            );
        }

        println!("Server (timeout_ready): Intentionally not sending 'ready'. Simulating server processing then deliberate silence.");
        // Brief pause to ensure client timeout can occur if it hasn't already.
        // The client timeout is 100 ms, so the server doesn't need to wait long if connect was processed.
        tokio::time::sleep(Duration::from_millis(50)).await;
        println!("Server (timeout_ready): Shutting down stream and terminating server task.");
        server_stream.shutdown().await.ok();
    });

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let handshake_timeout_duration = channel.timeouts.guacd_handshake;
    let channel_id_clone = channel.channel_id.clone();
    let guacd_params_clone = channel.guacd_params.clone();
    let buffer_pool_clone = channel.buffer_pool.clone();
    let handshake_result = timeout(
        handshake_timeout_duration,
        crate::channel::connections::perform_guacd_handshake(
            &mut client_reader,
            &mut client_writer,
            &channel_id_clone,
            conn_no,
            guacd_params_clone,
            buffer_pool_clone,
        ),
    )
    .await;
    match handshake_result {
        Ok(Ok(_)) => panic!("Client (timeout_ready): Handshake unexpectedly succeeded"),
        Ok(Err(e)) => {
            println!(
                "Client (timeout_ready): Handshake failed with specific error (e.g. EOF): {}",
                e
            );
            assert!(e
                .to_string()
                .contains("EOF during Guacd handshake while waiting for 'ready'"));
        }
        Err(_) => {
            println!("Client (timeout_ready): Handshake timed out as expected waiting for 'ready'");
        }
    }
    let _ = server_task.await;
}
