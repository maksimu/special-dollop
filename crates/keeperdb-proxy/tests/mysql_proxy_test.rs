//! Integration tests for keeperdb-proxy
//!
//! These tests require a running MySQL database. Start the test databases with:
//! ```bash
//! ./scripts/start-databases.sh
//! ```
//!
//! Run tests with:
//! ```bash
//! cargo test --test integration_test -- --test-threads=1
//! ```

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

/// Default test timeout
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

use keeperdb_proxy::config::load_config_from_str;
use keeperdb_proxy::server::Listener;
use keeperdb_proxy::StaticAuthProvider;

/// Port counter for unique ports per test
static PORT_COUNTER: AtomicU16 = AtomicU16::new(13307);

fn get_unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Check if MySQL is available on the given port
async fn mysql_available(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Check if mysql CLI is available
fn mysql_cli_available() -> bool {
    Command::new("mysql")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the proxy server in background with a unique port
async fn start_proxy(
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
  port: 3306
  database: "testdb"

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

/// Run mysql CLI command through the proxy
fn mysql_query(
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_proxy_connection_mysql8() {
    // Skip if MySQL 8.0 is not available
    if !mysql_available("127.0.0.1", 3306).await {
        skip_test!("MySQL 8.0 not available on port 3306");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test query through proxy
        let result = mysql_query(
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
async fn test_proxy_any_credentials_accepted() {
    // Skip if MySQL 8.0 is not available
    if !mysql_available("127.0.0.1", 3306).await {
        skip_test!("MySQL 8.0 not available on port 3306");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test with random credentials - should still work
        let result1 = mysql_query(
            "127.0.0.1",
            proxy_port,
            "random_user",
            "wrong_pass",
            "SELECT 2",
        );
        let result2 = mysql_query("127.0.0.1", proxy_port, "nobody", "nopass", "SELECT 3");
        let result3 = mysql_query("127.0.0.1", proxy_port, "admin", "password123", "SELECT 4");

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(result1.unwrap(), "2");
        assert_eq!(result2.unwrap(), "3");
        assert_eq!(result3.unwrap(), "4");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_proxy_query_types() {
    // Skip if MySQL 8.0 is not available
    if !mysql_available("127.0.0.1", 3306).await {
        skip_test!("MySQL 8.0 not available on port 3306");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_proxy(proxy_port).await {
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
            let result = mysql_query("127.0.0.1", proxy_port, "test", "test", query);
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
async fn test_proxy_multiple_connections() {
    // Skip if MySQL 8.0 is not available
    if !mysql_available("127.0.0.1", 3306).await {
        skip_test!("MySQL 8.0 not available on port 3306");
    }

    // Skip if mysql CLI is not available
    if !mysql_cli_available() {
        skip_test!("mysql CLI not installed (brew install mysql-client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Run multiple connections sequentially (parallel would need multiple proxies)
        for i in 0..3 {
            let query = format!("SELECT {}", i);
            let result = mysql_query(
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
async fn test_proxy_server_unreachable() {
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
  port: 39999

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
            let result = mysql_query("127.0.0.1", proxy_port, "test", "test", "SELECT 1");
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

/// Module for protocol-level tests (no external dependencies)
mod protocol {
    use keeperdb_proxy::protocol::mysql::{auth::*, packets::*};

    #[test]
    fn test_scramble_generation() {
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
    fn test_auth_response_deterministic() {
        let password = "test_password";
        let scramble = generate_scramble();

        let r1 = compute_auth_response(password, &scramble);
        let r2 = compute_auth_response(password, &scramble);

        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 20);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_capability_flags() {
        assert!(DEFAULT_SERVER_CAPABILITIES & CLIENT_PROTOCOL_41 != 0);
        assert!(DEFAULT_SERVER_CAPABILITIES & CLIENT_SECURE_CONNECTION != 0);
        assert!(DEFAULT_SERVER_CAPABILITIES & CLIENT_PLUGIN_AUTH != 0);
    }

    #[test]
    fn test_err_packet_helpers() {
        let err = ErrPacket::access_denied("testuser", "localhost");
        assert_eq!(err.error_code, 1045);
        assert!(err.error_message.contains("testuser"));
        assert!(err.error_message.contains("localhost"));

        let err = ErrPacket::connection_refused("db.example.com", 3306);
        assert_eq!(err.error_code, 2003);
        assert!(err.error_message.contains("db.example.com"));
    }
}
