"""
Tests for close_connection and close_tube operations

This module tests the close operations to ensure proper cleanup of connections
and tubes, including error handling for non-existent IDs.
"""

import unittest
import logging
import time
import threading

import keeper_pam_webrtc_rs

from test_utils import with_runtime, BaseWebRTCTest

# CloseConnectionReason codes (from PyCloseConnectionReason enum)
REASON_NORMAL = 0
REASON_ERROR = 1
REASON_TIMEOUT = 2
REASON_SERVER_REFUSE = 4
REASON_CLIENT = 5
REASON_UNKNOWN = 6
REASON_INVALID_INSTRUCTION = 7
REASON_GUACD_REFUSE = 8
REASON_CONNECTION_LOST = 9
REASON_CONNECTION_FAILED = 10
REASON_TUNNEL_CLOSED = 11
REASON_ADMIN_CLOSED = 12
REASON_ERROR_RECORDING = 13
REASON_GUACD_ERROR = 14
REASON_AI_CLOSED = 15
REASON_ADDRESS_RESOLUTION_FAILED = 16
REASON_DECRYPTION_FAILED = 17
REASON_CONFIGURATION_ERROR = 18
REASON_PROTOCOL_ERROR = 19
REASON_UPSTREAM_CLOSED = 20

# Test configuration
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"


class TestCloseOperations(BaseWebRTCTest, unittest.TestCase):
    """Tests for close_connection and close_tube functionality"""
    
    def setUp(self):
        super().setUp()
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.created_tubes = set()
        self.tube_states = {}
        self.tube_connection_events = {}
        self._lock = threading.Lock()
        self.peer_map = {}
        self.signal_log = []

    def tearDown(self):
        super().tearDown()
        try:
            if self.created_tubes:
                logging.info(f"tearDown: Cleaning up {len(self.created_tubes)} tubes")
                self.tube_registry.cleanup_tubes(list(self.created_tubes))
            elif self.tube_registry.has_active_tubes():
                logging.warning("tearDown: Found unexpected tubes, cleaning all")
                self.tube_registry.cleanup_all()
        except Exception as e:
            logging.error(f"tearDown cleanup failed: {e}")
        
        self.created_tubes.clear()
        with self._lock:
            self.tube_states.clear()
            self.tube_connection_events.clear()
            self.peer_map.clear()
            self.signal_log.clear()

    def _signal_handler(self, signal_dict):
        """Signal handler that logs all signals"""
        try:
            with self._lock:
                self.signal_log.append(signal_dict.copy())
                
                tube_id = signal_dict.get('tube_id')
                kind = signal_dict.get('kind')
                data = signal_dict.get('data')

                if not tube_id or not kind:
                    logging.warning(f"Received incomplete signal: {signal_dict}")
                    return

                logging.info(f"Signal received - Tube: {tube_id}, Kind: {kind}, Data: {data}")

                if kind == "connection_state_changed":
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
                        try:
                            self.tube_registry.add_ice_candidate(peer_tube_id, data)
                        except Exception as e:
                            logging.error(f"Failed to add ICE candidate to {peer_tube_id}: {e}")
                            
                elif kind == "channel_closed":
                    logging.info(f"Channel closed signal for tube {tube_id}, conversation: {signal_dict.get('conversation_id')}")
                    # Parse the data JSON to extract close reason
                    try:
                        import json
                        data_obj = json.loads(data)
                        if 'close_reason' in data_obj:
                            close_reason = data_obj['close_reason']
                            logging.info(f"  Close reason - Code: {close_reason['code']}, "
                                       f"Name: {close_reason['name']}, "
                                       f"Critical: {close_reason['is_critical']}, "
                                       f"Retryable: {close_reason['is_retryable']}")
                    except json.JSONDecodeError:
                        pass
                    
        except Exception as e:
            logging.error(f"Signal handler error: {e}", exc_info=True)

    def create_tube_tracked(self, conversation_id, **kwargs):
        """Helper to create tube and track it for cleanup"""
        result = self.tube_registry.create_tube(
            conversation_id=conversation_id,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            signal_callback=self._signal_handler,
            **kwargs
        )
        
        if 'tube_id' in result:
            self.created_tubes.add(result['tube_id'])
            logging.debug(f"Tracking tube {result['tube_id']} for cleanup")
            
        return result

    def wait_for_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for tubes to establish connection"""
        logging.info(f"Waiting for connection between {tube_id1} and {tube_id2}")
        start_time = time.time()
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info(f"Connection established!")
                return True
            time.sleep(0.1)
        logging.warning(f"Connection timeout - State1: {state1}, State2: {state2}")
        return False

    @with_runtime
    def test_close_connection_valid(self):
        """Test closing a valid connection (the main conversation channel)"""
        logging.info("=== Testing close_connection with valid connection ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create server tube
            server_info = self.create_tube_tracked(
                conversation_id="close-conn-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            server_id = server_info['tube_id']
            
            # The main conversation ID is the channel that actually exists
            main_connection_id = "close-conn-server"
            
            # Verify the connection exists
            conv_ids = self.tube_registry.get_conversation_ids_by_tube_id(server_id)
            self.assertIn(main_connection_id, conv_ids, "Main connection should exist")
            
            # Test that we can find the tube from the connection ID
            found_tube_id = self.tube_registry.tube_id_from_connection_id(main_connection_id)
            self.assertEqual(found_tube_id, server_id, "Should find correct tube ID from connection ID")
            
            # Close the connection with Normal reason (this closes the actual channel)
            logging.info(f"Closing connection: {main_connection_id} with Normal reason")
            self.tube_registry.close_connection(main_connection_id, REASON_NORMAL)
            
            # Give some time for async operations and signal propagation
            time.sleep(1.0)

            # NEW BEHAVIOR: close_connection is idempotent - closing again is OK (no exception)
            # This is BETTER than the old behavior (throwing errors)
            logging.info(f"Closing same connection again (idempotent test): {main_connection_id}")
            self.tube_registry.close_connection(main_connection_id)  # Should NOT raise
            logging.info("✅ Idempotent close successful (no exception)")

            # The tube should still exist (it has other channels like 'control')
            self.assertTrue(self.tube_registry.tube_found(server_id), "Tube should still exist after closing one channel")

            logging.info("close_connection with valid connection test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_connection_invalid(self):
        """Test closing a non-existent connection (idempotent behavior)"""
        logging.info("=== Testing close_connection with invalid connection (idempotent) ===")

        try:
            # Try to close a connection that doesn't exist
            fake_connection_id = "non-existent-connection-123"

            logging.info(f"Attempting to close non-existent connection: {fake_connection_id}")

            # Should still raise if NO TUBE exists for that conversation
            with self.assertRaises(Exception) as context:
                self.tube_registry.close_connection(fake_connection_id)

            error_msg = str(context.exception)
            self.assertIn("No tube found for connection ID", error_msg,
                         "Should get appropriate error for non-existent conversation")

            logging.info(f"Got expected error: {error_msg}")
            logging.info("close_connection with invalid connection test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_tube_with_connections(self):
        """Test closing a tube that has active connections"""
        logging.info("=== Testing close_tube with active connections ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="close-tube-test",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Associate multiple connections
            connection_ids = ["conn-1", "conn-2", "conn-3"]
            for conn_id in connection_ids:
                self.tube_registry.associate_conversation(tube_id, conn_id)
            
            # Verify connections exist
            conv_ids = self.tube_registry.get_conversation_ids_by_tube_id(tube_id)
            self.assertEqual(len(conv_ids), len(connection_ids) + 1, 
                           "Should have all connections plus original conversation")
            
            # Close the entire tube with Admin reason
            logging.info(f"Closing tube: {tube_id} with AdminClosed reason")
            self.tube_registry.close_tube(tube_id, REASON_ADMIN_CLOSED)
            
            # Give some time for async operations
            time.sleep(0.5)
            
            # Verify tube is gone
            self.assertFalse(self.tube_registry.tube_found(tube_id), 
                           "Tube should not exist after closing")
            
            # Verify none of the connections can be found
            for conn_id in connection_ids:
                found_tube = self.tube_registry.tube_id_from_connection_id(conn_id)
                self.assertIsNone(found_tube, 
                                f"Connection {conn_id} should not be found after tube closure")
            
            # Check for channel_closed signals
            channel_closed_signals = [s for s in self.signal_log if s.get('kind') == 'channel_closed']
            self.assertGreater(len(channel_closed_signals), 0, 
                             "Should have received channel_closed signals")
            
            logging.info("close_tube with connections test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_tube_invalid(self):
        """Test closing a non-existent tube"""
        logging.info("=== Testing close_tube with invalid tube ID ===")
        
        try:
            # Try to close a tube that doesn't exist
            fake_tube_id = "non-existent-tube-456"
            
            logging.info(f"Attempting to close non-existent tube: {fake_tube_id}")
            
            # This should raise an error
            with self.assertRaises(Exception) as context:
                self.tube_registry.close_tube(fake_tube_id)
            
            error_msg = str(context.exception)
            self.assertIn("Failed to close tube", error_msg, 
                         "Should get appropriate error for non-existent tube")
            
            logging.info(f"Got expected error: {error_msg}")
            logging.info("close_tube with invalid ID test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_connection_after_peer_connection(self):
        """Test closing a connection in a peer-to-peer setup"""
        logging.info("=== Testing close_connection in peer setup ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create server and client tubes
            server_info = self.create_tube_tracked(
                conversation_id="peer-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            server_id = server_info['tube_id']
            
            client_info = self.create_tube_tracked(
                conversation_id="peer-client",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG,
                offer=server_info['offer']
            )
            client_id = client_info['tube_id']
            
            # Set up peer mapping
            with self._lock:
                self.peer_map[server_id] = client_id
                self.peer_map[client_id] = server_id
            
            # Complete connection
            self.tube_registry.set_remote_description(server_id, client_info['answer'], is_answer=True)
            
            # Wait for connection
            connected = self.wait_for_connection(server_id, client_id)
            self.assertTrue(connected, "Tubes should be connected")
            
            # Close the actual server channel (not a made-up connection ID)
            connection_id = "peer-server"  # This is the actual channel name
            
            # Close the connection with Client reason
            logging.info(f"Closing connection: {connection_id} with Client reason")
            self.tube_registry.close_connection(connection_id, REASON_CLIENT)
            
            time.sleep(1.0)
            
            # The server tube should still exist (it has a control channel)
            self.assertTrue(self.tube_registry.tube_found(server_id), "Server tube should exist")
            self.assertTrue(self.tube_registry.tube_found(client_id), "Client tube should exist")

            # NEW BEHAVIOR: Idempotent - closing again is OK
            logging.info(f"Closing {connection_id} again (idempotent)")
            self.tube_registry.close_connection(connection_id)  # Should NOT raise
            logging.info(f"✅ Idempotent close successful for {connection_id}")

            logging.info("close_connection in peer setup test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_connection_race_condition(self):
        """Test closing the same connection multiple times (race condition)"""
        logging.info("=== Testing close_connection race condition ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="race-test",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Use the actual channel name
            connection_id = "race-test"  # This is the actual channel that exists
            
            # Try to close the same connection multiple times
            errors = []
            success_count = 0

            def close_connection_thread():
                try:
                    self.tube_registry.close_connection(connection_id)
                    nonlocal success_count
                    success_count += 1
                    logging.info("Successfully closed connection (idempotent)")
                except Exception as e:
                    errors.append(str(e))
                    logging.info(f"Got error closing connection: {e}")

            # Start multiple threads trying to close the same connection
            threads = []
            for i in range(3):
                t = threading.Thread(target=close_connection_thread)
                threads.append(t)
                t.start()

            # Wait for all threads
            for t in threads:
                t.join()

            # NEW BEHAVIOR: Actor model + idempotent close = ALL threads succeed
            # The actor serializes the requests, and each close is idempotent
            self.assertEqual(success_count, 3, "All threads should succeed (idempotent behavior)")
            self.assertEqual(len(errors), 0, "No errors expected (idempotent)")

            logging.info(f"✅ close_connection race condition test passed - all {success_count} threads succeeded (idempotent)")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_tube_then_connection(self):
        """Test trying to close a connection after its tube is already closed"""
        logging.info("=== Testing close connection after tube closure ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="tube-then-conn",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Use the actual channel name
            connection_id = "tube-then-conn"
            
            # Close the tube first with Normal reason
            logging.info(f"Closing tube: {tube_id} with Normal reason")
            self.tube_registry.close_tube(tube_id, REASON_NORMAL)
            
            time.sleep(0.5)
            
            # Now try to close the connection
            logging.info(f"Attempting to close connection after tube closure: {connection_id}")
            
            with self.assertRaises(Exception) as context:
                self.tube_registry.close_connection(connection_id)
            
            error_msg = str(context.exception)
            self.assertIn("No tube found", error_msg, 
                         "Should get error when closing connection of closed tube")
            
            logging.info("close connection after tube closure test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_associate_conversation_behavior(self):
        """Test and document how associate_conversation actually works"""
        logging.info("=== Testing associate_conversation behavior ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="main-conversation",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Associate additional conversation IDs
            additional_conv_ids = ["conv-1", "conv-2", "conv-3"]
            for conv_id in additional_conv_ids:
                self.tube_registry.associate_conversation(tube_id, conv_id)
            
            # Verify all associations exist
            conv_ids = self.tube_registry.get_conversation_ids_by_tube_id(tube_id)
            logging.info(f"Conversation IDs for tube: {conv_ids}")
            
            # All the additional IDs should be there
            for conv_id in additional_conv_ids:
                self.assertIn(conv_id, conv_ids, f"{conv_id} should be associated")
            
            # The main conversation should also be there
            self.assertIn("main-conversation", conv_ids, "Main conversation should be associated")

            # NEW BEHAVIOR: Only the main conversation has an actual channel
            # Trying to close the additional conversations is idempotent (they were never channels)
            for conv_id in additional_conv_ids:
                logging.info(f"Closing associated conversation {conv_id} (no actual channel - idempotent)")
                self.tube_registry.close_connection(conv_id)  # Should NOT raise (idempotent)
                logging.info(f"✅ Idempotent close for {conv_id} (was never a channel)")

            # Close the main conversation channel with Normal reason
            logging.info("Closing main-conversation (actual channel)")
            self.tube_registry.close_connection("main-conversation", REASON_NORMAL)

            time.sleep(1.0)

            # NEW BEHAVIOR: Idempotent - closing again is OK
            logging.info("Closing main-conversation again (idempotent)")
            self.tube_registry.close_connection("main-conversation")  # Should NOT raise
            logging.info("✅ Idempotent close successful for main-conversation")

            logging.info("associate_conversation behavior test passed")
            logging.info("NOTE: associate_conversation creates mappings; close_connection is idempotent!")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_multiple_channels_on_tube(self):
        """Test creating and closing multiple actual channels on the same tube"""
        logging.info("=== Testing multiple channels on same tube ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # For multiple channels, we need to use an existing tube with create_tube
            # specifying an existing tube_id
            
            # First create initial tube
            initial_info = self.create_tube_tracked(
                conversation_id="initial-channel",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = initial_info['tube_id']
            
            # Create a second channel on the same tube by using the existing tube_id
            second_info = self.tube_registry.create_tube(
                conversation_id="second-channel",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                signal_callback=self._signal_handler,
                tube_id=tube_id  # Use existing tube
            )
            
            # The returned tube_id should be the same
            self.assertEqual(second_info['tube_id'], tube_id, "Should use the same tube")
            
            # Now we should have multiple channels
            conv_ids = self.tube_registry.get_conversation_ids_by_tube_id(tube_id)
            self.assertIn("initial-channel", conv_ids, "Initial channel should exist")
            self.assertIn("second-channel", conv_ids, "Second channel should exist")
            
            # Close the first channel with Normal reason
            logging.info("Closing initial-channel with Normal reason")
            self.tube_registry.close_connection("initial-channel", REASON_NORMAL)
            
            time.sleep(1.0)
            
            # Tube should still exist
            self.assertTrue(self.tube_registry.tube_found(tube_id), "Tube should exist after closing one channel")

            # NEW BEHAVIOR: close_connection is idempotent - closing again is OK
            logging.info("Closing initial-channel again (idempotent)")
            self.tube_registry.close_connection("initial-channel")  # Should NOT raise
            logging.info("✅ Idempotent close successful for initial-channel")

            # But second channel should still work - close with Client reason
            logging.info("Closing second-channel with Client reason")
            self.tube_registry.close_connection("second-channel", REASON_CLIENT)

            time.sleep(0.5)

            # NEW BEHAVIOR: Closing second channel again is also idempotent
            logging.info("Closing second-channel again (idempotent)")
            self.tube_registry.close_connection("second-channel")  # Should NOT raise
            logging.info("✅ Idempotent close successful for second-channel")

            logging.info("Multiple channels test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_with_different_reasons(self):
        """Test closing connections and tubes with different reason codes"""
        logging.info("=== Testing close operations with different reasons ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create multiple tubes to test different reasons
            tubes_and_reasons = [
                ("timeout-tube", "timeout-conn", REASON_TIMEOUT, "Timeout"),
                ("error-tube", "error-conn", REASON_ERROR, "Error"),
                ("guacd-tube", "guacd-conn", REASON_GUACD_ERROR, "GuacdError"),
                ("protocol-tube", "protocol-conn", REASON_PROTOCOL_ERROR, "ProtocolError"),
            ]
            
            created_tube_ids = []
            
            for tube_name, conn_name, reason_code, reason_name in tubes_and_reasons:
                tube_info = self.create_tube_tracked(
                    conversation_id=conn_name,
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    ksm_config=TEST_KSM_CONFIG
                )
                tube_id = tube_info['tube_id']
                created_tube_ids.append(tube_id)
                
                # Close the connection with the specific reason
                logging.info(f"Closing connection {conn_name} with reason {reason_name} ({reason_code})")
                self.tube_registry.close_connection(conn_name, reason_code)

                time.sleep(0.2)

                # NEW BEHAVIOR: Idempotent close - closing again is OK
                logging.info(f"Closing {conn_name} again (idempotent test)")
                self.tube_registry.close_connection(conn_name, REASON_NORMAL)  # Should NOT raise
                logging.info(f"✅ Idempotent close successful for {conn_name}")
            
            # Now close tubes with different reasons
            tube_close_reasons = [
                (created_tube_ids[0], REASON_CONNECTION_LOST, "ConnectionLost"),
                (created_tube_ids[1], REASON_CONNECTION_FAILED, "ConnectionFailed"),
                (created_tube_ids[2], REASON_CONFIGURATION_ERROR, "ConfigurationError"),
                (created_tube_ids[3], REASON_UPSTREAM_CLOSED, "UpstreamClosed"),
            ]
            
            for tube_id, reason_code, reason_name in tube_close_reasons:
                logging.info(f"Closing tube {tube_id} with reason {reason_name} ({reason_code})")
                self.tube_registry.close_tube(tube_id, reason_code)
                
                time.sleep(0.2)
                
                # Verify tube is closed
                self.assertFalse(self.tube_registry.tube_found(tube_id), 
                               f"Tube {tube_id} should not exist after closing")
            
            logging.info("Close with different reasons test passed")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_close_without_reason(self):
        """Test closing connections and tubes without specifying a reason (defaults to Unknown)"""
        logging.info("=== Testing close operations without reason (default behavior) ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="default-reason-conn",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Associate additional connection
            self.tube_registry.associate_conversation(tube_id, "additional-conn")
            
            # Close connection without specifying reason (should default to Unknown)
            logging.info("Closing connection without specifying reason")
            self.tube_registry.close_connection("default-reason-conn")

            time.sleep(0.5)

            # NEW BEHAVIOR: Idempotent - closing again is OK
            logging.info("Closing default-reason-conn again (idempotent)")
            self.tube_registry.close_connection("default-reason-conn")  # Should NOT raise
            logging.info("✅ Idempotent close successful for default-reason-conn")
            
            # Create another tube to test tube close without reason
            tube_info2 = self.create_tube_tracked(
                conversation_id="default-tube-conn",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id2 = tube_info2['tube_id']
            
            # Close tube without specifying reason
            logging.info("Closing tube without specifying reason")
            self.tube_registry.close_tube(tube_id2)
            
            time.sleep(0.5)
            
            # Verify tube is closed
            self.assertFalse(self.tube_registry.tube_found(tube_id2), 
                           "Tube should not exist after closing")
            
            logging.info("Close without reason test passed - defaults work correctly")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime 
    def test_close_with_invalid_reason_code(self):
        """Test closing with an invalid reason code (should default to Unknown)"""
        logging.info("=== Testing close with invalid reason code ===")
        
        settings = {"conversationType": "tunnel"}
        
        try:
            # Create a tube
            tube_info = self.create_tube_tracked(
                conversation_id="invalid-reason-conn",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id = tube_info['tube_id']
            
            # Close connection with invalid reason code (999)
            # This should work but use Unknown reason internally
            logging.info("Closing connection with invalid reason code 999")
            self.tube_registry.close_connection("invalid-reason-conn", 999)

            time.sleep(0.5)

            # NEW BEHAVIOR: Idempotent - closing again is OK
            logging.info("Closing invalid-reason-conn again (idempotent)")
            self.tube_registry.close_connection("invalid-reason-conn")  # Should NOT raise
            logging.info("✅ Idempotent close successful for invalid-reason-conn")
            
            # Create another tube for tube close test
            tube_info2 = self.create_tube_tracked(
                conversation_id="invalid-tube-conn",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            tube_id2 = tube_info2['tube_id']
            
            # Close tube with invalid reason code
            logging.info("Closing tube with invalid reason code 1234")
            self.tube_registry.close_tube(tube_id2, 1234)
            
            time.sleep(0.5)
            
            # Verify tube is closed
            self.assertFalse(self.tube_registry.tube_found(tube_id2), 
                           "Tube should not exist after closing")
            
            logging.info("Close with invalid reason code test passed - defaults to Unknown")
            
        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise


if __name__ == '__main__':
    # Configure logging for tests
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    unittest.main()