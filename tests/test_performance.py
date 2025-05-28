import unittest
import logging
import time
import socket
import threading
import os # Added import for os
import base64 # Add base64 import

import keeper_pam_webrtc_rs

from test_utils import with_runtime, BaseWebRTCTest, run_ack_server_in_thread

# Special test mode values that might be recognized by the Rust code
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"

class TestWebRTCPerformance(BaseWebRTCTest, unittest.TestCase):
    """Performance tests for WebRTC data channels"""
    
    def setUp(self):
        super().setUp()
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.tube_states = {}  # Stores current state of each tube_id
        self.tube_connection_events = {} # tube_id -> threading.Event for connected state
        self._lock = threading.Lock() # To protect access to shared tube_states and events
        self.peer_map = {} # To map a tube_id to its peer for ICE candidate relay

    def tearDown(self):
        super().tearDown()
        # Give more time for OS to release ports, especially mDNS
        # Value can be tuned or made configurable via environment variable
        delay = float(os.getenv("PYTEST_INTER_TEST_DELAY", "0.5")) # Default 0.5 seconds
        if delay > 0:
            logging.info(f"Waiting {delay}s for resource cleanup before next test...")
            time.sleep(delay)
        
        # Clear shared Python-side state more explicitly
        # Although individual tests have finally blocks, this ensures cleanup
        # even if a test fails before its finally block or if setUp itself had issues.
        with self._lock:
            # Close any tubes that might still be open due to test errors
            # This requires knowing all created tube_ids.
            # If tests reliably clean up their own tubes, this might be redundant
            # but can act as a fallback.
            # For now, focusing on clearing the maps used by the shared signal handler.
            
            self.tube_states.clear()
            logging.debug("Cleared tube_states in tearDown.")
            
            # Clear events; new ones will be created in setUp or signal_handler
            for event in self.tube_connection_events.values():
                event.clear() # Clear any set events
            self.tube_connection_events.clear()
            logging.debug("Cleared tube_connection_events in tearDown.")

            # peer_map is typically cleared by tests, but ensure it here too.
            self.peer_map.clear()
            logging.debug("Cleared peer_map in tearDown.")
            
        logging.info(f"{self.__class__.__name__} tearDown completed.")

    def _signal_handler(self, signal_dict):
        """Handles signals from the Rust layer."""
        try:
            # This handler can be called from a Rust thread, so protect shared data
            with self._lock:
                tube_id = signal_dict.get('tube_id')
                kind = signal_dict.get('kind')
                data = signal_dict.get('data')
                # conv_id = signal_dict.get('conversation_id') # Available if needed

                if not tube_id or not kind:
                    logging.warning(f"Received incomplete signal: {signal_dict}")
                    return

                # logging.debug(f"Signal handler received: tube_id={tube_id}, kind={kind}, data={data}, conv_id={conv_id}")

                if kind == "connection_state_changed":
                    logging.info(f"Tube {tube_id} connection state changed to: {data}")
                    self.tube_states[tube_id] = data.lower() # Store lowercase state
                    
                    # If this tube_id doesn't have an event yet, create one
                    if tube_id not in self.tube_connection_events:
                        self.tube_connection_events[tube_id] = threading.Event()

                    if data.lower() == "connected":
                        self.tube_connection_events[tube_id].set() # Signal that this tube is connected
                    elif data.lower() in ["failed", "closed", "disconnected"]:
                        # If it was connected and now it's not, clear the event
                        # Or, if tests need to react to failures, set a different event.
                        if tube_id in self.tube_connection_events:
                             self.tube_connection_events[tube_id].clear() # Or handle failure explicitly
                elif kind == "icecandidate":
                    peer_tube_id = self.peer_map.get(tube_id)
                    if peer_tube_id:
                        if data: # Ensure data is not empty, empty candidate means end-of-candidates
                            logging.info(f"PYTHON _signal_handler: Relaying ICE from {tube_id} to {peer_tube_id}. Candidate: {data}")
                            self.tube_registry.add_ice_candidate(peer_tube_id, data)
                        else:
                            logging.info(f"PYTHON _signal_handler: Received end-of-candidates signal from {tube_id} for {peer_tube_id}. Not relaying empty data.")
                    elif tube_id in self.peer_map: 
                         logging.warning(f"PYTHON _signal_handler: Peer for {tube_id} (value: {peer_tube_id}) not fully mapped yet, ICE candidate {data[:50]}... dropped.")
                    else:
                        logging.warning(f"PYTHON _signal_handler: No peer entry found for {tube_id} in peer_map to relay ICE candidate. Data: {data[:50]}...")
                # else: 
                    # Potentially handle other signal kinds like 'icecandidate', 'answer' if needed by tests directly
                    # logging.debug(f"Received other signal for {tube_id}: {kind}")
        except Exception as e:
            logging.error(f"PYTHON _signal_handler CRASHED for signal {signal_dict}: {e}", exc_info=True)
            # Optionally re-raise if PyO3/Rust should see it, but for now, just log it
            # to see if this is where the task is dying.
            # raise # This might be needed if Rust expects to see an error propagate
    
    @with_runtime
    def test_data_channel_load(self):
        """Test basic tube creation and connection performance"""
        logging.info("Starting tube creation and connection test")

        settings = {"conversationType": "tunnel"}
        
        # Create a server tube
        server_tube_info = self.tube_registry.create_tube(
            conversation_id="performance-test-server",
            ksm_config=TEST_KSM_CONFIG,
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            signal_callback=self._signal_handler
        )
        
        # Get the offer from a server
        offer = server_tube_info['offer']
        server_id = server_tube_info['tube_id']
        self.assertIsNotNone(offer, "Server should generate an offer")
        logging.info(f"Server Offer SDP:\n{offer}") # Log the server's offer SDP
        
        # Create a client tube with the offer
        client_tube_info = self.tube_registry.create_tube(
            conversation_id="performance-test-client",
            ksm_config=TEST_KSM_CONFIG,
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            offer=offer,
            signal_callback=self._signal_handler
        )
        
        # Get the answer from a client
        answer = client_tube_info['answer']
        client_id = client_tube_info['tube_id']
        self.assertIsNotNone(answer, "Client should generate an answer")
        logging.info(f"Client Answer SDP:\n{answer}") # Log the client's answer SDP
        
        # Populate the peer map for ICE candidate relaying
        with self._lock:
            self.peer_map[server_id] = client_id
            self.peer_map[client_id] = server_id
        
        # Set the answer on the server
        self.tube_registry.set_remote_description(server_id, answer, is_answer=True)
        
        # Wait for a connection establishment
        start_time = time.time()
        connected = self.wait_for_tube_connection(server_id, client_id, 20) # Increased timeout to 20s
        connection_time = time.time() - start_time
        
        self.assertTrue(connected, "Failed to establish connection")
        logging.info(f"Connection established in {connection_time:.2f} seconds")
        
        channel_name = "performance-test-channel"
        channel_settings = {
            "conversationType": "tunnel",
            "ksm_config": TEST_KSM_CONFIG,
            "callback_token": TEST_CALLBACK_TOKEN
        } 
        
        self.tube_registry.create_channel(
            channel_name, # connection_id for Rust binding
            server_id,    # tube_id for Rust binding
            channel_settings
        )
        
        # Clean up
        self.tube_registry.close_connection(server_id, channel_name)
        self.tube_registry.close_tube(server_id)
        self.tube_registry.close_tube(client_id)
        with self._lock:
            self.peer_map.pop(server_id, None)
            self.peer_map.pop(client_id, None)
        
        logging.info("Tube creation and connection test completed")
    
    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to establish a connection"""
        logging.info(f"Waiting for tube connection: {tube_id1} ({self.tube_registry.get_connection_state(tube_id1)}) and {tube_id2} ({self.tube_registry.get_connection_state(tube_id2)}) (timeout: {timeout}s)")
        start_time = time.time()
        state1 = "unknown" # Initialize states
        state2 = "unknown" # Initialize states
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            logging.debug(f"Poll: {tube_id1} state: {state1}, {tube_id2} state: {state2}")
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info(f"Connection established between {tube_id1} and {tube_id2}")
                return True
            time.sleep(0.1)
        logging.warning(f"Connection establishment timed out for {tube_id1} and {tube_id2}. Final states: {tube_id1}={state1}, {tube_id2}={state2}")
        return False

    @with_runtime
    def test_e2e_echo_flow(self):
        logging.info("Starting E2E echo flow test")
        ack_server = None
        external_client_socket = None
        server_tube_id = None
        client_tube_id = None

        try:
            # 1. Start the AckServer
            ack_server = run_ack_server_in_thread()
            self.assertIsNotNone(ack_server.actual_port, "AckServer did not start or report port")
            logging.info(f"[E2E_Test] AckServer running on 127.0.0.1:{ack_server.actual_port}")

            # 2. Server Tube Setup
            server_conv_id = "e2e-server-conv"
            server_settings = {
                "conversationType": "tunnel", # As per Rust test
                "local_listen_addr": "127.0.0.1:0" # Server tube listens here, dynamic port
            }
            
            # The create_tube in Python seems to be a bit different from Rust's.
            # It might not directly expose on_ice_candidate per-tube object in the same way.
            # We are using the BaseWebRTCTest's on_ice_candidate1/2, which is generic.
            # We need to ensure these are somehow linked or the library handles it.
            # For now, let's assume the library's PyTubeRegistry might have a way to globally set these 
            # or that `create_tube` itself registers some internal callbacks.
            # The existing tests use self.tube_registry.get_connection_state, implying ICE is handled.

            logging.info(f"[E2E_Test] Creating server tube with settings: {server_settings}")
            server_tube_info = self.tube_registry.create_tube(
                conversation_id=server_conv_id,
                ksm_config=TEST_KSM_CONFIG, 
                settings=server_settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                signal_callback=self._signal_handler
            )
            self.assertIsNotNone(server_tube_info, "Server tube creation failed")
            server_offer_sdp = server_tube_info['offer']
            server_tube_id = server_tube_info['tube_id']
            server_actual_listen_addr_str = server_tube_info.get('actual_local_listen_addr') # Use .get for safety
            
            self.assertIsNotNone(server_offer_sdp, "Server should generate an offer")
            self.assertIsNotNone(server_tube_id, "Server tube should have an ID")
            self.assertIsNotNone(server_actual_listen_addr_str, "Server tube should have actual_local_listen_addr")
            logging.info(f"[E2E_Test] Server tube {server_tube_id} created. Offer generated. Listening on {server_actual_listen_addr_str}")

            # 3. Client Tube Setup
            client_conv_id = "e2e-client-conv"
            client_settings = {
                "conversationType": "tunnel",
                "target_host": "127.0.0.1",
                "target_port": str(ack_server.actual_port) # Connect to AckServer
            }
            logging.info(f"[E2E_Test] Creating client tube with offer and settings: {client_settings}")
            client_tube_info = self.tube_registry.create_tube(
                conversation_id=client_conv_id,
                ksm_config=TEST_KSM_CONFIG,
                settings=client_settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                offer=server_offer_sdp,
                signal_callback=self._signal_handler
            )
            self.assertIsNotNone(client_tube_info, "Client tube creation failed")
            client_answer_sdp = client_tube_info['answer']
            client_tube_id = client_tube_info['tube_id']

            self.assertIsNotNone(client_answer_sdp, "Client should generate an answer")
            self.assertIsNotNone(client_tube_id, "Client tube should have an ID")
            logging.info(f"[E2E_Test] Client tube {client_tube_id} created. Answer generated.")

            # Populate the peer map for ICE candidate relaying
            with self._lock:
                self.peer_map[server_tube_id] = client_tube_id
                self.peer_map[client_tube_id] = server_tube_id

            # 4. Signaling: Set remote description
            # The Rust test has a more elaborate ICE exchange via signal channels.
            # Python tests rely on `wait_for_tube_connection` which implies internal ICE handling after SDP exchange.
            logging.info(f"[E2E_Test] Server tube {server_tube_id} setting remote description (client's answer)")
            self.tube_registry.set_remote_description(server_tube_id, client_answer_sdp, is_answer=True)
            
            # The client tube in Rust's `create_tube` (when offer is provided) also calls `set_remote_description` internally for the offer.
            # And then `create_answer`. The Python `create_tube` with an offer likely does this too.
            # We might not need an explicit `set_remote_description` on the client side if `create_tube` handles the initial offer.

            # 5. Wait for connection
            logging.info(f"[E2E_Test] Waiting for WebRTC connection between {server_tube_id} and {client_tube_id}...")
            connected = self.wait_for_tube_connection(server_tube_id, client_tube_id, timeout=20) # Increased timeout for E2E
            self.assertTrue(connected, f"WebRTC connection failed between {server_tube_id} and {client_tube_id}")
            logging.info(f"[E2E_Test] WebRTC connection established.")

            # At this point, data channels should be ready if the library follows WebRTC standards.
            # The Rust test waits for data channels to open. Here, we assume the connection implies data channel readiness for simplicity,
            # unless specific API calls for data channel status are available and necessary.

            # 6. Simulate External Client connecting to Server Tube's local TCP endpoint
            self.assertIsNotNone(server_actual_listen_addr_str, "Server actual_local_listen_addr is None") # Check from .get()
            parts = server_actual_listen_addr_str.split(':')
            self.assertEqual(len(parts), 2, "server_actual_listen_addr_str is not in host:port format")
            server_listen_host = parts[0]
            server_listen_port = int(parts[1])
            
            logging.info(f"[E2E_Test] External client connecting to ServerTube's local TCP endpoint: {server_listen_host}:{server_listen_port}")
            external_client_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            external_client_socket.settimeout(10) # Set timeout for socket operations
            external_client_socket.connect((server_listen_host, server_listen_port))
            logging.info("[E2E_Test] External client connected.")

            # 7. Send a message from External Client
            message_content = "Hello Proxied World via Python!"
            message_bytes = message_content.encode('utf-8')
            external_client_socket.sendall(message_bytes)
            logging.info(f"[E2E_Test] External client sent: '{message_content}'")

            # 8. Receive acked message by External Client
            # Expected flow: External Client -> ServerTube(TCP) -> ServerTube(WebRTC) -> ClientTube(WebRTC) 
            # -> ClientTube(TCP) -> AckServer -> ClientTube(TCP) -> ClientTube(WebRTC) 
            # -> ServerTube(WebRTC) -> ServerTube(TCP) -> External Client
            
            received_buffer = bytearray()
            expected_response = (message_content + " ack").encode('utf-8')
            time_limit = time.time() + 15 # 15-second timeout for receiving response
            
            while time.time() < time_limit:
                try:
                    chunk = external_client_socket.recv(4096)
                    if not chunk:
                        logging.warning("[E2E_Test] External client socket closed by server while receiving.")
                        break
                    received_buffer.extend(chunk)
                    if received_buffer == expected_response:
                        break 
                except socket.timeout:
                    logging.debug("[E2E_Test] Socket recv timeout, retrying...")
                    continue
                except Exception as e:
                    logging.error(f"[E2E_Test] Error receiving from external client socket: {e}")
                    self.fail(f"Error receiving from external client socket: {e}")
            
            received_message = received_buffer.decode('utf-8')
            logging.info(f"[E2E_Test] External client received: '{received_message}'")
            self.assertEqual(received_message, expected_response.decode('utf-8'), 
                             "Final acked message mismatch on external client socket")
            logging.info("[E2E_Test] SUCCESS! External client received expected acked message.")

        except Exception as e:
            logging.error(f"[E2E_Test] Exception: {e}", exc_info=True)
            self.fail(f"E2E echo flow test failed with exception: {e}")
        finally:
            logging.info("[E2E_Test] Cleaning up...")
            if external_client_socket:
                try:
                    external_client_socket.close()
                    logging.info("[E2E_Test] External client socket closed.")
                except Exception as e:
                    logging.error(f"[E2E_Test] Error closing external client socket: {e}")
            
            if self.tube_registry and server_tube_id:
                try:
                    self.tube_registry.close_tube(server_tube_id)
                    logging.info(f"[E2E_Test] Server tube {server_tube_id} closed.")
                except Exception as e:
                    logging.error(f"[E2E_Test] Error closing server tube {server_tube_id}: {e}")
            
            if self.tube_registry and client_tube_id:
                try:
                    self.tube_registry.close_tube(client_tube_id)
                    logging.info(f"[E2E_Test] Client tube {client_tube_id} closed.")
                except Exception as e:
                    logging.error(f"[E2E_Test] Error closing client tube {client_tube_id}: {e}")
            
            with self._lock:
                if server_tube_id: self.peer_map.pop(server_tube_id, None)
                if client_tube_id: self.peer_map.pop(client_tube_id, None)

            if ack_server:
                ack_server.stop()
                logging.info("[E2E_Test] AckServer stopped.")
            logging.info("[E2E_Test] Cleanup finished.")

class TestWebRTCFragmentation(BaseWebRTCTest, unittest.TestCase):
    """Tests for tube connection with different settings"""
    
    def setUp(self):
        super().setUp() 
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.tube_states = {}  # Stores current state of each tube_id
        self.tube_connection_events = {} # tube_id -> threading.Event for connected state
        self._lock = threading.Lock() # To protect access to shared tube_states and events
        self.peer_map = {} # To map a tube_id to its peer for ICE candidate relay

    def tearDown(self):
        super().tearDown()
        delay = float(os.getenv("PYTEST_INTER_TEST_DELAY", "0.5")) 
        if delay > 0:
            logging.info(f"Waiting {delay}s for resource cleanup before next test...")
            time.sleep(delay)
        with self._lock:
            self.tube_states.clear()
            for event in self.tube_connection_events.values():
                event.clear()
            self.tube_connection_events.clear()
            self.peer_map.clear()
        logging.info(f"{self.__class__.__name__} tearDown completed for FragmentationTest.")
    
    @with_runtime
    def test_data_channel_fragmentation(self):
        """Test basic tube connection with non-trickle ICE to evaluate connection reliability"""
        logging.info("Starting tube connection reliability test")

        settings = {"conversationType": "tunnel"}
        
        # Create a server tube with non-trickle ICE
        server_tube_info = self.tube_registry.create_tube(
            conversation_id="fragmentation-test-server",
            ksm_config=TEST_KSM_CONFIG,
            settings=settings,
            trickle_ice=False,  # Use non-trickle ICE
            callback_token=TEST_CALLBACK_TOKEN,
        )
        
        # Get the offer from a server
        offer_b64 = server_tube_info['offer']
        server_id = server_tube_info['tube_id']
        self.assertIsNotNone(offer_b64, "Server should generate an offer")
        
        # Decode the offer before logging and checking for candidates
        try:
            offer_decoded_bytes = base64.b64decode(offer_b64)
            offer_decoded_str = offer_decoded_bytes.decode('utf-8')
        except Exception as e:
            logging.error(f"Failed to decode server offer from base64: {e}\nOffer b64: {offer_b64}")
            self.fail(f"Failed to decode server offer: {e}")
            
        logging.info(f"Server Offer SDP (decoded):\n{offer_decoded_str}")
        self.assertTrue("a=candidate:" in offer_decoded_str, "Server offer SDP (decoded) should contain ICE candidates")
        
        # Create a client tube with the offer
        client_settings = {"conversationType": "tunnel"} # Ensure client also has its own settings if needed
        client_tube_info = self.tube_registry.create_tube(
            conversation_id="fragmentation-test-client",
            ksm_config=TEST_KSM_CONFIG,
            settings=client_settings, # Pass original settings, not modified ones
            trickle_ice=False,  # Use non-trickle ICE
            callback_token=TEST_CALLBACK_TOKEN,
            offer=offer_b64, # Pass the original base64 encoded offer
        )
        
        # Get the answer from a client
        answer_b64 = client_tube_info['answer']
        client_id = client_tube_info['tube_id']
        self.assertIsNotNone(answer_b64, "Client should generate an answer")

        # Decode the answer before logging and checking for candidates
        try:
            answer_decoded_bytes = base64.b64decode(answer_b64)
            answer_decoded_str = answer_decoded_bytes.decode('utf-8')
        except Exception as e:
            logging.error(f"Failed to decode client answer from base64: {e}\nAnswer b64: {answer_b64}")
            self.fail(f"Failed to decode client answer: {e}")

        logging.info(f"Client Answer SDP (decoded):\n{answer_decoded_str}") 
        self.assertTrue("a=candidate:" in answer_decoded_str, "Client answer SDP (decoded) should contain ICE candidates")
        
        # Set the answer on the server
        self.tube_registry.set_remote_description(server_id, answer_b64, is_answer=True) # Pass original base64 encoded answer
        
        # Wait for a connection establishment
        # server_id and client_id are already assigned
        
        # Measure connection establishment time
        start_time = time.time()
        connected = self.wait_for_tube_connection(server_id, client_id, 15)
        connection_time = time.time() - start_time
        
        self.assertTrue(connected, "Failed to establish connection")
        logging.info(f"Non-trickle ICE connection established in {connection_time:.2f} seconds")

        # TODO: send different sized messages through the tube and verify we got them
        
        # Clean up
        self.tube_registry.close_tube(server_id)
        self.tube_registry.close_tube(client_id)
        
        logging.info("Tube connection reliability test completed")
    
    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to establish a connection"""
        logging.info(f"Waiting for tube connection establishment (timeout: {timeout}s)")
        start_time = time.time()
        state1 = "unknown" # Initialize states
        state2 = "unknown" # Initialize states
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            logging.debug(f"Poll: {tube_id1} state: {state1}, {tube_id2} state: {state2}")
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info("Connection established")
                return True
            time.sleep(0.1)
        logging.warning(f"Connection establishment timed out. Final states: {tube_id1}={state1}, {tube_id2}={state2}")
        return False

if __name__ == '__main__':
    unittest.main() 