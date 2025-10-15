// Main test module that imports and re-exports the other test modules
//
// Test Organization:
// - Rust unit tests (below): Fast, deterministic, CI-friendly
// - Python stress tests: ../tests/manual_stress_tests.py (manual only, not CI)
// - Performance benchmarks: See docs/HOT_PATH_OPTIMIZATION_SUMMARY.md
//
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
mod nat_keepalive_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod registry_actor_tests;
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
#[cfg(test)]
mod webrtc_core_tests;
