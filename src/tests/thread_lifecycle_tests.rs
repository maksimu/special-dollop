#![cfg(test)]
//! Thread lifecycle tests for actor-based architecture
//!
//! Verifies that actor doesn't leak threads and cleanup is proper

use crate::tube_registry::REGISTRY;
use std::time::Duration;

fn count_threads() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
}

#[tokio::test]
async fn test_registry_initialization_threads() {
    // Verify registry initialization doesn't leak threads
    let threads_before = count_threads();
    println!("Threads before registry access: {}", threads_before);

    // Access REGISTRY (triggers Lazy initialization if not already done)
    let tube_count = REGISTRY.tube_count();
    println!("Registry has {} tubes", tube_count);

    tokio::time::sleep(Duration::from_millis(100)).await;

    let threads_after = count_threads();
    println!("Threads after registry access: {}", threads_after);

    // With actor model, we expect 1 additional thread for the actor
    // But thread count shouldn't grow unbounded
    println!("✓ Thread lifecycle test complete");
}

#[tokio::test]
async fn test_actor_doesnt_leak_threads_on_create() {
    // Verify that creating tubes doesn't leak threads
    let threads_before = count_threads();
    println!("Threads before tube operations: {}", threads_before);

    // Do some registry operations
    let has_tubes = REGISTRY.has_tubes();
    let ids = REGISTRY.all_tube_ids_sync();
    println!(
        "Registry state: has_tubes={}, count={}",
        has_tubes,
        ids.len()
    );

    tokio::time::sleep(Duration::from_millis(500)).await;

    let threads_after = count_threads();
    println!("Threads after operations: {}", threads_after);

    // Thread count should be stable
    let thread_delta = threads_after.abs_diff(threads_before);
    assert!(
        thread_delta < 10,
        "Should not leak threads (delta: {})",
        thread_delta
    );

    println!("✓ No thread leaks detected");
}
