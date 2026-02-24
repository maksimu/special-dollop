"""Integration tests for keeper_pam_connections Python bindings.

These tests verify that the PyO3 bindings work correctly for:
- Module import and version
- Proxy lifecycle (start, stop)
- Context manager support
- Configuration parsing
- Error handling
"""

import socket
import time

import pytest


def test_module_import():
    """Test that the module can be imported."""
    import keeper_pam_connections

    assert hasattr(keeper_pam_connections, "keeperdb_proxy")
    assert hasattr(keeper_pam_connections, "ProxyHandle")
    assert hasattr(keeper_pam_connections, "__version__")


def test_module_version():
    """Test that version is correctly exposed."""
    import keeper_pam_connections

    version = keeper_pam_connections.__version__
    assert isinstance(version, str)
    assert len(version) > 0
    # Should be semver format
    parts = version.split(".")
    assert len(parts) >= 2


def test_start_proxy_minimal():
    """Test starting proxy with minimal configuration."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13310,
        }
    )

    try:
        assert proxy.host == "127.0.0.1"
        assert proxy.port == 13310
        assert proxy.is_running()
    finally:
        proxy.stop()

    assert not proxy.is_running()


def test_start_proxy_postgresql():
    """Test starting proxy for PostgreSQL."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "postgresql",
            "target_host": "localhost",
            "target_port": 5432,
            "username": "pguser",
            "password": "pgpass",
            "listen_port": 13311,
        }
    )

    try:
        assert proxy.is_running()
    finally:
        proxy.stop()


def test_start_proxy_with_database():
    """Test starting proxy with database name."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "database": "mydb",
            "listen_port": 13312,
        }
    )

    try:
        assert proxy.is_running()
    finally:
        proxy.stop()


def test_start_proxy_with_session_config():
    """Test starting proxy with session configuration."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13313,
            "session_config": {
                "max_duration_secs": 3600,
                "idle_timeout_secs": 300,
                "max_queries": 1000,
                "max_connections": 2,
                "grace_period_ms": 500,
            },
        }
    )

    try:
        assert proxy.is_running()
    finally:
        proxy.stop()


def test_start_proxy_with_tls_config():
    """Test starting proxy with TLS configuration."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13314,
            "tls_config": {
                "enabled": False,  # Disabled for test
                "verify_mode": "verify",
            },
        }
    )

    try:
        assert proxy.is_running()
    finally:
        proxy.stop()


def test_start_proxy_with_auth_method():
    """Test specifying auth method."""
    import keeper_pam_connections

    # Test MySQL caching_sha2
    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "auth_method": "caching_sha2",
            "listen_port": 13315,
        }
    )

    try:
        assert proxy.is_running()
    finally:
        proxy.stop()


def test_context_manager():
    """Test using proxy as context manager."""
    import keeper_pam_connections

    with keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13316,
        }
    ) as proxy:
        assert proxy.is_running()

    # After context, proxy should be stopped
    assert not proxy.is_running()


def test_context_manager_with_exception():
    """Test context manager cleans up on exception."""
    import keeper_pam_connections

    proxy_ref = None
    try:
        with keeper_pam_connections.keeperdb_proxy(
            {
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": 13317,
            }
        ) as proxy:
            proxy_ref = proxy
            assert proxy.is_running()
            raise ValueError("Test exception")
    except ValueError:
        pass

    # Proxy should still be stopped even after exception
    assert proxy_ref is not None
    assert not proxy_ref.is_running()


def test_proxy_repr():
    """Test ProxyHandle string representation."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13318,
        }
    )

    try:
        repr_str = repr(proxy)
        assert "ProxyHandle" in repr_str
        assert "127.0.0.1" in repr_str
        assert "13318" in repr_str
        assert "running=true" in repr_str
    finally:
        proxy.stop()


def test_stop_idempotent():
    """Test that calling stop multiple times is safe."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13319,
        }
    )

    # Stop multiple times should not raise
    proxy.stop()
    proxy.stop()
    proxy.stop()

    assert not proxy.is_running()


def test_error_missing_database_type():
    """Test error when database_type is missing."""
    import keeper_pam_connections

    with pytest.raises(ValueError) as exc_info:
        keeper_pam_connections.keeperdb_proxy(
            {
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
            }
        )

    assert "database_type" in str(exc_info.value)


def test_error_missing_target_host():
    """Test error when target_host is missing."""
    import keeper_pam_connections

    with pytest.raises(ValueError) as exc_info:
        keeper_pam_connections.keeperdb_proxy(
            {
                "database_type": "mysql",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
            }
        )

    assert "target_host" in str(exc_info.value)


def test_missing_credentials_starts_gateway_mode():
    """Test that missing credentials starts proxy in gateway mode."""
    import keeper_pam_connections

    # No username/password = gateway mode, even with other settings
    # The db_type, host, port are ignored in gateway mode
    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
        }
    )
    try:
        assert proxy.is_running()
        assert proxy.gateway_mode is True
    finally:
        proxy.stop()


def test_error_invalid_database_type():
    """Test error for invalid database_type."""
    import keeper_pam_connections

    with pytest.raises(ValueError) as exc_info:
        keeper_pam_connections.keeperdb_proxy(
            {
                "database_type": "oracle",
                "target_host": "localhost",
                "target_port": 1521,
                "username": "testuser",
                "password": "testpass",
            }
        )

    assert "oracle" in str(exc_info.value).lower()
    assert "mysql" in str(exc_info.value).lower() or "postgresql" in str(
        exc_info.value
    ).lower()


def test_error_invalid_tls_config():
    """Test error for invalid TLS configuration."""
    import keeper_pam_connections

    with pytest.raises(ValueError) as exc_info:
        keeper_pam_connections.keeperdb_proxy(
            {
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": 13320,
                "tls_config": {
                    "enabled": True,
                    "client_cert_path": "/path/to/cert",
                    # Missing client_key_path - should fail validation
                },
            }
        )

    assert "client" in str(exc_info.value).lower()


def test_port_binding():
    """Test that the proxy actually binds to the port."""
    import keeper_pam_connections

    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13321,
        }
    )

    try:
        # Give the proxy a moment to bind
        time.sleep(0.2)

        # Try to connect to the proxy port
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        result = sock.connect_ex(("127.0.0.1", 13321))
        sock.close()

        # Connection should succeed (result == 0)
        assert result == 0, f"Failed to connect to proxy port: {result}"
    finally:
        proxy.stop()


def test_database_type_case_insensitive():
    """Test that database_type parsing is case insensitive."""
    import keeper_pam_connections

    # Test uppercase
    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "MYSQL",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13322,
        }
    )
    proxy.stop()

    # Test mixed case
    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "PostgreSQL",
            "target_host": "localhost",
            "target_port": 5432,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13323,
        }
    )
    proxy.stop()

    # Test 'postgres' alias
    proxy = keeper_pam_connections.keeperdb_proxy(
        {
            "database_type": "postgres",
            "target_host": "localhost",
            "target_port": 5432,
            "username": "testuser",
            "password": "testpass",
            "listen_port": 13324,
        }
    )
    proxy.stop()


def test_verify_mode_aliases():
    """Test various TLS verify mode aliases."""
    import keeper_pam_connections

    verify_modes = ["verify", "full", "verify_ca", "ca", "none", "disabled"]

    port = 13325
    for mode in verify_modes:
        proxy = keeper_pam_connections.keeperdb_proxy(
            {
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": port,
                "tls_config": {
                    "enabled": False,
                    "verify_mode": mode,
                },
            }
        )
        proxy.stop()
        port += 1


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
