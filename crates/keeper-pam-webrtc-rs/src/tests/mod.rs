// Main test module that imports and re-exports the other test modules
//
// Test Organization:
// - Rust unit tests (below): Fast, deterministic, CI-friendly
// - Python stress tests: ../tests/manual_stress_tests.py (manual only, not CI)
// - Performance benchmarks: See docs/HOT_PATH_OPTIMIZATION_SUMMARY.md
//

// CRITICAL: Initialize rustls crypto provider for all tests
// This is required for DTLS to work in WebRTC connections
// Without this, tests will panic with "no process-level CryptoProvider available"
#[cfg(test)]
#[ctor::ctor]
fn init_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider for tests");
        println!("Initialized rustls crypto provider for tests");
    });
}

#[cfg(test)]
mod adaptive_pool_tests;
#[cfg(test)]
mod assembler_tests;
#[cfg(test)]
mod channel_tests;
#[cfg(test)]
mod common_tests;
#[cfg(test)]
mod concurrent_close_tests;
#[cfg(test)]
pub mod guacd_handshake_tests;
#[cfg(test)]
mod misc_tests;
#[cfg(test)]
mod nat_keepalive_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod registry_actor_tests;
#[cfg(test)]
mod router_helpers_tests;
#[cfg(test)]
mod size_instruction_integration_tests;
#[cfg(test)]
mod socks_tests;
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
