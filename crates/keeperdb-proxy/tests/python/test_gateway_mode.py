"""Tests for gateway mode functionality.

Gateway mode is activated when no credentials (username/password) are provided.
In this mode, credentials are delivered via the Gateway handshake protocol.
"""

import socket
import time

import pytest


class TestGatewayModeStartup:
    """Tests for starting proxy in gateway mode."""

    def test_gateway_mode_startup(self, gateway_config):
        """Test that proxy starts in gateway mode without credentials."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(gateway_config)
        try:
            assert proxy.is_running()
            assert proxy.gateway_mode is True
        finally:
            proxy.stop()

    def test_gateway_mode_with_listen_host(self, unique_port):
        """Test gateway mode with custom listen host."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy({
            "listen_host": "127.0.0.1",
            "listen_port": unique_port,
        })
        try:
            assert proxy.is_running()
            assert proxy.host == "127.0.0.1"
            assert proxy.gateway_mode is True
        finally:
            proxy.stop()

    def test_gateway_mode_port_binding(self, gateway_config):
        """Test that gateway mode proxy actually binds to the port."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy(gateway_config)
        try:
            # Give proxy a moment to bind
            time.sleep(0.1)

            # Try to connect
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            result = sock.connect_ex((proxy.host, proxy.port))
            sock.close()

            assert result == 0, f"Failed to connect to gateway proxy: {result}"
        finally:
            proxy.stop()

    def test_gateway_mode_with_session_config(self, unique_port):
        """Test gateway mode with session configuration."""
        import keeper_pam_connections

        proxy = keeper_pam_connections.keeperdb_proxy({
            "listen_port": unique_port,
            "session_config": {
                "max_duration_secs": 3600,
                "idle_timeout_secs": 300,
                "max_connections": 10,
            },
        })
        try:
            assert proxy.is_running()
            assert proxy.gateway_mode is True
        finally:
            proxy.stop()


class TestGatewayModeFlag:
    """Tests for the gateway_mode property."""

    def test_gateway_mode_flag_true(self, gateway_proxy):
        """Test gateway_mode is True when no credentials provided."""
        assert gateway_proxy.gateway_mode is True

    def test_settings_mode_flag_false(self, proxy):
        """Test gateway_mode is False when credentials are provided."""
        assert proxy.gateway_mode is False

    def test_gateway_mode_detection_username_only(self, unique_port):
        """Test that username without password is still settings mode attempt."""
        import keeper_pam_connections

        # This should raise an error because settings mode needs both
        with pytest.raises(ValueError):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "username": "testuser",
                # No password - incomplete for settings mode
                "listen_port": unique_port,
            })


class TestActiveSessions:
    """Tests for the active_sessions() method."""

    def test_active_sessions_initial_zero(self, gateway_proxy):
        """Test active_sessions returns 0 initially in gateway mode."""
        assert gateway_proxy.active_sessions() == 0

    def test_active_sessions_settings_mode_zero(self, proxy):
        """Test active_sessions returns 0 in settings mode."""
        # Settings mode doesn't use credential store
        assert proxy.active_sessions() == 0

    def test_active_sessions_callable(self, gateway_proxy):
        """Test active_sessions is callable and returns int."""
        result = gateway_proxy.active_sessions()
        assert isinstance(result, int)
        assert result >= 0


class TestForceReleaseTunnel:
    """Tests for the force_release_tunnel() method."""

    def test_force_release_tunnel_no_error(self, gateway_proxy):
        """Test force_release_tunnel doesn't raise with valid call."""
        # Should not raise even with unknown UID
        gateway_proxy.force_release_tunnel("unknown-session-uid")

    def test_force_release_tunnel_empty_uid(self, gateway_proxy):
        """Test force_release_tunnel handles empty UID."""
        gateway_proxy.force_release_tunnel("")

    def test_force_release_tunnel_special_chars(self, gateway_proxy):
        """Test force_release_tunnel handles special characters in UID."""
        gateway_proxy.force_release_tunnel("session-uid-with-special-chars-!@#$%")


class TestGatewayModeRepr:
    """Tests for string representation in gateway mode."""

    def test_gateway_mode_repr(self, gateway_proxy):
        """Test __repr__ shows mode='gateway'."""
        repr_str = repr(gateway_proxy)
        assert "ProxyHandle" in repr_str
        assert "gateway" in repr_str
        assert "running=true" in repr_str

    def test_settings_mode_repr(self, proxy):
        """Test __repr__ shows mode='settings'."""
        repr_str = repr(proxy)
        assert "ProxyHandle" in repr_str
        assert "settings" in repr_str
        assert "running=true" in repr_str

    def test_repr_includes_host_port(self, gateway_proxy):
        """Test __repr__ includes host and port."""
        repr_str = repr(gateway_proxy)
        assert gateway_proxy.host in repr_str
        assert str(gateway_proxy.port) in repr_str


class TestGatewayModeContextManager:
    """Tests for context manager in gateway mode."""

    def test_gateway_context_manager(self, gateway_config):
        """Test using gateway proxy as context manager."""
        import keeper_pam_connections

        with keeper_pam_connections.keeperdb_proxy(gateway_config) as proxy:
            assert proxy.is_running()
            assert proxy.gateway_mode is True

        assert not proxy.is_running()

    def test_gateway_context_manager_on_exception(self, gateway_config):
        """Test gateway proxy cleanup on exception."""
        import keeper_pam_connections

        proxy_ref = None
        try:
            with keeper_pam_connections.keeperdb_proxy(gateway_config) as proxy:
                proxy_ref = proxy
                assert proxy.gateway_mode is True
                raise ValueError("Test exception")
        except ValueError:
            pass

        assert proxy_ref is not None
        assert not proxy_ref.is_running()
