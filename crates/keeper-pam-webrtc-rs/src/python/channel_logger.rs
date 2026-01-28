// channel_logger.rs - Channel-based logger that sends records to Python without GIL contention
//
// Architecture:
// - Rust threads call log::debug!(), log::info!(), etc.
// - ChannelLogger::log() sends LogRecord to unbounded channel (NO GIL!)
// - Python task (logger_task.rs) receives records and forwards to Python logging
//
// Performance:
// - enabled() check: ~1-2ns (atomic read)
// - log() overhead: ~100-200ns (string formatting + channel send)
// - vs pyo3-log: ~50-100μs (500-1000× improvement due to no GIL contention)

use log::{LevelFilter, Metadata, Record};
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Log record structure sent over channel from Rust to Python
///
/// This struct is fully owned (no references to Python objects) so it can be sent
/// across thread boundaries safely without holding the GIL.
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Log level (ERROR=40, WARN=30, INFO=20, DEBUG=10)
    pub level: log::Level,

    /// Logger name/target (e.g., "keeper_pam_webrtc_rs", "webrtc_ice")
    /// This is used to call logging.getLogger(target) in Python
    pub target: String,

    /// Formatted log message
    pub message: String,

    /// Optional: Module path (e.g., "keeper_pam_webrtc_rs::tube")
    pub module_path: Option<String>,

    /// Optional: Source file name
    pub file: Option<String>,

    /// Optional: Line number
    pub line: Option<u32>,
}

/// Custom logger that sends log records to an unbounded channel
///
/// This logger never acquires the Python GIL during logging operations,
/// eliminating GIL contention in hot paths like frame processing.
pub struct ChannelLogger {
    /// Sender end of the unbounded channel
    /// Multiple senders (Rust threads) → One receiver (Python task)
    sender: UnboundedSender<LogRecord>,

    /// Global log level filter (stored as atomic u8 for lock-free access)
    /// Used for fast filtering before creating log records
    level_filter: AtomicU8,
}

impl ChannelLogger {
    /// Create a new channel logger with the specified level filter
    fn new(sender: UnboundedSender<LogRecord>, level_filter: LevelFilter) -> Self {
        Self {
            sender,
            level_filter: AtomicU8::new(level_filter as u8),
        }
    }
}

impl log::Log for ChannelLogger {
    /// Fast-path check if a log record would be enabled
    ///
    /// Performance: ~1-2ns (single atomic load with Relaxed ordering)
    fn enabled(&self, metadata: &Metadata) -> bool {
        let filter_value = self.level_filter.load(Ordering::Relaxed);

        // Convert u8 back to LevelFilter
        // Values: Off=0, Error=1, Warn=2, Info=3, Debug=4, Trace=5
        let filter = match filter_value {
            0 => LevelFilter::Off,
            1 => LevelFilter::Error,
            2 => LevelFilter::Warn,
            3 => LevelFilter::Info,
            4 => LevelFilter::Debug,
            5 => LevelFilter::Trace,
            _ => LevelFilter::Off, // Fallback for invalid values
        };

        metadata.level() <= filter
    }

    /// Log a record by sending it to the channel
    ///
    /// Performance: ~100-200ns total
    /// - enabled() check: ~1-2ns
    /// - String formatting: ~30-50ns
    /// - Channel send: ~50-100ns
    ///
    /// Critical: This NEVER acquires the Python GIL!
    fn log(&self, record: &Record) {
        // Fast path: check if enabled (avoid allocation if filtered)
        if !self.enabled(record.metadata()) {
            return;
        }

        // Create owned log record (no Python objects!)
        let log_record = LogRecord {
            level: record.level(),
            target: record.target().to_string(),
            message: format!("{}", record.args()),
            module_path: record.module_path().map(String::from),
            file: record.file().map(String::from),
            line: record.line(),
        };

        // Send to channel (non-blocking, no GIL!)
        // If channel is closed (shutdown), silently drop the log
        let _ = self.sender.send(log_record);
    }

    /// Flush is a no-op for channel-based logger
    ///
    /// Python's logging handlers will flush on their own schedule.
    /// The unbounded channel ensures no logs are lost during normal operation.
    fn flush(&self) {
        // No-op: Python side handles flushing
    }
}

/// Create a channel logger and return (logger, receiver)
///
/// Usage:
/// ```
/// let (logger, receiver) = create_channel_logger(log::LevelFilter::Debug);
/// log::set_boxed_logger(Box::new(logger)).unwrap();
/// spawn_logger_task(receiver, runtime_handle);  // From logger_task.rs
/// ```
pub fn create_channel_logger(
    level_filter: LevelFilter,
) -> (ChannelLogger, UnboundedReceiver<LogRecord>) {
    let (sender, receiver) = unbounded_channel();
    let logger = ChannelLogger::new(sender, level_filter);
    (logger, receiver)
}
