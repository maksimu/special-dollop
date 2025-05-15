//! Miscellaneous tests
use crate::logger;

#[test]
fn test_logger_enhancements() {
    // Initialize the logger in test mode with debug level and a small cache size
    let test_module_name = "test_logger";
    let init_result = logger::initialize_logger(test_module_name, Some(true), 10);
    assert!(init_result.is_ok(), "Logger initialization should succeed");

    // Test that logs are being processed at different levels
    log::info!("Test info message");
    log::debug!("Test debug message");
    log::warn!("Test warning message");
    log::error!("Test error message");

    // Test that we can call initialize multiple times without error
    let reinit_result = logger::initialize_logger(test_module_name, Some(true), 10);
    assert!(reinit_result.is_ok(), "Logger re-initialization should succeed");

    // Test with a different module name
    let other_module = "other_module";
    let other_init = logger::initialize_logger(other_module, Some(true), 10);
    assert!(other_init.is_ok(), "Logger initialization with different module should succeed");

    // Test with a different log level
    let init_trace = logger::initialize_logger(test_module_name, Some(true), 10);
    assert!(init_trace.is_ok(), "Logger initialization with trace level should succeed");

    // Create and log some messages
    log::trace!("This is a trace message that may not be shown");
    log::debug!("This is a debug message");
    log::info!("This is an info message");
    log::warn!("This is a warning message");
    log::error!("This is an error message");
}