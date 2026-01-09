//! Integration tests for RDP handler
//!
//! These tests require a running RDP server. Start one with:
//!   docker-compose -f docker-compose.test.yml up -d rdp
//!
//! Run tests with:
//!   cargo test --package guacr-rdp --test integration_test -- --include-ignored
//!
//! Connection details:
//!   Host: localhost:3389
//!   User: linuxuser
//!   Password: alpine

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

/// Test connection timeout
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

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

mod rdp_handler_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use guacr_rdp::RdpHandler;
    use tokio::sync::mpsc;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 3389;
    const USERNAME: &str = "linuxuser";
    const PASSWORD: &str = "alpine";

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping RDP tests - server not available on {}:{}",
                HOST, PORT
            );
            eprintln!(
                "Start a test RDP server with: docker-compose -f docker-compose.test.yml up -d rdp"
            );
            return true;
        }
        false
    }

    #[tokio::test]
    #[ignore] // Requires RDP server
    async fn test_rdp_connection_basic() {
        if skip_if_not_available().await {
            return;
        }

        let handler = RdpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("width".to_string(), "1024".to_string());
        params.insert("height".to_string(), "768".to_string());
        // Use RDP security for xrdp compatibility
        params.insert("security".to_string(), "rdp".to_string());
        params.insert("ignore-cert".to_string(), "true".to_string());

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
        println!("RDP: First message: {}", msg_str);

        // RDP should send ready or size instruction
        assert!(
            msg_str.contains("ready") || msg_str.contains("size"),
            "Expected ready or size instruction, got: {}",
            msg_str
        );

        // Wait for a few more messages (size, img, sync)
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
                        break; // Got an image, connection is working
                    }
                }
                _ => break,
            }
        }

        println!(
            "RDP: received_size={}, received_img={}",
            received_size, received_img
        );

        // Close the connection
        drop(from_client_tx);

        // Wait for handler to finish (with timeout)
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires RDP server
    async fn test_rdp_security_settings() {
        if skip_if_not_available().await {
            return;
        }

        let handler = RdpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("security".to_string(), "rdp".to_string());
        params.insert("ignore-cert".to_string(), "true".to_string());
        // Security settings
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-copy".to_string(), "true".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for connection
        let _ = timeout(CONNECT_TIMEOUT, to_client_rx.recv()).await;

        // In read-only mode, keyboard/mouse should be blocked
        // Send a key event (should be ignored by handler)
        let key_instr = "3.key,2.65,1.1;"; // 'A' key
        from_client_tx
            .send(Bytes::from(key_instr))
            .await
            .expect("Send failed");

        // Wait briefly
        tokio::time::sleep(Duration::from_millis(500)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore] // Requires RDP server
    async fn test_rdp_resize() {
        if skip_if_not_available().await {
            return;
        }

        let handler = RdpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("security".to_string(), "rdp".to_string());
        params.insert("ignore-cert".to_string(), "true".to_string());
        params.insert("width".to_string(), "800".to_string());
        params.insert("height".to_string(), "600".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for initial connection
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

        // Send resize instruction
        let resize_instr = "4.size,4.1024,3.768;";
        from_client_tx
            .send(Bytes::from(resize_instr))
            .await
            .expect("Send failed");

        // Wait for new size response
        let mut got_new_size = false;
        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("size") && msg_str.contains("1024") {
                        got_new_size = true;
                        break;
                    }
                }
                _ => break,
            }
        }

        println!("RDP resize: got_new_size={}", got_new_size);

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_rdp_different_color_depths() {
        if skip_if_not_available().await {
            return;
        }

        for color_depth in &["8", "16", "24", "32"] {
            let handler = RdpHandler::with_defaults();
            let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
            let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

            let mut params = HashMap::new();
            params.insert("hostname".to_string(), HOST.to_string());
            params.insert("port".to_string(), PORT.to_string());
            params.insert("username".to_string(), USERNAME.to_string());
            params.insert("password".to_string(), PASSWORD.to_string());
            params.insert("security".to_string(), "rdp".to_string());
            params.insert("ignore-cert".to_string(), "true".to_string());
            params.insert("color-depth".to_string(), color_depth.to_string());

            let handle =
                tokio::spawn(
                    async move { handler.connect(params, to_client_tx, from_client_rx).await },
                );

            // Wait for connection
            let _ = timeout(CONNECT_TIMEOUT, to_client_rx.recv()).await;

            println!("RDP: Tested with color depth {}", color_depth);

            drop(from_client_tx);
            let _ = timeout(Duration::from_secs(5), handle).await;
        }
    }

    #[tokio::test]
    #[ignore]
    async fn test_rdp_keyboard_input() {
        if skip_if_not_available().await {
            return;
        }

        let handler = RdpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("security".to_string(), "rdp".to_string());
        params.insert("ignore-cert".to_string(), "true".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for connection
        for _ in 0..10 {
            if timeout(Duration::from_secs(1), to_client_rx.recv())
                .await
                .is_err()
            {
                break;
            }
        }

        // Send key event (letter 'A')
        let key_instr = "3.key,2.65,1.1;";
        from_client_tx
            .send(Bytes::from(key_instr))
            .await
            .expect("Send failed");

        tokio::time::sleep(Duration::from_millis(200)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_rdp_mouse_input() {
        if skip_if_not_available().await {
            return;
        }

        let handler = RdpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());
        params.insert("security".to_string(), "rdp".to_string());
        params.insert("ignore-cert".to_string(), "true".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for connection
        for _ in 0..10 {
            if timeout(Duration::from_secs(1), to_client_rx.recv())
                .await
                .is_err()
            {
                break;
            }
        }

        // Send mouse move
        let mouse_instr = "5.mouse,3.100,3.100;";
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

        tokio::time::sleep(Duration::from_millis(200)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }
}

mod unit_tests {
    use guacr_handlers::ProtocolHandler;
    use guacr_rdp::RdpHandler;

    #[test]
    fn test_rdp_handler_creation() {
        let handler = RdpHandler::with_defaults();
        assert_eq!(handler.name(), "rdp");
    }

    #[tokio::test]
    async fn test_rdp_handler_health_check() {
        let handler = RdpHandler::with_defaults();
        let health = handler.health_check().await;
        assert!(health.is_ok());
    }
}
