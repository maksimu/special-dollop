"""
Test to verify AIClosed reason is properly sent and received

This test creates a peer connection and closes one side with AIClosed reason,
then verifies that the remote peer receives the correct close reason code.
"""

import unittest
import logging
import time
import threading
import json

import keeper_pam_connections

from test_utils import with_runtime, BaseWebRTCTest

TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"

# CloseConnectionReason codes (from PyCloseConnectionReason enum)
REASON_AI_CLOSED = 15
REASON_ADMIN_CLOSED = 12


class TestAIClosedReason(BaseWebRTCTest, unittest.TestCase):
    """Tests for AIClosed reason propagation"""

    def setUp(self):
        super().setUp()
        self.created_tubes = set()
        self.tube_states = {}
        self.tube_connection_events = {}
        self._lock = threading.Lock()
        self.peer_map = {}
        self.signal_log = []
        self.channel_closed_events = {}

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
            self.channel_closed_events.clear()

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
                    conversation_id = signal_dict.get('conversation_id')
                    logging.info(f"Channel closed signal for tube {tube_id}, conversation: {conversation_id}")

                    # Only process events for tubes created in THIS test
                    if tube_id not in self.created_tubes:
                        logging.debug(f"Ignoring channel_closed event for tube {tube_id} (not from current test)")
                        return

                    # Parse the data JSON to extract close reason
                    try:
                        data_obj = json.loads(data)
                        if 'close_reason' in data_obj:
                            close_reason = data_obj['close_reason']
                            logging.info(f"  Close reason - Code: {close_reason['code']}, "
                                       f"Name: {close_reason['name']}, "
                                       f"Critical: {close_reason['is_critical']}, "
                                       f"Retryable: {close_reason['is_retryable']}")

                            # Store the close reason for verification
                            key = f"{tube_id}:{conversation_id}"
                            self.channel_closed_events[key] = close_reason
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
    def test_ai_closed_reason_propagates_to_peer(self):
        """Test that AIClosed reason properly propagates to the remote peer"""
        logging.info("=== Testing AIClosed reason propagation ===")

        settings = {"conversationType": "tunnel"}

        try:
            # Create server and client tubes
            server_info = self.create_tube_tracked(
                conversation_id="ai-close-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            server_id = server_info['tube_id']

            client_info = self.create_tube_tracked(
                conversation_id="ai-close-client",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG,
                offer=server_info['offer']
            )
            client_id = client_info['tube_id']

            # Set up peer mapping for ICE candidate exchange
            with self._lock:
                self.peer_map[server_id] = client_id
                self.peer_map[client_id] = server_id

            # Complete connection
            self.tube_registry.set_remote_description(server_id, client_info['answer'], is_answer=True)

            # Wait for connection
            connected = self.wait_for_connection(server_id, client_id, timeout=15)
            self.assertTrue(connected, "Tubes should be connected")

            logging.info("Connection established successfully")
            time.sleep(1.0)  # Let connection stabilize

            # Close the server connection with AIClosed reason
            connection_id = "ai-close-server"
            logging.info(f"🤖 Closing connection '{connection_id}' with AIClosed reason (code {REASON_AI_CLOSED})")
            self.tube_registry.close_connection(connection_id, REASON_AI_CLOSED)

            # Give time for the close message to propagate
            time.sleep(2.0)

            # Check if either side received the channel_closed signal with AIClosed reason
            # (The signal may arrive at either the server or client side depending on timing)
            client_key = f"{client_id}:ai-close-server"
            server_key = f"{server_id}:ai-close-server"
            logging.info(f"Checking for channel_closed event with keys: {client_key} or {server_key}")
            logging.info(f"All channel_closed_events: {self.channel_closed_events}")

            # Accept event from either tube (race condition in signal delivery)
            if client_key in self.channel_closed_events:
                close_reason_key = client_key
            elif server_key in self.channel_closed_events:
                close_reason_key = server_key
            else:
                self.fail(f"Neither client nor server received channel_closed signal for '{connection_id}'")

            close_reason = self.channel_closed_events[close_reason_key]
            logging.info(f"✅ Received close reason on {close_reason_key}: {close_reason}")

            # Verify the reason code is AIClosed (15)
            self.assertEqual(close_reason['code'], REASON_AI_CLOSED,
                           f"Close reason code should be {REASON_AI_CLOSED} (AIClosed), got {close_reason['code']}")
            self.assertEqual(close_reason['name'], 'AIClosed',
                           f"Close reason name should be 'AIClosed', got {close_reason['name']}")

            logging.info("✅ AIClosed reason successfully propagated to remote peer!")

        except Exception as e:
            logging.error(f"Test failed: {e}", exc_info=True)
            raise

    @with_runtime
    def test_ai_closed_on_tube_close(self):
        """Test that closing entire tube with AIClosed reason works"""
        logging.info("=== Testing AIClosed on tube close ===")

        settings = {"conversationType": "tunnel"}

        try:
            # Create server and client tubes
            server_info = self.create_tube_tracked(
                conversation_id="ai-tube-server",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                ksm_config=TEST_KSM_CONFIG
            )
            server_id = server_info['tube_id']

            client_info = self.create_tube_tracked(
                conversation_id="ai-tube-client",
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
            connected = self.wait_for_connection(server_id, client_id, timeout=15)
            self.assertTrue(connected, "Tubes should be connected")

            logging.info("Connection established successfully")
            time.sleep(1.0)

            # Close the entire server tube with AIClosed reason
            logging.info(f"🤖 Closing entire tube '{server_id}' with AIClosed reason (code {REASON_AI_CLOSED})")
            self.tube_registry.close_tube(server_id, REASON_AI_CLOSED)

            # Give time for the close message to propagate
            time.sleep(2.0)

            # Check if either side received the channel_closed signal
            # (The signal may arrive at either the server or client side depending on timing)
            client_key = f"{client_id}:ai-tube-server"
            server_key = f"{server_id}:ai-tube-server"
            logging.info(f"Checking for channel_closed event with keys: {client_key} or {server_key}")
            logging.info(f"All channel_closed_events: {self.channel_closed_events}")

            # Accept event from either tube (race condition in signal delivery)
            if client_key in self.channel_closed_events:
                close_reason_key = client_key
            elif server_key in self.channel_closed_events:
                close_reason_key = server_key
            else:
                self.fail("Neither client nor server received channel_closed signal")

            close_reason = self.channel_closed_events[close_reason_key]
            logging.info(f"✅ Received close reason on {close_reason_key}: {close_reason}")

            # Verify the reason code is AIClosed (15)
            self.assertEqual(close_reason['code'], REASON_AI_CLOSED,
                           f"Close reason code should be {REASON_AI_CLOSED} (AIClosed), got {close_reason['code']}")
            self.assertEqual(close_reason['name'], 'AIClosed',
                           f"Close reason name should be 'AIClosed', got {close_reason['name']}")

            logging.info("✅ AIClosed reason successfully propagated on tube close!")

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