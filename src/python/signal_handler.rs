use crate::unlikely;
use log::{debug, warn};
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Helper function to set up signal handling for a tube
pub fn setup_signal_handler(
    tube_id_key: String,
    mut signal_receiver: tokio::sync::mpsc::UnboundedReceiver<crate::tube_registry::SignalMessage>,
    runtime_handle: crate::runtime::RuntimeHandle,
    callback_pyobj: Py<PyAny>, // Use the passed callback object
) {
    let task_tube_id = tube_id_key.clone();
    let runtime = runtime_handle.runtime().clone(); // Extract the Arc<Runtime>
    runtime.spawn(async move {
        debug!("Signal handler task started for tube_id: {}", task_tube_id);
        let mut signal_count = 0;
        while let Some(signal) = signal_receiver.recv().await {
            signal_count += 1;
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!("Rust task received signal {}: {:?} for tube {}. Preparing Python callback.", signal_count, signal.kind, task_tube_id);
            }

            Python::attach(|py| {
                let py_dict = PyDict::new(py);
                let mut success = true;
                if let Err(e) = py_dict.set_item("tube_id", &signal.tube_id) {
                    warn!("Failed to set 'tube_id' in signal dict for {}: {:?}", task_tube_id, e);
                    success = false;
                }
                if success {
                    if let Err(e) = py_dict.set_item("kind", &signal.kind) {
                        warn!("Failed to set 'kind' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }
                if success {
                    if let Err(e) = py_dict.set_item("data", &signal.data) {
                        warn!("Failed to set 'data' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }
                if success {
                    if let Err(e) = py_dict.set_item("conversation_id", &signal.conversation_id) {
                        warn!("Failed to set 'conversation_id' in signal dict for {}: {:?}", task_tube_id, e);
                        success = false;
                    }
                }

                if success {
                    let result = callback_pyobj.call1(py, (py_dict,));
                    if let Err(e) = result {
                        // Only log if it's not an expected KeyError during closure
                        if !(e.is_instance_of::<PyKeyError>(py)
                            && (signal.kind == "channel_closed" || signal.kind == "disconnect")) {
                            warn!("Error in Python signal kind:{} callback for tube {}: {:?}: ", signal.kind, task_tube_id, e);
                        }
                    }
                } else {
                    warn!("Skipping Python callback for tube {} due to error setting dict items for signal {:?}", task_tube_id, signal.kind);
                }
            });
            if unlikely!(crate::logger::is_verbose_logging()) {
                debug!("Rust task completed Python callback GIL block for signal {}: {:?} for tube {}", signal_count, signal.kind, task_tube_id);
            }
        }
        // Only log termination if it's not a normal closure
        if signal_count > 0 {
            debug!("Signal handler task for tube {} completed normally after processing {} signals", task_tube_id, signal_count);
        } else {
            warn!("Signal handler task FOR TUBE {} IS TERMINATING (processed {} signals) because MPSC channel receive loop ended.", task_tube_id, signal_count);
        }
    });
}
