use std::fmt;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

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

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Define all logging targets once
        let common_targets = [
            // Application logs
            "keeper_pam_webrtc_rs",
            "keeper_pam_webrtc_rs_webrtc",
            "connection_lifecycle",
            "guac_protocol",
            "guac_opcode_debug",
            "guac_opcode_dispatch",
            "guac_special_opcodes",
            "guac_size_instruction_outbound",
            "guac_size_instruction_handshake",
            "guac_error_handling",
            "guac_disconnect",
            "signal_handler",
            "webrtc_lifecycle",
            // Channel and protocol logs
            "channel_setup",
            "channel_flow",
            "channel_health",
            "protocol_parse",
            "protocol_event",
            "registry",
            "ice_config",
            "tube_lifecycle",
            "python_bindings",
            "lifecycle",
        ];

        // WebRTC specific targets (using actual target names from WebRTC library)
        let webrtc_targets = [
            // Actual targets from logs (no dots, use underscores)
            "webrtc_ice",
            "webrtc_ice_connection",
            "webrtc_ice_gathering",
            "webrtc_sdp",
            "webrtc_state",
            "webrtc_state_report",
            "webrtc_sctp",
            "webrtc",
            "webrtc_lifecycle",
        ];

        if verbose.unwrap_or(false) {
            // When verbose is true, use trace level for everything
            let mut filter = EnvFilter::new(format!(
                "{},lifecycle=trace",
                rust_level.to_string().to_lowercase()
            ));

            // Add all targets at trace level
            for target in common_targets.iter().chain(webrtc_targets.iter()) {
                let directive = format!("{}={}", target, "trace");
                if let Ok(d) = directive.parse() {
                    filter = filter.add_directive(d);
                }
            }

            filter
        } else {
            // When verbose is false, use rust_level for normal logs and error for WebRTC
            let mut filter = EnvFilter::new(rust_level.to_string().to_lowercase());

            // Add normal targets at rust_level
            for target in common_targets.iter() {
                let directive = format!("{}={}", target, rust_level.to_string().to_lowercase());
                if let Ok(d) = directive.parse() {
                    filter = filter.add_directive(d);
                }
            }

            // Add WebRTC targets at error level to suppress warnings
            for target in webrtc_targets.iter() {
                let directive = format!("{}={}", target, "error");
                if let Ok(d) = directive.parse() {
                    filter = filter.add_directive(d);
                }
            }

            filter
        }
    });

    // Get the filter's string representation for logging *before* it's consumed
    let filter_str = filter.to_string();

    let subscriber_builder = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_target(true)
        .with_level(true)
        .compact();

    #[cfg(not(feature = "python"))]
    let subscriber = subscriber_builder.pretty().finish();

    #[cfg(feature = "python")]
    let subscriber = subscriber_builder.finish();

    #[cfg(feature = "python")]
    {
        pyo3_log::try_init().map_err(|e| InitializeLoggerError::Pyo3LogError(e.to_string()))?;
    }

    tracing::subscriber::set_global_default(subscriber).map_err(|e| {
        let msg = format!("Logger already initialized or failed to set: {e}");
        tracing::debug!("{}", msg);
        InitializeLoggerError::SetGlobalDefaultError(e.to_string())
    })?;

    tracing::info!(
        module_path = module_path!(),
        target = logger_name,
        "Logger initialized for '{}' with level {:?} (effective filter: {})",
        logger_name,
        rust_level,
        filter_str // Use the stored string representation
    );

    Ok(())
}

#[inline]
fn convert_py_level_to_tracing_level(level: i32, verbose: bool) -> Level {
    if verbose {
        return Level::TRACE;
    }
    match level {
        50 | 40 => Level::ERROR, // CRITICAL, ERROR
        30 => Level::WARN,       // WARNING
        20 => Level::INFO,       // INFO
        10 => Level::DEBUG,      // DEBUG
        _ => Level::TRACE,       // NOTSET or other values
    }
}
