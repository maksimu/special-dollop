import queue
import uuid
from concurrent.futures import ThreadPoolExecutor
from queue import Queue, Empty
import asyncio
import functools
import logging
import time
import threading
import unittest

try:
    import pam_rustwebrtc
except ImportError as e:
    logging.error(f"Failed to import pam_rustwebrtc: {e}")
    logging.error("Make sure the module is built and installed before running tests")
    raise

def with_runtime(func):
    """Decorator to ensure the test runs in both asyncio and tokio runtime contexts"""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        # Ensure we have an asyncio event loop
        try:
            loop = asyncio.get_event_loop()
        except RuntimeError:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)

        # Create a ThreadPoolExecutor for running the test
        with ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(func, *args, **kwargs)
            try:
                return future.result()
            except Exception as e:
                raise e
    return wrapper

class TestWebRTC(unittest.TestCase):
    def setUp(self):
        self.ice_candidates1 = Queue()
        self.ice_candidates2 = Queue()
        self.data_channel_received = Queue()
        self.received_messages = Queue()
        self.connection_established = threading.Event()
        logging.info("Test setup completed")

    def on_ice_candidate1(self, candidate):
        if candidate:
            logging.info(f"Peer1 ICE candidate: {candidate}")
            self.ice_candidates1.put(candidate)

    def on_ice_candidate2(self, candidate):
        if candidate:
            logging.info(f"Peer2 ICE candidate: {candidate}")
            self.ice_candidates2.put(candidate)

    def on_data_channel(self, dc):
        logging.info(f"Data channel received: {dc.label}")
        dc.on_message = self.on_message
        self.data_channel_received.put(dc)

    def on_message(self, msg):
        logging.info(f"Message received: {len(msg)} bytes")
        self.received_messages.put(msg)

    def wait_for_connection(self, peer1, peer2, timeout=10):
        """Wait for both peers to establish connection"""
        logging.info(f"Waiting for connection establishment (timeout: {timeout}s)")
        start_time = time.time()
        while time.time() - start_time < timeout:
            if (peer1.connection_state == "Connected" and
                    peer2.connection_state == "Connected"):
                logging.info("Connection established")
                return True
            time.sleep(0.1)
        logging.warning("Connection establishment timed out")
        return False

    def exchange_ice_candidates(self, peer1, peer2, timeout=5):
        """Exchange ICE candidates between peers"""
        logging.info(f"Starting ICE candidate exchange (timeout: {timeout}s)")
        start_time = time.time()
        while time.time() - start_time < timeout:
            # Handle peer1's candidates
            try:
                while True:  # Process all available candidates
                    candidate = self.ice_candidates1.get_nowait()
                    try:
                        peer2.add_ice_candidate(candidate)
                        logging.info("Added ICE candidate to peer2")
                    except Exception as e:
                        logging.error(f"Failed to add ICE candidate to peer2: {e}")
            except Empty:
                pass

            # Handle peer2's candidates
            try:
                while True:  # Process all available candidates
                    candidate = self.ice_candidates2.get_nowait()
                    try:
                        peer1.add_ice_candidate(candidate)
                        logging.info("Added ICE candidate to peer1")
                    except Exception as e:
                        logging.error(f"Failed to add ICE candidate to peer1: {e}")
            except Empty:
                pass

            # Check if connection is established
            if peer1.connection_state == "Connected" and peer2.connection_state == "Connected":
                logging.info("Connection established during ICE exchange")
                return True

            time.sleep(0.1)

        logging.warning("ICE exchange timed out")
        return False

    @with_runtime
    def test_peer_connection_creation(self):
        logging.info("Starting peer connection creation test")
        config = {"iceServers": []}
        pc = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        self.assertEqual(pc.connection_state, "New")
        logging.info("Peer connection creation test passed")

    @with_runtime
    def test_peer_connection_with_config(self):
        logging.info("Starting peer connection with config test")
        config = {
            "iceServers": [
                {
                    "urls": ["stun:stun.l.google.com:19302"],
                    "username": "",
                    "credential": ""
                }
            ]
        }
        pc = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        self.assertEqual(pc.connection_state, "New")
        logging.info("Peer connection with config test passed")

    @with_runtime
    def test_data_channel_creation(self):
        logging.info("Starting data channel creation test")
        config = {"iceServers": []}
        pc = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        dc = pc.create_data_channel("test")
        self.assertEqual(dc.label, "test")
        self.assertEqual(dc.ready_state, "Connecting")
        logging.info("Data channel creation test passed")

    @with_runtime
    def test_p2p_connection(self):
        """Test peer connection with trickle ICE"""
        logging.info("Starting P2P connection test with trickle ICE")

        # Create peer connections
        config = {"iceServers": []}
        peer1 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        peer2 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate2,
            self.on_data_channel,
            trickle_ice=True
        )

        # Create data channel
        dc1 = peer1.create_data_channel("test-channel")
        logging.info("Created data channel on peer1")

        # Create offer
        offer = peer1.create_offer()
        logging.info("Created offer")

        # Start exchanging ICE candidates in a separate thread
        ice_thread = threading.Thread(
            target=self.exchange_ice_candidates,
            args=(peer1, peer2),
            daemon=True
        )
        ice_thread.start()

        # Set remote description (offer) on peer2
        try:
            peer2.set_remote_description(offer)
            logging.info("Set remote description on peer2")
        except Exception as e:
            logging.error(f"Failed to set remote description on peer2: {e}")
            raise

        # Create answer
        answer = peer2.create_answer()
        logging.info("Created answer")

        # Set remote description (answer) on peer1
        try:
            peer1.set_remote_description(answer)
            logging.info("Set remote description on peer1")
        except Exception as e:
            logging.error(f"Failed to set remote description on peer1: {e}")
            raise

        # Wait for connection to establish
        self.assertTrue(
            self.wait_for_connection(peer1, peer2),
            "Failed to establish connection"
        )

        # Verify data channel was received
        dc2 = self.data_channel_received.get(timeout=5)
        self.assertEqual(dc2.label, "test-channel")

        # Test data channel communication
        message = b"Hello WebRTC!"
        logging.info(f"Sending message: {message}")
        dc1.send(message)
        received = self.received_messages.get(timeout=5)
        self.assertEqual(received, message)
        logging.info("Message received correctly")

        # Clean up
        dc1.close()
        dc2.close()
        logging.info("P2P connection test passed")

    @with_runtime
    def test_p2p_connection_non_trickle(self):
        """Test peer connection without trickle ICE"""
        logging.info("Starting P2P connection test without trickle ICE")

        config = {
            "iceServers": [
                {
                    "urls": ["stun:stun.l.google.com:19302"],
                    "username": "",
                    "credential": ""
                }
            ]
        }
        peer1 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            None,  # No ICE candidate callback for non-trickle
            self.on_data_channel,
            trickle_ice=False
        )
        peer2 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            None,  # No ICE candidate callback for non-trickle
            self.on_data_channel,
            trickle_ice=False
        )

        # Create data channel
        dc1 = peer1.create_data_channel("test-channel")
        logging.info("Created data channel")

        # Create and set offer
        try:
            offer = peer1.create_offer()
            logging.info(f"Created offer (length: {len(offer)} bytes)")
            self.assertIn("a=candidate:", offer, "Offer should contain ICE candidates")
            peer2.set_remote_description(offer)
            logging.info("Offer exchange completed")
        except Exception as e:
            logging.error(f"Failed during offer exchange: {e}")
            raise

        # Create and set answer
        try:
            answer = peer2.create_answer()
            logging.info(f"Created answer (length: {len(answer)} bytes)")
            self.assertIn("a=candidate:", answer, "Answer should contain ICE candidates")
            peer1.set_remote_description(answer)
            logging.info("Answer exchange completed")
        except Exception as e:
            logging.error(f"Failed during answer exchange: {e}")
            raise

        # Wait for connection
        self.assertTrue(
            self.wait_for_connection(peer1, peer2, timeout=15),
            "Failed to establish connection"
        )

        # Verify data channel
        dc2 = self.data_channel_received.get(timeout=5)
        self.assertEqual(dc2.label, "test-channel")

        # Test communication
        message = b"Hello WebRTC!"
        logging.info(f"Sending message: {message}")
        dc1.send(message)
        received = self.received_messages.get(timeout=5)
        self.assertEqual(received, message)
        logging.info("Message received correctly")

        # Clean up
        dc1.close()
        dc2.close()
        logging.info("Non-trickle P2P connection test passed")

    @with_runtime
    def test_data_channel_load(self):
        """Test data channel performance with large data transfers"""
        logging.info("Starting data channel load test")

        # Create peer connections
        config = {"iceServers": []}
        peer1 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        peer2 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate2,
            self.on_data_channel,
            trickle_ice=True
        )

        # Create data channel
        dc1 = peer1.create_data_channel("load-test-channel")
        logging.info("Created data channel with performance options")

        # Set up connection
        offer = peer1.create_offer()
        ice_thread = threading.Thread(
            target=self.exchange_ice_candidates,
            args=(peer1, peer2),
            daemon=True
        )
        ice_thread.start()

        peer2.set_remote_description(offer)
        answer = peer2.create_answer()
        peer1.set_remote_description(answer)

        # Wait for connection and data channel
        self.assertTrue(
            self.wait_for_connection(peer1, peer2),
            "Failed to establish connection"
        )
        dc2 = self.data_channel_received.get(timeout=5)
        self.assertEqual(dc2.label, "load-test-channel")

        # Test data configurations - keeping messages under 16KB to avoid fragmentation
        data_sizes = [
            (1024, 100),        # 100 KB total in 1KB chunks
            (8192, 20),         # 160 KB total in 8KB chunks
            (16384, 10),        # 160 KB total in 16KB chunks
        ]

        for chunk_size, num_messages in data_sizes:
            with self.subTest(f"Testing {chunk_size * num_messages / 1024:.0f}KB total in {num_messages} messages"):
                logging.info(f"Testing {chunk_size * num_messages / 1024:.0f}KB total in {num_messages} messages")

                # Create test data
                test_data = b"x" * chunk_size
                received_count = 0
                start_time = time.time()

                # Reset message queue
                while not self.received_messages.empty():
                    self.received_messages.get_nowait()

                # Set up message counter
                def count_messages(msg):
                    nonlocal received_count
                    received_count += 1
                    logging.debug(f"Received message {received_count}/{num_messages} ({len(msg)} bytes)")
                    self.received_messages.put(msg)

                # Set message handler
                dc2.on_message = count_messages

                # Send messages with a small delay between chunks to prevent overloading
                for i in range(num_messages):
                    dc1.send(test_data)
                    if i % 10 == 0:  # Add a small delay every 10 messages
                        time.sleep(0.01)
                    if i % 20 == 0:
                        logging.info(f"Sent {i}/{num_messages} messages")

                # Wait for all messages
                try:
                    timeout = 30  # 30 second timeout
                    while received_count < num_messages and (time.time() - start_time) < timeout:
                        time.sleep(0.1)

                    elapsed_time = time.time() - start_time
                    total_bytes = chunk_size * num_messages
                    throughput = (total_bytes / 1024 / 1024) / elapsed_time  # MB/s

                    logging.info(f"Transfer complete: {total_bytes/1024:.1f}KB in {elapsed_time:.2f}s")
                    logging.info(f"Throughput: {throughput:.2f} MB/s")
                    logging.info(f"Messages received: {received_count}/{num_messages}")

                    # Verify results
                    self.assertEqual(received_count, num_messages,
                                     f"Only received {received_count}/{num_messages} messages")

                    # Verify data integrity for last message
                    last_received = self.received_messages.get_nowait()
                    self.assertEqual(len(last_received), chunk_size,
                                     "Received message size doesn't match sent size")
                    self.assertEqual(last_received, test_data,
                                     "Received message content doesn't match sent content")

                finally:
                    # Clear the message handler
                    dc2.on_message = None
                    logging.info(f"Completed test for {chunk_size} byte chunks")

        # Clean up
        dc1.close()
        dc2.close()
        logging.info("Data channel load test passed")

    @with_runtime
    def test_data_channel_reconnection(self):
        """Test data channel behavior when connection is maintained but channel closes"""
        logging.info("Starting data channel reconnection test")

        # Create peer connections with same configuration as your production code
        config = {"iceServers": []}
        peer1 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate1,
            self.on_data_channel,
            trickle_ice=True
        )
        peer2 = pam_rustwebrtc.PyRTCPeerConnection(
            config,
            self.on_ice_candidate2,
            self.on_data_channel,
            trickle_ice=True
        )

        # Connect peers and establish data channel
        dc1 = peer1.create_data_channel("control")
        logging.info("Created data channel on peer1")

        # Setup signaling
        offer = peer1.create_offer()
        logging.info("Created offer")

        # Exchange ICE candidates
        ice_thread = threading.Thread(
            target=self.exchange_ice_candidates,
            args=(peer1, peer2),
            daemon=True
        )
        ice_thread.start()

        peer2.set_remote_description(offer)
        logging.info("Set remote description on peer2")

        answer = peer2.create_answer()
        logging.info("Created answer")

        peer1.set_remote_description(answer)
        logging.info("Set remote description on peer1")

        # Wait for connection to establish
        self.assertTrue(
            self.wait_for_connection(peer1, peer2),
            "Failed to establish connection"
        )

        # Get the data channel from peer2
        dc2 = self.data_channel_received.get(timeout=5)
        self.assertEqual(dc2.label, "control")

        # Establish message queue similar to your WebRTCConnection class
        message_queue = queue.Queue()

        def queue_message_handler(msg):
            message_id = uuid.uuid4().hex[:8]
            logging.info(f"Received message ({len(msg)} bytes), ID: {message_id}")
            message_queue.put((message_id, msg))

        # Set the message handler
        dc2.on_message = queue_message_handler

        # Test data transmission
        for i in range(5):
            # Format message similar to your packet structure
            connection_no = 1
            timestamp = int(time.time() * 1000)
            test_data = f"Test message {i}".encode()

            # Build a packet similar to your production code
            packet = (
                    int.to_bytes(connection_no, 4, byteorder='big') +  # Connection number (4 bytes)
                    int.to_bytes(timestamp, 8, byteorder='big') +      # Timestamp (8 bytes)
                    int.to_bytes(len(test_data), 4, byteorder='big') + # Data length (4 bytes)
                    test_data +                                         # Actual data
                    b';;;;;'                                           # Terminator (similar to yours)
            )

            dc1.send(packet)
            logging.info(f"Sent packet {i}")

            # Wait for message to be received
            try:
                msg_id, received = message_queue.get(timeout=3)
                self.assertEqual(received, packet, f"Received data doesn't match for packet {i}")
                logging.info(f"Successfully received packet {i}")
            except queue.Empty:
                self.fail(f"Did not receive packet {i}")

        # Now simulate data channel closing but connection remaining open
        # This part is tricky - we'll close dc1 but keep the PC connection
        logging.info("Simulating data channel closure but maintaining connection")
        dc1.close()

        # Wait for the state to update
        time.sleep(1)

        # Verify the state matches what you see in production
        self.assertEqual(peer1.connection_state, "Connected", "Peer connection should still be connected")
        # It might take time for dc1's state to change to Closed
        max_attempts = 20
        attempts = 0
        while attempts < max_attempts:
            if dc1.ready_state == "Closed":
                break
            time.sleep(0.5)
            attempts += 1
            logging.debug(f"Waiting for data channel to close, current state: {dc1.ready_state}")

        self.assertIn(dc1.ready_state, ["Closing", "Closed"], "Data channel should be closed state")
        logging.info(f"Data channel closed successfully after {attempts} attempts")

        # Try to create a new data channel and test data flow again
        logging.info("Attempting to create a new data channel after closure")
        dc1_new = peer1.create_data_channel("control_new")
        logging.info(f"Created new data channel: {dc1_new.label}")

        # Wait for the new channel to be received
        try:
            dc2_new = self.data_channel_received.get(timeout=5)
            self.assertEqual(dc2_new.label, "control_new")
            logging.info("New data channel received by peer2")

            # Set message handler for the new channel
            dc2_new.on_message = queue_message_handler

            # Test data transmission with the new channel
            test_msg = b"New channel test"
            logging.info(f"Sending test message on new channel: {test_msg}")
            dc1_new.send(test_msg)

            msg_id, received = message_queue.get(timeout=3)
            self.assertEqual(received, test_msg)
            logging.info("Successfully transmitted on new data channel")
        except Exception as e:
            logging.error(f"Failed during new data channel creation: {e}")
            self.fail(f"Failed to create new data channel: {e}")

        # Clean up
        dc1.close()
        if dc1_new:
            dc1_new.close()
        dc2.close()
        if dc2_new:
            dc2_new.close()
        logging.info("Data channel reconnection test passed")

    @with_runtime
    def test_sequential_connection_creation(self):
        """Test creating connections one after another"""
        logging.info("Testing sequential connection creation")

        for i in range(3):  # Try creating 3 connections in sequence
            logging.info(f"Creating connection {i+1}")

            # Reset test state
            while not self.ice_candidates1.empty():
                self.ice_candidates1.get_nowait()
            while not self.ice_candidates2.empty():
                self.ice_candidates2.get_nowait()
            while not self.data_channel_received.empty():
                self.data_channel_received.get_nowait()
            while not self.received_messages.empty():
                self.received_messages.get_nowait()

            # Create peer connections
            config = {"iceServers": []}
            peer1 = pam_rustwebrtc.PyRTCPeerConnection(
                config,
                self.on_ice_candidate1,
                self.on_data_channel,
                trickle_ice=True
            )
            peer2 = pam_rustwebrtc.PyRTCPeerConnection(
                config,
                self.on_ice_candidate2,
                self.on_data_channel,
                trickle_ice=True
            )

            # Create data channel
            dc1 = peer1.create_data_channel(f"test-channel-{i}")
            logging.info(f"Created data channel {i+1}")

            # Set up connection
            offer = peer1.create_offer()
            peer2.set_remote_description(offer)
            answer = peer2.create_answer()
            peer1.set_remote_description(answer)

            # Exchange ICE candidates
            self.exchange_ice_candidates(peer1, peer2)

            # Wait for connection
            self.assertTrue(
                self.wait_for_connection(peer1, peer2),
                f"Failed to establish connection {i+1}"
            )

            # Get data channel
            dc2 = self.data_channel_received.get(timeout=5)

            # Test data transfer
            test_msg = f"Hello from connection {i+1}".encode()
            dc1.send(test_msg)
            received = self.received_messages.get(timeout=5)
            self.assertEqual(received, test_msg)

            # Close everything
            logging.info(f"Closing connection {i+1}")
            dc1.close()
            dc2.close()
            peer1.close()
            peer2.close()

            # Force garbage collection
            import gc
            gc.collect()

            # Add a delay to ensure complete cleanup
            time.sleep(1)

        logging.info("Sequential connection creation test passed")

if __name__ == '__main__':
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )

    # Initialize the Rust logger to use Python's logging system
    try:
        pam_rustwebrtc.initialize_logger("pam_rustwebrtc", verbose=True, level=logging.DEBUG)
        logging.info("Rust logger initialized successfully")
    except Exception as e:
        logging.error(f"Failed to initialize Rust logger: {e}")

    unittest.main()