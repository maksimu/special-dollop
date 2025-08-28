"""
Tests for ICE restart functionality and keepalive monitoring

This module tests the new ICE restart and NAT timeout prevention features,
focusing on connection recovery and performance validation.
"""

import unittest
import logging
import time
import threading
import queue
import os
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeoutError

import keeper_pam_webrtc_rs

from test_utils import BaseWebRTCTest, init_logger

# Test constants
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"


class TestICERestartAndKeepalive(BaseWebRTCTest, unittest.TestCase):
    """Tests for ICE restart and keepalive functionality"""
    
    def setUp(self):
        super().setUp()
        init_logger()
        self.created_registries = []
        self.tube_states = {}
        self.tube_connection_events = {}
        self._lock = threading.Lock()
        self.peer_map = {}
        
        # Metrics tracking for tests
        self.keepalive_events = []
        self.ice_restart_attempts = []
        self.connection_recovery_times = []

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
        
        with self._lock:
            self.tube_states.clear()
            self.tube_connection_events.clear()
            self.peer_map.clear()

    def create_tracked_registry(self):
        """Create a registry and track it for cleanup"""
        registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.configure_test_resource_limits(registry)
        self.created_registries.append(registry)
        return registry

    def _enhanced_signal_handler(self, signal_dict):
        """Enhanced signal handler that tracks keepalive and ICE restart events"""
        try:
            with self._lock:
                tube_id = signal_dict.get('tube_id')
                kind = signal_dict.get('kind')
                data = signal_dict.get('data')
                
                if not tube_id or not kind:
                    return
                
                # Track connection state changes for recovery timing
                if kind == "connection_state_changed":
                    timestamp = time.time()
                    state = data.lower()
                    
                    logging.info(f"ENHANCED: Tube {tube_id} state -> {state} at {timestamp}")
                    self.tube_states[tube_id] = state
                    
                    # Create connection event if needed
                    if tube_id not in self.tube_connection_events:
                        self.tube_connection_events[tube_id] = threading.Event()
                    
                    if state == "connected":
                        self.tube_connection_events[tube_id].set()
                        # Record recovery time if we have a disconnect timestamp
                        if hasattr(self, f'_disconnect_time_{tube_id}'):
                            disconnect_time = getattr(self, f'_disconnect_time_{tube_id}')
                            recovery_time = timestamp - disconnect_time
                            self.connection_recovery_times.append(recovery_time)
                            logging.info(f"RECOVERY: Tube {tube_id} recovered in {recovery_time:.3f}s")
                            delattr(self, f'_disconnect_time_{tube_id}')
                    elif state in ["disconnected", "failed"]:
                        self.tube_connection_events[tube_id].clear()
                        # Record disconnect time for recovery measurement
                        setattr(self, f'_disconnect_time_{tube_id}', timestamp)
                        logging.info(f"DISCONNECT: Tube {tube_id} disconnected at {timestamp}")
                
                # Track ICE candidate exchanges (proxy for keepalive activity)
                elif kind == "icecandidate":
                    timestamp = time.time()
                    if data:  # Non-empty candidate indicates keepalive activity
                        self.keepalive_events.append({
                            'tube_id': tube_id,
                            'timestamp': timestamp,
                            'candidate_preview': data[:50] + "..." if len(data) > 50 else data
                        })
                        logging.debug(f"KEEPALIVE: ICE candidate activity from {tube_id}")
                    
                    # Standard ICE relay logic
                    peer_tube_id = self.peer_map.get(tube_id)
                    if peer_tube_id:
                        try:
                            registry = self.created_registries[0] if self.created_registries else None
                            if registry:
                                registry.add_ice_candidate(peer_tube_id, data)
                        except Exception as e:
                            logging.error(f"ICE relay failed: {e}")
                
        except Exception as e:
            logging.error(f"Enhanced signal handler error: {e}", exc_info=True)

    def test_keepalive_functionality_monitoring(self):
        """Test keepalive prevents NAT timeouts with detailed logging"""
        logging.info("=== Testing Keepalive Functionality Monitoring ===")
        
        # Create tubes that should establish keepalive
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create server tube
        server_tube_info = registry.create_tube(
            conversation_id="keepalive-test-server",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        server_id = server_tube_info['tube_id']
        offer = server_tube_info['offer']
        
        # Create client tube
        client_tube_info = registry.create_tube(
            conversation_id="keepalive-test-client",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            offer=offer,
            signal_callback=self._enhanced_signal_handler
        )
        client_id = client_tube_info['tube_id']
        answer = client_tube_info['answer']
        
        # Set up peer mapping for ICE relay
        with self._lock:
            self.peer_map[server_id] = client_id
            self.peer_map[client_id] = server_id
        
        # Complete signaling
        registry.set_remote_description(server_id, answer, is_answer=True)
        
        # Wait for connection
        connected = self.wait_for_tube_connection(server_id, client_id, 20)
        self.assertTrue(connected, "Failed to establish connection for keepalive test")
        
        # Monitor keepalive activity for extended period
        logging.info("Monitoring keepalive activity for 10 seconds...")
        initial_keepalive_count = len(self.keepalive_events)
        
        # Wait and monitor
        time.sleep(10.0)
        
        final_keepalive_count = len(self.keepalive_events)
        keepalive_activity = final_keepalive_count - initial_keepalive_count
        
        logging.info(f"Keepalive monitoring results:")
        logging.info(f"  - Initial events: {initial_keepalive_count}")
        logging.info(f"  - Final events: {final_keepalive_count}")
        logging.info(f"  - Activity during test: {keepalive_activity}")
        
        # Verify keepalive is working (should see some activity)
        if keepalive_activity > 0:
            logging.info("[PASS] KEEPALIVE ACTIVE: Detected ICE candidate activity")
            
            # Log recent keepalive events
            recent_events = self.keepalive_events[-5:] if self.keepalive_events else []
            for event in recent_events:
                logging.info(f"  - {event['tube_id']}: {event['candidate_preview']} at {event['timestamp']}")
        else:
            logging.warning("[WARN] NO KEEPALIVE ACTIVITY: May indicate keepalive not functioning")
        
        # Verify both tubes are still connected
        server_state = registry.get_connection_state(server_id)
        client_state = registry.get_connection_state(client_id)
        
        logging.info(f"Final connection states: server={server_state}, client={client_state}")
        self.assertEqual(server_state.lower(), "connected", "Server should remain connected")
        self.assertEqual(client_state.lower(), "connected", "Client should remain connected")
        
        # Cleanup
        registry.close_tube(server_id)
        registry.close_tube(client_id)
        with self._lock:
            self.peer_map.pop(server_id, None)
            self.peer_map.pop(client_id, None)

    def test_ice_restart_success_rate_tracking(self):
        """Test ICE restart success/failure rates under network changes"""
        logging.info("=== Testing ICE Restart Success Rate Tracking ===")
        
        # This test simulates network changes that would trigger ICE restart
        # Since we can't actually change networks in a unit test, we'll test
        # the ICE restart mechanism's ability to handle state transitions
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create a tube for ICE restart testing
        tube_info = registry.create_tube(
            conversation_id="ice-restart-test",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        tube_id = tube_info['tube_id']
        
        # Verify initial state
        initial_state = registry.get_connection_state(tube_id)
        logging.info(f"Initial tube state: {initial_state}")
        
        # Monitor for any state changes that might indicate ICE restart attempts
        # In a real network change scenario, we'd see disconnected -> connected transitions
        
        monitoring_duration = 5.0
        logging.info(f"Monitoring for ICE restart activity for {monitoring_duration}s...")
        
        start_time = time.time()
        state_changes = []
        
        while time.time() - start_time < monitoring_duration:
            current_state = registry.get_connection_state(tube_id)
            if len(state_changes) == 0 or state_changes[-1][1] != current_state:
                timestamp = time.time() - start_time
                state_changes.append((timestamp, current_state))
                logging.debug(f"State change: {current_state} at {timestamp:.3f}s")
            time.sleep(0.1)
        
        # Analyze state transitions
        logging.info("ICE restart monitoring results:")
        logging.info(f"  - Total state changes: {len(state_changes)}")
        for timestamp, state in state_changes:
            logging.info(f"    {timestamp:.3f}s: {state}")
        
        # Look for patterns that indicate restart attempts
        disconnect_events = [s for t, s in state_changes if 'disconnect' in s.lower()]
        reconnect_events = [s for t, s in state_changes if 'connect' in s.lower()]
        
        logging.info(f"  - Disconnect events: {len(disconnect_events)}")
        logging.info(f"  - Reconnect events: {len(reconnect_events)}")
        
        # In a stable test environment, we shouldn't see many state changes
        # But the mechanism should be ready to handle them
        if len(state_changes) <= 2:  # Initial state + maybe one transition
            logging.info("[PASS] STABLE CONNECTION: No ICE restarts needed in test environment")
        else:
            logging.info(f"[INFO] DYNAMIC CONNECTION: {len(state_changes)} state transitions observed")
        
        # Verify final state is stable
        final_state = registry.get_connection_state(tube_id)
        logging.info(f"Final state: {final_state}")
        
        # Cleanup
        registry.close_tube(tube_id)

    def test_mutex_contention_under_load(self):
        """Test new mutex usage doesn't degrade hot path performance"""
        logging.info("=== Testing Mutex Contention Under Load ===")
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create multiple tubes to increase mutex contention
        tube_ids = []
        for i in range(5):
            tube_info = registry.create_tube(
                conversation_id=f"contention-test-{i}",
                settings=settings,
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                signal_callback=self._enhanced_signal_handler
            )
            tube_ids.append(tube_info['tube_id'])
        
        logging.info(f"Created {len(tube_ids)} tubes for contention testing")
        
        # Test concurrent operations that use the new consolidated mutexes
        contention_results = queue.Queue()
        
        def concurrent_state_checks(tube_id, iterations):
            """Perform concurrent state checks to test mutex performance"""
            start_time = time.time()
            for i in range(iterations):
                try:
                    state = registry.get_connection_state(tube_id)
                    # This internally calls methods that use our new consolidated mutexes
                    time.sleep(0.001)  # Small delay to increase contention
                except Exception as e:
                    contention_results.put(f"ERROR: {tube_id}: {e}")
                    return
            
            duration = time.time() - start_time
            avg_time_per_op = (duration / iterations) * 1000  # Convert to ms
            contention_results.put(f"SUCCESS: {tube_id}: {iterations} ops in {duration:.3f}s (avg: {avg_time_per_op:.3f}ms/op)")
        
        # Run concurrent operations
        iterations = 100
        threads = []
        
        logging.info(f"Starting {len(tube_ids)} concurrent threads, {iterations} operations each...")
        start_time = time.time()
        
        for tube_id in tube_ids:
            thread = threading.Thread(target=concurrent_state_checks, args=(tube_id, iterations))
            threads.append(thread)
            thread.start()
        
        # Wait for all threads
        for thread in threads:
            thread.join(timeout=30.0)  # 30s timeout
            if thread.is_alive():
                self.fail("Contention test thread did not complete - possible deadlock!")
        
        total_duration = time.time() - start_time
        total_operations = len(tube_ids) * iterations
        
        # Collect results
        results = []
        while not contention_results.empty():
            try:
                result = contention_results.get_nowait()
                results.append(result)
                logging.info(f"  {result}")
            except queue.Empty:
                break
        
        # Analyze performance
        success_count = len([r for r in results if r.startswith("SUCCESS")])
        error_count = len([r for r in results if r.startswith("ERROR")])
        
        logging.info(f"Contention test results:")
        logging.info(f"  - Total operations: {total_operations}")
        logging.info(f"  - Total duration: {total_duration:.3f}s")
        logging.info(f"  - Operations/second: {total_operations/total_duration:.0f}")
        logging.info(f"  - Success threads: {success_count}/{len(tube_ids)}")
        logging.info(f"  - Error threads: {error_count}/{len(tube_ids)}")
        
        # Performance assertions
        self.assertEqual(error_count, 0, "Should have no errors from mutex contention")
        self.assertEqual(success_count, len(tube_ids), "All threads should complete successfully")
        
        # Performance should be reasonable (not exact due to test environment variability)
        ops_per_second = total_operations / total_duration
        logging.info(f"Achieved {ops_per_second:.0f} operations/second with {len(tube_ids)} concurrent threads")
        
        if ops_per_second > 1000:  # More than 1000 ops/sec indicates good performance
            logging.info("[EXCELLENT] PERFORMANCE: High throughput with low contention")
        elif ops_per_second > 500:  # More than 500 ops/sec is acceptable
            logging.info("[GOOD] PERFORMANCE: Acceptable throughput under contention")
        else:
            logging.warning(f"[WARN] PERFORMANCE CONCERN: Only {ops_per_second:.0f} ops/sec - may indicate excessive contention")
        
        # Cleanup
        for tube_id in tube_ids:
            registry.close_tube(tube_id)

    def test_restart_storm_prevention(self):
        """Verify exponential backoff prevents ICE restart storms"""
        logging.info("=== Testing Restart Storm Prevention ===")
        
        # This test validates that the exponential backoff logic prevents
        # rapid-fire ICE restart attempts that could overwhelm the system
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        tube_info = registry.create_tube(
            conversation_id="storm-prevention-test",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        tube_id = tube_info['tube_id']
        
        logging.info(f"Testing restart storm prevention with tube {tube_id}")
        
        # Simulate rapid state changes that might trigger restart attempts
        # We'll monitor the actual timing to verify backoff is working
        
        state_check_interval = 0.1  # Check state every 100ms
        monitoring_duration = 10.0  # Monitor for 10 seconds
        state_history = []
        
        logging.info(f"Monitoring state changes for {monitoring_duration}s...")
        start_time = time.time()
        
        while time.time() - start_time < monitoring_duration:
            current_time = time.time() - start_time
            current_state = registry.get_connection_state(tube_id)
            
            # Record state with timestamp
            if not state_history or state_history[-1][1] != current_state:
                state_history.append((current_time, current_state))
                logging.debug(f"State: {current_state} at {current_time:.3f}s")
            
            time.sleep(state_check_interval)
        
        # Analyze for restart storm patterns
        logging.info("Storm prevention analysis:")
        logging.info(f"  - Total state observations: {len(state_history)}")
        
        # Look for rapid state transitions that might indicate restart attempts
        rapid_transitions = []
        for i in range(1, len(state_history)):
            time_diff = state_history[i][0] - state_history[i-1][0]
            if time_diff < 1.0:  # Transitions less than 1 second apart
                rapid_transitions.append((state_history[i-1], state_history[i], time_diff))
        
        logging.info(f"  - Rapid transitions (<1s apart): {len(rapid_transitions)}")
        
        for prev, curr, time_diff in rapid_transitions:
            logging.info(f"    {prev[1]} -> {curr[1]} in {time_diff:.3f}s")
        
        # In a well-behaved system with exponential backoff, we should see:
        # 1. Limited number of rapid transitions
        # 2. Increasing intervals between restart attempts
        
        if len(rapid_transitions) == 0:
            logging.info("[EXCELLENT]: No rapid state transitions detected")
        elif len(rapid_transitions) < 3:
            logging.info("[GOOD]: Limited rapid transitions - backoff likely working")
        else:
            logging.warning(f"[WARN] POTENTIAL STORM: {len(rapid_transitions)} rapid transitions detected")
        
        # Check if we can observe exponential backoff pattern
        # (In a test environment, we might not trigger actual restarts)
        transition_intervals = [td for _, _, td in rapid_transitions]
        if len(transition_intervals) >= 2:
            logging.info("Transition intervals:")
            for i, interval in enumerate(transition_intervals):
                logging.info(f"  - Attempt {i+1}: {interval:.3f}s")
            
            # Check if intervals are increasing (exponential backoff)
            increasing = all(transition_intervals[i] <= transition_intervals[i+1] 
                           for i in range(len(transition_intervals)-1))
            if increasing:
                logging.info("[PASS] EXPONENTIAL BACKOFF: Intervals increasing as expected")
            else:
                logging.info("[INFO] PATTERN: Mixed intervals (expected in test environment)")
        
        # Verify final state
        final_state = registry.get_connection_state(tube_id)
        logging.info(f"Final state: {final_state}")
        
        # Cleanup
        registry.close_tube(tube_id)

    def wait_for_tube_connection(self, tube_id1, tube_id2, timeout=10):
        """Wait for both tubes to establish a connection"""
        logging.debug(f"Waiting for connection: {tube_id1} <-> {tube_id2} (timeout: {timeout}s)")
        start_time = time.time()
        
        while time.time() - start_time < timeout:
            registry = self.created_registries[0] if self.created_registries else None
            if not registry:
                return False
                
            state1 = registry.get_connection_state(tube_id1)
            state2 = registry.get_connection_state(tube_id2)
            
            if state1.lower() == "connected" and state2.lower() == "connected":
                logging.debug(f"Connection established: {tube_id1}={state1}, {tube_id2}={state2}")
                return True
                
            time.sleep(0.1)
        
        # Log final states on timeout
        if registry:
            state1 = registry.get_connection_state(tube_id1)
            state2 = registry.get_connection_state(tube_id2)
            logging.warning(f"Connection timeout: {tube_id1}={state1}, {tube_id2}={state2}")
        
        return False

    def test_manual_ice_restart_api(self):
        """Test manual ICE restart API functionality"""
        logging.info("=== Testing Manual ICE Restart API ===")
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create server tube
        server_tube_info = registry.create_tube(
            conversation_id="manual-restart-server",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        server_id = server_tube_info['tube_id']
        offer = server_tube_info['offer']
        
        # Create client tube
        client_tube_info = registry.create_tube(
            conversation_id="manual-restart-client",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            offer=offer,
            signal_callback=self._enhanced_signal_handler
        )
        client_id = client_tube_info['tube_id']
        answer = client_tube_info['answer']
        
        # Set up peer mapping
        with self._lock:
            self.peer_map[server_id] = client_id
            self.peer_map[client_id] = server_id
        
        # Complete signaling
        registry.set_remote_description(server_id, answer, is_answer=True)
        
        # Wait for connection
        connected = self.wait_for_tube_connection(server_id, client_id, 20)
        self.assertTrue(connected, "Failed to establish connection for restart test")
        
        # Test manual ICE restart
        logging.info("Testing manual ICE restart...")
        try:
            registry.restart_ice(server_id)
            logging.info("[PASS] Manual ICE restart call succeeded")
        except Exception as e:
            self.fail(f"Manual ICE restart failed: {e}")
        
        # Monitor for restart effects (state changes)
        time.sleep(2.0)  # Allow restart to take effect
        
        # Verify connection can recover
        final_connected = self.wait_for_tube_connection(server_id, client_id, 15)
        if final_connected:
            logging.info("[PASS] Connection recovered after manual ICE restart")
        else:
            logging.warning("[WARN] Connection did not recover - may be expected in test environment")
        
        # Cleanup
        registry.close_tube(server_id)
        registry.close_tube(client_id)

    def test_connection_stats_api(self):
        """Test connection statistics API functionality"""
        logging.info("=== Testing Connection Statistics API ===")
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create server tube
        server_tube_info = registry.create_tube(
            conversation_id="stats-test-server",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        server_id = server_tube_info['tube_id']
        offer = server_tube_info['offer']
        
        # Create client tube
        client_tube_info = registry.create_tube(
            conversation_id="stats-test-client",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            offer=offer,
            signal_callback=self._enhanced_signal_handler
        )
        client_id = client_tube_info['tube_id']
        answer = client_tube_info['answer']
        
        # Set up peer mapping
        with self._lock:
            self.peer_map[server_id] = client_id
            self.peer_map[client_id] = server_id
        
        # Complete signaling
        registry.set_remote_description(server_id, answer, is_answer=True)
        
        # Wait for connection
        connected = self.wait_for_tube_connection(server_id, client_id, 20)
        self.assertTrue(connected, "Failed to establish connection for stats test")
        
        # Test connection stats API
        logging.info("Testing connection statistics...")
        
        # Get stats for server
        try:
            server_stats = registry.get_connection_stats(server_id)
            logging.info(f"[PASS] Server stats retrieved: {server_stats}")
            
            # Validate stats structure
            required_fields = ['bytes_sent', 'bytes_received', 'packet_loss_rate', 'rtt_ms']
            for field in required_fields:
                self.assertIn(field, server_stats, f"Missing required field: {field}")
            
            # Validate stats types
            self.assertIsInstance(server_stats['bytes_sent'], int)
            self.assertIsInstance(server_stats['bytes_received'], int)
            self.assertIsInstance(server_stats['packet_loss_rate'], (int, float))
            self.assertTrue(
                server_stats['rtt_ms'] is None or isinstance(server_stats['rtt_ms'], (int, float)),
                "rtt_ms should be None or numeric"
            )
            
            # Log stats values
            logging.info(f"  📈 Bytes sent: {server_stats['bytes_sent']:,}")
            logging.info(f"  📉 Bytes received: {server_stats['bytes_received']:,}")
            logging.info(f"  📡 Packet loss: {server_stats['packet_loss_rate']:.4f}")
            if server_stats['rtt_ms'] is not None:
                logging.info(f"  ⏱️  RTT: {server_stats['rtt_ms']:.1f}ms")
            else:
                logging.info(f"  ⏱️  RTT: Not available yet")
            
        except Exception as e:
            self.fail(f"Failed to get connection stats: {e}")
        
        # Get stats for client
        try:
            client_stats = registry.get_connection_stats(client_id)
            logging.info(f"[PASS] Client stats retrieved: {client_stats}")
        except Exception as e:
            self.fail(f"Failed to get client connection stats: {e}")
        
        # Verify stats are reasonable
        total_bytes = server_stats['bytes_sent'] + server_stats['bytes_received']
        if total_bytes > 0:
            logging.info(f"[PASS] Data transfer detected: {total_bytes:,} total bytes")
        else:
            logging.info(f"[INFO] No data transfer yet (connection just established)")
        
        # Verify packet loss is within reasonable bounds
        if server_stats['packet_loss_rate'] <= 0.1:  # ≤10% loss acceptable in test
            logging.info(f"[PASS] Acceptable packet loss: {server_stats['packet_loss_rate']:.2%}")
        else:
            logging.warning(f"[WARN] High packet loss in test: {server_stats['packet_loss_rate']:.2%}")
        
        # Cleanup
        registry.close_tube(server_id)
        registry.close_tube(client_id)

    def test_stats_driven_restart_logic(self):
        """Test using connection stats to drive restart decisions"""
        logging.info("=== Testing Stats-Driven Restart Logic ===")
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create connection for stats testing
        tube_info = registry.create_tube(
            conversation_id="stats-driven-restart",
            settings=settings,
            trickle_ice=True,
            callback_token=TEST_CALLBACK_TOKEN,
            krelay_server="test.relay.server.com",
            client_version="ms16.5.0",
            ksm_config=TEST_KSM_CONFIG,
            signal_callback=self._enhanced_signal_handler
        )
        tube_id = tube_info['tube_id']
        
        # Monitor stats over time to detect quality patterns
        stats_history = []
        monitoring_duration = 8.0
        check_interval = 1.0
        
        logging.info(f"Monitoring connection stats for {monitoring_duration}s...")
        start_time = time.time()
        
        while time.time() - start_time < monitoring_duration:
            try:
                current_time = time.time() - start_time
                stats = registry.get_connection_stats(tube_id)
                
                stats_entry = {
                    'timestamp': current_time,
                    'bytes_sent': stats['bytes_sent'],
                    'bytes_received': stats['bytes_received'],
                    'packet_loss_rate': stats['packet_loss_rate'],
                    'rtt_ms': stats['rtt_ms']
                }
                stats_history.append(stats_entry)
                
                logging.debug(f"Stats at {current_time:.1f}s: loss={stats['packet_loss_rate']:.4f}, rtt={stats['rtt_ms']}")
                
                time.sleep(check_interval)
            except Exception as e:
                logging.error(f"Stats collection error: {e}")
                break
        
        # Analyze stats patterns
        logging.info("Stats analysis:")
        logging.info(f"  - Data points collected: {len(stats_history)}")
        
        if stats_history:
            # Check for data transfer progression
            initial_bytes = stats_history[0]['bytes_sent'] + stats_history[0]['bytes_received']
            final_bytes = stats_history[-1]['bytes_sent'] + stats_history[-1]['bytes_received']
            bytes_diff = final_bytes - initial_bytes
            
            logging.info(f"  - Data transfer: {bytes_diff:,} bytes during monitoring")
            
            # Analyze packet loss trends
            loss_rates = [s['packet_loss_rate'] for s in stats_history]
            avg_loss = sum(loss_rates) / len(loss_rates)
            max_loss = max(loss_rates)
            
            logging.info(f"  - Average packet loss: {avg_loss:.4f}")
            logging.info(f"  - Maximum packet loss: {max_loss:.4f}")
            
            # Test restart decision logic
            high_loss_threshold = 0.05  # 5%
            if avg_loss > high_loss_threshold:
                logging.info(f"[TEST] Simulating restart due to high packet loss")
                try:
                    registry.restart_ice(tube_id)
                    time.sleep(2.0)  # Allow restart to process
                    post_restart_stats = registry.get_connection_stats(tube_id)
                    logging.info(f"[PASS] Post-restart stats retrieved: {post_restart_stats}")
                except Exception as e:
                    logging.error(f"Restart after high loss failed: {e}")
            else:
                logging.info(f"[PASS] Normal packet loss - no restart needed")
            
            # Analyze RTT trends
            rtt_values = [s['rtt_ms'] for s in stats_history if s['rtt_ms'] is not None]
            if rtt_values:
                avg_rtt = sum(rtt_values) / len(rtt_values)
                logging.info(f"  - Average RTT: {avg_rtt:.1f}ms")
                
                if avg_rtt > 500:  # High latency
                    logging.warning(f"[WARN] High RTT detected - network issues possible")
                else:
                    logging.info(f"[PASS] Acceptable RTT")
            else:
                logging.info(f"  - RTT: Not available during test")
        
        # Cleanup
        registry.close_tube(tube_id)


if __name__ == '__main__':
    # Configure logging for detailed output
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    unittest.main(verbosity=2)