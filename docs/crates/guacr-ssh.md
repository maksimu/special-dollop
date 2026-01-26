# guacr-ssh - SSH Protocol Handler

Production-ready SSH handler with JPEG rendering, clipboard, recording, and threat detection.

## Status

✅ **Production Ready** - Complete implementation with all features

## Features

### Authentication
- ✅ Password authentication
- ✅ SSH key authentication (all formats: OpenSSH, PEM, PKCS#8)
- ✅ Passphrase-protected keys
- ✅ Host key verification (known_hosts)

### Terminal Features
- ✅ JPEG rendering (5-10x faster than PNG)
- ✅ 16ms debounce (60fps smoothness)
- ✅ Default handshake size (1024x768 @ 96 DPI, then resizes to browser)
- ✅ Bidirectional clipboard (OSC 52 + bracketed paste)
- ✅ Dirty region optimization with scroll detection
- ✅ Control character handling (Ctrl+C, etc.)
- ✅ Scrollback buffer (~150 lines)
- ✅ Mouse events for vim/tmux (X11 mouse sequences)

### Recording
- ✅ Session recording (Asciicast + Guacamole .ses formats)
- ✅ Recording transports (Files, S3, ZeroMQ, channels)
- ✅ Zero-copy, lock-free recording
- See [docs/concepts/RECORDING.md](../concepts/RECORDING.md)

### Security
- ✅ AI threat detection with BAML REST API
- ✅ Automatic session termination on threats
- ✅ Three modes: reactive, proactive, stateful
- See [docs/concepts/THREAT_DETECTION.md](../concepts/THREAT_DETECTION.md)

## Architecture

```
SSH Server
    ↓ TCP
russh Client
    ├── Authentication (password/key)
    ├── PTY Channel → Terminal Output → vt100 Parser
    ├── Terminal Emulator → JPEG Renderer → Guacamole Protocol
    ├── Input Handler ← Guacamole Protocol ← Client
    ├── Clipboard (OSC 52 + bracketed paste)
    └── Recording → Multiple Transports
```

## Configuration

```rust
pub struct SshConfig {
    pub hostname: String,
    pub port: u16,                    // Default: 22
    pub username: String,
    pub password: Option<String>,
    pub private_key: Option<String>,
    pub passphrase: Option<String>,
    pub width: u32,                   // Default: 80 cols
    pub height: u32,                  // Default: 24 rows
    pub enable_recording: bool,
    pub recording_path: Option<String>,
    pub enable_threat_detection: bool,
    pub threat_detection_endpoint: Option<String>,
}
```

## Example Usage

### Password Authentication

```rust
use guacr_ssh::{SshHandler, SshConfig};
use std::collections::HashMap;

let mut params = HashMap::new();
params.insert("hostname".to_string(), "ssh-server.example.com".to_string());
params.insert("port".to_string(), "22".to_string());
params.insert("username".to_string(), "user".to_string());
params.insert("password".to_string(), "password123".to_string());

let handler = SshHandler::with_defaults();
handler.connect(params, to_client, from_client).await?;
```

### SSH Key Authentication

```rust
params.insert("username".to_string(), "user".to_string());
params.insert("private-key".to_string(), "-----BEGIN OPENSSH PRIVATE KEY-----\n...".to_string());
params.insert("passphrase".to_string(), "key-passphrase".to_string()); // Optional
```

### With Recording

```rust
params.insert("enable-recording".to_string(), "true".to_string());
params.insert("recording-path".to_string(), "/tmp/session.cast".to_string());
```

### With Threat Detection

```rust
params.insert("threat_detection_enabled".to_string(), "true".to_string());
params.insert("threat_detection_baml_endpoint".to_string(), "http://localhost:8000/api".to_string());
params.insert("threat_detection_auto_terminate".to_string(), "true".to_string());
```

## Performance

### Rendering
- **Frame rate**: 60 FPS (16ms debounce)
- **Encoding**: JPEG quality 95 (5-10x faster than PNG)
- **Latency**: <100ms for input events
- **Memory**: ~10MB per connection

### Optimizations
- Dirty region tracking (only render changed areas)
- Scroll detection (copy + render only new lines)
- Font rendering with fontdue (fast)
- JPEG encoding (faster than PNG)

## Clipboard Integration

### Copy (Server → Client)
- Detects OSC 52 escape sequences from server
- Sends clipboard data to client via Guacamole protocol

### Paste (Client → Server)
- Receives clipboard data from client
- Uses bracketed paste mode for safe pasting

## Recording

### Formats
1. **Asciicast v2** - Compatible with asciinema player
2. **Guacamole .ses** - Native Guacamole recording format

### Transports
1. **Files** - Write to local filesystem
2. **S3** - Upload to AWS S3
3. **Channels** - Stream to in-memory channels (zero-copy)
4. **Named Pipes** - Stream to external processes

See [docs/concepts/RECORDING.md](../concepts/RECORDING.md) for details.

## Threat Detection

### Modes

#### Reactive Mode (Default)
Analyzes commands after they're sent, can terminate session:
```rust
params.insert("threat_detection_enabled".to_string(), "true".to_string());
params.insert("threat_detection_auto_terminate".to_string(), "true".to_string());
```

#### Proactive Mode
Buffers commands and requires AI approval before execution:
```rust
params.insert("threat_detection_proactive_mode".to_string(), "true".to_string());
params.insert("threat_detection_approval_timeout_ms".to_string(), "2000".to_string());
```

#### Stateful Mode
AI receives full terminal state for context-aware decisions:
```rust
// Enable with terminal-state feature
cargo build --features terminal-state
```

See [docs/concepts/THREAT_DETECTION.md](../concepts/THREAT_DETECTION.md) for details.

## Key Files

- `src/handler.rs` - Main SSH handler implementation
- `src/auth.rs` - Authentication (password/key)
- `src/terminal.rs` - Terminal emulation
- `src/clipboard.rs` - Clipboard integration

## Testing

```bash
# Unit tests
cargo test -p guacr-ssh

# Integration tests (requires Docker)
make docker-up
make test-ssh

# With threat detection
cargo test -p guacr-ssh --features threat-detection

# With recording
cargo test -p guacr-ssh --features recording
```

## Troubleshooting

### Authentication Failures

**Key format issues:**
```bash
# Convert key to OpenSSH format
ssh-keygen -p -m PEM -f ~/.ssh/id_rsa
```

**Host key verification:**
```rust
// Disable for testing (not recommended for production)
params.insert("ignore-host-key".to_string(), "true".to_string());
```

### Rendering Issues

**Slow rendering:**
- Check JPEG quality setting (default 95)
- Verify 16ms debounce is active
- Check dirty region tracking is enabled

**Garbled output:**
- Verify terminal size matches client
- Check vt100 parser compatibility
- Ensure UTF-8 encoding

### Clipboard Not Working

**OSC 52 not detected:**
- Verify server sends OSC 52 sequences
- Check terminal emulator compatibility

**Paste not working:**
- Verify bracketed paste mode is enabled
- Check for special characters in clipboard

## References

- russh: https://github.com/warp-tech/russh
- vt100 parser: https://docs.rs/vt100/
- Guacamole Protocol: See [docs/concepts/GUACAMOLE_PROTOCOL.md](../concepts/GUACAMOLE_PROTOCOL.md)
- Recording: See [docs/concepts/RECORDING.md](../concepts/RECORDING.md)
- Threat Detection: See [docs/concepts/THREAT_DETECTION.md](../concepts/THREAT_DETECTION.md)
