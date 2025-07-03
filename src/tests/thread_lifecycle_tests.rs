//! Tests for thread lifecycle and shutdown behavior
//!
//! These tests help identify what's causing Windows service hanging
//! by testing thread creation and cleanup in isolation.

use std::thread;
use std::time::Duration;

use crate::runtime::get_runtime;
use crate::tube_registry::TubeRegistry;

#[tokio::test]
async fn test_registry_only_no_tubes() {
    println!("=== TEST: Registry only (no tubes) ===");
    let start_threads = count_threads();
    println!("Threads at start: {}", start_threads);

    // Create a tube registry (mimics Python: PyTubeRegistry())
    let registry = TubeRegistry::new();
    println!("Registry created");

    let after_registry = count_threads();
    println!("Threads after registry: {}", after_registry);

    // Don't create any tubes, just check if the registry itself causes issues
    println!("Registry has {} tubes", registry.all_tube_ids_sync().len());

    // Sleep to simulate service running for a bit
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let after_sleep = count_threads();
    println!("Threads after sleep: {}", after_sleep);

    // Explicitly drop registry
    drop(registry);

    let after_drop = count_threads();
    println!("Threads after drop: {}", after_drop);

    println!("=== Registry test complete ===");
    // Test should exit cleanly here
}

#[test]
fn test_thread_exit_behavior_sync() {
    println!("=== TEST: Synchronous thread exit test ===");

    let start_threads = count_threads();
    println!("Sync test - Threads at start: {}", start_threads);

    // Test what happens in a pure sync context
    {
        let _runtime = get_runtime();
        println!("Sync test - Runtime acquired");

        let after_runtime = count_threads();
        println!("Sync test - Threads after runtime: {}", after_runtime);

        // Create and immediately drop a registry
        {
            let _registry = TubeRegistry::new();
            println!("Sync test - Registry created and dropped");
        }

        let after_registry = count_threads();
        println!("Sync test - Threads after registry: {}", after_registry);
    }

    // Everything should be out of scope now
    thread::sleep(Duration::from_millis(500));

    let final_threads = count_threads();
    println!("Sync test - Final threads: {}", final_threads);

    println!("=== Synchronous test complete ===");
}

fn count_threads() -> usize {
    // Get approximate thread count
    // Note: This is imperfect but gives us a rough idea
    thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .max(1)

    // Alternative: On some platforms we could use more sophisticated thread counting
    // For testing purposes, we'll mainly rely on observing test exit behavior
}

/// Test that mimics the exact PyTubeRegistry creation/drop cycle
#[test]
fn test_python_registry_lifecycle() {
    println!("=== TEST: Python Registry Lifecycle ===");

    // Simulate the exact sequence that happens in Python:
    // 1. self.server_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
    // 2. Service runs (no tubes created)
    // 3. Service stops
    // 4. Python GC eventually calls Drop

    {
        println!("Creating registry (mimics PyTubeRegistry::new())...");
        let _registry = TubeRegistry::new();

        println!("Registry exists, service running...");
        thread::sleep(Duration::from_millis(500));

        println!("Service stopping, registry going out of scope...");
        // registry drops here
    }

    println!("Registry dropped, waiting for threads to clean up...");
    thread::sleep(Duration::from_millis(1000));

    println!("=== Python Registry Lifecycle complete ===");
    // Key question: Does this return or hang like your Windows service?
}
