//! Integration tests for PostgreSQL proxy
//!
//! These tests require a running PostgreSQL database. Start the test databases with:
//! ```bash
//! ./scripts/start-databases.sh
//! ```
//!
//! Run tests with:
//! ```bash
//! cargo test --test postgres_proxy_test -- --test-threads=1
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
static PORT_COUNTER: AtomicU16 = AtomicU16::new(15432);

fn get_unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Check if PostgreSQL is available on the given port
async fn postgres_available(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Check if psql CLI is available
fn psql_cli_available() -> bool {
    Command::new("psql")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the proxy server in background with a unique port, configured for PostgreSQL
async fn start_postgres_proxy(
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
  port: 5432
  database: "testdb"

credentials:
  username: "postgres"
  password: "postgres"

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

/// Run psql CLI command through the proxy
fn psql_query(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    query: &str,
) -> Result<String, String> {
    // psql uses PGPASSWORD environment variable
    let output = Command::new("psql")
        .env("PGPASSWORD", password)
        .args([
            "-h",
            host,
            "-p",
            &port.to_string(),
            "-U",
            user,
            "-d",
            "testdb",
            "-t", // tuples only (no headers)
            "-A", // unaligned output
            "-c",
            query,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run psql: {}", e))?;

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
async fn test_postgres_proxy_connection() {
    // Skip if PostgreSQL is not available
    if !postgres_available("127.0.0.1", 5432).await {
        skip_test!("PostgreSQL not available on port 5432");
    }

    // Skip if psql CLI is not available
    if !psql_cli_available() {
        skip_test!("psql CLI not installed (brew install postgresql)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_postgres_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test query through proxy
        let result = psql_query(
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
async fn test_postgres_proxy_any_credentials_accepted() {
    // Skip if PostgreSQL is not available
    if !postgres_available("127.0.0.1", 5432).await {
        skip_test!("PostgreSQL not available on port 5432");
    }

    // Skip if psql CLI is not available
    if !psql_cli_available() {
        skip_test!("psql CLI not installed (brew install postgresql)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_postgres_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test with random credentials - should still work (proxy injects real credentials)
        let result1 = psql_query(
            "127.0.0.1",
            proxy_port,
            "random_user",
            "wrong_pass",
            "SELECT 2",
        );
        let result2 = psql_query("127.0.0.1", proxy_port, "nobody", "nopass", "SELECT 3");
        let result3 = psql_query("127.0.0.1", proxy_port, "admin", "password123", "SELECT 4");

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(result1.unwrap(), "2");
        assert_eq!(result2.unwrap(), "3");
        assert_eq!(result3.unwrap(), "4");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_postgres_proxy_query_types() {
    // Skip if PostgreSQL is not available
    if !postgres_available("127.0.0.1", 5432).await {
        skip_test!("PostgreSQL not available on port 5432");
    }

    // Skip if psql CLI is not available
    if !psql_cli_available() {
        skip_test!("psql CLI not installed (brew install postgresql)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_postgres_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test various query types
        let queries = [
            ("SELECT version()", true),
            ("SELECT 1 + 1", true),
            ("SELECT 'hello world'", true),
            ("SELECT current_database()", true),
            ("SELECT now()", true),
        ];

        for (query, should_succeed) in queries {
            let result = psql_query("127.0.0.1", proxy_port, "test", "test", query);
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
async fn test_postgres_proxy_multiple_connections() {
    // Skip if PostgreSQL is not available
    if !postgres_available("127.0.0.1", 5432).await {
        skip_test!("PostgreSQL not available on port 5432");
    }

    // Skip if psql CLI is not available
    if !psql_cli_available() {
        skip_test!("psql CLI not installed (brew install postgresql)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_postgres_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Run multiple connections sequentially
        for i in 0..3 {
            let query = format!("SELECT {}", i);
            let result = psql_query(
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
async fn test_postgres_proxy_server_unreachable() {
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
  port: 59999

credentials:
  username: "postgres"
  password: "postgres"
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

        // Skip psql client check for this test - we just test that the proxy handles it
        if psql_cli_available() {
            // This should fail because target server doesn't exist
            let result = psql_query("127.0.0.1", proxy_port, "test", "test", "SELECT 1");
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
    use keeperdb_proxy::protocol::postgres::{auth::*, constants::*, messages::*};

    #[test]
    fn test_md5_password_generation() {
        let hash1 = compute_md5_password("password", "user", &[1, 2, 3, 4]);
        let hash2 = compute_md5_password("password", "user", &[1, 2, 3, 4]);

        // Should be deterministic
        assert_eq!(hash1, hash2);

        // Should start with "md5"
        assert!(hash1.starts_with("md5"));

        // Should be 35 chars (md5 + 32 hex chars)
        assert_eq!(hash1.len(), 35);
    }

    #[test]
    fn test_md5_password_different_salt() {
        let hash1 = compute_md5_password("password", "user", &[1, 2, 3, 4]);
        let hash2 = compute_md5_password("password", "user", &[5, 6, 7, 8]);

        // Different salt should produce different hash
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_scram_client_creation() {
        let mut client = ScramClient::new("user", "password");
        let first_msg = client.client_first_message();

        // Should start with "n,," (no channel binding)
        let first_msg_str = String::from_utf8(first_msg).unwrap();
        assert!(first_msg_str.starts_with("n,,n="));
    }

    #[test]
    fn test_scram_nonce_uniqueness() {
        let mut client1 = ScramClient::new("user", "password");
        let mut client2 = ScramClient::new("user", "password");

        // Different clients should have different nonces
        assert_ne!(
            client1.client_first_message(),
            client2.client_first_message()
        );
    }

    #[test]
    fn test_startup_message_creation() {
        let msg = StartupMessage::new("testuser");
        assert_eq!(msg.user(), Some("testuser"));
        assert_eq!(msg.database(), None);
    }

    #[test]
    fn test_startup_message_with_database() {
        let msg = StartupMessage::with_database("testuser", "mydb");
        assert_eq!(msg.user(), Some("testuser"));
        assert_eq!(msg.database(), Some("mydb"));
    }

    #[test]
    fn test_error_response_creation() {
        let err = ErrorNoticeResponse::error("ERROR", "28P01", "Authentication failed");
        assert_eq!(err.severity(), Some("ERROR"));
        assert_eq!(err.code(), Some("28P01"));
        assert_eq!(err.message(), Some("Authentication failed"));
    }

    #[test]
    fn test_error_response_fatal() {
        let err = ErrorNoticeResponse::fatal("08006", "Connection closed");
        assert_eq!(err.severity(), Some("FATAL"));
        assert_eq!(err.code(), Some("08006"));
    }

    #[test]
    fn test_authentication_message_checks() {
        let auth_ok = AuthenticationMessage::Ok;
        assert!(auth_ok.is_ok());
        assert!(!matches!(
            auth_ok,
            AuthenticationMessage::Md5Password { .. }
        ));
        assert!(!matches!(auth_ok, AuthenticationMessage::Sasl { .. }));

        let auth_md5 = AuthenticationMessage::Md5Password { salt: [1, 2, 3, 4] };
        assert!(!auth_md5.is_ok());
        assert!(matches!(
            auth_md5,
            AuthenticationMessage::Md5Password { .. }
        ));
        assert!(!matches!(auth_md5, AuthenticationMessage::Sasl { .. }));

        let auth_scram = AuthenticationMessage::Sasl {
            mechanisms: vec!["SCRAM-SHA-256".to_string()],
        };
        assert!(!auth_scram.is_ok());
        assert!(!matches!(
            auth_scram,
            AuthenticationMessage::Md5Password { .. }
        ));
        assert!(matches!(auth_scram, AuthenticationMessage::Sasl { .. }));
    }

    #[test]
    fn test_transaction_status_variants() {
        assert_eq!(TransactionStatus::Idle.to_byte(), b'I');
        assert_eq!(TransactionStatus::InTransaction.to_byte(), b'T');
        assert_eq!(TransactionStatus::Failed.to_byte(), b'E');
    }

    #[test]
    fn test_protocol_version() {
        // PostgreSQL protocol v3.0 = (3 << 16) | 0 = 196608
        assert_eq!(PROTOCOL_VERSION_3_0, 196608);
        assert_eq!(PROTOCOL_VERSION_3_0, 0x0003_0000);
    }

    #[test]
    fn test_ssl_request_code() {
        // SSL request code is 80877103
        assert_eq!(SSL_REQUEST_CODE, 80877103);
    }

    #[test]
    fn test_frontend_message_tags() {
        assert_eq!(MSG_QUERY, b'Q');
        assert_eq!(MSG_TERMINATE, b'X');
        assert_eq!(MSG_PASSWORD, b'p');
    }

    #[test]
    fn test_backend_message_tags() {
        assert_eq!(MSG_AUTH_REQUEST, b'R');
        assert_eq!(MSG_ERROR_RESPONSE, b'E');
        assert_eq!(MSG_READY_FOR_QUERY, b'Z');
        assert_eq!(MSG_PARAMETER_STATUS, b'S');
    }
}
