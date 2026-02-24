//! Logging macros that set target to "keeperdb_proxy" for all log calls.
//!
//! When running inside the Gateway (Python), Rust log targets become Python logger names.
//! Without an explicit target, tracing uses the full module path
//! (e.g., "keeperdb_proxy::server::handlers::mysql"), creating overly verbose logger names.
//! These macros ensure all logs from this crate use a single "keeperdb_proxy" target.

macro_rules! trace {
    ($($arg:tt)*) => { ::tracing::trace!(target: "keeperdb_proxy", $($arg)*) };
}

macro_rules! debug {
    ($($arg:tt)*) => { ::tracing::debug!(target: "keeperdb_proxy", $($arg)*) };
}

macro_rules! info {
    ($($arg:tt)*) => { ::tracing::info!(target: "keeperdb_proxy", $($arg)*) };
}

macro_rules! warn {
    ($($arg:tt)*) => { ::tracing::warn!(target: "keeperdb_proxy", $($arg)*) };
}

macro_rules! error {
    ($($arg:tt)*) => { ::tracing::error!(target: "keeperdb_proxy", $($arg)*) };
}
