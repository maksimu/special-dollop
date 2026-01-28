# Architecture Explanation - Python Bindings

## 🎯 Key Concept: Separation of Concerns

The Python bindings are **NOT** inside `keeper-pam-webrtc-rs` anymore. They live in a separate crate called `python-bindings`.

## 📁 Directory Structure Explained

### `crates/keeper-pam-webrtc-rs/` - Pure Rust Library

**Purpose**: Core business logic, no Python extension

```
crates/keeper-pam-webrtc-rs/
├── Cargo.toml                    # crate-type = ["rlib"] (NOT cdylib)
└── src/
    ├── lib.rs                    # Pure Rust exports
    ├── tube.rs                   # Core Rust code
    ├── webrtc_core.rs           # Core Rust code
    ├── python/                   # ⚠️ NOT Python bindings!
    │   ├── mod.rs               # Registration helper (Rust code)
    │   ├── tube_registry_binding.rs  # PyO3 struct definitions
    │   ├── enums.rs             # PyO3 enum definitions
    │   └── ...                  # Other PyO3 helpers
    └── ...
```

**What `src/python/` contains:**
- ✅ **Rust code** that uses PyO3 to define Python classes
- ✅ Registration function: `register_webrtc_module()`
- ✅ PyO3 struct definitions: `PyTubeRegistry`, `PyCloseConnectionReason`
- ❌ **NOT** the actual Python module (no `__init__.py`)
- ❌ **NOT** compiled as a Python extension

**Key function:**
```rust
// crates/keeper-pam-webrtc-rs/src/python/mod.rs
pub fn register_webrtc_module(_py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register PyTubeRegistry, PyCloseConnectionReason, etc.
    parent.add_class::<PyTubeRegistry>()?;
    parent.add_class::<PyCloseConnectionReason>()?;
    // ...
    Ok(())
}
```

This function is **called by** the unified bindings crate.

---

### `crates/python-bindings/` - Unified Python Package

**Purpose**: The actual Python extension module that aggregates all functionality

```
crates/python-bindings/
├── Cargo.toml                    # crate-type = ["cdylib"] (Python extension)
├── pyproject.toml                # Python package metadata
├── src/
│   └── lib.rs                    # Entry point - aggregates all bindings
├── python/
│   └── keeper_pam_connections/   # ✅ THE actual Python module
│       ├── __init__.py           # Python imports
│       └── connection_manager.py # Pure Python helpers
└── tests/                        # ✅ ALL Python tests
    ├── test_integration.py
    ├── test_performance.py
    └── ...
```

**What `src/lib.rs` does:**
```rust
// crates/python-bindings/src/lib.rs
#[pymodule]
fn keeper_pam_connections(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Call registration functions from all crates
    keeper_pam_webrtc_rs::python::register_webrtc_module(py, m)?;
    
    // Future: Add more crates
    // keeper_pam_ssh_rs::python::register_ssh_module(py, m)?;
    
    Ok(())
}
```

This creates the actual `keeper_pam_connections` Python module.

---

## 🔄 How It Works

### 1. **Build Process**

```bash
cd crates/python-bindings
maturin build --release
```

**What happens:**
1. Maturin compiles `python-bindings` as a cdylib
2. It links against `keeper-pam-webrtc-rs` (rlib)
3. Calls `keeper_pam_webrtc_rs::python::register_webrtc_module()`
4. Creates `keeper_pam_connections.so` (or `.pyd` on Windows)
5. Packages it with `python/keeper_pam_connections/__init__.py`

### 2. **Python Import**

```python
import keeper_pam_connections
```

**What happens:**
1. Python loads `keeper_pam_connections.so` (the compiled Rust extension)
2. Calls the `#[pymodule]` function in `python-bindings/src/lib.rs`
3. That function calls `register_webrtc_module()` from `keeper-pam-webrtc-rs`
4. All classes/functions get registered into the module
5. Python can now use `keeper_pam_connections.PyTubeRegistry()`

---

## 🎨 Analogy

Think of it like a restaurant:

- **`keeper-pam-webrtc-rs`** = The kitchen (core logic)
  - Has recipes (Rust code)
  - Has menu items (PyO3 struct definitions)
  - Has a "register menu" function (registration helper)
  - **Does NOT serve customers directly**

- **`python-bindings`** = The dining room (customer interface)
  - Calls the kitchen's "register menu" function
  - Serves the food to customers (Python users)
  - **This is what customers (Python) interact with**

---

## ✅ Benefits of This Architecture

### 1. **Clean Separation**
- Core logic (Rust) separate from Python interface
- `keeper-pam-webrtc-rs` can be used by other Rust crates without Python
- Python bindings don't pollute the core library

### 2. **Extensibility**
```rust
// Easy to add new crates!
#[pymodule]
fn keeper_pam_connections(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    keeper_pam_webrtc_rs::python::register_webrtc_module(py, m)?;
    keeper_pam_ssh_rs::python::register_ssh_module(py, m)?;      // Add SSH
    keeper_pam_rdp_rs::python::register_rdp_module(py, m)?;      // Add RDP
    keeper_pam_database_rs::python::register_database_module(py, m)?;  // Add DB
    Ok(())
}
```

All accessible through one import: `import keeper_pam_connections`

### 3. **Single Python Package**
- Users install one package: `pip install keeper-pam-connections`
- One import: `import keeper_pam_connections`
- All functionality available immediately
- No need to install multiple packages

### 4. **Testability**
- Rust unit tests in `keeper-pam-webrtc-rs/src/tests/`
- Python integration tests in `python-bindings/tests/`
- Clear separation of concerns

---

## 🚫 Common Misconceptions

### ❌ "The `python/` folder in keeper-pam-webrtc-rs is the Python module"
**Wrong!** That folder contains **Rust code** that uses PyO3. It's not the Python module itself.

### ❌ "keeper-pam-webrtc-rs re-exports the Python bindings"
**Wrong!** It provides a **registration function** that the unified bindings crate calls.

### ❌ "I need to build keeper-pam-webrtc-rs with maturin"
**Wrong!** Only `python-bindings` is built with maturin. `keeper-pam-webrtc-rs` is just a regular Rust library.

---

## ✅ Correct Mental Model

```
┌─────────────────────────────────────────────────────────┐
│ Python User                                              │
│ import keeper_pam_connections                            │
└─────────────────────┬───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│ python-bindings (cdylib)                                 │
│ - Compiled to keeper_pam_connections.so                  │
│ - Entry point: #[pymodule] fn keeper_pam_connections()  │
│ - Calls registration functions from all crates           │
└─────────────────────┬───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│ keeper-pam-webrtc-rs (rlib)                             │
│ - Pure Rust library                                      │
│ - Provides: register_webrtc_module()                     │
│ - Defines: PyTubeRegistry, PyCloseConnectionReason      │
│ - Core business logic                                    │
└─────────────────────────────────────────────────────────┘
```

---

## 📝 Summary

**Q: Should I still have Python bindings inside keeper-pam-webrtc-rs?**

**A: NO!** The structure is:

1. **`keeper-pam-webrtc-rs/src/python/`** = Rust code with PyO3 (registration helpers)
2. **`python-bindings/`** = The actual Python package (cdylib + Python files)

The `python/` folder in `keeper-pam-webrtc-rs` is **NOT** the Python module. It's Rust code that helps register Python classes. The actual Python module lives in `python-bindings/`.

**Think of it as:**
- `keeper-pam-webrtc-rs` = Library that **can be exposed** to Python
- `python-bindings` = The thing that **actually exposes** it to Python

This separation allows for clean architecture and easy extensibility.
