# Remote Browser Isolation (RBI) - Quick Reference

## What is RBI?
Remote Browser Isolation in KCM is a Guacamole protocol handler (`libguac-client-http.so`) that runs an isolated Chromium browser process and streams its output through the Guacamole protocol to users.

---

## Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| **libguac-client-http** | `/core/packages/kcm-libguac-client-http/` | Main RBI implementation (protocol handler) |
| **libcef** | `/core/packages/kcm-libcef/` | Chromium Embedded Framework (browser engine) |
| **HTTP Handler** | `src/http/http.c` | Protocol handler (910 lines) |
| **CEF Integration** | `src/cef/` | Browser integration (40+ files) |
| **Shared IPC** | `src/shared_ipc/` | Inter-process communication |
| **Autofill Rules** | `autofill-rules.yml` | Credential injection configuration |
| **Build Spec** | `kcm-libguac-client-http.spec` | RPM build definition |

---

## Architecture Overview

```
User Browser → Guacamole Protocol → guacd Daemon → libguac-client-http.so
                                                         ↓
                                    Shared Memory + FIFOs (IPC)
                                                         ↓
                                    CEF Process (Isolated Browser)
                                         ↓
                                    Chromium Engine
```

---

## Security Layers

1. **Process Isolation**: Separate CEF process with different PID
2. **Namespace Isolation**: User & mount namespaces
3. **DBus Isolation**: Isolated mount point for DBus socket (KCM-436)
4. **System Call Filtering**: Seccomp & AppArmor profiles
5. **URL Filtering**: Allowlist-based navigation control
6. **Resource Filtering**: Separate pattern lists for stylesheets, scripts
7. **Clipboard Control**: Configurable size limits, can disable copy
8. **Feature Disabling**: Block downloads, file dialogs, etc.

---

## Key Features

### Input/Output
- Keyboard input (with international support)
- Mouse & touch events (iOS compatible via KCM-425)
- Display rendering (dirty rectangle optimization - KCM-297)
- Audio output (configurable PCM format)
- Session recording (compliance audit trails)
- Clipboard (bidirectional, configurable limits)

### Browser Automation
- Credential autofill from Keeper Vault
- TOTP code generation & autofill (KCM-350)
- Multi-stage login form support
- Navigation history control
- Persistent browser sessions

### Security
- URL allowlist enforcement
- Resource allowlist (scripts, stylesheets, fonts)
- SSL/TLS certificate handling
- Content Security Policy (CSP) respect
- Autofill protection (no accidental exposure)

---

## Data Flow

```
Input Flow:
User Input → Guacamole Protocol → Input FIFO → CEF Process → Browser

Output Flow:
Browser Rendering → Display FIFO → guacd → Guacamole Protocol → User
```

---

## Configuration

### Global Autofill Rules
**File**: `/etc/guacamole/autofill-rules.yml`

```yaml
- page: 'https://*.example.com/login'
  username-field: '#username'
  password-field: '#password'
  submit: 'button[type="submit"]'
```

### Connection Parameters (in Keeper Vault)
```
protocol: http
url: https://target-app.com
username: vault-reference
password: vault-reference
allowed-url-patterns: https://target-app.com*
allowed-resource-url-patterns: https://cdn.*.com/*
allow-url-manipulation: false
disable-audio: false
disable-copy: false
ignore-initial-ssl-cert: false
profile-storage-directory: /var/lib/guacamole/profiles/
```

---

## Build Process

### RBI Package Build
```bash
cd core/packages/kcm-libguac-client-http/
autoreconf -fi
./configure --with-cef=/usr/share/cef
make
make check
make install
```

### CEF Build (Very Long!)
```bash
# Downloads Chromium sources (~30+ minutes)
# Applies KCM patches
# Builds with custom GN defines
# Marked DO_NOT_PARALLELIZE (memory constraints)
```

### Build Output
```
/opt/keeper/lib/libguac-client-http.so     # Main protocol handler
/opt/keeper/libexec/cef_process            # CEF wrapper executable
/etc/guacamole/autofill-rules.yml          # Autofill config
/var/run/dbus/                             # DBus isolation mount
```

---

## Build Dependencies

**Critical**:
- kcm-libcef-devel (Chromium Embedded Framework)
- kcm-libguac-devel (Core Guacamole)
- kcm-libcotp-devel (TOTP support)

**Graphics**:
- cairo-devel, libjpeg-turbo-devel, libwebp-devel

**Data Parsing**:
- libyaml-devel (autofill rules), json-c-devel, curl-devel

**X11/Display**:
- libX11, libxcb, mesa-libgbm

---

## Performance

**Memory Per Connection**:
- Shared memory: ~50MB+
- guacd thread: ~10-20MB
- CEF process: ~150-300MB
- **Total: ~200-350MB per connection**

**CPU Usage**:
- Idle: <1%
- Typing: 5-15%
- Video: 30-50%

**Bandwidth**:
- Dirty rectangle optimization reduces by 30-70%
- WebP/JPEG compression
- Clipboard size limits (256KB-50MB)

---

## Important Patches

| Patch | Issue | Impact |
|-------|-------|--------|
| 0013-KCM-425 | iOS touch IDs | Maps randomized IDs to sequential integers |
| 0002/0003-KCM-405 | Clipboard limits | Configurable 256KB-50MB buffers |
| KCM-436 | DBus isolation | User namespaces (removed CAP_SYS_ADMIN need) |
| KCM-297 | Display optimization | Dirty rectangle tracking, pixel precision |
| KCM-372 | Frame sync | Prevents tearing, optimizes latency |
| KCM-350 | TOTP autofill | Time-based OTP generation |

---

## Shared Memory IPC

**Location**: `/dev/shm/HTTP_CEF_SHM_{unique_id}`

**FIFOs**:
- `display_fifo`: CEF → guacd (rendering updates)
- `input_fifo`: guacd → CEF (keyboard/mouse/clipboard)
- `audio_fifo`: CEF → guacd (audio packets)
- `log_fifo`: CEF → guacd (log messages)

**Flags**:
- `GUAC_BROWSER_STATE_TERMINATING`: Process stopping
- `GUAC_BROWSER_STATE_TITLE_UPDATED`: Page title changed
- `GUAC_BROWSER_STATE_URL_UPDATED`: URL changed
- `GUAC_BROWSER_STATE_CLIPBOARD_UPDATED`: Clipboard changed

---

## Common Issues & Solutions

| Issue | Cause | Solution |
|-------|-------|----------|
| Touch not working on iOS | Large randomized IDs | KCM-425 patch maps to integers |
| Autofill not triggering | Wrong selectors | Verify CSS/XPath in rules |
| Large clipboard crashes | Buffer overflow | Increase MAX_CLIPBOARD_LENGTH |
| Navigation blocked | Not in allowlist | Add URL to allowed-url-patterns |
| DBus errors | Missing isolation | Ensure KCM-436 patch applied |
| Session doesn't persist | No storage directory | Set profile-storage-directory |
| Downloads appear | Feature not disabled | Set disable-downloads: true |
| Blank screen | CEF crash | Check CEF_LOG_LEVEL, enable logging |

---

## KCM-specific Enhancements

**Total RBI Issues**: 60+ KCM tickets addressing:
- Chromium version updates
- Input/output optimization
- Security hardening
- Multi-stage login support
- TOTP automation
- Accessibility improvements
- Recording enhancements
- International keyboard support
- Platform compatibility (EL7, EL8, EL9)

---

## Integration Points

**Guacamole**: Registered as "http" protocol handler
**KCM**: License-gated feature, integrates with Keeper Vault
**Docker**: Included in keeper/guacamole image
**Configuration**: `/etc/guacamole/guacamole.properties`

---

## Key Files

| File | Lines | Purpose |
|------|-------|---------|
| src/http/http.c | 910 | Main protocol handler |
| src/cef/cef_main.c | 252 | CEF process entry point |
| src/shared_ipc/shared_data.h | 600+ | IPC structure definition |
| src/cef/autofill.c | Major | Credential injection |
| src/http/input.c | Major | Input event handling |
| autofill-rules.yml | Configurable | Autofill rule examples |
| dbus.c | Major | DBus isolation (KCM-436) |

---

## Debugging

**Enable Logging**:
```bash
LOG_LEVEL=debug  # In /etc/guacamole/guacamole.properties
CEF_LOG_LEVEL=verbose
```

**Check Shared Memory**:
```bash
ls -la /dev/shm/HTTP_CEF_SHM_*
du -sh /dev/shm/HTTP_CEF_SHM_*
```

**Monitor Processes**:
```bash
ps aux | grep -E 'guacd|cef_process'
strace -p <PID>  # Trace system calls
```

**Examine Session Recording**:
```bash
guaclog /var/lib/guacamole/recordings/*.gz | head -100
```

---

## Testing

**Unit Tests**:
```bash
make check  # Runs CUnit tests in src/shared_ipc/tests/
```

**Manual Testing**:
1. Configure RBI connection in KCM
2. Connect via web browser
3. Verify display rendering
4. Test keyboard/mouse input
5. Test clipboard operations
6. Verify autofill (if configured)
7. Test URL allowlist enforcement

---

