# Infrastructure Resource Browser (K8s / Docker / VMs / Bare Metal)

## Overview

A protocol handler for browsing and accessing infrastructure resources through a browser-based
interface. Similar to how database handlers provide spreadsheet-like views of database tables,
this handler provides intuitive views of containers, pods, VMs, and other compute resources.

Covers: Kubernetes, Docker, Podman, Nomad, vSphere, Proxmox, AWS EC2, Azure VMs, GCP Compute,
libvirt/KVM, and bare metal (IPMI/Redfish).

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

**Rendering:** Use the Ratatui rendering pipeline from `guacr-terminal` (see
`docs/RATATUI_RENDERING_PLAN.md`). A `ratatui::widgets::Table` with `TableState` renders the
resource list into a `TestBackend` buffer, which is then converted to JPEG via
`TerminalRenderer::render_ratatui_buffer`. This replaces the hand-built `SpreadsheetRenderer`
(~1,600 lines of pixel math) with Ratatui's Table widget. The `SpreadsheetRenderer` can be
retired once this is in place.

Layout (3-zone vertical split):
```
┌─────────────────────────────────────────────┐
│ Namespace: [default    ] Filter: [_         ] │  <- 3 rows, Paragraph widgets
├─────────────────────────────────────────────┤
│ NAMESPACE  │ POD              │ STATUS    │ S │
│──────────────────────────────────────────│ c │
│ default    │ web-app-7d9f5    │ Running   │ r │  <- Table + TableState, Scrollbar
│ default    │ api-service-3a1  │ Running   │ o │
│ kube-sys   │ coredns-8f7a2    │ Running   │ l │
├─────────────────────────────────────────────┤
│ [S]hell  [L]ogs  [D]escribe   3/47 pods       │  <- 1 row, status/action bar
└─────────────────────────────────────────────┘
```

Status badges use color via `ratatui::style::Color`:
- Running → `Color::Green`
- Pending → `Color::Yellow`
- Error / CrashLoopBackOff → `Color::Red`
- Terminating → `Color::DarkGray`

**Interactions:**
- Click row → Select container
- Double-click / Enter → Open shell
- [S] / [L] / [D] shortcuts → Shell / Logs / Describe
- Filter bar at top for search (typed into a `Paragraph` input widget)
- Namespace dropdown (cycle with Tab)

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

**Prerequisite:** `docs/RATATUI_RENDERING_PLAN.md` Part 1 must land first
(`RatatuiRenderer` in `guacr-terminal`). The list view is built on
`ratatui::widgets::Table` + `TerminalRenderer::render_ratatui_buffer` — not
`SpreadsheetRenderer`.

**Tasks:**
1. Add `mode` and `container_backend` parameters to SSH config
2. Implement JSON parsing for `kubectl get pods -A -o json`
3. Implement JSON parsing for `docker ps --format json`
4. Render parsed data via Ratatui Table widget → `render_ratatui_buffer` → JPEG
5. Handle mode switching (ratatui list view ↔ TerminalEmulator shell/logs)

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

### Why Ratatui for the list view?

- `Table` + `TableState` handles selection, scrolling, column constraints — no pixel math
- Layout engine handles the filter bar, status bar, and action row at zero extra cost
- Colored status badges (Running=green, Error=red) are a one-liner with `Style::fg`
- Same fontdue JPEG output as existing terminal handlers — no new rendering infrastructure
- `SpreadsheetRenderer` (~1,600 lines) can be retired once the Ratatui pipeline lands
- Shell/Logs mode stays on the existing `TerminalEmulator` + vt100 JPEG path — unchanged

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

### Platform Coverage and "Entering" Resources

The two mechanisms for getting a shell into a resource are fundamentally different depending on
whether it's a container or a VM:

**Containers (K8s, Docker, Podman, Nomad)** — use the orchestrator's exec API directly. No SSH
needed. The API returns a WebSocket stream that pipes into `TerminalEmulator`. `ActionResult::Terminal`
wraps this stream.

**VMs (vSphere, EC2, Azure, Proxmox, KVM, bare metal)** — the PAM vault already holds credentials.
"Entering" means: get the VM's IP from the platform API → pull credentials from vault params →
open a standard SSH connection to that IP using the existing SSH handler. No new console protocols
needed. SSH works for every VM platform.

The only cases where SSH isn't an option are VMs with no SSH daemon (emergency console access,
locked-out machines). These require VNC/SPICE (Proxmox), WMKS (vSphere), or SOL/Redfish (bare
metal) — complex proprietary protocols that are out of scope for the initial implementation.

**Rust crate status per platform:**

| Platform | List crate | Enter (shell/desktop) | Notes |
|---|---|---|---|
| Kubernetes | `kube` (in codebase) | `kube` WebSocket exec → `ActionResult::Terminal` | `ws` feature required |
| Docker | `bollard` (in codebase) | `bollard` exec + TTY + resize → `ActionResult::Terminal` | |
| Podman | `podman-rest-client` or `podtender` | Same exec API as Docker | Do NOT use bollard — API compatibility issues |
| Nomad | `nomad-client-rs` | WebSocket alloc exec → `ActionResult::Terminal` | May need `tokio-tungstenite` directly |
| Active Directory | `ldap3` | RDP or SSH to selected computer → `ActionResult::NewSession` | See credentials section below |
| vSphere | custom REST client (in codebase) | SSH or RDP to VM guest IP → `ActionResult::NewSession` | WMKS console out of scope |
| Proxmox | `proxmox-api` | SSH or RDP to VM guest IP → `ActionResult::NewSession` | |
| AWS EC2 | `aws-sdk-ec2` | SSH to instance IP → `ActionResult::NewSession`; SSM plugin protocol proprietary, no Rust crate | |
| Azure VMs | `azure_mgmt_compute` | SSH or RDP to VM IP → `ActionResult::NewSession` | No Azure Bastion/Serial Console path in Rust |
| GCP Compute | `google-cloud-compute-v1` or `gcloud-sdk` | SSH to VM IP → `ActionResult::NewSession` | No IAP tunnel or serial console in Rust |
| libvirt/KVM | `virt` (FFI, requires `libvirt-dev`) | `open_console()` → serial console → `ActionResult::Terminal`; or SSH → `ActionResult::NewSession` | |
| Bare metal | `ipmi-rs` / `rust-ipmi`; `reqwest` for Redfish | SSH to BMC if available; SOL not in any Rust crate | SOL = `ipmitool sol activate` equivalent |

### ActionResult::NewSession — Spinning Off a Separate Session

For resources where "entering" means opening a full RDP or SSH session rather than an exec
stream within the current connection, a new `ActionResult` variant is needed:

```rust
pub enum ActionResult {
    Terminal { reader, writer },  // existing — exec into container, virsh console, etc.
    Status(String),               // existing
    Refresh,                      // existing

    /// Spin off a new independent session to a discovered resource.
    ///
    /// The resource browser stays open (user keeps the list view).
    /// The gateway opens a parallel session using these params.
    NewSession {
        protocol: String,              // "ssh", "rdp", "vnc"
        hostname: String,
        port: u16,
        params: HashMap<String, String>, // credential hints — see below
    },
}
```

Unlike `ActionResult::Terminal`, which hijacks the current connection's rendering pipeline,
`ActionResult::NewSession` signals to the **gateway** to open a separate WebRTC tube for the
new session. The user ends up with two concurrent sessions: the resource browser and the
machine session. This mirrors how a user would manually open a separate SSH/RDP connection
from a list.

### Credentials for NewSession — the unsolved part

When the resource browser discovers a machine (an AD computer, a vSphere VM, a Proxmox guest)
and the user wants to connect, credentials for that specific machine are needed. There are
three approaches:

**Option A — Re-use the browser session's credentials (simplest, limited)**
The domain admin or API credentials used to authenticate to AD/vSphere/Proxmox are passed
through as the machine credentials. Works if the same account has RDP/SSH access to the
discovered machines (e.g., domain admin can RDP anywhere). Does not work for least-privilege
setups where machine credentials differ from the API credentials.

**Option B — Gateway credential lookup by hostname (correct PAM approach)**
When the resource browser returns `ActionResult::NewSession`, the gateway looks up a vault
record for the discovered hostname before opening the new session. The handler only provides
the hostname and protocol hint; the gateway injects the credentials.

This is the proper PAM model — the handler never touches credentials for the target machine,
and credential access is enforced by vault policy. The handler's `params` map would carry a
`record_uid_hint` or `hostname` field that the gateway resolves against the vault.

**Option C — Present a credential picker in the browser (most flexible)**
If no vault record exists for the hostname, the gateway surfaces a picker to let the user
select which vault record to use for the new session. Out of scope for initial implementation
but the right long-term answer for machines not yet in the vault.

**Initial implementation:** Option A for the first pass (re-use browser credentials). Option B
wired in when the gateway's credential-lookup-by-hostname path is available.

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
