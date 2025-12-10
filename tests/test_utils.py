import functools
import asyncio
import logging
import socket
import time
import threading
from queue import Queue, Empty

import keeper_pam_webrtc_rs

# Global flag to ensure logger is initialized only once
RUST_LOGGER_INITIALIZED = False

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

    # ✅ ONE shared registry for all tests (process-wide singleton)
    _shared_registry = None
    _registry_lock = threading.Lock()

    def setUp(self):
        # Initialize shared registry once (thread-safe)
        with BaseWebRTCTest._registry_lock:
            if BaseWebRTCTest._shared_registry is None:
                BaseWebRTCTest._shared_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
                # Configure higher resource limits for testing
                test_config = {
                    "max_concurrent_ice_agents": 128,  # Increased from 32 for tests
                    "max_concurrent_sockets": 256,     # Increased from 64 for tests
                    "max_interfaces_per_agent": 16,    # Increased from 8 for tests
                    "max_turn_connections_per_server": 8  # Increased from 4 for tests
                }
                try:
                    BaseWebRTCTest._shared_registry.configure_resource_limits(test_config)
                    logging.info(f"Configured shared test registry with limits: {test_config}")
                except Exception as e:
                    logging.warning(f"Failed to configure shared registry limits: {e}")

        # Each test gets reference to THE shared registry
        self.tube_registry = BaseWebRTCTest._shared_registry

        # ✅ Track tubes created by THIS test for cleanup
        self.my_tube_ids = []

        # Existing setUp code
        self.ice_candidates1 = Queue()
        self.ice_candidates2 = Queue()
        self.data_channel_received = Queue()
        self.received_messages = Queue()
        self.connection_established = threading.Event()
        logging.info(f"{self.__class__.__name__} setup completed")

    def create_and_track_tube(self, **kwargs):
        """
        Helper to create tube and automatically track for cleanup.
        Use this instead of self.tube_registry.create_tube() directly.
        """
        tube_info = self.tube_registry.create_tube(**kwargs)
        tube_id = tube_info['tube_id']
        self.my_tube_ids.append(tube_id)
        logging.debug(f"Created and tracking tube: {tube_id}")
        return tube_info

    def close_and_untrack_tube(self, tube_id):
        """
        Helper to close tube and remove from tracking.
        Safe to call even if tube is already closed.
        """
        try:
            self.tube_registry.close_tube(tube_id)
            if tube_id in self.my_tube_ids:
                self.my_tube_ids.remove(tube_id)
            logging.debug(f"Closed and untracked tube: {tube_id}")
        except Exception as e:
            # Tube already closed - that's fine
            logging.debug(f"Tube {tube_id} already closed or not found: {e}")
            # Still remove from tracking
            if tube_id in self.my_tube_ids:
                self.my_tube_ids.remove(tube_id)

    def tearDown(self):
        """Clean up tubes created by THIS test only"""
        # Close all tubes created by THIS specific test
        for tube_id in list(self.my_tube_ids):  # Copy to avoid modification during iteration
            try:
                self.tube_registry.close_tube(tube_id)
                logging.debug(f"tearDown: Closed tube {tube_id}")
            except Exception as e:
                # Tube might already be closed in test - that's fine
                logging.debug(f"tearDown: Tube {tube_id} already closed: {e}")

        self.my_tube_ids.clear()
        logging.info(f"{self.__class__.__name__} tearDown: cleaned up tracked tubes")

    def configure_test_resource_limits(self, registry):
        """
        DEPRECATED: Configuration now done in setUp() for shared registry.
        This method kept for compatibility but does nothing.
        """
        # No-op - shared registry is already configured in setUp()
        pass

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
        candidates_exchanged = 0
        
        while time.time() - start_time < timeout:
            # Handle peer1's candidates
            try:
                while True:  # Process all available candidates
                    candidate = self.ice_candidates1.get_nowait()
                    try:
                        peer2.add_ice_candidate(candidate)
                        candidates_exchanged += 1
                        if candidate:  # Non-empty candidate
                            logging.info(f"Added ICE candidate to peer2 (total: {candidates_exchanged})")
                        else:  # Empty candidate = end-of-candidates
                            logging.info(f"Added end-of-candidates signal to peer2 (total: {candidates_exchanged})")
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
                        candidates_exchanged += 1
                        if candidate:  # Non-empty candidate
                            logging.info(f"Added ICE candidate to peer1 (total: {candidates_exchanged})")
                        else:  # Empty candidate = end-of-candidates
                            logging.info(f"Added end-of-candidates signal to peer1 (total: {candidates_exchanged})")
                    except Exception as e:
                        logging.error(f"Failed to add ICE candidate to peer1: {e}")
            except Empty:
                pass

            # Check if a connection is established
            if peer1.connection_state == "Connected" and peer2.connection_state == "Connected":
                logging.info(f"Connection established during ICE exchange after {candidates_exchanged} candidates")
                return True

            time.sleep(0.1)

        logging.warning(f"ICE exchange timed out after {candidates_exchanged} candidates exchanged")
        return False

class ExactEchoServer(threading.Thread):
    """Echo server that returns exact bytes received (no modification).

    Used for fragmentation testing where data integrity must be verified.
    """

    def __init__(self, host="127.0.0.1", port=0):
        super().__init__(daemon=True)
        self.host = host
        self.port = port
        self.server_socket = None
        self.actual_port = None
        self.running = False
        self._stop_event = threading.Event()

    def run(self):
        try:
            self.server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            self.server_socket.bind((self.host, self.port))
            self.actual_port = self.server_socket.getsockname()[1]
            self.server_socket.listen(1)
            logging.info(f"[ExactEchoServer] Listening on {self.host}:{self.actual_port}")
            self.running = True

            while not self._stop_event.is_set():
                self.server_socket.settimeout(0.1)
                try:
                    conn, addr = self.server_socket.accept()
                except socket.timeout:
                    continue

                logging.info(f"[ExactEchoServer] Accepted connection from {addr}")
                with conn:
                    while not self._stop_event.is_set():
                        try:
                            conn.settimeout(0.1)
                            data = conn.recv(65536)  # Large buffer for big payloads
                            if not data:
                                logging.info(f"[ExactEchoServer] Client {addr} disconnected.")
                                break
                            # Echo exact bytes back (no modification)
                            conn.sendall(data)
                        except socket.timeout:
                            continue
                        except ConnectionResetError:
                            logging.warning(f"[ExactEchoServer] Connection reset by {addr}")
                            break
                        except Exception as e:
                            logging.error(f"[ExactEchoServer] Error: {e}")
                            break
                if self._stop_event.is_set():
                    break
        except Exception as e:
            logging.error(f"[ExactEchoServer] Server error: {e}")
        finally:
            if self.server_socket:
                self.server_socket.close()
            self.running = False
            logging.info("[ExactEchoServer] Stopped.")

    def stop(self):
        logging.info("[ExactEchoServer] Stopping...")
        self._stop_event.set()
        self.join(timeout=2)
        if self.is_alive():
            logging.warning("[ExactEchoServer] Thread did not stop in time.")
            if self.server_socket:
                try:
                    self.server_socket.close()
                except Exception as e:
                    logging.error(f"[ExactEchoServer] Error force closing: {e}")


class AckServer(threading.Thread):
    def __init__(self, host="127.0.0.1", port=0):
        super().__init__(daemon=True)
        self.host = host
        self.port = port
        self.server_socket = None
        self.actual_port = None
        self.running = False
        self._stop_event = threading.Event()

    def run(self):
        try:
            self.server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            self.server_socket.bind((self.host, self.port))
            self.actual_port = self.server_socket.getsockname()[1]
            self.server_socket.listen(1)
            logging.info(f"[AckServer] Listening on {self.host}:{self.actual_port}")
            self.running = True

            while not self._stop_event.is_set():
                self.server_socket.settimeout(0.1) # Timeout to check stop_event
                try:
                    conn, addr = self.server_socket.accept()
                except socket.timeout:
                    continue

                logging.info(f"[AckServer] Accepted connection from {addr}")
                with conn:
                    while not self._stop_event.is_set():
                        try:
                            conn.settimeout(0.1) # Timeout to check stop_event
                            data = conn.recv(1024)
                            if not data:
                                logging.info(f"[AckServer] Client {addr} disconnected.")
                                break
                            logging.info(f"[AckServer] Received from {addr}: {data.decode(errors='ignore')}")
                            response = data + b" ack"
                            conn.sendall(response)
                            logging.info(f"[AckServer] Sent ack back to {addr}")
                        except socket.timeout:
                            continue
                        except ConnectionResetError:
                            logging.warning(f"[AckServer] Connection reset by {addr}")
                            break
                        except Exception as e:
                            logging.error(f"[AckServer] Error handling client {addr}: {e}")
                            break
                if self._stop_event.is_set():
                    break
        except Exception as e:
            logging.error(f"[AckServer] Server error: {e}")
        finally:
            if self.server_socket:
                self.server_socket.close()
            self.running = False
            logging.info("[AckServer] Stopped.")

    def stop(self):
        logging.info("[AckServer] Stopping...")
        self._stop_event.set()
        self.join(timeout=2) # Wait for thread to finish
        if self.is_alive():
            logging.warning("[AckServer] Thread did not stop in time.")
            if self.server_socket:
                 # Force close if still open, though this can be risky
                try:
                    self.server_socket.close()
                except Exception as e:
                    logging.error(f"[AckServer] Error force closing socket: {e}")


def run_ack_server_in_thread(host="127.0.0.1", port=0):
    server = AckServer(host, port)
    server.start()
    time.sleep(0.1) # Allow server thread to initialize
    timeout = 5  # seconds
    start_time = time.time()

    # Wait for the server to set its actual_port or for the thread to die, or timeout
    while not server.actual_port:
        if not server.is_alive():
            # Server thread died before setting the port
            raise RuntimeError(f"AckServer thread died prematurely. Running: {server.running}, Actual Port: {server.actual_port}")
        if time.time() - start_time > timeout:
            server.stop() # Attempt to clean up the lingering thread
            raise RuntimeError(f"AckServer timed out waiting for port. Still alive: {server.is_alive()}, Running: {server.running}, Actual Port: {server.actual_port}")
        time.sleep(0.01)

    # After the loop, double-check conditions if port was set but server might have issues
    if not server.running:
        # This case might occur if actual_port was set, but running flag was then set to False (e.g., immediate error after listen)
        if server.is_alive():
            server.stop()
        raise RuntimeError(f"AckServer started but is not in a running state. Still alive: {server.is_alive()}, Running: {server.running}, Actual Port: {server.actual_port}")

    # Check if port is reported (it should be if we exited the loop successfully)
    if server.actual_port is None:
        # This should ideally be caught by the timeout or not server.is_alive() above, but as a safeguard:
        if server.is_alive():
            server.stop()
        raise RuntimeError(f"AckServer failed to report an actual port. Still alive: {server.is_alive()}, Running: {server.running}, Actual Port: {server.actual_port}")

    logging.info(f"[run_ack_server_in_thread] AckServer started successfully on {server.host}:{server.actual_port}")
    return server

def init_logger():
    """Initialize the Rust logger to use Python's logging system"""
    global RUST_LOGGER_INITIALIZED
    if not RUST_LOGGER_INITIALIZED:
        try:
            keeper_pam_webrtc_rs.initialize_logger("keeper_pam_webrtc_rs", verbose=True, level=logging.DEBUG)
            logging.info("Rust logger initialized successfully")
            RUST_LOGGER_INITIALIZED = True
        except Exception as e:
            # Handle the already-initialized case gracefully
            error_msg = str(e).lower()
            if "already initialized" in error_msg or "logging system was already initialized" in error_msg:
                logging.debug(f"Rust logger already initialized: {e}")
                RUST_LOGGER_INITIALIZED = True
            else:
                # This is a real error - log but don't fail tests
                logging.error(f"Failed to initialize Rust logger: {e}")
                # Don't raise - let tests continue with existing logging setup 