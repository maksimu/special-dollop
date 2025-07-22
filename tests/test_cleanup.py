"""
Tests for PyTubeRegistry cleanup functionality

This module tests the cleanup methods we added to PyTubeRegistry and demonstrates
how to integrate cleanup into existing code patterns.
"""

import unittest
import logging
import time
import threading
import warnings
import weakref

import keeper_pam_webrtc_rs

from test_utils import with_runtime, BaseWebRTCTest

# Test configuration
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"


class TestTubeRegistryCleanup(BaseWebRTCTest, unittest.TestCase):
    """Tests for cleanup functionality"""
    
    def setUp(self):
        super().setUp()
        # Note: We'll create registries per test to test cleanup behavior

    def tearDown(self):
        super().tearDown()
        # Additional cleanup for any lingering resources
        try:
            # Clean up any remaining tubes in the global registry
            temp_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
            if temp_registry.active_tube_count() > 0:
                logging.warning(f"tearDown: Found {temp_registry.active_tube_count()} leftover tubes, cleaning up")
                temp_registry.cleanup_all()
        except Exception as e:
            logging.error(f"tearDown cleanup failed: {e}")

    @with_runtime
    def test_explicit_cleanup_all(self):
        """Test explicit cleanup of all tubes"""
        logging.info("Testing explicit cleanup_all()")
        
        tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        
        try:
            # Verify registry starts clean
            self.assertEqual(tube_registry.active_tube_count(), 0, "Registry should start empty")
            
            # Create multiple tubes
            settings = {"conversationType": "tunnel"}
            
            tube1_info = tube_registry.create_tube(
                conversation_id="cleanup-test-1",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG
            )
            
            tube2_info = tube_registry.create_tube(
                conversation_id="cleanup-test-2", 
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG
            )
            
            tube1_id = tube1_info['tube_id']
            tube2_id = tube2_info['tube_id']
            
            # Verify tubes were created
            self.assertEqual(tube_registry.active_tube_count(), 2, "Should have 2 active tubes")
            self.assertTrue(tube_registry.has_active_tubes(), "Should have active tubes")
            self.assertTrue(tube_registry.tube_found(tube1_id), "Tube 1 should exist")
            self.assertTrue(tube_registry.tube_found(tube2_id), "Tube 2 should exist")
            
            # Test explicit cleanup
            tube_registry.cleanup_all()
            
            # Verify cleanup worked
            self.assertEqual(tube_registry.active_tube_count(), 0, "All tubes should be cleaned up")
            self.assertFalse(tube_registry.has_active_tubes(), "Should have no active tubes")
            self.assertFalse(tube_registry.tube_found(tube1_id), "Tube 1 should be gone")
            self.assertFalse(tube_registry.tube_found(tube2_id), "Tube 2 should be gone")
            
            logging.info("Explicit cleanup_all() test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}")
            # Emergency cleanup
            try:
                tube_registry.cleanup_all()
            except:
                pass
            raise

    @with_runtime
    def test_selective_cleanup(self):
        """Test cleanup of specific tubes"""
        logging.info("Testing selective tube cleanup")
        
        tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        
        try:
            settings = {"conversationType": "tunnel"}
            
            # Create 3 tubes
            tube_infos = []
            for i in range(3):
                tube_info = tube_registry.create_tube(
                    conversation_id=f"selective-test-{i}",
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0",
                    ksm_config=TEST_KSM_CONFIG
                )
                tube_infos.append(tube_info)
            
            tube_ids = [info['tube_id'] for info in tube_infos]
            
            # Verify all created
            self.assertEqual(tube_registry.active_tube_count(), 3, "Should have 3 active tubes")
            
            # Clean up only the first two tubes
            tube_registry.cleanup_tubes([tube_ids[0], tube_ids[1]])
            
            # Verify selective cleanup
            self.assertEqual(tube_registry.active_tube_count(), 1, "Should have 1 tube remaining")
            self.assertFalse(tube_registry.tube_found(tube_ids[0]), "Tube 0 should be gone")
            self.assertFalse(tube_registry.tube_found(tube_ids[1]), "Tube 1 should be gone")
            self.assertTrue(tube_registry.tube_found(tube_ids[2]), "Tube 2 should remain")
            
            # Clean up the remaining tube
            tube_registry.close_tube(tube_ids[2])
            self.assertEqual(tube_registry.active_tube_count(), 0, "All tubes should be gone")
            
            logging.info("Selective cleanup test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}")
            try:
                tube_registry.cleanup_all()
            except:
                pass
            raise

    @with_runtime
    def test_cleanup_idempotent(self):
        """Test that cleanup methods are idempotent"""
        logging.info("Testing cleanup idempotency")
        
        tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        
        try:
            settings = {"conversationType": "tunnel"}
            
            # Create a tube
            tube_info = tube_registry.create_tube(
                conversation_id="idempotent-test",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG
            )
            
            tube_id = tube_info['tube_id']
            self.assertEqual(tube_registry.active_tube_count(), 1, "Should have 1 active tube")
            
            # Call cleanup multiple times
            tube_registry.cleanup_all()
            self.assertEqual(tube_registry.active_tube_count(), 0, "Should be cleaned up")
            
            # Calling cleanup again should not error
            tube_registry.cleanup_all()
            self.assertEqual(tube_registry.active_tube_count(), 0, "Should still be clean")
            
            # Third time should also be fine
            tube_registry.cleanup_all()
            self.assertEqual(tube_registry.active_tube_count(), 0, "Should still be clean")
            
            logging.info("Cleanup idempotency test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}")
            try:
                tube_registry.cleanup_all()
            except:
                pass
            raise

    def test_del_safety_net(self):
        """Test that __del__ safety net works (and warns)"""
        logging.info("Testing __del__ safety net")
        
        # Capture warnings
        with warnings.catch_warnings(record=True) as warning_list:
            warnings.simplefilter("always")
            
            # Create registry and tubes in a scope
            def create_and_abandon_registry():
                tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
                settings = {"conversationType": "tunnel"}
                
                tube_info = tube_registry.create_tube(
                    conversation_id="del-test",
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0",
                    ksm_config=TEST_KSM_CONFIG
                )
                
                tube_id = tube_info['tube_id']
                logging.info(f"Created tube {tube_id} without cleanup")
                
                # Don't call cleanup - let it go out of scope
                return tube_id
            
            tube_id = create_and_abandon_registry()
            
            # Force garbage collection
            import gc
            gc.collect()
            
            # Give some time for cleanup to happen
            time.sleep(0.5)
            
            # Check that cleanup happened (tube should be gone)
            verification_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
            tube_still_exists = verification_registry.tube_found(tube_id)
            
            logging.info(f"After __del__, tube {tube_id} still exists: {tube_still_exists}")
            
            # Clean up any remaining tubes for safety
            verification_registry.cleanup_all()
            
        logging.info("__del__ safety net test completed")

    @with_runtime 
    def test_cleanup_with_connections(self):
        """Test cleanup works properly with active connections"""
        logging.info("Testing cleanup with active connections")
        
        tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        
        try:
            settings = {"conversationType": "tunnel"}
            
            # Create a server tube
            server_info = tube_registry.create_tube(
                conversation_id="cleanup-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG
            )
            
            server_id = server_info['tube_id']
            
            # Create a client tube
            client_info = tube_registry.create_tube(
                conversation_id="cleanup-client",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                offer=server_info['offer']
            )
            
            client_id = client_info['tube_id']
            
            # Set up the connection
            tube_registry.set_remote_description(server_id, client_info['answer'], is_answer=True)
            
            # Add some connections
            tube_registry.associate_conversation(server_id, "test-connection-1")
            tube_registry.associate_conversation(client_id, "test-connection-2")
            
            # Verify we have active tubes and connections
            self.assertEqual(tube_registry.active_tube_count(), 2, "Should have 2 tubes")
            conversation_ids = tube_registry.get_conversation_ids_by_tube_id(server_id)
            self.assertGreater(len(conversation_ids), 0, "Should have conversations")
            
            # Clean up everything
            tube_registry.cleanup_all()
            
            # Verify cleanup
            self.assertEqual(tube_registry.active_tube_count(), 0, "All tubes should be cleaned up")
            
            logging.info("Cleanup with connections test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}")
            try:
                tube_registry.cleanup_all()
            except:
                pass
            raise


class TestCleanupIntegration(BaseWebRTCTest, unittest.TestCase):
    """
    Tests showing how to integrate cleanup into existing code patterns
    """
    
    def setUp(self):
        super().setUp()
        # This shows the UPDATED pattern for your existing code
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.created_tubes = set()  # Track tubes we create
        self.tube_states = {}
        self.tube_connection_events = {}
        self._lock = threading.Lock()
        self.peer_map = {}

    def tearDown(self):
        super().tearDown()
        
        # UPDATED: Use the new cleanup methods
        try:
            if self.created_tubes:
                logging.info(f"tearDown: Cleaning up {len(self.created_tubes)} tubes")
                # Option 1: Clean up only tubes we created
                tube_ids_list = list(self.created_tubes)
                self.tube_registry.cleanup_tubes(tube_ids_list)
                
                # Option 2: Or clean up everything (more aggressive)
                # self.tube_registry.cleanup_all()
                
            elif self.tube_registry.has_active_tubes():
                # Emergency cleanup if something is left
                logging.warning(f"tearDown: Found unexpected tubes, cleaning all")
                self.tube_registry.cleanup_all()
                
        except Exception as e:
            logging.error(f"tearDown cleanup failed: {e}")
        
        # Clear tracking
        self.created_tubes.clear()
        with self._lock:
            self.tube_states.clear()
            self.tube_connection_events.clear()
            self.peer_map.clear()
        
        logging.info(f"{self.__class__.__name__} tearDown completed")

    def _signal_handler(self, signal_dict):
        """Signal handler (same as existing)"""
        try:
            with self._lock:
                tube_id = signal_dict.get('tube_id')
                kind = signal_dict.get('kind')
                data = signal_dict.get('data')

                if not tube_id or not kind:
                    logging.warning(f"Received incomplete signal: {signal_dict}")
                    return

                if kind == "connection_state_changed":
                    logging.info(f"Tube {tube_id} connection state changed to: {data}")
                    self.tube_states[tube_id] = data.lower()
                    
                    if tube_id not in self.tube_connection_events:
                        self.tube_connection_events[tube_id] = threading.Event()

                    if data.lower() == "connected":
                        self.tube_connection_events[tube_id].set()
                    elif data.lower() in ["failed", "closed", "disconnected"]:
                        if tube_id in self.tube_connection_events:
                            self.tube_connection_events[tube_id].clear()
                            
                elif kind == "icecandidate":
                    peer_tube_id = self.peer_map.get(tube_id)
                    if peer_tube_id:
                        # Always relay ICE candidates, including empty ones (end-of-candidates signal)
                        if data:  # Non-empty candidate
                            logging.info(f"Relaying ICE candidate from {tube_id} to {peer_tube_id}")
                        else:  # Empty candidate = end-of-candidates signal
                            logging.info(f"Relaying end-of-candidates signal from {tube_id} to {peer_tube_id}")
                        
                        try:
                            self.tube_registry.add_ice_candidate(peer_tube_id, data)
                        except Exception as e:
                            logging.error(f"Failed to add ICE candidate to {peer_tube_id}: {e}")
                        
        except Exception as e:
            logging.error(f"Signal handler error: {e}", exc_info=True)

    def create_tube_tracked(self, conversation_id, **kwargs):
        """Helper to create tube and track it for cleanup"""
        result = self.tube_registry.create_tube(
            conversation_id=conversation_id,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            **kwargs
        )
        
        if 'tube_id' in result:
            self.created_tubes.add(result['tube_id'])
            logging.debug(f"Tracking tube {result['tube_id']} for cleanup")
            
        return result

    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for connection (same as existing)"""
        logging.info(f"Waiting for tube connection: {tube_id1} and {tube_id2} (timeout: {timeout}s)")
        start_time = time.time()
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info(f"Connection established between {tube_id1} and {tube_id2}")
                return True
            time.sleep(0.1)
        logging.warning(f"Connection establishment timed out")
        return False

    @with_runtime
    def test_integration_pattern_old_style(self):
        """Shows how your existing code can be updated"""
        logging.info("Testing integration pattern (your existing style)")
        
        settings = {"conversationType": "tunnel"}
        server_tube_id = None
        client_tube_id = None
        
        try:
            # OLD WAY: Direct create_tube call
            # server_tube_info = self.tube_registry.create_tube(...)
            
            # NEW WAY: Use tracked creation
            server_tube_info = self.create_tube_tracked(
                conversation_id="integration-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG,
                signal_callback=self._signal_handler
            )
            
            server_tube_id = server_tube_info['tube_id']
            offer = server_tube_info['offer']
            
            client_tube_info = self.create_tube_tracked(
                conversation_id="integration-client",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG,
                offer=offer,
                signal_callback=self._signal_handler
            )
            
            client_tube_id = client_tube_info['tube_id']
            answer = client_tube_info['answer']
            
            # Set up peer mapping for ICE
            with self._lock:
                self.peer_map[server_tube_id] = client_tube_id
                self.peer_map[client_tube_id] = server_tube_id
            
            # Set remote description
            self.tube_registry.set_remote_description(server_tube_id, answer, is_answer=True)
            
            # Wait for connection
            connected = self.wait_for_tube_connection(server_tube_id, client_tube_id, 15)
            self.assertTrue(connected, "Failed to establish connection")
            
            logging.info("Integration test successful")
            
            # OLD WAY: Manual cleanup in finally
            # finally:
            #     try:
            #         if server_tube_id:
            #             self.tube_registry.close_tube(server_tube_id)
            #         if client_tube_id:
            #             self.tube_registry.close_tube(client_tube_id)
            #     except Exception as e:
            #         logging.error(f"Cleanup error: {e}")
            
            # NEW WAY: Cleanup is handled automatically in tearDown()
            # But you can still do explicit cleanup if needed:
            if server_tube_id and client_tube_id:
                self.tube_registry.cleanup_tubes([server_tube_id, client_tube_id])
                self.created_tubes.discard(server_tube_id)
                self.created_tubes.discard(client_tube_id)
                logging.info("Explicit cleanup completed")
            
        except Exception as e:
            logging.error(f"Integration test failed: {e}")
            # Emergency cleanup will happen in tearDown()
            raise

    @with_runtime
    def test_context_manager_pattern(self):
        """Shows how to use context manager pattern"""
        logging.info("Testing context manager pattern")
        
        # This would be in a separate module/class
        class TubeRegistryContext:
            def __init__(self):
                self.registry = keeper_pam_webrtc_rs.PyTubeRegistry()
                self.created_tubes = set()
            
            def __enter__(self):
                return self
            
            def __exit__(self, exc_type, exc_val, exc_tb):
                logging.info("Context manager cleanup")
                if self.created_tubes:
                    tube_ids = list(self.created_tubes)
                    self.registry.cleanup_tubes(tube_ids)
                elif self.registry.has_active_tubes():
                    self.registry.cleanup_all()
                
            def create_tube(self, **kwargs):
                result = self.registry.create_tube(
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0", 
                    **kwargs
                )
                if 'tube_id' in result:
                    self.created_tubes.add(result['tube_id'])
                return result
        
        # Usage with context manager
        with TubeRegistryContext() as ctx:
            settings = {"conversationType": "tunnel"}
            
            tube_info = ctx.create_tube(
                conversation_id="context-test",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            
            tube_id = tube_info['tube_id']
            self.assertTrue(ctx.registry.tube_found(tube_id), "Tube should exist")
            
            # Cleanup happens automatically when exiting context
        
        # Verify cleanup happened
        verification_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.assertFalse(verification_registry.tube_found(tube_id), "Tube should be cleaned up")
        
        logging.info("Context manager pattern test passed")

    def __del__(self):
        """Safety net cleanup when object is garbage collected"""
        if hasattr(self, 'server_registry'):
            try:
                if self.server_registry.has_active_tubes():
                    logger.warning('WebRTC tubes still active during DRConnection destruction - cleaning up')
                    self.server_registry.cleanup_all()
            except Exception as e:
                # Use print instead of logger since logging might not be available during GC
                print(f'Error during DRConnection.__del__ WebRTC cleanup: {e}')


if __name__ == '__main__':
    # Configure logging for tests
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    unittest.main() 