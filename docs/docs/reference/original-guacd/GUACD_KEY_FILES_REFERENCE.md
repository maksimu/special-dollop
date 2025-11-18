# Guacd Implementation - Key Files Quick Reference

## Critical Architecture Files

### Build Configuration
- **RPM Spec File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/kcm-guacamole-server.spec`
  - Defines entire build process from source download to package creation
  - Version 1.6.0-11 at time of exploration
  - Downloads Apache Guacamole from upstream GitHub repo

### Daemon Management
- **Init Script**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/guacd`
  - SysV-style init script for service management
  - Manages start/stop/status/restart operations
  - Runs as `guacd` non-root user

- **Docker Entrypoint**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/entrypoint-guacd.sh`
  - Runs guacd in Docker container
  - Executes as `guacd` user with reduced privileges
  - Format: `exec runuser -u guacd -- /opt/keeper/sbin/guacd -f "$@"`

### Configuration Files
- **Daemon Configuration**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/guacd.conf`
  - Template for guacd configuration
  - Sections: [server] (host/port), [ssl] (certs), [daemon] (logging)
  - Default: localhost:4822

- **Docker Configuration**: `/Users/mroberts/Documents/kcm/docker/images/guacd/Dockerfile`
  - Base: Rocky Linux 8
  - Installs `@kcm-guacd` package group
  - Exposes port 4822

### Security Profiles
- **Docker Seccomp Profile**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/docker-seccomp.json`
  - Security profile for Docker containers
  - Defines allowed system calls

- **AppArmor Profile**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/guacd-apparmor-profile`
  - Mandatory access control profile
  - Used for DBus isolation in Remote Browser Isolation

## Architectural Patches - Key Implementations

### Core Keeper Security Enhancements
1. **ZeroMQ Socket Support** (Most Important Architectural Addition)
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/5000-KCM-320-Add-ZMQ-guac_socket-with-libzmq-and-libczmq.patch`
   - **Size**: 564 lines of C code
   - **Impact**: Enables distributed guacd with message broker
   - **Key Components**:
     - `guac_socket_zmq_data` structure for ZMQ socket state
     - Read/write/flush/lock handlers for ZMQ communication
     - Support for 12 ZMQ socket types (PAIR, PUB, SUB, REQ, REP, DEALER, ROUTER, PULL, PUSH, XPUB, XSUB, STREAM)
     - Mutex-protected socket operations with buffer locking
     - Polling via `zmq_poll()` for socket readiness

2. **Clipboard Size Configuration**
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/0002-KCM-405-Add-ability-to-configure-clipboard-limits.patch`
   - **Feature**: Configurable clipboard buffer (256KB to 50MB)
   - **Impact**: Balance memory usage vs. usability

3. **SSH FIPS Compliance (AES-GCM Support)**
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/0001-KCM-418-Add-AES-GCM-to-fix-FIPS-SSH-connection.patch`
   - **Change**: Adds AES-GCM ciphers to FIPS-compliant cipher suite
   - **Ciphers**: aes256-gcm@openssh.com, aes128-gcm@openssh.com, plus CBC/CTR variants

4. **IPv4/IPv6 Address Processing**
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/0004-KCM-439-Correct-IPv4-IPv6-processing-of-addresses-fo.patch`
   - **Purpose**: Correct address handling for RDP connections

5. **Terminal Selection Improvements**
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/0007-KCM-461-Improvements-to-terminal-selection-behavior.patch`
   - **Size**: 42KB - Major enhancement to terminal UI

6. **Recording Generalization**
   - **File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/0009-KCM-472-Backport-generalization-of-recording-typescr.patch`
   - **Size**: 47KB - Upstream backport for recording improvements

## Protocol Handler Plugins

### Binary Locations (in built packages)
- RDP Handler: `/opt/keeper/lib/libguac-client-rdp.so`
- VNC Handler: `/opt/keeper/lib/libguac-client-vnc.so`
- SSH Handler: `/opt/keeper/lib/libguac-client-ssh.so`
- Telnet Handler: `/opt/keeper/lib/libguac-client-telnet.so`
- Kubernetes Handler: `/opt/keeper/lib/libguac-client-kubernetes.so`

### Shared Libraries
- Core Protocol: `/opt/keeper/lib/libguac.so` (ABI 25.0.0)
- Terminal Emulation: `/opt/keeper/lib/libguac-terminal.so`
- Header Files: `/opt/keeper/include/guacamole/*.h`

## Build Infrastructure

### Build Scripts
- **Main Build Script**: `/Users/mroberts/Documents/kcm/core/build.sh` (250+ lines)
  - Docker-based parallel build system
  - Handles all package builds
  - Parallelization: 2 * CPU_COUNT builds simultaneously
  
- **Build Order Script**: `/Users/mroberts/Documents/kcm/core/list-build-order.sh`
  - Determines dependency order for packages

- **Top-level Makefile**: `/Users/mroberts/Documents/kcm/Makefile`
  - Orchestrates core, docker, and setup builds
  - Supports dist target for production releases

### Docker Build Environments
- **Rocky Linux 8 (EL8)**: `/Users/mroberts/Documents/kcm/core/build-environments/Dockerfile.el8`
  - Primary build environment
  - Includes autotools, compilers, protocol libraries

- **Build Script**: `/Users/mroberts/Documents/kcm/core/build-environments/build-rpms.sh`
  - Runs inside Docker container
  - Handles source download, patching, compilation

## Dependency Chain

```
kcm-guacamole-server.spec (depends on):
├── kcm-libfreerdp-devel (RDP protocol)
├── kcm-libssh2-devel (SSH protocol)  
├── kcm-libtelnet-devel (Telnet)
├── kcm-libvncclient-devel (VNC)
├── kcm-libzmq-devel (ZeroMQ support)
├── kcm-libczmq-devel (CZMQ bindings)
├── kcm-libwebsockets-devel (Kubernetes/WebSocket)
├── cairo-devel (Graphics rendering)
├── openssl-devel (SSL/TLS)
└── Various system libraries
```

## Output Artifacts

### Built Packages (in target/kcm/RELEASE/el8/x86_64/Packages/)
- `kcm-guacd-X.X.X-N.el8.x86_64.rpm` - Main daemon
- `kcm-libguac-X.X.X-N.el8.x86_64.rpm` - Core library
- `kcm-libguac-devel-X.X.X-N.el8.x86_64.rpm` - Development headers
- `kcm-libguac-terminal-X.X.X-N.el8.x86_64.rpm` - Terminal emulation
- `kcm-libguac-client-rdp-X.X.X-N.el8.x86_64.rpm` - RDP handler
- `kcm-libguac-client-vnc-X.X.X-N.el8.x86_64.rpm` - VNC handler
- `kcm-libguac-client-ssh-X.X.X-N.el8.x86_64.rpm` - SSH handler
- `kcm-libguac-client-telnet-X.X.X-N.el8.x86_64.rpm` - Telnet handler
- `kcm-libguac-client-kubernetes-X.X.X-N.el8.x86_64.rpm` - Kubernetes handler
- `kcm-guaclog-X.X.X-N.el8.x86_64.rpm` - Log interpreter

### Docker Images
- `keeper/guacd:latest` - Guacd-only container image
- `keeper/guacamole:latest` - Full Guacamole with guacd

## Documentation Files
- **Main README**: `/Users/mroberts/Documents/kcm/README.md`
- **Core Package README**: `/Users/mroberts/Documents/kcm/core/README.md`
- **Docker README**: `/Users/mroberts/Documents/kcm/docker/README.md`
- **Guacd Docker README**: `/Users/mroberts/Documents/kcm/docker/images/guacd/README.md`
- **Guacamole Docker README**: `/Users/mroberts/Documents/kcm/docker/images/guacamole/README.md`

## Version Information
- **KCM Release Version**: 2 (stored in `/Users/mroberts/Documents/kcm/core/RELEASE_VERSION`)
- **Guacamole Server Version**: 1.6.0 (specified in spec file)
- **Upstream Repository**: https://github.com/apache/guacamole-server.git

## Network Configuration
- **Default Listen Port**: 4822 (TCP)
- **Default Listen Host**: localhost (127.0.0.1) - but Docker overrides to 0.0.0.0
- **SSL/TLS**: Optional certificate-based encryption between guacd and web app

## Threading and Concurrency
- **Model**: Thread-per-connection (not process-per-connection)
- **Synchronization**: POSIX pthread mutexes with PTHREAD_PROCESS_SHARED flag
- **Locks**: 
  - Socket lock (guac_socket_lock/unlock)
  - Buffer lock (output buffer protection)
  - Clipboard lock (mutex in guac_common_clipboard)

## Key Architectural Flows

1. **Connection Acceptance** → **Protocol Negotiation** → **Plugin Loading** → **Remote Connection** → **Message Routing** → **Cleanup**

2. **Build Process**: Source Git Clone → Patch Application → Configure → Make → Make Check (tests) → Make Install → RPM Creation

3. **Docker Deployment**: Docker Image → Container Start → Entrypoint Script → guacd Daemon (foreground) → Accept Connections on 4822

