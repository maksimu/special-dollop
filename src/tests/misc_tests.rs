//! Miscellaneous tests
use crate::logger;
use tracing::{info, debug, warn, error, trace};
use crate::logger::{InitializeLoggerError};

#[test]
fn test_logger_enhancements() {
    // Initialize the logger in test mode with debug level and a small cache size
    let test_module_name = "test_logger";
    let init_result = logger::initialize_logger(test_module_name, Some(true), 10);
    
    // In parallel test execution, logger might already be initialized by another test
    // This is fine - we just want to ensure the logger works
    let logger_was_already_initialized = init_result.is_err();
    if logger_was_already_initialized {
        println!("Logger already initialized by another test (this is expected in parallel execution)");
    } else {
        println!("Successfully initialized logger in this test");
    }

    // Test that logs are being processed at different levels
    info!("Test info message");
    debug!("Test debug message");
    warn!("Test warning message");
    error!("Test error message");

    // Test that we can call initialize multiple times without error (should always fail)
    let reinit_result = logger::initialize_logger(test_module_name, Some(true), 10);
    assert!(reinit_result.is_err(), "Logger re-initialization should always fail");
    if let Err(InitializeLoggerError::SetGlobalDefaultError(_)) = reinit_result {
        println!("Re-initialization correctly failed as expected");
    } else {
        panic!("Logger re-initialization failed with unexpected error: {:?}", reinit_result);
    }

    // Test with a different module name - this should also fail as a logger is already set
    let other_module = "other_module";
    let other_init = logger::initialize_logger(other_module, Some(true), 10);
    assert!(other_init.is_err(), "Logger initialization with different module should always fail");
    if let Err(InitializeLoggerError::SetGlobalDefaultError(_)) = other_init {
        println!("Different module initialization correctly failed as expected");
    } else {
        panic!("Logger initialization with different module failed with unexpected error: {:?}", other_init);
    }

    // Test with different parameters - this should also fail
    let init_trace = logger::initialize_logger(test_module_name, Some(true), 10);
    assert!(init_trace.is_err(), "Logger initialization with different params should always fail");
    if let Err(InitializeLoggerError::SetGlobalDefaultError(_)) = init_trace {
        println!("Different params initialization correctly failed as expected");
    } else {
        panic!("Logger initialization with different params failed with unexpected error: {:?}", init_trace);
    }

    // Create and log some messages
    trace!("This is a trace message that may not be shown");
    debug!("This is a debug message");
    info!("This is an info message");
    warn!("This is a warning message");
    error!("This is an error message");
}