//! Integration tests for guacr-rdp
//!
//! These tests require:
//! - FreeRDP installed (not just stub bindings)
//! - An RDP server to connect to
//!
//! Run with: cargo test -p guacr-rdp --test integration_test -- --include-ignored
//!
//! Environment variables:
//! - TEST_RDP_HOST: RDP server hostname (required)
//! - TEST_RDP_PORT: RDP server port (default: 3389)
//! - TEST_RDP_USER: Username (required)
//! - TEST_RDP_PASSWORD: Password (required)

use guacr_handlers::ProtocolHandler;
use guacr_rdp::{RdpConfig, RdpHandler};
use std::collections::HashMap;

/// Get test connection parameters from environment
fn get_test_params() -> Option<HashMap<String, String>> {
    let host = std::env::var("TEST_RDP_HOST").ok()?;
    let user = std::env::var("TEST_RDP_USER").ok()?;
    let password = std::env::var("TEST_RDP_PASSWORD").ok()?;

    let mut params = HashMap::new();
    params.insert("hostname".to_string(), host);
    params.insert("username".to_string(), user);
    params.insert("password".to_string(), password);

    if let Ok(port) = std::env::var("TEST_RDP_PORT") {
        params.insert("port".to_string(), port);
    }

    // Use smaller resolution for tests
    params.insert("width".to_string(), "800".to_string());
    params.insert("height".to_string(), "600".to_string());

    // Security settings for test servers (xrdp in Docker)
    // Use "any" to allow negotiation - xrdp may not have proper TLS certs
    params.insert("security".to_string(), "any".to_string());
    params.insert("ignore-cert".to_string(), "true".to_string());

    Some(params)
}

#[test]
fn test_rdp_handler_name() {
    let handler = RdpHandler::with_defaults();
    assert_eq!(handler.name(), "rdp");
}

#[tokio::test]
async fn test_rdp_handler_health() {
    let handler = RdpHandler::with_defaults();
    let health = handler.health_check().await;
    assert!(health.is_ok());
}

/// Test RDP connection to a real server
///
/// Ignored by default - requires TEST_RDP_* environment variables
#[tokio::test]
#[ignore]
async fn test_rdp_connection_basic() {
    let params = match get_test_params() {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: TEST_RDP_HOST, TEST_RDP_USER, TEST_RDP_PASSWORD not set");
            return;
        }
    };

    let handler = RdpHandler::with_defaults();

    let (to_client_tx, mut to_client_rx) = tokio::sync::mpsc::channel(100);
    let (from_client_tx, from_client_rx) = tokio::sync::mpsc::channel(100);

    // Spawn connection in background
    let connect_handle =
        tokio::spawn(async move { handler.connect(params, to_client_tx, from_client_rx).await });

    // Wait for ready instruction
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Some(msg) = to_client_rx.recv().await {
            let msg_str = String::from_utf8_lossy(&msg);
            if msg_str.contains("ready") {
                return true;
            }
        }
        false
    })
    .await;

    // Send disconnect
    let _ = from_client_tx
        .send(bytes::Bytes::from("10.disconnect;"))
        .await;

    // Wait for connection to finish
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), connect_handle).await;

    assert!(
        timeout.unwrap_or(false),
        "Did not receive ready instruction"
    );
}

/// Test RDP connection with invalid credentials
///
/// Ignored by default - requires FreeRDP installed
#[tokio::test]
#[ignore]
async fn test_rdp_connection_auth_failure() {
    let mut params = HashMap::new();
    params.insert("hostname".to_string(), "localhost".to_string());
    params.insert("username".to_string(), "invalid_user".to_string());
    params.insert("password".to_string(), "invalid_pass".to_string());
    params.insert("port".to_string(), "3389".to_string());

    let handler = RdpHandler::with_defaults();

    let (to_client_tx, _to_client_rx) = tokio::sync::mpsc::channel(100);
    let (_from_client_tx, from_client_rx) = tokio::sync::mpsc::channel(100);

    // This should fail with auth error (or connection refused if no server)
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handler.connect(params, to_client_tx, from_client_rx),
    )
    .await;

    // Either timeout or error is expected
    match result {
        Ok(Ok(())) => panic!("Expected connection to fail"),
        Ok(Err(_)) => (), // Expected - auth failure or connection refused
        Err(_) => (),     // Timeout - also acceptable
    }
}

/// Test RDP security settings
#[test]
fn test_rdp_security_config() {
    let config = RdpConfig {
        security_mode: "tls".to_string(),
        ..Default::default()
    };

    let handler = RdpHandler::new(config);
    assert_eq!(handler.name(), "rdp");
}
