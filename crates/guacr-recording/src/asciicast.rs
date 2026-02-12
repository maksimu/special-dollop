// Asciicast v2 session recorder
//
// Records terminal sessions in asciicast v2 format (newline-delimited JSON).
// Compatible with asciinema player for playback.
//
// # Format
//
// Line 1: Header (JSON object)
// Line 2+: Events (JSON arrays: [time, code, data])
//
// # Example
//
// ```json
// {"version":2,"width":80,"height":24,"timestamp":1234567890}
// [0.0,"o","$ "]
// [1.5,"o","ls\r\n"]
// [1.6,"o","file1.txt\r\nfile2.txt\r\n"]
// ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::ses::RecordingError;

/// Asciicast v2 header
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

/// Asciicast v2 session recorder
///
/// Records terminal sessions in asciicast v2 format (newline-delimited JSON).
/// Compatible with asciinema player for playback.
pub struct AsciicastRecorder {
    writer: BufWriter<File>,
    start_time: Instant,
    _rows: u16,
    _cols: u16,
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
    ) -> Result<Self, RecordingError> {
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
            _rows: height,
            _cols: width,
        })
    }

    /// Record terminal output
    pub fn record_output(&mut self, data: &[u8]) -> Result<(), RecordingError> {
        self.record_event(EventType::Output, data)
    }

    /// Record keyboard input
    pub fn record_input(&mut self, data: &[u8]) -> Result<(), RecordingError> {
        self.record_event(EventType::Input, data)
    }

    /// Record a marker/breakpoint
    pub fn record_marker(&mut self, label: &str) -> Result<(), RecordingError> {
        self.record_event(EventType::Marker, label.as_bytes())
    }

    /// Record terminal resize
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<(), RecordingError> {
        let resize_str = format!("{}x{}", cols, rows);
        self.record_event(EventType::Resize, resize_str.as_bytes())?;
        self._cols = cols;
        self._rows = rows;
        Ok(())
    }

    fn record_event(&mut self, event_type: EventType, data: &[u8]) -> Result<(), RecordingError> {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let event_char = event_type.as_char().to_string();
        let data_str = String::from_utf8_lossy(data);

        // Write event as JSON array: [time, code, data]
        write!(self.writer, "[{:.6},\"{}\",", elapsed, event_char)?;
        serde_json::to_writer(&mut self.writer, &data_str.as_ref())
            .map_err(std::io::Error::other)?;
        writeln!(self.writer, "]")?;

        Ok(())
    }

    /// Finalize recording and flush all data
    pub fn finalize(mut self) -> Result<(), RecordingError> {
        self.writer.flush()?;
        Ok(())
    }
}

/// RAII: Ensure recording file is flushed even on early return or panic
impl Drop for AsciicastRecorder {
    fn drop(&mut self) {
        if let Err(e) = self.writer.flush() {
            log::warn!("Failed to flush asciicast recording during drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_event_types() {
        assert_eq!(EventType::Output.as_char(), 'o');
        assert_eq!(EventType::Input.as_char(), 'i');
        assert_eq!(EventType::Marker.as_char(), 'm');
        assert_eq!(EventType::Resize.as_char(), 'r');
    }

    #[test]
    fn test_recorder_creates_file_with_header() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let path = tmp_dir.path().join("test.cast");

        let mut recorder = AsciicastRecorder::new(&path, 80, 24, None).unwrap();
        recorder.record_output(b"$ ").unwrap();
        recorder.finalize().unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 2); // Header + 1 event

        // Parse header
        let header: AsciicastHeader = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header.version, 2);
        assert_eq!(header.width, 80);
        assert_eq!(header.height, 24);

        // Check event
        assert!(lines[1].contains("\"o\""));
    }

    #[test]
    fn test_recorder_resize() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let path = tmp_dir.path().join("resize.cast");

        let mut recorder = AsciicastRecorder::new(&path, 80, 24, None).unwrap();
        recorder.record_resize(100, 30).unwrap();
        recorder.finalize().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"r\""));
        assert!(content.contains("100x30"));
    }
}
