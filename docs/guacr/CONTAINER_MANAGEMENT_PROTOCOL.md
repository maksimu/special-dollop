# Container Management Protocol (K8s/Docker)

## Overview

A new protocol handler for managing and accessing containerized environments through a browser-based interface. Similar to how database handlers provide spreadsheet-like views of database tables, this handler provides intuitive views of containers, pods, and their associated resources.

**Key Architecture Decision:** Container management runs **through SSH/WinRM** to a bastion/jump host where `kubectl`/`docker` is installed, rather than direct Kubernetes API access. This matches real-world deployment patterns where:
- Container management happens from specific bastion hosts
- kubectl configs and Docker sockets are on remote systems
- Users SSH (Linux/Mac) or WinRM (Windows) to jump boxes, then run container commands
- All access is audited through the remote execution layer
- Credentials and access control managed at the SSH/WinRM level

## Use Cases

1. **Developer Access** - SSH to dev box, browse and exec into containers
2. **Operations** - SSH to ops bastion, quick access to logs, metrics, and shell sessions
3. **Debugging** - SSH to jump host, inspect running containers without local kubectl/docker CLI
4. **Multi-cluster Management** - Switch between clusters via SSH to different bastions
5. **Security** - Audited SSH + container sessions with threat detection

## Architecture

### Simplified Approach: Extension to SSH/WinRM Handlers

**Key Insight:** Instead of creating a separate `guacr-container` crate, implement this as a **mode** or **command set** within existing SSH/WinRM handlers.

**Why This Makes Sense:**
1. Container commands run on remote hosts via SSH/WinRM
2. Output is already terminal-based (kubectl/docker CLI output)
3. Reuses 100% of existing SSH/WinRM authentication and connection logic
4. No need for direct Kubernetes API client (kube-rs)
5. No need for Docker API client (bollard)
6. Works with existing firewall rules (SSH port 22, WinRM port 5985/5986)

### Implementation Approach

**Option 1: Smart Terminal Enhancement**
Add intelligence to SSH/WinRM handlers to detect container management commands:
- Detect `kubectl get pods` → Parse output → Render as spreadsheet
- Detect `docker ps` → Parse output → Render as spreadsheet
- Detect `kubectl logs <pod>` → Stream with terminal rendering
- Regular commands → Normal terminal rendering

**Option 2: Explicit Container Mode**
Add a connection parameter `mode=container` that:
- Automatically runs listing commands on connect
- Presents spreadsheet UI by default
- Allows switching to shell mode on demand
- Parses kubectl/docker output for structured display

### Connection Parameters (Option 2)

```rust
pub struct SshConfig {
    // Existing SSH params...
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,

    // New: Container management mode
    pub mode: Option<ConnectionMode>,
    pub container_backend: Option<ContainerBackend>,
}

pub enum ConnectionMode {
    Terminal,      // Normal SSH terminal (default)
    Container,     // Container management UI
}

pub enum ContainerBackend {
    Kubernetes,    // Use kubectl commands
    Docker,        // Use docker commands
    AutoDetect,    // Try both, show what's available
}
```

**Flow:**
1. User connects with `mode=container`
2. Handler SSHs to remote host
3. Handler runs `kubectl get pods -A -o json` (or `docker ps --format json`)
4. Handler parses JSON output
5. Handler renders spreadsheet view
6. User double-clicks pod → Handler runs `kubectl exec -it <pod> -- /bin/bash`
7. Switches to terminal mode for interactive shell

## UI Modes

### 1. List View (Default)

Spreadsheet-like interface showing containers/pods:

**Columns (Kubernetes):**
- Namespace
- Pod Name
- Container Name
- Status (Running/Pending/Error)
- Restarts
- Age
- CPU Usage
- Memory Usage
- Actions (Shell/Logs/Delete)

**Columns (Docker):**
- Container ID (short)
- Name
- Image
- Status
- Ports
- CPU Usage
- Memory Usage
- Actions (Shell/Logs/Stop/Delete)

**Rendering:** Use Guacamole drawing instructions (rect, text, line) like database spreadsheet

**Interactions:**
- Click row → Select container
- Double-click → Open shell
- Right-click → Context menu (Logs/Shell/Delete)
- Filter bar at top for search
- Namespace/Context dropdown

### 2. Shell Mode

Terminal access to container:

**Kubernetes:** Execute `/bin/bash` or `/bin/sh` via `kubectl exec`
```bash
kubectl exec -it <pod> -c <container> -n <namespace> -- /bin/bash
```

**Docker:** Execute shell via Docker API
```bash
docker exec -it <container> /bin/bash
```

**Rendering:** Reuse terminal renderer from SSH handler
- Same JPEG + fontdue approach
- Same 1024x768 size cap
- Same 16ms debounce
- Same OSC 52 clipboard support

### 3. Logs Mode

Stream container logs in terminal-like view:

**Features:**
- Tail last N lines (default 100)
- Follow mode (live streaming)
- Timestamp toggle
- Filter by log level (if structured)
- Download logs as file

**Rendering:** Terminal-like with read-only output (no input)

### 4. Metrics Mode (Optional)

Real-time resource usage graphs:

**Metrics:**
- CPU usage (%)
- Memory usage (MB)
- Network I/O (KB/s)
- Disk I/O (KB/s)

**Rendering:** Simple bar charts using Guacamole rect instructions

## Protocol Flow

### Initial Connection

1. Client sends connection parameters
2. Handler authenticates to K8s/Docker
3. Handler sends list view with current containers
4. Wait for user interaction

### Mode Switching

**List → Shell:**
```
Client: select_container,<container_id>
Client: exec_shell
Handler: switch_mode,shell
Handler: <terminal output via JPEG images>
```

**Shell → List:**
```
Client: exit (or Ctrl+D in shell)
Handler: switch_mode,list
Handler: <spreadsheet drawing instructions>
```

**List → Logs:**
```
Client: select_container,<container_id>
Client: stream_logs,tail=100,follow=true
Handler: switch_mode,logs
Handler: <log output via terminal rendering>
```

### List View Updates

Handler polls for container status changes every 5 seconds:
- New containers appear
- Status changes (Running → Stopped)
- Metrics updated

Dirty region optimization:
- Only redraw changed cells
- Use copy instruction for scrolling

## Implementation Plan (Revised for SSH/WinRM Extension)

### Phase 1: SSH Handler Extension (~500 lines)

**Files:**
- `crates/guacr-ssh/src/container_mode.rs` - Container mode logic
- `crates/guacr-ssh/src/handler.rs` - Add mode parameter handling
- `crates/guacr-terminal/src/spreadsheet.rs` - Reuse from database handlers

**Tasks:**
1. Add `mode` and `container_backend` parameters to SSH config
2. Implement JSON parsing for `kubectl get pods -A -o json`
3. Implement JSON parsing for `docker ps --format json`
4. Render parsed data as spreadsheet (reuse database code)
5. Handle mode switching (spreadsheet ↔ terminal)

**Advantages of This Approach:**
- Much smaller scope (~500 lines vs ~1900 lines)
- No new external dependencies (kube-rs, bollard)
- Reuses existing SSH authentication (keys, passwords, etc.)
- Works through firewalls (only SSH port needed)
- Automatic session recording (already works for SSH)
- No kubeconfig or Docker socket complexity

### Phase 2: Interactive Commands (~300 lines)

**Tasks:**
1. Detect pod/container selection in spreadsheet
2. Build `kubectl exec` command string
3. Execute command over existing SSH channel
4. Switch to terminal mode for interactive shell
5. Allow returning to spreadsheet view (Ctrl+D or exit)

### Phase 3: Logs & Metrics (~200 lines)

**Tasks:**
1. Add "View Logs" action in spreadsheet
2. Run `kubectl logs -f <pod>` over SSH
3. Display logs in terminal view with line wrapping
4. Add "View Metrics" action
5. Run `kubectl top pod <pod>` and display results

### Phase 4: WinRM Support (~300 lines)

**Files:**
- `crates/guacr-winrm/src/container_mode.rs` - Same logic for WinRM

**Tasks:**
1. Add container mode to WinRM handler
2. Run same kubectl/docker commands via PowerShell
3. Parse same JSON output
4. Reuse all spreadsheet rendering code

## Security Considerations

### Authentication

**All authentication happens at SSH/WinRM layer:**
- SSH key or password authentication (already implemented)
- WinRM with Kerberos or certificate auth
- No need for separate kubeconfig or Docker credentials
- User's permissions on remote host determine container access

### Authorization

**Controlled by remote host permissions:**
- Read-only mode (disable exec commands in handler)
- User's kubectl RBAC permissions apply naturally
- User's Docker group membership determines access
- Audit all actions (SSH session + commands logged)

### Benefits of SSH/WinRM Approach

1. **Defense in depth** - Must pass SSH auth + host permissions + container RBAC
2. **Existing audit trails** - SSH logs show who connected and what commands ran
3. **No credential sprawl** - No kubeconfig files or Docker TLS certs to manage
4. **Bastion architecture** - Follows security best practice of jump hosts
5. **Network isolation** - Container hosts don't need external access

### Session Recording

Record all activity:
- Container list views (screenshots)
- Shell sessions (asciicast + video)
- Log queries (metadata)
- Metrics views (screenshots)

## Example User Flow

### 1. Connect to Kubernetes Cluster

User provides:
```json
{
  "protocol": "container",
  "backend_type": "kubernetes",
  "kubeconfig": "/home/user/.kube/config",
  "context": "production",
  "namespace": "default"
}
```

### 2. See Container List

Browser shows spreadsheet with pods:
```
┌──────────────┬─────────────────┬──────────┬─────────┬─────────┬─────┐
│ Namespace    │ Pod             │ Status   │ CPU     │ Memory  │ Age │
├──────────────┼─────────────────┼──────────┼─────────┼─────────┼─────┤
│ default      │ web-app-7d9f5   │ Running  │ 12%     │ 256MB   │ 2d  │
│ default      │ api-service-3a1 │ Running  │ 45%     │ 512MB   │ 5h  │
│ kube-system  │ coredns-8f7a2   │ Running  │ 2%      │ 64MB    │ 30d │
└──────────────┴─────────────────┴──────────┴─────────┴─────────┴─────┘
```

### 3. Double-Click Pod → Shell Opens

Browser switches to terminal view showing:
```
user@web-app-7d9f5:/app$ ls
index.js  package.json  node_modules/
user@web-app-7d9f5:/app$
```

### 4. Type Commands, Get Results

Just like SSH - full terminal emulation with:
- Copy/paste (OSC 52 + bracketed paste)
- Vim/tmux support
- Scrollback buffer
- Fast JPEG rendering

### 5. Exit Shell → Return to List

User types `exit`, browser returns to container list view.

## Comparison to Existing Solutions

### vs. Kubernetes Dashboard
- **Advantage:** Browser-only, no port-forward needed
- **Advantage:** Integrated with Keeper security/recording
- **Advantage:** Fast spreadsheet navigation
- **Disadvantage:** Read-only by default for security

### vs. kubectl CLI
- **Advantage:** No local installation needed
- **Advantage:** Visual interface for non-experts
- **Advantage:** Works from any device with browser
- **Disadvantage:** Network latency for commands

### vs. Lens Desktop
- **Advantage:** No desktop app installation
- **Advantage:** Centralized access control
- **Advantage:** Built-in session recording
- **Disadvantage:** Less feature-rich (initially)

## Future Enhancements

1. **Multi-container view** - Split screen for multiple shells
2. **File browser** - Upload/download files to/from containers
3. **Port forwarding** - Expose container ports through WebRTC
4. **YAML editor** - Edit deployments/configs in-browser
5. **Helm integration** - Browse and install Helm charts
6. **Event timeline** - Show K8s events for debugging
7. **Cost metrics** - Show resource costs per container/pod

## Technical Notes

### Why Spreadsheet View?

- Proven UI pattern (database handlers use this)
- Efficient for many containers (100s of rows)
- Sortable, filterable, searchable
- Works well with Guacamole drawing instructions
- Lower bandwidth than full HTML table

### Why Reuse Terminal Infrastructure?

- SSH handler has proven JPEG rendering
- Same performance optimizations apply
- Clipboard, resize, recording all work
- No need to reinvent terminal emulation
- Consistent user experience across protocols

### Kubernetes Client Choice

**kube-rs** chosen over kubectl subprocess:
- Native async Rust
- No subprocess overhead
- Proper error handling
- Streams (exec, logs) via WebSocket
- Active maintenance

### Docker Client Choice

**bollard** chosen over docker CLI:
- Native async Rust
- No subprocess overhead
- Full Docker API coverage
- Active maintenance
- TLS support built-in

## Development Estimate (Revised)

- **Phase 1 (SSH Extension):** 2-3 days (~500 lines, JSON parsing + spreadsheet)
- **Phase 2 (Interactive):** 1-2 days (~300 lines, command handling)
- **Phase 3 (Logs/Metrics):** 1 day (~200 lines, view modes)
- **Phase 4 (WinRM):** 1-2 days (~300 lines, port to WinRM)

**Total:** 5-8 days for full implementation

**Comparison to Original Approach:**
- Original: 8-12 days, 1900 lines, new dependencies (kube-rs, bollard)
- Revised: 5-8 days, 1300 lines, zero new dependencies

**Why Faster:**
- No Kubernetes/Docker API client complexity
- Reuses existing SSH infrastructure (auth, channels, recording)
- Simpler testing (mock SSH output vs mock K8s API)
- No network/firewall issues (goes through SSH)

## Success Criteria

1. Can SSH to bastion host with `mode=container` parameter
2. Can list Kubernetes pods via `kubectl get pods -A -o json`
3. Can list Docker containers via `docker ps --format json`
4. Can render pod/container list as interactive spreadsheet
5. Can double-click pod → exec into container with interactive shell
6. Can stream logs with `kubectl logs -f <pod>`
7. Can view metrics with `kubectl top pod <pod>`
8. Can copy/paste to/from container shell (already works - OSC 52)
9. All sessions recorded and auditable (already works - SSH recording)
10. Performance similar to SSH handler (16ms debounce, 60fps, JPEG)
11. Works through firewalls (only needs SSH port 22)
12. Works with user's existing kubectl/docker permissions (no special config)
13. WinRM version works identically on Windows hosts
14. Proper error handling (kubectl not found, permission denied, etc.)
15. No crashes or panics under normal usage
