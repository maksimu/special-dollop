// Hot path performance macros - Branch Prediction Optimizations
//
// UNIFIED LOGGING APPROACH:
// - All debug/trace logging gates with: if unlikely!(is_verbose_logging()) { debug!() }
// - All warn/error logging uses standard log macros: warn!(), error!()
// - This file provides ONLY branch prediction hints (likely!, unlikely!)
//
// Performance: is_verbose_logging() is ~1-2ns (single atomic load)
//              vs log::log_enabled!() at ~5-20ns (TLS + level comparison)

// ================================
// BRANCH PREDICTION OPTIMIZATIONS
// ================================

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
