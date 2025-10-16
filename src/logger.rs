use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "python"))]
use log::Level;

#[cfg(not(feature = "python"))]
use tracing_subscriber::EnvFilter;

#[cfg(not(feature = "python"))]
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};

#[cfg(feature = "python")]
use pyo3::{exceptions::PyRuntimeError, prelude::*};

/// Global flag for verbose logging (gated detailed logs)
/// Defaults to false for optimal performance - detailed logs only when explicitly enabled
pub static VERBOSE_LOGGING: AtomicBool = AtomicBool::new(false);

/// Global flag for including WebRTC library logs (separate from verbose)
/// Defaults to false - WebRTC library logs are EXTREMELY noisy (1000+ logs/sec)
/// Set KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS=1 to enable for deep protocol debugging
pub static INCLUDE_WEBRTC_LOGS: AtomicBool = AtomicBool::new(false);

/// Global flag tracking if logger has been initialized
/// Used to make initialize_logger() idempotent for Commander compatibility
static LOGGER_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Check if verbose logging is enabled (optimized for false case)
#[inline(always)]
pub fn is_verbose_logging() -> bool {
    VERBOSE_LOGGING.load(Ordering::Relaxed)
}

/// Set verbose logging flag (callable from Python)
#[cfg_attr(feature = "python", pyo3::pyfunction)]
pub fn set_verbose_logging(enabled: bool) {
    VERBOSE_LOGGING.store(enabled, Ordering::Relaxed);
    log::info!(
        "Verbose logging {}",
        if enabled { "enabled" } else { "disabled" }
    );
}

/// Set WebRTC library logging flag (internal use only - set once at init)
fn set_webrtc_logging(enabled: bool) {
    INCLUDE_WEBRTC_LOGS.store(enabled, Ordering::Relaxed);
    if enabled {
        log::warn!(
            "WebRTC library logging ENABLED - expect 1000+ logs/sec from webrtc-rs internals"
        );
    }
}

// Custom error type for logger initialization
#[derive(Debug)]
pub enum InitializeLoggerError {
    Pyo3LogError(String),
    SetGlobalDefaultError(String),
}

impl fmt::Display for InitializeLoggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitializeLoggerError::Pyo3LogError(e) => {
                write!(f, "Failed to initialize pyo3-log: {e}")
            }
            InitializeLoggerError::SetGlobalDefaultError(e) => write!(
                f,
                "Logger already initialized or failed to set global default subscriber: {e}",
            ),
        }
    }
}

impl std::error::Error for InitializeLoggerError {}

#[cfg(feature = "python")]
impl From<InitializeLoggerError> for PyErr {
    fn from(err: InitializeLoggerError) -> PyErr {
        PyRuntimeError::new_err(err.to_string())
    }
}

/// Initialize logger for keeper-pam-webrtc-rs
///
/// # Arguments
/// * `logger_name` - Logger name (for debug messages only)
/// * `verbose` - Enable verbose-gated debug logs (controls `is_verbose_logging()`)
/// * `level` - Log level (Python int: 10=DEBUG, 20=INFO, etc.)
///             **NOTE**: In Python mode, this is IGNORED. Rust always passes all logs
///             to Python at TRACE level, and Python's logger config controls filtering.
///             Only used in standalone Rust mode.
///
/// # Python Integration
/// When running in gateway (Python mode):
/// - Rust passes ALL logs to Python at TRACE level
/// - Python filters based on stdio_config.py logger configuration:
///   - keeper_pam_webrtc_rs: DEBUG (if --debug) or INFO
///   - webrtc_ice, webrtc_sctp, etc.: WARNING (suppresses spam)
/// - Verbose-gated logs still controlled by `is_verbose_logging()` checks in Rust
#[cfg_attr(feature = "python", pyfunction)]
#[cfg_attr(feature = "python", pyo3(signature = (logger_name, verbose=None, level=20)))]
pub fn initialize_logger(
    logger_name: &str,
    verbose: Option<bool>,
    level: i32,
) -> Result<(), InitializeLoggerError> {
    let is_verbose = verbose.unwrap_or(false);

    // Check environment variable for WebRTC library logging
    let include_webrtc_logs = std::env::var("KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    // IDEMPOTENT: If already initialized, just update flags and return
    // This makes Commander's repeated calls safe and efficient
    if LOGGER_INITIALIZED.swap(true, Ordering::SeqCst) {
        set_verbose_logging(is_verbose);
        set_webrtc_logging(include_webrtc_logs);
        log::debug!(
            "Logger already initialized for '{}', updated verbose={}, webrtc_logs={}",
            logger_name,
            is_verbose,
            include_webrtc_logs
        );
        return Ok(());
    }

    #[cfg(feature = "python")]
    {
        // Convert Python level to Rust LevelFilter
        let level_filter = match level {
            50 | 40 => log::LevelFilter::Error,
            30 => log::LevelFilter::Warn,
            20 => log::LevelFilter::Info,
            10 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };

        // Initialize pyo3_log with explicit filter
        Python::attach(|py| -> Result<(), InitializeLoggerError> {
            // Create the logger with global level filter
            let mut logger = pyo3_log::Logger::new(py, pyo3_log::Caching::Loggers)
                .map_err(|e| InitializeLoggerError::Pyo3LogError(e.to_string()))?
                .filter(level_filter);

            // **CRITICAL: Suppress WebRTC ecosystem spam unless explicitly requested**
            // WebRTC + dependencies have 1000+ debug/trace logs that fire constantly (per-packet!)
            // TURN client dumps full packet data: "try_send data = [0, 1, 0, 88, ...]" (200 bytes/log!)
            // Without suppression: 60 gateways * 40 packets/sec * 3 logs = 36GB+ in Docker logs
            // With suppression: Only errors from these crates pass through
            //
            // NOTE: Controlled by KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS env var, NOT verbose flag
            // This keeps WebRTC library debugging separate from application verbose logging
            if !include_webrtc_logs {
                logger = logger
                    // WebRTC core library
                    .filter_target("webrtc".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_ice".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_sctp".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_dtls".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_mdns".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_data".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_srtp".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_media".to_owned(), log::LevelFilter::Error)
                    .filter_target("webrtc_util".to_owned(), log::LevelFilter::Error)
                    // TURN/STUN libraries (MASSIVE spam - logs full packet data!)
                    .filter_target("turn".to_owned(), log::LevelFilter::Error)
                    .filter_target("stun".to_owned(), log::LevelFilter::Error)
                    // RTP/RTCP libraries
                    .filter_target("rtp".to_owned(), log::LevelFilter::Error)
                    .filter_target("rtcp".to_owned(), log::LevelFilter::Error);
            }

            // Install the configured logger
            logger
                .install()
                .map_err(|e| InitializeLoggerError::SetGlobalDefaultError(e.to_string()))?;

            Ok(()) // Explicitly return Ok(()) after discarding the ResetHandle
        })?;

        log::debug!(
            "pyo3_log bridge initialized for '{}' with level {:?}, webrtc_* suppression: {}",
            logger_name,
            level_filter,
            !include_webrtc_logs
        );

        set_verbose_logging(is_verbose);
        set_webrtc_logging(include_webrtc_logs);
    }

    #[cfg(not(feature = "python"))]
    {
        let rust_level = convert_py_level_to_tracing_level(level, is_verbose);

        // Check environment variable for WebRTC library logging (non-Python mode)
        let include_webrtc_logs_standalone = std::env::var("KEEPER_GATEWAY_INCLUDE_WEBRTC_LOGS")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        // Non-Python mode: Use EnvFilter to control what gets logged
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if is_verbose {
                // Verbose mode: show application logs, but still gate WebRTC unless explicitly enabled
                if include_webrtc_logs_standalone {
                    EnvFilter::new("trace")
                } else {
                    EnvFilter::new("trace,webrtc=error,webrtc_ice=error,webrtc_mdns=error,webrtc_dtls=error,webrtc_sctp=error,turn=error,stun=error")
                }
            } else {
                // Suppress webrtc dependency spam while allowing keeper_pam_webrtc_rs
                let keeper_level = rust_level.to_string().to_lowercase();
                let filter_str = format!(
                    "{keeper_level},\
                    webrtc=error,\
                    webrtc_ice=error,\
                    webrtc_mdns=error,\
                    webrtc_dtls=error,\
                    webrtc_sctp=error,\
                    turn=error,\
                    stun=error"
                );
                EnvFilter::new(&filter_str)
            }
        });

        let subscriber = FmtSubscriber::builder()
            .with_env_filter(filter)
            .with_span_events(FmtSpan::CLOSE)
            .with_target(true)
            .with_level(true)
            .compact()
            .finish();

        tracing::subscriber::set_global_default(subscriber).map_err(|e| {
            let msg = format!("Logger already initialized or failed to set: {e}");
            tracing::debug!("{}", msg);
            InitializeLoggerError::SetGlobalDefaultError(e.to_string())
        })?;

        // Set global flags AFTER subscriber is ready (for non-Python logging)
        set_verbose_logging(is_verbose);
        set_webrtc_logging(include_webrtc_logs_standalone);

        log::debug!(
            "Logger initialized for '{}' (verbose: {}, webrtc_logs: {})",
            logger_name,
            is_verbose,
            include_webrtc_logs_standalone
        );
    }

    Ok(())
}

#[inline]
#[cfg(not(feature = "python"))] // Only compiled in standalone mode (not used in Python builds)
fn convert_py_level_to_tracing_level(level: i32, verbose: bool) -> Level {
    if verbose {
        return Level::Trace;
    }
    match level {
        50 | 40 => Level::Error, // CRITICAL, ERROR
        30 => Level::Warn,       // WARNING
        20 => Level::Info,       // INFO
        10 => Level::Debug,      // DEBUG
        _ => Level::Trace,       // NOTSET or other values
    }
}
