// Main test module that imports and re-exports the other test modules
mod common;
mod webrtc_basic;
mod protocol;
mod handlers;
mod tube;
mod misc;
mod guacd_parser;
mod channel;

// Initialize the logger before any test runs but allow it to be safely called multiple times
#[ctor::ctor]
fn init() {
    let _ = crate::logger::initialize_logger("test", Some(true), 10);
}