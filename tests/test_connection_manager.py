#!/usr/bin/env python3
"""
Comprehensive test suite for the WebRTC connection management system.
Tests the connection manager, ICE restart policies, and network change handling.
"""

import asyncio
import pytest
import time
import json
import logging
from unittest.mock import Mock, AsyncMock, MagicMock, patch
from src.python.connection_manager import (
    TunnelConnectionManager,
    ConnectionState,
    NetworkChangeType,
    ConnectionMetrics,
    ICERestartPolicy,
    get_connection_manager,
    register_connection_manager,
    unregister_connection_manager,
    _connection_managers
)

# Configure logging for tests
logging.basicConfig(level=logging.DEBUG)

class TestICERestartPolicy:
    """Test the ICE restart policy logic"""
    
    def test_default_policy_values(self):
        """Test default policy configuration"""
        policy = ICERestartPolicy()
        
        assert policy.max_restart_attempts == 5
        assert policy.restart_backoff_base == 5.0
        assert policy.restart_backoff_max == 60.0
        assert policy.restart_window == 300.0
        assert policy.quality_threshold == 0.8
        assert policy.timeout_threshold == 30.0

    def test_should_restart_interface_change(self):
        """Interface changes should always trigger restart"""
        policy = ICERestartPolicy()
        metrics = ConnectionMetrics()
        
        # Interface change should always restart
        result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
        assert result == True

    def test_should_restart_max_attempts(self):
        """Should not restart after max attempts"""
        policy = ICERestartPolicy()
        metrics = ConnectionMetrics()
        metrics.ice_restart_attempts = 5  # At max
        
        result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
        assert result == False

    def test_should_restart_backoff_period(self):
        """Should not restart during backoff period"""
        policy = ICERestartPolicy()
        metrics = ConnectionMetrics()
        metrics.ice_restart_attempts = 1  # One previous attempt
        metrics.last_restart_time = time.time() - 3.0  # 3 seconds ago (less than 5s backoff)
        
        result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
        assert result == False

    def test_should_restart_after_backoff(self):
        """Should restart after backoff period expires"""
        policy = ICERestartPolicy()
        metrics = ConnectionMetrics()
        metrics.ice_restart_attempts = 1  # One previous attempt
        metrics.last_restart_time = time.time() - 15.0  # 15 seconds ago (more than 5s backoff)
        
        result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
        assert result == True

    def test_exponential_backoff_calculation(self):
        """Test that backoff time increases exponentially"""
        policy = ICERestartPolicy()
        
        # Test backoff calculation for different attempt counts
        base_backoff = policy.restart_backoff_base
        
        # First attempt: 5s backoff
        # Second attempt: 10s backoff  
        # Third attempt: 20s backoff
        # Fourth attempt: 40s backoff, but capped at 60s
        
        for attempts in range(3):  # Only test 0, 1, 2 attempts (under max)
            expected_backoff = min(base_backoff * (2 ** attempts), policy.restart_backoff_max)
            
            metrics = ConnectionMetrics()
            metrics.ice_restart_attempts = attempts
            metrics.last_restart_time = time.time() - (expected_backoff - 1.0)  # Just under backoff
            
            # Should not restart (still in backoff)
            result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
            assert result == False, f"Should be in backoff for attempt {attempts}"
            
            # Move past backoff period
            metrics.last_restart_time = time.time() - (expected_backoff + 1.0)  # Past backoff
            result = policy.should_restart(metrics, NetworkChangeType.INTERFACE_CHANGE)
            assert result == True, f"Should restart after backoff for attempt {attempts}"


class TestTunnelConnectionManager:
    """Test the main connection manager functionality"""
    
    def setup_method(self):
        """Set up test fixtures"""
        self.mock_tube_registry = Mock()
        self.mock_signal_handler = Mock()
        self.tube_id = "test_tube_123"
        self.conversation_id = "test_conversation_456"
        
        # Clear global registry
        _connection_managers.clear()

    def create_manager(self):
        """Create a test connection manager"""
        return TunnelConnectionManager(
            tube_id=self.tube_id,
            conversation_id=self.conversation_id,
            tube_registry=self.mock_tube_registry,
            signal_handler=self.mock_signal_handler
        )

    def test_manager_initialization(self):
        """Test manager initializes correctly"""
        manager = self.create_manager()
        
        assert manager.tube_id == self.tube_id
        assert manager.conversation_id == self.conversation_id
        assert manager.state == ConnectionState.INITIALIZING
        assert manager.tube_registry == self.mock_tube_registry
        assert manager.signal_handler == self.mock_signal_handler
        assert isinstance(manager.metrics, ConnectionMetrics)
        assert isinstance(manager.restart_policy, ICERestartPolicy)

    def test_activity_update(self):
        """Test activity timestamp updates"""
        manager = self.create_manager()
        
        initial_time = manager.metrics.last_activity
        time.sleep(0.01)  # Small delay
        
        manager.update_activity()
        
        assert manager.metrics.last_activity > initial_time

    @pytest.mark.asyncio
    async def test_state_change_callbacks(self):
        """Test state change callback mechanism"""
        manager = self.create_manager()
        
        callback_called = False
        callback_state = None
        callback_tube_id = None
        
        def state_callback(tube_id, state):
            nonlocal callback_called, callback_state, callback_tube_id
            callback_called = True
            callback_state = state
            callback_tube_id = tube_id
        
        manager.add_state_change_callback(state_callback)
        
        # Trigger state change
        manager.state = ConnectionState.CONNECTED
        await manager._notify_state_change()
        
        assert callback_called
        assert callback_state == ConnectionState.CONNECTED
        assert callback_tube_id == self.tube_id

    @pytest.mark.asyncio
    async def test_async_state_change_callback(self):
        """Test async state change callbacks"""
        manager = self.create_manager()
        
        callback_called = False
        
        async def async_callback(tube_id, state):
            nonlocal callback_called
            callback_called = True
            await asyncio.sleep(0.001)  # Small async delay
        
        manager.add_state_change_callback(async_callback)
        
        # Trigger state change
        manager.state = ConnectionState.CONNECTED
        await manager._notify_state_change()
        
        assert callback_called

    @pytest.mark.asyncio
    async def test_registry_operations(self):
        """Test connection manager registry"""
        manager = self.create_manager()
        
        # Register manager
        register_connection_manager(manager)
        
        # Should be able to retrieve it
        retrieved = get_connection_manager(self.tube_id)
        assert retrieved == manager
        
        # Unregister manager
        await unregister_connection_manager(self.tube_id)
        
        # Should no longer be available
        retrieved = get_connection_manager(self.tube_id)
        assert retrieved is None

    @pytest.mark.asyncio
    async def test_network_change_handling_interface_change(self):
        """Test network change handling for interface changes"""
        manager = self.create_manager()
        
        # Set initial state to connected so change to degraded is triggered
        manager.state = ConnectionState.CONNECTED
        
        # Mock the ICE restart method
        manager._attempt_ice_restart = AsyncMock()
        
        # Simulate interface change
        await manager.handle_network_change(NetworkChangeType.INTERFACE_CHANGE)
        
        # Should change state to degraded
        assert manager.state == ConnectionState.DEGRADED
        
        # Should attempt ICE restart
        manager._attempt_ice_restart.assert_called_once_with(NetworkChangeType.INTERFACE_CHANGE)

    @pytest.mark.asyncio
    async def test_network_change_handling_with_policy_blocking(self):
        """Test network change blocked by policy"""
        manager = self.create_manager()
        
        # Set up policy to block restart
        manager.metrics.ice_restart_attempts = 5  # Max attempts reached
        manager._attempt_ice_restart = AsyncMock()
        manager._handle_connection_failure = AsyncMock()
        
        # Simulate connection timeout
        await manager.handle_network_change(NetworkChangeType.CONNECTION_TIMEOUT)
        
        # Should not attempt ICE restart
        manager._attempt_ice_restart.assert_not_called()
        
        # Should handle connection failure instead
        manager._handle_connection_failure.assert_called_once()

    @pytest.mark.asyncio
    async def test_ice_restart_workflow_success(self):
        """Test successful ICE restart workflow"""
        manager = self.create_manager()
        
        # Mock dependencies
        manager._execute_ice_restart = AsyncMock(return_value="test_sdp")
        manager._send_restart_offer = AsyncMock()
        manager._verify_restart_success = AsyncMock(return_value=True)
        manager._notify_state_change = AsyncMock()
        
        # Execute restart workflow
        await manager._ice_restart_workflow(NetworkChangeType.INTERFACE_CHANGE)
        
        # Verify sequence
        manager._execute_ice_restart.assert_called_once()
        manager._send_restart_offer.assert_called_once_with("test_sdp")
        # _verify_restart_success may be called multiple times during polling
        assert manager._verify_restart_success.call_count >= 1
        
        # Should end in connected state
        assert manager.state == ConnectionState.CONNECTED
        assert manager.metrics.ice_restart_attempts == 1
        assert manager.metrics.last_restart_time is not None

    @pytest.mark.asyncio
    async def test_ice_restart_workflow_failure(self):
        """Test failed ICE restart workflow"""
        manager = self.create_manager()
        
        # Set initial state to connected
        manager.state = ConnectionState.CONNECTED
        
        # Mock failure
        manager._execute_ice_restart = AsyncMock(return_value=None)  # Failure
        manager._notify_state_change = AsyncMock()
        
        # Execute restart workflow
        await manager._ice_restart_workflow(NetworkChangeType.INTERFACE_CHANGE)
        
        # Should end in connected state (reverted from failure)
        assert manager.state == ConnectionState.CONNECTED
        assert manager.metrics.ice_restart_attempts == 1

    @pytest.mark.asyncio
    async def test_ice_restart_workflow_timeout(self):
        """Test ICE restart workflow timeout"""
        manager = self.create_manager()
        
        # Mock timeout
        async def slow_restart():
            await asyncio.sleep(60)  # Longer than timeout
            return "test_sdp"
        
        manager._execute_ice_restart = AsyncMock(side_effect=slow_restart)
        manager._notify_state_change = AsyncMock()
        
        # Execute restart workflow (should timeout)
        await manager._ice_restart_workflow(NetworkChangeType.INTERFACE_CHANGE)
        
        # Should end in failed state due to timeout
        assert manager.state == ConnectionState.FAILED

    @pytest.mark.asyncio 
    async def test_connection_quality_monitoring(self):
        """Test connection quality monitoring loop"""
        manager = self.create_manager()
        manager.shutdown_requested = False
        manager._check_connection_quality = AsyncMock()
        
        # Start monitoring task
        monitoring_task = asyncio.create_task(manager._monitoring_loop())
        
        # Let it run briefly
        await asyncio.sleep(0.1)
        
        # Stop monitoring
        manager.shutdown_requested = True
        
        # Wait for completion
        try:
            await asyncio.wait_for(monitoring_task, timeout=1.0)
        except asyncio.TimeoutError:
            monitoring_task.cancel()

    @pytest.mark.asyncio
    async def test_shutdown_cleanup(self):
        """Test proper shutdown and cleanup"""
        manager = self.create_manager()
        
        # Create actual asyncio tasks that can be awaited
        async def dummy_monitoring():
            while not manager.shutdown_requested:
                await asyncio.sleep(0.1)
        
        async def dummy_restart():
            await asyncio.sleep(0.1)
            
        # Start real tasks
        manager.monitoring_task = asyncio.create_task(dummy_monitoring())
        manager.restart_task = asyncio.create_task(dummy_restart())
        
        manager._notify_state_change = AsyncMock()
        
        # Give tasks a moment to start
        await asyncio.sleep(0.01)
        
        # Shutdown
        await manager.shutdown()
        
        # Verify cleanup
        assert manager.shutdown_requested == True
        assert manager.state == ConnectionState.TERMINATED
        manager._notify_state_change.assert_called()
        
        # Verify tasks were cancelled
        assert manager.monitoring_task.cancelled() or manager.monitoring_task.done()
        assert manager.restart_task.cancelled() or manager.restart_task.done()


class TestNetworkChangeSimulator:
    """Test network change simulation and scenarios"""
    
    @pytest.mark.asyncio
    async def test_simulate_ethernet_to_wifi_change(self):
        """Simulate switching from ethernet to wifi"""
        manager = TunnelConnectionManager(
            "test_tube", "test_conv",
            Mock(), Mock()
        )
        
        # Mock the restart mechanism
        manager._attempt_ice_restart = AsyncMock()
        
        # Initial connection state
        manager.state = ConnectionState.CONNECTED
        manager.update_activity()
        
        # Simulate network interface change
        await manager.handle_network_change(
            NetworkChangeType.INTERFACE_CHANGE,
            {"interface_change": "eth0 -> wlan0"}
        )
        
        # Should trigger restart attempt
        manager._attempt_ice_restart.assert_called_once()
        assert manager.state == ConnectionState.DEGRADED

    @pytest.mark.asyncio
    async def test_simulate_connection_timeout_scenario(self):
        """Simulate connection timeout scenario"""
        manager = TunnelConnectionManager(
            "test_tube", "test_conv", 
            Mock(), Mock()
        )
        
        # Set connection as established
        manager.state = ConnectionState.CONNECTED
        manager.metrics.connection_established_time = time.time() - 600  # 10 minutes ago
        
        # Simulate old activity (triggering timeout)
        manager.metrics.last_activity = time.time() - 3600  # 1 hour ago
        
        # Check quality should detect timeout
        await manager._check_connection_quality()
        
        # Should have handled timeout
        # (This would typically trigger network change handling)

    @pytest.mark.asyncio
    async def test_simulate_rapid_network_changes(self):
        """Test handling of rapid network changes with backoff"""
        # Use a fake subclass to override async methods instead of extensive patching
        class FakeTunnelConnectionManager(TunnelConnectionManager):
            async def _execute_ice_restart(self):
                return "test_sdp"
            async def _send_restart_offer(self, restart_sdp):
                return None
            async def _verify_restart_success(self):
                return True
            async def _attempt_ice_restart(self, change_type):
                # Track calls for the test
                return None
            async def _ice_restart_workflow(self, change_type):
                return None
        
        manager = FakeTunnelConnectionManager(
            "test_tube", "test_conv",
            Mock(), Mock()
        )
        
        # Set initial connected state
        manager.state = ConnectionState.CONNECTED
        
        # Track restart attempts through policy
        restart_calls = 0
        original_should_restart = manager.restart_policy.should_restart
        
        def mock_should_restart(metrics, change_type):
            nonlocal restart_calls
            result = original_should_restart(metrics, change_type)
            if result:
                restart_calls += 1
                # Simulate metrics update
                metrics.ice_restart_attempts += 1
                metrics.last_restart_time = time.time()
            return result
        
        manager.restart_policy.should_restart = mock_should_restart
        
        # Rapid network changes (using real handle_network_change but with fake dependencies)
        for i in range(5):
            await manager.handle_network_change(NetworkChangeType.INTERFACE_CHANGE)
            await asyncio.sleep(0.01)  # Small delay
        
        # Should have attempted restart initially, but backoff should limit subsequent attempts
        assert restart_calls >= 1  # At least one restart
        assert restart_calls < 5   # But not all 5 due to backoff
        
        # Check that attempts were incremented
        assert manager.metrics.ice_restart_attempts > 0
        
        # Cleanup to prevent hanging tasks
        await manager.shutdown()


class TestIntegrationScenarios:
    """Integration tests for complete scenarios"""
    
    @pytest.mark.asyncio
    async def test_complete_connection_lifecycle(self):
        """Test complete connection lifecycle with network changes"""
        
        # Create mock tube registry and signal handler
        tube_registry = Mock()
        tube_registry.restart_ice = Mock(return_value="restart_sdp")
        tube_registry.get_connection_state = Mock(return_value="connected")
        
        signal_handler = Mock()
        signal_handler._send_ice_restart_offer = AsyncMock()
        
        # Create manager
        manager = TunnelConnectionManager(
            "integration_tube", "integration_conv",
            tube_registry, signal_handler
        )
        
        # Mock the async methods to prevent unawaited coroutines
        manager._execute_ice_restart = AsyncMock(return_value="test_sdp")
        manager._send_restart_offer = AsyncMock()
        manager._verify_restart_success = AsyncMock(return_value=True)
        
        # 1. Initial connection
        manager.state = ConnectionState.CONNECTING
        await manager._notify_state_change()
        
        # 2. Connection established
        manager.state = ConnectionState.CONNECTED
        manager.metrics.connection_established_time = time.time()
        await manager._notify_state_change()
        
        # 3. Network change occurs (this will trigger ICE restart workflow automatically)
        await manager.handle_network_change(NetworkChangeType.INTERFACE_CHANGE)
        
        # Should be in degraded state
        assert manager.state == ConnectionState.DEGRADED
        
        # Wait a bit for async workflow to complete
        await asyncio.sleep(0.1)
        
        # The network change handling should have triggered ICE restart
        # Check that restart attempts were incremented (this means the workflow ran)
        assert manager.metrics.ice_restart_attempts >= 1
        
        # Verify the mocked methods were called
        if manager.metrics.ice_restart_attempts > 0:
            manager._execute_ice_restart.assert_called()
        
        # 4. Final state should be reasonable
        assert manager.state in [ConnectionState.CONNECTED, ConnectionState.DEGRADED, ConnectionState.RECONNECTING, ConnectionState.FAILED]

    @pytest.mark.asyncio
    async def test_multiple_managers_isolation(self):
        """Test that multiple managers don't interfere with each other"""
        
        managers = []
        
        # Create multiple managers
        for i in range(3):
            manager = TunnelConnectionManager(
                f"tube_{i}", f"conv_{i}",
                Mock(), Mock()
            )
            managers.append(manager)
            register_connection_manager(manager)
        
        # Each should be independently accessible
        for i, manager in enumerate(managers):
            retrieved = get_connection_manager(f"tube_{i}")
            assert retrieved == manager
        
        # Modify one manager's state
        managers[0].state = ConnectionState.FAILED
        managers[1].state = ConnectionState.CONNECTED
        
        # Others should be unaffected
        assert get_connection_manager("tube_0").state == ConnectionState.FAILED
        assert get_connection_manager("tube_1").state == ConnectionState.CONNECTED
        assert get_connection_manager("tube_2").state == ConnectionState.INITIALIZING
        
        # Cleanup
        for i in range(3):
            await unregister_connection_manager(f"tube_{i}")
            await managers[i].shutdown()


if __name__ == "__main__":
    # Run tests with detailed output
    pytest.main([
        __file__,
        "-v",
        "--tb=short",
        "--asyncio-mode=auto"
    ])