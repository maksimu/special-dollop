// Session recording formats: asciicast v2 and guacd .ses
// Supports multiple transports: files, ZeroMQ, async channels, etc.

use crate::Result;
use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

// S3 support requires s3 feature and aws-sdk-s3 dependency
// #[cfg(feature = "s3")]
// use log;

/// asciicast v2 header
#[derive(Debug, Serialize, Deserialize)]
pub struct AsciicastHeader {
    pub version: u8,
    pub width: u16,
    pub height: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_time_limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Event types for asciicast v2
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventType {
    Output, // 'o' - terminal output
    Input,  // 'i' - keyboard input
    Marker, // 'm' - marker/breakpoint
    Resize, // 'r' - terminal resize
}

impl EventType {
    pub fn as_char(&self) -> char {
        match self {
            EventType::Output => 'o',
            EventType::Input => 'i',
            EventType::Marker => 'm',
            EventType::Resize => 'r',
        }
    }
}

/// asciicast v2 session recorder
///
/// Records terminal sessions in asciicast v2 format (newline-delimited JSON).
/// Compatible with asciinema player for playback.
///
/// # Format
///
/// Line 1: Header (JSON object)
/// Line 2+: Events (JSON arrays: [time, code, data])
///
/// # Example
///
/// ```json
/// {"version":2,"width":80,"height":24,"timestamp":1234567890}
/// [0.0,"o","$ "]
/// [1.5,"o","ls\r\n"]
/// [1.6,"o","file1.txt\r\nfile2.txt\r\n"]
/// ```
pub struct AsciicastRecorder {
    writer: BufWriter<File>,
    start_time: Instant,
    row: u16,
    cols: u16,
}

impl AsciicastRecorder {
    /// Create a new recorder
    ///
    /// # Arguments
    ///
    /// * `path` - Path to .cast file
    /// * `width` - Terminal width in columns
    /// * `height` - Terminal height in rows
    /// * `env` - Optional environment variables
    pub fn new(
        path: &Path,
        width: u16,
        height: u16,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write header
        let header = AsciicastHeader {
            version: 2,
            width,
            height,
            timestamp: Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            ),
            duration: None,
            idle_time_limit: None,
            command: None,
            title: None,
            env,
        };

        serde_json::to_writer(&mut writer, &header).map_err(std::io::Error::other)?;
        writeln!(writer)?;
        writer.flush()?;

        Ok(Self {
            writer,
            start_time: Instant::now(),
            row: height,
            cols: width,
        })
    }

    /// Record terminal output
    pub fn record_output(&mut self, data: &[u8]) -> Result<()> {
        self.record_event(EventType::Output, data)
    }

    /// Record keyboard input
    pub fn record_input(&mut self, data: &[u8]) -> Result<()> {
        self.record_event(EventType::Input, data)
    }

    /// Record a marker/breakpoint
    pub fn record_marker(&mut self, label: &str) -> Result<()> {
        self.record_event(EventType::Marker, label.as_bytes())
    }

    /// Record terminal resize
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let resize_str = format!("{}x{}", cols, rows);
        self.record_event(EventType::Resize, resize_str.as_bytes())?;
        self.cols = cols;
        self.row = rows;
        Ok(())
    }

    fn record_event(&mut self, event_type: EventType, data: &[u8]) -> Result<()> {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let event_char = event_type.as_char().to_string();
        let data_str = String::from_utf8_lossy(data);

        // Write event as JSON array: [time, code, data]
        write!(self.writer, "[{:.6},\"{}\",", elapsed, event_char)?;
        serde_json::to_writer(&mut self.writer, &data_str.as_ref())
            .map_err(std::io::Error::other)?;
        writeln!(self.writer, "]")?;

        // Flush periodically for safety
        if (elapsed as u64).is_multiple_of(5) {
            self.writer.flush()?;
        }

        Ok(())
    }

    /// Finalize recording and flush all data
    pub fn finalize(mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_asciicast_header() {
        let header = AsciicastHeader {
            version: 2,
            width: 80,
            height: 24,
            timestamp: Some(1234567890),
            duration: None,
            idle_time_limit: None,
            command: None,
            title: None,
            env: None,
        };

        let json = serde_json::to_string(&header).unwrap();
        assert!(json.contains("\"version\":2"));
        assert!(json.contains("\"width\":80"));
        assert!(json.contains("\"height\":24"));
    }

    #[test]
    fn test_recorder_creates_file() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test-recording.cast");

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("SHELL".to_string(), "/bin/bash".to_string());

        let mut recorder = AsciicastRecorder::new(&path, 80, 24, Some(env)).unwrap();

        recorder.record_output(b"$ ").unwrap();
        recorder.record_output(b"ls\r\n").unwrap();
        recorder
            .record_output(b"file1.txt\r\nfile2.txt\r\n")
            .unwrap();

        recorder.finalize().unwrap();

        // Verify file exists and has content
        assert!(path.exists());

        let mut file = File::open(&path).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();

        // Check header line
        let lines: Vec<&str> = contents.lines().collect();
        assert!(lines.len() >= 4); // Header + 3 events

        // Parse header
        let header: AsciicastHeader = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header.version, 2);
        assert_eq!(header.width, 80);
        assert_eq!(header.height, 24);

        // Check events
        assert!(lines[1].contains("\"o\""));
        assert!(lines[1].contains("$ "));

        // Cleanup
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_event_types() {
        assert_eq!(EventType::Output.as_char(), 'o');
        assert_eq!(EventType::Input.as_char(), 'i');
        assert_eq!(EventType::Marker.as_char(), 'm');
        assert_eq!(EventType::Resize.as_char(), 'r');
    }

    #[test]
    fn test_record_resize() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("test-resize.cast");

        let mut recorder = AsciicastRecorder::new(&path, 80, 24, None).unwrap();
        recorder.record_resize(100, 30).unwrap();
        recorder.finalize().unwrap();

        let mut file = File::open(&path).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();

        assert!(contents.contains("\"r\""));
        assert!(contents.contains("100x30"));

        std::fs::remove_file(path).ok();
    }
}

/// Guacamole session recorder (.ses format)
///
/// Records Guacamole protocol instructions in guacd-compatible .ses format.
/// This format records all protocol-level events (images, mouse, keyboard, etc.)
/// with timestamps, allowing full session playback.
///
/// # Format
///
/// Each line contains: `<timestamp_ms>.<direction>.<instruction_bytes>`
/// - timestamp_ms: Milliseconds since session start
/// - direction: '0' = server->client, '1' = client->server
/// - instruction_bytes: Raw Guacamole protocol instruction (e.g., "3.key,3.104,1.1;")
///
/// # File Format Example
///
/// ```text
/// 0.0.4.size,1.0,4.1920,4.1080;
/// 150.1.3.key,3.104,1.1;
/// 200.0.3.img,1.0,10.image/png,1.0,1.0,1000.<base64_data>;
/// ```
pub struct GuacamoleSessionRecorder {
    writer: BufWriter<File>,
    start_time: Instant,
}

impl GuacamoleSessionRecorder {
    /// Create a new Guacamole session recorder
    ///
    /// # Arguments
    ///
    /// * `path` - Path to .ses file
    pub fn new(path: &Path) -> Result<Self> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);

        Ok(Self {
            writer,
            start_time: Instant::now(),
        })
    }

    /// Record a Guacamole protocol instruction
    ///
    /// # Arguments
    ///
    /// * `direction` - 0 = server->client, 1 = client->server
    /// * `instruction` - Raw Guacamole protocol instruction bytes
    pub fn record_instruction(&mut self, direction: u8, instruction: &Bytes) -> Result<()> {
        let timestamp_ms = self.start_time.elapsed().as_millis() as u64;

        // Format: <timestamp_ms>.<direction>.<instruction>
        // Convert instruction to string (it's already UTF-8)
        let instruction_str = String::from_utf8_lossy(instruction);

        writeln!(
            self.writer,
            "{}.{}.{}",
            timestamp_ms, direction, instruction_str
        )?;

        // Flush periodically for safety
        if timestamp_ms.is_multiple_of(5000) {
            self.writer.flush()?;
        }

        Ok(())
    }

    /// Record instruction from server to client
    pub fn record_server_to_client(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(0, instruction)
    }

    /// Record instruction from client to server
    pub fn record_client_to_server(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(1, instruction)
    }

    /// Finalize recording and flush all data
    pub fn finalize(mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

/// Recording transport trait for pluggable recording destinations
///
/// Allows recordings to be written to files, ZeroMQ, channels, or other destinations.
#[async_trait]
pub trait RecordingTransport: Send + Sync {
    /// Write recording data
    async fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Flush any buffered data
    async fn flush(&mut self) -> Result<()>;

    /// Finalize the recording (close file, send final message, etc.)
    async fn finalize(&mut self) -> Result<()>;
}

/// File-based recording transport
pub struct FileRecordingTransport {
    file: tokio::fs::File,
}

impl FileRecordingTransport {
    pub fn new(path: &Path) -> Result<Self> {
        // Create file synchronously, then convert to async file
        let file = std::fs::File::create(path)?;
        let async_file = tokio::fs::File::from_std(file);
        Ok(Self { file: async_file })
    }
}

#[async_trait]
impl RecordingTransport for FileRecordingTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.file.write_all(data).await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        self.flush().await?;
        Ok(())
    }
}

/// Channel-based recording transport (for ZeroMQ, gRPC, etc.)
///
/// Sends recording data over an async channel to an external handler.
/// This can be used to forward recordings to ZeroMQ, message queues, or other systems.
///
/// # Example: ZeroMQ Integration
///
/// ```no_run
/// use tokio::sync::mpsc;
/// use guacr_terminal::{ChannelRecordingTransport, RecordingTransport};
/// use bytes::Bytes;
///
/// # async fn example() {
/// // Create channel for recording data
/// let (recording_tx, mut recording_rx) = mpsc::channel::<Bytes>(1024);
///
/// // Create transport
/// let mut transport = ChannelRecordingTransport::new(recording_tx);
///
/// // Spawn background task to forward to ZeroMQ
/// tokio::spawn(async move {
///     // Initialize ZeroMQ socket here
///     // let zmq_socket = zmq::Context::new().socket(zmq::PUSH).unwrap();
///     // zmq_socket.connect("tcp://localhost:5555").unwrap();
///     
///     while let Some(data) = recording_rx.recv().await {
///         // Send to ZeroMQ
///         // zmq_socket.send(&data, 0).unwrap();
///     }
/// });
/// # }
/// ```
pub struct ChannelRecordingTransport {
    sender: mpsc::Sender<Bytes>,
}

impl ChannelRecordingTransport {
    pub fn new(sender: mpsc::Sender<Bytes>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl RecordingTransport for ChannelRecordingTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.sender
            .send(Bytes::from(data.to_vec()))
            .await
            .map_err(|e| {
                crate::TerminalError::IoError(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!("Recording channel closed: {}", e),
                ))
            })?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        // Channels don't need explicit flushing
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        // Send final marker or close signal if needed
        // For now, just flush
        self.flush().await?;
        Ok(())
    }
}

/// Multi-destination recording transport
///
/// Writes to multiple transports simultaneously (e.g., file + ZeroMQ)
#[derive(Default)]
pub struct MultiTransportRecorder {
    transports: Vec<Box<dyn RecordingTransport>>,
}

impl MultiTransportRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_transport(&mut self, transport: Box<dyn RecordingTransport>) {
        self.transports.push(transport);
    }
}

#[async_trait]
impl RecordingTransport for MultiTransportRecorder {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        // Write to all transports
        for transport in &mut self.transports {
            transport.write(data).await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        for transport in &mut self.transports {
            transport.flush().await?;
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        for transport in &mut self.transports {
            transport.finalize().await?;
        }
        Ok(())
    }
}

/// Dual-format session recorder with pluggable transports
///
/// Records sessions in both asciicast (terminal I/O) and Guacamole .ses (protocol) formats.
/// Supports multiple transport destinations (files, ZeroMQ, channels, etc.)
pub struct DualFormatRecorder {
    asciicast: Option<AsciicastRecorder>,
    guacamole: Option<GuacamoleSessionRecorder>,
}

impl DualFormatRecorder {
    /// Create a new dual-format recorder with file-based storage
    ///
    /// # Arguments
    ///
    /// * `asciicast_path` - Optional path to .cast file (for terminal I/O)
    /// * `guacamole_path` - Optional path to .ses file (for protocol instructions)
    /// * `width` - Terminal width in columns
    /// * `height` - Terminal height in rows
    /// * `env` - Optional environment variables for asciicast
    pub fn new(
        asciicast_path: Option<&Path>,
        guacamole_path: Option<&Path>,
        width: u16,
        height: u16,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let asciicast = if let Some(path) = asciicast_path {
            Some(AsciicastRecorder::new(path, width, height, env)?)
        } else {
            None
        };

        let guacamole = if let Some(path) = guacamole_path {
            Some(GuacamoleSessionRecorder::new(path)?)
        } else {
            None
        };

        Ok(Self {
            asciicast,
            guacamole,
        })
    }

    /// Record terminal output (asciicast only)
    pub fn record_output(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast {
            recorder.record_output(data)?;
        }
        // Also write to additional transports
        // Note: This is sync, but transports are async - we'd need to make this async
        // For now, file-based recording works synchronously
        // Use AsyncDualFormatRecorder for async transports
        Ok(())
    }

    /// Record keyboard input (asciicast only)
    pub fn record_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast {
            recorder.record_input(data)?;
        }
        Ok(())
    }

    /// Record terminal resize (asciicast only)
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast {
            recorder.record_resize(cols, rows)?;
        }
        Ok(())
    }

    /// Record Guacamole protocol instruction (guacamole .ses format)
    pub fn record_instruction(&mut self, direction: u8, instruction: &Bytes) -> Result<()> {
        if let Some(ref mut recorder) = self.guacamole {
            recorder.record_instruction(direction, instruction)?;
        }
        Ok(())
    }

    /// Record instruction from server to client
    pub fn record_server_to_client(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(0, instruction)
    }

    /// Record instruction from client to server
    pub fn record_client_to_server(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(1, instruction)
    }

    /// Finalize both recorders and all transports
    pub fn finalize(self) -> Result<()> {
        if let Some(recorder) = self.asciicast {
            recorder.finalize()?;
        }
        if let Some(recorder) = self.guacamole {
            recorder.finalize()?;
        }
        // Note: Transports would need async finalization, but for now file-based is sync
        Ok(())
    }
}

/// Async dual-format recorder with transport support
///
/// This version supports async transports (ZeroMQ, channels, etc.)
pub struct AsyncDualFormatRecorder {
    asciicast_file: Option<AsciicastRecorder>,
    guacamole_file: Option<GuacamoleSessionRecorder>,
    asciicast_transports: Vec<Box<dyn RecordingTransport>>,
    guacamole_transports: Vec<Box<dyn RecordingTransport>>,
}

impl AsyncDualFormatRecorder {
    /// Create a new async recorder with file and transport support
    ///
    /// # Arguments
    ///
    /// * `asciicast_file` - Optional file path for asciicast
    /// * `guacamole_file` - Optional file path for guacamole .ses
    /// * `asciicast_transports` - Additional async transports for asciicast
    /// * `guacamole_transports` - Additional async transports for guacamole .ses
    /// * `width` - Terminal width in columns
    /// * `height` - Terminal height in rows
    /// * `env` - Optional environment variables
    pub fn new(
        asciicast_file: Option<&Path>,
        guacamole_file: Option<&Path>,
        asciicast_transports: Vec<Box<dyn RecordingTransport>>,
        guacamole_transports: Vec<Box<dyn RecordingTransport>>,
        width: u16,
        height: u16,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let asciicast_file_rec = if let Some(path) = asciicast_file {
            Some(AsciicastRecorder::new(path, width, height, env.clone())?)
        } else {
            None
        };

        let guacamole_file_rec = if let Some(path) = guacamole_file {
            Some(GuacamoleSessionRecorder::new(path)?)
        } else {
            None
        };

        Ok(Self {
            asciicast_file: asciicast_file_rec,
            guacamole_file: guacamole_file_rec,
            asciicast_transports,
            guacamole_transports,
        })
    }

    /// Record terminal output (asciicast)
    pub async fn record_output(&mut self, data: &[u8]) -> Result<()> {
        // Write to file if enabled
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder.record_output(data)?;
        }

        // Write to async transports
        for transport in &mut self.asciicast_transports {
            transport.write(data).await?;
        }

        Ok(())
    }

    /// Record keyboard input (asciicast)
    pub async fn record_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder.record_input(data)?;
        }

        for transport in &mut self.asciicast_transports {
            transport.write(data).await?;
        }

        Ok(())
    }

    /// Record terminal resize (asciicast)
    pub async fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder.record_resize(cols, rows)?;
        }
        Ok(())
    }

    /// Record Guacamole protocol instruction
    pub async fn record_instruction(&mut self, direction: u8, instruction: &Bytes) -> Result<()> {
        // Write to file if enabled
        if let Some(ref mut recorder) = self.guacamole_file {
            recorder.record_instruction(direction, instruction)?;
        }

        // Format instruction for transport: <timestamp_ms>.<direction>.<instruction>
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let instruction_str = String::from_utf8_lossy(instruction);
        let formatted = format!("{}.{}.{}\n", timestamp_ms, direction, instruction_str);

        // Write to async transports
        for transport in &mut self.guacamole_transports {
            transport.write(formatted.as_bytes()).await?;
        }

        Ok(())
    }

    /// Record instruction from server to client
    pub async fn record_server_to_client(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(0, instruction).await
    }

    /// Record instruction from client to server
    pub async fn record_client_to_server(&mut self, instruction: &Bytes) -> Result<()> {
        self.record_instruction(1, instruction).await
    }

    /// Finalize all recorders and transports
    pub async fn finalize(self) -> Result<()> {
        // Finalize file recorders
        if let Some(recorder) = self.asciicast_file {
            recorder.finalize()?;
        }
        if let Some(recorder) = self.guacamole_file {
            recorder.finalize()?;
        }

        // Finalize async transports
        for mut transport in self.asciicast_transports {
            transport.finalize().await?;
        }
        for mut transport in self.guacamole_transports {
            transport.finalize().await?;
        }

        Ok(())
    }
}

// S3 recording transport
//
// Uploads recordings directly to AWS S3 (or S3-compatible storage).
// Requires the `s3` feature to be enabled and aws-sdk-s3 dependency.
// Note: S3 feature not currently configured - uncomment when aws-sdk-s3 dependency is added
// To enable: Add aws-sdk-s3 to Cargo.toml and uncomment this code
/*
#[cfg(feature = "s3")]
pub struct S3RecordingTransport {
    client: aws_sdk_s3::Client,
    bucket: String,
    key_prefix: String,
    buffer: Vec<u8>,
    session_id: String,
    format: String, // "cast" or "ses"
}

#[cfg(feature = "s3")]
impl S3RecordingTransport {
    pub fn new(
        client: aws_sdk_s3::Client,
        bucket: String,
        key_prefix: String,
        session_id: String,
        format: String,
    ) -> Self {
        Self {
            client,
            bucket,
            key_prefix,
            buffer: Vec::new(),
            session_id,
            format,
        }
    }

    pub async fn from_config(
        config: &aws_config::SdkConfig,
        bucket: String,
        key_prefix: String,
        session_id: String,
        format: String,
    ) -> Self {
        let client = aws_sdk_s3::Client::new(config);
        Self::new(client, bucket, key_prefix, session_id, format)
    }

    fn s3_key(&self) -> String {
        format!("{}{}.{}", self.key_prefix, self.session_id, self.format)
    }
}

#[cfg(feature = "s3")]
#[async_trait]
impl RecordingTransport for S3RecordingTransport {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        use aws_sdk_s3::primitives::ByteStream;

        if !self.buffer.is_empty() {
            let key = self.s3_key();
            let body = ByteStream::from(self.buffer.clone());

            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body)
                .content_type(match self.format.as_str() {
                    "cast" => "application/json",
                    "ses" => "text/plain",
                    _ => "application/octet-stream",
                })
                .send()
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("S3 upload failed: {}", e)
                )))?;
        }

        Ok(())
    }
}
*/

/// Helper function to create recording transports from configuration
///
/// This function helps set up recording with multiple destinations based on parameters.
/// Supports file paths, S3, Azure Blob, and channel-based transports (ZeroMQ, message queues, etc.).
///
/// # Configuration Parameters
///
/// - `recording_s3_bucket` - S3 bucket name
/// - `recording_s3_region` - AWS region (defaults to us-east-1)
/// - `recording_s3_prefix` - Key prefix (defaults to "recordings/")
/// - `recording_s3_endpoint` - Custom endpoint for S3-compatible storage
/// - `recording_azure_account` - Azure storage account name
/// - `recording_azure_container` - Azure container name
/// - `recording_zmq_endpoint` - ZeroMQ endpoint
///
/// # Example Usage
///
/// ```no_run
/// use guacr_terminal::{create_recording_transports, AsyncDualFormatRecorder, ChannelRecordingTransport};
/// use tokio::sync::mpsc;
/// use bytes::Bytes;
/// use std::collections::HashMap;
///
/// # async fn example() {
/// let mut params = HashMap::new();
/// params.insert("recording_s3_bucket".to_string(), "my-recordings".to_string());
/// params.insert("recording_s3_region".to_string(), "us-west-2".to_string());
///
/// let (asciicast_transports, guacamole_transports) =
///     create_recording_transports(&params, "session-123").await.unwrap();
/// # }
/// ```
pub async fn create_recording_transports(
    params: &std::collections::HashMap<String, String>,
    _session_id: &str,
) -> Result<(
    Vec<Box<dyn RecordingTransport>>,
    Vec<Box<dyn RecordingTransport>>,
)> {
    let asciicast_transports = Vec::new();
    let guacamole_transports = Vec::new();

    // S3 transport (requires s3 feature and aws-sdk-s3 dependency)
    // Uncomment when aws-sdk-s3 is added to dependencies
    /*
    #[cfg(feature = "s3")]
    {
        if let Some(bucket) = params.get("recording_s3_bucket") {
            let region = params.get("recording_s3_region")
                .map(|s| aws_sdk_s3::config::Region::new(s.clone()))
                .unwrap_or_else(|| aws_sdk_s3::config::Region::new("us-east-1"));

            let prefix = params.get("recording_s3_prefix")
                .cloned()
                .unwrap_or_else(|| "recordings/".to_string());

            let mut config = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(region)
                .load()
                .await;

            if let Some(endpoint) = params.get("recording_s3_endpoint") {
                config = config.to_builder()
                    .endpoint_url(endpoint)
                    .build();
            }

            let client = aws_sdk_s3::Client::new(&config);

            asciicast_transports.push(Box::new(S3RecordingTransport::new(
                client.clone(),
                bucket.clone(),
                prefix.clone(),
                _session_id.to_string(),
                "cast".to_string(),
            )) as Box<dyn RecordingTransport>);

            guacamole_transports.push(Box::new(S3RecordingTransport::new(
                client,
                bucket.clone(),
                prefix,
                _session_id.to_string(),
                "ses".to_string(),
            )) as Box<dyn RecordingTransport>);
        }
    }
    */

    // Azure Blob Storage transport (placeholder - requires azure-storage-blobs crate)
    // To implement:
    // 1. Add azure-storage-blobs = "0.17" as optional dependency
    // 2. Create AzureBlobRecordingTransport struct similar to S3RecordingTransport
    // 3. Use azure_storage_blobs::BlobClient to upload recordings
    if params.contains_key("recording_azure_account") {
        log::warn!("Azure Blob Storage transport not yet implemented. Use S3 or file storage.");
    }

    // Google Cloud Storage transport (placeholder - requires google-cloud-storage crate)
    // To implement:
    // 1. Add google-cloud-storage = "0.15" as optional dependency
    // 2. Create GCSRecordingTransport struct similar to S3RecordingTransport
    // 3. Use google_cloud_storage::Client to upload recordings
    if params.contains_key("recording_gcs_bucket") {
        log::warn!("Google Cloud Storage transport not yet implemented. Use S3 or file storage.");
    }

    // ZeroMQ transport
    if params.contains_key("recording_zmq_endpoint") {
        // ZeroMQ would be set up via channels in the application layer
        // This is handled by ChannelRecordingTransport
    }

    Ok((asciicast_transports, guacamole_transports))
}
