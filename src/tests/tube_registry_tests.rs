#![cfg(test)]
//! Old TubeRegistry tests - replaced by registry_actor_tests.rs
//!
//! These tests tested internal TubeRegistry implementation which has been
//! replaced with actor-based RegistryHandle. See registry_actor_tests.rs
//! for tests of the new architecture.

use crate::tube_registry::REGISTRY;

#[test]
fn test_registry_initialized() {
    // Verify global REGISTRY is properly initialized
    let tube_count = REGISTRY.tube_count();
    println!("✓ Global REGISTRY initialized ({} tubes)", tube_count);
}

#[tokio::test]
async fn test_registry_has_tubes_api() {
    // Verify has_tubes() works
    let has_tubes = REGISTRY.has_tubes();
    println!("✓ REGISTRY.has_tubes() = {}", has_tubes);
}

#[tokio::test]
async fn test_registry_all_tube_ids() {
    // Verify all_tube_ids_sync() works
    let ids = REGISTRY.all_tube_ids_sync();
    println!("✓ REGISTRY.all_tube_ids_sync() returned {} IDs", ids.len());
}

// Legacy tests removed - see registry_actor_tests.rs for new architecture tests
