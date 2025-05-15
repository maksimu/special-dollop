import functools
import asyncio
import logging
import time
import threading
from queue import Queue, Empty

import keeper_pam_webrtc_rs

def with_runtime(func):
    """Decorator to ensure the test runs with its own Tokio runtime context"""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        # Ensure we have an asyncio event loop
        try:
            loop = asyncio.get_event_loop()
        except RuntimeError:
            loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        init_logger()

        # Run test in the main thread to avoid Tokio runtime deadlocks
        # This avoids thread pool executors which can lead to nested runtime issues
        try:
            return func(*args, **kwargs)
        except Exception as ex:
            raise ex
    return wrapper

class BaseWebRTCTest:
    """Base class with common WebRTC test functionality"""
    
    def setUp(self):
        self.ice_candidates1 = Queue()
        self.ice_candidates2 = Queue()
        self.data_channel_received = Queue()
        self.received_messages = Queue()
        self.connection_established = threading.Event()
        logging.info(f"{self.__class__.__name__} setup completed")

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

        # Put the channel in the queue and log the queue size
        self.data_channel_received.put(dc)
        logging.info(f"Data channel {dc.label} added to queue, queue size: {self.data_channel_received.qsize()}")

    def on_message(self, msg):
        logging.info(f"Message received: {len(msg)} bytes")
        self.received_messages.put(msg)

    def wait_for_connection(self, peer1, peer2, timeout=10):
        """Wait for both peers to establish a connection"""
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

            # Check if a connection is established
            if peer1.connection_state == "Connected" and peer2.connection_state == "Connected":
                logging.info("Connection established during ICE exchange")
                return True

            time.sleep(0.1)

        logging.warning("ICE exchange timed out")
        return False

def init_logger():
    """Initialize the Rust logger to use Python's logging system"""
    try:
        keeper_pam_webrtc_rs.initialize_logger("keeper_pam_webrtc_rs", verbose=True, level=logging.DEBUG)
        logging.info("Rust logger initialized successfully")
    except Exception as e:
        logging.error(f"Failed to initialize Rust logger: {e}") 