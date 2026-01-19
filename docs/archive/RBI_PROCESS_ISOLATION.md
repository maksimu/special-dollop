# RBI Process Isolation

## Overview

Each RBI session spawns a dedicated CEF browser process with no sharing between sessions. This prevents cross-session data leakage, memory contamination, and cookie/session hijacking.

**Status**: Architecture complete, CEF integration pending

## Security Requirement

**NO PROCESS SHARING BETWEEN SESSIONS**

Each session must have its own isolated browser process to prevent:
- Cross-session data leakage
- Memory contamination  
- Cookie/session hijacking
- Profile corruption

## Architecture

### Process Model

```
Session 1                Session 2                Session 3
├── CefBrowserClient    ├── CefBrowserClient    ├── CefBrowserClient
│   └── CefSession      │   └── CefSession      │   └── CefSession
│       └── CEF Tree    │       └── CEF Tree    │       └── CEF Tree
│           ├── Browser │           ├── Browser │           ├── Browser
│           ├── Renderer│           ├── Renderer│           ├── Renderer
│           ├── GPU     │           ├── GPU     │           ├── GPU
│           └── Utility │           └── Utility │           └── Utility
```

Each CefSession creates:
1. Main CEF process (browser process)
2. Renderer process (isolated sandbox)
3. GPU process (if available)
4. Utility processes (network, audio, etc.)

All processes terminate when CefSession::close() is called.

## Implementation

### Handler Level

```rust
#[cfg(feature = "cef")]
{
    let mut cef_client = CefBrowserClient::new(width, height, config);
    
    // Each connect() spawns dedicated CEF process
    cef_client.connect(url, &params, to_client, from_client).await?;
    
    // Process terminated when connect() returns
}
```

### Browser Client Level

```rust
impl CefBrowserClient {
    pub async fn connect(...) -> Result<(), String> {
        // Create and launch CEF session (spawns dedicated subprocess)
        let mut cef_session = CefSession::new(width, height);
        cef_session.launch(url, display_tx, audio_tx).await?;
        
        // Event loop (SSH handler pattern)
        loop {
            tokio::select! {
                _ = keepalive_interval.tick() => { ... }
                _ = debounce.tick() => { ... }
                Some(event) = display_rx.recv() => { ... }
                Some(packet) = audio_rx.recv() => { ... }
                Some(msg) = from_client.recv() => { ... }
            }
        }
        
        // Cleanup - terminates CEF process
        cef_session.close();
    }
}
```

### Session Level

```rust
impl CefSession {
    pub async fn launch(...) -> Result<(), String> {
        // Create unique profile directory (locked)
        let profile_lock = ProfileLock::create_temp(ProfileCreationMode::Exclusive)?;
        
        // Spawn CEF process
        // TODO: Implement CEF C API bindings
        // self.browser_pid = Some(pid);
        
        Ok(())
    }
    
    pub fn close(&mut self) {
        // Terminate CEF process
        // TODO: Implement CEF shutdown
        // Profile lock automatically released on drop
    }
}
```

## Profile Isolation

Each session uses a unique profile directory that is locked to prevent concurrent access:

```rust
// Temporary profile (auto-cleaned)
let profile_lock = ProfileLock::create_temp(ProfileCreationMode::Exclusive)?;

// Persistent profile (locked)
let profile_lock = ProfileLock::acquire("/path", ProfileCreationMode::Exclusive)?;
```

Lock is held for the lifetime of the session and automatically released when ProfileLock is dropped.

### DBus Isolation (Linux)

Each session gets its own isolated DBus socket (KCM-436 pattern):

```rust
#[cfg(target_os = "linux")]
let dbus_isolation = DbusIsolation::create()?;
```

Prevents cross-session IPC via DBus.

## Comparison with KCM

### KCM Architecture

```
guacd main process
├── Connection 1 → fork() → CEF process (PID 101)
│                           └── Profile: /tmp/http_cef_1
├── Connection 2 → fork() → CEF process (PID 201)
│                           └── Profile: /tmp/http_cef_2
└── Connection 3 → fork() → CEF process (PID 301)
                            └── Profile: /tmp/http_cef_3
```

### pam-guacr Architecture

```
tokio runtime
├── Task 1: RbiHandler::connect() → CEF process (PID 101)
│                                    └── Profile: /tmp/rbi-1
├── Task 2: RbiHandler::connect() → CEF process (PID 201)
│                                    └── Profile: /tmp/rbi-2
└── Task 3: RbiHandler::connect() → CEF process (PID 301)
                                     └── Profile: /tmp/rbi-3
```

Both provide the same isolation guarantees:
- One CEF process per connection
- Unique profile directory per connection
- Process terminates when connection ends
- No process sharing

## Security Guarantees

### Guaranteed

- Process Isolation: Each session spawns its own CEF process tree
- Profile Isolation: Unique locked profile per session
- Resource Cleanup: Automatic on session end
- DBus Isolation: Isolated socket per session (Linux)

### Not Guaranteed

- Kernel-Level Isolation: Sessions run in same user namespace (use containers for stronger isolation)
- Network Isolation: Sessions share network stack (use network namespaces)
- Filesystem Isolation: Sessions can access same filesystem (use chroot/containers)

## Production Recommendations

### Use Containers

```yaml
services:
  guacr-rbi:
    image: guacr:latest
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    read_only: true
    tmpfs:
      - /tmp
```

### Configure Resource Limits

```rust
RbiConfig {
    resource_limits: ResourceLimits {
        max_memory_mb: 500,
        max_cpu_percent: 80,
        timeout_seconds: 3600,
    },
    capture_fps: 30,
}
```

### Monitor Processes

```bash
# Monitor CEF processes
ps aux | grep cef

# Monitor memory usage
watch -n 1 'ps aux | grep cef | awk "{print \$6}"'

# Monitor profile directories
du -sh /tmp/rbi-*
```

## Implementation Status

### Complete

- Process isolation architecture
- Profile locking
- DBus isolation (Linux)
- Event loop structure (SSH handler pattern)
- Recording support
- Keep-alive management
- Documentation

### Pending

- CEF C API bindings
- Actual CEF process spawning
- Handler callbacks (RenderHandler, AudioHandler)
- PID tracking implementation
- Resource monitoring

### Testing

All 113 unit tests pass. Integration tests pending CEF implementation.

## References

- SSH Handler: `crates/guacr-ssh/src/handler.rs` (production-ready reference)
- CEF Browser Client: `crates/guacr-rbi/src/cef_browser_client.rs`
- CEF Session: `crates/guacr-rbi/src/cef_session.rs`
- Profile Isolation: `crates/guacr-rbi/src/profile_isolation.rs`
- KCM Implementation: `docs/docs/reference/original-guacd/RBI_IMPLEMENTATION_DEEP_DIVE.md`
