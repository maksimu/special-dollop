import unittest
import logging
import time
import socket
import threading
import os # Added import for os
import base64 # Add base64 import

import keeper_pam_webrtc_rs

from test_utils import with_runtime, BaseWebRTCTest, run_ack_server_in_thread, ExactEchoServer

# Special test mode values that might be recognized by the Rust code
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"

class TestWebRTCPerformance(BaseWebRTCTest, unittest.TestCase):
    """Performance tests for WebRTC data channels"""

    def setUp(self):
        super().setUp()  # ← This now sets self.tube_registry to shared instance
        # Removed: self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        # Removed: self.configure_test_resource_limits(self.tube_registry)
        # Both handled by BaseWebRTCTest.setUp()

        self.tube_states = {}  # Stores current state of each tube_id
        self.tube_connection_events = {} # tube_id -> threading.Event for connected state
        self._lock = threading.Lock() # To protect access to shared tube_states and events
        self.peer_map = {} # To map a tube_id to its peer for ICE candidate relay

    def tearDown(self):
        # ✅ CRITICAL: Close tubes BEFORE clearing state
        super().tearDown()  # ← This closes self.my_tube_ids tubes

        # Give more time for OS to release ports, especially mDNS
        delay = float(os.getenv("PYTEST_INTER_TEST_DELAY", "0.5"))
        if delay > 0:
            logging.info(f"Waiting {delay}s for resource cleanup before next test...")
            time.sleep(delay)

        # Clear test-specific state
        with self._lock:
            self.tube_states.clear()
            logging.debug("Cleared tube_states in tearDown.")

            for event in self.tube_connection_events.values():
                event.clear()
            self.tube_connection_events.clear()
            logging.debug("Cleared tube_connection_events in tearDown.")

            self.peer_map.clear()
            logging.debug("Cleared peer_map in tearDown.")

        logging.info(f"{self.__class__.__name__} tearDown completed.")

    def _signal_handler(self, signal_dict):
        """Enhanced signal handler with ICE candidate buffering for missing peer mappings"""
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
                        # If it was connected, and now it's not, clear the event
                        # Or, if tests need to react to failures, set a different event.
                        if tube_id in self.tube_connection_events:
                             self.tube_connection_events[tube_id].clear() # Or handle failure explicitly
                elif kind == "icecandidate":
                    peer_tube_id = self.peer_map.get(tube_id)
                    if peer_tube_id:
                        # Always relay ICE candidates, including empty ones (end-of-candidates signal)
                        if data:  # Non-empty candidate
                            logging.info(f"PYTHON _signal_handler: Relaying ICE candidate from {tube_id} to {peer_tube_id}. Candidate: {data}")
                        else:  # Empty candidate = end-of-candidates signal
                            logging.info(f"PYTHON _signal_handler: Relaying end-of-candidates signal from {tube_id} to {peer_tube_id}")
                        
                        try:
                            self.tube_registry.add_ice_candidate(peer_tube_id, data)
                        except Exception as e:
                            logging.error(f"PYTHON _signal_handler: Failed to add ICE candidate to {peer_tube_id}: {e}")
                    else:
                        # Buffer ICE candidates for missing peer mappings instead of dropping them
                        if not hasattr(self, '_buffered_ice_candidates'):
                            self._buffered_ice_candidates = {}
                        if tube_id not in self._buffered_ice_candidates:
                            self._buffered_ice_candidates[tube_id] = []
                        self._buffered_ice_candidates[tube_id].append(data)
                        
                        candidate_preview = data[:50] + "..." if data and len(data) > 50 else (data or "<empty>")
                        logging.warning(f"PYTHON _signal_handler: No peer entry found for {tube_id} in peer_map. Buffering ICE candidate. Data: {candidate_preview}")
                        
                        # Try to flush buffered candidates if peer mapping becomes available
                        self._try_flush_buffered_candidates_unlocked(tube_id)
                # else: 
                    # Potentially handle other signal kinds like 'icecandidate', 'answer' if needed by tests directly
                    # logging.debug(f"Received other signal for {tube_id}: {kind}")
        except Exception as e:
            logging.error(f"PYTHON _signal_handler CRASHED for signal {signal_dict}: {e}", exc_info=True)
            # Optionally re-raise if PyO3/Rust should see it, but for now, log it
            # to see if this is where the task is dying.
            # Raise # This might be needed if Rust expects to see an error propagate

    def _try_flush_buffered_candidates_unlocked(self, tube_id):
        """Try to flush buffered ICE candidates if peer mapping is now available (assumes lock is held)"""
        try:
            peer_tube_id = self.peer_map.get(tube_id)
            if peer_tube_id and hasattr(self, '_buffered_ice_candidates') and tube_id in self._buffered_ice_candidates:
                buffered_candidates = self._buffered_ice_candidates.pop(tube_id)
                logging.info(f"PYTHON _signal_handler: Flushing {len(buffered_candidates)} buffered ICE candidates from {tube_id} to {peer_tube_id}")
                
                for candidate_data in buffered_candidates:
                    try:
                        self.tube_registry.add_ice_candidate(peer_tube_id, candidate_data)
                        if candidate_data:
                            logging.info(f"PYTHON _signal_handler: Flushed buffered ICE candidate from {tube_id} to {peer_tube_id}")
                        else:
                            logging.info(f"PYTHON _signal_handler: Flushed buffered end-of-candidates signal from {tube_id} to {peer_tube_id}")
                    except Exception as e:
                        logging.error(f"PYTHON _signal_handler: Failed to flush buffered ICE candidate to {peer_tube_id}: {e}")
        except Exception as e:
            logging.error(f"PYTHON _signal_handler: Exception flushing buffered candidates for {tube_id}: {e}", exc_info=True)
    
    @with_runtime
    def test_data_channel_load(self):
        """Test basic tube creation and connection performance"""
        logging.info("Starting tube creation and connection test")

        settings = {"conversationType": "tunnel"}
        
        # Create a server tube
        server_tube_info = self.create_and_track_tube(
            conversation_id="performance-test-server",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._signal_handler
        )
        
        # Get the offer from a server
        offer = server_tube_info['offer']
        server_id = server_tube_info['tube_id']
        self.assertIsNotNone(offer, "Server should generate an offer")
        logging.info(f"Server Offer SDP:\n{offer}") # Log the server's offer SDP
        
        # Create a client tube with the offer
        client_tube_info = self.create_and_track_tube(
            conversation_id="performance-test-client",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
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
        connected = self.wait_for_tube_connection(server_id, client_id, 20) # Increased timeout to 20 s
        connection_time = time.time() - start_time
        
        if not connected:
            # Diagnose the connection failure
            logging.error(f"WebRTC connection failed within {20}s timeout")

            # Get final states for diagnosis
            try:
                server_state = self.tube_registry.get_connection_state(server_id)
                client_state = self.tube_registry.get_connection_state(client_id)
                logging.error(f"Final connection states: server={server_state}, client={client_state}")
            except Exception as e:
                logging.error(f"Could not get connection states: {e}")

            # This is a critical integration test - fail with detailed information
            self.fail("Data channel load test failed due to WebRTC connection failure. This indicates a fundamental integration issue.")

        logging.info(f"Connection established in {connection_time:.2f} seconds")
        
        channel_name = "performance-test-channel"
        channel_settings = {
            "conversationType": "tunnel",
            "ksm_config": TEST_KSM_CONFIG,
            "callback_token": TEST_CALLBACK_TOKEN
        } 
        
        # Add a new conversation to the existing server tube
        self.tube_registry.create_tube(
            conversation_id=channel_name,
            settings=channel_settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            tube_id=server_id  # Add to existing tube
        )
        
        # Clean up
        self.tube_registry.close_connection(channel_name)
        self.close_and_untrack_tube(server_id)
        self.close_and_untrack_tube(client_id)
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
        was_connecting = False  # Track if we ever saw a connecting state

        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            logging.debug(f"Poll: {tube_id1} state: {state1}, {tube_id2} state: {state2}")

            # Track if we ever started connecting (indicates network attempt was made)
            if state1.lower() in ["connecting", "connected"] or state2.lower() in ["connecting", "connected"]:
                was_connecting = True

            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info(f"Connection established between {tube_id1} and {tube_id2}")
                return True

            # If both connections failed after trying to connect, give up early
            if was_connecting and (state1.lower() == "failed" and state2.lower() == "failed"):
                logging.warning(f"Both connections failed after attempting to connect: {tube_id1}={state1}, {tube_id2}={state2}")
                break

            time.sleep(0.1)

        logging.warning(f"Connection establishment timed out for {tube_id1} and {tube_id2}. Final states: {tube_id1}={state1}, {tube_id2}={state2}")
        if not was_connecting:
            logging.warning("Neither connection attempted to connect - possible configuration issue")
        return False

    def wait_for_tubes_ready(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to be ready (data channels open and operational).

        This should be called AFTER wait_for_tube_connection succeeds.
        The 'ready' status indicates the data channel is open and we can safely send/receive data.
        """
        logging.info(f"Waiting for tubes to be ready: {tube_id1} and {tube_id2} (timeout: {timeout}s)")
        start_time = time.time()

        while time.time() - start_time < timeout:
            try:
                status1 = self.tube_registry.get_tube_status(tube_id1)
                status2 = self.tube_registry.get_tube_status(tube_id2)
                logging.debug(f"Poll tube status: {tube_id1}={status1}, {tube_id2}={status2}")

                if status1 == "ready" and status2 == "ready":
                    logging.info(f"Both tubes ready: {tube_id1} and {tube_id2}")
                    return True

                # Check for failure states
                if status1 in ["failed", "closed", "disconnected"] or status2 in ["failed", "closed", "disconnected"]:
                    logging.warning(f"Tube entered failure state: {tube_id1}={status1}, {tube_id2}={status2}")
                    return False

            except Exception as e:
                logging.warning(f"Error getting tube status: {e}")

            time.sleep(0.1)

        # Final status check for logging
        try:
            status1 = self.tube_registry.get_tube_status(tube_id1)
            status2 = self.tube_registry.get_tube_status(tube_id2)
            logging.warning(f"Timeout waiting for tubes ready. Final status: {tube_id1}={status1}, {tube_id2}={status2}")
        except Exception as e:
            logging.warning(f"Could not get final tube status: {e}")

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

            # 2. Pre-populate peer map to avoid losing early ICE candidates
            # We'll generate tube IDs and set up the mapping before creating tubes
            server_tube_id = None
            client_tube_id = None
            
            # 3. Server Tube Setup
            # IMPORTANT: Both tubes must use the SAME conversation_id because WebRTC data channels
            # are shared between peers. The offerer creates a channel with label=conversation_id,
            # and the answerer receives that same channel. If they use different conversation_ids,
            # the answerer will receive a channel with the offerer's conversation_id, not its own.
            shared_conv_id = "e2e-shared-conv"  # Use same ID for both server and client
            server_conv_id = shared_conv_id
            server_settings = {
                "conversationType": "tunnel", # As per Rust test
                "local_listen_addr": "127.0.0.1:0" # Server tube listens here, dynamic port
            }

            # The create_tube in Python seems to be a bit different from Rust's.
            # It might not directly expose it on_ice_candidate per-tube object in the same way.
            # We are using the BaseWebRTCTest's on_ice_candidate1/2, which is generic.
            # We need to ensure these are somehow linked or the library handles it.
            # For now, let's assume the library's PyTubeRegistry might have a way to globally set these
            # or that `create_tube` itself registers some internal callbacks.
            # The existing tests use self.tube_registry.get_connection_state, implying ICE is handled.

            logging.info(f"[E2E_Test] Creating server tube with settings: {server_settings}")
            server_tube_info = self.create_and_track_tube(
                conversation_id=server_conv_id,
                settings=server_settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
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

            # 4. Client Tube Setup
            client_conv_id = shared_conv_id  # Use SAME conversation_id as server
            client_settings = {
                "conversationType": "tunnel",
                "target_host": "127.0.0.1",
                "target_port": str(ack_server.actual_port) # Connect to AckServer
            }
            logging.info(f"[E2E_Test] Creating client tube with offer and settings: {client_settings}")
            client_tube_info = self.create_and_track_tube(
                conversation_id=client_conv_id,
                settings=client_settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                offer=server_offer_sdp,
                signal_callback=self._signal_handler
            )
            self.assertIsNotNone(client_tube_info, "Client tube creation failed")
            client_answer_sdp = client_tube_info['answer']
            client_tube_id = client_tube_info['tube_id']

            self.assertIsNotNone(client_answer_sdp, "Client should generate an answer")
            self.assertIsNotNone(client_tube_id, "Client tube should have an ID")
            logging.info(f"[E2E_Test] Client tube {client_tube_id} created. Answer generated.")

            # 5. Populate the peer map for ICE candidate relaying (Do this immediately after getting tube IDs)
            with self._lock:
                self.peer_map[server_tube_id] = client_tube_id
                self.peer_map[client_tube_id] = server_tube_id

            logging.info(f"[E2E_Test] Peer map populated: {server_tube_id} <-> {client_tube_id}")

            # 6. Flush any buffered ICE candidates and add brief delay for ICE negotiation to start
            with self._lock:
                # Flush any buffered ICE candidates now that peer mapping is available
                self._try_flush_buffered_candidates_unlocked(server_tube_id)
                self._try_flush_buffered_candidates_unlocked(client_tube_id)

            # Give ICE candidates time to be exchanged
            import time
            time.sleep(0.5)

            # 6. Signaling: Set remote description
            # The Rust test has a more elaborate ICE exchange via signal channels.
            # Python tests rely on `wait_for_tube_connection,` which implies internal ICE handling after SDP exchange.
            logging.info(f"[E2E_Test] Server tube {server_tube_id} setting remote description (client's answer)")
            self.tube_registry.set_remote_description(server_tube_id, client_answer_sdp, is_answer=True)
            
            # The client tube in Rust's `create_tube` (when offer is provided) also calls `set_remote_description` internally for the offer.
            # And then `create_answer`. The Python `create_tube` with an offer likely does this too.
            # We might not need explicit `set_remote_description` on the client side if `create_tube` handles the initial offer.

            # 7. Wait for connection
            logging.info(f"[E2E_Test] Waiting for WebRTC connection between {server_tube_id} and {client_tube_id}...")
            connected = self.wait_for_tube_connection(server_tube_id, client_tube_id, timeout=20) # Increased timeout for E2E
            if not connected:
                # Diagnose why the connection failed
                logging.error(f"[E2E_Test] WebRTC connection failed between {server_tube_id} and {client_tube_id}")

                # Get final states for diagnosis
                try:
                    server_state = self.tube_registry.get_connection_state(server_tube_id)
                    client_state = self.tube_registry.get_connection_state(client_tube_id)
                    logging.error(f"Final states: server={server_state}, client={client_state}")
                except Exception as e:
                    logging.error(f"Could not get final connection states: {e}")

                # Check if ICE candidates were exchanged
                ice_exchange_info = ""
                if hasattr(self, '_buffered_ice_candidates'):
                    server_candidates = len(self._buffered_ice_candidates.get(server_tube_id, []))
                    client_candidates = len(self._buffered_ice_candidates.get(client_tube_id, []))
                    ice_exchange_info = f" Buffered candidates: server={server_candidates}, client={client_candidates}"

                # This is a critical integration test - fail with detailed information
                self.fail(f"E2E WebRTC connection failed. This indicates a fundamental issue with WebRTC integration.{ice_exchange_info}")

            logging.info(f"[E2E_Test] WebRTC connection established.")

            # 7b. Wait for data channels to be ready (tube status == "ready")
            # This is critical! The ICE connection being established doesn't mean data channels are open.
            # We must wait for tubes to reach "ready" state before attempting to send/receive data.
            # Note: WebRTC negotiation can take several seconds, especially in test environments.
            # The Rust P2P test takes ~8s, so allow up to 30s for Python overhead and variability.
            logging.info(f"[E2E_Test] Waiting for data channels to be ready...")
            ready = self.wait_for_tubes_ready(server_tube_id, client_tube_id, timeout=30)
            if not ready:
                # Get status for diagnosis
                try:
                    server_status = self.tube_registry.get_tube_status(server_tube_id)
                    client_status = self.tube_registry.get_tube_status(client_tube_id)
                    logging.error(f"Final tube status: server={server_status}, client={client_status}")
                except Exception as e:
                    logging.error(f"Could not get final tube status: {e}")
                self.fail("E2E test failed: Data channels did not become ready in time")

            logging.info(f"[E2E_Test] Data channels ready.")

            # 8. Simulate External Client connecting to Server Tube's local TCP endpoint
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

            # 9. Send a message from External Client
            message_content = "Hello Proxied World via Python!"
            message_bytes = message_content.encode('utf-8')
            external_client_socket.sendall(message_bytes)
            logging.info(f"[E2E_Test] External client sent: '{message_content}'")

            # 10. Receive acked message by External Client
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

            # Close and untrack tubes (tearDown will handle them if this fails)
            if server_tube_id:
                self.close_and_untrack_tube(server_tube_id)
                logging.info(f"[E2E_Test] Server tube {server_tube_id} closed.")

            if client_tube_id:
                self.close_and_untrack_tube(client_tube_id)
                logging.info(f"[E2E_Test] Client tube {client_tube_id} closed.")

            # Clear peer map
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
        super().setUp()  # ← This now sets self.tube_registry to shared instance
        # Removed: self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        # Removed: self.configure_test_resource_limits(self.tube_registry)
        # Both handled by BaseWebRTCTest.setUp()

        self.tube_states = {}  # Stores current state of each tube_id
        self.tube_connection_events = {} # tube_id -> threading.Event for connected state
        self._lock = threading.Lock() # To protect access to shared tube_states and events
        self.peer_map = {} # To map a tube_id to its peer for ICE candidate relay

    def tearDown(self):
        # ✅ CRITICAL: Close tubes BEFORE clearing state
        super().tearDown()  # ← This closes self.my_tube_ids tubes

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
        server_tube_info = self.create_and_track_tube(
            conversation_id="fragmentation-test-server",
            settings=settings,
            trickle_ice=False,  # Use non-trickle ICE
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG
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
        client_settings = {"conversationType": "tunnel"} # Ensure the client also has its own settings if needed
        client_tube_info = self.create_and_track_tube(
            conversation_id="fragmentation-test-client",
            settings=client_settings, # Pass original settings, not modified ones
            trickle_ice=False,  # Use non-trickle ICE
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            offer=offer_b64 # Pass the original base64 encoded offer
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
        
        if not connected:
            # Diagnose the non-trickle connection failure
            logging.error(f"Non-trickle WebRTC connection failed within {15}s timeout")

            # Get final states for diagnosis
            try:
                server_state = self.tube_registry.get_connection_state(server_id)
                client_state = self.tube_registry.get_connection_state(client_id)
                logging.error(f"Final connection states: server={server_state}, client={client_state}")
            except Exception as e:
                logging.error(f"Could not get connection states: {e}")

            # This is a critical integration test - fail with detailed information
            self.fail("Data channel fragmentation test failed due to non-trickle WebRTC connection failure. This indicates a fundamental integration issue.")

        logging.info(f"Non-trickle ICE connection established in {connection_time:.2f} seconds")

        # Clean up
        self.close_and_untrack_tube(server_id)
        self.close_and_untrack_tube(client_id)

        logging.info("Tube connection reliability test completed")

    @with_runtime
    def test_e2e_fragmentation_bidirectional(self):
        """
        End-to-end test for WebRTC multi-channel fragmentation integration.

        This test validates that the fragmentation pipeline is correctly wired up:
        1. enable_multi_channel=True enables FRAGMENTATION capability
        2. Assembler is created for each channel with cleanup task
        3. Large data (up to 100KB) flows correctly through WebRTC in both directions
        4. Data integrity is maintained across the full pipeline

        Note: The TCP backend reads in 8KB chunks (MAX_READ_SIZE), so individual frames
        won't exceed the 16KB fragment threshold in this test. However, this verifies
        the integration is correctly plumbed. The Rust unit tests (assembler_tests.rs)
        directly verify the fragment_frame() code path for frames > 16KB.
        """
        logging.info("Starting E2E fragmentation bidirectional test")
        echo_server = None
        external_client_socket = None
        server_tube_id = None
        client_tube_id = None

        # Test payloads: small (no frag), medium (some frags), large (many frags)
        # Fragment threshold is 16KB, so:
        # - 8KB: no fragmentation needed
        # - 24KB: ~2 fragments
        # - 50KB: ~4 fragments
        # - 100KB: ~7 fragments
        test_sizes = [8 * 1024, 24 * 1024, 50 * 1024, 100 * 1024]

        try:
            # 1. Start an exact echo server (echoes bytes back exactly)
            echo_server = ExactEchoServer()
            echo_server.start()

            # Wait for server to be ready
            timeout = 5
            start_time = time.time()
            while not echo_server.actual_port and time.time() - start_time < timeout:
                time.sleep(0.01)

            self.assertIsNotNone(echo_server.actual_port, "Echo server did not start")
            logging.info(f"[FragTest] ExactEchoServer running on 127.0.0.1:{echo_server.actual_port}")

            # 2. Create server tube (accepts external TCP connections)
            server_settings = {
                "conversationType": "tunnel",
                "local_listen_addr": "127.0.0.1:0"  # Dynamic port
            }

            server_tube_info = self.create_and_track_tube(
                conversation_id="frag-e2e-server",
                settings=server_settings,
                trickle_ice=False,  # Non-trickle for simpler test
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                enable_multi_channel=True  # Enable fragmentation
            )

            server_offer_b64 = server_tube_info['offer']
            server_tube_id = server_tube_info['tube_id']
            server_listen_addr = server_tube_info.get('actual_local_listen_addr')

            self.assertIsNotNone(server_offer_b64, "Server should generate an offer")
            self.assertIsNotNone(server_listen_addr, "Server should have listen address")
            logging.info(f"[FragTest] Server tube {server_tube_id} listening on {server_listen_addr}")

            # 3. Create client tube (connects to echo server backend)
            client_settings = {
                "conversationType": "tunnel",
                "target_host": "127.0.0.1",
                "target_port": str(echo_server.actual_port)
            }

            client_tube_info = self.create_and_track_tube(
                conversation_id="frag-e2e-client",
                settings=client_settings,
                trickle_ice=False,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                offer=server_offer_b64,
                enable_multi_channel=True  # Enable fragmentation
            )

            client_answer_b64 = client_tube_info['answer']
            client_tube_id = client_tube_info['tube_id']

            self.assertIsNotNone(client_answer_b64, "Client should generate an answer")
            logging.info(f"[FragTest] Client tube {client_tube_id} created")

            # 4. Complete signaling
            self.tube_registry.set_remote_description(server_tube_id, client_answer_b64, is_answer=True)

            # 5. Wait for WebRTC connection
            connected = self.wait_for_tube_connection(server_tube_id, client_tube_id, timeout=20)
            if not connected:
                server_state = self.tube_registry.get_connection_state(server_tube_id)
                client_state = self.tube_registry.get_connection_state(client_tube_id)
                self.fail(f"WebRTC connection failed. States: server={server_state}, client={client_state}")

            logging.info("[FragTest] WebRTC connection established")

            # 6. Connect external client to server tube's TCP listener
            parts = server_listen_addr.split(':')
            server_host = parts[0]
            server_port = int(parts[1])

            external_client_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            external_client_socket.settimeout(30)  # Long timeout for large payloads
            external_client_socket.connect((server_host, server_port))
            logging.info(f"[FragTest] External client connected to {server_listen_addr}")

            # 7. Test each payload size
            for size in test_sizes:
                # Generate deterministic test data
                test_data = bytes([(i % 256) for i in range(size)])

                frag_info = ""
                if size > 16 * 1024:
                    num_frags = (size + 16 * 1024 - 1) // (16 * 1024)
                    frag_info = f" (~{num_frags} fragments)"

                logging.info(f"[FragTest] Testing {size // 1024}KB payload{frag_info}")

                # Send data
                external_client_socket.sendall(test_data)

                # Receive echoed data
                received = bytearray()
                recv_start = time.time()
                while len(received) < size and time.time() - recv_start < 30:
                    try:
                        chunk = external_client_socket.recv(min(65536, size - len(received)))
                        if not chunk:
                            break
                        received.extend(chunk)
                    except socket.timeout:
                        continue

                # Verify
                self.assertEqual(len(received), size,
                    f"Size mismatch for {size // 1024}KB: got {len(received)} bytes")
                self.assertEqual(bytes(received), test_data,
                    f"Data corruption for {size // 1024}KB payload")

                logging.info(f"[FragTest] {size // 1024}KB payload verified successfully")

            logging.info("[FragTest] All fragmentation tests passed!")

        except Exception as e:
            logging.error(f"[FragTest] Exception: {e}", exc_info=True)
            self.fail(f"E2E fragmentation test failed: {e}")
        finally:
            logging.info("[FragTest] Cleaning up...")

            if external_client_socket:
                try:
                    external_client_socket.close()
                except Exception as e:
                    logging.error(f"[FragTest] Error closing socket: {e}")

            if server_tube_id:
                self.close_and_untrack_tube(server_tube_id)

            if client_tube_id:
                self.close_and_untrack_tube(client_tube_id)

            if echo_server:
                echo_server.stop()

            logging.info("[FragTest] Cleanup completed")
    
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