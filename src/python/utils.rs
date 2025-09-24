use crate::runtime::get_runtime;
use log::warn;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyFloat, PyInt, PyList, PyNone, PyString};
use std::collections::HashMap;

/// Helper function to safely execute async code from Python bindings
pub fn safe_python_async_execute<F, R>(py: Python<'_>, future: F) -> R
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    Python::detach(py, || {
        // Check if we're already in a runtime context
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // We're in a runtime context, spawn the task and wait for it
            let (tx, rx) = std::sync::mpsc::channel();
            handle.spawn(async move {
                let result = future.await;
                let _ = tx.send(result);
            });
            // Block on the std channel receiver (safe in runtime context)
            rx.recv().expect("Task failed to complete")
        } else {
            // We're not in a runtime, safe to use block_on
            let runtime = get_runtime();
            runtime.block_on(future)
        }
    })
}

/// Helper function to convert any PyAny to serde_json::Value
pub fn py_any_to_json_value(py_obj: &Bound<PyAny>) -> PyResult<serde_json::Value> {
    if py_obj.is_instance_of::<PyDict>() {
        let dict = py_obj.downcast::<PyDict>()?;
        let mut map = serde_json::Map::new();
        for (key, value) in dict.iter() {
            let key_str = key
                .extract::<String>()
                .map_err(|e| PyRuntimeError::new_err(format!("Dict key is not a string: {e}")))?;
            map.insert(key_str, py_any_to_json_value(&value)?);
        }
        Ok(serde_json::Value::Object(map))
    } else if py_obj.is_instance_of::<PyList>() {
        let list = py_obj.downcast::<PyList>()?;
        let mut vec = Vec::new();
        for item in list.iter() {
            vec.push(py_any_to_json_value(&item)?);
        }
        Ok(serde_json::Value::Array(vec))
    } else if py_obj.is_instance_of::<PyString>() {
        Ok(serde_json::Value::String(py_obj.extract::<String>()?))
    } else if py_obj.is_instance_of::<PyBool>() {
        Ok(serde_json::Value::Bool(py_obj.extract::<bool>()?))
    } else if py_obj.is_instance_of::<PyInt>() {
        // Python int can be large. Try i64, then u64.
        // If it's too large for Rust's 64-bit integers, serde_json will handle it
        // as a Number which can represent larger values or fallback to float if necessary.
        if let Ok(val) = py_obj.extract::<i64>() {
            Ok(serde_json::Value::Number(serde_json::Number::from(val)))
        } else if let Ok(val) = py_obj.extract::<u64>() {
            Ok(serde_json::Value::Number(serde_json::Number::from(val)))
        } else {
            // For very large integers that don't fit i64/u64, PyO3 might allow extraction as f64
            // or you might need a specific BigInt handling if precision is paramount for extremely large numbers
            // not representable by f64. For typical numeric parameters in JSON, f64 is often acceptable.
            let val_f64 = py_obj.extract::<f64>()?;
            serde_json::Number::from_f64(val_f64)
                .map(serde_json::Value::Number)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "Failed to convert large Python int to JSON number: {py_obj:?}"
                    ))
                })
        }
    } else if py_obj.is_instance_of::<PyFloat>() {
        serde_json::Number::from_f64(py_obj.extract::<f64>()?)
            .map(serde_json::Value::Number)
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "Failed to convert float to JSON number: {py_obj:?}"
                ))
            })
    } else if py_obj.is_none() || py_obj.is_instance_of::<PyNone>() {
        Ok(serde_json::Value::Null)
    } else {
        let type_name = py_obj.get_type().name()?;
        warn!("py_any_to_json_value: Unhandled Python type '{}', falling back to string conversion for value: {:?}", type_name, py_obj);
        let str_val = py_obj.str()?.extract::<String>()?;
        Ok(serde_json::Value::String(str_val))
    }
}

/// Convert a Python dictionary (PyObject) to HashMap<String, serde_json::Value>
pub fn pyobj_to_json_hashmap(
    py: Python<'_>,
    dict_obj: &Py<PyAny>,
) -> PyResult<HashMap<String, serde_json::Value>> {
    let bound_settings_obj = dict_obj.bind(py);

    if !bound_settings_obj.is_instance_of::<PyDict>() {
        return Err(PyRuntimeError::new_err(
            "Settings parameter must be a dictionary.",
        ));
    }

    match py_any_to_json_value(bound_settings_obj)? {
        serde_json::Value::Object(map) => {
            // Convert serde_json::Map to HashMap<String, serde_json::Value>
            // This is mostly a type conversion, the structure is already correct.
            Ok(map.into_iter().collect())
        }
        _ => {
            // This case should ideally not be reached if the input is confirmed to be PyDict
            // and py_any_to_json_value handles PyDict correctly.
            Err(PyRuntimeError::new_err(
                "Failed to convert Python dictionary to a Rust HashMap<String, JsonValue>.",
            ))
        }
    }
}
