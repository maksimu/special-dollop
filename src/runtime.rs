// runtime.rs - Proper reference counting architecture with Python-callable cleanup
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub(crate) use tokio::runtime::{Builder, Runtime};

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Reference-counted handle to the global runtime
#[derive(Clone)]
pub struct RuntimeHandle {
    runtime: Arc<Runtime>,
    _drop_guard: Arc<DropGuard>,
}

impl RuntimeHandle {
    /// Get the underlying runtime
    #[cfg(feature = "python")]
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// Block on a future using this runtime
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Spawn a task on this runtime
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }
}

/// Drop guard that handles runtime cleanup when the last reference is dropped
struct DropGuard;

impl Drop for DropGuard {
    fn drop(&mut self) {
        let count = RUNTIME_MANAGER
            .reference_count
            .fetch_sub(1, Ordering::SeqCst);

        if count == 1 {
            // This was the last reference
            eprintln!("RUNTIME: Last reference dropped - scheduling runtime cleanup");

            // Clear the global runtime state and schedule cleanup
            let mut state = RUNTIME_MANAGER.state.lock().unwrap_or_else(|poisoned| {
                eprintln!("RUNTIME: Lock was poisoned during drop, recovering...");
                poisoned.into_inner()
            });

            // Schedule cleanup in a separate thread to avoid async context issues
            if let Some(runtime) = state.take() {
                eprintln!("RUNTIME: Scheduling runtime cleanup in separate thread");
                RUNTIME_MANAGER.schedule_cleanup(runtime);
            }
            eprintln!("RUNTIME: Runtime state cleared - cleanup scheduled");
        }
    }
}

/// Global runtime manager with reference counting
struct RuntimeManager {
    state: Mutex<Option<Arc<Runtime>>>,
    reference_count: AtomicUsize,
}

impl RuntimeManager {
    fn new() -> Self {
        Self {
            state: Mutex::new(None),
            reference_count: AtomicUsize::new(0),
        }
    }

    fn get_or_create_handle(&self) -> RuntimeHandle {
        // Handle poisoned locks gracefully - if the lock is poisoned, we can still recover
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("RUNTIME: Lock was poisoned, recovering...");
            poisoned.into_inner()
        });

        let runtime = if let Some(existing) = &*state {
            existing.clone()
        } else {
            eprintln!("RUNTIME: Creating new global runtime");
            let new_runtime = Arc::new(
                Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build global Tokio runtime"),
            );
            *state = Some(new_runtime.clone());
            new_runtime
        };

        RuntimeHandle {
            runtime: runtime.clone(),
            _drop_guard: Arc::new(DropGuard),
        }
    }

    /// Schedule runtime cleanup in a separate thread to avoid async context issues
    fn schedule_cleanup(&self, runtime: Arc<Runtime>) {
        std::thread::spawn(move || {
            eprintln!("RUNTIME: Cleanup thread started - waiting for safe shutdown opportunity");
            // Give a brief moment for any pending async operations to complete
            std::thread::sleep(Duration::from_millis(100));

            if let Ok(runtime) = Arc::try_unwrap(runtime) {
                eprintln!("RUNTIME: Performing clean runtime shutdown");
                runtime.shutdown_timeout(Duration::from_secs(2));
                eprintln!("RUNTIME: Runtime shutdown complete - threads terminated cleanly");
            } else {
                eprintln!("RUNTIME: Runtime has remaining references - will be cleaned up when process exits");
            }
        });
    }

    #[cfg(feature = "python")]
    fn force_shutdown_sync(&self) {
        eprintln!("RUNTIME: Synchronous force shutdown requested");
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("RUNTIME: Lock was poisoned during sync shutdown, recovering...");
            poisoned.into_inner()
        });
        if let Some(runtime) = state.take() {
            // Reset reference count since we're force-shutting down
            self.reference_count.store(0, Ordering::SeqCst);

            self.perform_shutdown(runtime, true);
        } else {
            eprintln!("RUNTIME: Sync force shutdown - no runtime to shutdown");
        }
    }

    #[cfg(feature = "python")]
    fn perform_shutdown(&self, runtime: Arc<Runtime>, wait_for_completion: bool) {
        if wait_for_completion {
            // Synchronous shutdown - MUST complete before returning
            eprintln!("RUNTIME: Performing synchronous shutdown");

            // For synchronous shutdown, we FORCE it to happen now
            // First, try the clean way
            match Arc::try_unwrap(runtime) {
                Ok(runtime) => {
                    eprintln!("RUNTIME: Clean synchronous shutdown - no other references");
                    runtime.shutdown_timeout(Duration::from_secs(5));
                    eprintln!(
                        "RUNTIME: Synchronous shutdown complete - threads terminated cleanly"
                    );
                }
                Err(runtime_arc) => {
                    // If there are still references, we need to be more aggressive
                    let ref_count = Arc::strong_count(&runtime_arc);
                    eprintln!("RUNTIME: Synchronous shutdown - runtime has {} other references, forcing shutdown anyway", ref_count - 1);

                    // Wait a bit for other references to potentially drop
                    std::thread::sleep(Duration::from_millis(200));

                    // Try again
                    match Arc::try_unwrap(runtime_arc) {
                        Ok(runtime) => {
                            eprintln!("RUNTIME: Delayed synchronous shutdown - references dropped");
                            runtime.shutdown_timeout(Duration::from_secs(5));
                            eprintln!("RUNTIME: Delayed synchronous shutdown complete - threads terminated");
                        }
                        Err(runtime_arc) => {
                            let ref_count = Arc::strong_count(&runtime_arc);
                            eprintln!("RUNTIME: FORCE SYNCHRONOUS SHUTDOWN - {ref_count} references still exist, but proceeding anyway");
                            // For Python shutdown, we proceed even with remaining references
                            // The process is likely shutting down anyway
                            drop(runtime_arc);
                            eprintln!("RUNTIME: Force synchronous shutdown complete - runtime dropped with remaining references");
                        }
                    }
                }
            }
        } else {
            // Asynchronous shutdown in background thread (for Drop scenarios)
            std::thread::spawn(move || {
                eprintln!("RUNTIME: Performing background shutdown");
                std::thread::sleep(Duration::from_millis(100));
                if let Ok(runtime) = Arc::try_unwrap(runtime) {
                    runtime.shutdown_timeout(Duration::from_secs(2));
                    eprintln!("RUNTIME: Background shutdown complete - threads terminated");
                } else {
                    eprintln!("RUNTIME: Background shutdown - runtime has other references");
                }
            });
        }
    }
}

static RUNTIME_MANAGER: Lazy<RuntimeManager> = Lazy::new(RuntimeManager::new);

/// Get a reference-counted handle to the global runtime
/// Returns RuntimeHandle instead of Arc<Runtime> for proper lifecycle management
pub fn get_runtime() -> RuntimeHandle {
    RUNTIME_MANAGER.get_or_create_handle()
}

/// Python-callable function to explicitly shutdown the runtime
/// This should be called before Python process exit to ensure clean thread termination
#[cfg(feature = "python")]
#[pyfunction]
pub fn shutdown_runtime_from_python() {
    eprintln!("RUNTIME: Python-initiated shutdown requested");
    RUNTIME_MANAGER.force_shutdown_sync();
}
