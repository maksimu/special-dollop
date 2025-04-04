# Keeper PAM WebRTC for Python

A high-performance WebRTC implementation for Python, written in Rust for maximum efficiency and reliability.

## Description

`keeper-pam-webrtc-rs` provides Python bindings to a Rust-based WebRTC implementation, allowing for:

- Real-time data communication via WebRTC data channels
- Peer connection management
- ICE candidate handling
- Cross-platform compatibility (Linux, macOS, Windows, Alpine)

This package is designed to be used with Keeper Gateway and Keeper Commander.

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
    trickle_ice=True
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

## Build from Source

### Wheel Files

```shell
maturin build --release
pip install keeper-pam-webrtc-rs*
```

If using a different chip set you can use docker to build it:

```shell
docker run --rm --platform linux/amd64 -v $(pwd):/io ghcr.io/pyo3/maturin build --release --manylinux 2014
pip install keeper-pam-webrtc-rs*
```

## Features

- Async-first design with Tokio runtime integration
- Optimized for performance and reliability
- Cross-platform compatibility
- Built with abi3 for maximum Python version compatibility (Python 3.7+)
- Comprehensive WebRTC functionality
