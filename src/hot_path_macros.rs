// Hot path performance macros
// These macros are ALWAYS OPTIMIZED for maximum speed with minimal overhead

/// Performance-optimized debug macro for hot paths
/// Uses branch prediction hints for near-zero overhead when disabled
#[macro_export]
macro_rules! debug_hot_path {
    ($($arg:tt)*) => {
        #[cfg(not(feature = "disable_hot_path_logging"))]
        {
            // **FAST PATH**: Inline branch prediction hint - debug is rarely enabled in hot paths
            #[cold]
            fn cold_debug() {}

            let enabled = log::log_enabled!(log::Level::Debug);
            if enabled {
                log::debug!($($arg)*);
            } else {
                cold_debug(); // Mark the false case as cold for better prediction
            }
        }
    };
}

/// Performance-optimized trace macro for hot paths  
/// Uses branch prediction hints for near-zero overhead when disabled
#[macro_export]
macro_rules! trace_hot_path {
    ($($arg:tt)*) => {
        #[cfg(not(feature = "disable_hot_path_logging"))]
        {
            // **FAST PATH**: Inline branch prediction hint - trace is usually disabled in production
            #[cold]
            fn cold_trace() {}

            let enabled = log::log_enabled!(log::Level::Trace);
            if enabled {
                log::trace!($($arg)*);
            } else {
                cold_trace(); // Mark the false case as cold for better prediction
            }
        }
    };
}

/// Performance-optimized warn macro for hot paths
/// Warnings are more common so no branch prediction hint
#[macro_export]
macro_rules! warn_hot_path {
    ($($arg:tt)*) => {
        #[cfg(not(feature = "disable_hot_path_logging"))]
        {
            if log::log_enabled!(log::Level::Warn) {
                log::warn!($($arg)*);
            }
        }
    };
}

/// Performance-optimized info macro for hot paths
/// Info logging has moderate probability so no branch prediction hint
#[macro_export]
macro_rules! info_hot_path {
    ($($arg:tt)*) => {
        #[cfg(not(feature = "disable_hot_path_logging"))]
        {
            if log::log_enabled!(log::Level::Info) {
                log::info!($($arg)*);
            }
        }
    };
}

/// Ultra-performance trace macro for the most critical hot paths
/// Only enabled in debug builds unless production_debug is explicitly enabled  
#[macro_export]
macro_rules! trace_ultra_hot_path {
    ($($arg:tt)*) => {
        #[cfg(not(feature = "disable_hot_path_logging"))]
        {
            #[cfg(any(debug_assertions, feature = "production_debug"))]
            {
                #[cold]
                fn cold_ultra_trace() {}

                let enabled = log::log_enabled!(log::Level::Trace);
                if enabled {
                    log::trace!($($arg)*);
                } else {
                    cold_ultra_trace(); // Mark the false case as cold
                }
            }
        }
    };
}

/// Critical error logging that's always enabled
/// Errors are always logged regardless of feature flags
#[macro_export]
macro_rules! error_hot_path {
    ($($arg:tt)*) => {
        if log::log_enabled!(log::Level::Error) {
            log::error!($($arg)*);
        }
    };
}

/// Production-optimized logging that only runs if explicitly enabled
#[macro_export]
macro_rules! production_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "production_debug")]
        {
            #[cold]
            fn cold_production_debug() {}

            let enabled = log::log_enabled!(log::Level::Debug);
            if enabled {
                log::debug!($($arg)*);
            } else {
                cold_production_debug(); // Mark the false case as cold
            }
        }
    };
}

// ================================
// BRANCH PREDICTION OPTIMIZATIONS
// ================================
// **ALWAYS ENABLED** - No feature flags needed

/// Branch prediction hint that a condition is likely to be true
/// Helps the CPU optimize pipeline and caching for the common case
#[macro_export]
macro_rules! likely {
    ($cond:expr) => {{
        #[cold]
        fn cold_fn() {}

        let result = $cond;
        if !result {
            cold_fn(); // Mark the false case as cold
        }
        result
    }};
}

/// Branch prediction hint that a condition is unlikely to be true  
/// Helps the CPU optimize pipeline and caching for the error case
#[macro_export]
macro_rules! unlikely {
    ($cond:expr) => {{
        #[cold]
        fn cold_fn() {}

        let result = $cond;
        if result {
            cold_fn(); // Mark the true case as cold
        }
        result
    }};
}

/// Mark a function as "hot" for aggressive optimization
/// **ALWAYS ENABLED** for maximum performance
#[macro_export]
macro_rules! hot_function {
    (fn $name:ident($($args:tt)*) -> $ret:ty $body:block) => {
        #[inline(always)] // Always inline hot functions
        fn $name($($args)*) -> $ret $body
    };
}

/// Mark a function as "cold" to optimize for space, not speed
/// **ALWAYS ENABLED** for better code layout
#[macro_export]
macro_rules! cold_function {
    (fn $name:ident($($args:tt)*) -> $ret:ty $body:block) => {
        #[cold]
        #[inline(never)] // Never inline cold functions
        fn $name($($args)*) -> $ret $body
    };
}
