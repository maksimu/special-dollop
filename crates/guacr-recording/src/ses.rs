// Guacamole .ses format recorder
//
// Records Guacamole protocol instructions in the native format compatible with
// guacenc (video encoding) and guaclog (text logging) utilities.
//
// Format: Raw Guacamole protocol instructions written sequentially, one per line.
// Each instruction is in standard Guacamole protocol format:
// `LENGTH.OPCODE,LENGTH.ARG1,LENGTH.ARG2,...;`
//
// There is NO timestamp prefix. The Apache Guacamole recording format is simply
// the raw protocol stream. Client-to-server instructions (mouse, key) can
// optionally be recorded with their timestamp parameter for playback.

use async_trait::async_trait;
use bytes::Bytes;
use std::io::Write;
use std::time::Instant;

use crate::config::{find_unique_path, RecordingConfig};
use crate::helpers::{extract_opcode, inject_timestamp, is_drawing_instruction};

/// Recording direction (for .ses format)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingDirection {
    /// Server to client (output)
    ServerToClient = 0,
    /// Client to server (input)
    ClientToServer = 1,
}

/// Recording error
#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path creation failed: {0}")]
    PathCreation(String),

    #[error("File exists and overwrite not allowed: {0}")]
    FileExists(String),

    #[error("Recording disabled")]
    Disabled,
}

/// Session recorder trait for all formats
///
/// Implemented by protocol handlers to record session data.
#[async_trait]
pub trait SessionRecorder: Send + Sync {
    /// Record a Guacamole protocol instruction
    fn record_instruction(
        &mut self,
        direction: RecordingDirection,
        instruction: &Bytes,
    ) -> Result<(), RecordingError>;

    /// Record terminal output (for asciicast/typescript)
    fn record_output(&mut self, data: &[u8]) -> Result<(), RecordingError>;

    /// Record keyboard input (if recording_include_keys is true)
    fn record_input(&mut self, data: &[u8]) -> Result<(), RecordingError>;

    /// Record terminal resize
    fn record_resize(&mut self, cols: u16, rows: u16) -> Result<(), RecordingError>;

    /// Flush all buffers
    fn flush(&mut self) -> Result<(), RecordingError>;

    /// Finalize recording (close files, upload, etc.)
    async fn finalize(self: Box<Self>) -> Result<(), RecordingError>;
}

/// Guacamole .ses format recorder
///
/// Records Guacamole protocol instructions in the native format
/// compatible with guacenc (video encoding) and guaclog (text logging) utilities.
///
/// **Format**: Raw Guacamole protocol instructions written sequentially.
/// Each instruction is in standard Guacamole protocol format:
/// `LENGTH.OPCODE,LENGTH.ARG1,LENGTH.ARG2,...;`
///
/// Timing information is embedded via `sync` instructions in the stream.
/// The recording is essentially a replay of all server-to-client instructions.
///
/// **Note**: Unlike some documentation suggests, there is NO timestamp prefix.
/// The Apache Guacamole recording format is simply the raw protocol stream.
/// Client-to-server instructions (mouse, key) can optionally be recorded with
/// their timestamp parameter for playback.
pub struct GuacamoleSesRecorder {
    pub(crate) writer: std::io::BufWriter<std::fs::File>,
    start_time: Instant,
    config: RecordingConfig,
    last_sync_ms: u64,
}

impl GuacamoleSesRecorder {
    /// Create a new .ses recorder
    pub fn new(path: &std::path::Path, config: &RecordingConfig) -> Result<Self, RecordingError> {
        // Check if directory exists, create if allowed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if config.create_recording_path {
                    std::fs::create_dir_all(parent)?;
                } else {
                    return Err(RecordingError::PathCreation(format!(
                        "Directory does not exist: {:?}",
                        parent
                    )));
                }
            }
        }

        // Resolve to a unique path if the file exists and overwrite is not allowed
        let actual_path = if path.exists() && !config.recording_write_existing {
            find_unique_path(path, 255).ok_or_else(|| {
                RecordingError::FileExists(format!("All candidate paths exhausted for: {:?}", path))
            })?
        } else {
            path.to_path_buf()
        };

        let file = std::fs::File::create(&actual_path)?;
        let writer = std::io::BufWriter::new(file);

        Ok(Self {
            writer,
            start_time: Instant::now(),
            config: config.clone(),
            last_sync_ms: 0,
        })
    }

    /// Record an instruction to the .ses file
    ///
    /// Instructions are written in raw Guacamole protocol format.
    /// For server-to-client, instructions are written as-is.
    /// For client-to-server (mouse, key), a timestamp argument is appended
    /// so that guacenc knows when input events occurred.
    pub fn record(
        &mut self,
        direction: RecordingDirection,
        instruction: &Bytes,
    ) -> Result<(), RecordingError> {
        let instr_str = String::from_utf8_lossy(instruction);

        // Check exclusion filters
        if self.config.recording_exclude_output && direction == RecordingDirection::ServerToClient {
            // Filter all drawing instructions but keep sync (timing) and size (dimensions)
            let opcode = extract_opcode(&instr_str);
            if is_drawing_instruction(&instr_str) && opcode != "sync" && opcode != "size" {
                return Ok(());
            }
        }

        if self.config.recording_exclude_mouse
            && direction == RecordingDirection::ClientToServer
            && instr_str.starts_with("5.mouse,")
        {
            return Ok(()); // Skip mouse events
        }

        if !self.config.recording_include_keys
            && direction == RecordingDirection::ClientToServer
            && instr_str.starts_with("3.key,")
        {
            return Ok(()); // Skip key events unless explicitly enabled
        }

        // For client-to-server mouse/key, inject a timestamp argument
        // so guacenc can replay input events with correct timing.
        if direction == RecordingDirection::ClientToServer {
            let opcode = extract_opcode(&instr_str);
            if opcode == "mouse" || opcode == "key" {
                let timestamped = inject_timestamp(&instr_str);
                self.writer.write_all(timestamped.as_bytes())?;
                if !timestamped.ends_with('\n') {
                    self.writer.write_all(b"\n")?;
                }
                return self.maybe_flush();
            }
        }

        // Write the raw instruction (Guacamole protocol format)
        self.writer.write_all(instruction)?;

        // Ensure newline for readability (matches guacd behavior)
        if !instruction.ends_with(b"\n") {
            self.writer.write_all(b"\n")?;
        }

        self.maybe_flush()
    }

    /// Flush writer periodically (~every 5 seconds)
    fn maybe_flush(&mut self) -> Result<(), RecordingError> {
        let timestamp_ms = self.start_time.elapsed().as_millis() as u64;
        if timestamp_ms - self.last_sync_ms > 5000 {
            self.writer.flush()?;
            self.last_sync_ms = timestamp_ms;
        }
        Ok(())
    }

    /// Flush and close the recording
    ///
    /// This method should be called explicitly when the session ends normally.
    /// The `Drop` implementation provides a safety net for abnormal termination.
    pub fn finalize(mut self) -> Result<(), RecordingError> {
        self.flush_internal()
    }

    /// Internal flush logic
    fn flush_internal(&mut self) -> Result<(), RecordingError> {
        self.writer.flush()?;
        Ok(())
    }
}

/// RAII: Ensure recording file is flushed even on early return or panic
impl Drop for GuacamoleSesRecorder {
    fn drop(&mut self) {
        // Attempt to flush any remaining buffered data
        if let Err(e) = self.writer.flush() {
            log::warn!("Failed to flush .ses recording during drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    #[test]
    fn test_recording_direction() {
        assert_eq!(RecordingDirection::ServerToClient as u8, 0);
        assert_eq!(RecordingDirection::ClientToServer as u8, 1);
    }

    #[test]
    fn test_ses_recorder_creates_file() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("test.ses");

        let config = RecordingConfig::default();
        let recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();
        recorder.finalize().unwrap();

        assert!(ses_path.exists());
    }

    #[test]
    fn test_ses_recorder_writes_raw_instruction() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("test.ses");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Write a test instruction (Guacamole protocol format)
        let instruction = Bytes::from("4.sync,13.1234567890123,1.0;");
        recorder
            .record(RecordingDirection::ServerToClient, &instruction)
            .unwrap();
        recorder.finalize().unwrap();

        // Read and verify
        let mut content = String::new();
        std::fs::File::open(&ses_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        // Format: raw Guacamole protocol instruction (NO timestamp prefix)
        assert!(
            content.contains("4.sync,13.1234567890123,1.0;"),
            "Should contain raw instruction without timestamp prefix"
        );
        // Should NOT have old format with timestamp prefix
        assert!(
            !content.starts_with("0."),
            "Should NOT start with timestamp prefix"
        );
    }

    #[test]
    fn test_ses_recorder_records_multiple_instructions() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("test.ses");

        // Enable key recording to test client-to-server direction
        let config = RecordingConfig {
            recording_include_keys: true,
            ..Default::default()
        };

        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Server to client - size instruction
        let instr1 = Bytes::from("4.size,1.0,3.800,3.600;");
        recorder
            .record(RecordingDirection::ServerToClient, &instr1)
            .unwrap();

        // Server to client - sync instruction (provides timing)
        let instr2 = Bytes::from("4.sync,13.1234567890123,1.0;");
        recorder
            .record(RecordingDirection::ServerToClient, &instr2)
            .unwrap();

        // Client to server - key event (with timestamp as per protocol)
        let instr3 = Bytes::from("3.key,2.65,1.1,13.1234567890200;");
        recorder
            .record(RecordingDirection::ClientToServer, &instr3)
            .unwrap();

        recorder.finalize().unwrap();

        let mut content = String::new();
        std::fs::File::open(&ses_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        // Verify raw instructions are recorded (Guacamole protocol format)
        assert!(
            content.contains("4.size,1.0,3.800,3.600;"),
            "Should contain size instruction"
        );
        assert!(
            content.contains("4.sync,13.1234567890123,1.0;"),
            "Should contain sync instruction"
        );
        // Key instruction gets a timestamp injected
        assert!(
            content.contains("3.key,2.65,1.1,13.1234567890200"),
            "Should contain key instruction"
        );
    }

    #[test]
    fn test_ses_recorder_filters_keys_by_default() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("test.ses");

        // Default config - keys NOT included (security: don't record passwords)
        let config = RecordingConfig::default();
        assert!(!config.recording_include_keys);

        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Server to client (should be recorded)
        let instr1 = Bytes::from("4.size,1.0,3.800,3.600;");
        recorder
            .record(RecordingDirection::ServerToClient, &instr1)
            .unwrap();

        // Client to server key event (should be FILTERED for security)
        let instr2 = Bytes::from("3.key,2.65,1.1,13.1234567890200;");
        recorder
            .record(RecordingDirection::ClientToServer, &instr2)
            .unwrap();

        recorder.finalize().unwrap();

        let mut content = String::new();
        std::fs::File::open(&ses_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        // Only the first instruction should be recorded
        assert!(
            content.contains("4.size,1.0,3.800,3.600;"),
            "Should contain size instruction"
        );
        assert!(
            !content.contains("3.key,"),
            "Should NOT contain key event (filtered for security)"
        );

        // Only 1 non-empty line
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "Should have exactly 1 recorded instruction (key filtered)"
        );
    }

    #[test]
    fn test_ses_format_compatible_with_guacenc() {
        // Test that output matches what guacenc/guaclog expect
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("session.ses");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Typical recording instructions
        let instructions = [
            // Size instruction (defines display dimensions)
            Bytes::from("4.size,1.0,4.1024,3.768;"),
            // Name instruction (optional metadata)
            Bytes::from("4.name,1.0,12.Test Session;"),
            // Sync instruction (timing marker - timestamp in milliseconds)
            Bytes::from("4.sync,10.1000000000,1.1;"),
            // Another sync to show time passing
            Bytes::from("4.sync,10.1000000100,1.2;"),
        ];

        for instr in &instructions {
            recorder
                .record(RecordingDirection::ServerToClient, instr)
                .unwrap();
        }
        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&ses_path).unwrap();

        // Each instruction should be on its own line, raw format
        for instr in &instructions {
            let instr_str = String::from_utf8_lossy(instr);
            assert!(
                content.contains(instr_str.as_ref()),
                "Recording should contain: {}",
                instr_str
            );
        }

        // Verify file can be parsed as Guacamole protocol
        // Each line should start with a length-prefixed opcode
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            assert!(
                line.contains('.'),
                "Line should contain Guacamole protocol separator"
            );
            assert!(
                line.ends_with(';'),
                "Line should end with semicolon: {}",
                line
            );
        }
    }

    #[test]
    fn test_ses_recording_can_be_parsed() {
        // Verify that recorded .ses files can be parsed back as valid Guacamole protocol
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("parseable.ses");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Record typical session instructions
        let instructions = vec![
            "4.size,1.0,4.1024,3.768;",
            "4.name,1.0,7.Session;",
            "4.sync,13.1000000000000,1.0;",
            "3.img,1.0,1.2,9.image/png,1.0,1.0;",
            "4.sync,13.1000000100000,1.1;",
        ];

        for instr in &instructions {
            recorder
                .record(RecordingDirection::ServerToClient, &Bytes::from(*instr))
                .unwrap();
        }
        recorder.finalize().unwrap();

        // Parse the file and verify each instruction
        let content = std::fs::read_to_string(&ses_path).unwrap();

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }

            // Parse Guacamole instruction format: LENGTH.OPCODE,LENGTH.ARG,...;
            assert!(
                line.ends_with(';'),
                "Instruction should end with semicolon: {}",
                line
            );

            // Extract opcode (first element after length prefix)
            let parts: Vec<&str> = line.split(',').collect();
            assert!(!parts.is_empty(), "Should have at least opcode");

            // First part should be LENGTH.OPCODE
            let first = parts[0];
            assert!(
                first.contains('.'),
                "Should have length-prefixed opcode: {}",
                first
            );

            // Verify length prefix is numeric
            let dot_pos = first.find('.').unwrap();
            let length_str = &first[..dot_pos];
            assert!(
                length_str.parse::<usize>().is_ok(),
                "Length prefix should be numeric: {}",
                length_str
            );
        }
    }

    #[test]
    fn test_ses_recording_binary_safe() {
        // Guacamole instructions can contain binary blob data
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("binary.ses");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Simulate a blob instruction with base64 data (how Guacamole handles binary)
        let blob_instr = "4.blob,1.1,16.SGVsbG8gV29ybGQh;"; // "Hello World!" in base64
        recorder
            .record(RecordingDirection::ServerToClient, &Bytes::from(blob_instr))
            .unwrap();

        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&ses_path).unwrap();
        assert!(
            content.contains("SGVsbG8gV29ybGQh"),
            "Base64 data should be preserved"
        );
    }

    #[test]
    fn test_recording_exclude_output_filters_all_drawing() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("exclude-output.ses");

        let config = RecordingConfig {
            recording_exclude_output: true,
            ..Default::default()
        };
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Drawing instructions -- should all be filtered
        let drawing_instrs = vec![
            Bytes::from("3.img,1.0,9.image/png,1.0,1.0;"),
            Bytes::from("4.blob,1.1,16.SGVsbG8gV29ybGQh;"),
            Bytes::from("3.end,1.1;"),
            Bytes::from("4.copy,1.0,1.0,1.0,3.100,3.100,1.1,1.0,1.0;"),
            Bytes::from("4.rect,1.0,1.0,1.0,3.100,3.100;"),
            Bytes::from("5.cfill,1.0,1.0,3.255,1.0,1.0,3.255;"),
            Bytes::from("6.cursor,1.0,1.0,1.0,3.100,3.100,2.10,2.10;"),
            Bytes::from("5.audio,1.0,9.audio/ogg;"),
        ];

        // Non-drawing instructions -- should be kept
        let kept_instrs = vec![
            Bytes::from("4.size,1.0,3.800,3.600;"),
            Bytes::from("4.sync,13.1234567890123,1.0;"),
            Bytes::from("4.name,1.0,7.Session;"),
        ];

        for instr in &drawing_instrs {
            recorder
                .record(RecordingDirection::ServerToClient, instr)
                .unwrap();
        }
        for instr in &kept_instrs {
            recorder
                .record(RecordingDirection::ServerToClient, instr)
                .unwrap();
        }

        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&ses_path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();

        // Should only have the 3 non-drawing instructions
        assert_eq!(lines.len(), 3, "Should only have non-drawing instructions");
        assert!(content.contains("4.size,"));
        assert!(content.contains("4.sync,"));
        assert!(content.contains("4.name,"));

        // Drawing instructions should NOT be present
        assert!(!content.contains("3.img,"));
        assert!(!content.contains("4.blob,"));
        assert!(!content.contains("6.cursor,"));
    }

    #[test]
    fn test_timestamp_injection_on_mouse_key() {
        let tmp_dir = TempDir::new().unwrap();
        let ses_path = tmp_dir.path().join("timestamp.ses");

        let config = RecordingConfig {
            recording_include_keys: true,
            ..Default::default()
        };
        let mut recorder = GuacamoleSesRecorder::new(&ses_path, &config).unwrap();

        // Mouse instruction (client-to-server)
        let mouse = Bytes::from("5.mouse,3.100,3.200,1.0;");
        recorder
            .record(RecordingDirection::ClientToServer, &mouse)
            .unwrap();

        // Key instruction (client-to-server)
        let key = Bytes::from("3.key,2.65,1.1;");
        recorder
            .record(RecordingDirection::ClientToServer, &key)
            .unwrap();

        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&ses_path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();

        assert_eq!(lines.len(), 2);

        // Both should have extra timestamp argument appended
        for line in &lines {
            // Original had N commas; timestamped has N+1
            assert!(line.ends_with(';'));
            // The timestamp is the last argument, a large number
            let last_arg = line.rsplit(',').next().unwrap();
            // Format: LEN.TIMESTAMP;
            assert!(
                last_arg.contains('.'),
                "Should have length-prefixed timestamp: {}",
                last_arg
            );
        }
    }
}
