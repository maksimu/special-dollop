"""Tests for proxy lifecycle management.

These tests verify start/stop cycles, cleanup behavior, state transitions,
and proper resource management.
"""

import socket
import time

import pytest


class TestStartStop:
    """Tests for basic start and stop operations."""

    def test_start_creates_running_proxy(self, minimal_config):
        """Test that start_proxy creates a running proxy."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        try:
            assert proxy.is_running()
        finally:
            proxy.stop()

    def test_stop_stops_proxy(self, minimal_config):
        """Test that stop() stops the proxy."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        assert proxy.is_running()
        proxy.stop()
        assert not proxy.is_running()

    def test_stop_idempotent(self, minimal_config):
        """Test that calling stop() multiple times is safe."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        proxy.stop()
        proxy.stop()
        proxy.stop()
        proxy.stop()
        proxy.stop()
        # No exception should be raised
        assert not proxy.is_running()

    def test_rapid_start_stop_cycles(self, unique_port):
        """Test rapid start/stop cycles on the same port."""
        import keeper_pam_connections

        config = {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": unique_port,
        }

        for i in range(3):
            proxy = keeper_pam_connections.keeperdb_proxy(config)
            assert proxy.is_running(), f"Cycle {i}: proxy should be running"
            proxy.stop()
            assert not proxy.is_running(), f"Cycle {i}: proxy should be stopped"
            # Small delay to allow port release
            time.sleep(0.15)


class TestMultipleProxies:
    """Tests for running multiple proxies simultaneously."""

    def test_multiple_proxies_different_ports(self, unique_port):
        """Test running multiple proxies on different ports."""
        import keeper_pam_connections

        proxies = []
        try:
            for i in range(3):
                config = {
                    "database_type": "mysql",
                    "target_host": "localhost",
                    "target_port": 3306,
                    "username": "testuser",
                    "password": "testpass",
                    "listen_port": unique_port + i,
                }
                proxy = keeper_pam_connections.keeperdb_proxy(config)
                proxies.append(proxy)

            # All should be running
            for i, proxy in enumerate(proxies):
                assert proxy.is_running(), f"Proxy {i} should be running"

            # All should have different ports
            ports = [p.port for p in proxies]
            assert len(set(ports)) == len(ports), "All proxies should have unique ports"

        finally:
            for proxy in proxies:
                proxy.stop()

    def test_multiple_proxies_mixed_modes(self, unique_port):
        """Test running settings mode and gateway mode proxies together."""
        import keeper_pam_connections

        settings_proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": unique_port,
        })

        gateway_proxy = keeper_pam_connections.keeperdb_proxy({
            "listen_port": unique_port + 1,
        })

        try:
            assert settings_proxy.is_running()
            assert gateway_proxy.is_running()
            assert settings_proxy.gateway_mode is False
            assert gateway_proxy.gateway_mode is True
        finally:
            settings_proxy.stop()
            gateway_proxy.stop()


class TestStateTransitions:
    """Tests for is_running() state accuracy."""

    def test_is_running_true_after_start(self, minimal_config):
        """Test is_running() is True immediately after start."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        try:
            # Should be True immediately
            assert proxy.is_running() is True
        finally:
            proxy.stop()

    def test_is_running_false_after_stop(self, minimal_config):
        """Test is_running() is False after stop."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        proxy.stop()
        assert proxy.is_running() is False

    def test_is_running_returns_bool(self, proxy):
        """Test is_running() returns actual bool type."""
        result = proxy.is_running()
        assert isinstance(result, bool)
        assert result is True


class TestAutoPortAssignment:
    """Tests for automatic port assignment."""

    def test_auto_port_assignment(self):
        """Test that port 0 auto-assigns a valid port."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 0,  # Auto-assign
        })
        try:
            assert proxy.is_running()
            assert proxy.port > 0
            assert proxy.port < 65536
        finally:
            proxy.stop()

    def test_auto_port_actually_binds(self):
        """Test that auto-assigned port is actually bound."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 0,
        })
        try:
            time.sleep(0.1)
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            result = sock.connect_ex((proxy.host, proxy.port))
            sock.close()
            assert result == 0, f"Could not connect to auto-assigned port {proxy.port}"
        finally:
            proxy.stop()


class TestContextManager:
    """Tests for context manager behavior."""

    def test_context_manager_basic(self, minimal_config):
        """Test basic context manager usage."""
        import keeper_pam_connections

        with keeper_pam_connections.keeperdb_proxy(minimal_config) as proxy:
            assert proxy.is_running()

        assert not proxy.is_running()

    def test_context_manager_cleanup_on_exception(self, minimal_config):
        """Test context manager cleans up on exception."""
        import keeper_pam_connections

        proxy_ref = None
        try:
            with keeper_pam_connections.keeperdb_proxy(minimal_config) as proxy:
                proxy_ref = proxy
                raise RuntimeError("Test error")
        except RuntimeError:
            pass

        assert proxy_ref is not None
        assert not proxy_ref.is_running()

    def test_context_manager_nested(self, unique_port):
        """Test nested context managers."""
        import keeper_pam_connections

        config1 = {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": unique_port,
        }
        config2 = {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": unique_port + 1,
        }

        with keeper_pam_connections.keeperdb_proxy(config1) as proxy1:
            with keeper_pam_connections.keeperdb_proxy(config2) as proxy2:
                assert proxy1.is_running()
                assert proxy2.is_running()
            assert not proxy2.is_running()
            assert proxy1.is_running()
        assert not proxy1.is_running()


class TestHostPortProperties:
    """Tests for host and port properties."""

    def test_host_property(self, minimal_config):
        """Test host property returns correct value."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        try:
            assert proxy.host == "127.0.0.1"
        finally:
            proxy.stop()

    def test_port_property(self, minimal_config):
        """Test port property returns correct value."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(minimal_config)
        try:
            assert proxy.port == minimal_config["listen_port"]
        finally:
            proxy.stop()

    def test_custom_listen_host(self, unique_port):
        """Test custom listen host is respected."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_host": "127.0.0.1",
            "listen_port": unique_port,
        })
        try:
            assert proxy.host == "127.0.0.1"
        finally:
            proxy.stop()
