# WebSocket Integration Guide

This guide explains how to integrate the WebRTC connection management system with WebSocket message handling for the keeper-pam-webrtc-rs library.

## Overview

The connection management system (introduced in the latest release) provides:
- **NAT Timeout Prevention**: 5-minute keepalive intervals prevent the 19-minute NAT timeout issue
- **Intelligent ICE Restart**: Exponential backoff retry logic (5s → 10s → 20s → 60s max)
- **Network Change Detection**: Automatic handling of interface changes, IP changes, and connection timeouts
- **Connection State Management**: Proper async state tracking with graceful degradation
- **Resource Management**: Proper cleanup and connection lifecycle management

## Integration Architecture

### 1. Connection Manager Setup

```python
from connection_manager import (
    TunnelConnectionManager, 
    ConnectionState, 
    NetworkChangeType,
    get_connection_manager,
    register_connection_manager,
    unregister_connection_manager
)

# Create and register connection manager
manager = TunnelConnectionManager(tube_id, conversation_id, tube_registry, signal_handler)
manager.add_state_change_callback(your_callback_function)
register_connection_manager(manager)
```

### 2. WebSocket Message Handling

Your WebSocket message handler should:

1. **Parse incoming messages** - Handle both single dict and array formats
2. **Route to connection manager** - Get the appropriate manager for the tube_id
3. **Update activity** - Call `connection_manager.update_activity()` on message receipt
4. **Handle state changes** - Process connection state updates from Rust

### 3. Signal Handler Integration

The signal handler receives events from the Rust WebRTC core:

```python
def signal_from_rust(self, response: dict):
    signal_kind = response.get('kind', '')
    tube_id = response.get('tube_id', '')
    
    if signal_kind == 'connection_state_changed':
        # Update connection manager state
    elif signal_kind == 'network_change_detected':
        # Trigger ICE restart if needed
    elif signal_kind == 'keepalive':
        # Update activity timestamp
```

## Key Integration Points

### Activity Tracking
- Call `connection_manager.update_activity()` on any WebSocket message
- Keepalive signals from Rust automatically update activity
- Activity tracking prevents NAT timeouts

### State Management
- Monitor `ConnectionState` changes (INITIALIZING → CONNECTING → CONNECTED)
- Handle failures with automatic ICE restart attempts
- Use exponential backoff for restart attempts

### Network Change Handling
- Listen for `network_change_detected` signals
- Automatically trigger ICE restart on network changes
- Support different change types (interface change, timeout, ICE failure)

## Error Handling

- Connection failures trigger automatic ICE restart
- Multiple restart attempts use exponential backoff
- Failed connections can be manually restarted
- Proper cleanup on connection shutdown

## Implementation Notes

### Encryption Handling
This connection management system handles WebRTC signaling and connection state only. Message encryption/decryption should be handled at a different layer in your application stack, not in the WebSocket handlers.

### Integration with Existing Code
The connection manager is designed to work alongside your existing tube registry and signal handlers. It enhances them with:
- Automatic connection recovery via ICE restart
- Network change detection and response
- Activity-based timeout prevention
- Exponential backoff retry logic

### Testing
The system includes comprehensive tests covering:
- Connection lifecycle management (`test_connection_manager.py`)
- ICE restart and keepalive functionality (`test_ice_restart_and_keepalive.py`) 
- Performance under load (`test_performance.py`)
- Resource cleanup and stress testing (`test_resource_cleanup_stress.py`)
- Integration scenarios (`test_integration.py`)

## Connection Lifecycle

1. **Initialize** - Create connection manager and register
2. **Connect** - Establish WebRTC connection
3. **Monitor** - Track activity and connection health
4. **Handle Changes** - Respond to network changes with ICE restart
5. **Cleanup** - Unregister manager on connection close