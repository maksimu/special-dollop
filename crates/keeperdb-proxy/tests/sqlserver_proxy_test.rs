//! Integration tests for SQL Server proxy
//!
//! These tests require a running SQL Server database. Start with Docker:
//! ```bash
//! docker run -e 'ACCEPT_EULA=Y' -e 'MSSQL_SA_PASSWORD=YourStrong!Passw0rd' \
//!   -p 1433:1433 mcr.microsoft.com/mssql/server:2022-latest
//! ```
//!
//! Run tests with:
//! ```bash
//! cargo test --test sqlserver_proxy_test -- --test-threads=1
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
static PORT_COUNTER: AtomicU16 = AtomicU16::new(11433);

fn get_unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Check if SQL Server is available on the given port
async fn sqlserver_available(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Check if sqlcmd CLI is available
fn sqlcmd_cli_available() -> bool {
    Command::new("sqlcmd")
        .arg("-?")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the proxy server in background with a unique port, configured for SQL Server
async fn start_sqlserver_proxy(
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
  port: 1433
  database: "master"

credentials:
  username: "sa"
  password: "YourStrong!Passw0rd"

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

/// Run sqlcmd CLI command through the proxy
fn sqlcmd_query(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    query: &str,
) -> Result<String, String> {
    // sqlcmd uses -S host,port format (comma, not colon)
    let server = format!("{},{}", host, port);
    let output = Command::new("sqlcmd")
        .args([
            "-S", &server, "-U", user, "-P", password, "-Q", query, "-h", "-1", // No headers
            "-W", // Remove trailing spaces
            "-C", // Trust server certificate
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run sqlcmd: {}", e))?;

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
async fn test_sqlserver_proxy_connection() {
    // Skip if SQL Server is not available
    if !sqlserver_available("127.0.0.1", 1433).await {
        skip_test!("SQL Server not available on port 1433");
    }

    // Skip if sqlcmd CLI is not available
    if !sqlcmd_cli_available() {
        skip_test!("sqlcmd CLI not installed (brew install mssql-tools18)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_sqlserver_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test query through proxy
        let result = sqlcmd_query(
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
async fn test_sqlserver_proxy_any_credentials_accepted() {
    // Skip if SQL Server is not available
    if !sqlserver_available("127.0.0.1", 1433).await {
        skip_test!("SQL Server not available on port 1433");
    }

    // Skip if sqlcmd CLI is not available
    if !sqlcmd_cli_available() {
        skip_test!("sqlcmd CLI not installed (brew install mssql-tools18)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_sqlserver_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test with random credentials - should still work (proxy injects real credentials)
        let result1 = sqlcmd_query(
            "127.0.0.1",
            proxy_port,
            "random_user",
            "wrong_pass",
            "SELECT 2",
        );
        let result2 = sqlcmd_query("127.0.0.1", proxy_port, "nobody", "nopass", "SELECT 3");
        let result3 = sqlcmd_query("127.0.0.1", proxy_port, "admin", "password123", "SELECT 4");

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(result1.unwrap(), "2");
        assert_eq!(result2.unwrap(), "3");
        assert_eq!(result3.unwrap(), "4");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_sqlserver_proxy_query_types() {
    // Skip if SQL Server is not available
    if !sqlserver_available("127.0.0.1", 1433).await {
        skip_test!("SQL Server not available on port 1433");
    }

    // Skip if sqlcmd CLI is not available
    if !sqlcmd_cli_available() {
        skip_test!("sqlcmd CLI not installed (brew install mssql-tools18)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_sqlserver_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test various query types
        let queries = [
            ("SELECT @@VERSION", true),
            ("SELECT 1 + 1", true),
            ("SELECT 'hello world'", true),
            ("SELECT DB_NAME()", true),
            ("SELECT GETDATE()", true),
        ];

        for (query, should_succeed) in queries {
            let result = sqlcmd_query("127.0.0.1", proxy_port, "test", "test", query);
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
async fn test_sqlserver_proxy_multiple_connections() {
    // Skip if SQL Server is not available
    if !sqlserver_available("127.0.0.1", 1433).await {
        skip_test!("SQL Server not available on port 1433");
    }

    // Skip if sqlcmd CLI is not available
    if !sqlcmd_cli_available() {
        skip_test!("sqlcmd CLI not installed (brew install mssql-tools18)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_sqlserver_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Run multiple connections sequentially
        for i in 0..3 {
            let query = format!("SELECT {}", i);
            let result = sqlcmd_query(
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
async fn test_sqlserver_proxy_server_unreachable() {
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
  username: "sa"
  password: "TestPassword123!"
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

        // Skip sqlcmd client check for this test - we just test that the proxy handles it
        if sqlcmd_cli_available() {
            // This should fail because target server doesn't exist
            let result = sqlcmd_query("127.0.0.1", proxy_port, "test", "test", "SELECT 1");
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
    use keeperdb_proxy::protocol::sqlserver::{
        auth::{encode_password, string_to_utf16le, utf16le_to_string},
        constants::{
            packet_type, prelogin_token, status, token_type, version, EncryptionMode,
            LOGIN7_HEADER_SIZE, TDS_HEADER_SIZE,
        },
    };

    #[test]
    fn test_password_encoding_deterministic() {
        let password = "TestPassword123!";
        let encoded1 = encode_password(password);
        let encoded2 = encode_password(password);

        // Encoding should be deterministic
        assert_eq!(encoded1, encoded2);

        // Should produce non-empty output
        assert!(!encoded1.is_empty());

        // Each UTF-16 code unit produces 2 bytes
        assert_eq!(encoded1.len(), password.len() * 2);
    }

    #[test]
    fn test_utf16_roundtrip() {
        let original = "Hello, SQL Server!";
        let encoded = string_to_utf16le(original);
        let decoded = utf16le_to_string(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_utf16_unicode_roundtrip() {
        let original = "Café ☕ データベース";
        let encoded = string_to_utf16le(original);
        let decoded = utf16le_to_string(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_tds_version_constant() {
        // TDS 7.4 should be 0x74000004
        assert_eq!(version::TDS_7_4, 0x7400_0004);

        // Versions should be in ascending order
        const _: () = assert!(version::TDS_7_0 < version::TDS_7_1);
        const _: () = assert!(version::TDS_7_1 < version::TDS_7_2);
        const _: () = assert!(version::TDS_7_2 < version::TDS_7_3);
        const _: () = assert!(version::TDS_7_3 < version::TDS_7_4);
    }

    #[test]
    fn test_encryption_modes() {
        // Test all encryption mode conversions
        assert_eq!(EncryptionMode::from_byte(0x00), Some(EncryptionMode::Off));
        assert_eq!(EncryptionMode::from_byte(0x01), Some(EncryptionMode::On));
        assert_eq!(
            EncryptionMode::from_byte(0x02),
            Some(EncryptionMode::NotSupported)
        );
        assert_eq!(
            EncryptionMode::from_byte(0x03),
            Some(EncryptionMode::Required)
        );
        assert_eq!(EncryptionMode::from_byte(0x04), None);

        // Test to_byte roundtrip
        assert_eq!(EncryptionMode::Off.to_byte(), 0x00);
        assert_eq!(EncryptionMode::On.to_byte(), 0x01);
        assert_eq!(EncryptionMode::NotSupported.to_byte(), 0x02);
        assert_eq!(EncryptionMode::Required.to_byte(), 0x03);
    }

    #[test]
    fn test_packet_type_constants() {
        // Verify key packet types
        assert_eq!(packet_type::SQL_BATCH, 0x01);
        assert_eq!(packet_type::TABULAR_RESULT, 0x04);
        assert_eq!(packet_type::LOGIN7, 0x10);
        assert_eq!(packet_type::PRELOGIN, 0x12);

        // Ensure no collision between common packet types
        let types = [
            packet_type::SQL_BATCH,
            packet_type::TABULAR_RESULT,
            packet_type::LOGIN7,
            packet_type::PRELOGIN,
        ];
        for i in 0..types.len() {
            for j in (i + 1)..types.len() {
                assert_ne!(types[i], types[j]);
            }
        }
    }

    #[test]
    fn test_token_type_constants() {
        // Verify key token types used in authentication and results
        assert_eq!(token_type::LOGINACK, 0xAD);
        assert_eq!(token_type::ERROR, 0xAA);
        assert_eq!(token_type::INFO, 0xAB);
        assert_eq!(token_type::ENVCHANGE, 0xE3);
        assert_eq!(token_type::DONE, 0xFD);
        assert_eq!(token_type::ROW, 0xD1);
        assert_eq!(token_type::COLMETADATA, 0x81);
    }

    #[test]
    fn test_prelogin_token_constants() {
        assert_eq!(prelogin_token::VERSION, 0x00);
        assert_eq!(prelogin_token::ENCRYPTION, 0x01);
        assert_eq!(prelogin_token::TERMINATOR, 0xFF);
    }

    #[test]
    fn test_status_flags() {
        assert_eq!(status::NORMAL, 0x00);
        assert_eq!(status::EOM, 0x01);
    }

    #[test]
    fn test_header_size_constants() {
        assert_eq!(TDS_HEADER_SIZE, 8);
        assert_eq!(LOGIN7_HEADER_SIZE, 94);
    }

    #[test]
    fn test_password_encoding_special_chars() {
        // Test with special characters commonly used in passwords
        let password = "P@$$w0rd!#%^&*()";
        let encoded = encode_password(password);

        // Should encode successfully
        assert!(!encoded.is_empty());

        // Encoding should be deterministic
        let encoded2 = encode_password(password);
        assert_eq!(encoded, encoded2);
    }
}
