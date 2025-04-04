use once_cell::sync::OnceCell;
use std::sync::Arc;
#[cfg(not(feature = "python"))]
use std::sync::Mutex;
use tokio::runtime::Runtime;

// Replace OnceLock with OnceCell for consistency
static SHARED_RUNTIME: OnceCell<Arc<Runtime>> = OnceCell::new();
#[cfg(not(feature = "python"))]
static RUNTIME_INIT_MUTEX: Mutex<()> = Mutex::new(());

// Function to initialize or get the shared runtime with thread priority management
pub fn get_or_create_runtime() -> Result<Arc<Runtime>, String> {
    // Fast path: if runtime is already initialized, return it
    if let Some(runtime) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(runtime));
    }

    #[cfg(not(feature = "python"))]
    let _guard = RUNTIME_INIT_MUTEX.lock().unwrap();

    // Check again in case another thread initialized while we were waiting
    if let Some(runtime) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(runtime));
    }

    // Initialize the runtime with thread priority handling
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    builder.thread_name("webrtc-rt");

    // Set worker threads to use a lower priority on Linux
    #[cfg(target_os = "linux")]
    {
        builder.on_thread_start(|| {
            if let Err(e) = set_current_thread_priority(10) {
                log::warn!("Failed to set thread priority: {}", e);
            }
        });
    }

    let runtime = match builder.build() {
        Ok(rt) => Arc::new(rt),
        Err(e) => return Err(format!("Failed to create Tokio runtime: {}", e)),
    };

    // Store it in the OnceCell
    match SHARED_RUNTIME.set(Arc::clone(&runtime)) {
        Ok(_) => Ok(runtime),
        Err(_) => {
            // This should never happen due to our locking, but handle it anyway
            if let Some(existing) = SHARED_RUNTIME.get() {
                Ok(Arc::clone(existing))
            } else {
                Err("Unexpected concurrent initialization failure".to_string())
            }
        }
    }
}

// For Python bindings, adapt the function to return PyResult with the appropriate error type
#[cfg(feature = "python")]
pub fn get_or_create_runtime_py() -> pyo3::PyResult<Arc<Runtime>> {
    get_or_create_runtime().map_err(pyo3::exceptions::PyRuntimeError::new_err)
}

// Helper function for Linux thread priority
#[cfg(target_os = "linux")]
fn set_current_thread_priority(priority: i32) -> Result<(), String> {
    unsafe {
        let ret = libc::nice(priority);
        if ret == -1 {
            Err(format!(
                "Failed to set nice value: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }
}

mod logger;
mod webrtc_core;

#[cfg(test)]
mod tests;

#[cfg(feature = "python")]
mod python_bindings;

pub use webrtc_core::*;

#[cfg(feature = "python")]
pub use python_bindings::*;

pub use logger::initialize_logger;
