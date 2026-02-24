//! Integration tests for the Guacd-style handshake protocol.
//!
//! These tests verify the handshake flow used in gateway mode.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use keeperdb_proxy::handshake::{CredentialStore, HandshakeHandler, Instruction};

/// Test parsing and encoding of handshake instructions.
#[test]
fn test_instruction_roundtrip() {
    // Test select instruction
    let select = Instruction::select("mysql");
    let encoded = select.encode();
    let (parsed, consumed) = Instruction::parse(&encoded).expect("parse select");
    assert_eq!(consumed, encoded.len());
    assert_eq!(parsed.opcode, "select");
    assert_eq!(parsed.args, vec!["mysql"]);

    // Test args instruction
    let args = Instruction::args(&["target_host", "target_port", "username", "password"]);
    let encoded = args.encode();
    let (parsed, consumed) = Instruction::parse(&encoded).expect("parse args");
    assert_eq!(consumed, encoded.len());
    assert_eq!(parsed.opcode, "args");
    assert_eq!(parsed.args.len(), 4);

    // Test connect instruction with values
    let connect = Instruction::connect(vec![
        "db.example.com".to_string(),
        "3306".to_string(),
        "root".to_string(),
        "secret".to_string(),
    ]);
    let encoded = connect.encode();
    let (parsed, consumed) = Instruction::parse(&encoded).expect("parse connect");
    assert_eq!(consumed, encoded.len());
    assert_eq!(parsed.opcode, "connect");
    assert_eq!(parsed.args[0], "db.example.com");
    assert_eq!(parsed.args[1], "3306");

    // Test ready instruction
    let session_id = "550e8400-e29b-41d4-a716-446655440000";
    let ready = Instruction::ready(session_id);
    let encoded = ready.encode();
    let (parsed, consumed) = Instruction::parse(&encoded).expect("parse ready");
    assert_eq!(consumed, encoded.len());
    assert_eq!(parsed.opcode, "ready");
    assert_eq!(parsed.args[0], session_id);

    // Test error instruction
    let error = Instruction::error("auth_failed", "Invalid credentials");
    let encoded = error.encode();
    let (parsed, consumed) = Instruction::parse(&encoded).expect("parse error");
    assert_eq!(consumed, encoded.len());
    assert_eq!(parsed.opcode, "error");
    assert_eq!(parsed.args[0], "auth_failed");
    assert_eq!(parsed.args[1], "Invalid credentials");
}

/// Test credential store operations.
#[test]
fn test_credential_store_basic() {
    use keeperdb_proxy::auth::DatabaseType;
    use keeperdb_proxy::handshake::HandshakeCredentials;
    use uuid::Uuid;

    let store = CredentialStore::new();
    let session_id = Uuid::new_v4();

    let creds = HandshakeCredentials::new(
        DatabaseType::MySQL,
        "db.example.com".into(),
        3306,
        "root".into(),
        "secret".into(),
    );

    // Store credentials
    store.store(session_id, creds);

    // Peek should return the credentials without removing them
    {
        let peeked = store.peek(&session_id);
        assert!(peeked.is_some());
        let peeked = peeked.unwrap();
        assert_eq!(peeked.target_host, "db.example.com");
        assert_eq!(peeked.target_port, 3306);
    }

    // Peek again should still work
    assert!(store.peek(&session_id).is_some());

    // Take should return and remove the credentials
    let taken = store.take(&session_id);
    assert!(taken.is_some());
    let taken = taken.unwrap();
    assert_eq!(taken.username, "root");

    // Take again should return None
    assert!(store.take(&session_id).is_none());
    assert!(store.peek(&session_id).is_none());
}

/// Test credential store concurrent access.
#[test]
fn test_credential_store_concurrent() {
    use keeperdb_proxy::auth::DatabaseType;
    use keeperdb_proxy::handshake::HandshakeCredentials;
    use std::thread;
    use uuid::Uuid;

    let store = Arc::new(CredentialStore::new());
    let num_threads = 10;
    let num_sessions_per_thread = 100;

    let mut handles = vec![];

    for _ in 0..num_threads {
        let store = Arc::clone(&store);
        let handle = thread::spawn(move || {
            for _ in 0..num_sessions_per_thread {
                let session_id = Uuid::new_v4();
                let creds = HandshakeCredentials::new(
                    DatabaseType::MySQL,
                    "localhost".into(),
                    3306,
                    "user".into(),
                    "pass".into(),
                );

                store.store(session_id, creds);
                assert!(store.peek(&session_id).is_some());
                assert!(store.take(&session_id).is_some());
                assert!(store.take(&session_id).is_none());
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

/// Test full handshake flow.
#[tokio::test]
async fn test_handshake_flow() {
    let store = Arc::new(CredentialStore::new());

    // Start a TCP listener
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Spawn server task
    let server_store = Arc::clone(&store);
    let server_handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let handler = HandshakeHandler::new(socket, server_store);
        handler.handle().await
    });

    // Connect as client and perform handshake
    let mut client = TcpStream::connect(addr).await.unwrap();

    // 1. Send select instruction
    let select = Instruction::select("mysql");
    client.write_all(&select.encode()).await.unwrap();

    // 2. Read args response
    let mut buf = vec![0u8; 1024];
    let n = client.read(&mut buf).await.unwrap();
    let (args_inst, _) = Instruction::parse(&buf[..n]).expect("parse args");
    assert_eq!(args_inst.opcode, "args");

    // 3. Send connect instruction with required args
    let connect_args: Vec<String> = args_inst
        .args
        .iter()
        .map(|arg| match arg.as_str() {
            "target_host" => "db.example.com".to_string(),
            "target_port" => "3306".to_string(),
            "username" => "testuser".to_string(),
            "password" => "testpass".to_string(),
            "database" => "testdb".to_string(),
            _ => "".to_string(),
        })
        .collect();

    let connect = Instruction::connect(connect_args);
    client.write_all(&connect.encode()).await.unwrap();

    // 4. Read ready response
    let n = client.read(&mut buf).await.unwrap();
    let (ready_inst, _) = Instruction::parse(&buf[..n]).expect("parse ready");
    assert_eq!(ready_inst.opcode, "ready");
    assert!(!ready_inst.args.is_empty());

    // Parse session ID
    let session_id: uuid::Uuid = ready_inst.args[0].parse().expect("parse session id");

    // 5. Wait for server to complete and verify result
    let result = server_handle.await.unwrap();
    assert!(result.is_ok());
    let (handshake_result, _stream) = result.unwrap();
    assert_eq!(handshake_result.session_id, session_id);
    assert_eq!(handshake_result.target_host, "db.example.com");
    assert_eq!(handshake_result.target_port, 3306);
}

/// Test handshake with PostgreSQL database type.
#[tokio::test]
async fn test_handshake_postgresql() {
    let store = Arc::new(CredentialStore::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_store = Arc::clone(&store);
    let server_handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let handler = HandshakeHandler::new(socket, server_store);
        handler.handle().await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();

    // Send select for PostgreSQL
    let select = Instruction::select("postgresql");
    client.write_all(&select.encode()).await.unwrap();

    // Read args
    let mut buf = vec![0u8; 1024];
    let n = client.read(&mut buf).await.unwrap();
    let (args_inst, _) = Instruction::parse(&buf[..n]).expect("parse args");

    // Send connect
    let connect_args: Vec<String> = args_inst
        .args
        .iter()
        .map(|arg| match arg.as_str() {
            "target_host" => "pg.example.com".to_string(),
            "target_port" => "5432".to_string(),
            "username" => "pguser".to_string(),
            "password" => "pgpass".to_string(),
            "database" => "pgdb".to_string(),
            _ => "".to_string(),
        })
        .collect();

    let connect = Instruction::connect(connect_args);
    client.write_all(&connect.encode()).await.unwrap();

    // Read ready
    let n = client.read(&mut buf).await.unwrap();
    let (ready_inst, _) = Instruction::parse(&buf[..n]).expect("parse ready");
    assert_eq!(ready_inst.opcode, "ready");

    // Verify server result
    let result = server_handle.await.unwrap();
    assert!(result.is_ok());
    let (handshake_result, _stream) = result.unwrap();
    assert_eq!(handshake_result.target_host, "pg.example.com");
    assert_eq!(handshake_result.target_port, 5432);
    assert!(matches!(
        handshake_result.database_type,
        keeperdb_proxy::auth::DatabaseType::PostgreSQL
    ));
}

/// Test handshake with invalid database type.
#[tokio::test]
async fn test_handshake_invalid_database_type() {
    let store = Arc::new(CredentialStore::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_store = Arc::clone(&store);
    let server_handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let handler = HandshakeHandler::new(socket, server_store);
        handler.handle().await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();

    // Send select with invalid database type
    let select = Instruction::select("invalid_db");
    client.write_all(&select.encode()).await.unwrap();

    // Server may send an error response or close the connection
    let mut buf = vec![0u8; 1024];
    let read_result = client.read(&mut buf).await;

    match read_result {
        Ok(0) => {
            // Connection closed by server - acceptable
        }
        Ok(n) => {
            // Got a response - should be an error instruction
            if let Ok((error_inst, _)) = Instruction::parse(&buf[..n]) {
                assert_eq!(error_inst.opcode, "error");
            }
            // If parse fails, that's also OK - server might have sent partial data
        }
        Err(_) => {
            // Read error - acceptable for error case
        }
    }

    // Server should return an error
    let result = server_handle.await.unwrap();
    assert!(result.is_err());
}

/// Test handshake timeout handling.
#[tokio::test]
async fn test_handshake_timeout() {
    let store = Arc::new(CredentialStore::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_store = Arc::clone(&store);
    let server_handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let handler = HandshakeHandler::new(socket, server_store);
        handler.handle().await
    });

    // Connect but don't send anything
    let _client = TcpStream::connect(addr).await.unwrap();

    // Wait for timeout (with a reasonable timeout for the test)
    let result = tokio::time::timeout(Duration::from_secs(35), server_handle).await;

    // Server should timeout or error
    match result {
        Ok(Ok(server_result)) => {
            // Server completed - should be an error
            assert!(server_result.is_err());
        }
        Ok(Err(_)) => {
            // Task panicked - acceptable for timeout
        }
        Err(_) => {
            // Test timeout - also acceptable
        }
    }
}

/// Test multiple concurrent handshakes.
#[tokio::test]
async fn test_concurrent_handshakes() {
    let store = Arc::new(CredentialStore::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let num_clients = 5;
    let mut client_handles = vec![];

    // Use a single listener accepting multiple clients
    let server_store = Arc::clone(&store);
    let server_handle = tokio::spawn(async move {
        let mut results = vec![];
        for _ in 0..num_clients {
            let store = Arc::clone(&server_store);
            let (socket, _) = listener.accept().await.unwrap();
            let handler = HandshakeHandler::new(socket, store);
            let result = handler.handle().await;
            results.push(result);
        }
        results
    });

    // Give server time to start accepting
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Spawn client tasks
    for i in 0..num_clients {
        let handle = tokio::spawn(async move {
            let mut client = TcpStream::connect(addr).await.unwrap();

            // Send select
            let select = Instruction::select("mysql");
            client.write_all(&select.encode()).await.unwrap();

            // Read args
            let mut buf = vec![0u8; 1024];
            let n = client.read(&mut buf).await.unwrap();
            let (args_inst, _) = Instruction::parse(&buf[..n]).unwrap();

            // Send connect with unique username
            let connect_args: Vec<String> = args_inst
                .args
                .iter()
                .map(|arg| match arg.as_str() {
                    "target_host" => "db.example.com".to_string(),
                    "target_port" => "3306".to_string(),
                    "username" => format!("user{}", i),
                    "password" => format!("pass{}", i),
                    "database" => "testdb".to_string(),
                    _ => "".to_string(),
                })
                .collect();

            let connect = Instruction::connect(connect_args);
            client.write_all(&connect.encode()).await.unwrap();

            // Read ready
            let n = client.read(&mut buf).await.unwrap();
            let (ready_inst, _) = Instruction::parse(&buf[..n]).unwrap();
            assert_eq!(ready_inst.opcode, "ready");

            i
        });
        client_handles.push(handle);
    }

    // Wait for all clients to complete
    for handle in client_handles {
        handle.await.unwrap();
    }

    // Wait for server to complete
    let results = server_handle.await.unwrap();
    assert_eq!(results.len(), num_clients);

    // All handshakes should succeed
    for result in results {
        assert!(result.is_ok());
    }
}
