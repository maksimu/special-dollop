// logger_task.rs - Async task that receives log records and forwards them to Python
//
// Architecture:
// - Single async task runs on tokio runtime
// - Receives LogRecords from unbounded channel (sent by ChannelLogger)
// - Acquires Python GIL only when forwarding to Python's logging system
// - Mirrors signal_handler.rs pattern exactly
//
// Performance:
// - receiver.recv().await: Non-blocking, waits for next log
// - Python::attach(): Acquires GIL only during forwarding (not during Rust logging!)
// - Result: Zero GIL contention from Rust logging threads

use log::debug;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::python::channel_logger::LogRecord;
use crate::runtime::RuntimeHandle;

/// Spawn a background task to process log records from Rust and forward to Python
///
/// This task runs for the lifetime of the application, receiving log records
/// from the channel and forwarding them to Python's logging system.
///
/// # Arguments
/// * `receiver` - Receiver end of unbounded channel from ChannelLogger
/// * `runtime_handle` - Tokio runtime handle to spawn the task on
///
/// # How it works
/// 1. Loop indefinitely, waiting for log records
/// 2. When a record arrives, acquire Python GIL
/// 3. Call Python's `logging.getLogger(target).log(level, message)`
/// 4. Release GIL and wait for next record
///
/// # Shutdown
/// Task terminates gracefully when the channel closes (all senders dropped).
/// This happens automatically when the application exits.
pub fn spawn_logger_task(
    mut receiver: UnboundedReceiver<LogRecord>,
    runtime_handle: RuntimeHandle,
) {
    let runtime = runtime_handle.runtime().clone();

    runtime.spawn(async move {
        debug!("Logger task started - forwarding Rust logs to Python");
        let mut log_count: u64 = 0;

        // Main event loop: receive and forward log records
        while let Some(record) = receiver.recv().await {
            log_count += 1;

            // Acquire GIL only for forwarding (not during Rust logging!)
            Python::attach(|py| {
                // Get Python's logging module
                let logging_module = match PyModule::import(py, "logging") {
                    Ok(module) => module,
                    Err(e) => {
                        // Fallback: print to stderr if Python logging unavailable
                        // This should never happen in production, but handle gracefully
                        eprintln!(
                            "Logger task: Failed to import Python logging module: {:?}",
                            e
                        );
                        return;
                    }
                };

                // Get logger for this target (e.g., "keeper_pam_webrtc_rs")
                // This respects Python's logging configuration (stdio_config.py)
                //
                // IMPORTANT: Convert Rust's "::" separator to Python's "." separator
                // Rust: "keeper_pam_webrtc_rs::logger::module"
                // Python: "keeper_pam_webrtc_rs.logger.module"
                // This ensures Python's hierarchical logger config works correctly
                let python_logger_name = record.target.replace("::", ".");

                let logger = match logging_module.call_method1("getLogger", (&python_logger_name,))
                {
                    Ok(logger) => logger,
                    Err(e) => {
                        eprintln!(
                            "Logger task: Failed to get Python logger for '{}': {:?}",
                            python_logger_name, e
                        );
                        return;
                    }
                };

                // Map Rust log level to Python logging level
                // Python logging levels: DEBUG=10, INFO=20, WARNING=30, ERROR=40, CRITICAL=50
                let py_level = match record.level {
                    log::Level::Error => 40, // logging.ERROR
                    log::Level::Warn => 30,  // logging.WARNING
                    log::Level::Info => 20,  // logging.INFO
                    log::Level::Debug => 10, // logging.DEBUG
                    log::Level::Trace => 10, // logging.DEBUG (Python has no TRACE level)
                };

                // Build the full log message with optional metadata
                let full_message = if let (Some(file), Some(line)) = (&record.file, record.line) {
                    format!("{} ({}:{})", record.message, file, line)
                } else {
                    record.message.clone()
                };

                // Call Python: logger.log(level, message)
                // This goes through Python's configured handlers (file, console, etc.)
                if let Err(e) = logger.call_method1("log", (py_level, full_message)) {
                    // Fallback: print to stderr if Python logging fails
                    // This should be rare, but we don't want to lose logs
                    eprintln!(
                        "Logger task: Failed to log to Python (level={}, target={}): {:?}",
                        record.level, record.target, e
                    );
                }
            });
        }

        // Channel closed - all senders dropped
        debug!(
            "Logger task terminated after processing {} log records",
            log_count
        );
    });
}
