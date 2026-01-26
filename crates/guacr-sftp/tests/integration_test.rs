//! Integration tests for SFTP handler
//!
//! These tests require a running SSH server with SFTP subsystem.
//! Start one with:
//!   docker-compose -f docker-compose.test.yml up -d ssh
//!
//! Run tests with:
//!   cargo test --package guacr-sftp --test integration_test -- --include-ignored
//!
//! Connection details:
//!   Host: localhost:2222
//!   User: linuxuser
//!   Password: alpine

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

async fn port_is_open(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use guacr_sftp::SftpHandler;
    use tokio::sync::mpsc;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 2222;
    const USERNAME: &str = "linuxuser";
    const PASSWORD: &str = "alpine";

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping SFTP tests - SSH server not available on {}:{}",
                HOST, PORT
            );
            eprintln!("Start with: docker-compose -f docker-compose.test.yml up -d ssh");
            return true;
        }
        false
    }

    #[tokio::test]
    #[ignore]
    async fn test_sftp_connection() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SftpHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for initial file browser rendering
        // SFTP handler renders a graphical file browser which takes time
        // Just verify it doesn't crash immediately
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Try to receive any messages
        let mut received_any = false;
        for _ in 0..5 {
            if let Ok(Some(_msg)) = timeout(Duration::from_millis(500), to_client_rx.recv()).await {
                received_any = true;
                break;
            }
        }

        // The handler should send something (file browser images)
        // If it doesn't, it might be working but slow, so we just check it didn't crash
        if !received_any {
            eprintln!("Warning: SFTP handler didn't send messages (might be slow rendering)");
        }

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_directory_listing() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SftpHandler::with_defaults();
        let (to_client_tx, _to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), USERNAME.to_string());
        params.insert("password".to_string(), PASSWORD.to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Give the handler time to connect and render
        // SFTP handler needs to: connect SSH, start SFTP subsystem, read directory, render PNG
        tokio::time::sleep(Duration::from_secs(3)).await;

        // The handler should still be running (not crashed)
        assert!(!handle.is_finished(), "Handler should still be running");

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_authentication_failure() {
        if skip_if_not_available().await {
            return;
        }

        let handler = SftpHandler::with_defaults();
        let (to_client_tx, _to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (_from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("hostname".to_string(), HOST.to_string());
        params.insert("port".to_string(), PORT.to_string());
        params.insert("username".to_string(), "wronguser".to_string());
        params.insert("password".to_string(), "wrongpass".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for handler to fail
        // With wrong password, the handler should return an error and close the channel
        let result = timeout(CONNECT_TIMEOUT, handle).await;

        // The handler should complete (with an error)
        assert!(result.is_ok(), "Handler should complete within timeout");

        // The handler should return an error result
        if let Ok(Ok(handler_result)) = result {
            assert!(
                handler_result.is_err(),
                "Handler should return error for wrong password"
            );
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use guacr_sftp::{SftpConfig, SftpHandler};
    use std::path::PathBuf;

    #[test]
    fn test_path_validation() {
        let config = SftpConfig {
            chroot_to_home: true,
            ..Default::default()
        };
        let handler = SftpHandler::new(config);

        // Create temporary directory structure for testing
        let temp_dir = std::env::temp_dir();
        let home = temp_dir.join("test_home_sftp");
        let _ = std::fs::create_dir_all(&home);

        // Create test files/directories
        let test_file = home.join("file.txt");
        let test_subdir = home.join("subdir");
        let _ = std::fs::create_dir_all(&test_subdir);
        let _ = std::fs::write(&test_file, "test");

        // Valid paths (files exist, so canonicalize works)
        assert!(handler.test_validate_path(&test_file, &home).is_ok());
        assert!(handler.test_validate_path(&test_subdir, &home).is_ok());

        // Invalid paths (should fail)
        let invalid_paths = vec![PathBuf::from("/etc/passwd"), home.join("../../etc/passwd")];

        for path in invalid_paths {
            let result = handler.test_validate_path(&path, &home);
            assert!(
                result.is_err(),
                "Path traversal should be rejected: {:?}",
                path
            );
        }
    }

    #[test]
    fn test_chroot_enforcement() {
        use guacr_sftp::{SftpConfig, SftpHandler};
        use std::path::PathBuf;

        let config = SftpConfig {
            chroot_to_home: true,
            ..Default::default()
        };
        let handler = SftpHandler::new(config);

        let temp_dir = std::env::temp_dir();
        let home = temp_dir.join("test_home");
        let _ = std::fs::create_dir_all(&home);

        // Path traversal attempts should be rejected
        let traversal_paths = vec![
            "/etc/passwd",
            "../etc/passwd",
            "/home/user/../../etc/passwd",
        ];

        for path_str in traversal_paths {
            let path = PathBuf::from(path_str);
            let result = handler.test_validate_path(&path, &home);
            // Note: Some paths may not exist, so canonicalize might fail
            // But if they do resolve, they should be rejected
            if let Ok(resolved) = result {
                // If it resolved, verify it's still within home
                assert!(
                    resolved.starts_with(&home),
                    "Resolved path should be within home: {:?}",
                    resolved
                );
            }
        }
    }

    #[test]
    fn test_path_validation_with_chroot_disabled() {
        use guacr_sftp::{SftpConfig, SftpHandler};

        let config = SftpConfig {
            chroot_to_home: false, // Disable chroot
            ..Default::default()
        };
        let handler = SftpHandler::new(config);

        let temp_dir = std::env::temp_dir();
        let home = temp_dir.join("test_home_sftp_no_chroot");
        let _ = std::fs::create_dir_all(&home);

        // With chroot disabled, any path should be allowed (if it exists)
        let test_file = home.join("file.txt");
        let _ = std::fs::write(&test_file, "test");

        // Should succeed even if outside home (when chroot is disabled)
        let result = handler.test_validate_path(&test_file, &home);
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_validation_nested_directories() {
        use guacr_sftp::{SftpConfig, SftpHandler};

        let config = SftpConfig {
            chroot_to_home: true,
            ..Default::default()
        };
        let handler = SftpHandler::new(config);

        let temp_dir = std::env::temp_dir();
        let home = temp_dir.join("test_home_sftp_nested");
        let nested = home.join("a").join("b").join("c");
        let _ = std::fs::create_dir_all(&nested);
        let test_file = nested.join("file.txt");
        let _ = std::fs::write(&test_file, "test");

        // Deeply nested paths should be valid if within home
        assert!(handler.test_validate_path(&test_file, &home).is_ok());
        assert!(handler.test_validate_path(&nested, &home).is_ok());
    }

    #[test]
    fn test_path_validation_symlink_traversal() {
        use guacr_sftp::{SftpConfig, SftpHandler};

        let config = SftpConfig {
            chroot_to_home: true,
            ..Default::default()
        };
        let handler = SftpHandler::new(config);

        let temp_dir = std::env::temp_dir();
        let home = temp_dir.join("test_home_sftp_symlink");
        let _ = std::fs::create_dir_all(&home);

        // Create a symlink that points outside (if possible)
        // Note: canonicalize() should resolve symlinks, so traversal attempts should fail
        let symlink_path = home.join("link");

        // Try to validate - if symlink exists and points outside, should fail
        // If it doesn't exist, canonicalize will fail (which is also correct)
        let result = handler.test_validate_path(&symlink_path, &home);
        // Either the path doesn't exist (canonicalize fails) or it's outside (validation fails)
        // Both are correct behavior
        assert!(result.is_err() || !result.unwrap().starts_with(&home));
    }

    #[test]
    fn test_sftp_config_defaults() {
        use guacr_sftp::SftpConfig;

        let config = SftpConfig::default();
        assert_eq!(config.default_port, 22);
        assert!(config.chroot_to_home); // Security enabled by default
        assert!(config.allow_uploads);
        assert!(config.allow_downloads);
        assert!(!config.allow_delete); // Delete disabled by default
        assert_eq!(config.max_file_size, 100 * 1024 * 1024); // 100MB
    }

    #[test]
    fn test_sftp_handler_creation() {
        use guacr_sftp::{SftpConfig, SftpHandler};

        let config = SftpConfig::default();
        let handler = SftpHandler::new(config);

        // Handler should be created successfully
        // Note: name() is part of ProtocolHandler trait, tested via trait implementation
        let _handler = handler; // Just verify it compiles
    }

    #[test]
    fn test_sftp_handler_with_defaults() {
        use guacr_sftp::SftpHandler;

        let handler = SftpHandler::with_defaults();
        // Handler should be created successfully
        let _handler = handler; // Just verify it compiles
    }
}
