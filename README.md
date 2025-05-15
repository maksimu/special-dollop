# Keeper PAM WebRTC for Python

A high-performance WebRTC implementation for Python, written in Rust for maximum efficiency and reliability.

## Description

`keeper-pam-webrtc-rs` provides Python bindings to a Rust-based WebRTC implementation, allowing for:

- Real-time data communication via WebRTC data channels
- Peer connection management
- ICE candidate handling
- Cross-platform compatibility (Linux, macOS, Windows, Alpine)

This package is designed to be used with Keeper Gateway and Keeper Commander. It serves as a lightweight, focused replacement for [aiortc](https://github.com/aiortc/aiortc), tailored specifically for Keeper Security's internal products and use cases.

> **Note**: This package is intended for internal Keeper Security products and is not being actively advertised for general use.

## Installation

```shell
pip install keeper-pam-webrtc-rs
```

## Usage

```python
import keeper_pam_webrtc_rs

# Initialize WebRTC peer connection
config = {"iceServers": [{"urls": ["stun:stun.l.google.com:19302"]}]}
pc = keeper_pam_webrtc_rs.PyRTCPeerConnection(
    config, 
    on_ice_candidate=lambda c: print(f"ICE candidate: {c}"), 
    on_data_channel=lambda dc: print(f"Data channel: {dc.label()}"),
    trickle_ice=True,
    turn_only=False
)

# Create offer
offer = pc.create_offer()

# Create data channel
dc = pc.create_data_channel("example-channel")

# Send data
dc.send(b'Hello WebRTC!')

# Set message handler
def on_message(data):
    print(f"Received: {data}")
dc.on_message = on_message

# Close when done
pc.close()
```

## Features

- Async-first design with Tokio runtime integration
- Optimized for performance and reliability
- Cross-platform compatibility
- Built with abi3 for maximum Python version compatibility (Python 3.7+)
- Comprehensive WebRTC functionality

## WebRTC Configuration

### TURN-only Mode

The WebRTC implementation supports a "TURN-only" mode which forces all WebRTC traffic through TURN servers rather than attempting direct peer-to-peer connections. This can be useful in the following scenarios:

- When you need to guarantee connectivity through restrictive firewalls
- When you need to ensure consistent network behavior
- For regulatory compliance requirements that mandate traffic through specific servers
- When troubleshooting WebRTC connectivity issues

To enable TURN-only mode, set the `turn_only` parameter to `True` when creating a peer connection:

```python
# Regular mode (uses all ICE candidate types)
peer = keeper_pam_webrtc_rs.PyRTCPeerConnection(
    config, on_ice_candidate, on_data_channel,
    trickle_ice=True, turn_only=False  # Default is False
)

# TURN-only mode (only uses relay candidates)
peer = keeper_pam_webrtc_rs.PyRTCPeerConnection(
    config, on_ice_candidate, on_data_channel,
    trickle_ice=True, turn_only=True
)
```

Note that TURN-only mode requires properly configured TURN servers in your ICE server configuration:

```python
config = {
    "iceServers": [
        {
            "urls": ["turn:your-turn-server.example.com:3478"],
            "username": "your-username",
            "credential": "your-password"
        }
    ]
}
```

#### Performance Considerations

TURN-only mode forces all media traffic through relay servers, which can introduce:
- Higher latency compared to direct connections
- Increased bandwidth costs for TURN server operations
- Potential bottlenecks if TURN servers are overloaded

Use this mode only when necessary, and ensure your TURN servers are properly scaled for your application's needs.
