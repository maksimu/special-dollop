//! Background query logging service.
//!
//! Receives [`QueryLog`] entries via a bounded mpsc channel and writes them
//! to a named pipe (FIFO) as JSON Lines. The Python Gateway reads the pipe,
//! encrypts the data (AES-256-GCM), and uploads it to Krouter.
//!
//! ## Design
//!
//! - Callers use `try_send()` to submit log entries without blocking the relay loop.
//! - A background tokio task drains the channel and writes JSON Lines output.
//! - Output path is a FIFO created by the Python Gateway before session start.
//! - Python side handles encryption and upload (same pattern as guacd recordings).

use std::io::{self, Write};
use std::path::PathBuf;
use tokio::sync::mpsc;

use super::config::ConnectionLoggingConfig;
use super::log_entry::QueryLog;

/// Channel buffer size for log entries.
/// Large enough to handle bursts without blocking the relay loop.
const CHANNEL_BUFFER_SIZE: usize = 1024;

/// Handle for submitting log entries to the background service.
///
/// This is the caller-facing side. It wraps an mpsc sender and provides
/// a non-blocking `try_send()` method safe for use in the relay hot path.
#[derive(Clone)]
pub struct QueryLogSender {
    tx: mpsc::Sender<LogMessage>,
}

/// Messages sent to the background logging service.
enum LogMessage {
    /// A query log entry to record.
    Entry(Box<QueryLog>),
    /// Signal to flush all buffers.
    Flush,
    /// Signal to shut down the service.
    Shutdown,
}

impl QueryLogSender {
    /// Submit a log entry without blocking.
    ///
    /// Returns `true` if the entry was accepted, `false` if the channel is full
    /// or the service has shut down. Callers should NOT retry on failure -
    /// dropped log entries are expected under extreme load.
    pub fn try_send(&self, entry: QueryLog) -> bool {
        self.tx.try_send(LogMessage::Entry(Box::new(entry))).is_ok()
    }

    /// Request the background service to flush its buffers.
    pub fn flush(&self) {
        let _ = self.tx.try_send(LogMessage::Flush);
    }

    /// Signal the background service to shut down gracefully.
    pub async fn shutdown(self) {
        let _ = self.tx.send(LogMessage::Shutdown).await;
    }
}

/// Background query logging service.
///
/// Spawns a tokio task that processes log entries from the channel
/// and writes them to the configured pipe path.
pub struct QueryLoggingService;

impl QueryLoggingService {
    /// Start the logging service and return a sender for submitting entries.
    ///
    /// Returns `None` if logging is disabled or no pipe path is configured.
    pub fn start(config: &ConnectionLoggingConfig) -> Option<QueryLogSender> {
        if !config.query_logging_enabled {
            return None;
        }

        let pipe_path = config.pipe_path.as_ref()?;

        let writer = match PipeWriter::new(pipe_path) {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to open query log pipe {}: {}", pipe_path, e);
                return None;
            }
        };

        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);

        tokio::task::spawn_blocking(move || {
            run_logging_task(rx, writer);
        });

        Some(QueryLogSender { tx })
    }
}

/// Writes QueryLog entries as JSON Lines to a pipe (FIFO) path.
///
/// On BrokenPipe error, sets `active = false` and silently drops
/// subsequent writes. This handles the case where the Python reader
/// closes the pipe before the Rust session ends.
struct PipeWriter {
    file: std::fs::File,
    path: PathBuf,
    active: bool,
}

impl PipeWriter {
    /// Open a path for writing. In production this will be a FIFO
    /// created by the Python Gateway; in tests it can be a regular file.
    fn new(path: &str) -> io::Result<Self> {
        let path_buf = PathBuf::from(path);
        let file = std::fs::OpenOptions::new().write(true).open(&path_buf)?;
        debug!("Query log pipe opened: {}", path);
        Ok(Self {
            file,
            path: path_buf,
            active: true,
        })
    }

    /// Write a single QueryLog entry as a JSON line.
    ///
    /// Returns Ok(()) even if the pipe is inactive (broken).
    fn write_entry(&mut self, entry: &QueryLog) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        let json = serde_json::to_string(entry).map_err(io::Error::other)?;

        match writeln!(self.file, "{}", json) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                warn!("Query log pipe broken: {}", self.path.display());
                self.active = false;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        match self.file.flush() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                warn!("Query log pipe broken on flush: {}", self.path.display());
                self.active = false;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn finalize(&mut self) {
        let _ = self.flush();
    }
}

/// Background task that processes log entries from the channel.
///
/// Runs on the blocking thread pool via `spawn_blocking` so that synchronous
/// pipe I/O does not block the tokio async runtime.
fn run_logging_task(mut rx: mpsc::Receiver<LogMessage>, mut writer: PipeWriter) {
    debug!("Query logging service started");

    let mut entry_count: u64 = 0;

    loop {
        match rx.blocking_recv() {
            Some(LogMessage::Entry(entry)) => {
                if let Err(e) = writer.write_entry(&entry) {
                    warn!("Failed to write query log entry: {}", e);
                }
                entry_count += 1;

                // Auto-flush every 100 entries
                if entry_count.is_multiple_of(100) {
                    if let Err(e) = writer.flush() {
                        debug!("Auto-flush failed after {} entries: {}", entry_count, e);
                    }
                }
            }
            Some(LogMessage::Flush) => {
                if let Err(e) = writer.flush() {
                    debug!("Explicit flush failed: {}", e);
                }
            }
            Some(LogMessage::Shutdown) | None => {
                debug!(
                    "Query logging service shutting down ({} entries)",
                    entry_count
                );
                writer.finalize();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_logging::log_entry::{SessionContext, SqlCommandType};
    use chrono::Utc;

    fn test_session_context() -> SessionContext {
        SessionContext {
            session_id: "test-session-123".to_string(),
            user_id: "testuser".to_string(),
            database: "testdb".to_string(),
            protocol: "mysql".to_string(),
            client_addr: "127.0.0.1:12345".to_string(),
            server_addr: "10.0.0.1:3306".to_string(),
        }
    }

    fn test_query_log(ctx: &SessionContext) -> QueryLog {
        QueryLog {
            timestamp: Utc::now(),
            session_id: ctx.session_id.clone(),
            user_id: ctx.user_id.clone(),
            database: ctx.database.clone(),
            protocol: ctx.protocol.clone(),
            command_type: SqlCommandType::Select,
            query_text: Some("SELECT * FROM users".to_string()),
            duration_ms: 12,
            rows_affected: Some(5),
            success: true,
            error_message: None,
            client_addr: ctx.client_addr.clone(),
            server_addr: ctx.server_addr.clone(),
        }
    }

    #[test]
    fn test_pipe_writer_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pipe");
        // Create a regular file to stand in for a FIFO in tests
        std::fs::File::create(&path).unwrap();

        let mut writer = PipeWriter::new(path.to_str().unwrap()).unwrap();
        let ctx = test_session_context();
        let entry = test_query_log(&ctx);

        writer.write_entry(&entry).unwrap();
        writer.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("SELECT * FROM users"));
        assert!(content.contains("\"success\":true"));
        assert!(content.contains("\"rows_affected\":5"));
    }

    #[test]
    fn test_pipe_writer_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pipe");
        std::fs::File::create(&path).unwrap();

        let mut writer = PipeWriter::new(path.to_str().unwrap()).unwrap();
        let ctx = test_session_context();

        for i in 0..3 {
            let mut entry = test_query_log(&ctx);
            entry.duration_ms = i;
            writer.write_entry(&entry).unwrap();
        }
        writer.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_pipe_writer_inactive_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pipe");
        std::fs::File::create(&path).unwrap();

        let mut writer = PipeWriter::new(path.to_str().unwrap()).unwrap();
        // Simulate broken pipe state
        writer.active = false;

        let ctx = test_session_context();
        let entry = test_query_log(&ctx);

        // Should silently succeed when inactive
        writer.write_entry(&entry).unwrap();
        writer.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn test_pipe_writer_finalize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pipe");
        std::fs::File::create(&path).unwrap();

        let mut writer = PipeWriter::new(path.to_str().unwrap()).unwrap();
        let ctx = test_session_context();
        let entry = test_query_log(&ctx);

        writer.write_entry(&entry).unwrap();
        writer.finalize();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("SELECT * FROM users"));
    }

    #[test]
    fn test_pipe_writer_jsonl_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pipe");
        std::fs::File::create(&path).unwrap();

        let mut writer = PipeWriter::new(path.to_str().unwrap()).unwrap();
        let ctx = test_session_context();

        for i in 0..3 {
            let mut entry = test_query_log(&ctx);
            entry.duration_ms = i;
            writer.write_entry(&entry).unwrap();
        }
        writer.flush().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Each line should be valid JSON
        for line in content.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("session_id").is_some());
            assert!(parsed.get("command_type").is_some());
            assert!(parsed.get("duration_ms").is_some());
        }
    }

    #[test]
    fn test_log_sender_try_send() {
        let (tx, _rx) = mpsc::channel(10);
        let sender = QueryLogSender { tx };
        let ctx = test_session_context();
        let entry = test_query_log(&ctx);

        assert!(sender.try_send(entry));
    }

    #[test]
    fn test_disabled_config_returns_none() {
        let config = ConnectionLoggingConfig::default();
        assert!(!config.query_logging_enabled);
    }

    #[tokio::test]
    async fn test_logging_service_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let pipe_path = dir.path().join("query.pipe");
        std::fs::File::create(&pipe_path).unwrap();

        let config = ConnectionLoggingConfig {
            query_logging_enabled: true,
            include_query_text: true,
            max_query_length: 10_000,
            pipe_path: Some(pipe_path.to_string_lossy().to_string()),
        };

        let sender = QueryLoggingService::start(&config).unwrap();
        let ctx = test_session_context();

        // Send some entries
        for i in 0..5 {
            let mut entry = test_query_log(&ctx);
            entry.duration_ms = i as u64;
            assert!(sender.try_send(entry));
        }

        // Shutdown gracefully
        sender.shutdown().await;

        // Allow background task to finish
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Verify JSON Lines output
        let content = std::fs::read_to_string(&pipe_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);

        // Each line should be valid JSON with expected fields
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("session_id").is_some());
            assert!(parsed.get("command_type").is_some());
        }
    }

    #[test]
    fn test_logging_service_no_pipe_path_returns_none() {
        let config = ConnectionLoggingConfig {
            query_logging_enabled: true,
            include_query_text: true,
            max_query_length: 10_000,
            pipe_path: None, // No pipe path
        };

        let result = QueryLoggingService::start(&config);
        assert!(result.is_none());
    }

    #[test]
    fn test_logging_service_disabled_returns_none() {
        let config = ConnectionLoggingConfig {
            query_logging_enabled: false,
            include_query_text: true,
            max_query_length: 10_000,
            pipe_path: Some("/tmp/test.pipe".to_string()),
        };

        let result = QueryLoggingService::start(&config);
        assert!(result.is_none());
    }

    #[test]
    fn test_pipe_writer_nonexistent_path_fails() {
        let result = PipeWriter::new("/tmp/nonexistent_dir_12345/test.pipe");
        assert!(result.is_err());
    }

    #[test]
    fn test_log_sender_full_channel_returns_false() {
        // Create a channel with capacity 1
        let (tx, _rx) = mpsc::channel(1);
        let sender = QueryLogSender { tx };
        let ctx = test_session_context();

        // First send should succeed
        assert!(sender.try_send(test_query_log(&ctx)));
        // Second send should fail (channel full, receiver not draining)
        assert!(!sender.try_send(test_query_log(&ctx)));
    }

    #[test]
    fn test_log_sender_dropped_receiver_returns_false() {
        let (tx, rx) = mpsc::channel(10);
        let sender = QueryLogSender { tx };
        // Drop the receiver
        drop(rx);

        let ctx = test_session_context();
        assert!(!sender.try_send(test_query_log(&ctx)));
    }

    /// Test PipeWriter with a real Unix FIFO (named pipe).
    /// This verifies the actual production path: write JSON Lines to a FIFO
    /// that a reader thread drains.
    #[cfg(unix)]
    #[test]
    fn test_pipe_writer_with_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let fifo_path = dir.path().join("test.fifo");
        let fifo_str = fifo_path.to_str().unwrap().to_string();

        // Create a real FIFO
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_str)
            .status()
            .expect("mkfifo command failed");
        assert!(status.success(), "mkfifo failed");

        // Spawn a reader thread (FIFO open blocks until both ends are open)
        let reader_path = fifo_path.clone();
        let reader_handle =
            std::thread::spawn(move || std::fs::read_to_string(&reader_path).unwrap());

        // Open FIFO for writing and write entries
        let mut writer = PipeWriter::new(&fifo_str).unwrap();
        let ctx = test_session_context();

        for i in 0..3 {
            let mut entry = test_query_log(&ctx);
            entry.duration_ms = i;
            writer.write_entry(&entry).unwrap();
        }
        writer.flush().unwrap();
        // Drop writer to close the write end, unblocking the reader
        drop(writer);

        // Collect reader output
        let content = reader_handle.join().expect("Reader thread panicked");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);

        // Verify each line is valid JSON
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["session_id"], "test-session-123");
            assert!(
                parsed["command_type"].is_string(),
                "command_type should be a string"
            );
        }
    }

    /// Test that writing to a socket whose read end is closed triggers
    /// BrokenPipe handling and sets active = false.
    #[cfg(unix)]
    #[test]
    fn test_pipe_writer_broken_pipe_detection() {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        use std::os::unix::net::UnixStream;

        // Create a connected pair of Unix sockets
        let (reader, writer_stream) = UnixStream::pair().unwrap();

        // Drop the reader to simulate broken pipe
        drop(reader);

        // Convert the writer socket into a std::fs::File via raw fd
        let file = unsafe { std::fs::File::from_raw_fd(writer_stream.into_raw_fd()) };
        let mut writer = PipeWriter {
            file,
            path: PathBuf::from("(test-pipe)"),
            active: true,
        };

        let ctx = test_session_context();
        let entry = test_query_log(&ctx);

        // First write should detect BrokenPipe and deactivate
        writer.write_entry(&entry).unwrap();
        assert!(!writer.active, "Writer should be inactive after BrokenPipe");

        // Subsequent writes should silently succeed (no-op)
        writer.write_entry(&entry).unwrap();
        assert!(!writer.active);
    }
}
