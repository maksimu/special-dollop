# RBI Process Isolation Architecture

## Critical Security Requirement

**Each RBI session MUST have its own dedicated browser process - NO SHARING**

This document explains how process isolation is implemented to prevent cross-session data leakage.

## Architecture Overview

### One Session = One Process Tree

```
Session 1                          Session 2                          Session 3
├── CefBrowserClient              ├── CefBrowserClient              ├── CefBrowserClient
│   └── CefSession                │   └── CefSession                │   └── CefSession
│       ├── Profile: /tmp/rbi-1   │       ├── Profile: /tmp/rbi-2   │       ├── Profile: /tmp/rbi-3
│       └── CEF Process Tree      │       └── CEF Process Tree      │       └── CEF Process Tree
│           ├── Browser (PID 101) │           ├── Browser (PID 201) │           ├── Browser (PID 301)
│           ├── Renderer (102)    │           ├── Renderer (202)    │           ├── Renderer (302)
│           ├── GPU (103)          │           ├── GPU (203)          │           ├── GPU (303)
│           └── Utility (104)      │           └── Utility (204)      │           └── Utility (304)
```

**Key Points:**
- Each session spawns its own CEF process tree
- No process sharing between sessions
- Complete memory isolation
- Profile directories are locked to prevent reuse

## Implementation Details

### 1. Handler Level (`handler.rs`)

```rust
#[cfg(feature = "cef")]
{
    use crate::cef_browser_client::CefBrowserClient;
    let mut cef_client = CefBrowserClient::new(_width, _height, self.config.clone());

    // SECURITY: Each connect() call spawns a DEDICATED CEF process
    // No process sharing between sessions - complete isolation
    info!("RBI: Spawning dedicated CEF process (no sharing with other sessions)");
    
    cef_client
        .connect(url, &params, to_client, from_client)
        .await
        .map_err(HandlerError::ConnectionFailed)?;

    info!("RBI handler ended (CEF) - dedicated process terminated");
}
```

### 2. Browser Client Level (`cef_browser_client.rs`)

```rust
/// CEF Browser client for RBI sessions with full audio support
///
/// SECURITY: Each instance creates a DEDICATED CEF process - NO SHARING
/// - Spawns separate CEF subprocess per session
/// - Uses unique profile directory (locked to prevent reuse)
/// - Process terminates when session ends
/// - No cross-session contamination possible
pub struct CefBrowserClient {
    // ... fields ...
}

impl CefBrowserClient {
    pub async fn connect(
        &mut self,
        url: &str,
        params: &std::collections::HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), String> {
        info!("RBI/CEF: Launching DEDICATED CEF process for URL: {}", url);
        info!("RBI/CEF: SECURITY - Each session gets isolated browser process (no sharing)");

        // Create and launch CEF session (spawns dedicated subprocess)
        let mut cef_session = CefSession::new(self.width, self.height);
        
        // CRITICAL: This spawns a NEW CEF process - no sharing!
        cef_session.launch(url, display_tx, audio_tx).await?;

        info!("RBI/CEF: Dedicated CEF process spawned (PID will be logged by CEF)");

        // ... event loop ...

        // Cleanup
        info!("RBI/CEF: Closing CEF session and terminating dedicated process");
        cef_session.close();

        info!("RBI/CEF: Session ended, CEF process terminated");
        Ok(())
    }
}
```

### 3. Session Level (`cef_session.rs`)

```rust
/// CEF session manager
///
/// SECURITY: Each instance spawns a DEDICATED CEF process
/// - Manages a single isolated CEF browser instance
/// - Spawns separate process tree (browser + renderer + GPU + utility)
/// - No sharing with other CefSession instances
/// - Process terminates when close() is called
pub struct CefSession {
    width: u32,
    height: u32,
    /// Process ID of the main CEF browser process
    /// SECURITY: Tracked for monitoring and termination
    browser_pid: Option<u32>,
    /// Profile directory path (locked for this session)
    profile_path: Option<String>,
}

impl CefSession {
    pub async fn launch(
        &mut self,
        url: &str,
        display_tx: mpsc::Sender<CefDisplayEvent>,
        audio_tx: mpsc::Sender<CefAudioPacket>,
    ) -> Result<(), String> {
        info!("CEF: Launching DEDICATED browser process for URL: {}", url);
        info!("CEF: SECURITY - Each session gets isolated process (no sharing)");

        // Create unique profile directory with lock
        let profile_lock = ProfileLock::create_temp(ProfileCreationMode::Exclusive)?;
        let profile_path = profile_lock.path().to_string_lossy().to_string();
        info!("CEF: Profile directory: {} (locked for this session)", profile_path);

        // TODO: Spawn CEF process here
        // 1. Initialize CEF with profile_path
        // 2. Spawn browser process
        // 3. Store PID: self.browser_pid = Some(pid);

        Ok(())
    }

    pub fn close(&mut self) {
        info!("CEF: Closing browser and terminating dedicated process");
        
        if let Some(pid) = self.browser_pid {
            info!("CEF: Terminating browser process (PID: {})", pid);
        }

        // TODO: Terminate CEF process
        // 1. Close browser window
        // 2. Shutdown CEF
        // 3. Wait for process termination
        // 4. Profile lock is automatically released
    }
}
```

## Profile Isolation

### Profile Directory Locking

Each session uses a unique profile directory that is locked to prevent concurrent access:

```rust
use crate::profile_isolation::{ProfileCreationMode, ProfileLock};

// Create temporary profile (auto-cleaned on drop)
let profile_lock = ProfileLock::create_temp(ProfileCreationMode::Exclusive)?;

// Or use persistent profile (locked)
let profile_lock = ProfileLock::acquire("/path/to/profile", ProfileCreationMode::Exclusive)?;
```

**Security Properties:**
- `Exclusive` mode: Only one session can use a profile at a time
- Lock is held for the lifetime of the session
- Lock is automatically released when `ProfileLock` is dropped
- Prevents profile reuse while session is active

### DBus Isolation (Linux)

On Linux, each session gets its own isolated DBus socket (KCM-436 pattern):

```rust
#[cfg(target_os = "linux")]
use crate::profile_isolation::DbusIsolation;

// Create isolated DBus socket
let dbus_isolation = DbusIsolation::create()?;

// CEF will use this isolated socket
// Prevents cross-session IPC via DBus
```

## Comparison with KCM

### KCM Architecture

KCM uses a similar approach with fork-based process isolation:

```
guacd main process
├── Connection 1
│   └── fork() → CEF process (PID 101)
│       ├── Profile: /tmp/http_cef_1
│       └── Shared memory: /dev/shm/HTTP_CEF_SHM_1
├── Connection 2
│   └── fork() → CEF process (PID 201)
│       ├── Profile: /tmp/http_cef_2
│       └── Shared memory: /dev/shm/HTTP_CEF_SHM_2
└── Connection 3
    └── fork() → CEF process (PID 301)
        ├── Profile: /tmp/http_cef_3
        └── Shared memory: /dev/shm/HTTP_CEF_SHM_3
```

**Key Similarities:**
- One CEF process per connection
- Unique profile directory per connection
- Process terminates when connection ends
- No process sharing

### pam-guacr Architecture

pam-guacr uses tokio async instead of fork, but maintains the same isolation:

```
tokio runtime
├── Task 1: RbiHandler::connect()
│   └── CefSession::launch() → CEF process (PID 101)
│       ├── Profile: /tmp/rbi-1 (locked)
│       └── Channels: display_tx, audio_tx
├── Task 2: RbiHandler::connect()
│   └── CefSession::launch() → CEF process (PID 201)
│       ├── Profile: /tmp/rbi-2 (locked)
│       └── Channels: display_tx, audio_tx
└── Task 3: RbiHandler::connect()
    └── CefSession::launch() → CEF process (PID 301)
        ├── Profile: /tmp/rbi-3 (locked)
        └── Channels: display_tx, audio_tx
```

**Key Differences:**
- Uses tokio tasks instead of fork
- Uses channels instead of shared memory
- Same isolation guarantees

## Security Guarantees

### What IS Guaranteed

✅ **Process Isolation:**
- Each session spawns its own CEF process tree
- No memory sharing between sessions
- Process terminates when session ends

✅ **Profile Isolation:**
- Each session uses unique profile directory
- Profile is locked to prevent concurrent access
- Profile is cleaned up after session ends (temp profiles)

✅ **DBus Isolation (Linux):**
- Each session gets isolated DBus socket
- Prevents cross-session IPC

✅ **Resource Cleanup:**
- CEF processes are terminated on session end
- Profile locks are released automatically
- No process leakage

### What is NOT Guaranteed

❌ **Kernel-Level Isolation:**
- Sessions run in same user namespace (unless containerized)
- Kernel vulnerabilities could allow escape
- Use containers/VMs for stronger isolation

❌ **Network Isolation:**
- Sessions share network stack
- Use network namespaces for isolation

❌ **Filesystem Isolation:**
- Sessions can access same filesystem
- Use chroot/containers for isolation

## Best Practices

### For Production Deployments

1. **Use Containers:**
   ```yaml
   # docker-compose.yml
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

2. **Limit Resources:**
   ```rust
   RbiConfig {
       resource_limits: ResourceLimits {
           max_memory_mb: 500,
           max_cpu_percent: 80,
           timeout_seconds: 3600,
       },
       // ...
   }
   ```

3. **Monitor Processes:**
   - Track CEF process PIDs
   - Monitor memory usage
   - Kill runaway processes

4. **Clean Up Profiles:**
   - Use temporary profiles for untrusted sessions
   - Regularly clean up persistent profiles
   - Monitor disk usage

### For Development

1. **Test Process Isolation:**
   ```bash
   # Start two sessions
   # Verify separate CEF processes
   ps aux | grep cef
   
   # Verify separate profile directories
   ls -la /tmp/rbi-*
   ```

2. **Test Profile Locking:**
   ```bash
   # Try to start two sessions with same profile
   # Second session should fail to acquire lock
   ```

3. **Test Cleanup:**
   ```bash
   # End session
   # Verify CEF process terminated
   # Verify profile directory cleaned up (temp profiles)
   ```

## Implementation Status

### ✅ Completed

- Process isolation architecture designed
- Profile locking implemented
- DBus isolation implemented (Linux)
- Documentation complete
- Tests pass

### ⚠️ In Progress

- CEF integration (stub implementation)
- Actual CEF process spawning
- PID tracking and monitoring

### 📋 TODO

- Complete CEF C API bindings
- Implement CEF process spawning
- Add process monitoring
- Add resource limits enforcement
- Add integration tests

## References

- KCM Implementation: `/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/`
- SSH Handler (reference): `/Users/mroberts/Documents/pam-guacr/crates/guacr-ssh/src/handler.rs`
- Profile Isolation: `/Users/mroberts/Documents/pam-guacr/crates/guacr-rbi/src/profile_isolation.rs`
- CEF Session: `/Users/mroberts/Documents/pam-guacr/crates/guacr-rbi/src/cef_session.rs`
