# Keeper PAM WebRTC for Python

A secure, stable, and high-performance **Tube API** for Python, providing WebRTC-based secure tunneling with enterprise-grade security and reliability optimizations.

## Core Values

**Security • Stability • Performance** - Built for Keeper Security's mission-critical applications:

- **🔒 Security First**: Memory-safe Rust implementation with comprehensive bounds checking
- **🛡️ Enterprise Stability**: Lock-free architecture eliminates race conditions and deadlocks  
- **⚡ Optimized Performance**: Advanced optimizations deliver exceptional speed when you need it
- **🔧 Production Ready**: Zero-configuration reliability for demanding security applications

## Description

`keeper-pam-webrtc-rs` provides Python bindings to a Rust-based **Tube API** for secure communication, designed for:

- **Secure tunneling** via WebRTC data channels with memory-safe operations
- **Multi-connection management** through tube abstractions
- **Reliable peer connection handling** with comprehensive error handling
- **Efficient channel management** for different communication patterns
- **Cross-platform compatibility** (Linux, macOS, Windows, Alpine)
- **Mission-critical reliability** for security-focused applications

This package is designed to be used with Keeper Gateway and Keeper Commander. It provides a **secure, reliable tube-based communication system** built on WebRTC, specifically tailored for Keeper Security's internal products and security-critical tunneling use cases.

> **Note**: This package is intended for internal Keeper Security products and is not being actively advertised for general use.

## Installation

```shell
pip install keeper-pam-webrtc-rs
```

## Usage

```python
import keeper_pam_webrtc_rs

# Create a tube registry
registry = keeper_pam_webrtc_rs.PyTubeRegistry()

# Define a signal callback for WebRTC events
def on_signal(signal_dict):
    print(f"Received signal: {signal_dict}")
    # Handle ICE candidates, connection state changes, etc.

# Create a server-side tube for tunneling
server_result = registry.create_tube(
    conversation_id="tunnel-session-123",
    settings={
        "conversationType": "tunnel",
        "target_host": "127.0.0.1", 
        "target_port": "22"  # SSH tunnel example
    },
    trickle_ice=True,
    callback_token="server-token",
    ksm_config="server-config",
    signal_callback=on_signal
)

# Get the offer SDP to send to the client
server_offer = server_result['offer']
server_tube_id = server_result['tube_id']

# Create a client-side tube with the offer
client_result = registry.create_tube(
    conversation_id="tunnel-client-123", 
    settings={
        "conversationType": "tunnel",
        "target_host": "192.168.1.100",
        "target_port": "22"
    },
    trickle_ice=True,
    callback_token="client-token", 
    ksm_config="client-config",
    offer=server_offer,  # Use server's offer
    signal_callback=on_signal
)

# Get the answer SDP to send back to server
client_answer = client_result['answer']
client_tube_id = client_result['tube_id']

# Set the remote description on the server
registry.set_remote_description(server_tube_id, client_answer, is_answer=True)

# Check connection state
state = registry.get_connection_state(server_tube_id)
print(f"Connection state: {state}")

# Close when done
registry.close_tube(server_tube_id)
registry.close_tube(client_tube_id)
```

## Features

- **🔒 Memory Safety**: Rust-powered implementation prevents buffer overflows and memory corruption
- **🛡️ Reliable Architecture**: Lock-free design eliminates race conditions and ensures stability
- **⚡ Efficient Performance**: Optimized for speed without compromising security or stability
- **🌊 Tube Abstraction**: High-level API for managing WebRTC-based secure tunnels
- **🌍 Cross-Platform**: Secure, consistent behavior across Linux, macOS, Windows, Alpine
- **🐍 Python Integration**: Built with abi3 for maximum compatibility (Python 3.7+)
- **🔧 Production Hardened**: Comprehensive error handling and graceful degradation

## Tube API Architecture

This implementation provides a **Tube-based abstraction** over WebRTC:

### **Security Features**
- **Memory-Safe Operations**: Rust's ownership system prevents common security vulnerabilities
- **Bounds Checking**: Comprehensive validation prevents buffer overflows and data corruption
- **Zero Unsafe Code**: Hot paths use only verified, safe Rust code (except vetted SIMD intrinsics)
- **Graceful Error Handling**: Robust error recovery prevents crashes and data leaks

### **Tube Management**  
- **Multi-Connection Support**: Each tube can manage multiple WebRTC connections
- **Channel Abstraction**: High-level channel management for different protocols
- **State Management**: Comprehensive connection state tracking and reporting
- **Signal Handling**: Event-driven architecture for ICE candidates and state changes

### **Performance Features**
- **SIMD Optimization**: Hardware-accelerated frame parsing with safe fallbacks
- **Zero-Copy Pipelines**: Efficient data handling minimizes memory overhead
- **Event-Driven Design**: Native WebRTC events provide responsive communication
- **Always Optimized**: Maximum efficiency by default, no configuration required

## Tube API Reference

### **Core Methods**

- `create_tube(conversation_id, settings, ...)` - Create a new secure tube or add conversation to existing tube
- `set_remote_description(tube_id, sdp, is_answer)` - Set remote SDP description
- `add_ice_candidate(tube_id, candidate)` - Add ICE candidate for connection
- `get_connection_state(tube_id)` - Get current connection state
- `close_connection(connection_id)` - Close specific connection
- `close_tube(tube_id)` - Close entire tube

### **Conversation Types**

The tube API supports different communication patterns:

- **`tunnel`** - Secure TCP tunneling through WebRTC
- **`guacd`** - Apache Guacamole protocol tunneling
- **`socks5`** - SOCKS5 proxy tunneling

## Build & Verification

To build and verify the implementation:

```bash
# Standard build (all optimizations enabled)
cargo build --release

# Run comprehensive test suite
cargo test --release

# Optional: Enable debug logging for troubleshooting
cargo build --release --features production_debug
```

## Why This Implementation?

Built specifically for Keeper Security's tunneling requirements:

- **Security-First Design**: Memory safety and comprehensive validation prevent vulnerabilities
- **Mission-Critical Reliability**: Lock-free architecture ensures stable operation under load  
- **Optimized for Security Applications**: Performance optimizations that don't compromise security
- **Tube Abstraction**: High-level API designed specifically for secure tunneling use cases

**The secure, stable, high-performance tube communication system for enterprise security applications.**
