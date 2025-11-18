// asciicast v2 format session recorder

use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
