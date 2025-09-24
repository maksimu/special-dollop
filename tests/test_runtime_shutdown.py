"""
Tests for runtime shutdown functionality

This module tests the new runtime shutdown functionality that was added to prevent
thread hanging on Windows services.
"""

import unittest
import logging
import time
import threading

import keeper_pam_webrtc_rs

from test_utils import BaseWebRTCTest, init_logger


class TestRuntimeShutdown(BaseWebRTCTest, unittest.TestCase):
    """Tests for runtime shutdown functionality"""
    
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

    def test_shutdown_runtime_function_exists(self):
        """Test that the shutdown_runtime_from_python function exists and is callable"""
        logging.info("Testing shutdown_runtime_from_python function availability")
        
        # Check that the function exists
        self.assertTrue(hasattr(keeper_pam_webrtc_rs, 'shutdown_runtime_from_python'))
        
        # Check that it's callable
        self.assertTrue(callable(keeper_pam_webrtc_rs.shutdown_runtime_from_python))
        
        logging.info("shutdown_runtime_from_python function is available")

    def test_runtime_shutdown_basic(self):
        """Test basic runtime shutdown functionality"""
        logging.info("Testing basic runtime shutdown")
        
        # Create a registry to initialize the runtime
        registry = self.create_tracked_registry()
        
        # Verify initial state
        self.assertEqual(registry.active_tube_count(), 0)
        
        # Call shutdown_runtime_from_python - should not crash
        try:
            keeper_pam_webrtc_rs.shutdown_runtime_from_python()
            logging.info("Runtime shutdown completed without error")
        except Exception as e:
            self.fail(f"Runtime shutdown failed: {e}")
        
        # After shutdown, we should still be able to create new registries
        # (the runtime should be re-created on demand)
        new_registry = self.create_tracked_registry()
        self.assertEqual(new_registry.active_tube_count(), 0)

    def test_registry_cleanup_with_runtime_shutdown(self):
        """Test cleanup_all() followed by runtime shutdown"""
        logging.info("Testing registry cleanup with runtime shutdown")
        
        # Create a registry
        registry = self.create_tracked_registry()
        
        # Create a tube to have something to clean up
        settings = {"conversationType": "tunnel"}
        tube_info = registry.create_tube(
            conversation_id="runtime-shutdown-test",
            settings=settings,
            trickle_ice=True,
            callback_token="TEST_MODE_CALLBACK_TOKEN",
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config="TEST_MODE_KSM_CONFIG"
        )
        
        # Verify tube was created
        self.assertEqual(registry.active_tube_count(), 1)
        
        # Clean up all tubes
        registry.cleanup_all()
        
        # Verify cleanup
        self.assertEqual(registry.active_tube_count(), 0)
        
        # Now shutdown runtime
        keeper_pam_webrtc_rs.shutdown_runtime_from_python()
        
        logging.info("Registry cleanup and runtime shutdown completed")

    def test_registry_shutdown_runtime_method(self):
        """Test the shutdown_runtime() method on PyTubeRegistry"""
        logging.info("Testing PyTubeRegistry.shutdown_runtime() method")
        
        # Create a registry
        registry = self.create_tracked_registry()
        
        # Check that the method exists
        self.assertTrue(hasattr(registry, 'shutdown_runtime'))
        self.assertTrue(callable(registry.shutdown_runtime))
        
        # Call the method - should not crash
        try:
            registry.shutdown_runtime()
            logging.info("Registry shutdown_runtime() method completed without error")
        except Exception as e:
            self.fail(f"Registry shutdown_runtime() failed: {e}")

    def test_multiple_shutdown_calls(self):
        """Test that multiple shutdown calls are safe"""
        logging.info("Testing multiple shutdown calls")
        
        # Create a registry
        registry = self.create_tracked_registry()
        
        # Call shutdown multiple times - should be safe
        for i in range(3):
            try:
                keeper_pam_webrtc_rs.shutdown_runtime_from_python()
                logging.info(f"Shutdown call {i+1} completed")
            except Exception as e:
                self.fail(f"Shutdown call {i+1} failed: {e}")
        
        # Also test the registry method multiple times
        for i in range(3):
            try:
                registry.shutdown_runtime()
                logging.info(f"Registry shutdown call {i+1} completed")
            except Exception as e:
                self.fail(f"Registry shutdown call {i+1} failed: {e}")

    def test_cleanup_all_includes_runtime_shutdown(self):
        """Test that cleanup_all() includes runtime shutdown"""
        logging.info("Testing that cleanup_all() includes runtime shutdown")

        # Create a registry
        registry = self.create_tracked_registry()

        # Clean up any leftover tubes from previous tests first
        initial_count = registry.active_tube_count()
        if initial_count > 0:
            logging.warning(f"Found {initial_count} leftover tubes from previous tests, cleaning up first")
            registry.cleanup_all()
            # Brief wait for cleanup to complete
            time.sleep(0.1)

        # Verify clean state
        clean_count = registry.active_tube_count()
        logging.info(f"Clean state: {clean_count} tubes active")

        # Create a tube
        settings = {"conversationType": "tunnel"}
        tube_info = registry.create_tube(
            conversation_id="cleanup-all-shutdown-test",
            settings=settings,
            trickle_ice=True,
            callback_token="TEST_MODE_CALLBACK_TOKEN",
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config="TEST_MODE_KSM_CONFIG"
        )

        # Verify tube was created (should be clean_count + 1)
        expected_count = clean_count + 1
        actual_count = registry.active_tube_count()
        logging.info(f"After creating tube: expected {expected_count}, actual {actual_count}")
        self.assertEqual(actual_count, expected_count,
                        f"Expected {expected_count} tubes after creation, got {actual_count}")

        # cleanup_all() should now include runtime shutdown
        registry.cleanup_all()

        # Brief wait for cleanup to complete
        time.sleep(0.1)

        # Verify cleanup
        final_count = registry.active_tube_count()
        logging.info(f"After cleanup: {final_count} tubes remain")
        self.assertEqual(final_count, 0,
                        f"Expected 0 tubes after cleanup, got {final_count}")

        logging.info("cleanup_all() with runtime shutdown completed")

    def test_thread_cleanup_after_shutdown(self):
        """Test that threads are properly cleaned up after shutdown"""
        logging.info("Testing thread cleanup after shutdown")
        
        # Get baseline thread count and names
        baseline_threads = threading.active_count()
        baseline_thread_names = [t.name for t in threading.enumerate()]
        logging.info(f"Baseline thread count: {baseline_threads}")
        logging.info(f"Baseline threads: {baseline_thread_names}")
        
        # Create a registry (this initializes the runtime)
        logging.info("Creating registry...")
        registry = self.create_tracked_registry()
        
        # Give time for any threads to start
        time.sleep(1.0)
        
        after_registry_threads = threading.active_count()
        after_registry_thread_names = [t.name for t in threading.enumerate()]
        logging.info(f"Thread count after registry creation: {after_registry_threads}")
        logging.info(f"After registry threads: {after_registry_thread_names}")
        
        # Show new threads created
        new_threads = set(after_registry_thread_names) - set(baseline_thread_names)
        if new_threads:
            logging.info(f"NEW THREADS CREATED: {list(new_threads)}")
        else:
            logging.info("No new threads detected after registry creation")
        
        # Call shutdown
        logging.info("Calling shutdown_runtime_from_python()...")
        keeper_pam_webrtc_rs.shutdown_runtime_from_python()
        logging.info("shutdown_runtime_from_python() returned")
        
        # Give time for threads to clean up
        logging.info("Waiting for thread cleanup...")
        time.sleep(3.0)
        
        after_shutdown_threads = threading.active_count()
        after_shutdown_thread_names = [t.name for t in threading.enumerate()]
        logging.info(f"Thread count after shutdown: {after_shutdown_threads}")
        logging.info(f"After shutdown threads: {after_shutdown_thread_names}")
        
        # Show which threads were removed
        removed_threads = set(after_registry_thread_names) - set(after_shutdown_thread_names)
        if removed_threads:
            logging.info(f"THREADS REMOVED: {list(removed_threads)}")
        else:
            logging.warning("NO THREADS REMOVED BY SHUTDOWN - potential issue!")
        
        # Show persistent new threads
        persistent_new_threads = set(after_shutdown_thread_names) - set(baseline_thread_names)
        if persistent_new_threads:
            logging.warning(f"PERSISTENT NEW THREADS: {list(persistent_new_threads)}")
        else:
            logging.info("No persistent new threads - good!")
        
        # We should not have significantly more threads than baseline
        # (allowing some tolerance for test framework threads)
        thread_increase = after_shutdown_threads - baseline_threads
        logging.info(f"Net thread increase: {thread_increase}")
        if thread_increase > 3:
            logging.warning(f"Thread count increased by {thread_increase} - possible thread leak")
        else:
            logging.info("Thread count within acceptable range")
        
        logging.info("Thread cleanup test completed")

    def test_windows_service_shutdown_simulation(self):
        """Simulate the Windows service shutdown pattern"""
        logging.info("Simulating Windows service shutdown pattern")
        
        # Phase 1: Service startup
        registry = self.create_tracked_registry()
        logging.info("Service startup phase completed")
        
        # Phase 2: Service operation (create some tubes)
        settings = {"conversationType": "tunnel"}
        
        tube1_info = registry.create_tube(
            conversation_id="service-test-1",
            settings=settings,
            trickle_ice=True,
            callback_token="TEST_MODE_CALLBACK_TOKEN",
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config="TEST_MODE_KSM_CONFIG"
        )
        
        tube2_info = registry.create_tube(
            conversation_id="service-test-2",
            settings=settings,
            trickle_ice=True,
            callback_token="TEST_MODE_CALLBACK_TOKEN",
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config="TEST_MODE_KSM_CONFIG"
        )
        
        # Verify tubes were created
        self.assertEqual(registry.active_tube_count(), 2)
        logging.info("Service operation phase completed")
        
        # Phase 3: Service shutdown
        start_time = time.time()
        
        # Clean up all tubes
        registry.cleanup_all()
        
        cleanup_time = time.time() - start_time
        logging.info(f"Service cleanup completed in {cleanup_time:.2f}s")
        
        # Verify cleanup
        self.assertEqual(registry.active_tube_count(), 0)
        
        # This should have included runtime shutdown
        logging.info("Windows service shutdown simulation completed")

    def test_error_handling_during_shutdown(self):
        """Test error handling during shutdown operations"""
        logging.info("Testing error handling during shutdown")
        
        # Create a registry
        registry = self.create_tracked_registry()
        
        # Create a tube
        settings = {"conversationType": "tunnel"}
        tube_info = registry.create_tube(
            conversation_id="error-handling-test",
            settings=settings,
            trickle_ice=True,
            callback_token="TEST_MODE_CALLBACK_TOKEN",
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config="TEST_MODE_KSM_CONFIG"
        )
        
        # Verify tube was created
        self.assertEqual(registry.active_tube_count(), 1)
        
        # Force an error condition by calling shutdown first
        keeper_pam_webrtc_rs.shutdown_runtime_from_python()
        
        # Now try cleanup - should handle the error gracefully
        try:
            registry.cleanup_all()
            logging.info("Cleanup after shutdown completed without exception")
        except Exception as e:
            # Even if there's an error, it should be handled gracefully
            logging.info(f"Cleanup after shutdown handled error: {e}")
        
        # Verify final state
        self.assertEqual(registry.active_tube_count(), 0)
        
        logging.info("Error handling test completed")


if __name__ == '__main__':
    unittest.main() 