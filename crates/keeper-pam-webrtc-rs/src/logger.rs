use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "python-support"))]
use log::Level;

#[cfg(not(feature = "python-support"))]
use tracing_subscriber::EnvFilter;

#[cfg(not(feature = "python-support"))]
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};

#[cfg(feature = "python-support")]
use pyo3::{exceptions::PyRuntimeError, prelude::*};

#[cfg(feature = "python-support")]
use crate::python::channel_logger::create_channel_logger;

#[cfg(feature = "python-support")]
use crate::python::logger_task::spawn_logger_task;

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
#[cfg_attr(feature = "python-support", pyfunction)]
pub fn set_verbose_logging(enabled: bool) {
    VERBOSE_LOGGING.store(enabled, Ordering::Relaxed);
    log::debug!(
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
    SetLoggerError(String),
    SetGlobalDefaultError(String),
}

impl fmt::Display for InitializeLoggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitializeLoggerError::SetLoggerError(e) => {
                write!(f, "Failed to set global logger: {e}")
            }
            InitializeLoggerError::SetGlobalDefaultError(e) => write!(
                f,
                "Logger already initialized or failed to set global default subscriber: {e}",
            ),
        }
    }
}

impl std::error::Error for InitializeLoggerError {}

#[cfg(feature = "python-support")]
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
///   **NOTE**: In Python mode, this is IGNORED. Rust always passes all logs
///   to Python at TRACE level, and Python's logger config controls filtering.
///   Only used in standalone Rust mode.
///
/// # Python Integration
/// When running in gateway (Python mode):
/// - Rust passes ALL logs to Python at TRACE level
/// - Python filters based on stdio_config.py logger configuration:
///   - keeper_pam_webrtc_rs: DEBUG (if --debug) or INFO
///   - webrtc_ice, webrtc_sctp, etc.: WARNING (suppresses spam)
/// - Verbose-gated logs still controlled by `is_verbose_logging()` checks in Rust
#[cfg_attr(feature = "python-support", pyfunction)]
#[cfg_attr(feature = "python-support", pyo3(signature = (logger_name, verbose=None, level=20)))]
pub fn initialize_logger(
    logger_name: &str,
    verbose: Option<bool>,
    level: i32,
) -> Result<(), InitializeLoggerError> {
    let is_verbose = verbose.unwrap_or(false);

    // Check environment variable for WebRTC library logging
    let include_webrtc_logs = crate::config::include_webrtc_logs();

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

    #[cfg(feature = "python-support")]
    {
        // Convert Python level to Rust LevelFilter
        let level_filter = match level {
            50 | 40 => log::LevelFilter::Error,
            30 => log::LevelFilter::Warn,
            20 => log::LevelFilter::Info,
            10 => log::LevelFilter::Debug,
            _ => log::LevelFilter::Trace,
        };

        // Create channel-based logger and receiver
        // This logger sends log records to an unbounded channel (NO GIL!)
        // A background task receives them and forwards to Python's logging system
        let (channel_logger, log_receiver) = create_channel_logger(level_filter);

        // Install as global logger
        log::set_boxed_logger(Box::new(channel_logger))
            .map_err(|e| InitializeLoggerError::SetLoggerError(e.to_string()))?;

        log::set_max_level(level_filter);

        // Spawn background task to forward logs to Python
        // This task acquires the GIL only when forwarding logs, not during Rust logging!
        let runtime_handle = crate::runtime::get_runtime();
        spawn_logger_task(log_receiver, runtime_handle);

        log::debug!(
            "Channel-based logger initialized for '{}' with level {:?}",
            logger_name,
            level_filter
        );

        // **NOTE ON WEBRTC SPAM SUPPRESSION**:
        // We intentionally removed per-target filtering from the Rust logger.
        // Instead, Python's stdio_config.py handles this via logger configuration:
        //   'webrtc_ice': LogInfo(logging.WARNING),
        //   'webrtc_dtls': LogInfo(logging.WARNING),
        //   etc.
        // This approach is simpler and respects Python as the "owner" of logging.
        // If performance becomes an issue, we can add per-target filtering back.

        set_verbose_logging(is_verbose);
        set_webrtc_logging(include_webrtc_logs);
    }

    #[cfg(not(feature = "python-support"))]
    {
        let rust_level = convert_py_level_to_tracing_level(level, is_verbose);

        // Check environment variable for WebRTC library logging (non-Python mode)
        let include_webrtc_logs_standalone = crate::config::include_webrtc_logs();

        // Non-Python mode: Use EnvFilter to control what gets logged
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            if is_verbose {
                // Verbose mode: show application logs, but still gate WebRTC unless explicitly enabled
                if include_webrtc_logs_standalone {
                    EnvFilter::new("trace")
                } else {
                    EnvFilter::new("trace,webrtc=error,webrtc_ice=error,webrtc_mdns=error,webrtc_dtls=error,webrtc_sctp=error,turn=error,stun=error,ironrdp_session=debug,ironrdp_pdu=debug,ironrdp_graphics=debug")
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
                    stun=error,\
                    ironrdp_session=error,\
                    ironrdp_pdu=error,\
                    ironrdp_graphics=error"
                );
                EnvFilter::new(&filter_str)
            }
        });

        let subscriber = FmtSubscriber::builder()
            .with_env_filter(filter)
            .with_span_events(FmtSpan::CLOSE)
            .with_target(true)
            .with_level(true)
            .with_ansi(false)
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

    // Log configuration in debug/verbose mode only
    if is_verbose {
        log::debug!("=== Gateway WebRTC Configuration (Verbose Mode) ===");
        log::debug!(
            "Backend flush timeout: {:?}",
            crate::config::backend_flush_timeout()
        );
        log::debug!(
            "Max flush failures: {}",
            crate::config::max_flush_failures()
        );
        log::debug!(
            "Channel shutdown grace: {:?}",
            crate::config::channel_shutdown_grace_period()
        );
        log::debug!(
            "Data channel close timeout: {:?}",
            crate::config::data_channel_close_timeout()
        );
        log::debug!(
            "Peer connection close timeout: {:?}",
            crate::config::peer_connection_close_timeout()
        );
        log::debug!(
            "Disconnect to EOF delay: {:?}",
            crate::config::disconnect_to_eof_delay()
        );
        log::debug!(
            "ICE gather timeout: {:?}",
            crate::config::ice_gather_timeout()
        );
        log::debug!(
            "ICE restart answer timeout: {:?}",
            crate::config::ice_restart_answer_timeout()
        );
        log::debug!(
            "ICE disconnected wait: {:?}",
            crate::config::ice_disconnected_wait()
        );
        log::debug!("Activity timeout: {:?}", crate::config::activity_timeout());
        log::debug!(
            "Stale tube sweep interval: {:?}",
            crate::config::stale_tube_sweep_interval()
        );
        log::debug!(
            "Max concurrent creates: {}",
            crate::config::max_concurrent_creates()
        );
        log::debug!(
            "Router HTTP timeout: {:?}",
            crate::config::router_http_timeout()
        );
        log::debug!(
            "Tube creation timeout: {:?}",
            crate::config::tube_creation_timeout()
        );
        log::debug!(
            "Router circuit breaker threshold: {}",
            crate::config::router_circuit_breaker_threshold()
        );
        log::debug!(
            "Router circuit breaker cooldown: {:?}",
            crate::config::router_circuit_breaker_cooldown()
        );
        log::debug!(
            "Include WebRTC logs: {}",
            crate::config::include_webrtc_logs()
        );
        log::debug!("=============================================");
    }

    Ok(())
}

#[inline]
#[cfg(not(feature = "python-support"))] // Only compiled in standalone mode (not used in Python builds)
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
