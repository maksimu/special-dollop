"""Shared pytest fixtures for keeper_pam_connections tests.

This module provides common fixtures used across all test files:
- Port allocation (thread-safe unique ports)
- Configuration builders (minimal, gateway, postgresql)
- Proxy lifecycle helpers
"""

import threading

import pytest

# Thread-safe port allocator
# Start at 14000 to avoid conflicts with existing tests (13310-13330)
_port_lock = threading.Lock()
_next_port = 14000


@pytest.fixture
def unique_port():
    """Get a unique port number for this test.

    Thread-safe port allocation starting at 14000.
    Each call returns the next available port.
    """
    global _next_port
    with _port_lock:
        port = _next_port
        _next_port += 1
    return port


@pytest.fixture
def minimal_config(unique_port):
    """Minimal valid configuration for MySQL proxy.

    Returns a dict with all required fields for settings mode.
    """
    return {
        "database_type": "mysql",
        "target_host": "localhost",
        "target_port": 3306,
        "username": "testuser",
        "password": "testpass",
        "listen_port": unique_port,
    }


@pytest.fixture
def postgresql_config(unique_port):
    """Configuration for PostgreSQL proxy."""
    return {
        "database_type": "postgresql",
        "target_host": "localhost",
        "target_port": 5432,
        "username": "pguser",
        "password": "pgpass",
        "listen_port": unique_port,
    }


@pytest.fixture
def gateway_config(unique_port):
    """Configuration for gateway mode (no credentials).

    Gateway mode is activated when no username/password is provided.
    """
    return {
        "listen_host": "127.0.0.1",
        "listen_port": unique_port,
    }


@pytest.fixture
def config_with_session(unique_port):
    """Configuration with session limits."""
    return {
        "database_type": "mysql",
        "target_host": "localhost",
        "target_port": 3306,
        "username": "testuser",
        "password": "testpass",
        "listen_port": unique_port,
        "session_config": {
            "max_duration_secs": 3600,
            "idle_timeout_secs": 300,
            "max_queries": 1000,
            "max_connections": 5,
        },
    }


@pytest.fixture
def proxy(minimal_config):
    """Create and auto-cleanup a proxy instance.

    Yields a running proxy, then stops it after the test.
    """
    import keeper_pam_connections

    p = keeper_pam_connections.keeperdb_proxy(minimal_config)
    yield p
    p.stop()


@pytest.fixture
def gateway_proxy(gateway_config):
    """Create and auto-cleanup a gateway mode proxy instance."""
    import keeper_pam_connections

    p = keeper_pam_connections.keeperdb_proxy(gateway_config)
    yield p
    p.stop()
