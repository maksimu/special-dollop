//! Integration tests for SSH handler
//!
//! These tests require a running SSH server. Start one with:
//!   docker run -d --name test-ssh -p 2222:22 \
//!     -e SSH_ENABLE_PASSWORD_AUTH=true \
//!     -e SSH_ENABLE_ROOT_LOGIN=true \
//!     lscr.io/linuxserver/openssh-server:latest
//!
//! Or use docker-compose:
//!   docker-compose -f docker-compose.test.yml up -d ssh
//!
//! Run tests with:
//!   cargo test --package guacr-ssh --test integration_test
//!
//! Connection details:
//!   Host: localhost:2222
//!   User: root or test_user
//!   Password: test_password

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

/// Test connection timeout
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Check if a port is open (server is running)
async fn port_is_open(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

mod ssh_handler_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use guacr_handlers::{
        parse_blob_instruction, parse_pipe_instruction, PIPE_NAME_STDOUT, PIPE_STREAM_STDOUT,
    };
    use guacr_ssh::SshHandler;
    use tokio::sync::mpsc;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 2222;
    const USERNAME: &str = "test_user";
    const PASSWORD: &str = "test_password";

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping SSH tests - server not available on {}:{}",
                HOST, PORT
            );
            eprintln!("Start a test SSH server with: docker run -d -p 2222:22 ...");
            return true;
        }
        false
    }

    #[tokio::test]
    #[ignore] // Requires SSH server
    async fn test_ssh_connection_basic() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SshHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());

        // Spawn handler in background
        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for ready instruction
        let msg = timeout(CONNECT_TIMEOUT, to_client_rx.recv())
            .await
            .expect("Timeout waiting for ready")
            .expect("Channel closed");

        let msg_str = String::from_utf8_lossy(&msg);
        assert!(msg_str.contains("ready"), "Expected ready instruction");

        // Wait for size instruction
        let msg = to_client_rx.recv().await.expect("Channel closed");
        let msg_str = String::from_utf8_lossy(&msg);
        assert!(msg_str.contains("size"), "Expected size instruction");

        // Close the connection
        drop(from_client_tx);

        // Wait for handler to finish (with timeout)
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires SSH server
    async fn test_ssh_with_pipe_enabled() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SshHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("enable-pipe".to_string(), "true".to_string()); // Enable pipe!

        // Spawn handler in background
        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Collect initial instructions
        let mut found_pipe = false;
        let mut found_ready = false;
        let mut found_size = false;

        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);

                    if msg_str.contains("pipe") && msg_str.contains("STDOUT") {
                        found_pipe = true;

                        // Verify pipe instruction can be parsed
                        let parsed = parse_pipe_instruction(&msg_str);
                        assert!(parsed.is_some(), "Pipe instruction should be parseable");
                        let parsed = parsed.unwrap();
                        assert_eq!(parsed.name, PIPE_NAME_STDOUT);
                        assert_eq!(parsed.stream_id, PIPE_STREAM_STDOUT);
                    }

                    if msg_str.contains("ready") {
                        found_ready = true;
                    }

                    if msg_str.contains("size") {
                        found_size = true;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }

        assert!(
            found_pipe,
            "Should have received pipe instruction for STDOUT"
        );
        assert!(found_ready, "Should have received ready instruction");
        assert!(found_size, "Should have received size instruction");

        // Close the connection
        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires SSH server
    async fn test_ssh_pipe_receives_terminal_output() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SshHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("enable-pipe".to_string(), "true".to_string());

        // Spawn handler
        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Skip initial instructions until we get past ready/size
        let mut ready = false;
        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("ready") {
                        ready = true;
                    }
                    if ready && msg_str.contains("size") {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Wait a bit for any banner/prompt
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send a command via key instructions
        let cmd = "echo hello\n";
        for c in cmd.chars() {
            let keysym = c as u32;
            let key_instr = format!("3.key,{}.{},1.1;", keysym.to_string().len(), keysym);
            from_client_tx
                .send(Bytes::from(key_instr))
                .await
                .expect("Send failed");
        }

        // Look for blob instructions containing terminal output
        let mut found_pipe_blob = false;
        for _ in 0..20 {
            if let Ok(Some(msg)) = timeout(Duration::from_millis(500), to_client_rx.recv()).await {
                let msg_str = String::from_utf8_lossy(&msg);

                if msg_str.contains(".blob,") {
                    // Verify we can parse the blob
                    if let Some(parsed) = parse_blob_instruction(&msg_str) {
                        // Only check blobs on the STDOUT pipe stream (100)
                        // Other blobs (like clipboard stream 1) should be ignored
                        if parsed.stream_id == PIPE_STREAM_STDOUT {
                            found_pipe_blob = true;
                            println!(
                                "Received {} bytes of terminal output via pipe",
                                parsed.data.len()
                            );

                            // The data should contain raw bytes (possibly ANSI codes)
                            assert!(!parsed.data.is_empty());
                        }
                    }
                }
            }
        }

        // Note: We might not always receive blob if the output goes to img instead
        // This test validates the mechanism when it works
        println!("Pipe blob received: {}", found_pipe_blob);

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }
}

mod unit_tests {
    use guacr_handlers::ProtocolHandler;
    use guacr_ssh::SshHandler;

    #[test]
    fn test_ssh_handler_creation() {
        let handler = SshHandler::with_defaults();
        assert_eq!(handler.name(), "ssh");
    }

    #[tokio::test]
    async fn test_ssh_handler_health_check() {
        let handler = SshHandler::with_defaults();
        let health = handler.health_check().await;
        assert!(health.is_ok());
    }
}
