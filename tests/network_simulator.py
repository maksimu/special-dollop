#!/usr/bin/env python3
"""
Network change simulator for testing WebRTC connection resilience.
Simulates various network conditions and interface changes.
"""

import asyncio
import logging
import time
import random
from dataclasses import dataclass, field
from typing import List, Dict, Optional, Callable
from enum import Enum
from connection_manager import (
    TunnelConnectionManager, 
    NetworkChangeType, 
    ConnectionState,
    get_connection_manager
)

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class NetworkCondition(Enum):
    """Different network conditions to simulate"""
    STABLE = "stable"
    INTERFACE_CHANGE = "interface_change" 
    PACKET_LOSS = "packet_loss"
    HIGH_LATENCY = "high_latency"
    INTERMITTENT_CONNECTIVITY = "intermittent"
    CONNECTION_DROP = "connection_drop"
    IP_ADDRESS_CHANGE = "ip_change"

@dataclass
class NetworkEvent:
    """Represents a network event"""
    event_type: NetworkCondition
    duration: float  # seconds
    parameters: Dict = field(default_factory=dict)
    start_time: Optional[float] = None

@dataclass
class SimulationScenario:
    """A sequence of network events to simulate"""
    name: str
    events: List[NetworkEvent]
    description: str = ""

class NetworkSimulator:
    """
    Simulates various network conditions and changes for testing
    WebRTC connection resilience.
    """
    
    def __init__(self):
        self.running = False
        self.current_scenario = None
        self.event_callbacks: List[Callable] = []
        self.stats = {
            'events_triggered': 0,
            'connections_affected': 0,
            'successful_recoveries': 0,
            'failed_recoveries': 0
        }
        
    def add_event_callback(self, callback: Callable):
        """Add callback to be notified of network events"""
        self.event_callbacks.append(callback)
    
    async def simulate_scenario(self, scenario: SimulationScenario, tube_ids: List[str]):
        """
        Run a complete network simulation scenario
        """
        logger.info(f"Starting simulation scenario: {scenario.name}")
        logger.info(f"Description: {scenario.description}")
        logger.info(f"Affecting tubes: {tube_ids}")
        
        self.running = True
        self.current_scenario = scenario
        
        try:
            for event in scenario.events:
                if not self.running:
                    break
                    
                logger.info(f"Triggering network event: {event.event_type.value} for {event.duration}s")
                event.start_time = time.time()
                
                # Trigger event on all specified tubes
                for tube_id in tube_ids:
                    await self._trigger_network_event(tube_id, event)
                
                # Wait for event duration
                if event.duration > 0:
                    await asyncio.sleep(event.duration)
                
                # Event cleanup
                await self._cleanup_network_event(tube_ids, event)
                
        except Exception as e:
            logger.error(f"Error during simulation: {e}")
        finally:
            self.running = False
            logger.info(f"Simulation scenario completed: {scenario.name}")
            self._log_statistics()
    
    async def _trigger_network_event(self, tube_id: str, event: NetworkEvent):
        """Trigger a specific network event on a tube"""
        manager = get_connection_manager(tube_id)
        if not manager:
            logger.warning(f"No connection manager found for tube {tube_id}")
            return
            
        self.stats['events_triggered'] += 1
        self.stats['connections_affected'] += 1
        
        # Map event types to network change types
        network_change_map = {
            NetworkCondition.INTERFACE_CHANGE: NetworkChangeType.INTERFACE_CHANGE,
            NetworkCondition.IP_ADDRESS_CHANGE: NetworkChangeType.IP_ADDRESS_CHANGE,
            NetworkCondition.CONNECTION_DROP: NetworkChangeType.ICE_FAILURE,
            NetworkCondition.INTERMITTENT_CONNECTIVITY: NetworkChangeType.CONNECTION_TIMEOUT,
            NetworkCondition.PACKET_LOSS: NetworkChangeType.QUALITY_DEGRADATION,
            NetworkCondition.HIGH_LATENCY: NetworkChangeType.QUALITY_DEGRADATION,
        }
        
        if event.event_type in network_change_map:
            change_type = network_change_map[event.event_type]
            
            # Store initial state for recovery tracking
            initial_state = manager.state
            
            # Trigger the network change
            await manager.handle_network_change(change_type, {
                'simulation_event': event.event_type.value,
                'parameters': event.parameters,
                'initial_state': initial_state.value
            })
            
            # Notify callbacks
            for callback in self.event_callbacks:
                try:
                    if asyncio.iscoroutinefunction(callback):
                        await callback(tube_id, event, manager)
                    else:
                        callback(tube_id, event, manager)
                except Exception as e:
                    logger.error(f"Error in event callback: {e}")
        
        elif event.event_type == NetworkCondition.STABLE:
            # Simulate return to stable conditions
            logger.info(f"Restoring stable conditions for tube {tube_id}")
            manager.update_activity()
            
        else:
            logger.warning(f"Unknown event type: {event.event_type}")
    
    async def _cleanup_network_event(self, tube_ids: List[str], event: NetworkEvent):
        """Clean up after a network event"""
        if event.event_type == NetworkCondition.INTERMITTENT_CONNECTIVITY:
            # Restore connectivity after intermittent period
            for tube_id in tube_ids:
                manager = get_connection_manager(tube_id)
                if manager:
                    manager.update_activity()
                    logger.info(f"Restored connectivity for tube {tube_id}")
    
    def stop_simulation(self):
        """Stop the current simulation"""
        logger.info("Stopping network simulation")
        self.running = False
    
    def _log_statistics(self):
        """Log simulation statistics"""
        logger.info("=== Simulation Statistics ===")
        logger.info(f"Events triggered: {self.stats['events_triggered']}")
        logger.info(f"Connections affected: {self.stats['connections_affected']}")
        logger.info(f"Successful recoveries: {self.stats['successful_recoveries']}")
        logger.info(f"Failed recoveries: {self.stats['failed_recoveries']}")
        
        if self.stats['connections_affected'] > 0:
            recovery_rate = (self.stats['successful_recoveries'] / 
                           self.stats['connections_affected']) * 100
            logger.info(f"Recovery rate: {recovery_rate:.1f}%")

# Pre-defined simulation scenarios
class SimulationScenarios:
    """Collection of pre-defined network simulation scenarios"""
    
    @staticmethod
    def ethernet_to_wifi_switch():
        """Simulate switching from ethernet to WiFi"""
        return SimulationScenario(
            name="Ethernet to WiFi Switch",
            description="Simulates unplugging ethernet and connecting to WiFi",
            events=[
                NetworkEvent(NetworkCondition.CONNECTION_DROP, 2.0, 
                           {"reason": "ethernet_unplugged"}),
                NetworkEvent(NetworkCondition.INTERFACE_CHANGE, 5.0,
                           {"from_interface": "eth0", "to_interface": "wlan0"}),
                NetworkEvent(NetworkCondition.STABLE, 10.0)
            ]
        )
    
    @staticmethod
    def mobile_handoff():
        """Simulate mobile network handoff"""
        return SimulationScenario(
            name="Mobile Network Handoff", 
            description="Simulates switching between cell towers",
            events=[
                NetworkEvent(NetworkCondition.HIGH_LATENCY, 3.0,
                           {"latency_ms": 500}),
                NetworkEvent(NetworkCondition.IP_ADDRESS_CHANGE, 2.0,
                           {"old_ip": "192.168.1.100", "new_ip": "192.168.2.100"}),
                NetworkEvent(NetworkCondition.PACKET_LOSS, 5.0,
                           {"loss_percentage": 15}),
                NetworkEvent(NetworkCondition.STABLE, 15.0)
            ]
        )
    
    @staticmethod
    def intermittent_connectivity():
        """Simulate poor/intermittent connectivity"""
        return SimulationScenario(
            name="Intermittent Connectivity",
            description="Simulates unreliable network with drops and recovery",
            events=[
                NetworkEvent(NetworkCondition.PACKET_LOSS, 10.0,
                           {"loss_percentage": 25}),
                NetworkEvent(NetworkCondition.CONNECTION_DROP, 3.0),
                NetworkEvent(NetworkCondition.INTERMITTENT_CONNECTIVITY, 20.0,
                           {"drop_interval": 5, "restore_interval": 3}),
                NetworkEvent(NetworkCondition.STABLE, 10.0)
            ]
        )
    
    @staticmethod
    def rapid_network_changes():
        """Simulate rapid consecutive network changes"""
        return SimulationScenario(
            name="Rapid Network Changes",
            description="Tests resilience against rapid network state changes",
            events=[
                NetworkEvent(NetworkCondition.INTERFACE_CHANGE, 1.0),
                NetworkEvent(NetworkCondition.IP_ADDRESS_CHANGE, 1.0),
                NetworkEvent(NetworkCondition.CONNECTION_DROP, 0.5),
                NetworkEvent(NetworkCondition.INTERFACE_CHANGE, 1.0),
                NetworkEvent(NetworkCondition.PACKET_LOSS, 2.0),
                NetworkEvent(NetworkCondition.STABLE, 5.0)
            ]
        )
    
    @staticmethod
    def stress_test():
        """Comprehensive stress test scenario"""
        return SimulationScenario(
            name="Stress Test",
            description="Comprehensive test of multiple failure modes",
            events=[
                NetworkEvent(NetworkCondition.STABLE, 5.0),
                NetworkEvent(NetworkCondition.ETHERNET_TO_WIFI_SWITCH(), 0),  # Nested scenario
                NetworkEvent(NetworkCondition.HIGH_LATENCY, 10.0, {"latency_ms": 1000}),
                NetworkEvent(NetworkCondition.INTERMITTENT_CONNECTIVITY, 30.0),
                NetworkEvent(NetworkCondition.CONNECTION_DROP, 5.0),
                NetworkEvent(NetworkCondition.INTERFACE_CHANGE, 3.0),
                NetworkEvent(NetworkCondition.STABLE, 20.0)
            ]
        )

class ConnectionRecoveryTracker:
    """Tracks connection recovery metrics during simulation"""
    
    def __init__(self):
        self.tube_states = {}
        self.recovery_times = {}
        self.failure_events = {}
    
    async def track_connection(self, tube_id: str, event: NetworkEvent, manager: TunnelConnectionManager):
        """Track connection state changes"""
        current_time = time.time()
        
        # Record the failure event
        if tube_id not in self.failure_events:
            self.failure_events[tube_id] = []
        
        self.failure_events[tube_id].append({
            'event': event.event_type.value,
            'time': current_time,
            'state_before': manager.state.value,
            'parameters': event.parameters
        })
        
        # Start tracking recovery time
        if event.event_type != NetworkCondition.STABLE:
            self.recovery_times[tube_id] = current_time
        
        # Monitor for recovery
        await self._monitor_recovery(tube_id, manager)
    
    async def _monitor_recovery(self, tube_id: str, manager: TunnelConnectionManager):
        """Monitor connection for recovery"""
        start_time = self.recovery_times.get(tube_id)
        if not start_time:
            return
            
        # Wait up to 30 seconds for recovery
        for _ in range(30):
            await asyncio.sleep(1.0)
            
            if manager.state == ConnectionState.CONNECTED:
                recovery_time = time.time() - start_time
                logger.info(f"Connection {tube_id} recovered in {recovery_time:.1f}s")
                
                # Record successful recovery
                if tube_id in self.recovery_times:
                    del self.recovery_times[tube_id]
                break
        else:
            logger.warning(f"Connection {tube_id} failed to recover within 30s")

async def run_simulation_test():
    """Example of running a network simulation test"""
    
    # Create simulator
    simulator = NetworkSimulator()
    recovery_tracker = ConnectionRecoveryTracker()
    
    # Add recovery tracking callback
    simulator.add_event_callback(recovery_tracker.track_connection)
    
    # Create mock tube managers for testing
    from unittest.mock import Mock
    
    tube_ids = ["test_tube_1", "test_tube_2"]
    
    for tube_id in tube_ids:
        # Create manager with mocked dependencies
        manager = TunnelConnectionManager(
            tube_id, f"conv_{tube_id}",
            Mock(), Mock()
        )
        
        # Set initial connected state
        manager.state = ConnectionState.CONNECTED
        manager.metrics.connection_established_time = time.time()
        
        # Register manager
        from connection_manager import register_connection_manager
        register_connection_manager(manager)
    
    # Run simulation scenarios
    scenarios = [
        SimulationScenarios.ethernet_to_wifi_switch(),
        SimulationScenarios.mobile_handoff(),
        SimulationScenarios.intermittent_connectivity()
    ]
    
    for scenario in scenarios:
        logger.info(f"\n{'='*50}")
        logger.info(f"Running scenario: {scenario.name}")
        logger.info(f"{'='*50}")
        
        await simulator.simulate_scenario(scenario, tube_ids)
        
        # Brief pause between scenarios
        await asyncio.sleep(2.0)
    
    logger.info("\nAll simulation scenarios completed!")

if __name__ == "__main__":
    # Run the simulation test
    asyncio.run(run_simulation_test())