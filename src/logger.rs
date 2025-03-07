use log::{Metadata, Record, LevelFilter};
use std::sync::atomic::{AtomicBool, Ordering};
use once_cell::sync::Lazy;

// Static flag to track whether logger has been initialized using once_cell instead of lazy_static
static LOGGER_INITIALIZED: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

// Python-dependent logger
#[cfg(feature = "python")]
mod python_logger {
    use super::*;
    use pyo3::{pyfunction, PyErr, PyObject, PyResult, Python};
    use pyo3::exceptions::{PyRuntimeError};
    use pyo3::prelude::{PyAnyMethods, PyModule};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use std::sync::Arc;
    use crate::get_or_create_runtime_py;
    use parking_lot::RwLock;
    use std::collections::VecDeque;

    // Constant for the queue size to avoid unbound memory growth
    const LOG_BUFFER_SIZE: usize = 1000;

    enum LoggerMessage {
        // Special message for logger initialization
        Initialize(PyObject),
        // Normal log message
        LogMessage(String, String),
    }

    // Cache structure for batching logs before Python logger is initialized
    struct LogCache {
        messages: VecDeque<(String, String)>,
        initialized: bool,
    }

    impl LogCache {
        fn new() -> Self {
            Self {
                messages: VecDeque::with_capacity(LOG_BUFFER_SIZE),
                initialized: false,
            }
        }

        fn add(&mut self, level: String, message: String) {
            // If we're at capacity, print the oldest message to stderr and remove it
            if self.messages.len() >= LOG_BUFFER_SIZE {
                if let Some((old_level, old_message)) = self.messages.pop_front() {
                    eprintln!("[BUFFER FULL][{}] {}", old_level, old_message);
                }
            }
            self.messages.push_back((level, message));
        }

        fn drain(&mut self) -> Vec<(String, String)> {
            // Create a properly sized result vector
            let mut result = Vec::with_capacity(self.messages.len());

            // Drain the queue into the vector
            while let Some(item) = self.messages.pop_front() {
                result.push(item);
            }

            result
        }
    }

    #[derive(Clone)]
    pub struct RustPythonLogger {
        sender: Arc<mpsc::UnboundedSender<LoggerMessage>>,
        module_filter: String,
        verbose: bool,
        _task_handle: Arc<JoinHandle<()>>,
        // Use a shared cache for logs before Python logger is initialized
        log_cache: Arc<RwLock<LogCache>>,
    }

    impl RustPythonLogger {
        pub fn new(module_filter: String, verbose: bool) -> Result<Self, PyErr> {
            // Get the shared runtime
            let runtime = get_or_create_runtime_py()?;

            // Create an unbounded channel to prevent blocking
            let (sender, receiver) = mpsc::unbounded_channel::<LoggerMessage>();
            let sender = Arc::new(sender);

            // Create log cache
            let log_cache = Arc::new(RwLock::new(LogCache::new()));
            let log_cache_clone = Arc::clone(&log_cache);

            // Spawn a dedicated thread for logging that won't deadlock with other operations
            let _handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                // Run a dedicated runtime for logging
                rt.block_on(async {
                    process_log_messages(receiver, log_cache_clone).await;
                });
            });

            // Create a dummy task handle that will be used for cleanup
            let task_handle = Arc::new(runtime.spawn(async {
                // This task doesn't do anything - real work happens in the dedicated thread
            }));

            Ok(RustPythonLogger {
                sender,
                module_filter,
                verbose,
                _task_handle: task_handle,
                log_cache,
            })
        }
    }

    // Process log messages in a separate async function to keep ownership simple
    async fn process_log_messages(
        mut receiver: mpsc::UnboundedReceiver<LoggerMessage>,
        log_cache: Arc<RwLock<LogCache>>,
    ) {
        // The Python logger reference (outside the loop to avoid ownership issues)
        let mut python_logger: Option<PyObject> = None;

        while let Some(message) = receiver.recv().await {
            match message {
                LoggerMessage::Initialize(logger_obj) => {
                    // Store the Python logger
                    python_logger = Some(logger_obj);
                    log::debug!("Python logger initialized successfully");

                    // Process any cached messages
                    Python::with_gil(|py| {
                        let mut cache = log_cache.write();
                        if !cache.initialized {
                            // Drain all cached messages
                            let cached_messages = cache.drain();

                            // Log them with the now-initialized logger
                            if let Some(ref logger) = python_logger {
                                for (level, message) in cached_messages {
                                    let method = match &*level {
                                        "ERROR" => "error",
                                        "WARN" => "warning",
                                        "INFO" => "info",
                                        "DEBUG" => "debug",
                                        "TRACE" => "debug",
                                        _ => "info"
                                    };

                                    if let Err(e) = logger.call_method1(py, method, (message,)) {
                                        eprintln!("Failed to log cached message: {:?}", e);
                                    }
                                }
                            }

                            // Mark as initialized so we don't try to cache more messages
                            cache.initialized = true;
                        }
                    });
                },
                LoggerMessage::LogMessage(level, message) => {
                    // Check if we have a logger or need to cache
                    let should_cache = {
                        let cache = log_cache.read();
                        !cache.initialized
                    };

                    if should_cache {
                        // Cache the message for later
                        let mut cache = log_cache.write();
                        if !cache.initialized {
                            cache.add(level.clone(), message.clone());
                            // Print to stderr for visibility during init
                            eprintln!("[{}] {}", level, message);
                            continue;
                        }
                    }

                    // Only try to log if we have a logger
                    if let Some(ref logger) = python_logger {
                        Python::with_gil(|py| {
                            let method = match &*level {
                                "ERROR" => "error",
                                "WARN" => "warning",
                                "INFO" => "info",
                                "DEBUG" => "debug",
                                "TRACE" => "debug",
                                _ => "info"
                            };

                            if let Err(e) = logger.call_method1(py, method, (message,)) {
                                eprintln!("Failed to log message: {:?}", e);
                            }
                        });
                    } else {
                        // If logger completely missing, print to stderr
                        eprintln!("[{}] {}", level, message);
                    }
                }
            }
        }
        log::debug!("Logger task terminated");
    }

    impl log::Log for RustPythonLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            // Fast path for common case
            if self.verbose {
                return true;
            }

            // Get the log level and source
            let level = metadata.level();
            let source = metadata.target();

            // Filter out noisy TURN/ICE traffic at DEBUG level
            if level == log::Level::Debug && (source.contains("turn::") || source.contains( "webrtc_sctp::")) {
                    return false;
            }
            source.starts_with(&self.module_filter)
        }

        fn log(&self, record: &Record) {
            // Check if enabled first to avoid unnecessary work
            if !self.enabled(record.metadata()) {
                return;
            }

            let level = match record.level() {
                log::Level::Error => "ERROR",
                log::Level::Warn => "WARN",
                log::Level::Info => "INFO",
                log::Level::Debug => "DEBUG",
                log::Level::Trace => "TRACE",
            };

            // Format the message only if we'll actually log it
            let message = format!("{}", record.args());

            // Check if we should cache this message locally
            let should_cache = {
                let cache = self.log_cache.read();
                !cache.initialized
            };

            if should_cache {
                // Store in the local cache for when Python logger is ready
                let mut cache = self.log_cache.write();
                if !cache.initialized {
                    cache.add(level.to_string(), message.clone());
                }
            }

            // Try to send non-blocking first to improve performance
            // This won't block because we're using an unbounded channel
            if let Err(e) = self.sender.send(LoggerMessage::LogMessage(level.to_string(), message.clone())) {
                // If channel closed, just print to stderr as fallback
                eprintln!("Logger channel closed: {}. Message was: [{}] {}",
                          e, level, message);
            }
        }

        fn flush(&self) {}
    }

    impl Drop for RustPythonLogger {
        fn drop(&mut self) {
            // Only cleanup if this is the last reference
            if Arc::strong_count(&self.sender) == 1 {
                // Abort the task when logger is dropped
                self._task_handle.abort();
            }
        }
    }

    #[pyfunction]
    #[pyo3(signature = (logger_name, verbose=None, level=20))]
    pub fn initialize_logger(logger_name: &str, verbose: Option<bool>, level: i32) -> PyResult<()> {
        // Check if logger has already been initialized with relaxed ordering for better performance
        if LOGGER_INITIALIZED.load(Ordering::Relaxed) {
            // Already initialized, just return success
            return Ok(());
        }

        // Double-check with acquire to ensure visibility
        if LOGGER_INITIALIZED.load(Ordering::Acquire) {
            return Ok(());
        }

        Python::with_gil(|py| {
            // Create Python logger instance
            let logging = PyModule::import(py, "logging")?;
            let logger = logging.call_method1("getLogger", (logger_name,))?;
            logger.call_method1("setLevel", (level,))?;

            // Create the Rust logger
            let rust_logger = RustPythonLogger::new(
                logger_name.to_string(),
                verbose.unwrap_or(false)
            )?;

            // Set up the logger
            if let Err(e) = log::set_boxed_logger(Box::new(rust_logger.clone())) {
                // Check if the error is because logger is already initialized
                if e.to_string().contains("already initialized") {
                    // Mark as initialized and continue
                    LOGGER_INITIALIZED.store(true, Ordering::Release);
                    return Ok(());
                }

                return Err(PyErr::new::<PyRuntimeError, _>(
                    format!("Failed to initialize logger: {}", e)
                ));
            }

            // Send the Python logger object through the channel
            let py_logger = logger.into();
            if let Err(e) = rust_logger.sender.send(LoggerMessage::Initialize(py_logger)) {
                return Err(PyErr::new::<PyRuntimeError, _>(
                    format!("Failed to send logger initialization: {}", e)
                ));
            }

            let rust_level = convert_log_level(level, verbose.unwrap_or(false));
            log::set_max_level(rust_level);

            // Mark logger as initialized with release ordering
            LOGGER_INITIALIZED.store(true, Ordering::Release);

            Ok(())
        })
    }
}

// Non-Python logger for tests and non-Python builds
#[cfg(not(feature = "python"))]
mod simple_logger {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Counter for rate limiting
    static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(0);
    static LAST_LOG_CLEANUP: AtomicU64 = AtomicU64::new(0);
    const LOG_CLEANUP_INTERVAL: u64 = 1000; // Check every 1000 logs

    pub struct SimpleLogger {
        level: LevelFilter,
        module_filter: Option<String>,
        verbose: bool,
    }

    impl SimpleLogger {
        pub fn new(level: LevelFilter, module_filter: Option<String>, verbose: bool) -> Self {
            SimpleLogger {
                level,
                module_filter,
                verbose,
            }
        }
    }

    impl log::Log for SimpleLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            // Fast path for level check
            if metadata.level() > self.level {
                return false;
            }

            // If verbose, all modules are enabled
            if self.verbose {
                return true;
            }

            // Filter by module if filter is specified
            let source = metadata.target();
            match &self.module_filter {
                Some(filter) => source.starts_with(filter),
                None => true
            }
        }

        fn log(&self, record: &Record) {
            // First check if enabled to avoid formatting
            if !self.enabled(record.metadata()) {
                return;
            }

            // Get current timestamp once
            let now = chrono::Local::now();
            let timestamp = now.format("%Y-%m-%d %H:%M:%S%.3f");

            // Source module path
            let source = record.module_path().unwrap_or_else(|| record.target());

            // Format message only once
            let message = format!("{} {} [{}] {}",
                                  timestamp, record.level(), source, record.args());

            // Write to stderr
            let _ = writeln!(std::io::stderr(), "{}", message);

            // Clean up counter occasionally to prevent overflow
            let count = MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
            if count % LOG_CLEANUP_INTERVAL == 0 {
                let last_cleanup = LAST_LOG_CLEANUP.load(Ordering::Relaxed);
                let now_secs = now.timestamp() as u64;

                // Only cleanup if at least an hour has passed
                if now_secs - last_cleanup > 3600 {
                    if LAST_LOG_CLEANUP.compare_exchange(
                        last_cleanup, now_secs, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                        // Reset counter to prevent overflow
                        MESSAGE_COUNTER.store(0, Ordering::Relaxed);
                    }
                }
            }
        }

        fn flush(&self) {
            let _ = std::io::stderr().flush();
        }
    }
}

// Helper function to convert Python log level to Rust log level
#[inline]
fn convert_log_level(level: i32, verbose: bool) -> LevelFilter {
    // Fast path for verbose mode
    if verbose {
        return LevelFilter::Trace;
    }

    // Fast path for common levels
    match level {
        50 | 40 => LevelFilter::Error,
        30 => LevelFilter::Warn,
        20 => LevelFilter::Info,
        10 => LevelFilter::Debug,
        0 => LevelFilter::Trace,
        _ => {
            // Uncommon levels
            if level > 40 {
                LevelFilter::Error
            } else if level > 30 {
                LevelFilter::Warn
            } else if level > 20 {
                LevelFilter::Info
            } else if level > 10 {
                LevelFilter::Debug
            } else {
                LevelFilter::Trace
            }
        }
    }
}

// Expose the appropriate initialize_logger function
#[cfg(feature = "python")]
pub use python_logger::initialize_logger;

// Non-Python logger initialization
#[cfg(not(feature = "python"))]
pub fn initialize_logger(logger_name: &str, verbose: Option<bool>, level: i32) -> Result<(), String> {
    // Check if logger has already been initialized with relaxed ordering for performance
    if LOGGER_INITIALIZED.load(Ordering::Relaxed) {
        // Already initialized, just return success
        return Ok(());
    }

    // Double-check with acquire for proper visibility
    if LOGGER_INITIALIZED.load(Ordering::Acquire) {
        return Ok(());
    }

    // Convert Python log level to Rust log level
    let rust_level = convert_log_level(level, verbose.unwrap_or(false));

    // Create and set up the simple logger
    let logger = simple_logger::SimpleLogger::new(
        rust_level,
        Some(logger_name.to_string()),
        verbose.unwrap_or(false)
    );

    if let Err(e) = log::set_boxed_logger(Box::new(logger)) {
        // Check if the error is because logger is already initialized
        if e.to_string().contains("already initialized") {
            // Mark as initialized and continue
            LOGGER_INITIALIZED.store(true, Ordering::Release);
            return Ok(());
        }

        return Err(format!("Failed to initialize logger: {}", e));
    }

    log::set_max_level(rust_level);

    // Log the initialization
    log::info!("Initialized simple logger for '{}' with level {}", logger_name, level);

    // Mark logger as initialized with release ordering for visibility
    LOGGER_INITIALIZED.store(true, Ordering::Release);

    Ok(())
}