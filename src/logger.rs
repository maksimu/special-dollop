use log::Level;
use std::fmt;
use tracing_subscriber::EnvFilter;

#[cfg(not(feature = "python"))]
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};

#[cfg(feature = "python")]
use pyo3::{exceptions::PyRuntimeError, prelude::*};

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

#[cfg_attr(feature = "python", pyfunction)]
#[cfg_attr(feature = "python", pyo3(signature = (logger_name, verbose=None, level=20)))]
pub fn initialize_logger(
    logger_name: &str,
    verbose: Option<bool>,
    level: i32,
) -> Result<(), InitializeLoggerError> {
    let rust_level = convert_py_level_to_tracing_level(level, verbose.unwrap_or(false));

    // Let Python handle all filtering - just set a permissive filter in Rust
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if verbose.unwrap_or(false) {
            // When verbose, pass everything through at TRACE level
            EnvFilter::new("trace")
        } else {
            // Use the requested level as the baseline, let Python do the rest
            EnvFilter::new(rust_level.to_string().to_lowercase())
        }
    });

    // Get the filter's string representation for logging *before* it's consumed
    let filter_str = filter.to_string();

    #[cfg(feature = "python")]
    {
        // Initialize pyo3_log bridge (log crate -> Python)
        match pyo3_log::try_init() {
            Ok(_handle) => {
                log::info!("pyo3_log bridge initialized successfully");
            }
            Err(e) => {
                if e.to_string().contains("already initialized") {
                    log::info!("pyo3_log bridge already initialized");
                } else {
                    return Err(InitializeLoggerError::Pyo3LogError(e.to_string()));
                }
            }
        }

        // Since we've converted all tracing! macros to log! macros, we no longer need LogTracer

        log::info!("Logging bridge setup complete");
    }

    #[cfg(not(feature = "python"))]
    {
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
    }

    log::debug!(
        "Logger initialized for '{}' with level {:?} (effective filter: {})",
        logger_name,
        rust_level,
        filter_str
    );

    Ok(())
}

#[inline]
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
