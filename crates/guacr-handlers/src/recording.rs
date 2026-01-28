// Shared recording configuration and traits for all protocol handlers
//
// Supports multiple recording formats:
// 1. Guacamole .ses format (legacy, for playback in Guacamole player)
// 2. Asciicast v2 format (for terminal sessions, asciinema player)
// 3. Typescript format (legacy text logging)
//
// Reference: KCM's recording parameters from settings.c

use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Recording format types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingFormat {
    /// Guacamole .ses format - binary protocol recording
    /// Compatible with Guacamole's guaclog/guacplay utilities
    GuacamoleSes,

    /// Asciicast v2 format - JSON terminal recording
    /// Compatible with asciinema player
    Asciicast,

    /// Typescript format - legacy text logging
    /// Simple text file with timing information
    Typescript,
}

impl RecordingFormat {
    /// Get file extension for this format
    pub fn extension(&self) -> &str {
        match self {
            RecordingFormat::GuacamoleSes => "ses",
            RecordingFormat::Asciicast => "cast",
            RecordingFormat::Typescript => "typescript",
        }
    }
}

/// Recording configuration parsed from connection parameters
///
/// Based on KCM's GUAC_DB_CLIENT_ARGS from settings.c:
/// - recording-path: Directory to store recordings
/// - recording-name: Filename template (supports variables)
/// - recording-exclude-output: Exclude graphical output
/// - recording-exclude-mouse: Exclude mouse movements
/// - recording-include-keys: Include keyboard input (security risk!)
/// - create-recording-path: Create directory if missing
/// - recording-write-existing: Allow overwriting existing files
///
/// Plus typescript-specific parameters:
/// - typescript-path: Directory for typescript files
/// - typescript-name: Filename for typescript
/// - create-typescript-path: Create directory if missing
/// - typescript-write-existing: Allow overwriting
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    // ========================================================================
    // Guacamole Recording (.ses format)
    // ========================================================================
    /// Path to store Guacamole recording files
    pub recording_path: Option<String>,

    /// Recording filename (supports templates: ${GUAC_DATE}, ${GUAC_TIME}, ${GUAC_USERNAME})
    pub recording_name: String,

    /// Create recording path if it doesn't exist
    pub create_recording_path: bool,

    /// Allow overwriting existing recording files
    pub recording_write_existing: bool,

    /// Exclude graphical output from recording (reduces file size)
    pub recording_exclude_output: bool,

    /// Exclude mouse movements from recording
    pub recording_exclude_mouse: bool,

    /// Include key events in recording
    /// WARNING: May capture passwords - default is false
    pub recording_include_keys: bool,

    // ========================================================================
    // Typescript Recording (text format)
    // ========================================================================
    /// Path to store typescript files
    pub typescript_path: Option<String>,

    /// Typescript filename
    pub typescript_name: String,

    /// Create typescript path if it doesn't exist
    pub create_typescript_path: bool,

    /// Allow overwriting existing typescript files
    pub typescript_write_existing: bool,

    // ========================================================================
    // Asciicast Recording (.cast format)
    // ========================================================================
    /// Path to store asciicast files (uses recording_path if not set)
    pub asciicast_path: Option<String>,

    /// Asciicast filename (uses recording_name with .cast extension if not set)
    pub asciicast_name: Option<String>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            recording_path: None,
            recording_name: "recording".to_string(),
            create_recording_path: false,
            recording_write_existing: false,
            recording_exclude_output: false,
            recording_exclude_mouse: false,
            recording_include_keys: false,
            typescript_path: None,
            typescript_name: "typescript".to_string(),
            create_typescript_path: false,
            typescript_write_existing: false,
            asciicast_path: None,
            asciicast_name: None,
        }
    }
}

impl RecordingConfig {
    /// Parse recording configuration from connection parameters
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        Self {
            recording_path: params.get("recording-path").cloned(),
            recording_name: params
                .get("recording-name")
                .cloned()
                .unwrap_or_else(|| "recording".to_string()),
            create_recording_path: parse_bool(params.get("create-recording-path")),
            recording_write_existing: parse_bool(params.get("recording-write-existing")),
            recording_exclude_output: parse_bool(params.get("recording-exclude-output")),
            recording_exclude_mouse: parse_bool(params.get("recording-exclude-mouse")),
            recording_include_keys: parse_bool(params.get("recording-include-keys")),
            typescript_path: params.get("typescript-path").cloned(),
            typescript_name: params
                .get("typescript-name")
                .cloned()
                .unwrap_or_else(|| "typescript".to_string()),
            create_typescript_path: parse_bool(params.get("create-typescript-path")),
            typescript_write_existing: parse_bool(params.get("typescript-write-existing")),
            asciicast_path: params.get("asciicast-path").cloned(),
            asciicast_name: params.get("asciicast-name").cloned(),
        }
    }

    /// Check if any recording is enabled
    pub fn is_enabled(&self) -> bool {
        self.recording_path.is_some()
            || self.typescript_path.is_some()
            || self.asciicast_path.is_some()
    }

    /// Check if Guacamole .ses recording is enabled
    pub fn is_ses_enabled(&self) -> bool {
        self.recording_path.is_some()
    }

    /// Check if typescript recording is enabled
    pub fn is_typescript_enabled(&self) -> bool {
        self.typescript_path.is_some()
    }

    /// Check if asciicast recording is enabled
    pub fn is_asciicast_enabled(&self) -> bool {
        self.asciicast_path.is_some() || self.recording_path.is_some()
    }

    /// Expand filename template with actual values
    ///
    /// Supports:
    /// - ${GUAC_DATE}: Current date (YYYYMMDD)
    /// - ${GUAC_TIME}: Current time (HHMMSS)
    /// - ${GUAC_USERNAME}: Username from connection
    /// - ${GUAC_HOSTNAME}: Hostname from connection
    /// - ${GUAC_PROTOCOL}: Protocol name
    pub fn expand_filename(
        template: &str,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> String {
        let now = chrono::Utc::now();
        let date = now.format("%Y%m%d").to_string();
        let time = now.format("%H%M%S").to_string();

        let username = params.get("username").cloned().unwrap_or_default();
        let hostname = params.get("hostname").cloned().unwrap_or_default();

        template
            .replace("${GUAC_DATE}", &date)
            .replace("${GUAC_TIME}", &time)
            .replace("${GUAC_USERNAME}", &username)
            .replace("${GUAC_HOSTNAME}", &hostname)
            .replace("${GUAC_PROTOCOL}", protocol)
    }

    /// Get full path for Guacamole .ses recording
    pub fn get_ses_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        self.recording_path.as_ref().map(|dir| {
            let filename = Self::expand_filename(&self.recording_name, params, protocol);
            PathBuf::from(dir).join(format!("{}.ses", filename))
        })
    }

    /// Get full path for typescript recording
    pub fn get_typescript_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        self.typescript_path.as_ref().map(|dir| {
            let filename = Self::expand_filename(&self.typescript_name, params, protocol);
            PathBuf::from(dir).join(filename)
        })
    }

    /// Get full path for asciicast recording
    pub fn get_asciicast_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        let base_path = self
            .asciicast_path
            .as_ref()
            .or(self.recording_path.as_ref())?;
        let name = self.asciicast_name.as_ref().unwrap_or(&self.recording_name);
        let filename = Self::expand_filename(name, params, protocol);
        Some(PathBuf::from(base_path).join(format!("{}.cast", filename)))
    }
}

/// Recording direction (for .ses format)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingDirection {
    /// Server to client (output)
    ServerToClient = 0,
    /// Client to server (input)
    ClientToServer = 1,
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

/// Guacamole .guac format recorder
///
/// Records Guacamole protocol instructions in the native .guac format
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
    writer: std::io::BufWriter<std::fs::File>,
    start_time: Instant,
    config: RecordingConfig,
    last_sync_ms: u64,
}

impl GuacamoleSesRecorder {
    /// Create a new .guac recorder
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

        // Check if file exists
        if path.exists() && !config.recording_write_existing {
            return Err(RecordingError::FileExists(format!(
                "File exists: {:?}",
                path
            )));
        }

        let file = std::fs::File::create(path)?;
        let writer = std::io::BufWriter::new(file);

        Ok(Self {
            writer,
            start_time: Instant::now(),
            config: config.clone(),
            last_sync_ms: 0,
        })
    }

    /// Record an instruction to the .guac file
    ///
    /// Instructions are written in raw Guacamole protocol format.
    /// For server-to-client, instructions are written as-is.
    /// For client-to-server (mouse, key), the timestamp parameter is used for timing.
    pub fn record(
        &mut self,
        direction: RecordingDirection,
        instruction: &Bytes,
    ) -> Result<(), RecordingError> {
        // Only record server-to-client by default (like guacd does)
        // Client-to-server is optionally recorded for input replay

        let instr_str = String::from_utf8_lossy(instruction);

        // Check exclusion filters
        if self.config.recording_exclude_output && direction == RecordingDirection::ServerToClient {
            // Skip graphical output if excluded (but keep sync for timing)
            if instr_str.starts_with("3.img,") {
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

        // Write the raw instruction (Guacamole protocol format)
        // The instruction should already include the terminating semicolon
        self.writer.write_all(instruction)?;

        // Ensure newline for readability (optional but matches guacd behavior)
        if !instruction.ends_with(b"\n") {
            self.writer.write_all(b"\n")?;
        }

        // Periodic flush every ~5 seconds based on sync timing
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
            log::warn!("Failed to flush .guac recording during drop: {}", e);
        }
    }
}

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

        let typescript_writer = if let Some(path) = config.get_typescript_path(params, protocol) {
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

            Some(writer)
        } else {
            None
        };

        Ok(Self {
            ses_recorder,
            asciicast_writer,
            typescript_writer,
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

fn parse_bool(value: Option<&String>) -> bool {
    value.map(|v| v == "true" || v == "1").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    #[test]
    fn test_recording_config_default() {
        let config = RecordingConfig::default();
        assert!(!config.is_enabled());
        assert_eq!(config.recording_name, "recording");
        assert!(!config.recording_include_keys);
    }

    #[test]
    fn test_recording_config_from_params() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/tmp/recordings".to_string());
        params.insert(
            "recording-name".to_string(),
            "session-${GUAC_DATE}".to_string(),
        );
        params.insert("recording-include-keys".to_string(), "true".to_string());
        params.insert("create-recording-path".to_string(), "1".to_string());

        let config = RecordingConfig::from_params(&params);

        assert!(config.is_enabled());
        assert!(config.is_ses_enabled());
        assert_eq!(config.recording_path, Some("/tmp/recordings".to_string()));
        assert!(config.recording_include_keys);
        assert!(config.create_recording_path);
    }

    #[test]
    fn test_filename_expansion() {
        let mut params = HashMap::new();
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("hostname".to_string(), "server.example.com".to_string());

        let template = "${GUAC_USERNAME}-${GUAC_HOSTNAME}-${GUAC_PROTOCOL}";
        let result = RecordingConfig::expand_filename(template, &params, "ssh");

        assert_eq!(result, "testuser-server.example.com-ssh");
    }

    #[test]
    fn test_recording_format_extension() {
        assert_eq!(RecordingFormat::GuacamoleSes.extension(), "ses");
        assert_eq!(RecordingFormat::Asciicast.extension(), "cast");
        assert_eq!(RecordingFormat::Typescript.extension(), "typescript");
    }

    #[test]
    fn test_recording_direction() {
        assert_eq!(RecordingDirection::ServerToClient as u8, 0);
        assert_eq!(RecordingDirection::ClientToServer as u8, 1);
    }

    // ========================================================================
    // Guacamole .ses Format Tests
    // ========================================================================

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
    fn test_guac_recorder_writes_raw_instruction() {
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("test.guac");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

        // Write a test instruction (Guacamole protocol format)
        let instruction = Bytes::from("4.sync,13.1234567890123,1.0;");
        recorder
            .record(RecordingDirection::ServerToClient, &instruction)
            .unwrap();
        recorder.finalize().unwrap();

        // Read and verify
        let mut content = String::new();
        std::fs::File::open(&guac_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        // Format: raw Guacamole protocol instruction (NO timestamp prefix)
        assert!(
            content.contains("4.sync,13.1234567890123,1.0;"),
            "Should contain raw instruction without timestamp prefix"
        );
        // Should NOT have our old format with timestamp prefix
        assert!(
            !content.starts_with("0."),
            "Should NOT start with timestamp prefix"
        );
    }

    #[test]
    fn test_guac_recorder_records_multiple_instructions() {
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("test.guac");

        // Enable key recording to test client-to-server direction
        let config = RecordingConfig {
            recording_include_keys: true,
            ..Default::default()
        };

        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

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
        std::fs::File::open(&guac_path)
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
        assert!(
            content.contains("3.key,2.65,1.1,13.1234567890200;"),
            "Should contain key instruction"
        );

        // The file should have 3 lines (one per instruction)
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            3,
            "Should have exactly 3 recorded instructions"
        );
    }

    #[test]
    fn test_guac_recorder_filters_keys_by_default() {
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("test.guac");

        // Default config - keys NOT included (security: don't record passwords)
        let config = RecordingConfig::default();
        assert!(!config.recording_include_keys);

        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

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
        std::fs::File::open(&guac_path)
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
    fn test_guac_format_compatible_with_guacenc() {
        // Test that output matches what guacenc/guaclog expect
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("session.guac");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

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

        let content = std::fs::read_to_string(&guac_path).unwrap();

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
            // Should match pattern: DIGIT.OPCODE,... or similar
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

    // ========================================================================
    // Asciicast .cast Format Tests
    // ========================================================================

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
    fn test_multi_format_recorder_guac_and_asciicast() {
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

        // Record instruction (for .guac/.ses) - raw Guacamole protocol
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

        assert!(ses_path.exists(), ".ses/.guac file should exist");
        assert!(cast_path.exists(), ".cast file should exist");

        // Verify .guac/.ses content - raw Guacamole protocol format
        let ses_content = std::fs::read_to_string(&ses_path).unwrap();
        assert!(
            ses_content.contains("4.size,1.0,3.800,3.600;"),
            ".guac should have raw instruction without timestamp prefix"
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
        // Note: NOT setting recording-include-keys
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

    // ========================================================================
    // Typescript Format Tests
    // ========================================================================

    #[test]
    fn test_typescript_recording() {
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
    }

    // ========================================================================
    // Recording Path Tests
    // ========================================================================

    #[test]
    fn test_get_recording_paths() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert("recording-name".to_string(), "session".to_string());
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("hostname".to_string(), "host.example.com".to_string());

        let config = RecordingConfig::from_params(&params);

        let ses_path = config.get_ses_path(&params, "ssh");
        assert_eq!(ses_path, Some(PathBuf::from("/recordings/session.ses")));

        let cast_path = config.get_asciicast_path(&params, "ssh");
        assert_eq!(cast_path, Some(PathBuf::from("/recordings/session.cast")));
    }

    #[test]
    fn test_recording_path_with_template() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert(
            "recording-name".to_string(),
            "${GUAC_USERNAME}-${GUAC_HOSTNAME}".to_string(),
        );
        params.insert("username".to_string(), "admin".to_string());
        params.insert("hostname".to_string(), "server1".to_string());

        let config = RecordingConfig::from_params(&params);

        let ses_path = config.get_ses_path(&params, "ssh");
        assert_eq!(
            ses_path,
            Some(PathBuf::from("/recordings/admin-server1.ses"))
        );
    }

    // ========================================================================
    // Playback / Parsing Tests
    // ========================================================================

    #[test]
    fn test_guac_recording_can_be_parsed() {
        // Verify that recorded .guac files can be parsed back as valid Guacamole protocol
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("parseable.guac");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

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
        let content = std::fs::read_to_string(&guac_path).unwrap();

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
    fn test_asciicast_timestamps_are_monotonic() {
        // Timestamps should increase monotonically
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "timing-test".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Record multiple outputs with small delays
        for i in 0..5 {
            recorder
                .record_output(format!("Line {}\r\n", i).as_bytes())
                .unwrap();
        }
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("timing-test.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        let mut last_time = -1.0_f64;
        for (i, line) in content.lines().enumerate() {
            if i == 0 {
                continue;
            } // Skip header

            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            let time = event[0].as_f64().unwrap();

            assert!(
                time >= last_time,
                "Timestamps should be monotonic: {} >= {}",
                time,
                last_time
            );
            assert!(time >= 0.0, "Timestamps should be non-negative");

            last_time = time;
        }
    }

    // ========================================================================
    // Realistic Session Simulation Tests
    // ========================================================================

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

        // 1. Initial size/ready
        recorder
            .record_instruction(
                RecordingDirection::ServerToClient,
                &Bytes::from("4.size,1.0,2.80,2.24;"),
            )
            .unwrap();

        // 2. Login prompt
        recorder.record_output(b"login: ").unwrap();

        // 3. User types username
        recorder.record_input(b"testuser").unwrap();
        recorder.record_output(b"testuser\r\n").unwrap();

        // 4. Password prompt
        recorder.record_output(b"Password: ").unwrap();

        // 5. User types password (recorded because include-keys=true)
        recorder.record_input(b"secret123").unwrap();
        recorder.record_output(b"\r\n").unwrap();

        // 6. Shell prompt
        recorder.record_output(b"testuser@host:~$ ").unwrap();

        // 7. User runs command
        recorder.record_input(b"ls -la").unwrap();
        recorder.record_output(b"ls -la\r\n").unwrap();
        recorder.record_output(b"total 32\r\n").unwrap();
        recorder
            .record_output(b"drwxr-xr-x 4 testuser testuser 4096 Dec  3 10:00 .\r\n")
            .unwrap();

        // 8. Terminal resize
        recorder.record_resize(120, 40).unwrap();

        // 9. Logout
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

        // .ses should have the size instruction
        assert!(
            ses_content.contains("4.size,"),
            ".ses should have size instruction"
        );

        // .cast should have session data
        assert!(
            cast_content.contains("testuser@host"),
            ".cast should have prompt"
        );
        assert!(cast_content.contains("ls -la"), ".cast should have command");
        assert!(
            cast_content.contains("120x40"),
            ".cast should have resize event"
        );

        // .cast should have input events (because include-keys=true)
        assert!(
            cast_content.contains(r#","i","#),
            ".cast should have input events"
        );
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_recording_handles_special_characters() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "special-chars".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Test common special characters that need escaping in JSON
        // Note: Control characters like ESC (\x1b) are passed through as-is
        // since asciicast players handle raw terminal output
        let special_outputs = [
            b"Tab:\there\r\n".as_slice(),
            b"Newlines:\n\n\n".as_slice(),
            b"Quotes: \"hello\" 'world'\r\n".as_slice(),
            b"Backslash: C:\\Users\\test\r\n".as_slice(),
            b"Unicode: \xc3\xa9\xc3\xa8\xc3\xa0 \xe2\x9c\x93\r\n".as_slice(), // éèà ✓
        ];

        for output in special_outputs {
            recorder.record_output(output).unwrap();
        }
        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("special-chars.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // Verify file is still valid JSON
        for (i, line) in content.lines().enumerate() {
            if i == 0 {
                continue;
            }
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "Line {} should still be valid JSON after special chars: {}",
                i + 1,
                line
            );
        }

        // Verify escaping worked for JSON-sensitive chars
        assert!(content.contains("\\t"), "Tab should be escaped");
        assert!(content.contains("\\n"), "Newline should be escaped");
        assert!(content.contains("\\\""), "Quote should be escaped");
        assert!(content.contains("\\\\"), "Backslash should be escaped");

        // Unicode should be preserved
        assert!(
            content.contains("éèà") || content.contains("\\u"),
            "Unicode should be preserved or escaped"
        );
    }

    #[test]
    fn test_recording_handles_ansi_escape_codes() {
        // ANSI escape codes are common in terminal output - test they're preserved
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "ansi-codes".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // ANSI escape sequences for colors
        // ESC[31m = red, ESC[0m = reset
        let ansi_output = b"\x1b[31mred text\x1b[0m normal\r\n";
        recorder.record_output(ansi_output).unwrap();

        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("ansi-codes.cast");
        let content = std::fs::read_to_string(&cast_path).unwrap();

        // The content should exist (even if ESC is escaped somehow)
        assert!(
            content.contains("red text"),
            "Text content should be preserved"
        );
        assert!(
            content.contains("normal"),
            "Text after reset should be preserved"
        );
    }

    #[test]
    fn test_recording_handles_long_output() {
        let tmp_dir = TempDir::new().unwrap();

        let mut params = HashMap::new();
        params.insert(
            "recording-path".to_string(),
            tmp_dir.path().to_string_lossy().to_string(),
        );
        params.insert("recording-name".to_string(), "long-output".to_string());
        params.insert("create-recording-path".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);
        let mut recorder = MultiFormatRecorder::new(&config, &params, "ssh", 80, 24).unwrap();

        // Generate a very long line (simulating large file output)
        let long_line = "x".repeat(10_000);
        recorder.record_output(long_line.as_bytes()).unwrap();

        // Also test many small outputs
        for i in 0..1000 {
            recorder
                .record_output(format!("{}\r\n", i).as_bytes())
                .unwrap();
        }

        recorder.finalize().unwrap();

        let cast_path = tmp_dir.path().join("long-output.cast");
        assert!(cast_path.exists());

        let metadata = std::fs::metadata(&cast_path).unwrap();
        assert!(
            metadata.len() > 10_000,
            "File should be larger than long line"
        );

        // Verify it's still valid
        let content = std::fs::read_to_string(&cast_path).unwrap();
        let line_count = content.lines().count();
        assert!(line_count > 1000, "Should have many lines: {}", line_count);
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
    fn test_guac_recording_binary_safe() {
        // Guacamole instructions can contain binary blob data
        let tmp_dir = TempDir::new().unwrap();
        let guac_path = tmp_dir.path().join("binary.guac");

        let config = RecordingConfig::default();
        let mut recorder = GuacamoleSesRecorder::new(&guac_path, &config).unwrap();

        // Simulate a blob instruction with base64 data (how Guacamole handles binary)
        let blob_instr = "4.blob,1.1,16.SGVsbG8gV29ybGQh;"; // "Hello World!" in base64
        recorder
            .record(RecordingDirection::ServerToClient, &Bytes::from(blob_instr))
            .unwrap();

        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&guac_path).unwrap();
        assert!(
            content.contains("SGVsbG8gV29ybGQh"),
            "Base64 data should be preserved"
        );
    }
}
