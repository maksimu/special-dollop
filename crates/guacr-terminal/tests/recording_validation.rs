// Comprehensive recording validation tests
// Based on KCM patches and real-world recording requirements

use guacr_terminal::{AsciicastHeader, DualFormatRecorder};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

/// Test asciicast v2 format compliance
#[test]
fn test_asciicast_format_compliance() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("test.cast");

    let mut env = HashMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("SHELL".to_string(), "/bin/bash".to_string());

    let mut recorder = DualFormatRecorder::new(
        Some(&cast_path),
        None, // No .ses file for this test
        80,
        24,
        Some(env),
    )
    .unwrap();

    // Record some output
    recorder.record_output(b"Hello, World!\n").unwrap();
    recorder.record_output(b"Line 2\n").unwrap();

    // Finalize to flush
    recorder.finalize().unwrap();

    // Read and validate the file
    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // First line should be valid JSON header
    assert!(!lines.is_empty(), "Recording file is empty");
    let header: AsciicastHeader = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header.version, 2, "Should be asciicast v2");
    assert_eq!(header.width, 80);
    assert_eq!(header.height, 24);
    assert!(header.env.is_some(), "Environment should be recorded");

    // Subsequent lines should be events [time, type, data]
    for line in &lines[1..] {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(event.is_array(), "Event should be array");
        let arr = event.as_array().unwrap();
        assert_eq!(arr.len(), 3, "Event should have [time, type, data]");

        // Validate timestamp is a number
        assert!(
            arr[0].is_f64() || arr[0].is_u64(),
            "Timestamp should be numeric"
        );

        // Validate type is 'o' for output
        assert_eq!(
            arr[1].as_str().unwrap(),
            "o",
            "Type should be 'o' for output"
        );

        // Validate data is a string
        assert!(arr[2].is_string(), "Data should be string");
    }
}

/// Test Unicode handling (KCM-427: Fix playback of recordings containing Unicode)
#[test]
fn test_unicode_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("unicode.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Record various Unicode characters
    recorder.record_output("Hello 世界\n".as_bytes()).unwrap(); // Chinese
    recorder.record_output("Привет мир\n".as_bytes()).unwrap(); // Russian
    recorder
        .record_output("مرحبا بالعالم\n".as_bytes())
        .unwrap(); // Arabic
    recorder
        .record_output("🚀 Emoji test\n".as_bytes())
        .unwrap(); // Emoji

    recorder.finalize().unwrap();

    // Read and validate
    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Check that Unicode is preserved
    assert!(
        content.contains("世界"),
        "Chinese characters should be preserved"
    );
    assert!(
        content.contains("Привет"),
        "Russian characters should be preserved"
    );
    assert!(
        content.contains("مرحبا"),
        "Arabic characters should be preserved"
    );
    assert!(content.contains("🚀"), "Emoji should be preserved");

    // Validate each event line is valid JSON
    for line in &lines[1..] {
        serde_json::from_str::<serde_json::Value>(line).expect("Each line should be valid JSON");
    }
}

/// Test short session recording (KCM-403: Fix short session recording playback)
#[test]
fn test_short_session_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("short.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Very short session - just one line
    recorder.record_output(b"quick command\n").unwrap();
    recorder.finalize().unwrap();

    // Should still create valid recording
    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert!(lines.len() >= 2, "Should have header + at least one event");

    // Validate header
    let header: AsciicastHeader = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header.version, 2);

    // Validate event
    let event: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!(event.is_array());
}

/// Test Guacamole .ses format
#[test]
fn test_ses_format() {
    let temp_dir = TempDir::new().unwrap();
    let ses_path = temp_dir.path().join("test.ses");

    let mut recorder = DualFormatRecorder::new(
        None, // No asciicast
        Some(&ses_path),
        80,
        24,
        None,
    )
    .unwrap();

    // .ses format records Guacamole protocol instructions, not raw terminal output
    // We need to use record_server_to_client or record_client_to_server
    use bytes::Bytes;

    // Record a size instruction
    let size_instr = Bytes::from("4.size,1.0,2.80,2.24;");
    recorder.record_server_to_client(&size_instr).unwrap();

    // Record an img instruction
    let img_instr = Bytes::from("3.img,1.0,9.image/png,1.0,1.0,4.test;");
    recorder.record_server_to_client(&img_instr).unwrap();

    recorder.finalize().unwrap();

    // .ses format is text-based (timestamp.direction.instruction)
    let content = fs::read_to_string(&ses_path).unwrap();
    assert!(!content.is_empty(), ".ses file should have content");

    // Check format: timestamp.direction.instruction
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2, "Should have at least 2 instructions");

    for line in lines {
        let parts: Vec<&str> = line.splitn(3, '.').collect();
        assert_eq!(
            parts.len(),
            3,
            "Each line should have timestamp.direction.instruction"
        );

        // Validate timestamp is numeric
        parts[0]
            .parse::<u64>()
            .expect("Timestamp should be numeric");

        // Validate direction is 0 or 1
        let direction = parts[1].parse::<u8>().unwrap();
        assert!(direction <= 1, "Direction should be 0 or 1");
    }
}

/// Test dual format recording (both asciicast and .ses)
#[test]
fn test_dual_format_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("dual.cast");
    let ses_path = temp_dir.path().join("dual.ses");

    let mut recorder =
        DualFormatRecorder::new(Some(&cast_path), Some(&ses_path), 80, 24, None).unwrap();

    use bytes::Bytes;

    // Record terminal output (goes to asciicast)
    recorder.record_output(b"test\n").unwrap();

    // Record protocol instruction (goes to .ses)
    let instr = Bytes::from("4.sync,4.1234;");
    recorder.record_server_to_client(&instr).unwrap();

    recorder.finalize().unwrap();

    // Both files should exist
    assert!(cast_path.exists(), "asciicast file should exist");
    assert!(ses_path.exists(), ".ses file should exist");

    // Both should have content
    assert!(
        fs::metadata(&cast_path).unwrap().len() > 0,
        "asciicast should have content"
    );
    assert!(
        fs::metadata(&ses_path).unwrap().len() > 0,
        ".ses file should have content"
    );
}

/// Test terminal resize events
#[test]
fn test_resize_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("resize.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    recorder.record_output(b"before resize\n").unwrap();
    recorder.record_resize(100, 30).unwrap();
    recorder.record_output(b"after resize\n").unwrap();
    recorder.finalize().unwrap();

    let content = fs::read_to_string(&cast_path).unwrap();

    // Should contain resize event
    assert!(
        content.contains("\"r\""),
        "Should contain resize event type"
    );
}

/// Test input recording
#[test]
fn test_input_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("input.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    recorder.record_input(b"ls -la\n").unwrap();
    recorder.record_output(b"file1.txt\nfile2.txt\n").unwrap();
    recorder.finalize().unwrap();

    let content = fs::read_to_string(&cast_path).unwrap();

    // Should contain both input ('i') and output ('o') events
    assert!(content.contains("\"i\""), "Should contain input event");
    assert!(content.contains("\"o\""), "Should contain output event");
}

/// Test multiple output events
#[test]
fn test_multiple_output_events() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("multiple.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    recorder.record_output(b"step 1\n").unwrap();
    recorder.record_output(b"step 2\n").unwrap();
    recorder.record_output(b"step 3\n").unwrap();
    recorder.finalize().unwrap();

    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Should have header + 3 events
    assert!(lines.len() >= 4, "Should have header + 3 events");
}

/// Test large output recording
#[test]
fn test_large_output_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("large.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Record 1000 lines of output
    for i in 0..1000 {
        recorder
            .record_output(format!("Line {}\n", i).as_bytes())
            .unwrap();
    }
    recorder.finalize().unwrap();

    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Should have header + 1000 events
    assert!(lines.len() >= 1001, "Should have header + 1000 events");

    // Validate all lines are valid JSON
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).expect("Each line should be valid JSON");
    }
}

/// Test timing accuracy
#[test]
fn test_timing_accuracy() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("timing.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Record events with known delays
    recorder.record_output(b"event 1\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    recorder.record_output(b"event 2\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    recorder.record_output(b"event 3\n").unwrap();
    recorder.finalize().unwrap();

    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    // Parse timestamps
    let mut timestamps = Vec::new();
    for line in &lines[1..] {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        let timestamp = event[0].as_f64().unwrap();
        timestamps.push(timestamp);
    }

    // Verify timestamps are increasing
    for i in 1..timestamps.len() {
        assert!(
            timestamps[i] > timestamps[i - 1],
            "Timestamps should be monotonically increasing"
        );
    }

    // Verify approximate delays (within 50ms tolerance)
    if timestamps.len() >= 2 {
        let delay1 = timestamps[1] - timestamps[0];
        assert!(
            (0.05..=0.15).contains(&delay1),
            "First delay should be ~100ms, got {:.3}s",
            delay1
        );
    }
}

/// Test binary data handling
#[test]
fn test_binary_data_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("binary.cast");

    let mut recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Record binary data (non-UTF8)
    let binary_data = vec![0xFF, 0xFE, 0xFD, 0x00, 0x01, 0x02];
    recorder.record_output(&binary_data).unwrap();
    recorder.finalize().unwrap();

    // File should still be created and valid
    assert!(cast_path.exists());
    let metadata = fs::metadata(&cast_path).unwrap();
    assert!(metadata.len() > 0);
}

/// Test empty recording
#[test]
fn test_empty_recording() {
    let temp_dir = TempDir::new().unwrap();
    let cast_path = temp_dir.path().join("empty.cast");

    let recorder = DualFormatRecorder::new(Some(&cast_path), None, 80, 24, None).unwrap();

    // Don't record anything, just finalize
    recorder.finalize().unwrap();

    // Should still create valid file with just header
    let content = fs::read_to_string(&cast_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(lines.len(), 1, "Empty recording should have just header");

    // Validate header
    let header: AsciicastHeader = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header.version, 2);
}
