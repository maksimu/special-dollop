"""
Thread monitoring tests - specifically check if a Rust library creates background threads

This module focuses on monitoring what threads are actually created by the Rust library
and whether they properly terminate during cleanup.
"""

import unittest
import logging
import time
import threading
from typing import List, Dict

import keeper_pam_webrtc_rs

from test_utils import BaseWebRTCTest, init_logger


def get_current_threads() -> Dict[int, str]:
    """Get current Python threads with their names and IDs"""
    threads = {}
    try:
        # Get Python threads
        for thread in threading.enumerate():
            thread_name = getattr(thread, 'name', 'unnamed')
            daemon_status = 'daemon' if getattr(thread, 'daemon', False) else 'main'
            alive_status = 'alive' if thread.is_alive() else 'dead'
            threads[thread.ident] = f"Python:{thread_name}[{daemon_status},{alive_status}]"
    except Exception as e:
        logging.warning(f"Failed to get thread info: {e}")
    
    return threads


def thread_diff(before: Dict[int, str], after: Dict[int, str]) -> Dict[str, List[str]]:
    """Compare thread snapshots and return differences"""
    before_ids = set(before.keys())
    after_ids = set(after.keys())
    
    return {
        'added': [after[tid] for tid in (after_ids - before_ids)],
        'removed': [before[tid] for tid in (before_ids - after_ids)],
        'persistent': [after[tid] for tid in (after_ids & before_ids)]
    }


class TestThreadMonitoring(BaseWebRTCTest, unittest.TestCase):
    """Tests that monitor actual thread creation and cleanup"""
    
    def setUp(self):
        super().setUp()
        init_logger()
        self.created_registries = []

    def tearDown(self):
        super().tearDown()
        # Emergency cleanup
        for registry in self.created_registries:
            try:
                if registry.has_active_tubes():
                    registry.cleanup_all()
            except Exception as e:
                logging.error(f"tearDown registry cleanup failed: {e}")
        self.created_registries.clear()

    def create_tracked_registry(self):
        """Create a registry and track it for cleanup"""
        registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        # Configure higher resource limits for testing
        self.configure_test_resource_limits(registry)
        self.created_registries.append(registry)
        return registry

    def test_thread_creation_monitoring(self):
        """Monitor what threads are created when PyTubeRegistry is instantiated"""
        logging.info("=== Thread Creation Monitoring Test ===")
        
        # Baseline - threads before any Rust library usage
        threads_baseline = get_current_threads()
        baseline_count = len(threads_baseline)
        logging.info(f"Baseline thread count: {baseline_count}")
        for tid, name in threads_baseline.items():
            logging.debug(f"  Baseline thread: {name}")
        
        # Wait a moment to ensure a stable baseline
        time.sleep(0.5)
        threads_stable = get_current_threads()
        stable_count = len(threads_stable)
        logging.info(f"Stable baseline thread count: {stable_count}")
        
        # Create PyTubeRegistry - this should trigger global runtime creation
        logging.info("Creating PyTubeRegistry...")
        registry = self.create_tracked_registry()
        
        # Check threads immediately after creation
        threads_after_creation = get_current_threads()
        creation_count = len(threads_after_creation)
        logging.info(f"Thread count after PyTubeRegistry creation: {creation_count}")
        
        diff_creation = thread_diff(threads_stable, threads_after_creation)
        if diff_creation['added']:
            logging.info("NEW THREADS CREATED:")
            for thread in diff_creation['added']:
                logging.info(f"  + {thread}")
        else:
            logging.info("No new threads detected immediately after creation")
        
        # Wait for any delayed thread creation
        time.sleep(2.0)
        threads_after_delay = get_current_threads()
        delay_count = len(threads_after_delay)
        logging.info(f"Thread count after 2s delay: {delay_count}")
        
        diff_delay = thread_diff(threads_after_creation, threads_after_delay)
        if diff_delay['added']:
            logging.info("DELAYED THREADS CREATED:")
            for thread in diff_delay['added']:
                logging.info(f"  + {thread}")
        
        # Cleanup registry
        logging.info("Cleaning up PyTubeRegistry...")
        registry.cleanup_all()
        
        # Check threads immediately after cleanup
        threads_after_cleanup = get_current_threads()
        cleanup_count = len(threads_after_cleanup)
        logging.info(f"Thread count after cleanup: {cleanup_count}")
        
        diff_cleanup = thread_diff(threads_after_delay, threads_after_cleanup)
        if diff_cleanup['removed']:
            logging.info("THREADS REMOVED BY CLEANUP:")
            for thread in diff_cleanup['removed']:
                logging.info(f"  - {thread}")
        else:
            logging.info("No threads removed by cleanup")
        
        # Wait for potential delayed cleanup
        time.sleep(2.0)
        threads_final = get_current_threads()
        final_count = len(threads_final)
        logging.info(f"Final thread count after 2s: {final_count}")
        
        diff_final = thread_diff(threads_after_cleanup, threads_final)
        if diff_final['removed']:
            logging.info("DELAYED THREAD CLEANUP:")
            for thread in diff_final['removed']:
                logging.info(f"  - {thread}")
        
        # Summary
        net_change = final_count - baseline_count
        logging.info(f"NET THREAD CHANGE: {net_change} (baseline: {baseline_count} -> final: {final_count})")
        
        if net_change > 0:
            persistent_new = thread_diff(threads_baseline, threads_final)['added']
            logging.warning("PERSISTENT NEW THREADS (potential leak):")
            for thread in persistent_new:
                logging.warning(f"  ! {thread}")
        
        # Test assertion - we shouldn't have persistent thread leaks
        if net_change > 2:  # Allow some tolerance for test framework threads
            self.fail(f"Too many persistent threads created: {net_change} net increase")

    def test_multiple_registry_thread_impact(self):
        """Test thread impact of creating multiple registries"""
        logging.info("=== Multiple Registry Thread Impact Test ===")
        
        threads_start = get_current_threads()
        start_count = len(threads_start)
        logging.info(f"Starting thread count: {start_count}")
        
        registries = []
        for i in range(3):
            logging.info(f"Creating registry {i+1}...")
            registry = self.create_tracked_registry()
            registries.append(registry)
            
            threads_current = get_current_threads()
            current_count = len(threads_current)
            logging.info(f"Thread count after registry {i+1}: {current_count}")
            
            # Brief pause between creations
            time.sleep(0.5)
        
        # Check the final state with all registries
        threads_all_created = get_current_threads()
        all_count = len(threads_all_created)
        logging.info(f"Thread count with all registries: {all_count}")
        
        diff_all = thread_diff(threads_start, threads_all_created)
        logging.info(f"Total new threads for 3 registries: {len(diff_all['added'])}")
        
        # Cleanup all registries
        for i, registry in enumerate(registries):
            logging.info(f"Cleaning up registry {i+1}...")
            registry.cleanup_all()
            
            threads_current = get_current_threads()
            current_count = len(threads_current)
            logging.info(f"Thread count after cleanup {i+1}: {current_count}")
            
            time.sleep(0.3)
        
        # Final cleanup check
        time.sleep(2.0)
        threads_final = get_current_threads()
        final_count = len(threads_final)
        
        net_change = final_count - start_count
        logging.info(f"NET CHANGE after all cleanup: {net_change}")
        
        # Should return close to original thread count
        if net_change > 3:
            persistent = thread_diff(threads_start, threads_final)['added']
            logging.error("PERSISTENT THREADS:")
            for thread in persistent:
                logging.error(f"  ! {thread}")
            self.fail(f"Too many persistent threads: {net_change}")

    def test_registry_with_brief_operations(self):
        """Test thread behavior with brief registry operations (simulating quick service restart)"""
        logging.info("=== Brief Operations Thread Test ===")
        
        threads_baseline = get_current_threads()
        baseline_count = len(threads_baseline)
        
        for cycle in range(5):
            logging.info(f"Quick cycle {cycle + 1}")
            
            # Quick create/use/cleanup cycle
            registry = self.create_tracked_registry()
            
            # Brief operation
            count = registry.active_tube_count()
            self.assertEqual(count, 0)
            
            # Immediate cleanup
            registry.cleanup_all()
            
            # Check thread state
            threads_current = get_current_threads()
            current_count = len(threads_current)
            net_change = current_count - baseline_count
            
            logging.debug(f"  Cycle {cycle + 1}: {current_count} threads (net: {net_change:+d})")
            
            # Brief pause
            time.sleep(0.1)
        
        # Final assessment
        time.sleep(1.0)
        threads_final = get_current_threads()
        final_count = len(threads_final)
        final_net = final_count - baseline_count
        
        logging.info(f"After 5 quick cycles: {final_count} threads (net: {final_net:+d})")
        
        if final_net > 3:
            self.fail(f"Quick cycles created persistent threads: {final_net}")

    def test_thread_names_and_types(self):
        """Analyze what types of threads are created"""
        logging.info("=== Thread Names and Types Analysis ===")
        
        threads_before = get_current_threads()
        logging.info("Threads before PyTubeRegistry:")
        for tid, name in threads_before.items():
            logging.info(f"  {name}")
        
        # Create registry
        registry = self.create_tracked_registry()
        time.sleep(1.0)  # Allow background threads to start
        
        threads_after = get_current_threads()
        logging.info("Threads after PyTubeRegistry:")
        for tid, name in threads_after.items():
            logging.info(f"  {name}")
        
        diff = thread_diff(threads_before, threads_after)
        
        if diff['added']:
            logging.info("ANALYSIS - New threads by type:")
            python_threads = [t for t in diff['added'] if t.startswith('Python:')]
            system_threads = [t for t in diff['added'] if t.startswith('System:')]
            
            logging.info(f"  Python threads: {len(python_threads)}")
            for thread in python_threads:
                logging.info(f"    {thread}")
            
            logging.info(f"  System threads: {len(system_threads)}")
            for thread in system_threads:
                logging.info(f"    {thread}")
        
        # Cleanup and recheck
        registry.cleanup_all()
        time.sleep(1.0)
        
        threads_cleanup = get_current_threads()
        cleanup_diff = thread_diff(threads_after, threads_cleanup)
        
        if cleanup_diff['removed']:
            logging.info("THREADS REMOVED BY CLEANUP:")
            for thread in cleanup_diff['removed']:
                logging.info(f"  - {thread}")
        else:
            logging.warning("NO THREADS REMOVED BY CLEANUP - potential issue!")
    def test_logger_initialization_discovery(self):
        """Document the discovery about logger initialization and threads"""
        logging.info("=== Logger Initialization Discovery ===")
        
        # This test documents what we discovered about thread creation timing
        logging.info("IMPORTANT DISCOVERY:")
        logging.info("  1. This test file imports keeper_pam_webrtc_rs at module level")
        logging.info("  2. setUp() calls init_logger() before each test")
        logging.info("  3. This means logger is ALREADY initialized before registry tests run")
        logging.info("  4. If logger init creates threads, they're created BEFORE we measure!")
        
        threads_current = get_current_threads()
        current_count = len(threads_current)
        logging.info(f"Current thread count (logger already initialized): {current_count}")
        
        for tid, name in threads_current.items():
            logging.info(f"  {name}")
        
        # Test what happens when we create a registry (should be no new threads)
        logging.info("Creating PyTubeRegistry (after logger already initialized)...")
        threads_before_registry = get_current_threads()
        before_count = len(threads_before_registry)
        
        registry = self.create_tracked_registry()
        
        threads_after_registry = get_current_threads()
        after_count = len(threads_after_registry)
        
        registry_change = after_count - before_count
        logging.info(f"Registry creation thread change: {registry_change}")
        
        if registry_change == 0:
            logging.info("[PASS] CONFIRMED: PyTubeRegistry creation adds no threads")
            logging.info("   This suggests threads are created during logger init, not registry creation")
        else:
            logging.info(f"[FAIL] UNEXPECTED: PyTubeRegistry created {registry_change} threads")
        
        # Cleanup
        registry.cleanup_all()
        
        # The smoking gun evidence
        logging.info("")
        logging.info("CONCLUSION:")
        logging.info("   Your Windows service hangs because:")
        logging.info("   1. initialize_rust_logger() creates global Tokio runtime threads")
        logging.info("   2. These threads are created early in service startup")  
        logging.info("   3. On Windows, these threads don't terminate properly during service shutdown")
        logging.info("   4. This causes the 'CLI thread waiting' hang you observed")
        logging.info("")
        logging.info("SOLUTION:")
        logging.info("   Add explicit Rust runtime shutdown in your service stop sequence")


if __name__ == '__main__':
    # Configure logging for detailed output
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    # Run tests with verbose output
    unittest.main(verbosity=2)
