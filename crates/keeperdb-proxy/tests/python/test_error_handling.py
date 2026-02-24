"""Tests for error handling and validation.

These tests verify that invalid configurations produce appropriate
Python exceptions with clear error messages.
"""

import pytest


class TestMissingRequiredFields:
    """Tests for missing required configuration fields."""

    def test_empty_config_starts_gateway_mode(self):
        """Test that empty config starts proxy in gateway mode."""
        import keeper_pam_connections

        # Empty config = gateway mode (no credentials needed)
        proxy = keeper_pam_connections.keeperdb_proxy({})
        try:
            assert proxy.is_running()
            assert proxy.gateway_mode is True
        finally:
            proxy.stop()

    def test_error_missing_database_type(self, unique_port):
        """Test error when database_type is missing in settings mode."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

        assert "database_type" in str(exc_info.value).lower()

    def test_error_missing_target_host(self, unique_port):
        """Test error when target_host is missing in settings mode."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

        assert "target_host" in str(exc_info.value).lower()

    def test_error_missing_username(self, unique_port):
        """Test error when username is missing in settings mode."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "password": "testpass",
                "listen_port": unique_port,
            })

        # Should mention username or credentials
        error_msg = str(exc_info.value).lower()
        assert "username" in error_msg or "credentials" in error_msg

    def test_error_missing_password(self, unique_port):
        """Test error when password is missing in settings mode."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "listen_port": unique_port,
            })

        # Should mention password or credentials
        error_msg = str(exc_info.value).lower()
        assert "password" in error_msg or "credentials" in error_msg


class TestInvalidDatabaseType:
    """Tests for invalid database type values."""

    def test_error_invalid_database_type(self, unique_port):
        """Test error for unsupported database type."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "oracle",
                "target_host": "localhost",
                "target_port": 1521,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

        error_msg = str(exc_info.value).lower()
        assert "oracle" in error_msg
        # Should suggest valid options
        assert "mysql" in error_msg or "postgresql" in error_msg

    def test_error_database_type_wrong_type(self, unique_port):
        """Test error when database_type is wrong type."""
        import keeper_pam_connections

        with pytest.raises((ValueError, TypeError)):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": 123,  # Should be string
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

    def test_error_database_type_empty_string(self, unique_port):
        """Test error when database_type is empty string."""
        import keeper_pam_connections

        with pytest.raises(ValueError):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })


class TestInvalidPortValues:
    """Tests for invalid port configurations."""

    def test_error_negative_target_port(self, unique_port):
        """Test error for negative target port."""
        import keeper_pam_connections

        with pytest.raises((ValueError, OverflowError)):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": -1,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

    def test_error_negative_listen_port(self):
        """Test error for negative listen port."""
        import keeper_pam_connections

        with pytest.raises((ValueError, OverflowError)):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": -1,
            })

    def test_error_port_too_high(self, unique_port):
        """Test error for port > 65535."""
        import keeper_pam_connections

        with pytest.raises((ValueError, OverflowError)):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 70000,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })


class TestInvalidTLSConfig:
    """Tests for invalid TLS configuration."""

    def test_error_tls_cert_without_key(self, unique_port):
        """Test error when TLS cert provided without key."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
                "tls_config": {
                    "enabled": True,
                    "client_cert_path": "/path/to/cert.pem",
                    # Missing client_key_path
                },
            })

        error_msg = str(exc_info.value).lower()
        assert "key" in error_msg or "client" in error_msg

    def test_error_tls_key_without_cert(self, unique_port):
        """Test error when TLS key provided without cert."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
                "tls_config": {
                    "enabled": True,
                    "client_key_path": "/path/to/key.pem",
                    # Missing client_cert_path
                },
            })

        error_msg = str(exc_info.value).lower()
        assert "cert" in error_msg or "client" in error_msg


class TestInvalidSessionConfig:
    """Tests for invalid session configuration."""

    def test_session_config_wrong_type(self, unique_port):
        """Test error when session_config is wrong type."""
        import keeper_pam_connections

        with pytest.raises((ValueError, TypeError)):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
                "session_config": "invalid",  # Should be dict
            })

    def test_session_config_negative_duration(self, unique_port):
        """Test handling of negative duration values."""
        import keeper_pam_connections

        # Negative values might be silently ignored or error
        # This test documents the current behavior
        try:
            proxy = keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
                "session_config": {
                    "max_duration_secs": -1,
                },
            })
            proxy.stop()
            # If we get here, negative values are handled gracefully
        except (ValueError, OverflowError):
            # Negative values cause error - also acceptable
            pass


class TestErrorMessageClarity:
    """Tests that error messages are clear and actionable."""

    def test_error_mentions_field_name(self, unique_port):
        """Test that errors mention the problematic field."""
        import keeper_pam_connections

        with pytest.raises(ValueError) as exc_info:
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "invalid_db",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })

        # Error should mention what was wrong
        error_msg = str(exc_info.value)
        assert len(error_msg) > 10  # Not just a generic error

    def test_error_is_valueerror(self, unique_port):
        """Test that config errors are ValueError."""
        import keeper_pam_connections

        with pytest.raises(ValueError):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "not_a_database",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
            })


class TestEdgeCases:
    """Tests for edge case inputs."""

    def test_none_value_for_optional_field(self, unique_port):
        """Test that None for optional fields raises TypeError (PyO3 behavior)."""
        import keeper_pam_connections

        # PyO3 doesn't allow None where a string is expected
        # This documents the actual behavior
        with pytest.raises(TypeError):
            keeper_pam_connections.keeperdb_proxy({
                "database_type": "mysql",
                "target_host": "localhost",
                "target_port": 3306,
                "username": "testuser",
                "password": "testpass",
                "listen_port": unique_port,
                "database": None,  # Causes TypeError in PyO3
            })

    def test_empty_string_username_accepted(self, unique_port):
        """Test that empty string username is accepted (valid in some scenarios)."""
        import keeper_pam_connections

        # Empty string username is technically allowed - the proxy will
        # pass it through to the database which may reject it
        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "",
            "password": "testpass",
            "listen_port": unique_port,
        })
        try:
            assert proxy.is_running()
        finally:
            proxy.stop()

    def test_whitespace_only_password(self, unique_port):
        """Test that whitespace-only password is accepted (valid in some DBs)."""
        import keeper_pam_connections

        # Whitespace passwords should be allowed - some DBs support them
        proxy = keeper_pam_connections.keeperdb_proxy({
            "database_type": "mysql",
            "target_host": "localhost",
            "target_port": 3306,
            "username": "testuser",
            "password": "   ",
            "listen_port": unique_port,
        })
        try:
            assert proxy.is_running()
        finally:
            proxy.stop()
