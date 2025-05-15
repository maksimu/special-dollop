//! Protocol handler tests
use crate::channel::protocols;
use crate::runtime::get_runtime;
use std::collections::HashMap;
use serde_json::Value;

#[test]
fn test_protocol_handler_registration() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Test SOCKS5 handler creation with server mode settings
        let mut settings = HashMap::new();
        settings.insert("mode".to_string(), Value::String("server".to_string()));

        // Test allowed hosts/ports configuration
        settings.insert("allowed_hosts".to_string(), Value::String(vec!["localhost".to_string(), "127.0.0.1".to_string()].join(",")));
        settings.insert("allowed_ports".to_string(), Value::String(format!("{},{}", 8080, 8081)));
        settings.insert("conversationType".to_string(), Value::String("tunnel".to_string()));

        // Verify handler can be created without errors
        let handler_result = protocols::create_protocol_handler(
            false,
            settings.clone(),
        );

        assert!(handler_result.is_ok(), "Should create SOCKS5 handler without errors");

        // Also test for guacd protocol
        settings.clear();
        settings.insert("client_id".to_string(), Value::String("test-client".to_string()));
        settings.insert("conversationType".to_string(), Value::String("rdp".to_string()));
        let guacd_result = protocols::create_protocol_handler(false, settings);
        assert!(guacd_result.is_ok(), "Should create guacd handler without errors");
    });
}

#[test]
fn test_port_forward_protocol_handler() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Get the list of supported protocols
        let mut supported = false;
        
        let mut settings = HashMap::new();
        settings.insert("conversationType".to_string(), Value::String("tunnel".to_string()));

        // Skip test if port_forward protocol is not supported
        if let Ok(mut handler) = protocols::create_protocol_handler(
            false,
            settings,
        ) {
            supported = true;

            // Create the protocol handler with settings if supported
            let mut settings = HashMap::new();
            settings.insert("target_host".to_string(), Value::String("127.0.0.1".to_string()));
            settings.insert("target_port".to_string(), Value::String("8080".to_string()));

            // Create fake test data
            let test_data = vec![1, 2, 3, 4, 5];

            // Test connection number
            let conn_no = 42;

            // Initialize the handler
            let init_result = handler.initialize().await;
            assert!(init_result.is_ok(), "Handler initialization should succeed");

            // Test new connection handling
            let new_conn_result = handler.handle_new_connection(conn_no).await;
            assert!(new_conn_result.is_ok(), "New connection handling should succeed");

            // Process client data
            let client_result = handler.process_client_data(conn_no, &test_data).await;
            assert!(client_result.is_ok(), "Client data processing should succeed");

            // Process server data
            let server_result = handler.process_server_data(conn_no, &test_data).await;
            assert!(server_result.is_ok(), "Server data processing should succeed");

            // Test EOF handling
            let eof_result = handler.handle_eof(conn_no).await;
            assert!(eof_result.is_ok(), "EOF handling should succeed");

            // Test close handling
            let close_result = handler.handle_close(conn_no).await;
            assert!(close_result.is_ok(), "Close handling should succeed");

            // Test command handling
            let command_result = handler.handle_command("disconnect", &[]).await;
            assert!(command_result.is_ok(), "Command handling should succeed");

            // Test status reporting
            let status = handler.status();
            assert!(status.contains("Port Forward"), "Status should mention port forwarding");

            // Test shutdown
            let shutdown_result = handler.shutdown().await;
            assert!(shutdown_result.is_ok(), "Handler shutdown should succeed");
        }

        if !supported {
            // Skip the test but don't fail it
            assert!(supported, "Port forward protocol should be supported");
        }
    });
}

#[test]
fn test_socks5_server_mode() {
    let runtime = get_runtime();
    runtime.block_on(async {
        // Create a SOCKS5 handler in server mode
        let mut settings = HashMap::new();
        settings.insert("mode".to_string(), Value::String("server".to_string()));

        // Allowed hosts and ports for security restrictions
        // Test allowed hosts/ports configuration
        settings.insert("allowed_hosts".to_string(), Value::String(vec!["localhost".to_string(), "127.0.0.1".to_string()].join(",")));
        settings.insert("allowed_ports".to_string(), Value::String(format!("{},{},{}", 8080, 8081, 9090)));

        settings.insert("conversationType".to_string(), Value::String("tunnel".to_string()));

        // Create the handler using the factory function
        let handler_result = protocols::create_protocol_handler(
            true,
            settings.clone(),
        );

        assert!(handler_result.is_ok(), "SOCKS5 handler creation should succeed");
        let mut handler = handler_result.unwrap();

        // Initialize the handler
        let init_result = handler.initialize().await;
        assert!(init_result.is_ok(), "Handler initialization should succeed");

        // Test status reporting
        let status = handler.status();
        assert!(status.contains("SOCKS5"), "Status should mention SOCKS5");

        // Test shutdown
        let shutdown_result = handler.shutdown().await;
        assert!(shutdown_result.is_ok(), "Handler shutdown should succeed");
    });
}