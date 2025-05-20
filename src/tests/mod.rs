// Main test module that imports and re-exports the other test modules
mod common_tests;
mod webrtc_basic_tests;
mod protocol_tests;
mod tube_tests;
mod misc_tests;
mod guacd_parser_tests;
mod channel_tests;
mod tube_registry_tests;

// Initialize the logger before any test runs but allow it to be safely called multiple times
#[ctor::ctor]
fn init() {
    let _ = crate::logger::initialize_logger("test", Some(true), 10);
}