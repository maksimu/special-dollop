// Main test module that imports and re-exports the other test modules
#[cfg(test)]
mod channel_tests;
#[cfg(test)]
mod common_tests;
#[cfg(test)]
pub mod guacd_handshake_tests;
#[cfg(test)]
mod guacd_parser_tests;
#[cfg(test)]
mod misc_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod size_instruction_integration_tests;
#[cfg(test)]
mod thread_lifecycle_tests;
#[cfg(test)]
mod tube_registry_tests;
#[cfg(test)]
mod tube_tests;
#[cfg(test)]
mod webrtc_basic_tests;
