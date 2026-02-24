//! Integration tests for keeperdb-proxy with MariaDB
//!
//! These tests require a running MariaDB database. Start the test databases with:
//! ```bash
//! docker run -e MYSQL_ROOT_PASSWORD=rootpass -p 3307:3306 mariadb:10.11
//! ```
//!
//! Run tests with:
//! ```bash
//! cargo test --test mariadb_proxy_test -- --test-threads=1
//! ```

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

use keeperdb_proxy::config::load_config_from_str;
use keeperdb_proxy::server::Listener;
use keeperdb_proxy::StaticAuthProvider;

/// Default test timeout
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Port counter for unique ports per test (different range from MySQL tests)
static PORT_COUNTER: AtomicU16 = AtomicU16::new(13407);

fn get_unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Check if MariaDB is available on the given port
async fn mariadb_available(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Check if mysql CLI is available (works with MariaDB too)
fn mysql_cli_available() -> bool {
    Command::new("mysql")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the proxy server in background targeting MariaDB
async fn start_mariadb_proxy(
    proxy_port: u16,
) -> Result<(broadcast::Sender<()>, tokio::task::JoinHandle<()>), String> {
    let yaml = format!(
        r#"
server:
  listen_address: "127.0.0.1"
  listen_port: {}
  connect_timeout_secs: 30
  idle_timeout_secs: 300

target:
  host: "127.0.0.1"
  port: 3307
  database: "mysql"

credentials:
  username: "root"
  password: "rootpass"

logging:
  level: "info"
  protocol_debug: false
"#,
        proxy_port
    );

    let config = load_config_from_str(&yaml).map_err(|e| format!("Config error: {}", e))?;
    let config = Arc::new(config);

    // Create auth provider from config
    let auth_provider = Arc::new(
        StaticAuthProvider::from_config(&config)
            .map_err(|e| format!("Auth provider error: {}", e))?,
    );

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let listener = Listener::bind(Arc::clone(&config), auth_provider, shutdown_rx)
        .await
        .map_err(|e| format!("Bind error: {}", e))?;

    let handle = tokio::spawn(async move {
        let _ = listener.run().await;
    });

    // Give the listener time to start
    sleep(Duration::from_millis(100)).await;

    Ok((shutdown_tx, handle))
}

/// Run mysql CLI command through the proxy (works with MariaDB)
fn mariadb_query(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    query: &str,
) -> Result<String, String> {
    let output = Command::new("mysql")
        .args([
            "-h",
            host,
            "-P",
            &port.to_string(),
            "-u",
            user,
            &format!("-p{}", password),
            "-e",
            query,
            "--skip-column-names",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run mysql: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

/// Helper macro to skip tests with a visible message
macro_rules! skip_test {
    ($reason:expr) => {{
        eprintln!("\n  *** SKIPPED: {} ***\n", $reason);
        return;
    }};
}

/// Helper macro to run test with timeout
macro_rules! with_timeout {
    ($body:expr) => {
        match timeout(TEST_TIMEOUT, async { $body }).await {
            Ok(result) => result,
            Err(_) => panic!("Test timed out after {:?}", TEST_TIMEOUT),
        }
    };
}

// =============================================================================
// Integration Tests
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mariadb_proxy_connection() {
    // Skip if MariaDB is not available
    if !mariadb_available("127.0.0.1", 3307).await {
        skip_test!("MariaDB not available on port 3307");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_mariadb_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test query through proxy
        let result = mariadb_query(
            "127.0.0.1",
            proxy_port,
            "any_user",
            "any_password",
            "SELECT 1",
        );

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        match result {
            Ok(output) => assert_eq!(output, "1"),
            Err(e) => panic!("Query failed: {}", e),
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mariadb_proxy_any_credentials_accepted() {
    // Skip if MariaDB is not available
    if !mariadb_available("127.0.0.1", 3307).await {
        skip_test!("MariaDB not available on port 3307");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_mariadb_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test with random credentials - should still work
        let result1 = mariadb_query(
            "127.0.0.1",
            proxy_port,
            "random_user",
            "wrong_pass",
            "SELECT 2",
        );
        let result2 = mariadb_query("127.0.0.1", proxy_port, "nobody", "nopass", "SELECT 3");
        let result3 = mariadb_query("127.0.0.1", proxy_port, "admin", "password123", "SELECT 4");

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(result1.unwrap(), "2");
        assert_eq!(result2.unwrap(), "3");
        assert_eq!(result3.unwrap(), "4");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mariadb_proxy_query_types() {
    // Skip if MariaDB is not available
    if !mariadb_available("127.0.0.1", 3307).await {
        skip_test!("MariaDB not available on port 3307");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_mariadb_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test various query types
        let queries = [
            ("SELECT VERSION()", true),
            ("SELECT 1 + 1", true),
            ("SELECT 'hello world'", true),
            ("SHOW DATABASES", true),
            ("SELECT NOW()", true),
        ];

        for (query, should_succeed) in queries {
            let result = mariadb_query("127.0.0.1", proxy_port, "test", "test", query);
            if should_succeed {
                assert!(
                    result.is_ok(),
                    "Query '{}' should succeed: {:?}",
                    query,
                    result
                );
            }
        }

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mariadb_proxy_multiple_connections() {
    // Skip if MariaDB is not available
    if !mariadb_available("127.0.0.1", 3307).await {
        skip_test!("MariaDB not available on port 3307");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_mariadb_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Run multiple connections sequentially
        for i in 0..3 {
            let query = format!("SELECT {}", i);
            let result = mariadb_query(
                "127.0.0.1",
                proxy_port,
                &format!("user{}", i),
                "pass",
                &query,
            );
            assert_eq!(result.unwrap(), i.to_string());
        }

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mariadb_proxy_server_unreachable() {
    with_timeout!({
        // Start proxy pointing to non-existent server
        let proxy_port = get_unique_port();
        let yaml = format!(
            r#"
server:
  listen_address: "127.0.0.1"
  listen_port: {}
  connect_timeout_secs: 5
  idle_timeout_secs: 300

target:
  host: "127.0.0.1"
  port: 39998

credentials:
  username: "root"
  password: "root"
"#,
            proxy_port
        );

        let config = load_config_from_str(&yaml).unwrap();
        let config = Arc::new(config);

        // Create auth provider from config
        let auth_provider = Arc::new(StaticAuthProvider::from_config(&config).unwrap());

        let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);
        let listener = Listener::bind(Arc::clone(&config), auth_provider, shutdown_rx)
            .await
            .unwrap();

        let handle = tokio::spawn(async move {
            let _ = listener.run().await;
        });

        sleep(Duration::from_millis(100)).await;

        // Skip mysql client check for this test - we just test that the proxy handles it
        if mysql_cli_available() {
            // This should fail because target server doesn't exist
            let result = mariadb_query("127.0.0.1", proxy_port, "test", "test", "SELECT 1");
            assert!(
                result.is_err(),
                "Should fail when target server is unreachable"
            );
        }

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}

// =============================================================================
// Protocol Tests (ed25519 authentication)
// =============================================================================

mod protocol {
    use keeperdb_proxy::protocol::mysql::auth::*;

    #[test]
    fn test_ed25519_response_length() {
        // ed25519 uses a 32-byte nonce and returns 64-byte signature
        let nonce = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let response = compute_ed25519_response("password", &nonce);
        assert_eq!(response.len(), 64);
    }

    #[test]
    fn test_ed25519_deterministic() {
        let password = "test_password";
        let nonce = [0x42u8; 32];

        let r1 = compute_ed25519_response(password, &nonce);
        let r2 = compute_ed25519_response(password, &nonce);

        assert_eq!(r1, r2);
    }

    #[test]
    fn test_ed25519_empty_password() {
        let nonce = [0x42u8; 32];
        let response = compute_ed25519_response("", &nonce);
        assert!(response.is_empty());
    }

    #[test]
    fn test_ed25519_different_from_native() {
        let password = "testpass";
        let scramble = generate_scramble();

        let native = compute_auth_response(password, &scramble);
        let ed25519 = compute_ed25519_response(password, &scramble);

        // Different lengths (20 vs 64)
        assert_ne!(native.len(), ed25519.len());
        assert_eq!(native.len(), 20);
        assert_eq!(ed25519.len(), 64);
    }

    #[test]
    fn test_ed25519_dispatch() {
        let nonce = [0x42u8; 32];
        let direct = compute_ed25519_response("pass", &nonce);
        let dispatched = compute_auth_for_plugin("client_ed25519", "pass", &nonce);
        assert_eq!(direct, dispatched);
    }

    #[test]
    fn test_ed25519_different_passwords_different_signatures() {
        let nonce = [0x42u8; 32];
        let response1 = compute_ed25519_response("password1", &nonce);
        let response2 = compute_ed25519_response("password2", &nonce);
        assert_ne!(response1, response2);
    }

    #[test]
    fn test_ed25519_different_nonces_different_signatures() {
        let password = "testpass";
        let nonce1 = [0x01u8; 32];
        let nonce2 = [0x02u8; 32];

        let response1 = compute_ed25519_response(password, &nonce1);
        let response2 = compute_ed25519_response(password, &nonce2);
        assert_ne!(response1, response2);
    }

    #[test]
    fn test_scramble_generation_unique() {
        let s1 = generate_scramble();
        let s2 = generate_scramble();

        // Scrambles should be different
        assert_ne!(s1, s2);

        // Should be 20 bytes
        assert_eq!(s1.len(), 20);
        assert_eq!(s2.len(), 20);

        // Should not contain null bytes
        assert!(!s1.contains(&0));
        assert!(!s2.contains(&0));
    }

    #[test]
    fn test_auth_response_roundtrip() {
        let password = "test_password";
        let scramble = generate_scramble();

        let r1 = compute_auth_response(password, &scramble);
        let r2 = compute_auth_response(password, &scramble);

        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 20);
    }

    #[test]
    fn test_mariadb_version_detection_concept() {
        // MariaDB versions include "MariaDB" in version string
        // This tests the concept that MariaDB can be detected
        let mariadb_version = "10.11.6-MariaDB";
        assert!(mariadb_version.contains("MariaDB"));

        let mysql_version = "8.0.35";
        assert!(!mysql_version.contains("MariaDB"));
    }
}
