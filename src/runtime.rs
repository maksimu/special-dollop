// runtime.rs
use once_cell::sync::Lazy;
use std::sync::Arc;
pub(crate) use tokio::runtime::{Builder, Runtime};

/// A single multithread Tokio runtime for the whole process,
/// wrapped in Arc so callers can clone cheaply.
static RUNTIME: Lazy<Arc<Runtime>> = Lazy::new(|| {
    Arc::new(
        Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build global Tokio runtime"),
    )
});

/// Borrow (clone) the global runtime handle.
pub fn get_runtime() -> Arc<Runtime> {
    Arc::clone(&RUNTIME)
}