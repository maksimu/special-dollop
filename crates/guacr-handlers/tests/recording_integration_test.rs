// Integration test for session recording
//
// This test verifies that recordings are actually created during handler execution.

use guacr_handlers::{MultiFormatRecorder, RecordingConfig};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_recording_files_are_created() {
    // Create temporary directory for recordings
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().to_string_lossy().to_string();

    // Set up connection parameters with recording enabled
    let mut params = HashMap::new();
    params.insert("recording-path".to_string(), recording_path.clone());
    params.insert("recording-name".to_string(), "test-session".to_string());
    params.insert("create-recording-path".to_string(), "true".to_string());
    params.insert("username".to_string(), "testuser".to_string());
    params.insert("hostname".to_string(), "testhost".to_string());

    // Parse recording configuration
    let config = RecordingConfig::from_params(&params);

    // Verify configuration is enabled
    assert!(config.is_enabled(), "Recording should be enabled");
    assert!(config.is_ses_enabled(), "SES recording should be enabled");
    assert!(
        config.is_asciicast_enabled(),
        "Asciicast recording should be enabled"
    );

    // Create recorder (simulating what SSH/RDP/Telnet handlers do)
    let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24)
        .expect("Failed to create recorder");

    // Simulate some session activity
    // Terminal output goes to .cast (asciicast)
    recorder
        .record_output(b"Welcome to test server\r\n")
        .unwrap();
    recorder.record_output(b"$ ").unwrap();
    recorder.record_output(b"ls -la\r\n").unwrap();
    recorder.record_output(b"total 32\r\n").unwrap();
    recorder
        .record_output(b"drwxr-xr-x 4 user user 4096 Jan 19 14:30 .\r\n")
        .unwrap();

    // Protocol instructions go to .ses (Guacamole format)
    use bytes::Bytes;
    use guacr_handlers::RecordingDirection;

    // Record a size instruction (server to client)
    let size_instr = Bytes::from("4.size,1.0,2.80,2.24;");
    recorder
        .record_instruction(RecordingDirection::ServerToClient, &size_instr)
        .unwrap();

    // Record a sync instruction (timing marker)
    let sync_instr = Bytes::from("4.sync,4.1000,1.0;");
    recorder
        .record_instruction(RecordingDirection::ServerToClient, &sync_instr)
        .unwrap();

    // Finalize recording (flushes and closes files)
    recorder.finalize().expect("Failed to finalize recording");

    // Verify recording files were created
    let ses_path = PathBuf::from(&recording_path).join("test-session.ses");
    let cast_path = PathBuf::from(&recording_path).join("test-session.cast");

    assert!(
        ses_path.exists(),
        "SES recording file should exist at {:?}",
        ses_path
    );
    assert!(
        cast_path.exists(),
        "Asciicast recording file should exist at {:?}",
        cast_path
    );

    // Verify files have content
    let ses_content = fs::read_to_string(&ses_path).unwrap();
    let cast_content = fs::read_to_string(&cast_path).unwrap();

    assert!(
        !ses_content.is_empty(),
        "SES file should have content (Guacamole protocol instructions)"
    );
    assert!(
        !cast_content.is_empty(),
        "Asciicast file should have content (terminal output)"
    );

    // Verify asciicast format
    assert!(
        cast_content.starts_with("{\"version\":2"),
        "Asciicast should have v2 header"
    );
    assert!(
        cast_content.contains("\"width\":80"),
        "Asciicast should have width"
    );
    assert!(
        cast_content.contains("\"height\":24"),
        "Asciicast should have height"
    );
    assert!(
        cast_content.contains("Welcome to test server"),
        "Asciicast should contain output"
    );

    println!("✓ Recording files created successfully:");
    println!("  - SES: {} bytes", ses_content.len());
    println!("  - Cast: {} bytes", cast_content.len());
}

#[test]
fn test_recording_with_template_filename() {
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().to_string_lossy().to_string();

    let mut params = HashMap::new();
    params.insert("recording-path".to_string(), recording_path.clone());
    params.insert(
        "recording-name".to_string(),
        "session-${GUAC_USERNAME}-${GUAC_HOSTNAME}".to_string(),
    );
    params.insert("create-recording-path".to_string(), "true".to_string());
    params.insert("username".to_string(), "alice".to_string());
    params.insert("hostname".to_string(), "server1".to_string());

    let config = RecordingConfig::from_params(&params);
    let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

    recorder.record_output(b"Test output\r\n").unwrap();
    recorder.finalize().unwrap();

    // Verify files with expanded template names
    let ses_path = PathBuf::from(&recording_path).join("session-alice-server1.ses");
    let cast_path = PathBuf::from(&recording_path).join("session-alice-server1.cast");

    assert!(
        ses_path.exists(),
        "SES file with template name should exist"
    );
    assert!(
        cast_path.exists(),
        "Cast file with template name should exist"
    );

    println!("✓ Template filename expanded correctly:");
    println!("  - {}", ses_path.display());
    println!("  - {}", cast_path.display());
}

#[test]
fn test_recording_disabled_by_default() {
    let mut params = HashMap::new();
    params.insert("username".to_string(), "testuser".to_string());
    params.insert("hostname".to_string(), "testhost".to_string());
    // Note: NO recording-path parameter

    let config = RecordingConfig::from_params(&params);

    assert!(
        !config.is_enabled(),
        "Recording should be disabled without recording-path"
    );
    assert!(!config.is_ses_enabled(), "SES recording should be disabled");

    println!("✓ Recording correctly disabled when no recording-path specified");
}

#[test]
fn test_recording_with_keys_disabled_by_default() {
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().to_string_lossy().to_string();

    let mut params = HashMap::new();
    params.insert("recording-path".to_string(), recording_path.clone());
    params.insert("recording-name".to_string(), "test-keys".to_string());
    params.insert("create-recording-path".to_string(), "true".to_string());
    // Note: recording-include-keys NOT set (should default to false for security)

    let config = RecordingConfig::from_params(&params);
    let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

    // Record output (should be recorded)
    recorder.record_output(b"Password: ").unwrap();

    // Record input (should be IGNORED for security - no recording-include-keys)
    recorder.record_input(b"secret_password123").unwrap();

    recorder.finalize().unwrap();

    // Read the asciicast file
    let cast_path = PathBuf::from(&recording_path).join("test-keys.cast");
    let cast_content = fs::read_to_string(&cast_path).unwrap();

    // Should have output but NOT input
    assert!(cast_content.contains("Password:"), "Should have output");
    assert!(
        !cast_content.contains("secret_password"),
        "Should NOT have input (security)"
    );
    assert!(
        !cast_content.contains("\"i\""),
        "Should have no input events"
    );

    println!("✓ Keystroke recording correctly disabled by default (security)");
}

#[test]
fn test_recording_with_keys_explicitly_enabled() {
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().to_string_lossy().to_string();

    let mut params = HashMap::new();
    params.insert("recording-path".to_string(), recording_path.clone());
    params.insert(
        "recording-name".to_string(),
        "test-keys-enabled".to_string(),
    );
    params.insert("create-recording-path".to_string(), "true".to_string());
    params.insert("recording-include-keys".to_string(), "true".to_string()); // Explicitly enable

    let config = RecordingConfig::from_params(&params);
    assert!(config.recording_include_keys, "Keys should be enabled");

    let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

    recorder.record_output(b"$ ").unwrap();
    recorder.record_input(b"echo hello").unwrap(); // Should be recorded now

    recorder.finalize().unwrap();

    let cast_path = PathBuf::from(&recording_path).join("test-keys-enabled.cast");
    let cast_content = fs::read_to_string(&cast_path).unwrap();

    // Should have both output AND input
    assert!(cast_content.contains("\"o\""), "Should have output events");
    assert!(cast_content.contains("\"i\""), "Should have input events");
    assert!(
        cast_content.contains("echo hello"),
        "Should have recorded input"
    );

    println!("✓ Keystroke recording works when explicitly enabled");
}

#[test]
fn test_recording_directory_creation() {
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().join("nested/deep/directory");
    let recording_path_str = recording_path.to_string_lossy().to_string();

    // Directory doesn't exist yet
    assert!(
        !recording_path.exists(),
        "Directory should not exist initially"
    );

    let mut params = HashMap::new();
    params.insert("recording-path".to_string(), recording_path_str.clone());
    params.insert("recording-name".to_string(), "test".to_string());
    params.insert("create-recording-path".to_string(), "true".to_string()); // Auto-create

    let config = RecordingConfig::from_params(&params);
    let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

    recorder.record_output(b"test\r\n").unwrap();
    recorder.finalize().unwrap();

    // Directory should now exist
    assert!(recording_path.exists(), "Directory should be auto-created");
    assert!(
        recording_path.join("test.ses").exists(),
        "SES file should exist"
    );
    assert!(
        recording_path.join("test.cast").exists(),
        "Cast file should exist"
    );

    println!("✓ Recording directory auto-creation works");
}

#[test]
fn test_multiple_protocol_recordings() {
    let temp_dir = TempDir::new().unwrap();
    let recording_path = temp_dir.path().to_string_lossy().to_string();

    // Test SSH recording
    {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), recording_path.clone());
        params.insert("recording-name".to_string(), "ssh-session".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();
        recorder.record_output(b"SSH output\r\n").unwrap();
        recorder.finalize().unwrap();
    }

    // Test RDP recording
    {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), recording_path.clone());
        params.insert("recording-name".to_string(), "rdp-session".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "rdp", 1920, 1080).unwrap();
        recorder.record_output(b"RDP output\r\n").unwrap();
        recorder.finalize().unwrap();
    }

    // Test Telnet recording
    {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), recording_path.clone());
        params.insert("recording-name".to_string(), "telnet-session".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "telnet", 80, 24).unwrap();
        recorder.record_output(b"Telnet output\r\n").unwrap();
        recorder.finalize().unwrap();
    }

    // Verify all recordings exist
    let ssh_ses = PathBuf::from(&recording_path).join("ssh-session.ses");
    let rdp_ses = PathBuf::from(&recording_path).join("rdp-session.ses");
    let telnet_ses = PathBuf::from(&recording_path).join("telnet-session.ses");

    assert!(ssh_ses.exists(), "SSH recording should exist");
    assert!(rdp_ses.exists(), "RDP recording should exist");
    assert!(telnet_ses.exists(), "Telnet recording should exist");

    println!("✓ Multiple protocol recordings work correctly");
}
