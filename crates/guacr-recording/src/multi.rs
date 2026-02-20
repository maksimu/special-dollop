// Multi-format session recorder
//
// Records to multiple formats simultaneously:
// - Guacamole .ses (if recording_path is set)
// - Asciicast (if asciicast_path is set, or recording_path for terminal protocols)
// - Typescript (if typescript_path is set)
// - Typescript timing file (companion .timing alongside typescript)

use bytes::Bytes;
use std::collections::HashMap;
use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::RecordingConfig;
use crate::ses::{GuacamoleSesRecorder, RecordingDirection, RecordingError};

/// Multi-format session recorder
///
/// Records to multiple formats simultaneously:
/// - Guacamole .ses (if recording_path is set)
/// - Asciicast (if asciicast_path is set, or recording_path for terminal protocols)
/// - Typescript (if typescript_path is set)
pub struct MultiFormatRecorder {
    ses_recorder: Option<GuacamoleSesRecorder>,
    asciicast_writer: Option<std::io::BufWriter<std::fs::File>>,
    typescript_writer: Option<std::io::BufWriter<std::fs::File>>,
    typescript_timing_writer: Option<std::io::BufWriter<std::fs::File>>,
    start_time: Instant,
    config: RecordingConfig,
    terminal_width: u16,
    terminal_height: u16,
}

impl MultiFormatRecorder {
    /// Create a new multi-format recorder
    pub fn new(
        config: &RecordingConfig,
        params: &HashMap<String, String>,
        protocol: &str,
        terminal_width: u16,
        terminal_height: u16,
    ) -> Result<Self, RecordingError> {
        let ses_recorder = if let Some(path) = config.get_ses_path(params, protocol) {
            Some(GuacamoleSesRecorder::new(&path, config)?)
        } else {
            None
        };

        let asciicast_writer = if let Some(path) = config.get_asciicast_path(params, protocol) {
            // Create directory if needed
            if let Some(parent) = path.parent() {
                if !parent.exists() && config.create_recording_path {
                    std::fs::create_dir_all(parent)?;
                }
            }

            // Check file exists
            if path.exists() && !config.recording_write_existing {
                return Err(RecordingError::FileExists(format!(
                    "File exists: {:?}",
                    path
                )));
            }

            let file = std::fs::File::create(&path)?;
            let mut writer = std::io::BufWriter::new(file);

            // Write asciicast v2 header
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let header = format!(
                r#"{{"version":2,"width":{},"height":{},"timestamp":{}}}"#,
                terminal_width, terminal_height, timestamp
            );
            writeln!(writer, "{}", header)?;
            writer.flush()?;

            Some(writer)
        } else {
            None
        };

        let (typescript_writer, typescript_timing_writer) =
            if let Some(path) = config.get_typescript_path(params, protocol) {
                // Create directory if needed
                if let Some(parent) = path.parent() {
                    if !parent.exists() && config.create_typescript_path {
                        std::fs::create_dir_all(parent)?;
                    }
                }

                // Check file exists
                if path.exists() && !config.typescript_write_existing {
                    return Err(RecordingError::FileExists(format!(
                        "File exists: {:?}",
                        path
                    )));
                }

                let file = std::fs::File::create(&path)?;
                let mut writer = std::io::BufWriter::new(file);

                // Write typescript header
                let now = chrono::Utc::now();
                writeln!(writer, "Script started on {}", now.format("%c"))?;
                writer.flush()?;

                // Create companion .timing file for scriptreplay
                let timing_path = path.with_extension("timing");
                let timing_file = std::fs::File::create(&timing_path)?;
                let timing_writer = std::io::BufWriter::new(timing_file);

                (Some(writer), Some(timing_writer))
            } else {
                (None, None)
            };

        Ok(Self {
            ses_recorder,
            asciicast_writer,
            typescript_writer,
            typescript_timing_writer,
            start_time: Instant::now(),
            config: config.clone(),
            terminal_width,
            terminal_height,
        })
    }

    /// Check if any recording is active
    pub fn is_active(&self) -> bool {
        self.ses_recorder.is_some()
            || self.asciicast_writer.is_some()
            || self.typescript_writer.is_some()
    }

    /// Record a Guacamole protocol instruction (.ses format)
    pub fn record_instruction(
        &mut self,
        direction: RecordingDirection,
        instruction: &Bytes,
    ) -> Result<(), RecordingError> {
        if let Some(ref mut recorder) = self.ses_recorder {
            recorder.record(direction, instruction)?;
        }
        Ok(())
    }

    /// Record terminal output (asciicast + typescript)
    pub fn record_output(&mut self, data: &[u8]) -> Result<(), RecordingError> {
        let elapsed = self.start_time.elapsed().as_secs_f64();

        // Asciicast format: [time, "o", "data"]
        if let Some(ref mut writer) = self.asciicast_writer {
            let data_str = String::from_utf8_lossy(data);
            // Escape JSON string
            let escaped = data_str
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            writeln!(writer, r#"[{:.6},"o","{}"]"#, elapsed, escaped)?;
        }

        // Typescript format: raw output
        if let Some(ref mut writer) = self.typescript_writer {
            writer.write_all(data)?;
        }

        // Typescript timing: elapsed_seconds byte_count
        if let Some(ref mut timing_writer) = self.typescript_timing_writer {
            writeln!(timing_writer, "{:.6} {}", elapsed, data.len())?;
        }

        Ok(())
    }

    /// Record keyboard input (asciicast, if recording_include_keys)
    pub fn record_input(&mut self, data: &[u8]) -> Result<(), RecordingError> {
        if !self.config.recording_include_keys {
            return Ok(());
        }

        let elapsed = self.start_time.elapsed().as_secs_f64();

        // Asciicast format: [time, "i", "data"]
        if let Some(ref mut writer) = self.asciicast_writer {
            let data_str = String::from_utf8_lossy(data);
            let escaped = data_str
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            writeln!(writer, r#"[{:.6},"i","{}"]"#, elapsed, escaped)?;
        }

        Ok(())
    }

    /// Record terminal resize
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<(), RecordingError> {
        self.terminal_width = cols;
        self.terminal_height = rows;

        let elapsed = self.start_time.elapsed().as_secs_f64();

        // Asciicast format: [time, "r", "COLSxROWS"]
        if let Some(ref mut writer) = self.asciicast_writer {
            writeln!(writer, r#"[{:.6},"r","{}x{}"]"#, elapsed, cols, rows)?;
        }

        Ok(())
    }

    /// Flush all writers
    pub fn flush(&mut self) -> Result<(), RecordingError> {
        if let Some(ref mut recorder) = self.ses_recorder {
            recorder.writer.flush()?;
        }
        if let Some(ref mut writer) = self.asciicast_writer {
            writer.flush()?;
        }
        if let Some(ref mut writer) = self.typescript_writer {
            writer.flush()?;
        }
        if let Some(ref mut writer) = self.typescript_timing_writer {
            writer.flush()?;
        }
        Ok(())
    }

    /// Finalize all recordings
    ///
    /// This method should be called explicitly when the session ends normally.
    /// The `Drop` implementation provides a safety net for abnormal termination.
    pub fn finalize(mut self) -> Result<(), RecordingError> {
        self.finalize_internal()
    }

    /// Internal finalization logic (used by both finalize and Drop)
    fn finalize_internal(&mut self) -> Result<(), RecordingError> {
        // Finalize .ses
        if let Some(recorder) = self.ses_recorder.take() {
            recorder.finalize()?;
        }

        // Finalize asciicast (no footer needed)
        if let Some(mut writer) = self.asciicast_writer.take() {
            writer.flush()?;
        }

        // Finalize typescript
        if let Some(mut writer) = self.typescript_writer.take() {
            let now = chrono::Utc::now();
            writeln!(writer, "\nScript done on {}", now.format("%c"))?;
            writer.flush()?;
        }

        // Finalize typescript timing
        if let Some(mut writer) = self.typescript_timing_writer.take() {
            writer.flush()?;
        }

        Ok(())
    }
}

/// RAII: Ensure recording files are flushed even on early return or panic
impl Drop for MultiFormatRecorder {
    fn drop(&mut self) {
        // Only finalize if not already done (writers still present)
        if self.ses_recorder.is_some()
            || self.asciicast_writer.is_some()
            || self.typescript_writer.is_some()
        {
            if let Err(e) = self.finalize_internal() {
                // Can't propagate error from Drop, just log it
                log::warn!("Recording finalization failed during drop: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_multi_format_recorder_asciicast() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "test-session".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record some output
        recorder.record_output(b"Hello, World!\r\n").unwrap();
        recorder.record_output(b"$ ").unwrap();

        recorder.finalize().unwrap();

        // Check asciicast file exists and has valid format
        let cast_path = tmp_dir.path().join("test-session.cast");
        assert!(cast_path.exists(), "Asciicast file should exist");

        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Check header line
        assert!(
            content.starts_with("{\"version\":2"),
            "Should have v2 header"
        );
        assert!(content.contains("\"width\":80"), "Should have width");
        assert!(content.contains("\"height\":24"), "Should have height");

        // Check output events
        assert!(
            content.contains(r#","o","Hello, World!"#),
            "Should contain output event"
        );
    }

    #[test]
    fn test_multi_format_recorder_ses_and_asciicast() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "dual-format".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record instruction (for .ses) - raw Guacamole protocol
        let instruction = Bytes::from("4.size,1.0,3.800,3.600;");
        recorder
            .record_instruction(RecordingDirection::ServerToClient, &instruction)
            .unwrap();

        // Record output (for .cast)
        recorder.record_output(b"Login successful\r\n").unwrap();

        recorder.finalize().unwrap();

        // Both files should exist
        let ses_path = tmp_dir.path().join("dual-format.ses");
        let cast_path = tmp_dir.path().join("dual-format.cast");

        assert!(ses_path.exists(), ".ses file should exist");
        assert!(cast_path.exists(), ".cast file should exist");

        // Verify .ses content - raw Guacamole protocol format
        let ses_content = std::fs::read_to_string(&ses_path).unwrap();
        assert!(
            ses_content.contains("4.size,1.0,3.800,3.600;"),
            ".ses should have raw instruction without timestamp prefix"
        );

        // Verify .cast content - asciicast v2 format
        let cast_content = std::fs::read_to_string(&cast_path).unwrap();
        assert!(
            cast_content.contains("Login successful"),
            ".cast should have output"
        );
    }

    #[test]
    fn test_asciicast_input_recording() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "input-test".to_string());
        params.insert("recording-include-keys".to_string(), "true".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record input (only if recording_include_keys is true)
        recorder.record_input(b"ls -la").unwrap();
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("input-test.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Input events use "i" type
        assert!(
            content.contains(r#","i","ls -la"#),
            "Should contain input event"
        );
    }

    #[test]
    fn test_asciicast_input_not_recorded_by_default() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "no-input".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        assert!(
            !config.recording_include_keys,
            "Keys should NOT be included by default"
        );

        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record output and input
        recorder.record_output(b"prompt$ ").unwrap();
        recorder.record_input(b"secret_password").unwrap(); // Should be ignored
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("no-input.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Should have output but NOT input
        assert!(content.contains("prompt$"), "Should have output");
        assert!(
            !content.contains("secret_password"),
            "Should NOT have input (security)"
        );
    }

    #[test]
    fn test_asciicast_resize_event() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "resize-test".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record resize
        recorder.record_resize(120, 40).unwrap();
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("resize-test.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Resize events use "r" type
        assert!(
            content.contains(r#","r","120x40"#),
            "Should contain resize event"
        );
    }

    #[test]
    fn test_typescript_recording_with_timing() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "typescript-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("typescript-name".to_string(), "typescript-test".to_string());
        params.insert("create-typescript-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        assert!(
            config.is_typescript_enabled(),
            "Typescript should be enabled"
        );

        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record output
        recorder.record_output(b"Script output here\r\n").unwrap();
        recorder.finalize().unwrap();

        let ts_path = tmp_dir.path().join("typescript-test");
        assert!(ts_path.exists(), "Typescript file should exist");

        let content = std::fs::read_to_string(&ts_path).unwrap();
        assert!(
            content.contains("Script output here"),
            "Should have raw output"
        );
        assert!(content.contains("Script done on"), "Should have footer");

        // Check timing file exists
        let timing_path = tmp_dir.path().join("typescript-test.timing");
        assert!(timing_path.exists(), "Timing file should exist");

        let timing_content = std::fs::read_to_string(&timing_path).unwrap();
        assert!(
            !timing_content.is_empty(),
            "Timing file should have content"
        );
        // Format: elapsed_seconds byte_count
        let line = timing_content.lines().next().unwrap();
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(parts.len(), 2, "Timing line should have 2 fields");
        parts[0]
            .parse::<f64>()
            .expect("First field should be elapsed seconds");
        parts[1]
            .parse::<usize>()
            .expect("Second field should be byte count");
    }

    #[test]
    fn test_asciicast_can_be_parsed_as_json() {
        // Verify asciicast output is valid JSON (NDJSON format)
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "json-test".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record some output
        recorder.record_output(b"Hello, World!\r\n").unwrap();
        recorder.record_output(b"$ ls -la\r\n").unwrap();
        recorder.record_resize(120, 40).unwrap();
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("json-test.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Each line should be valid JSON
        for (i, line) in content.lines().enumerate() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "Line {} should be valid JSON: {}",
                i + 1,
                line
            );

            if i == 0 {
                // First line is header
                let header = parsed.unwrap();
                assert_eq!(header["version"], 2, "Should be asciicast v2");
                assert_eq!(header["width"], 80);
                assert_eq!(header["height"], 24);
            } else {
                // Other lines are events: [time, type, data]
                let event = parsed.unwrap();
                assert!(event.is_array(), "Event should be array");
                let arr = event.as_array().unwrap();
                assert_eq!(arr.len(), 3, "Event should have 3 elements");
                assert!(arr[0].is_f64(), "First element should be timestamp");
                assert!(arr[1].is_string(), "Second element should be event type");
            }
        }
    }

    #[test]
    fn test_recording_empty_session() {
        // Edge case: session with no recorded content
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "empty".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Immediately finalize without recording anything
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("empty.cast");
        assert!(cast_path.exists(), "File should exist even if empty");

        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Should have at least the header
        assert!(!content.is_empty(), "Should have header");
        let header: serde_json::Value =
            serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
    }

    #[test]
    fn test_full_ssh_session_recording() {
        // Simulate a complete SSH session recording
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "ssh-session".to_string());
        params.insert("recording-include-keys".to_string(), "true".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("hostname".to_string(), "192.168.1.100".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Simulate session flow
        recorder
            .record_instruction(
                RecordingDirection::ServerToClient,
                &Bytes::from("4.size,1.0,2.80,2.24;"),
            )
            .unwrap();

        recorder.record_output(b"login: ").unwrap();
        recorder.record_input(b"testuser").unwrap();
        recorder.record_output(b"testuser\r\n").unwrap();
        recorder.record_output(b"Password: ").unwrap();
        recorder.record_input(b"secret123").unwrap();
        recorder.record_output(b"\r\n").unwrap();
        recorder.record_output(b"testuser@host:~$ ").unwrap();
        recorder.record_input(b"ls -la").unwrap();
        recorder.record_output(b"ls -la\r\n").unwrap();
        recorder.record_output(b"total 32\r\n").unwrap();
        recorder.record_resize(120, 40).unwrap();
        recorder.record_input(b"exit").unwrap();
        recorder.record_output(b"exit\r\nlogout\r\n").unwrap();

        recorder.finalize().unwrap();

        // Verify files exist and have content
        let ses_path = tmp_dir.path().join("ssh-session.ses");
        let cast_path = tmp_dir.path().join("ssh-session.cast");

        assert!(ses_path.exists(), ".ses file should exist");
        assert!(cast_path.exists(), ".cast file should exist");

        let ses_content = std::fs::read_to_string(&ses_path).unwrap();
        let cast_content = std::fs::read_to_string(&cast_path).unwrap();

        assert!(
            ses_content.contains("4.size,"),
            ".ses should have size instruction"
        );

        assert!(
            cast_content.contains("testuser@host"),
            ".cast should have prompt"
        );
        assert!(cast_content.contains("ls -la"), ".cast should have command");
        assert!(
            cast_content.contains("120x40"),
            ".cast should have resize event"
        );
        assert!(
            cast_content.contains(r#","i","#),
            ".cast should have input events"
        );
    }
}
