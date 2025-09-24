#!/usr/bin/env python3
"""
Test script to verify the metrics system integration with Python.
Tests the Gateway Performance Metrics System implementation.
"""

import json
import pytest
import time
from keeper_pam_webrtc_rs import PyTubeRegistry


class TestMetricsSystem:
    """Test the metrics system integration."""

    def test_system_stats(self):
        """Test get_system_stats() method."""
        registry = PyTubeRegistry()
        try:
            system_stats = registry.get_system_stats()

            # Verify expected keys are present
            assert "uptime_seconds" in system_stats
            assert "active_connection_count" in system_stats
            assert "active_tube_count" in system_stats

            # Verify types
            assert isinstance(system_stats["uptime_seconds"], int)
            assert isinstance(system_stats["active_connection_count"], int)
            assert isinstance(system_stats["active_tube_count"], int)

            # Verify reasonable values
            assert system_stats["uptime_seconds"] >= 0
            assert system_stats["active_connection_count"] >= 0
            assert system_stats["active_tube_count"] >= 0

        finally:
            registry.cleanup_all()

    def test_aggregated_metrics(self):
        """Test get_aggregated_metrics() method."""
        registry = PyTubeRegistry()
        try:
            metrics = registry.get_aggregated_metrics()

            # Verify expected keys are present
            expected_keys = [
                "timestamp", "active_connections", "active_tubes",
                "avg_system_rtt_ms", "avg_packet_loss", "total_message_throughput",
                "total_bandwidth", "excellent_connections", "good_connections",
                "fair_connections", "poor_connections", "total_alerts",
                "critical_alerts", "warning_alerts", "memory_usage_bytes",
                "cpu_utilization", "network_utilization"
            ]

            for key in expected_keys:
                assert key in metrics, f"Missing key: {key}"

            # Verify types for key metrics
            assert isinstance(metrics["active_connections"], int)
            assert isinstance(metrics["active_tubes"], int)
            assert isinstance(metrics["avg_system_rtt_ms"], float)
            assert isinstance(metrics["avg_packet_loss"], float)

            # Verify reasonable values
            assert metrics["active_connections"] >= 0
            assert metrics["active_tubes"] >= 0
            assert metrics["avg_packet_loss"] >= 0.0

        finally:
            registry.cleanup_all()

    def test_metrics_json_export(self):
        """Test export_metrics_json() method."""
        registry = PyTubeRegistry()
        try:
            # Clean up any existing connections first
            registry.cleanup_all()
            # Clear metrics connections from previous tests
            registry.clear_metrics_connections()
            # Brief wait for cleanup to complete
            time.sleep(0.1)

            # Get JSON export
            metrics_json = registry.export_metrics_json()

            # Verify it's valid JSON
            metrics_data = json.loads(metrics_json)

            # Verify expected top-level structure
            expected_keys = ["timestamp", "connections", "aggregated", "alerts"]
            for key in expected_keys:
                assert key in metrics_data, f"Missing top-level key: {key}"

            # Verify types
            assert isinstance(metrics_data["connections"], dict)
            assert isinstance(metrics_data["aggregated"], dict)
            assert isinstance(metrics_data["alerts"], list)

            # Since we cleared metrics connections, connections should be empty
            connection_count = len(metrics_data["connections"])
            assert connection_count == 0, f"Expected 0 connections after clearing metrics, got {connection_count}"

        finally:
            registry.cleanup_all()

    def test_active_alerts(self):
        """Test get_active_alerts() method."""
        registry = PyTubeRegistry()
        try:
            alerts = registry.get_active_alerts()

            # Should return a list
            assert isinstance(alerts, list)

            # Since no connections exist, should be empty
            assert len(alerts) == 0

        finally:
            registry.cleanup_all()

    def test_live_stats_nonexistent_connection(self):
        """Test get_live_stats() with non-existent connection."""
        registry = PyTubeRegistry()
        try:
            live_stats = registry.get_live_stats("non-existent-connection")

            # Should return None for non-existent connections
            assert live_stats is None

        finally:
            registry.cleanup_all()

    def test_connection_health_nonexistent_tube(self):
        """Test get_connection_health() with non-existent tube."""
        registry = PyTubeRegistry()
        try:
            health = registry.get_connection_health("non-existent-tube")

            # Should return None for non-existent tubes
            assert health is None

        finally:
            registry.cleanup_all()

    def test_metrics_methods_exist(self):
        """Test that all expected metrics methods exist on PyTubeRegistry."""
        registry = PyTubeRegistry()
        try:
            # List of all metrics-related methods that should exist
            expected_methods = [
                "get_live_stats",
                "get_connection_health",
                "export_metrics_json",
                "get_aggregated_metrics",
                "get_active_alerts",
                "get_system_stats"
            ]

            for method_name in expected_methods:
                assert hasattr(registry, method_name), f"Missing method: {method_name}"
                assert callable(getattr(registry, method_name)), f"Method {method_name} is not callable"

        finally:
            registry.cleanup_all()

    def test_metrics_integration_with_basic_functionality(self):
        """Test that metrics integration doesn't break basic functionality."""
        registry = PyTubeRegistry()
        try:
            # Test basic registry operations still work
            has_tubes = registry.has_active_tubes()
            assert isinstance(has_tubes, bool)
            assert has_tubes == False  # No tubes created yet

            count = registry.active_tube_count()
            assert isinstance(count, int)
            assert count == 0

            # Test server mode
            registry.set_server_mode(True)
            is_server = registry.is_server_mode()
            assert isinstance(is_server, bool)
            assert is_server == True

            # Verify metrics still work after server mode changes
            system_stats = registry.get_system_stats()
            assert "active_tube_count" in system_stats
            assert system_stats["active_tube_count"] == 0

        finally:
            registry.cleanup_all()


def test_metrics_system_quick():
    """Quick integration test for CI/CD."""
    registry = PyTubeRegistry()
    try:
        # Quick test that all main methods work
        system_stats = registry.get_system_stats()
        assert "uptime_seconds" in system_stats

        metrics = registry.get_aggregated_metrics()
        assert "active_connections" in metrics

        alerts = registry.get_active_alerts()
        assert isinstance(alerts, list)

        metrics_json = registry.export_metrics_json()
        json.loads(metrics_json)  # Just verify it's valid JSON

        print("✅ Quick metrics system test passed!")

    finally:
        registry.cleanup_all()


if __name__ == "__main__":
    print("🚀 Running metrics system tests...")
    test_metrics_system_quick()
    print("🎉 All metrics system tests completed!")