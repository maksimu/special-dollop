# keeper-pam-webrtc-rs

Python bindings for the Keeper PAM WebRTC tunneling library.

Provides a secure WebRTC-based tunneling API for connecting to remote services. Supports three connection modes:

- **tunnel** — raw TCP tunneling
- **guacd** — Guacamole protocol (for RDP, SSH, VNC, etc. via a guacd server)
- **socks5** — SOCKS5 proxy

## Installation

```
pip install keeper-pam-webrtc-rs
```

## Usage

```python
import keeper_pam_webrtc_rs

keeper_pam_webrtc_rs.initialize_logger()
registry = keeper_pam_webrtc_rs.PyTubeRegistry()
```

## Requirements

Python 3.7 or later. Wheels are provided for Linux (x86_64, aarch64), macOS (x86_64, arm64), Windows (x86_64), and Alpine Linux.
