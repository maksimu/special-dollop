# pam-guacr

Protocol handlers for remote desktop access (SSH, RDP, VNC, Telnet, Database protocols, RBI).

Designed to work with WebRTC-based secure tunneling systems like [keeper-pam-webrtc-rs](https://github.com/Keeper-Security/keeper-pam-webrtc-rs).

## Supported Protocols

- **SSH** - Secure Shell protocol handler
- **Telnet** - Telnet protocol handler
- **RDP** - Remote Desktop Protocol handler
- **VNC** - Virtual Network Computing handler
- **SFTP** - SSH File Transfer Protocol handler
- **Database** - Database protocol handlers (MySQL, PostgreSQL, MongoDB, Redis, Oracle, SQL Server)
- **RBI** - Remote Browser Isolation handler

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
guacr = { version = "1.1", features = ["ssh", "telnet"] }
```

Available features: `ssh`, `telnet`, `rdp`, `vnc`, `sftp`, `database`, `rbi`, `all`

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

## License

MIT
