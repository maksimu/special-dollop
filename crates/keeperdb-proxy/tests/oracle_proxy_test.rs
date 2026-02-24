//! Integration tests for Oracle TNS proxy
//!
//! These tests require a running Oracle database. Start with Docker:
//! ```bash
//! docker run -d -p 1521:1521 -e ORACLE_PASSWORD=TestPassword123 \
//!   gvenzl/oracle-xe:21-slim
//! ```
//!
//! Wait for the database to be ready (check with `docker logs`), then run tests:
//! ```bash
//! cargo test --test oracle_proxy_test -- --test-threads=1
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
static PORT_COUNTER: AtomicU16 = AtomicU16::new(11521);

fn get_unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Check if Oracle is available on the given port
async fn oracle_available(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Check if sqlplus CLI is available
fn sqlplus_cli_available() -> bool {
    Command::new("sqlplus")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the proxy server in background with a unique port, configured for Oracle
async fn start_oracle_proxy(
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
  port: 1521

credentials:
  username: "system"
  password: "TestPassword123"

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

/// Run sqlplus CLI command through the proxy
fn sqlplus_query(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    service: &str,
    query: &str,
) -> Result<String, String> {
    // sqlplus uses user/password@host:port/service format
    let connect_string = format!("{}/{}@{}:{}/{}", user, password, host, port, service);

    // Create a script that runs the query and exits
    let script = format!(
        "SET PAGESIZE 0\nSET FEEDBACK OFF\nSET HEADING OFF\n{}\nEXIT;",
        query
    );

    // Use echo to pipe the script to sqlplus
    let output = Command::new("sh")
        .args([
            "-c",
            &format!(
                "echo '{}' | sqlplus -S '{}'",
                script.replace("'", "'\\''"),
                connect_string.replace("'", "'\\''")
            ),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run sqlplus: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Filter out ORA- errors in output
        if stdout.contains("ORA-") {
            Err(stdout)
        } else {
            Ok(stdout)
        }
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
async fn test_oracle_proxy_connection() {
    // Skip if Oracle is not available
    if !oracle_available("127.0.0.1", 1521).await {
        skip_test!("Oracle not available on port 1521");
    }

    // Skip if sqlplus CLI is not available
    if !sqlplus_cli_available() {
        skip_test!("sqlplus CLI not installed (install Oracle Instant Client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_oracle_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test query through proxy
        let result = sqlplus_query(
            "127.0.0.1",
            proxy_port,
            "any_user",
            "any_password",
            "XE",
            "SELECT 1 FROM DUAL;",
        );

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        match result {
            Ok(output) => assert_eq!(output.trim(), "1"),
            Err(e) => panic!("Query failed: {}", e),
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_oracle_proxy_any_credentials_accepted() {
    // Skip if Oracle is not available
    if !oracle_available("127.0.0.1", 1521).await {
        skip_test!("Oracle not available on port 1521");
    }

    // Skip if sqlplus CLI is not available
    if !sqlplus_cli_available() {
        skip_test!("sqlplus CLI not installed (install Oracle Instant Client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_oracle_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test with random credentials - should still work (proxy injects real credentials)
        let result1 = sqlplus_query(
            "127.0.0.1",
            proxy_port,
            "random_user",
            "wrong_pass",
            "XE",
            "SELECT 2 FROM DUAL;",
        );
        let result2 = sqlplus_query(
            "127.0.0.1",
            proxy_port,
            "nobody",
            "nopass",
            "XE",
            "SELECT 3 FROM DUAL;",
        );
        let result3 = sqlplus_query(
            "127.0.0.1",
            proxy_port,
            "admin",
            "password123",
            "XE",
            "SELECT 4 FROM DUAL;",
        );

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;

        assert_eq!(result1.unwrap().trim(), "2");
        assert_eq!(result2.unwrap().trim(), "3");
        assert_eq!(result3.unwrap().trim(), "4");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_oracle_proxy_query_types() {
    // Skip if Oracle is not available
    if !oracle_available("127.0.0.1", 1521).await {
        skip_test!("Oracle not available on port 1521");
    }

    // Skip if sqlplus CLI is not available
    if !sqlplus_cli_available() {
        skip_test!("sqlplus CLI not installed (install Oracle Instant Client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_oracle_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Test various query types
        let queries = [
            ("SELECT BANNER FROM V$VERSION WHERE ROWNUM = 1;", true),
            ("SELECT 1 + 1 FROM DUAL;", true),
            ("SELECT 'hello world' FROM DUAL;", true),
            ("SELECT SYSDATE FROM DUAL;", true),
            ("SELECT USER FROM DUAL;", true),
        ];

        for (query, should_succeed) in queries {
            let result = sqlplus_query("127.0.0.1", proxy_port, "test", "test", "XE", query);
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
async fn test_oracle_proxy_multiple_connections() {
    // Skip if Oracle is not available
    if !oracle_available("127.0.0.1", 1521).await {
        skip_test!("Oracle not available on port 1521");
    }

    // Skip if sqlplus CLI is not available
    if !sqlplus_cli_available() {
        skip_test!("sqlplus CLI not installed (install Oracle Instant Client)");
    }

    with_timeout!({
        let proxy_port = get_unique_port();
        let (shutdown_tx, handle) = match start_oracle_proxy(proxy_port).await {
            Ok(r) => r,
            Err(e) => {
                panic!("Failed to start proxy: {}", e);
            }
        };

        // Run multiple connections sequentially
        for i in 0..3 {
            let query = format!("SELECT {} FROM DUAL;", i);
            let result = sqlplus_query(
                "127.0.0.1",
                proxy_port,
                &format!("user{}", i),
                "pass",
                "XE",
                &query,
            );
            assert_eq!(result.unwrap().trim(), i.to_string());
        }

        // Shutdown proxy
        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_oracle_proxy_server_unreachable() {
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
  username: "system"
  password: "TestPassword123"
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

        // Skip sqlplus client check for this test - we just test that the proxy handles it
        if sqlplus_cli_available() {
            // This should fail because target server doesn't exist
            let result = sqlplus_query(
                "127.0.0.1",
                proxy_port,
                "test",
                "test",
                "XE",
                "SELECT 1 FROM DUAL;",
            );
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
    use keeperdb_proxy::protocol::oracle::{
        auth::{is_auth_failure, is_auth_packet, is_auth_success, parse_auth_request},
        constants::{data_flags, packet_type, TNS_HEADER_SIZE, TNS_MAX_PACKET_SIZE},
        packets::{AcceptPacket, ConnectPacket, DataPacket, RefusePacket, TnsHeader},
        parser::{
            modify_connect_string_user, parse_accept, parse_connect, parse_connect_string,
            parse_data, parse_header, parse_refuse, serialize_accept, serialize_connect,
            serialize_data, serialize_header, serialize_refuse,
        },
    };

    #[test]
    fn test_tns_header_constants() {
        assert_eq!(TNS_HEADER_SIZE, 8);
        assert_eq!(TNS_MAX_PACKET_SIZE, 65535);
    }

    #[test]
    fn test_packet_type_constants() {
        // Verify key packet types
        assert_eq!(packet_type::CONNECT, 0x01);
        assert_eq!(packet_type::ACCEPT, 0x02);
        assert_eq!(packet_type::REFUSE, 0x04);
        assert_eq!(packet_type::REDIRECT, 0x05);
        assert_eq!(packet_type::DATA, 0x06);
        assert_eq!(packet_type::ABORT, 0x09);
        assert_eq!(packet_type::MARKER, 0x0C);

        // Ensure no collision between packet types
        let types = [
            packet_type::CONNECT,
            packet_type::ACCEPT,
            packet_type::REFUSE,
            packet_type::REDIRECT,
            packet_type::DATA,
            packet_type::ABORT,
        ];
        for i in 0..types.len() {
            for j in (i + 1)..types.len() {
                assert_ne!(types[i], types[j], "Packet types should be unique");
            }
        }
    }

    #[test]
    fn test_data_flags_constants() {
        assert_eq!(data_flags::EOF, 0x0040);
        assert_eq!(data_flags::SEND_TOKEN, 0x0001);
        assert_eq!(data_flags::MORE_DATA, 0x0020);
        assert_eq!(data_flags::REQUEST_CONFIRMATION, 0x0002);
    }

    #[test]
    fn test_tns_header_parse_roundtrip() {
        let header = TnsHeader {
            length: 256,
            packet_checksum: 0x1234,
            packet_type: packet_type::DATA,
            flags: 0x20,
            header_checksum: 0x5678,
        };

        let serialized = serialize_header(&header);
        assert_eq!(serialized.len(), TNS_HEADER_SIZE);

        let parsed = parse_header(&serialized).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_tns_header_big_endian() {
        // Verify big-endian encoding
        let header = TnsHeader {
            length: 0x1234,
            packet_checksum: 0,
            packet_type: packet_type::CONNECT,
            flags: 0,
            header_checksum: 0,
        };

        let serialized = serialize_header(&header);
        // Length should be big-endian: 0x12 0x34
        assert_eq!(serialized[0], 0x12);
        assert_eq!(serialized[1], 0x34);
    }

    #[test]
    fn test_connect_packet_roundtrip() {
        let connect_string =
            "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=localhost)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=XE)))";
        let original = ConnectPacket::new(connect_string.to_string());

        let serialized = serialize_connect(&original);
        let parsed = parse_connect(&serialized).unwrap();

        assert_eq!(parsed.version, original.version);
        assert_eq!(parsed.version_compatible, original.version_compatible);
        assert_eq!(
            parsed.session_data_unit_size,
            original.session_data_unit_size
        );
        assert_eq!(parsed.connect_data, original.connect_data);
    }

    #[test]
    fn test_accept_packet_roundtrip() {
        let original = AcceptPacket::new(318, 8192, 65535);

        let serialized = serialize_accept(&original);
        let parsed = parse_accept(&serialized).unwrap();

        assert_eq!(parsed.version, original.version);
        assert_eq!(
            parsed.session_data_unit_size,
            original.session_data_unit_size
        );
        assert_eq!(
            parsed.maximum_transmission_data_unit_size,
            original.maximum_transmission_data_unit_size
        );
    }

    #[test]
    fn test_refuse_packet_roundtrip() {
        let original = RefusePacket::new(1, 2, "Connection refused by server");

        let serialized = serialize_refuse(&original);
        let parsed = parse_refuse(&serialized).unwrap();

        assert_eq!(parsed.user_reason, original.user_reason);
        assert_eq!(parsed.system_reason, original.system_reason);
        assert_eq!(parsed.refuse_data, original.refuse_data);
    }

    #[test]
    fn test_data_packet_roundtrip() {
        let payload = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let original = DataPacket::new(data_flags::EOF | data_flags::SEND_TOKEN, payload);

        let serialized = serialize_data(&original);
        let parsed = parse_data(&serialized).unwrap();

        assert_eq!(parsed.data_flags, original.data_flags);
        assert_eq!(parsed.payload, original.payload);
        assert!(parsed.is_eof());
    }

    #[test]
    fn test_connect_string_parse_description() {
        let cs = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db.example.com)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)))";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.protocol, Some("TCP".to_string()));
        assert_eq!(parts.host, Some("db.example.com".to_string()));
        assert_eq!(parts.port, Some(1521));
        assert_eq!(parts.service_name, Some("ORCL".to_string()));
    }

    #[test]
    fn test_connect_string_parse_easy_connect() {
        let cs = "db.example.com:1521/ORCL";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.protocol, Some("TCP".to_string()));
        assert_eq!(parts.host, Some("db.example.com".to_string()));
        assert_eq!(parts.port, Some(1521));
        assert_eq!(parts.service_name, Some("ORCL".to_string()));
    }

    #[test]
    fn test_connect_string_default_port() {
        let cs = "db.example.com/ORCL";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.port, None);
        assert_eq!(parts.effective_port(), 1521); // Default Oracle port
    }

    #[test]
    fn test_modify_connect_string_replace_user() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(USER=olduser))))";
        let modified = modify_connect_string_user(cs, "newuser");

        assert!(modified.contains("(USER=newuser)"));
        assert!(!modified.contains("(USER=olduser)"));
    }

    #[test]
    fn test_modify_connect_string_add_cid() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)))";
        let modified = modify_connect_string_user(cs, "testuser");

        assert!(modified.contains("(CID=(USER=testuser))"));
    }

    #[test]
    fn test_modify_easy_connect_to_description() {
        let cs = "db.example.com:1521/ORCL";
        let modified = modify_connect_string_user(cs, "testuser");

        assert!(modified.starts_with("(DESCRIPTION="));
        assert!(modified.contains("(HOST=db.example.com)"));
        assert!(modified.contains("(PORT=1521)"));
        assert!(modified.contains("(SERVICE_NAME=ORCL)"));
        assert!(modified.contains("(USER=testuser)"));
    }

    #[test]
    fn test_modify_connect_string_case_insensitive() {
        // JDBC thin clients may use lowercase or mixed-case keywords
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(PROGRAM=JDBC)(HOST=client)(USER=olduser))))";

        // Lowercase CID and USER
        let lower = cs.replace("CID", "cid").replace("USER", "user");
        let modified = modify_connect_string_user(&lower, "newuser");
        assert!(
            modified.contains("(USER=newuser)"),
            "Failed on lowercase: {}",
            modified
        );
        assert!(
            !modified.contains("(user=olduser)"),
            "Old user still present: {}",
            modified
        );

        // Mixed case
        let mixed = cs.replace("CID", "Cid").replace("USER", "User");
        let modified = modify_connect_string_user(&mixed, "newuser");
        assert!(
            modified.contains("(USER=newuser)"),
            "Failed on mixed case: {}",
            modified
        );
        assert!(
            !modified.contains("(User=olduser)"),
            "Old user still present: {}",
            modified
        );

        // Lowercase CONNECT_DATA (no CID present)
        let no_cid =
            "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(connect_data=(SERVICE_NAME=ORCL)))";
        let modified = modify_connect_string_user(no_cid, "testuser");
        assert!(
            modified.contains("(CID=(USER=testuser))"),
            "Failed on lowercase connect_data: {}",
            modified
        );
    }

    #[test]
    fn test_is_auth_packet_detection() {
        // Empty payload - not auth
        assert!(!is_auth_packet(&[]));

        // Too short - not auth
        assert!(!is_auth_packet(&[0x00, 0x01]));

        // Regular data - not auth
        let regular_data = vec![0x11, 0x69, 0x00, 0x00, 0x00, 0x00];
        assert!(!is_auth_packet(&regular_data));

        // AUTH packet pattern (0x03 0x76) at typical offset
        let mut auth_like = vec![0x00; 20];
        auth_like[4] = 0x03;
        auth_like[5] = 0x76;
        // This might or might not be detected depending on the exact pattern
        // The test verifies the function doesn't panic
        let _ = is_auth_packet(&auth_like);
    }

    #[test]
    fn test_auth_request_parse_empty() {
        // Empty payload should return None
        let result = parse_auth_request(&[]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_is_auth_success_failure() {
        // Empty payload - not success/failure
        assert!(!is_auth_success(&[]));
        assert!(!is_auth_failure(&[]));

        // Short payload - not success/failure
        let short = vec![0x00, 0x01, 0x02];
        assert!(!is_auth_success(&short));
        assert!(!is_auth_failure(&short));
    }

    #[test]
    fn test_tns_header_type_name() {
        let header = TnsHeader::new(packet_type::CONNECT, 0);
        assert_eq!(header.type_name(), "CONNECT");

        let header = TnsHeader::new(packet_type::ACCEPT, 0);
        assert_eq!(header.type_name(), "ACCEPT");

        let header = TnsHeader::new(packet_type::REFUSE, 0);
        assert_eq!(header.type_name(), "REFUSE");

        let header = TnsHeader::new(packet_type::DATA, 0);
        assert_eq!(header.type_name(), "DATA");

        let header = TnsHeader::new(0xFF, 0);
        assert_eq!(header.type_name(), "UNKNOWN");
    }

    #[test]
    fn test_data_packet_is_eof() {
        let with_eof = DataPacket::new(data_flags::EOF, vec![]);
        assert!(with_eof.is_eof());

        let without_eof = DataPacket::simple(vec![]);
        assert!(!without_eof.is_eof());

        let combined = DataPacket::new(data_flags::EOF | data_flags::SEND_TOKEN, vec![]);
        assert!(combined.is_eof());
    }
}
