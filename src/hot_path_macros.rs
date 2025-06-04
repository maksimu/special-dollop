// Hot path performance macros
// These macros eliminate string formatting overhead when logging is disabled

/// Performance-optimized debug macro for hot paths
/// Only performs tracing work if DEBUG level is enabled
#[macro_export]
macro_rules! debug_hot_path {
    ($($arg:tt)*) => {
        if tracing::enabled!(tracing::Level::DEBUG) {
            tracing::debug!($($arg)*);
        }
    };
}

/// Performance-optimized trace macro for hot paths  
/// Only performs tracing work if TRACE level is enabled
#[macro_export]
macro_rules! trace_hot_path {
    ($($arg:tt)*) => {
        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!($($arg)*);
        }
    };
}

/// Performance-optimized warn macro for hot paths
/// Only performs tracing work if WARN level is enabled  
#[macro_export]
macro_rules! warn_hot_path {
    ($($arg:tt)*) => {
        if tracing::enabled!(tracing::Level::WARN) {
            tracing::warn!($($arg)*);
        }
    };
}

/// Performance-optimized info macro for hot paths
/// Only performs tracing work if INFO level is enabled
#[macro_export]
macro_rules! info_hot_path {
    ($($arg:tt)*) => {
        if tracing::enabled!(tracing::Level::INFO) {
            tracing::info!($($arg)*);
        }
    };
} 