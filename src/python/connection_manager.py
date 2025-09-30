"""
WebRTC Connection Management with proper state handling and network resilience.
"""

import asyncio
import logging
import time
import json
from enum import Enum
from dataclasses import dataclass, field
from typing import Optional, Dict, List, Callable, Any
from datetime import datetime, timedelta

class ConnectionState(Enum):
    """WebRTC Connection states"""
    INITIALIZING = "initializing"
    CONNECTING = "connecting" 
    CONNECTED = "connected"
    RECONNECTING = "reconnecting"
    DEGRADED = "degraded"       # Connected but with issues
    FAILED = "failed"
    TERMINATED = "terminated"

class NetworkChangeType(Enum):
    """Types of network changes detected"""
    INTERFACE_CHANGE = "interface_change"    # ethernet -> wifi
    IP_ADDRESS_CHANGE = "ip_change"         # IP changed
    CONNECTION_TIMEOUT = "timeout"          # No activity timeout  
    ICE_FAILURE = "ice_failure"            # ICE connection failed
    QUALITY_DEGRADATION = "quality_degraded" # High packet loss, etc.

@dataclass
class ConnectionMetrics:
    """Connection quality metrics"""
    last_activity: float = field(default_factory=time.time)
    ice_restart_attempts: int = 0
    reconnection_attempts: int = 0
    last_restart_time: Optional[float] = None
    last_quality_check: float = field(default_factory=time.time)
    packet_loss_rate: float = 0.0
    rtt_ms: Optional[float] = None
    bytes_sent: int = 0
    bytes_received: int = 0
    connection_established_time: Optional[float] = None

@dataclass  
class ICERestartPolicy:
    """Policy for ICE restart decisions"""
    max_restart_attempts: int = 5
    restart_backoff_base: float = 5.0  # seconds
    restart_backoff_max: float = 60.0
    restart_window: float = 300.0      # 5 minutes
    quality_threshold: float = 0.8     # Restart if quality drops below 80%
    timeout_threshold: float = 30.0    # Restart after 30s of no activity

    def should_restart(self, metrics: ConnectionMetrics, change_type: NetworkChangeType) -> bool:
        """Determine if ICE restart should be attempted"""
        current_time = time.time()
        
        # Check if we've exceeded max attempts
        if metrics.ice_restart_attempts >= self.max_restart_attempts:
            logging.info(f"ICE restart blocked: exceeded max attempts ({self.max_restart_attempts})")
            return False
        
        # Check backoff period
        if metrics.last_restart_time:
            backoff_time = self.restart_backoff_base * (2 ** metrics.ice_restart_attempts)
            backoff_time = min(backoff_time, self.restart_backoff_max)
            
            if current_time - metrics.last_restart_time < backoff_time:
                logging.info(f"ICE restart blocked: in backoff period ({backoff_time:.1f}s remaining)")
                return False
        
        # Always restart for interface changes
        if change_type == NetworkChangeType.INTERFACE_CHANGE:
            return True
            
        # Restart for IP changes
        if change_type == NetworkChangeType.IP_ADDRESS_CHANGE:
            return True
            
        # Restart for timeouts if connection was previously established
        if change_type == NetworkChangeType.CONNECTION_TIMEOUT:
            if metrics.connection_established_time and current_time - metrics.last_activity > self.timeout_threshold:
                return True
        
        # Restart for quality issues
        if change_type == NetworkChangeType.QUALITY_DEGRADATION:
            if metrics.packet_loss_rate > (1.0 - self.quality_threshold):
                return True
        
        return False

class TunnelConnectionManager:
    """
    Manages WebRTC tunnel connections with proper state handling,
    network change detection, and graceful degradation.
    """
    
    def __init__(self, tube_id: str, conversation_id: str, tube_registry, signal_handler):
        self.tube_id = tube_id
        self.conversation_id = conversation_id
        self.tube_registry = tube_registry
        self.signal_handler = signal_handler
        
        # State management
        self.state = ConnectionState.INITIALIZING
        self.metrics = ConnectionMetrics()
        self.restart_policy = ICERestartPolicy()
        
        # Event callbacks
        self.state_change_callbacks: List[Callable] = []
        self.network_change_callbacks: List[Callable] = []
        
        # Async management
        self.restart_task: Optional[asyncio.Task] = None
        self.monitoring_task: Optional[asyncio.Task] = None
        self.shutdown_requested = False
        
        logging.info(f"TunnelConnectionManager initialized for tube {tube_id}")

    async def start_monitoring(self):
        """Start connection quality monitoring"""
        if not self.monitoring_task or self.monitoring_task.done():
            self.monitoring_task = asyncio.create_task(self._monitoring_loop())
            logging.debug(f"Started monitoring for tube {self.tube_id}")

    async def _monitoring_loop(self):
        """Background monitoring loop for connection quality"""
        try:
            while not self.shutdown_requested:
                await asyncio.sleep(30)  # Check every 30 seconds
                
                if self.state in [ConnectionState.CONNECTED, ConnectionState.DEGRADED]:
                    await self._check_connection_quality()
                    
        except asyncio.CancelledError:
            logging.debug(f"Monitoring loop cancelled for tube {self.tube_id}")
        except Exception as e:
            logging.error(f"Error in monitoring loop for tube {self.tube_id}: {e}")

    async def _check_connection_quality(self):
        """Check connection quality and take action if needed"""
        current_time = time.time()
        time_since_activity = current_time - self.metrics.last_activity
        
        # Check for timeout
        if time_since_activity > self.restart_policy.timeout_threshold:
            logging.warning(f"Connection timeout detected for tube {self.tube_id} ({time_since_activity:.1f}s)")
            await self.handle_network_change(NetworkChangeType.CONNECTION_TIMEOUT)
            return
            
        # Update quality metrics
        self.metrics.last_quality_check = current_time
        
        # Get actual quality metrics from Rust
        try:
            stats = await self._get_connection_stats()
            if stats:
                self.metrics.packet_loss_rate = stats.get('packet_loss_rate', 0.0)
                self.metrics.rtt_ms = stats.get('rtt_ms')
                self.metrics.bytes_sent = stats.get('bytes_sent', self.metrics.bytes_sent)
                self.metrics.bytes_received = stats.get('bytes_received', self.metrics.bytes_received)
        except Exception as e:
            logging.debug(f"Could not get connection stats for tube {self.tube_id}: {e}")
            # Use default values if stats unavailable
            self.metrics.packet_loss_rate = 0.0
            self.metrics.rtt_ms = None

    async def handle_network_change(self, change_type: NetworkChangeType, data: Optional[Dict] = None):
        """
        Handle network change events with proper state management and backoff.
        """
        logging.info(f"Handling network change for tube {self.tube_id}: {change_type.value}")
        
        # Update state based on change type
        if self.state == ConnectionState.CONNECTED:
            self.state = ConnectionState.DEGRADED
            await self._notify_state_change()
        
        # Check if we should attempt ICE restart
        if self.restart_policy.should_restart(self.metrics, change_type):
            await self._attempt_ice_restart(change_type)
        else:
            logging.info(f"ICE restart skipped for tube {self.tube_id} based on policy")
            
            # If we can't restart, consider other options
            if change_type in [NetworkChangeType.ICE_FAILURE, NetworkChangeType.CONNECTION_TIMEOUT]:
                await self._handle_connection_failure()

    async def _attempt_ice_restart(self, change_type: NetworkChangeType):
        """
        Attempt ICE restart with proper error handling and state management.
        """
        if self.restart_task and not self.restart_task.done():
            logging.info(f"ICE restart already in progress for tube {self.tube_id}")
            return
            
        self.restart_task = asyncio.create_task(self._ice_restart_workflow(change_type))

    async def _ice_restart_workflow(self, change_type: NetworkChangeType):
        """
        Complete ICE restart workflow with timeout and error handling.
        """
        try:
            logging.info(f"Starting ICE restart for tube {self.tube_id} due to {change_type.value}")
            
            # Update metrics
            self.metrics.ice_restart_attempts += 1
            self.metrics.last_restart_time = time.time()
            
            # Change state
            old_state = self.state
            self.state = ConnectionState.RECONNECTING
            await self._notify_state_change()
            
            # Execute restart with timeout
            restart_timeout = 30.0  # 30 second timeout
            restart_sdp = await asyncio.wait_for(
                self._execute_ice_restart(),
                timeout=restart_timeout
            )
            
            if restart_sdp:
                logging.info(f"ICE restart offer generated for tube {self.tube_id}")
                await self._send_restart_offer(restart_sdp)
                
                # Wait for restart to complete with proper cancellation support
                try:
                    await asyncio.wait_for(
                        self._wait_for_restart_completion(),
                        timeout=2.0  # Much shorter, more reasonable timeout
                    )
                except asyncio.TimeoutError:
                    logging.warning(f"ICE restart completion check timed out for tube {self.tube_id}")
                except asyncio.CancelledError:
                    logging.info(f"ICE restart workflow cancelled for tube {self.tube_id}")
                    raise  # Re-raise to properly handle cancellation
                
                # Check if restart was successful
                if await self._verify_restart_success():
                    logging.info(f"ICE restart successful for tube {self.tube_id}")
                    self.state = ConnectionState.CONNECTED
                    self.metrics.connection_established_time = time.time()
                else:
                    logging.warning(f"ICE restart did not improve connection for tube {self.tube_id}")
                    self.state = ConnectionState.DEGRADED
                    
            else:
                logging.error(f"Failed to generate ICE restart offer for tube {self.tube_id}")
                self.state = old_state
                
            await self._notify_state_change()
            
        except asyncio.TimeoutError:
            logging.error(f"ICE restart timed out for tube {self.tube_id}")
            self.state = ConnectionState.FAILED
            await self._notify_state_change()
            
        except Exception as e:
            logging.error(f"ICE restart failed for tube {self.tube_id}: {e}")
            self.state = ConnectionState.FAILED
            await self._notify_state_change()

    async def _execute_ice_restart(self) -> Optional[str]:
        """Execute the actual ICE restart call to Rust"""
        try:
            # Check if restart_ice method exists on tube_registry
            if not hasattr(self.tube_registry, 'restart_ice'):
                logging.error(f"tube_registry does not have restart_ice method for tube {self.tube_id}")
                return None
                
            # Run in executor to avoid blocking the event loop
            loop = asyncio.get_event_loop()
            restart_sdp = await loop.run_in_executor(
                None, 
                self.tube_registry.restart_ice, 
                self.tube_id
            )
            return restart_sdp
        except AttributeError as e:
            logging.error(f"AttributeError during ICE restart for tube {self.tube_id}: {e}")
            return None
        except Exception as e:
            logging.error(f"Failed to execute ICE restart for tube {self.tube_id}: {e}")
            return None

    async def _send_restart_offer(self, restart_sdp: str):
        """Send ICE restart offer to remote peer"""
        try:
            if hasattr(self.signal_handler, '_send_ice_restart_offer'):
                await asyncio.get_event_loop().run_in_executor(
                    None,
                    self.signal_handler._send_ice_restart_offer,
                    restart_sdp,
                    self.tube_id
                )
            else:
                logging.error("Signal handler does not support ICE restart offers")
        except Exception as e:
            logging.error(f"Failed to send ICE restart offer: {e}")

    async def _wait_for_restart_completion(self) -> None:
        """Wait for ICE restart to complete using event-driven approach with cancellation support"""
        # Poll connection state with short intervals and proper cancellation
        check_interval = 0.1  # 100ms intervals
        max_checks = 20  # Maximum 2 seconds (20 * 0.1s)

        for _ in range(max_checks):
            # Check if we should cancel
            if asyncio.current_task().cancelled():
                raise asyncio.CancelledError()

            # Check if restart completed
            if await self._verify_restart_success():
                logging.debug(f"ICE restart completed successfully for tube {self.tube_id}")
                return

            # Short sleep with cancellation support
            try:
                await asyncio.sleep(check_interval)
            except asyncio.CancelledError:
                logging.info(f"ICE restart wait cancelled for tube {self.tube_id}")
                raise

        # If we get here, restart didn't complete in time
        logging.warning(f"ICE restart did not complete within {max_checks * check_interval}s for tube {self.tube_id}")

    async def _verify_restart_success(self) -> bool:
        """Verify that the ICE restart was successful"""
        try:
            # Get current connection state from Rust
            loop = asyncio.get_event_loop()
            connection_state = await loop.run_in_executor(
                None,
                getattr(self.tube_registry, 'get_connection_state', lambda x: 'Unknown'),
                self.tube_id
            )
            return connection_state.lower() == 'connected'
        except Exception as e:
            logging.error(f"Failed to verify restart success: {e}")
            return False

    async def _handle_connection_failure(self):
        """Handle complete connection failure"""
        if self.metrics.reconnection_attempts >= 3:
            logging.error(f"Max reconnection attempts reached for tube {self.tube_id}")
            self.state = ConnectionState.FAILED
            await self._notify_state_change()
            return
            
        logging.info(f"Attempting full reconnection for tube {self.tube_id}")
        self.metrics.reconnection_attempts += 1
        # TODO: Implement full reconnection logic
        
    async def _notify_state_change(self):
        """Notify all callbacks of state change"""
        for callback in self.state_change_callbacks:
            try:
                if asyncio.iscoroutinefunction(callback):
                    await callback(self.tube_id, self.state)
                else:
                    callback(self.tube_id, self.state)
            except Exception as e:
                logging.error(f"Error in state change callback: {e}")

    def update_activity(self):
        """Update last activity timestamp"""
        self.metrics.last_activity = time.time()
        
        # If we were in degraded state and activity returned, improve state
        if self.state == ConnectionState.DEGRADED:
            self.state = ConnectionState.CONNECTED
            asyncio.create_task(self._notify_state_change())

    def add_state_change_callback(self, callback: Callable):
        """Add callback for state changes"""
        self.state_change_callbacks.append(callback)

    def add_network_change_callback(self, callback: Callable):
        """Add callback for network changes"""  
        self.network_change_callbacks.append(callback)

    async def shutdown(self):
        """Gracefully shutdown the connection manager"""
        logging.info(f"Shutting down connection manager for tube {self.tube_id}")
        
        self.shutdown_requested = True
        
        if self.monitoring_task and not self.monitoring_task.done():
            self.monitoring_task.cancel()
            try:
                await self.monitoring_task
            except asyncio.CancelledError:
                pass
                
        if self.restart_task and not self.restart_task.done():
            self.restart_task.cancel()
            try:
                await self.restart_task
            except asyncio.CancelledError:
                pass
                
        self.state = ConnectionState.TERMINATED
        await self._notify_state_change()

    async def _get_connection_stats(self) -> Optional[Dict]:
        """Get connection statistics from Rust"""
        try:
            loop = asyncio.get_event_loop()
            stats = await loop.run_in_executor(
                None,
                getattr(self.tube_registry, 'get_connection_stats', lambda x: None),
                self.tube_id
            )
            return stats
        except Exception as e:
            logging.debug(f"Failed to get connection stats for tube {self.tube_id}: {e}")
            return None

# Global registry for connection managers
_connection_managers: Dict[str, TunnelConnectionManager] = {}

def get_connection_manager(tube_id: str) -> Optional[TunnelConnectionManager]:
    """Get connection manager for tube ID"""
    return _connection_managers.get(tube_id)

def register_connection_manager(manager: TunnelConnectionManager):
    """Register connection manager"""
    _connection_managers[manager.tube_id] = manager
    logging.debug(f"Registered connection manager for tube {manager.tube_id}")

async def unregister_connection_manager(tube_id: str):
    """Unregister connection manager"""
    manager = _connection_managers.pop(tube_id, None)
    if manager:
        await manager.shutdown()
        logging.debug(f"Unregistered connection manager for tube {tube_id}")