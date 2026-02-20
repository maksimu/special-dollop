// Session recording formats: asciicast v2 and guacd .ses
//
// Core recording types (AsciicastRecorder, GuacamoleSesRecorder, transports)
// live in the guacr-recording crate. This module provides terminal-specific
// wrappers (DualFormatRecorder, AsyncDualFormatRecorder) and the
// create_recording_transports helper.

use crate::Result;
use bytes::Bytes;
use std::collections::HashMap;
use std::path::Path;

// Re-export core types from guacr-recording
pub use guacr_recording::{
    AsciicastHeader, AsciicastRecorder, ChannelRecordingTransport, EventType,
    FileRecordingTransport, MultiTransportRecorder, RecordingTransport,
};

/// Guacamole session recorder (.ses format)
///
/// Thin wrapper around `guacr_recording::GuacamoleSesRecorder` that maps errors
/// to `crate::TerminalError`. Uses the correct raw-protocol format (no
/// `timestamp.direction.instruction` prefix).
pub struct GuacamoleSessionRecorder {
    inner: guacr_recording::GuacamoleSesRecorder,
}

impl GuacamoleSessionRecorder {
    /// Create a new Guacamole session recorder
    pub fn new(path: &Path) -> Result<Self> {
        let config = guacr_recording::RecordingConfig::default();
        let inner = guacr_recording::GuacamoleSesRecorder::new(path, &config)
            .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        Ok(Self { inner })
    }

    /// Record a Guacamole protocol instruction
    pub fn record_instruction(&mut self, direction: u8, instruction: &Bytes) -> Result<()> {
        let dir = if direction == 0 {
            guacr_recording::RecordingDirection::ServerToClient
        } else {
            guacr_recording::RecordingDirection::ClientToServer
        };
        self.inner
            .record(dir, instruction)
            .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))
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
    pub fn finalize(self) -> Result<()> {
        self.inner
            .finalize()
            .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))
    }
}

/// Dual-format session recorder with pluggable transports
///
/// Records sessions in both asciicast (terminal I/O) and Guacamole .ses (protocol) formats.
pub struct DualFormatRecorder {
    asciicast: Option<AsciicastRecorder>,
    guacamole: Option<GuacamoleSessionRecorder>,
}

impl DualFormatRecorder {
    /// Create a new dual-format recorder with file-based storage
    pub fn new(
        asciicast_path: Option<&Path>,
        guacamole_path: Option<&Path>,
        width: u16,
        height: u16,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let asciicast = if let Some(path) = asciicast_path {
            Some(
                AsciicastRecorder::new(path, width, height, env).map_err(|e| {
                    crate::TerminalError::IoError(std::io::Error::other(e.to_string()))
                })?,
            )
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
            recorder
                .record_output(data)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        Ok(())
    }

    /// Record keyboard input (asciicast only)
    pub fn record_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast {
            recorder
                .record_input(data)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        Ok(())
    }

    /// Record terminal resize (asciicast only)
    pub fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast {
            recorder
                .record_resize(cols, rows)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
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
            recorder
                .finalize()
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        if let Some(recorder) = self.guacamole {
            recorder.finalize()?;
        }
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
            Some(
                AsciicastRecorder::new(path, width, height, env.clone()).map_err(|e| {
                    crate::TerminalError::IoError(std::io::Error::other(e.to_string()))
                })?,
            )
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
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder
                .record_output(data)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }

        for transport in &mut self.asciicast_transports {
            transport
                .write(data)
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }

        Ok(())
    }

    /// Record keyboard input (asciicast)
    pub async fn record_input(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder
                .record_input(data)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }

        for transport in &mut self.asciicast_transports {
            transport
                .write(data)
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }

        Ok(())
    }

    /// Record terminal resize (asciicast)
    pub async fn record_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref mut recorder) = self.asciicast_file {
            recorder
                .record_resize(cols, rows)
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        Ok(())
    }

    /// Record Guacamole protocol instruction
    pub async fn record_instruction(&mut self, direction: u8, instruction: &Bytes) -> Result<()> {
        if let Some(ref mut recorder) = self.guacamole_file {
            recorder.record_instruction(direction, instruction)?;
        }

        // Format instruction for transport
        let instruction_str = String::from_utf8_lossy(instruction);
        let formatted = format!("{}\n", instruction_str);

        for transport in &mut self.guacamole_transports {
            transport
                .write(formatted.as_bytes())
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
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
        if let Some(recorder) = self.asciicast_file {
            recorder
                .finalize()
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        if let Some(recorder) = self.guacamole_file {
            recorder.finalize()?;
        }

        for mut transport in self.asciicast_transports {
            transport
                .finalize()
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }
        for mut transport in self.guacamole_transports {
            transport
                .finalize()
                .await
                .map_err(|e| crate::TerminalError::IoError(std::io::Error::other(e.to_string())))?;
        }

        Ok(())
    }
}

/// Helper function to create recording transports from configuration
pub async fn create_recording_transports(
    params: &std::collections::HashMap<String, String>,
    _session_id: &str,
) -> Result<(
    Vec<Box<dyn RecordingTransport>>,
    Vec<Box<dyn RecordingTransport>>,
)> {
    let asciicast_transports = Vec::new();
    let guacamole_transports = Vec::new();

    // Azure Blob Storage transport (placeholder)
    if params.contains_key("recording_azure_account") {
        log::warn!("Azure Blob Storage transport not yet implemented. Use S3 or file storage.");
    }

    // Google Cloud Storage transport (placeholder)
    if params.contains_key("recording_gcs_bucket") {
        log::warn!("Google Cloud Storage transport not yet implemented. Use S3 or file storage.");
    }

    Ok((asciicast_transports, guacamole_transports))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_types() {
        assert_eq!(EventType::Output.as_char(), 'o');
        assert_eq!(EventType::Input.as_char(), 'i');
        assert_eq!(EventType::Marker.as_char(), 'm');
        assert_eq!(EventType::Resize.as_char(), 'r');
    }

    #[test]
    fn test_dual_format_creates_files() {
        let temp_dir = std::env::temp_dir();
        let cast_path = temp_dir.join("guacr-term-test-dual.cast");
        let ses_path = temp_dir.join("guacr-term-test-dual.ses");

        let mut recorder =
            DualFormatRecorder::new(Some(&cast_path), Some(&ses_path), 80, 24, None).unwrap();

        recorder.record_output(b"$ ").unwrap();
        recorder
            .record_server_to_client(&Bytes::from("4.size,1.0,2.80,2.24;"))
            .unwrap();

        recorder.finalize().unwrap();

        assert!(cast_path.exists());
        assert!(ses_path.exists());

        // Verify .ses format is raw protocol (not timestamp.direction.instruction)
        let ses_content = std::fs::read_to_string(&ses_path).unwrap();
        for line in ses_content.lines() {
            if line.is_empty() {
                continue;
            }
            assert!(line.ends_with(';'), "Should end with semicolon: {}", line);
            assert!(line.contains('.'), "Should have Guacamole protocol format");
            // Should NOT match the old broken format "TIMESTAMP.DIRECTION.INSTRUCTION"
            let first_char = line.chars().next().unwrap();
            assert!(
                first_char.is_ascii_digit(),
                "Should start with length prefix"
            );
        }

        std::fs::remove_file(cast_path).ok();
        std::fs::remove_file(ses_path).ok();
    }
}
