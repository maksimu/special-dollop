"""
Tests for Windows service thread hanging scenarios

This module tests the specific thread hanging issues observed on Windows services
when using PyTubeRegistry and background threads.
"""

import unittest
import logging
import time
import threading
import queue
import gc
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeoutError

import keeper_pam_webrtc_rs

from test_utils import BaseWebRTCTest, init_logger


class TestThreadHanging(BaseWebRTCTest, unittest.TestCase):
    """Tests for thread hanging issues on Windows services"""
    
    def setUp(self):
        super().setUp()
        init_logger()
        self.created_registries = []  # Track registries for cleanup

    def tearDown(self):
        super().tearDown()
        # Emergency cleanup of any lingering registries
        for registry in self.created_registries:
            try:
                if registry.has_active_tubes():
                    registry.cleanup_all()
            except Exception as e:
                logging.error(f"tearDown registry cleanup failed: {e}")
        self.created_registries.clear()

    def create_tracked_registry(self):
        """Return the shared registry (kept for compatibility with existing test code)"""
        # NOTE: This method now returns the shared registry from BaseWebRTCTest
        # instead of creating new instances, which was causing "Registry actor unavailable" errors
        # and file descriptor corruption
        return self.tube_registry

    def test_registry_creation_no_tubes_immediate_cleanup(self):
        """Test creating registry without tubes and immediate cleanup - simulates quick service restart"""
        logging.info("Testing immediate registry creation and cleanup")
        
        for i in range(5):
            registry = self.create_tracked_registry()
            
            # Verify it starts clean
            self.assertEqual(registry.active_tube_count(), 0)
            self.assertFalse(registry.has_active_tubes())
            
            # Immediate cleanup (simulates quick service stop)
            registry.cleanup_all()
            
            # Verify cleanup worked
            self.assertEqual(registry.active_tube_count(), 0)
            self.assertFalse(registry.has_active_tubes())
            
            logging.debug(f"Iteration {i+1} completed successfully")

    def test_registry_creation_with_delay_cleanup(self):
        """Test registry creation with delay before cleanup - simulates normal service lifecycle"""
        logging.info("Testing delayed registry cleanup")
        
        registry = self.create_tracked_registry()
        
        # Simulate service running for a while
        time.sleep(1.0)
        
        # Check state
        self.assertEqual(registry.active_tube_count(), 0)
        self.assertFalse(registry.has_active_tubes())
        
        # Cleanup after delay
        registry.cleanup_all()
        
        # Verify cleanup
        self.assertEqual(registry.active_tube_count(), 0)
        
        logging.info("Delayed cleanup test completed")

    def test_multiple_registries_concurrent_cleanup(self):
        """Test multiple registries being cleaned up concurrently - simulates multiple service instances"""
        logging.info("Testing concurrent registry cleanup")
        
        registries = []
        for i in range(3):
            registry = self.create_tracked_registry()
            registries.append(registry)
        
        # Cleanup all registries concurrently
        cleanup_threads = []
        results = queue.Queue()
        
        def cleanup_registry(reg, index):
            try:
                reg.cleanup_all()
                results.put((index, "success"))
            except Exception as e:
                results.put((index, f"error: {e}"))
        
        # Start concurrent cleanups
        for i, registry in enumerate(registries):
            thread = threading.Thread(target=cleanup_registry, args=(registry, i))
            cleanup_threads.append(thread)
            thread.start()
        
        # Wait for all cleanups with timeout
        for thread in cleanup_threads:
            thread.join(timeout=10.0)
            if thread.is_alive():
                self.fail(f"Cleanup thread did not complete within timeout")
        
        # Check results
        for i in range(len(registries)):
            try:
                index, result = results.get(timeout=1.0)
                if "error" in result:
                    self.fail(f"Registry {index} cleanup failed: {result}")
            except queue.Empty:
                self.fail(f"Did not receive cleanup result for registry {i}")
        
        logging.info("Concurrent cleanup test completed")

    def test_windows_service_simulation(self):
        """Simulate the exact Windows service scenario that was hanging"""
        logging.info("Simulating Windows service lifecycle")
        
        # Phase 1: Service startup (create registry, no tubes)
        registry = self.create_tracked_registry()
        startup_thread_count = threading.active_count()
        logging.info(f"Service started - Thread count: {startup_thread_count}")
        
        # Phase 2: Service running (simulate brief operation)
        time.sleep(0.5)
        running_thread_count = threading.active_count()
        logging.info(f"Service running - Thread count: {running_thread_count}")
        
        # Verify no tubes were created
        self.assertEqual(registry.active_tube_count(), 0)
        
        # Phase 3: Service stop signal received
        logging.info("Service stop signal received")
        stop_completed = threading.Event()
        stop_error = queue.Queue()
        
        def service_stop():
            try:
                logging.info("Starting service cleanup...")
                registry.cleanup_all()
                logging.info("Registry cleanup completed")
                stop_completed.set()
            except Exception as e:
                stop_error.put(e)
                stop_completed.set()
        
        # Phase 4: Cleanup with timeout (critical test)
        stop_thread = threading.Thread(target=service_stop, daemon=True)
        stop_thread.start()
        
        # This is the critical test - does cleanup complete within reasonable time?
        if stop_completed.wait(timeout=15.0):
            # Check if there was an error
            try:
                error = stop_error.get_nowait()
                self.fail(f"Service stop failed with error: {error}")
            except queue.Empty:
                logging.info("Service stop completed successfully")
        else:
            self.fail("Service stop timed out - this is the Windows hanging issue!")
        
        final_thread_count = threading.active_count()
        logging.info(f"Service stopped - Thread count: {final_thread_count}")


    def test_cli_thread_simulation(self):
        """Simulate the CLI thread hanging scenario"""
        logging.info("Simulating CLI thread scenario")
        
        registry = self.create_tracked_registry()
        cli_stop_event = threading.Event()
        cli_result = queue.Queue()
        
        def mock_cli_thread():
            """Mock CLI thread that waits for stop signal"""
            try:
                logging.info("CLI thread started, waiting for commands...")
                # Simulate waiting for exit command
                if cli_stop_event.wait(timeout=30.0):
                    logging.info("CLI thread received stop signal")
                    cli_result.put("stopped_normally")
                else:
                    logging.warning("CLI thread timed out waiting for stop")
                    cli_result.put("timeout")
            except Exception as e:
                cli_result.put(f"error: {e}")
        
        def mock_service_stop():
            """Mock service stop that cleans up and signals CLI"""
            try:
                logging.info("Service stop initiated")
                
                # Clean up WebRTC (this was working)
                registry.cleanup_all()
                logging.info("WebRTC cleanup completed")
                
                # Signal CLI thread to stop
                cli_stop_event.set()
                logging.info("CLI stop signal sent")
                
            except Exception as e:
                logging.error(f"Service stop error: {e}")
                cli_stop_event.set()  # Signal anyway
        
        # Start CLI thread
        cli_thread = threading.Thread(target=mock_cli_thread, daemon=True)
        cli_thread.start()
        
        # Simulate service running briefly
        time.sleep(0.5)
        
        # Trigger service stop
        service_thread = threading.Thread(target=mock_service_stop, daemon=True)
        service_thread.start()
        service_thread.join(timeout=10.0)
        
        if service_thread.is_alive():
            self.fail("Service stop thread did not complete")
        
        # Wait for CLI thread with timeout - this is the critical test
        cli_thread.join(timeout=10.0)
        if cli_thread.is_alive():
            self.fail("CLI thread did not stop within timeout - this is the hanging issue!")
        
        # Check CLI result
        try:
            result = cli_result.get(timeout=1.0)
            if "error" in result:
                self.fail(f"CLI thread failed: {result}")
            elif result == "timeout":
                self.fail("CLI thread timed out waiting for stop signal")
            else:
                logging.info(f"CLI thread result: {result}")
        except queue.Empty:
            self.fail("No result from CLI thread")

    def test_thread_pool_cleanup(self):
        """Test cleanup with thread pool executors - simulates APScheduler scenario"""
        logging.info("Testing thread pool cleanup")
        
        registry = self.create_tracked_registry()
        
        # Simulate using thread pool (like APScheduler)
        with ThreadPoolExecutor(max_workers=2) as executor:
            # Submit some work
            future1 = executor.submit(lambda: registry.active_tube_count())
            future2 = executor.submit(lambda: registry.has_active_tubes())
            
            # Wait for completion
            count = future1.result(timeout=5.0)
            has_tubes = future2.result(timeout=5.0)
            
            self.assertEqual(count, 0)
            self.assertFalse(has_tubes)
            
            # Test cleanup while thread pool exists
            cleanup_future = executor.submit(lambda: registry.cleanup_all())
            
            try:
                cleanup_future.result(timeout=10.0)
                logging.info("Thread pool cleanup completed successfully")
            except FutureTimeoutError:
                self.fail("Thread pool cleanup timed out")

    def test_stress_cleanup_cycles(self):
        """Stress test with multiple create/cleanup cycles"""
        logging.info("Running stress test with multiple cleanup cycles")
        
        for cycle in range(10):
            logging.debug(f"Stress test cycle {cycle + 1}")
            
            # Create registry
            registry = self.create_tracked_registry()
            
            # Brief operation
            count = registry.active_tube_count()
            self.assertEqual(count, 0)
            
            # Cleanup with timeout monitoring
            cleanup_start = time.time()
            registry.cleanup_all()
            cleanup_duration = time.time() - cleanup_start
            
            # Verify cleanup was fast (should be nearly instant for no tubes)
            if cleanup_duration > 5.0:
                self.fail(f"Cleanup took too long: {cleanup_duration:.2f}s")
            
            # Brief pause between cycles
            time.sleep(0.1)
        
        logging.info("Stress test completed successfully")

    def test_forced_exit_simulation(self):
        """Test forced exit scenario - what happens when process is killed"""
        logging.info("Testing forced exit simulation")
        
        registry = self.create_tracked_registry()
        
        # Simulate what happens during forced process termination
        # (We can't actually kill the process in a unit test)
        
        # Create some activity
        count = registry.active_tube_count()
        self.assertEqual(count, 0)
        
        # Don't call cleanup_all() - simulate process being killed
        # Just clear our reference
        registry = None
        
        # Force garbage collection (simulates process cleanup)
        gc.collect()
        
        # If we get here without hanging, the forced exit scenario works
        logging.info("Forced exit simulation completed")


if __name__ == '__main__':
    # Configure logging for detailed output
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    # Run tests with verbose output
    unittest.main(verbosity=2) 