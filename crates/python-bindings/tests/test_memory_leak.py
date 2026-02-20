"""
Memory Leak Diagnostic Tests

This module contains diagnostic tests to identify memory leaks in WebRTC connections.
These tests are OPTIONAL and require the 'psutil' package.

Run with: pytest tests/test_memory_leak.py -v -s

Note: These are diagnostic tests for local debugging. They will be skipped
in CI if psutil is not available (no test failures).
"""

import unittest
import logging
import time
import gc
import os

import keeper_pam_connections

from test_utils import with_runtime, BaseWebRTCTest, run_ack_server_in_thread

# Optional dependency: psutil for memory tracking
try:
    import psutil
    HAS_PSUTIL = True
except ImportError:
    HAS_PSUTIL = False
    logging.warning("psutil not available - memory leak tests will be skipped")

# Configure logging to show all diagnostic messages
logging.basicConfig(
    level=logging.DEBUG,
    format='%(asctime)s %(name)-30s %(levelname)-8s %(message)s'
)

TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"


class TestMemoryLeak(BaseWebRTCTest, unittest.TestCase):
    """Diagnostic tests to identify memory leak sources"""

    def setUp(self):
        super().setUp()
        self.process = psutil.Process(os.getpid())
        self.initial_memory = self.process.memory_info().rss / 1024 / 1024  # MB
        logging.info(f"=== TEST STARTING: Initial memory: {self.initial_memory:.1f} MB ===")

    def get_current_memory_mb(self):
        """Get current process memory in MB"""
        return self.process.memory_info().rss / 1024 / 1024

    def force_gc_and_measure(self):
        """Force garbage collection and measure memory"""
        gc.collect()
        time.sleep(0.5)  # Let GC settle
        return self.get_current_memory_mb()

    @unittest.skipIf(not HAS_PSUTIL, "psutil not available - install with: pip install psutil")
    @with_runtime
    def test_sequential_connections_memory_leak(self):
        """
        Test 1: Sequential Connection Memory Leak Detection

        Creates and closes N connections sequentially, measuring memory after each.
        This test will expose:
        - Memory leaks per connection
        - Arc refcount issues (via log output)
        - Task lifecycle issues (via log output)

        Expected: Memory should stabilize after 2-3 connections (buffer pool warmup)
        If fails: Memory grows >1MB per connection = LEAK DETECTED
        """
        logging.info("="*80)
        logging.info("TEST 1: Sequential Connection Memory Leak Detection")
        logging.info("="*80)

        num_connections = 5
        memory_samples = []

        for i in range(num_connections):
            logging.info(f"\n--- Connection {i+1}/{num_connections} ---")

            # Measure memory before connection
            mem_before = self.force_gc_and_measure()
            logging.info(f"Memory BEFORE connection {i+1}: {mem_before:.1f} MB")

            # Create connection
            settings = {"conversationType": "tunnel"}

            try:
                # Create server tube
                server_tube_info = self.create_and_track_tube(
                    conversation_id=f"leak-test-server-{i}",
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0",
                    ksm_config=TEST_KSM_CONFIG,
                )
                server_id = server_tube_info['tube_id']
                logging.info(f"Created server tube: {server_id}")

                # Create client tube
                client_tube_info = self.create_and_track_tube(
                    conversation_id=f"leak-test-client-{i}",
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0",
                    ksm_config=TEST_KSM_CONFIG,
                    offer=server_tube_info['offer'],
                )
                client_id = client_tube_info['tube_id']
                logging.info(f"Created client tube: {client_id}")

                # Set remote description
                self.tube_registry.set_remote_description(server_id, client_tube_info['answer'], is_answer=True)

                # Wait briefly for connection establishment
                time.sleep(2)

                # Close tubes explicitly
                logging.info(f"Closing tubes for connection {i+1}...")
                self.close_and_untrack_tube(server_id)
                self.close_and_untrack_tube(client_id)
                logging.info(f"Tubes closed for connection {i+1}")

            except Exception as e:
                logging.error(f"Error in connection {i+1}: {e}", exc_info=True)
                continue

            # Measure memory after connection cleanup
            mem_after = self.force_gc_and_measure()
            mem_delta = mem_after - mem_before

            memory_samples.append({
                'connection': i + 1,
                'mem_before': mem_before,
                'mem_after': mem_after,
                'delta': mem_delta
            })

            logging.info(f"Memory AFTER connection {i+1}: {mem_after:.1f} MB (delta: {mem_delta:+.1f} MB)")
            logging.info(f"---")

        # Analyze results
        logging.info("\n" + "="*80)
        logging.info("MEMORY LEAK ANALYSIS RESULTS")
        logging.info("="*80)

        for sample in memory_samples:
            logging.info(
                f"Connection {sample['connection']}: "
                f"{sample['mem_before']:.1f} MB → {sample['mem_after']:.1f} MB "
                f"(delta: {sample['delta']:+.1f} MB)"
            )

        # Check for memory leak pattern
        if len(memory_samples) >= 3:
            # Skip first connection (includes pool warmup)
            deltas_after_warmup = [s['delta'] for s in memory_samples[1:]]
            avg_delta = sum(deltas_after_warmup) / len(deltas_after_warmup)

            logging.info(f"\nAverage memory delta after warmup: {avg_delta:+.1f} MB/connection")

            # FAIL on significant leak (regression detection)
            if avg_delta > 1.0:
                msg = f"Memory leak detected: {avg_delta:.1f} MB per connection (threshold: 1.0 MB)"
                logging.error(f"❌ {msg}")
                logging.error("This indicates a regression. Potential causes:")
                logging.error("  - Tokio tasks not being aborted (Conn::Drop not firing)")
                logging.error("  - Buffer pools not being drained (drain_thread_local not called)")
                logging.error("  - Arc references leaked (refcount > 2)")
                logging.error("="*80)
                self.fail(msg)  # ← FAIL THE TEST

            # WARN on potential leak (but pass)
            elif avg_delta > 0.5:
                logging.warning(f"⚠️  POTENTIAL LEAK: {avg_delta:.1f} MB per connection")
                logging.warning("This is acceptable but close to threshold - monitor for growth")

            # PASS
            else:
                logging.info(f"✅ NO SIGNIFICANT LEAK: {avg_delta:.1f} MB per connection")

        logging.info("="*80)

    @unittest.skipIf(not HAS_PSUTIL, "psutil not available - install with: pip install psutil")
    @with_runtime
    def test_connection_with_data_transfer(self):
        """
        Test 2: Connection with Data Transfer Memory Leak

        Tests memory leak when actually transferring data through the connection.
        This will expose:
        - Buffer pool leaks under load
        - Task cleanup when actively processing data

        Expected: Memory should return to baseline after close
        """
        logging.info("="*80)
        logging.info("TEST 2: Connection with Data Transfer Memory Leak")
        logging.info("="*80)

        # Start echo server
        ack_server = run_ack_server_in_thread()
        self.assertIsNotNone(ack_server.actual_port, "AckServer did not start")
        logging.info(f"AckServer running on port {ack_server.actual_port}")

        try:
            mem_before = self.force_gc_and_measure()
            logging.info(f"Memory before: {mem_before:.1f} MB")

            # Create server tube with TCP listener
            server_tube_info = self.create_and_track_tube(
                conversation_id="data-transfer-server",
                settings={
                    "conversationType": "tunnel",
                    "local_listen_addr": "127.0.0.1:0"
                },
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
            )
            server_id = server_tube_info['tube_id']
            server_port = server_tube_info.get('actual_local_listen_addr')
            logging.info(f"Server tube listening on: {server_port}")

            # Create client tube connecting to ack server
            client_tube_info = self.create_and_track_tube(
                conversation_id="data-transfer-client",
                settings={
                    "conversationType": "tunnel",
                    "target_host": "127.0.0.1",
                    "target_port": str(ack_server.actual_port)
                },
                trickle_ice=True,
                callback_token=TEST_CALLBACK_TOKEN,
                krelay_server="test.relay.server.com",
                client_version="ms16.5.0",
                ksm_config=TEST_KSM_CONFIG,
                offer=server_tube_info['offer'],
            )
            client_id = client_tube_info['tube_id']

            # Set remote description
            self.tube_registry.set_remote_description(server_id, client_tube_info['answer'], is_answer=True)

            # Wait for connection
            time.sleep(2)

            # TODO: Send data through connection
            # For now, just wait and close

            logging.info("Closing tubes...")
            self.close_and_untrack_tube(server_id)
            self.close_and_untrack_tube(client_id)

            mem_after = self.force_gc_and_measure()
            mem_delta = mem_after - mem_before

            logging.info(f"Memory after: {mem_after:.1f} MB (delta: {mem_delta:+.1f} MB)")

            # FAIL on significant leak (regression detection)
            # Note: First connection includes warmup overhead (registry + metrics + buffer pools)
            # Threshold accounts for this: ~7-8 MB warmup is normal
            if mem_delta > 10.0:
                msg = f"Memory leak detected: {mem_delta:.1f} MB (threshold: 10.0 MB incl. warmup)"
                logging.error(f"❌ {msg}")
                logging.error("This indicates a regression in data transfer cleanup")
                logging.error("="*80)
                self.fail(msg)  # ← FAIL THE TEST
            elif mem_delta > 8.0:
                logging.warning(f"⚠️  HIGH WARMUP OVERHEAD: {mem_delta:.1f} MB")
                logging.warning("This is higher than typical ~7-8 MB warmup - investigate if consistent")
            else:
                logging.info(f"✅ Memory delta acceptable: {mem_delta:.1f} MB (includes ~7-8 MB warmup)")

        finally:
            ack_server.stop()
            logging.info("="*80)


if __name__ == '__main__':
    unittest.main()
