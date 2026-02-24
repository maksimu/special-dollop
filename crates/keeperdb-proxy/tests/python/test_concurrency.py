"""Tests for thread safety and concurrent access.

These tests verify that ProxyHandle is safe to use across multiple threads.
"""

import threading
import time

import pytest


class TestConcurrentProxyStarts:
    """Tests for starting multiple proxies from different threads."""

    def test_concurrent_proxy_starts(self, unique_port):
        """Test starting multiple proxies from different threads."""
        import keeper_pam_connections

        proxies = []
        errors = []
        lock = threading.Lock()

        def start_proxy(port):
            try:
                proxy = keeper_pam_connections.keeperdb_proxy({
                    "database_type": "mysql",
                    "target_host": "localhost",
                    "target_port": 3306,
                    "username": "testuser",
                    "password": "testpass",
                    "listen_port": port,
                })
                with lock:
                    proxies.append(proxy)
            except Exception as e:
                with lock:
                    errors.append(e)

        threads = []
        for i in range(5):
            t = threading.Thread(target=start_proxy, args=(unique_port + i,))
            threads.append(t)

        for t in threads:
            t.start()

        for t in threads:
            t.join(timeout=5.0)

        try:
            assert len(errors) == 0, f"Errors during concurrent start: {errors}"
            assert len(proxies) == 5, f"Expected 5 proxies, got {len(proxies)}"

            for proxy in proxies:
                assert proxy.is_running()
        finally:
            for proxy in proxies:
                proxy.stop()


class TestConcurrentStopCalls:
    """Tests for concurrent stop() calls."""

    def test_concurrent_stop_calls(self, minimal_config):
        """Test multiple threads calling stop() simultaneously."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        errors = []

        def call_stop():
            try:
                proxy.stop()
            except Exception as e:
                errors.append(e)

        threads = []
        for _ in range(10):
            t = threading.Thread(target=call_stop)
            threads.append(t)

        for t in threads:
            t.start()

        for t in threads:
            t.join(timeout=2.0)

        assert len(errors) == 0, f"Errors during concurrent stop: {errors}"
        assert not proxy.is_running()


class TestConcurrentStateQueries:
    """Tests for concurrent is_running() queries."""

    def test_concurrent_is_running_checks(self, proxy):
        """Test concurrent is_running() calls."""
        results = []
        lock = threading.Lock()

        def check_running():
            for _ in range(100):
                result = proxy.is_running()
                with lock:
                    results.append(result)

        threads = []
        for _ in range(5):
            t = threading.Thread(target=check_running)
            threads.append(t)

        for t in threads:
            t.start()

        for t in threads:
            t.join(timeout=5.0)

        # All results should be True (proxy still running)
        assert all(results), "Some is_running() calls returned False unexpectedly"
        assert len(results) == 500

    def test_concurrent_active_sessions(self, gateway_proxy):
        """Test concurrent active_sessions() calls."""
        results = []
        lock = threading.Lock()

        def check_sessions():
            for _ in range(100):
                count = gateway_proxy.active_sessions()
                with lock:
                    results.append(count)

        threads = []
        for _ in range(5):
            t = threading.Thread(target=check_sessions)
            threads.append(t)

        for t in threads:
            t.start()

        for t in threads:
            t.join(timeout=5.0)

        # All results should be 0 (no sessions)
        assert all(r == 0 for r in results), "Some active_sessions() returned non-zero"
        assert len(results) == 500


class TestCrossThreadUsage:
    """Tests for using proxy across different threads."""

    def test_proxy_created_in_main_used_in_thread(self, proxy):
        """Test using a proxy created in main thread from another thread."""
        results = []
        errors = []

        def use_proxy():
            try:
                results.append(proxy.is_running())
                results.append(proxy.host)
                results.append(proxy.port)
                results.append(proxy.gateway_mode)
            except Exception as e:
                errors.append(e)

        t = threading.Thread(target=use_proxy)
        t.start()
        t.join(timeout=2.0)

        assert len(errors) == 0, f"Errors using proxy from thread: {errors}"
        assert len(results) == 4
        assert results[0] is True  # is_running
        assert results[1] == "127.0.0.1"  # host
        assert isinstance(results[2], int)  # port
        assert results[3] is False  # gateway_mode

    def test_proxy_stopped_from_different_thread(self, minimal_config):
        """Test stopping a proxy from a different thread than it was created."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        stopped = []

        def stop_proxy():
            proxy.stop()
            stopped.append(True)

        t = threading.Thread(target=stop_proxy)
        t.start()
        t.join(timeout=2.0)

        assert len(stopped) == 1
        assert not proxy.is_running()


class TestThreadSafetyStress:
    """Stress tests for thread safety."""

    def test_mixed_operations_concurrent(self, unique_port):
        """Test mixed operations (start, query, stop) concurrently."""
        import keeper_pam_connections

        operations_completed = []
        errors = []
        lock = threading.Lock()

        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": unique_port,
        })

        def query_operations():
            try:
                for _ in range(50):
                    _ = proxy.is_running()
                    _ = proxy.host
                    _ = proxy.port
                    _ = repr(proxy)
                with lock:
                    operations_completed.append("query")
            except Exception as e:
                with lock:
                    errors.append(("query", e))

        threads = []
        for _ in range(4):
            t = threading.Thread(target=query_operations)
            threads.append(t)

        for t in threads:
            t.start()

        for t in threads:
            t.join(timeout=10.0)

        proxy.stop()

        assert len(errors) == 0, f"Errors during stress test: {errors}"
        assert len(operations_completed) == 4
