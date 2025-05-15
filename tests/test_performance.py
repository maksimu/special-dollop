import unittest
import logging
import time

import keeper_pam_webrtc_rs

from test_utils import with_runtime, BaseWebRTCTest

# Special test mode values that might be recognized by the Rust code
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"

class TestWebRTCPerformance(BaseWebRTCTest, unittest.TestCase):
    """Performance tests for WebRTC data channels"""
    
    def setUp(self):
        super().setUp()
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
    
    @with_runtime
    def test_data_channel_load(self):
        """Test basic tube creation and connection performance"""
        logging.info("Starting tube creation and connection test")

        # Create tubes for testing
        settings = {}
        
        # Create server tube
        server_tube = self.tube_registry.create_tube(
            conversation_id="performance-test-server",
            relay_server="test-relay.example.com",
            ksm_config=TEST_KSM_CONFIG,
            protocol_type="port_forward",
            settings=settings,
            trickle_ice=True,
            turn_only=False,
            callback_token=TEST_CALLBACK_TOKEN,
            use_turn=False
        )
        
        # Get the offer from server
        offer = server_tube.sdp
        self.assertIsNotNone(offer, "Server should generate an offer")
        
        # Create client tube with the offer
        client_tube = self.tube_registry.create_tube(
            conversation_id="performance-test-client",
            relay_server="test-relay.example.com",
            ksm_config=TEST_KSM_CONFIG,
            protocol_type="port_forward",
            settings=settings,
            trickle_ice=True,
            turn_only=False,
            callback_token=TEST_CALLBACK_TOKEN,
            offer=offer,
            use_turn=False
        )
        
        # Get the answer from client
        answer = client_tube.sdp
        self.assertIsNotNone(answer, "Client should generate an answer")
        
        # Set the answer on the server
        self.tube_registry.set_remote_description(server_tube.id, answer)
        
        # Wait for connection establishment
        server_id = server_tube.id
        client_id = client_tube.id
        
        start_time = time.time()
        connected = self.wait_for_tube_connection(server_id, client_id, 10)
        connection_time = time.time() - start_time
        
        self.assertTrue(connected, "Failed to establish connection")
        logging.info(f"Connection established in {connection_time:.2f} seconds")
        
        # Create a channel for data exchange
        channel_name = "performance-test-channel"
        channel_settings = {}
        
        self.tube_registry.create_channel(
            server_id,
            channel_name,
            "port_forward",
            channel_settings
        )
        
        # Clean up
        self.tube_registry.close_connection(server_id, channel_name)
        self.tube_registry.close_tube(server_id)
        self.tube_registry.close_tube(client_id)
        
        logging.info("Tube creation and connection test completed")
    
    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to establish a connection"""
        logging.info(f"Waiting for tube connection establishment (timeout: {timeout}s)")
        start_time = time.time()
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info("Connection established")
                return True
            time.sleep(0.1)
        logging.warning("Connection establishment timed out")
        return False

class TestWebRTCFragmentation(BaseWebRTCTest, unittest.TestCase):
    """Tests for tube connection with different settings"""
    
    def setUp(self):
        super().setUp() 
        self.tube_registry = keeper_pam_webrtc_rs.PyTubeRegistry()
    
    @with_runtime
    def test_data_channel_fragmentation(self):
        """Test basic tube connection with non-trickle ICE to evaluate connection reliability"""
        logging.info("Starting tube connection reliability test")

        # Create tubes with non-trickle ICE
        settings = {}
        
        # Create a server tube with non-trickle ICE
        server_tube = self.tube_registry.create_tube(
            conversation_id="fragmentation-test-server",
            relay_server="test-relay.example.com",
            ksm_config=TEST_KSM_CONFIG,
            protocol_type="port_forward",
            settings=settings,
            trickle_ice=False,  # Use non-trickle ICE
            turn_only=False,
            callback_token=TEST_CALLBACK_TOKEN,
            use_turn=False
        )
        
        # Get the offer from server
        offer = server_tube.sdp
        self.assertIsNotNone(offer, "Server should generate an offer")
        
        # Create client tube with the offer
        client_tube = self.tube_registry.create_tube(
            conversation_id="fragmentation-test-client",
            relay_server="test-relay.example.com",
            ksm_config=TEST_KSM_CONFIG,
            protocol_type="port_forward",
            settings=settings,
            trickle_ice=False,  # Use non-trickle ICE
            turn_only=False,
            callback_token=TEST_CALLBACK_TOKEN,
            offer=offer,
            use_turn=False
        )
        
        # Get the answer from client
        answer = client_tube.sdp
        self.assertIsNotNone(answer, "Client should generate an answer")
        
        # Set the answer on the server
        self.tube_registry.set_remote_description(server_tube.id, answer)
        
        # Wait for a connection establishment
        server_id = server_tube.id
        client_id = client_tube.id
        
        # Measure connection establishment time
        start_time = time.time()
        connected = self.wait_for_tube_connection(server_id, client_id, 15)
        connection_time = time.time() - start_time
        
        self.assertTrue(connected, "Failed to establish connection")
        logging.info(f"Non-trickle ICE connection established in {connection_time:.2f} seconds")
        
        # Clean up
        self.tube_registry.close_tube(server_id)
        self.tube_registry.close_tube(client_id)
        
        logging.info("Tube connection reliability test completed")
    
    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to establish a connection"""
        logging.info(f"Waiting for tube connection establishment (timeout: {timeout}s)")
        start_time = time.time()
        while time.time() - start_time < timeout:
            state1 = self.tube_registry.get_connection_state(tube_id1)
            state2 = self.tube_registry.get_connection_state(tube_id2)
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.info("Connection established")
                return True
            time.sleep(0.1)
        logging.warning("Connection establishment timed out")
        return False

if __name__ == '__main__':
    unittest.main() 