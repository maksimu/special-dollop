//! Integration tests for VNC handler
//!
//! These tests require a running VNC server. Start one with:
//!   docker-compose -f docker-compose.test.yml up -d vnc
//!
//! Run tests with:
//!   cargo test --package guacr-vnc --test integration_test -- --include-ignored
//!
//! Connection details:
//!   Host: localhost:5900
//!   Password: test_password

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

/// Test connection timeout
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Check if a port is open (server is running)
async fn port_is_open(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

mod vnc_handler_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use guacr_vnc::VncHandler;
    use tokio::sync::mpsc;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 5900;
    const PASSWORD: &str = "test_password";

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping VNC tests - server not available on {}:{}",
                HOST, PORT
            );
            eprintln!(
                "Start a test VNC server with: docker-compose -f docker-compose.test.yml up -d vnc"
            );
            return true;
        }
        false
    }

    #[tokio::test]
    #[ignore] // Requires VNC server
    async fn test_vnc_connection_basic() {
        if skip_if_not_available().await {
            return;
        }

        let handler = VncHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
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
        println!("VNC: First message: {}", msg_str);

        // VNC should send ready or size instruction
        assert!(
            msg_str.contains("ready") || msg_str.contains("size"),
            "Expected ready or size instruction, got: {}",
            msg_str
        );

        // Wait for size and initial frame
        let mut received_size = false;
        let mut received_img = false;

        for _ in 0..20 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("size") {
                        received_size = true;
                    }
                    if msg_str.contains("img") {
                        received_img = true;
                        break;
                    }
                }
                _ => break,
            }
        }

        println!(
            "VNC: received_size={}, received_img={}",
            received_size, received_img
        );

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires VNC server
    async fn test_vnc_mouse_input() {
        if skip_if_not_available().await {
            return;
        }

        let handler = VncHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for connection
        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("img") {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Send mouse move
        let mouse_instr = "5.mouse,3.100,3.100,1.0;";
        from_client_tx
            .send(Bytes::from(mouse_instr))
            .await
            .expect("Send failed");

        // Send mouse click
        let click_instr = "5.mouse,3.100,3.100,1.1;";
        from_client_tx
            .send(Bytes::from(click_instr))
            .await
            .expect("Send failed");

        // Wait for any response
        tokio::time::sleep(Duration::from_millis(500)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires VNC server
    async fn test_vnc_security_readonly() {
        if skip_if_not_available().await {
            return;
        }

        let handler = VncHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("read-only".to_string(), "true".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for connection
        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("size") {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Send key event (should be blocked in read-only mode)
        let key_instr = "3.key,2.65,1.1;";
        from_client_tx
            .send(Bytes::from(key_instr))
            .await
            .expect("Send failed");

        // Send mouse click (should be blocked in read-only mode)
        let click_instr = "5.mouse,3.100,3.100,1.1;";
        from_client_tx
            .send(Bytes::from(click_instr))
            .await
            .expect("Send failed");

        // Mouse move should still work
        let move_instr = "5.mouse,3.200,3.200,1.0;";
        from_client_tx
            .send(Bytes::from(move_instr))
            .await
            .expect("Send failed");

        tokio::time::sleep(Duration::from_millis(500)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }
}

mod unit_tests {
    use guacr_handlers::ProtocolHandler;
    use guacr_vnc::VncHandler;

    #[test]
    fn test_vnc_handler_creation() {
        let handler = VncHandler::with_defaults();
        assert_eq!(handler.name(), "vnc");
    }

    #[tokio::test]
    async fn test_vnc_handler_health_check() {
        let handler = VncHandler::with_defaults();
        let health = handler.health_check().await;
        assert!(health.is_ok());
    }
}
