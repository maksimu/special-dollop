# pam-guacr

Protocol handlers for remote desktop access (SSH, RDP, VNC, Telnet, Database protocols, RBI).

Designed to work with WebRTC-based secure tunneling systems like [keeper-pam-webrtc-rs](https://github.com/Keeper-Security/keeper-pam-webrtc-rs).

## Supported Protocols

- **SSH** - Secure Shell protocol handler
- **Telnet** - Telnet protocol handler
- **RDP** - Remote Desktop Protocol handler (requires FreeRDP)
- **VNC** - Virtual Network Computing handler
- **SFTP** - SSH File Transfer Protocol handler
- **Database** - Database protocol handlers (MySQL, PostgreSQL, MongoDB, Redis, Oracle, SQL Server)
- **RBI** - Remote Browser Isolation handler

### RDP System Requirements

RDP support requires FreeRDP development libraries:

```bash
# Ubuntu/Debian
apt install freerdp2-dev libwinpr2-dev

# Fedora/RHEL
dnf install freerdp-devel

# macOS
brew install freerdp
```

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
guacr = { version = "1.1", features = ["ssh", "telnet"] }
```

Available features: `ssh`, `telnet`, `vnc`, `sftp`, `database`, `rbi`, `all`

## Example

```rust
use guacr::{ProtocolHandlerRegistry, ProtocolHandler};
use std::collections::HashMap;
use bytes::Bytes;
use tokio::sync::mpsc;

// Create a registry
let registry = ProtocolHandlerRegistry::new();

// Register handlers
registry.register(guacr::ssh::SshHandler::with_defaults());
registry.register(guacr::telnet::TelnetHandler::with_defaults());

// Get a handler
let handler = registry.get("ssh").expect("SSH handler not found");

// Setup communication channels
let (to_client, mut from_handler) = mpsc::channel::<Bytes>(128);
let (to_handler, from_client) = mpsc::channel::<Bytes>(128);

// Connection parameters
let mut params = HashMap::new();
params.insert("hostname".to_string(), "example.com".to_string());
params.insert("port".to_string(), "22".to_string());
params.insert("username".to_string(), "user".to_string());

// Connect
handler.connect(params, to_client, from_client).await?;
```

## Native Terminal Display (Pipe Streams)

Terminal protocols (SSH, Telnet) support native terminal display via Guacamole pipe streams. 
This allows CLI clients to receive raw terminal output (including ANSI escape codes) instead of 
rendered images, enabling display in native terminal applications.

Enable with the `enable-pipe=true` connection parameter:

```rust
params.insert("enable-pipe".to_string(), "true".to_string());
```

When enabled:
- Server opens a `STDOUT` pipe stream and sends raw terminal data as `blob` instructions
- Client can open a `STDIN` pipe stream to send input directly to the terminal
- ANSI escape codes (colors, cursor movement, etc.) are preserved
- Image rendering still occurs if `PIPE_INTERPRET_OUTPUT` flag is set (default)

## Testing

```bash
# Unit tests (fast, no Docker)
make test

# Full integration tests (requires Docker)
make docker-up
make test-full
make docker-down

# Protocol-specific
make test-ssh
make test-rdp
```

See [TESTING_GUIDE.md](docs/TESTING_GUIDE.md) for details.

### Quick Tests (No Docker Required)

```bash
make test
# Or
./scripts/run-all-tests.sh --quick
```

### Full Integration Tests

The test suite includes Docker-based integration tests for all protocols:

```bash
# Run all tests (starts Docker containers automatically)
./scripts/run-all-tests.sh

# Run specific protocols only
./scripts/run-all-tests.sh --protocol ssh,vnc

# Include database tests
./scripts/run-all-tests.sh --protocol ssh,database

# Clean up containers after tests
./scripts/run-all-tests.sh --clean

# Show help
./scripts/run-all-tests.sh --help
```

### Docker Test Services

| Service | Port | Credentials | Description |
|---------|------|-------------|-------------|
| SSH | 2222 | test_user / test_password | OpenSSH server |
| Telnet | 2323 | root / test_password | Alpine telnetd |
| RDP | 3389 | test_user / test_password | Ubuntu xrdp |
| VNC | 5901 | (password) test_password | Alpine x11vnc |
| MySQL | 13306 | testuser / testpassword | MySQL 8.0 |
| PostgreSQL | 15432 | testuser / testpassword | PostgreSQL 16 |
| MongoDB | 17017 | testuser / testpassword | MongoDB 7.0 |
| Redis | 16379 | (password) testpassword | Redis 7.2 |

### Manual Docker Setup

```bash
# Start all test services
docker-compose -f docker-compose.test.yml up -d

# Start specific services
docker-compose -f docker-compose.test.yml up -d ssh telnet

# Start database services
cd crates/guacr-database
docker-compose -f docker-compose.test.yml up -d

# Stop all services
docker-compose -f docker-compose.test.yml down
```

### Running Individual Test Suites

```bash
# Pipe stream tests (no Docker needed)
cargo test -p guacr-handlers --test pipe_integration_test

# SSH handler tests (requires SSH Docker container)
cargo test -p guacr-ssh --test integration_test -- --include-ignored

# Database tests (requires database Docker containers)
cargo test -p guacr-database --test integration_test

# All unit tests
cargo test --workspace
```

## License

MIT
