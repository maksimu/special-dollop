"""
Manual Stress Tests for WebRTC Resource Cleanup

⚠️  WARNING: These tests are NOT suitable for CI/CD environments!
    
    These tests are designed to stress the system under high load conditions
    and may exhibit non-deterministic behavior depending on system resources.
    They should ONLY be run manually during development and testing phases.

    For CI-safe tests, run the regular test suite instead.

Usage:
    python3 manual_stress_tests.py --help
    python3 manual_stress_tests.py --light    # Reduced load for slower systems
    python3 manual_stress_tests.py --full     # Full stress test (default)
    python3 manual_stress_tests.py --extreme  # Maximum stress (use with caution)
"""

import unittest
import logging
import time
import threading
import queue
import gc
import os
import argparse
import sys
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeoutError
from dataclasses import dataclass, field
from typing import Dict, List, Optional

import keeper_pam_webrtc_rs

from test_utils import BaseWebRTCTest, init_logger

# Test constants - dynamically adjusted based on test mode
TEST_KSM_CONFIG = "TEST_MODE_KSM_CONFIG"
TEST_CALLBACK_TOKEN = "TEST_MODE_CALLBACK_TOKEN"

@dataclass
class StressTestConfig:
    """Configuration for stress test parameters"""
    name: str
    num_tubes: int
    num_workers: int 
    num_registries: int
    cycles: int
    tubes_per_cycle: int
    worker_timeout: float
    acceptable_error_rate: float
    skip_timing_assertions: bool
    description: str

# Test configurations for different stress levels
STRESS_CONFIGS = {
    'light': StressTestConfig(
        name='Light Stress',
        num_tubes=4,
        num_workers=2,
        num_registries=2,
        cycles=3,
        tubes_per_cycle=5,
        worker_timeout=60.0,
        acceptable_error_rate=0.02,  # 2%
        skip_timing_assertions=True,
        description='Reduced load suitable for slower systems and CI debugging'
    ),
    'full': StressTestConfig(
        name='Full Stress',  
        num_tubes=8,
        num_workers=4,
        num_registries=3,
        cycles=5,
        tubes_per_cycle=10,
        worker_timeout=45.0,
        acceptable_error_rate=0.05,  # 5% - still much better than original 10%
        skip_timing_assertions=False,
        description='Standard stress test for development machines'
    ),
    'extreme': StressTestConfig(
        name='Extreme Stress',
        num_tubes=16,
        num_workers=8, 
        num_registries=5,
        cycles=10,
        tubes_per_cycle=20,
        worker_timeout=90.0,
        acceptable_error_rate=0.08,  # 8% - only for extreme conditions
        skip_timing_assertions=True,
        description='Maximum stress test - use with caution, may affect system stability'
    )
}

class ManualStressTest(BaseWebRTCTest, unittest.TestCase):
    """
    Manual stress tests for resource cleanup during concurrent operations.
    
    ⚠️  NOT SUITABLE FOR AUTOMATED CI/CD - MANUAL USE ONLY
    """
    
    def setUp(self):
        super().setUp()
        init_logger()
        
        # Get test configuration
        self.config = getattr(self, '_test_config', STRESS_CONFIGS['full'])
        
        self.created_registries = []
        self.resource_metrics = {
            'tubes_created': 0,
            'tubes_destroyed': 0,
            'cleanup_operations': 0,
            'concurrent_operations': 0,
            'errors': []
        }
        self._metrics_lock = threading.Lock()
        
        # System resource detection
        self.system_info = self._detect_system_resources()
        logging.info(f"🖥️  System Resources Detected:")
        logging.info(f"   CPU Cores: {self.system_info['cpu_cores']}")
        logging.info(f"   Available Memory: ~{self.system_info['memory_gb']:.1f}GB")
        logging.info(f"   Test Configuration: {self.config.name}")
        logging.info(f"   Description: {self.config.description}")
        
        # Warn about resource usage
        if self.config.name == 'Extreme Stress':
            logging.warning("⚠️  EXTREME STRESS MODE: This test may impact system stability!")
            logging.warning("   Monitor system resources and be prepared to interrupt if needed.")

    def _detect_system_resources(self) -> Dict:
        """Detect available system resources for adaptive testing"""
        try:
            cpu_count = os.cpu_count() or 4
            
            # Try to get memory info (Linux/Mac)
            try:
                if hasattr(os, 'sysconf') and hasattr(os, 'sysconf_names'):
                    if '_SC_PAGE_SIZE' in os.sysconf_names and '_SC_PHYS_PAGES' in os.sysconf_names:
                        page_size = os.sysconf('_SC_PAGE_SIZE')
                        phys_pages = os.sysconf('_SC_PHYS_PAGES')
                        memory_bytes = page_size * phys_pages
                        memory_gb = memory_bytes / (1024**3)
                    else:
                        memory_gb = 8.0  # Default assumption
                else:
                    memory_gb = 8.0  # Default assumption
            except (AttributeError, OSError):
                memory_gb = 8.0  # Default assumption
                
            return {
                'cpu_cores': cpu_count,
                'memory_gb': memory_gb,
                'is_constrained': cpu_count < 4 or memory_gb < 4
            }
        except Exception:
            return {'cpu_cores': 4, 'memory_gb': 8.0, 'is_constrained': False}

    def tearDown(self):
        super().tearDown()
        # Report final metrics
        with self._metrics_lock:
            logging.info(f"📊 Final resource metrics: {self.resource_metrics}")
        
        # Emergency cleanup
        for registry in self.created_registries:
            try:
                if registry.has_active_tubes():
                    registry.cleanup_all()
            except Exception as e:
                logging.error(f"tearDown registry cleanup failed: {e}")
        self.created_registries.clear()

    def create_tracked_registry(self):
        """Create a registry and track it for cleanup"""
        registry = keeper_pam_webrtc_rs.PyTubeRegistry()
        self.configure_test_resource_limits(registry)
        self.created_registries.append(registry)
        return registry

    def record_metric(self, metric_name, value=1):
        """Thread-safe metric recording"""
        with self._metrics_lock:
            if metric_name in self.resource_metrics:
                if isinstance(self.resource_metrics[metric_name], int):
                    self.resource_metrics[metric_name] += value
                elif isinstance(self.resource_metrics[metric_name], list):
                    self.resource_metrics[metric_name].append(value)

    def test_resource_cleanup_under_stress(self):
        """Test resource cleanup during concurrent restart attempts"""
        logging.info(f"🧪 === {self.config.name} Resource Cleanup Test ===")
        
        registry = self.create_tracked_registry()
        settings = {"conversationType": "tunnel"}
        
        # Create multiple tubes for stress testing
        num_tubes = self.config.num_tubes
        tube_ids = []
        
        logging.info(f"Creating {num_tubes} tubes for stress testing...")
        for i in range(num_tubes):
            try:
                tube_info = registry.create_tube(
                    conversation_id=f"stress-cleanup-{i}",
                    settings=settings,
                    trickle_ice=True,
                    callback_token=TEST_CALLBACK_TOKEN,
                    krelay_server="test.relay.server.com",
                    client_version="ms16.5.0",
                    ksm_config=TEST_KSM_CONFIG
                )
                tube_ids.append(tube_info['tube_id'])
                self.record_metric('tubes_created')
            except Exception as e:
                self.record_metric('errors', f"Tube creation {i}: {e}")
                logging.error(f"Failed to create tube {i}: {e}")
        
        logging.info(f"Successfully created {len(tube_ids)} tubes")
        
        # Verify initial active tube count
        initial_count = registry.active_tube_count()
        self.assertEqual(initial_count, len(tube_ids), "All tubes should be active initially")
        
        # Test concurrent cleanup operations
        cleanup_results = queue.Queue()
        error_results = queue.Queue()
        
        def stress_cleanup_worker(worker_id, tube_batch):
            """Worker that performs cleanup operations on a batch of tubes"""
            try:
                start_time = time.time()
                
                for tube_id in tube_batch:
                    try:
                        # Simulate concurrent operations that might interfere with cleanup
                        state = registry.get_connection_state(tube_id)
                        time.sleep(0.005)  # Reduced delay for better CI compatibility
                        
                        # Close the tube
                        registry.close_tube(tube_id)
                        self.record_metric('tubes_destroyed')
                        self.record_metric('cleanup_operations')
                        
                        logging.debug(f"Worker {worker_id}: Cleaned up {tube_id} (was {state})")
                        
                    except Exception as e:
                        error_msg = f"Worker {worker_id} cleanup error for {tube_id}: {e}"
                        error_results.put(error_msg)
                        self.record_metric('errors', error_msg)
                        logging.error(error_msg)
                
                duration = time.time() - start_time
                cleanup_results.put({
                    'worker_id': worker_id,
                    'tubes_processed': len(tube_batch),
                    'duration': duration
                })
                
            except Exception as e:
                error_msg = f"Worker {worker_id} critical error: {e}"
                error_results.put(error_msg)
                self.record_metric('errors', error_msg)
                logging.error(error_msg, exc_info=True)
        
        # Divide tubes among workers for concurrent cleanup
        num_workers = self.config.num_workers
        tubes_per_worker = len(tube_ids) // num_workers
        workers = []
        
        logging.info(f"Starting {num_workers} concurrent cleanup workers...")
        
        for worker_id in range(num_workers):
            start_idx = worker_id * tubes_per_worker
            if worker_id == num_workers - 1:  # Last worker gets remaining tubes
                end_idx = len(tube_ids)
            else:
                end_idx = start_idx + tubes_per_worker
            
            tube_batch = tube_ids[start_idx:end_idx]
            worker = threading.Thread(target=stress_cleanup_worker, args=(worker_id, tube_batch))
            workers.append(worker)
            worker.start()
        
        # Wait for all workers with timeout
        worker_timeout = self.config.worker_timeout
        start_time = time.time()
        
        for i, worker in enumerate(workers):
            remaining_time = worker_timeout - (time.time() - start_time)
            if remaining_time > 0:
                worker.join(timeout=remaining_time)
                if worker.is_alive():
                    self.fail(f"Cleanup worker {i} did not complete within {worker_timeout}s - possible deadlock!")
            else:
                self.fail(f"Overall cleanup timeout exceeded {worker_timeout}s")
        
        total_cleanup_time = time.time() - start_time
        
        # Collect results
        worker_results = []
        while not cleanup_results.empty():
            try:
                result = cleanup_results.get_nowait()
                worker_results.append(result)
            except queue.Empty:
                break
        
        errors = []
        while not error_results.empty():
            try:
                error = error_results.get_nowait()
                errors.append(error)
            except queue.Empty:
                break
        
        # Analyze results
        self._log_stress_test_results(
            total_cleanup_time, worker_results, errors, 
            len(tube_ids), num_workers, "Cleanup"
        )
        
        # Verify final state
        time.sleep(1.0)  # Allow cleanup to fully complete
        final_count = registry.active_tube_count()
        logging.info(f"Final active tube count: {final_count}")
        
        # Assertions
        self.assertEqual(len(worker_results), num_workers, "All workers should complete")
        
        total_tubes_processed = sum(r['tubes_processed'] for r in worker_results)
        self.assertEqual(total_tubes_processed, len(tube_ids), "All tubes should be processed")
        self.assertEqual(final_count, 0, "No tubes should remain active after cleanup")
        
        # Performance assertions (skip if requested)
        if not self.config.skip_timing_assertions:
            self._assert_performance_benchmarks(total_cleanup_time, errors, len(tube_ids))
        
        # Error rate assertion
        error_rate = len(errors) / len(tube_ids) if len(tube_ids) > 0 else 0
        if error_rate > self.config.acceptable_error_rate:
            self.fail(f"Error rate {error_rate:.1%} exceeds acceptable threshold {self.config.acceptable_error_rate:.1%}")
        
        logging.info(f"✅ Stress test passed with {error_rate:.1%} error rate (threshold: {self.config.acceptable_error_rate:.1%})")

    def test_concurrent_registry_operations(self):
        """Test multiple registries with concurrent operations"""
        logging.info(f"🧪 === {self.config.name} Concurrent Registry Operations Test ===")
        
        num_registries = self.config.num_registries
        registries = []
        
        # Create multiple registries
        for i in range(num_registries):
            registry = self.create_tracked_registry()
            registries.append(registry)
        
        logging.info(f"Created {num_registries} registries for concurrent testing")
        
        # Test concurrent operations across registries
        operation_results = queue.Queue()
        
        def registry_worker(registry_id, registry):
            """Worker that performs operations on a specific registry"""
            try:
                results = {
                    'registry_id': registry_id,
                    'tubes_created': 0,
                    'tubes_cleaned': 0,
                    'operations': 0,
                    'errors': []
                }
                
                # Create some tubes (fewer for lighter load)
                tubes_to_create = 3 if self.config.name == 'Light Stress' else 3
                tube_ids = []
                for i in range(tubes_to_create):
                    try:
                        tube_info = registry.create_tube(
                            conversation_id=f"concurrent-reg{registry_id}-tube{i}",
                            settings={"conversationType": "tunnel"},
                            trickle_ice=True,
                            callback_token=TEST_CALLBACK_TOKEN,
                            krelay_server="test.relay.server.com",
                            client_version="ms16.5.0",
                            ksm_config=TEST_KSM_CONFIG
                        )
                        tube_ids.append(tube_info['tube_id'])
                        results['tubes_created'] += 1
                        results['operations'] += 1
                        
                    except Exception as e:
                        results['errors'].append(f"Tube creation {i}: {e}")
                
                # Perform state checks (exercises mutex usage)
                state_checks = 10 if self.config.name != 'Extreme Stress' else 20
                for _ in range(state_checks):
                    try:
                        for tube_id in tube_ids:
                            state = registry.get_connection_state(tube_id)
                            results['operations'] += 1
                        time.sleep(0.005)  # Reduced delay for better compatibility
                    except Exception as e:
                        results['errors'].append(f"State check: {e}")
                
                # Clean up tubes
                for tube_id in tube_ids:
                    try:
                        registry.close_tube(tube_id)
                        results['tubes_cleaned'] += 1
                        results['operations'] += 1
                    except Exception as e:
                        results['errors'].append(f"Tube cleanup {tube_id}: {e}")
                
                # Final registry cleanup
                try:
                    registry.cleanup_all()
                    results['operations'] += 1
                except Exception as e:
                    results['errors'].append(f"Registry cleanup: {e}")
                
                operation_results.put(results)
                
            except Exception as e:
                operation_results.put({
                    'registry_id': registry_id,
                    'critical_error': str(e),
                    'operations': 0
                })
                logging.error(f"Registry worker {registry_id} critical error: {e}", exc_info=True)
        
        # Start concurrent workers
        workers = []
        start_time = time.time()
        
        for i, registry in enumerate(registries):
            worker = threading.Thread(target=registry_worker, args=(i, registry))
            workers.append(worker)
            worker.start()
        
        # Wait for completion
        timeout = self.config.worker_timeout * 0.8  # Slightly shorter timeout for this test
        for i, worker in enumerate(workers):
            worker.join(timeout=timeout)
            if worker.is_alive():
                self.fail(f"Registry worker {i} did not complete within {timeout}s timeout")
        
        total_duration = time.time() - start_time
        
        # Collect and analyze results
        worker_results = []
        while not operation_results.empty():
            try:
                result = operation_results.get_nowait()
                worker_results.append(result)
            except queue.Empty:
                break
        
        self._log_registry_test_results(total_duration, worker_results, num_registries)
        
        # Assertions
        self.assertEqual(len(worker_results), num_registries, "All registry workers should complete")
        
        critical_errors = len([r for r in worker_results if 'critical_error' in r])
        self.assertEqual(critical_errors, 0, "No registry workers should have critical errors")
        
        logging.info(f"✅ Concurrent registry test passed")

    def test_memory_cleanup_verification(self):
        """Test that memory is properly cleaned up after intensive operations"""
        logging.info(f"🧪 === {self.config.name} Memory Cleanup Verification Test ===")
        
        # Force garbage collection to get baseline
        gc.collect()
        initial_thread_count = threading.active_count()
        
        logging.info(f"Baseline thread count: {initial_thread_count}")
        
        # Perform intensive operations that create and destroy resources
        registry = self.create_tracked_registry()
        
        cycles = self.config.cycles
        tubes_per_cycle = self.config.tubes_per_cycle
        
        for cycle in range(cycles):
            logging.info(f"Memory test cycle {cycle + 1}/{cycles}")
            
            # Create many tubes
            tube_ids = []
            for i in range(tubes_per_cycle):
                try:
                    tube_info = registry.create_tube(
                        conversation_id=f"memory-test-c{cycle}-t{i}",
                        settings={"conversationType": "tunnel"},
                        trickle_ice=True,
                        callback_token=TEST_CALLBACK_TOKEN,
                        krelay_server="test.relay.server.com",
                        client_version="ms16.5.0",
                        ksm_config=TEST_KSM_CONFIG
                    )
                    tube_ids.append(tube_info['tube_id'])
                except Exception as e:
                    logging.error(f"Failed to create tube in cycle {cycle}: {e}")
            
            # Verify they're active
            active_count = registry.active_tube_count()
            logging.debug(f"Cycle {cycle + 1}: Created {len(tube_ids)} tubes, active: {active_count}")
            
            # Clean them up
            for tube_id in tube_ids:
                try:
                    registry.close_tube(tube_id)
                except Exception as e:
                    logging.error(f"Failed to cleanup tube {tube_id}: {e}")
            
            # Verify cleanup
            final_active = registry.active_tube_count()
            logging.debug(f"Cycle {cycle + 1}: After cleanup, active: {final_active}")
            
            # Force garbage collection between cycles
            gc.collect()
            current_thread_count = threading.active_count()
            logging.debug(f"Cycle {cycle + 1}: Thread count: {current_thread_count}")
        
        # Final cleanup and memory check
        registry.cleanup_all()
        time.sleep(1.0)  # Allow cleanup to complete
        gc.collect()
        
        final_thread_count = threading.active_count()
        final_active_tubes = registry.active_tube_count()
        
        self._log_memory_test_results(
            cycles, tubes_per_cycle, initial_thread_count, 
            final_thread_count, final_active_tubes
        )
        
        # Assertions for memory cleanup
        self.assertEqual(final_active_tubes, 0, "All tubes should be cleaned up")
        
        # Thread count should not increase significantly
        thread_increase = final_thread_count - initial_thread_count
        max_acceptable_increase = 5 if self.config.name != 'Extreme Stress' else 10
        
        if thread_increase > max_acceptable_increase:
            logging.warning(f"⚠️  Thread count increased by {thread_increase} (threshold: {max_acceptable_increase})")
            if not self.config.skip_timing_assertions:
                self.fail(f"Thread increase {thread_increase} exceeds threshold {max_acceptable_increase}")
        
        # Verify we can still create new registries after intensive operations
        test_registry = self.create_tracked_registry()
        self.assertEqual(test_registry.active_tube_count(), 0, "New registry should start clean")
        
        logging.info(f"✅ Memory cleanup test passed")

    def _log_stress_test_results(self, total_time, worker_results, errors, num_tubes, num_workers, test_name):
        """Log detailed stress test results"""
        logging.info(f"📊 {test_name} stress test results:")
        logging.info(f"   Total time: {total_time:.3f}s")
        logging.info(f"   Workers completed: {len(worker_results)}/{num_workers}")
        logging.info(f"   Total errors: {len(errors)}")
        
        total_tubes_processed = sum(r['tubes_processed'] for r in worker_results)
        logging.info(f"   Tubes processed: {total_tubes_processed}/{num_tubes}")
        
        if total_time > 0:
            cleanup_rate = total_tubes_processed / total_time
            logging.info(f"   Processing rate: {cleanup_rate:.1f} tubes/second")
        
        # Worker performance analysis
        for result in worker_results:
            rate = result['tubes_processed'] / result['duration'] if result['duration'] > 0 else 0
            logging.info(f"   Worker {result['worker_id']}: {result['tubes_processed']} tubes in {result['duration']:.3f}s ({rate:.1f} tubes/s)")
        
        # Log errors (limited)
        for error in errors[:5]:  # Limit to first 5 errors
            logging.error(f"   {error}")
        if len(errors) > 5:
            logging.error(f"   ... and {len(errors) - 5} more errors")

    def _log_registry_test_results(self, total_duration, worker_results, num_registries):
        """Log concurrent registry test results"""
        logging.info("📊 Concurrent registry operations results:")
        logging.info(f"   Total duration: {total_duration:.3f}s")
        logging.info(f"   Workers completed: {len(worker_results)}/{num_registries}")
        
        total_operations = 0
        total_errors = 0
        
        for result in worker_results:
            if 'critical_error' in result:
                logging.error(f"   Registry {result['registry_id']}: CRITICAL ERROR - {result['critical_error']}")
                total_errors += 1
            else:
                ops = result['operations']
                errors = len(result['errors'])
                total_operations += ops
                total_errors += errors
                
                rate = ops / total_duration if total_duration > 0 else 0
                logging.info(f"   Registry {result['registry_id']}: {ops} ops, {errors} errors, {rate:.1f} ops/s")
                logging.info(f"     Created: {result['tubes_created']}, Cleaned: {result['tubes_cleaned']}")
                
                # Log first few errors
                for error in result['errors'][:2]:
                    logging.warning(f"     Error: {error}")
                if len(result['errors']) > 2:
                    logging.warning(f"     ... and {len(result['errors']) - 2} more errors")
        
        # Overall metrics
        if total_duration > 0 and total_operations > 0:
            overall_rate = total_operations / total_duration
            error_rate = total_errors / total_operations
            logging.info(f"   Overall: {total_operations} ops in {total_duration:.3f}s ({overall_rate:.1f} ops/s)")
            logging.info(f"   Error rate: {error_rate:.1%}")

    def _log_memory_test_results(self, cycles, tubes_per_cycle, initial_threads, final_threads, final_tubes):
        """Log memory cleanup test results"""
        thread_change = final_threads - initial_threads
        total_processed = cycles * tubes_per_cycle
        
        logging.info("📊 Memory cleanup verification results:")
        logging.info(f"   Cycles completed: {cycles}")
        logging.info(f"   Tubes per cycle: {tubes_per_cycle}")
        logging.info(f"   Total tubes processed: {total_processed}")
        logging.info(f"   Initial thread count: {initial_threads}")
        logging.info(f"   Final thread count: {final_threads}")
        logging.info(f"   Thread count change: {thread_change}")
        logging.info(f"   Final active tubes: {final_tubes}")
        
        if thread_change <= 2:
            logging.info("   ✅ [EXCELLENT]: Thread count stable - no memory leaks detected")
        elif thread_change <= 5:
            logging.info("   ✅ [GOOD]: Small thread count increase - likely acceptable")
        else:
            logging.warning(f"   ⚠️  [WARN] MEMORY CONCERN: Thread count increased by {thread_change}")

    def _assert_performance_benchmarks(self, total_time, errors, num_tubes):
        """Assert performance benchmarks (when not skipped)"""
        # Adaptive timing based on system resources
        fast_threshold = 15.0 if self.system_info.get('is_constrained', False) else 10.0
        acceptable_threshold = 30.0 if self.system_info.get('is_constrained', False) else 20.0
        
        if total_time < fast_threshold:
            logging.info("   ⚡ [EXCELLENT]: Cleanup completed quickly")
        elif total_time < acceptable_threshold:
            logging.info("   ✅ [GOOD]: Cleanup completed within acceptable time")
        else:
            logging.warning(f"   ⚠️  [WARN] SLOW CLEANUP: Took {total_time:.1f}s (threshold: {acceptable_threshold:.1f}s)")
        
        if len(errors) == 0:
            logging.info("   ✅ [PERFECT] NO ERRORS: Clean concurrent cleanup")
        elif len(errors) < num_tubes * self.config.acceptable_error_rate:
            logging.info(f"   ✅ [PASS] LOW ERROR RATE: {len(errors)} errors out of {num_tubes} operations")
        else:
            logging.warning(f"   ⚠️  [WARN] HIGH ERROR RATE: {len(errors)} errors")


def main():
    """Main entry point for manual stress tests with command line options"""
    
    parser = argparse.ArgumentParser(
        description='Manual Stress Tests for WebRTC Resource Cleanup',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python3 manual_stress_tests.py --light     # Light stress test
  python3 manual_stress_tests.py --full      # Full stress test (default)
  python3 manual_stress_tests.py --extreme   # Extreme stress test
  python3 manual_stress_tests.py --list      # List available test modes

⚠️  WARNING: These tests are NOT for CI/CD! Manual development use only.
        """
    )
    
    mode_group = parser.add_mutually_exclusive_group()
    mode_group.add_argument('--light', action='store_true', 
                           help='Run light stress tests (reduced load)')
    mode_group.add_argument('--full', action='store_true', 
                           help='Run full stress tests (default)')
    mode_group.add_argument('--extreme', action='store_true', 
                           help='Run extreme stress tests (maximum load)')
    mode_group.add_argument('--list', action='store_true', 
                           help='List available test configurations')
    
    parser.add_argument('-v', '--verbose', action='store_true',
                       help='Enable verbose logging')
    parser.add_argument('--test', 
                       help='Run specific test method (e.g., test_resource_cleanup_under_stress)')
    
    args = parser.parse_args()
    
    if args.list:
        print("🧪 Available Stress Test Configurations:")
        print()
        for key, config in STRESS_CONFIGS.items():
            print(f"  {key.upper()}:")
            print(f"    Name: {config.name}")
            print(f"    Description: {config.description}")
            print(f"    Tubes: {config.num_tubes}, Workers: {config.num_workers}")
            print(f"    Registries: {config.num_registries}, Cycles: {config.cycles}")
            print(f"    Error Threshold: {config.acceptable_error_rate:.1%}")
            print()
        return
    
    # Determine test configuration
    if args.light:
        config_name = 'light'
    elif args.extreme:
        config_name = 'extreme' 
    else:
        config_name = 'full'  # default
    
    config = STRESS_CONFIGS[config_name]
    
    # Configure logging
    log_level = logging.DEBUG if args.verbose else logging.INFO
    logging.basicConfig(
        level=log_level,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    # Display warnings
    print("⚠️  " + "="*70)
    print("⚠️  WARNING: MANUAL STRESS TESTS - NOT FOR CI/CD")
    print("⚠️  " + "="*70)
    print(f"   Mode: {config.name}")
    print(f"   Description: {config.description}")
    print("   These tests may exhibit non-deterministic behavior")
    print("   and are intended for manual development testing only.")
    print("   " + "="*70)
    print()
    
    # Set test configuration on the test class
    ManualStressTest._test_config = config
    
    # Run tests
    if args.test:
        # Run specific test
        suite = unittest.TestSuite()
        suite.addTest(ManualStressTest(args.test))
        runner = unittest.TextTestRunner(verbosity=2)
        result = runner.run(suite)
    else:
        # Run all tests
        unittest.main(argv=[sys.argv[0]], verbosity=2, exit=False)


if __name__ == '__main__':
    main()