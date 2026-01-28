//! Integration tests for pipe stream functionality
//!
//! These tests verify the pipe stream implementation for native terminal display.
//! They test the protocol-level behavior without requiring actual SSH/Telnet servers.
//!
//! Test categories:
//! 1. Basic pipe stream operations (creation, flags, manager)
//! 2. Instruction formatting and parsing (roundtrip verification)
//! 3. Security integration (read-only mode, input blocking)
//! 4. Recording integration (pipe data captured in recordings)
//! 5. Bidirectional flow simulation (STDOUT + STDIN)

use bytes::Bytes;
use guacr_handlers::{
    // Pipe instruction formatting
    format_pipe_blob,
    format_pipe_instruction,
    // Pipe instruction parsing
    parse_blob_instruction,
    parse_end_instruction,
    parse_pipe_instruction,
    pipe_blob_bytes,
    // Security
    HandlerSecuritySettings,
    // Pipe stream types
    PipeStream,
    PipeStreamManager,
    // Pipe stream constants
    PIPE_AUTOFLUSH,
    PIPE_INTERPRET_OUTPUT,
    PIPE_NAME_STDIN,
    PIPE_NAME_STDOUT,
    PIPE_RAW,
    PIPE_STREAM_STDOUT,
};

mod pipe_stream_tests {
    use super::*;

    #[test]
    fn test_stdout_pipe_creation() {
        let pipe = PipeStream::stdout();

        assert_eq!(pipe.name, PIPE_NAME_STDOUT);
        assert_eq!(pipe.stream_id, PIPE_STREAM_STDOUT);
        assert!(pipe.is_raw(), "STDOUT pipe should have RAW flag");
        assert!(
            pipe.is_autoflush(),
            "STDOUT pipe should have AUTOFLUSH flag"
        );
        assert!(
            pipe.interpret_output(),
            "STDOUT pipe should have INTERPRET_OUTPUT flag"
        );
        assert!(!pipe.is_open, "New pipe should not be open yet");
    }

    #[test]
    fn test_stdin_pipe_creation() {
        let pipe = PipeStream::stdin();

        assert_eq!(pipe.name, PIPE_NAME_STDIN);
        assert!(
            !pipe.is_raw(),
            "STDIN pipe should not have RAW flag by default"
        );
    }

    #[test]
    fn test_pipe_flags_combinations() {
        // Test with only RAW flag
        let pipe = PipeStream::new(1, "test", "text/plain", PIPE_RAW);
        assert!(pipe.is_raw());
        assert!(!pipe.is_autoflush());
        assert!(!pipe.interpret_output());

        // Test with multiple flags
        let pipe = PipeStream::new(2, "test", "text/plain", PIPE_RAW | PIPE_AUTOFLUSH);
        assert!(pipe.is_raw());
        assert!(pipe.is_autoflush());
        assert!(!pipe.interpret_output());

        // Test with all flags
        let pipe = PipeStream::new(
            3,
            "test",
            "text/plain",
            PIPE_RAW | PIPE_AUTOFLUSH | PIPE_INTERPRET_OUTPUT,
        );
        assert!(pipe.is_raw());
        assert!(pipe.is_autoflush());
        assert!(pipe.interpret_output());
    }
}

mod pipe_manager_tests {
    use super::*;

    #[test]
    fn test_enable_stdout_returns_instruction() {
        let mut manager = PipeStreamManager::new();

        assert!(!manager.is_stdout_enabled());

        let instruction = manager.enable_stdout();

        assert!(manager.is_stdout_enabled());
        assert!(instruction.contains("pipe"));
        assert!(instruction.contains("STDOUT"));
        assert!(instruction.contains("application/octet-stream"));
    }

    #[test]
    fn test_register_incoming_stdin() {
        let mut manager = PipeStreamManager::new();

        manager.register_incoming(101, PIPE_NAME_STDIN, "text/plain");

        assert!(manager.is_stdin_stream(101));
        assert!(!manager.is_stdin_stream(100));
        assert!(!manager.is_stdin_stream(102));
    }

    #[test]
    fn test_get_pipe_by_name() {
        let mut manager = PipeStreamManager::new();
        manager.enable_stdout();

        let stdout = manager.get(PIPE_NAME_STDOUT);
        assert!(stdout.is_some());
        assert_eq!(stdout.unwrap().name, PIPE_NAME_STDOUT);

        let stdin = manager.get(PIPE_NAME_STDIN);
        assert!(stdin.is_none());
    }

    #[test]
    fn test_close_pipe() {
        let mut manager = PipeStreamManager::new();
        manager.enable_stdout();

        // Mark as open (simulating after instruction sent)
        if let Some(pipe) = manager.stdout() {
            assert!(!pipe.is_open); // Not automatically marked open
        }

        // Closing a not-yet-open pipe returns None
        let end_instr = manager.close(PIPE_NAME_STDOUT);
        assert!(end_instr.is_none());
    }

    #[test]
    fn test_close_all_pipes() {
        let mut manager = PipeStreamManager::new();
        manager.enable_stdout();
        manager.register_incoming(101, PIPE_NAME_STDIN, "text/plain");

        // Close all should return end instructions for open pipes
        let instructions = manager.close_all();

        // STDIN is marked as open when registered, so we get 1 end instruction
        // STDOUT is NOT marked as open (needs explicit marking after sending)
        assert_eq!(instructions.len(), 1);
        assert!(instructions[0].contains("end"));
        assert!(instructions[0].contains("101"));
    }
}

mod instruction_formatting_tests {
    use super::*;

    #[test]
    fn test_format_pipe_instruction_basic() {
        let pipe = PipeStream::stdout();
        let instr = format_pipe_instruction(&pipe);

        // Verify format: 4.pipe,{stream_len}.{stream},{mimetype_len}.{mimetype},{name_len}.{name};
        assert!(instr.starts_with("4.pipe,"));
        assert!(instr.ends_with(";"));
        assert!(instr.contains("100")); // PIPE_STREAM_STDOUT
        assert!(instr.contains("application/octet-stream"));
        assert!(instr.contains("STDOUT"));
    }

    #[test]
    fn test_format_pipe_blob_basic() {
        let data = b"Hello, World!";
        let instr = format_pipe_blob(100, data);

        // Verify format: 4.blob,{stream_len}.{stream},{data_len}.{base64_data};
        assert!(instr.starts_with("4.blob,"));
        assert!(instr.ends_with(";"));
        assert!(instr.contains("100")); // Stream ID

        // Verify base64 encoding
        // "Hello, World!" base64 = "SGVsbG8sIFdvcmxkIQ=="
        assert!(instr.contains("SGVsbG8sIFdvcmxkIQ=="));
    }

    #[test]
    fn test_format_pipe_blob_with_ansi_codes() {
        // Test with ANSI escape sequence (colored text)
        let data = b"\x1b[31mRed Text\x1b[0m";
        let instr = format_pipe_blob(100, data);

        assert!(instr.starts_with("4.blob,"));
        assert!(instr.ends_with(";"));

        // Should be base64 encoded
        // The base64 should decode back to the original ANSI sequence
        use base64::Engine;
        let base64_part = instr
            .split(',')
            .next_back()
            .unwrap()
            .trim_end_matches(';')
            .split('.')
            .next_back()
            .unwrap();

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(base64_part)
            .expect("Should decode base64");

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_format_pipe_blob_empty() {
        let data = b"";
        let instr = format_pipe_blob(100, data);

        assert!(instr.starts_with("4.blob,"));
        assert!(instr.ends_with(";"));
        // Empty data base64 = ""
        assert!(instr.contains(",0.;"));
    }

    #[test]
    fn test_pipe_blob_bytes_returns_bytes() {
        let data = b"test data";
        let bytes = pipe_blob_bytes(100, data);

        assert!(!bytes.is_empty());
        let instr_str = String::from_utf8_lossy(&bytes);
        assert!(instr_str.starts_with("4.blob,"));
    }
}

mod instruction_parsing_tests {
    use super::*;

    #[test]
    fn test_parse_pipe_instruction_valid() {
        let instr = "4.pipe,3.100,24.application/octet-stream,6.STDOUT;";
        let parsed = parse_pipe_instruction(instr);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, 100);
        assert_eq!(parsed.mimetype, "application/octet-stream");
        assert_eq!(parsed.name, "STDOUT");
    }

    #[test]
    fn test_parse_pipe_instruction_stdin() {
        let instr = "4.pipe,3.101,10.text/plain,5.STDIN;";
        let parsed = parse_pipe_instruction(instr);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, 101);
        assert_eq!(parsed.name, "STDIN");
    }

    #[test]
    fn test_parse_pipe_instruction_not_pipe() {
        let instr = "3.key,5.65507,1.1;";
        let parsed = parse_pipe_instruction(instr);

        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_blob_instruction_valid() {
        // "test" base64 = "dGVzdA=="
        let instr = "4.blob,3.100,8.dGVzdA==;";
        let parsed = parse_blob_instruction(instr);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, 100);
        assert_eq!(parsed.data, b"test");
    }

    #[test]
    fn test_parse_blob_instruction_with_ansi() {
        // ANSI escape sequence: "\x1b[31m" (red color)
        // base64 of "\x1b[31m" = "G1szMW0="
        use base64::Engine;
        let ansi_data = b"\x1b[31m";
        let base64_data = base64::engine::general_purpose::STANDARD.encode(ansi_data);
        let instr = format!("4.blob,3.100,{}.{};", base64_data.len(), base64_data);

        let parsed = parse_blob_instruction(&instr);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.data, ansi_data);
    }

    #[test]
    fn test_parse_blob_instruction_not_blob() {
        let instr = "4.pipe,3.100,24.application/octet-stream,6.STDOUT;";
        let parsed = parse_blob_instruction(instr);

        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_end_instruction_valid() {
        let instr = "3.end,3.100;";
        let stream_id = parse_end_instruction(instr);

        assert_eq!(stream_id, Some(100));
    }

    #[test]
    fn test_parse_end_instruction_not_end() {
        let instr = "4.blob,3.100,8.dGVzdA==;";
        let stream_id = parse_end_instruction(instr);

        assert!(stream_id.is_none());
    }
}

mod roundtrip_tests {
    use super::*;

    #[test]
    fn test_pipe_instruction_roundtrip() {
        let pipe = PipeStream::stdout();
        let formatted = format_pipe_instruction(&pipe);
        let parsed = parse_pipe_instruction(&formatted);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, pipe.stream_id);
        assert_eq!(parsed.mimetype, pipe.mimetype);
        assert_eq!(parsed.name, pipe.name);
    }

    #[test]
    fn test_blob_instruction_roundtrip() {
        let original_data = b"Hello, \x1b[32mGreen World\x1b[0m!\nNewline test";
        let stream_id = 42;

        let formatted = format_pipe_blob(stream_id, original_data);
        let parsed = parse_blob_instruction(&formatted);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, stream_id);
        assert_eq!(parsed.data, original_data);
    }

    #[test]
    fn test_large_blob_roundtrip() {
        // Simulate a large terminal output
        let mut large_data = Vec::new();
        for i in 0..1000 {
            large_data
                .extend_from_slice(format!("Line {}: Some terminal output text\r\n", i).as_bytes());
        }

        let formatted = format_pipe_blob(100, &large_data);
        let parsed = parse_blob_instruction(&formatted);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.data.len(), large_data.len());
        assert_eq!(parsed.data, large_data);
    }
}

mod simulation_tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Simulate the server-side STDOUT pipe flow
    #[tokio::test]
    async fn test_stdout_pipe_server_flow() {
        let (tx, mut rx) = mpsc::channel::<Bytes>(100);

        // Server: Initialize pipe manager and enable STDOUT
        let mut manager = PipeStreamManager::new();
        let pipe_instr = manager.enable_stdout();

        // Server: Send pipe open instruction
        tx.send(Bytes::from(pipe_instr)).await.unwrap();

        // Server: Send terminal data through pipe
        let terminal_output = b"user@host:~$ ls -la\r\ntotal 42\r\n";
        let blob_instr = format_pipe_blob(PIPE_STREAM_STDOUT, terminal_output);
        tx.send(Bytes::from(blob_instr)).await.unwrap();

        // Server: Send more output with ANSI colors
        let colored_output =
            b"\x1b[32mdrwxr-xr-x\x1b[0m 2 user user 4096 Dec  3 10:00 Documents\r\n";
        let blob_instr2 = format_pipe_blob(PIPE_STREAM_STDOUT, colored_output);
        tx.send(Bytes::from(blob_instr2)).await.unwrap();

        drop(tx);

        // Client: Receive and parse instructions
        let mut messages = Vec::new();
        while let Some(msg) = rx.recv().await {
            messages.push(String::from_utf8_lossy(&msg).to_string());
        }

        assert_eq!(messages.len(), 3);

        // Verify pipe instruction
        let pipe_parsed = parse_pipe_instruction(&messages[0]);
        assert!(pipe_parsed.is_some());
        assert_eq!(pipe_parsed.unwrap().name, "STDOUT");

        // Verify first blob
        let blob1 = parse_blob_instruction(&messages[1]);
        assert!(blob1.is_some());
        assert_eq!(blob1.unwrap().data, terminal_output);

        // Verify second blob with ANSI
        let blob2 = parse_blob_instruction(&messages[2]);
        assert!(blob2.is_some());
        assert_eq!(blob2.unwrap().data, colored_output);
    }

    /// Simulate the client-side STDIN pipe flow
    #[tokio::test]
    async fn test_stdin_pipe_client_flow() {
        // Server: Set up manager
        let mut manager = PipeStreamManager::new();

        // Client: Open STDIN pipe
        let stdin_pipe_instr = "4.pipe,3.101,10.text/plain,5.STDIN;";

        // Server: Parse and register incoming pipe
        let parsed = parse_pipe_instruction(stdin_pipe_instr).unwrap();
        manager.register_incoming(parsed.stream_id, &parsed.name, &parsed.mimetype);

        assert!(manager.is_stdin_stream(101));

        // Client: Send user input
        let user_input = b"ls -la\n";
        use base64::Engine;
        let base64_input = base64::engine::general_purpose::STANDARD.encode(user_input);
        let blob_instr = format!("4.blob,3.101,{}.{};", base64_input.len(), base64_input);

        // Server: Parse blob and verify it's for STDIN
        let blob = parse_blob_instruction(&blob_instr).unwrap();
        assert!(manager.is_stdin_stream(blob.stream_id));
        assert_eq!(blob.data, user_input);
    }

    /// Test bidirectional pipe flow
    #[tokio::test]
    async fn test_bidirectional_pipe_flow() {
        // This simulates a full terminal session with pipes

        let mut manager = PipeStreamManager::new();

        // 1. Server opens STDOUT pipe
        let stdout_instr = manager.enable_stdout();
        assert!(stdout_instr.contains("STDOUT"));

        // 2. Client opens STDIN pipe
        let stdin_instr = "4.pipe,3.101,24.application/octet-stream,5.STDIN;";
        let parsed = parse_pipe_instruction(stdin_instr).unwrap();
        manager.register_incoming(parsed.stream_id, &parsed.name, &parsed.mimetype);

        // 3. Server sends prompt
        let prompt = b"$ ";
        let prompt_blob = format_pipe_blob(PIPE_STREAM_STDOUT, prompt);
        let parsed_prompt = parse_blob_instruction(&prompt_blob).unwrap();
        assert_eq!(parsed_prompt.data, prompt);

        // 4. Client sends command
        let command = b"echo hello\n";
        use base64::Engine;
        let cmd_b64 = base64::engine::general_purpose::STANDARD.encode(command);
        let cmd_blob = format!("4.blob,3.101,{}.{};", cmd_b64.len(), cmd_b64);
        let parsed_cmd = parse_blob_instruction(&cmd_blob).unwrap();
        assert!(manager.is_stdin_stream(parsed_cmd.stream_id));
        assert_eq!(parsed_cmd.data, command);

        // 5. Server sends response
        let response = b"hello\n$ ";
        let resp_blob = format_pipe_blob(PIPE_STREAM_STDOUT, response);
        let parsed_resp = parse_blob_instruction(&resp_blob).unwrap();
        assert_eq!(parsed_resp.data, response);

        // 6. Client closes STDIN
        let end_instr = "3.end,3.101;";
        let end_stream = parse_end_instruction(end_instr).unwrap();
        assert!(manager.is_stdin_stream(end_stream));
        manager.close(PIPE_NAME_STDIN);
    }
}

// ============================================================================
// Security Integration Tests
// ============================================================================

mod security_tests {
    use super::*;
    use std::collections::HashMap;

    /// Test that STDIN pipe respects read-only mode
    #[test]
    fn test_stdin_blocked_in_readonly_mode() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());

        let security = HandlerSecuritySettings::from_params(&params);

        // Simulate receiving STDIN blob
        let stdin_data = b"rm -rf /\n";
        use base64::Engine;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(stdin_data);
        let blob_instr = format!("4.blob,3.101,{}.{};", base64_data.len(), base64_data);
        let parsed = parse_blob_instruction(&blob_instr).unwrap();

        // In read-only mode, we should NOT forward this to the SSH channel
        // The handler checks security.read_only before forwarding
        assert!(security.read_only);
        assert!(!security.is_keyboard_allowed());

        // Data was parsed but should be blocked by handler
        assert_eq!(parsed.data, stdin_data);
    }

    /// Test that clipboard paste on STDIN respects disable-paste
    #[test]
    fn test_stdin_paste_blocked_when_disabled() {
        let mut params = HashMap::new();
        params.insert("disable-paste".to_string(), "true".to_string());

        let security = HandlerSecuritySettings::from_params(&params);

        // Paste should be blocked
        assert!(!security.is_paste_allowed());

        // But regular keyboard input would still work (not read-only)
        assert!(security.is_keyboard_allowed());
    }

    /// Test that pipe input respects clipboard size limits
    #[test]
    fn test_stdin_respects_clipboard_size_limit() {
        let mut params = HashMap::new();
        params.insert("clipboard-buffer-size".to_string(), "1024".to_string()); // 1KB

        let security = HandlerSecuritySettings::from_params(&params);

        // Note: clipboard-buffer-size has a minimum of 256KB
        // So 1024 gets clamped to 262144 (256KB)
        assert_eq!(security.clipboard_buffer_size, 256 * 1024);

        // Large paste data should be truncated by handler
        let large_data = vec![b'x'; 300 * 1024]; // 300KB
        let truncated_len = security.clipboard_buffer_size.min(large_data.len());
        assert_eq!(truncated_len, 256 * 1024);
    }

    /// Test security settings parsing for pipe-enabled connection
    #[test]
    fn test_pipe_connection_security_params() {
        let mut params = HashMap::new();
        params.insert("enable-pipe".to_string(), "true".to_string());
        params.insert("read-only".to_string(), "false".to_string());
        params.insert("disable-copy".to_string(), "false".to_string());
        params.insert("disable-paste".to_string(), "false".to_string());

        let security = HandlerSecuritySettings::from_params(&params);

        // All operations allowed
        assert!(!security.read_only);
        assert!(security.is_keyboard_allowed());
        assert!(security.is_copy_allowed());
        assert!(security.is_paste_allowed());
    }

    /// Test that Ctrl+C is allowed even in read-only mode (for interrupt)
    #[test]
    fn test_ctrl_c_allowed_readonly() {
        use guacr_handlers::is_keyboard_event_allowed_readonly;

        // Ctrl+C should be allowed (keysym 0x63 with ctrl=true)
        assert!(is_keyboard_event_allowed_readonly(0x63, true));

        // Regular 'c' without Ctrl should be blocked
        assert!(!is_keyboard_event_allowed_readonly(0x63, false));

        // Other keys with Ctrl should be blocked (e.g., Ctrl+D)
        assert!(!is_keyboard_event_allowed_readonly(0x64, true));
    }
}

// ============================================================================
// Recording Integration Tests (Simulated)
// ============================================================================

mod recording_tests {
    use super::*;

    /// Verify pipe data format is suitable for recording
    #[test]
    fn test_pipe_data_format_for_recording() {
        // Raw terminal output with ANSI codes
        let terminal_data = b"\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/project\x1b[0m$ ls -la\r\n";

        // Create blob instruction
        let blob = format_pipe_blob(PIPE_STREAM_STDOUT, terminal_data);

        // Parse it back
        let parsed = parse_blob_instruction(&blob).unwrap();

        // Verify roundtrip preserves all ANSI codes
        assert_eq!(parsed.data, terminal_data);

        // This data would be recorded by MultiFormatRecorder:
        // - SES format: as-is (Guacamole session)
        // - Asciicast: raw terminal output with timestamps
        // - Typescript: raw terminal output
    }

    /// Test that pipe output preserves timing information for recordings
    #[test]
    fn test_pipe_preserves_chunk_boundaries() {
        // Multiple chunks of output (simulating streaming)
        let chunks = [
            b"First line\r\n".to_vec(),
            b"Second line\r\n".to_vec(),
            b"\x1b[32mColored\x1b[0m\r\n".to_vec(),
        ];

        // Each chunk becomes a separate blob
        let mut blobs = Vec::new();
        for chunk in &chunks {
            blobs.push(format_pipe_blob(PIPE_STREAM_STDOUT, chunk));
        }

        // Verify each blob preserves its chunk
        for (i, blob) in blobs.iter().enumerate() {
            let parsed = parse_blob_instruction(blob).unwrap();
            assert_eq!(parsed.data, chunks[i]);
        }

        // Recording can capture timing between blobs for asciicast replay
    }

    /// Test recording-compatible escaping
    #[test]
    fn test_special_chars_preserved() {
        // Characters that might need escaping in various formats
        let special_data = b"Tab:\t Quote:\" Backslash:\\ Newline:\n Control:\x03\x04";

        let blob = format_pipe_blob(PIPE_STREAM_STDOUT, special_data);
        let parsed = parse_blob_instruction(&blob).unwrap();

        assert_eq!(parsed.data, special_data);
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling_tests {
    use super::*;

    #[test]
    fn test_malformed_pipe_instruction() {
        // Missing name
        assert!(parse_pipe_instruction("4.pipe,3.100,24.application/octet-stream;").is_none());

        // Wrong opcode
        assert!(
            parse_pipe_instruction("4.file,3.100,24.application/octet-stream,6.STDOUT;").is_none()
        );

        // Invalid stream ID
        assert!(
            parse_pipe_instruction("4.pipe,3.abc,24.application/octet-stream,6.STDOUT;").is_none()
        );
    }

    #[test]
    fn test_malformed_blob_instruction() {
        // Invalid base64
        assert!(parse_blob_instruction("4.blob,3.100,4.!@#$;").is_none());

        // Missing data
        assert!(parse_blob_instruction("4.blob,3.100;").is_none());

        // Wrong opcode
        assert!(parse_blob_instruction("4.data,3.100,8.aGVsbG8=;").is_none());
    }

    #[test]
    fn test_malformed_end_instruction() {
        // Invalid stream ID
        assert!(parse_end_instruction("3.end,3.abc;").is_none());

        // Wrong opcode
        assert!(parse_end_instruction("3.fin,3.100;").is_none());
    }

    #[test]
    fn test_empty_blob_data() {
        // Empty data should work (empty base64 = empty string)
        let blob = format_pipe_blob(PIPE_STREAM_STDOUT, b"");
        let parsed = parse_blob_instruction(&blob);

        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap().data, b"");
    }
}

// ============================================================================
// Performance Edge Cases
// ============================================================================

mod performance_tests {
    use super::*;

    #[test]
    fn test_large_blob_roundtrip() {
        // 1MB of terminal data
        let large_data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();

        let blob = format_pipe_blob(PIPE_STREAM_STDOUT, &large_data);
        let parsed = parse_blob_instruction(&blob).unwrap();

        assert_eq!(parsed.data.len(), large_data.len());
        assert_eq!(parsed.data, large_data);
    }

    #[test]
    fn test_many_small_blobs() {
        // Simulate rapid keystroke echo (many small blobs)
        for i in 0..1000 {
            let data = format!("{}", (i % 10)).into_bytes();
            let blob = format_pipe_blob(PIPE_STREAM_STDOUT, &data);
            let parsed = parse_blob_instruction(&blob).unwrap();
            assert_eq!(parsed.data, data);
        }
    }

    #[test]
    fn test_binary_data_handling() {
        // Pure binary data (not valid UTF-8)
        let binary_data: Vec<u8> = (0..256).map(|i| i as u8).collect();

        let blob = format_pipe_blob(PIPE_STREAM_STDOUT, &binary_data);
        let parsed = parse_blob_instruction(&blob).unwrap();

        assert_eq!(parsed.data, binary_data);
    }
}

// ============================================================================
// Full Handler Simulation (No Docker Required)
// ============================================================================

mod handler_simulation_tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    /// Simulates a complete pipe-enabled session without needing a real SSH server
    ///
    /// This tests the full flow:
    /// 1. Handler sends pipe instruction
    /// 2. Handler sends terminal output as blobs
    /// 3. Client sends STDIN pipe
    /// 4. Client sends input blobs
    /// 5. Handler respects security settings
    /// 6. Session cleanup
    #[tokio::test]
    async fn test_full_pipe_session_simulation() {
        // Channels simulating Guacamole client <-> handler communication
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, mut from_client_rx) = mpsc::channel::<Bytes>(1024);

        // Parse connection parameters (simulating what the handler receives)
        let mut params = HashMap::new();
        params.insert("hostname".to_string(), "test-server".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("enable-pipe".to_string(), "true".to_string());
        params.insert("read-only".to_string(), "false".to_string());

        let security = HandlerSecuritySettings::from_params(&params);
        let enable_pipe = params
            .get("enable-pipe")
            .map(|v| v == "true")
            .unwrap_or(false);

        // === HANDLER SIDE (simulated) ===

        let mut pipe_manager = PipeStreamManager::new();

        // 1. Handler enables STDOUT pipe if requested
        if enable_pipe {
            let pipe_instr = pipe_manager.enable_stdout();
            to_client_tx.send(Bytes::from(pipe_instr)).await.unwrap();
        }

        // 2. Handler would send ready/size instructions (skipped for this test)

        // 3. Handler receives terminal output and sends as blob
        let terminal_output = b"\x1b[1;32mtest@server\x1b[0m:~$ ";
        if pipe_manager.is_stdout_enabled() {
            let blob = pipe_blob_bytes(PIPE_STREAM_STDOUT, terminal_output);
            to_client_tx.send(blob).await.unwrap();
        }

        // === CLIENT SIDE (simulated) ===

        // 4. Client receives pipe instruction
        let pipe_msg = to_client_rx.recv().await.unwrap();
        let pipe_str = String::from_utf8_lossy(&pipe_msg);
        let parsed_pipe = parse_pipe_instruction(&pipe_str).unwrap();
        assert_eq!(parsed_pipe.name, "STDOUT");

        // 5. Client receives terminal output blob
        let blob_msg = to_client_rx.recv().await.unwrap();
        let blob_str = String::from_utf8_lossy(&blob_msg);
        let parsed_blob = parse_blob_instruction(&blob_str).unwrap();
        assert_eq!(parsed_blob.data, terminal_output);

        // 6. Client opens STDIN pipe
        let stdin_pipe_instr = "4.pipe,3.101,24.application/octet-stream,5.STDIN;".to_string();
        from_client_tx
            .send(Bytes::from(stdin_pipe_instr))
            .await
            .unwrap();

        // 7. Client sends command through STDIN
        let user_command = b"ls -la\n";
        use base64::Engine;
        let b64_cmd = base64::engine::general_purpose::STANDARD.encode(user_command);
        let stdin_blob = format!("4.blob,3.101,{}.{};", b64_cmd.len(), b64_cmd);
        from_client_tx.send(Bytes::from(stdin_blob)).await.unwrap();

        // 8. Client closes STDIN
        from_client_tx
            .send(Bytes::from("3.end,3.101;"))
            .await
            .unwrap();

        // === BACK TO HANDLER SIDE ===

        // 9. Handler receives and processes client messages
        let msg1 = from_client_rx.recv().await.unwrap();
        let msg1_str = String::from_utf8_lossy(&msg1);
        if let Some(pipe_instr) = parse_pipe_instruction(&msg1_str) {
            if pipe_instr.name == PIPE_NAME_STDIN {
                pipe_manager.register_incoming(
                    pipe_instr.stream_id,
                    &pipe_instr.name,
                    &pipe_instr.mimetype,
                );
            }
        }
        assert!(pipe_manager.is_stdin_stream(101));

        let msg2 = from_client_rx.recv().await.unwrap();
        let msg2_str = String::from_utf8_lossy(&msg2);
        if let Some(blob_instr) = parse_blob_instruction(&msg2_str) {
            if pipe_manager.is_stdin_stream(blob_instr.stream_id) {
                // Security check before forwarding to SSH
                if !security.read_only {
                    // Would forward to SSH channel here
                    assert_eq!(blob_instr.data, user_command);
                }
            }
        }

        let msg3 = from_client_rx.recv().await.unwrap();
        let msg3_str = String::from_utf8_lossy(&msg3);
        if let Some(end_stream) = parse_end_instruction(&msg3_str) {
            if pipe_manager.is_stdin_stream(end_stream) {
                pipe_manager.close(PIPE_NAME_STDIN);
            }
        }

        // 10. Session ends, handler closes all pipes
        let end_instructions = pipe_manager.close_all();

        // Verify close_all returned end instruction (STDIN was still registered but we closed it earlier)
        // Note: close_all() closes open pipes but doesn't remove them from the map
        // The stream ID mapping still exists, but the pipe is marked as closed
        // This is correct behavior - we just verify end instructions were generated
        assert!(end_instructions.is_empty()); // STDIN was already closed manually

        for instr in end_instructions {
            to_client_tx.send(Bytes::from(instr)).await.unwrap();
        }
    }

    /// Test that read-only mode blocks STDIN properly in handler simulation
    #[tokio::test]
    async fn test_readonly_blocks_stdin_in_session() {
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, mut from_client_rx) = mpsc::channel::<Bytes>(1024);

        // Read-only session
        let mut params = HashMap::new();
        params.insert("enable-pipe".to_string(), "true".to_string());
        params.insert("read-only".to_string(), "true".to_string());

        let security = HandlerSecuritySettings::from_params(&params);
        let mut pipe_manager = PipeStreamManager::new();

        // Handler enables STDOUT (read-only doesn't block output)
        let pipe_instr = pipe_manager.enable_stdout();
        to_client_tx.send(Bytes::from(pipe_instr)).await.unwrap();

        // Client opens STDIN
        let stdin_pipe = "4.pipe,3.101,24.application/octet-stream,5.STDIN;";
        from_client_tx.send(Bytes::from(stdin_pipe)).await.unwrap();

        // Client sends dangerous command
        let dangerous_cmd = b"rm -rf /\n";
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(dangerous_cmd);
        let blob = format!("4.blob,3.101,{}.{};", b64.len(), b64);
        from_client_tx.send(Bytes::from(blob)).await.unwrap();

        // Handler side
        let msg1 = from_client_rx.recv().await.unwrap();
        let msg1_str = String::from_utf8_lossy(&msg1);
        if let Some(pipe_instr) = parse_pipe_instruction(&msg1_str) {
            pipe_manager.register_incoming(
                pipe_instr.stream_id,
                &pipe_instr.name,
                &pipe_instr.mimetype,
            );
        }

        let msg2 = from_client_rx.recv().await.unwrap();
        let msg2_str = String::from_utf8_lossy(&msg2);
        let mut forwarded_to_ssh = false;
        if let Some(blob_instr) = parse_blob_instruction(&msg2_str) {
            if pipe_manager.is_stdin_stream(blob_instr.stream_id) {
                // Security check - this is what the handler does
                if security.read_only {
                    // BLOCKED - do not forward
                    forwarded_to_ssh = false;
                } else {
                    forwarded_to_ssh = true;
                }
            }
        }

        // Verify dangerous command was NOT forwarded
        assert!(!forwarded_to_ssh, "Read-only should block STDIN data");

        // But STDOUT still works
        let pipe_msg = to_client_rx.recv().await.unwrap();
        assert!(String::from_utf8_lossy(&pipe_msg).contains("STDOUT"));
    }

    /// Test pipe with recording enabled (simulated)
    #[tokio::test]
    async fn test_pipe_with_recording_simulation() {
        let mut params = HashMap::new();
        params.insert("enable-pipe".to_string(), "true".to_string());
        params.insert(
            "recording-path".to_string(),
            "/tmp/test-session".to_string(),
        );

        // Simulated recording buffer
        let mut recorded_output: Vec<Vec<u8>> = Vec::new();
        let mut recorded_input: Vec<Vec<u8>> = Vec::new();

        let mut pipe_manager = PipeStreamManager::new();
        pipe_manager.enable_stdout();

        // Terminal output - would be recorded
        let outputs = [
            b"Welcome to test server\r\n".to_vec(),
            b"$ ".to_vec(),
            b"\x1b[32muser\x1b[0m@host:~$ ".to_vec(),
        ];

        for output in &outputs {
            // Send via pipe
            let _blob = format_pipe_blob(PIPE_STREAM_STDOUT, output);

            // Record output (what MultiFormatRecorder would do)
            recorded_output.push(output.clone());
        }

        // User input - would be recorded if recording_include_keys
        let inputs = [b"echo hello\n".to_vec(), b"ls -la\n".to_vec()];

        for input in &inputs {
            // Parse from blob (simulated receive)
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(input);
            let blob_str = format!("4.blob,3.101,{}.{};", b64.len(), b64);
            let parsed = parse_blob_instruction(&blob_str).unwrap();

            // Record input
            recorded_input.push(parsed.data);
        }

        // Verify recording captured everything
        assert_eq!(recorded_output.len(), 3);
        assert_eq!(recorded_input.len(), 2);
        assert_eq!(recorded_output[2], b"\x1b[32muser\x1b[0m@host:~$ ");
        assert_eq!(recorded_input[1], b"ls -la\n");
    }
}
