# Remote Browser Isolation (RBI) Implementation in KCM - Comprehensive Analysis

## Executive Summary

The Keeper Connection Manager (KCM) implements Remote Browser Isolation through the **libguac-client-http** protocol handler, which provides web content interaction using Apache Guacamole and the Chromium Embedded Framework (CEF). This is a complete browser isolation implementation that runs a separate isolated Chromium browser process and streams its rendering output through the Guacamole protocol.

---

## 1. RBI Components Location and Structure

### 1.1 Main RBI Package
**Location**: `/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/`

**Key Files and Directories**:
```
kcm-libguac-client-http/
├── kcm-libguac-client-http.spec          # RPM build specification
├── extra/libguac-client-http/            # Source code
│   ├── Makefile.am                       # Build configuration
│   ├── configure.ac                      # Autoconf configuration
│   ├── autofill-rules.yml                # Default autofill rules
│   ├── src/
│   │   ├── common/                       # Common utilities (clipboard, JSON, string handling)
│   │   ├── shared_ipc/                   # Inter-process communication between guacd and CEF
│   │   ├── http/                         # HTTP/RBI protocol handler (910 lines)
│   │   └── cef/                          # CEF (Chromium) integration (40+ files)
│   └── NOTICE                            # License information
├── EXCLUDED_BUILDS                       # Build exclusions
└── EXCLUDED_ARCHITECTURES                # Unsupported architectures
```

### 1.2 CEF (Chromium Embedded Framework) Support
**Location**: `/Users/mroberts/Documents/kcm/core/packages/kcm-libcef/`

**Purpose**: Provides precompiled Chromium Embedded Framework binaries with headless Ozone platform support

**CEF Version**: 138.0.15+gd0f1f64+chromium_138.0.7204.50

**Architecture**: 
- x86_64, aarch64 support
- Built with custom GN defines for headless operation
- Includes KCM-specific patches for D-Bus isolation

---

## 2. How RBI is Implemented

### 2.1 Implementation Type: **Guacamole Protocol Handler**

RBI is NOT a standalone service but a **protocol handler plugin** for Apache Guacamole:

- **Plugin Architecture**: libguac-client-http.so is a dynamically loadable shared library
- **Loading Mechanism**: Guacd loads it as a protocol handler when "http" protocol is requested
- **Integration Point**: Plugs into guacd's protocol handler framework (similar to RDP, VNC, SSH handlers)

### 2.2 Component Architecture

```
User/Browser
    ↓
WebSocket
    ↓
Guacamole Web Application (Tomcat/Java)
    ↓
Guacamole Protocol (TCP/ZMQ)
    ↓
guacd Daemon (Single Process, Multi-threaded)
    │
    ├── Input Thread (reads Guacamole protocol)
    │
    ├── HTTP Protocol Handler Thread
    │   │
    │   ├── guac_http_client_thread() - Main handler
    │   │
    │   ├── Input Processing Thread (reads from IPC FIFOs)
    │   │
    │   ├── Display Processing Thread (renders from IPC FIFOs)
    │   │
    │   └── Shared Memory (Communication with CEF)
    │
    └── Output Thread (writes display updates)
    
    ↓
    
CEF Process (Separate Process - Isolated)
    │
    ├── CEF Browser Process
    │   ├── Rendering Engine
    │   ├── Network Stack
    │   └── Resource Handling
    │
    └── CEF Render Process(es)
        ├── JavaScript Execution
        ├── DOM Manipulation
        └── Autofill Integration
```

---

## 3. RBI Architecture - Browser Isolation

### 3.1 Process-Level Isolation

**Key Insight**: RBI uses **process-level isolation with namespace separation**

```
Main guacd Process (UID: guacd, GID: guacd)
    │
    └─fork()──→ CEF Process (Separate Process Tree)
               │
               ├── PID: Separate from guacd
               ├── User Namespaces: Isolated user namespace via mount namespaces
               ├── DBus Isolation: Isolated mount point for DBus socket
               ├── Mount Namespaces: Custom /var/run/dbus isolation
               └── Limits: Configurable clipboard, URL patterns, resource constraints
```

### 3.2 Inter-Process Communication (IPC)

**Method**: Shared Memory + Named FIFOs

**Shared Memory Structure** (`shm_memory`):
- Located at: `/dev/shm/HTTP_CEF_SHM_{unique_id}`
- Contains multiple FIFOs for different data types:
  - `display_fifo`: Display/rendering updates from CEF to guacd
  - `input_fifo`: Input events from guacd to CEF
  - `audio_fifo`: Audio packets from CEF to guacd
  - `log_fifo`: Log messages from CEF to guacd

**Browser State Flags**:
- `GUAC_BROWSER_STATE_TERMINATING`: Browser process terminating
- `GUAC_BROWSER_STATE_TITLE_UPDATED`: Page title changed
- `GUAC_BROWSER_STATE_URL_UPDATED`: Current URL changed
- `GUAC_BROWSER_STATE_CLIPBOARD_UPDATED`: Clipboard changed

**Synchronization**: Using `guac_flag` (atomic flag operations) and pthread mutexes

### 3.3 Security Isolation Mechanisms

#### A. DBus Isolation (KCM-436)
**File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/extra/libguac-client-http/src/http/dbus.c`

```c
// Functions:
int guac_http_setup_isolated_dbus(guac_client* client, shm_memory* shm);
int guac_http_cleanup_isolated_dbus(guac_client* client, shm_memory* shm);

// Implementation:
// - Uses user namespaces for DBus isolation
// - Mount namespace for private /var/run/dbus mount point
// - Eliminates CAP_SYS_ADMIN requirement (previously needed)
// - Creates isolated DBus environment per CEF process
```

**Files Created**: `/var/run/dbus/` directory per connection with isolated socket

#### B. AppArmor Profile
**File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/guacd-apparmor-profile`

- Mandatory access control for guacd and CEF processes
- Restricts system call exposure
- Prevents escape from isolation

#### C. Docker Security Profile
**File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/extra/docker-seccomp.json`

- Seccomp profile limiting allowed system calls
- Restricts capabilities for containerized deployment

### 3.4 Resource Constraints

**Shared Memory Limits** (from `shared_limits.h`):
- `MAX_CLIPBOARD_LENGTH`: Configurable (256KB to 50MB)
- `MAX_URL_LENGTH`: Maximum URL length
- `MAX_PROFILE_STORAGE_DIRECTORY_LENGTH`: Session storage path length
- `MAX_ALLOWED_URL_PATTERNS`: Maximum number of allowed URL patterns
- `MAX_WIDTH`, `MAX_HEIGHT`: Display resolution limits
- `IMAGE_DATA_STRIDE`: RGBA pixel data stride calculation

---

## 4. Guacamole Protocol Integration

### 4.1 Protocol Handler Interface

RBI implements the standard Guacamole protocol handler interface:

```c
// Handler functions (from client.h):
guac_client_free_handler guac_http_client_free_handler;

// Exported functions:
void* guac_http_client_thread(void* data);  // Main thread entry point
```

### 4.2 Data Flow

```
User Input (from Web Browser)
    ↓
Guacamole Protocol Message
    ↓
guacd Input Handler
    ↓
HTTP Protocol Handler (input thread)
    ↓
Input FIFO (in shared memory)
    ↓
CEF Process reads input
    ↓
CEF processes input (keyboard, mouse, clipboard, navigation)
    ↓
Chromium rendering
    ↓
CEF notifies guacd via Display FIFO
    ↓
guacd reads Display FIFO
    ↓
Guacamole Output Instructions
    (paint, cursor, clipboard, audio)
    ↓
User's Browser (rendered output)
```

### 4.3 Protocol Features Supported

- **Keyboard Input**: Full keyboard event handling with Japanese support (KCM-413)
- **Mouse Events**: Position, buttons, scroll
- **Touch Events**: Mapped to sequential integer IDs (KCM-425 patch)
- **Clipboard**: Bidirectional clipboard with size limits (KCM-405)
- **Audio**: PCM format with configurable sample rate, channels, bps
- **Display**: Rendering updates with dirty rectangle tracking
- **Navigation**: History navigation (back/forward/refresh)
- **Session Recording**: Full protocol recording capability (KCM-310)

---

## 5. Patches and Modifications for RBI

### 5.1 RBI-Specific Patches

#### Patch: `0013-KCM-425-Fix-RBI-handling-of-touch-events-on-iOS-devi.patch`
**Location**: `/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-client/patches/`

**Issue**: iOS devices generate large, randomized touch IDs (incompatible with server expectations)

**Solution**: 
- Implemented `Guacamole.IntegerPool` for ID mapping
- Maps local touch IDs to sequential, predictable integer IDs
- Functions:
  - `mapIdentifier()`: Convert local to Guacamole IDs
  - `unmapIdentifier()`: Clean up completed touch events
  - Enables reuse of touch IDs

**Impact**: RBI can now handle iOS devices properly

### 5.2 Related Guacamole Server Patches (affecting RBI)

**Clipboard Limits** (KCM-405):
- `0002-KCM-405-Add-ability-to-configure-clipboard-limits.patch`
- `0003-KCM-405-Improve-warnings-about-a-wrong-clipboard-buf.patch`
- Configurable clipboard buffer (256KB to 50MB)

**Recording Improvements** (KCM-310, KCM-472):
- `0009-KCM-472-Backport-generalization-of-recording-typescr.patch`
- Enhanced recording capability for isolated browser sessions

---

## 6. Build System for RBI Components

### 6.1 RBI Package Build Process

**File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/kcm-libguac-client-http.spec`

#### %prep Phase:
- Bundled source (`extra/`) includes all RBI code
- No external download (unlike guacamole-server which clones from GitHub)

#### %build Phase:
```bash
cd extra/libguac-client-http/
autoreconf -fi              # Generate configure script
./configure --disable-static \
    --with-cef="/usr/share/cef" \
    LDFLAGS="-L/usr/lib" \
    CPPFLAGS="-I/usr/include"
make V=1 %{?_smp_mflags}    # Parallel build
```

#### %check Phase:
```bash
LD_LIBRARY_PATH="/usr/lib" make check
```

#### %install Phase:
```bash
make install DESTDIR=$RPM_BUILD_ROOT
# Generate SBOM for binaries:
#   - libguac-client-http.so (shared library)
#   - cef_process (CEF execution wrapper)
# Install autofill rules: /etc/guacamole/autofill-rules.yml
# Create DBus directory: /var/run/dbus
```

### 6.2 Build Dependencies

**BuildRequires**:
```
kcm-libcef-devel              # Chromium Embedded Framework headers/libs
kcm-libguac-devel             # Core Guacamole protocol library
kcm-common-base               # Common build utilities
cairo-devel                   # Graphics rendering
json-c-devel                  # JSON parsing
libyaml-devel                 # YAML parsing (for autofill rules)
curl-devel                    # URL pattern parsing
kcm-libcotp-devel             # TOTP code generation (for autofill)
```

**Additional X11/Graphics**:
- libX11, libXcomposite, libXdamage, libXrandr, libXfixes
- libxcb, libxkbcommon, mesa-libgbm
- at-spi2-atk, at-spi2-core (accessibility)
- nss, nspr (cryptography)

### 6.3 Build Output

**Package**: `kcm-libguac-client-http-{version}-{release}.el8.x86_64.rpm`

**Installs**:
```
/opt/keeper/lib/libguac-client-http.so      # Main protocol handler (shared library)
/opt/keeper/lib/libguac-client-http.so.*    # Version symlinks
/opt/keeper/lib/libguac_common.so           # Common utilities library
/opt/keeper/libexec/cef_process             # CEF process wrapper executable
/etc/guacamole/autofill-rules.yml           # Autofill configuration
/var/run/dbus/                              # DBus isolation mount point
/usr/share/libguac-client-http/NOTICE       # License
```

### 6.4 CEF Build Process

**File**: `/Users/mroberts/Documents/kcm/core/packages/kcm-libcef/kcm-libcef.spec`

**Process**:
1. Clone specific CEF commit from GitHub
2. Clone Chromium source via depot_tools
3. Apply CEF patches (KCM-specific):
   - `cef-patches/`: Custom CEF modifications
   - `chromium-patches/`: Chromium core modifications (D-Bus disabling)
4. Configure with custom GN defines:
   - File: `/Users/mroberts/Documents/kcm/core/packages/kcm-libcef/extra/gn-defines.gn`
   - Enables headless Ozone platform
5. Build binaries (with `./tools/build.py`)
6. Install to `/usr/share/cef/`

**Time Cost**: Very lengthy (approx. 30+ minutes on modern hardware)

**Marked**: `DO_NOT_PARALLELIZE` - memory constraints

---

## 7. Browser Session and Rendering/Input/Output Handling

### 7.1 Session Management

**Session Persistence**:
- `profile_storage_directory`: Configurable persistent browser session storage
- Allows browser sessions to be resumed across connections
- Path stored in shared memory structure
- Only one connection per profile directory (mutual exclusion)

**Session Structure**:
```c
typedef struct guac_http_client {
    guac_http_settings* settings;        // Connection settings (URL, patterns, autofill rules)
    pthread_t client_thread;             // Main HTTP handler thread
    shm_memory* shm;                     // Pointer to shared memory with CEF
    guac_display* display;               // Guacamole display surface (high-perf rendering)
    guac_display_layer* popup_layer;     // Popup overlay rendering
    guac_audio_stream* audio;            // Audio output stream
    guac_common_clipboard* clipboard;    // Clipboard management
    int frames_received;                 # Frame counter for flushing
    guac_recording* recording;           # Session recording (if enabled)
} guac_http_client;
```

### 7.2 Rendering Pipeline

**Display Rendering** (KCM-297, KCM-372):

```
CEF Rendering
    ↓
Paint Event to Display FIFO (with dirty rectangles)
    ↓
guacd Display Processing Thread
    ↓
Parse Display Events from FIFO
    ↓
guac_display Surface (high-performance Guacamole display)
    ↓
Dirty Rectangle Tracking (pixel-level precision - KCM-297)
    ↓
Generate Paint Instructions
    ↓
Frame Synchronization (KCM-372)
    ├── Automatic frame deferring during user joins
    ├── Cursor updates within frame boundaries
    ├── Regions outside frame bounds marked dirty
    └── Flush on mouse-only changes if no graphical pending
    ↓
WebP/JPEG Compression (configurable)
    ↓
Guacamole Protocol Output
    ↓
User's Browser
```

**Key Optimizations**:
- Dirty rectangle tracking reduces bandwidth
- Frame-level synchronization prevents tearing
- Support for transparency layers and WebP format
- Cursor position corrections to frame boundaries

### 7.3 Input Processing

**Input Handler Thread**:

```
Guacamole Protocol Input Message
    ↓
HTTP Input Handler (input.c)
    ↓
Convert to Input Event
    ├── Keyboard: Key code, modifiers
    ├── Mouse: X, Y, button, scroll
    ├── Clipboard: Text content
    ├── Navigation: Forward/Back/Refresh
    └── Resize: New display dimensions
    ↓
Input FIFO (in shared memory)
    ↓
CEF Process reads Input FIFO
    ↓
CEF Process Handler
    ├── Input.js validation
    ├── Keyboard focus handling
    ├── Field autofill integration
    └── Navigation policy enforcement
    ↓
Chromium Input Processing
```

**Input Features**:
- Keyboard shortcuts (Ctrl+A, Ctrl+C, Ctrl+V for clipboard)
- Mouse position and button tracking
- Touch event mapping (iOS compatibility via KCM-425)
- Navigation pipe streams for history control
- Input field focus management (KCM-301)

### 7.4 Audio Output Handling

**Audio Configuration**:
```c
typedef struct audio_output_format {
    int bps;            // 8, 16, or 32 bits per sample
    int channels;       // 1=mono, 2=stereo
    int sample_rate;    // Common: 44100Hz
} audio_output_format;
```

**Audio Pipeline**:
```
CEF Audio Processing
    ↓
Audio Packets → Audio FIFO (shared memory)
    ↓
guacd Audio Thread
    ↓
Convert PCM Format (bps, channels, sample_rate)
    ↓
Guacamole Audio Stream
    ↓
User Audio Output
```

**Features**:
- Configurable audio format (KCM-164)
- Configurable output bps, sample rate, channels (KCM-164)
- Can be disabled via `disable_audio` flag

---

## 8. Security Isolation Mechanisms

### 8.1 Multi-Layer Isolation Strategy

```
Layer 1: Process Level
├── Separate CEF process (different PID)
├── User namespaces (isolated user context)
└── Mount namespaces (isolated filesystem)

Layer 2: System Call Filtering
├── Seccomp profile (allowed system calls)
├── AppArmor profile (mandatory access control)
└── Capability dropping (CAP_SYS_ADMIN eliminated)

Layer 3: DBus Isolation (KCM-436)
├── Isolated mount point (/var/run/dbus)
├── Private DBus socket per connection
└── Prevents DBus escape vector

Layer 4: Browser Isolation
├── Content Security Policy (CSP)
├── Same-origin policy enforcement
├── Navigation allowlist (URL patterns)
└── Resource URL patterns (script/stylesheet filtering)

Layer 5: Clipboard Isolation
├── Configurable buffer limits (256KB-50MB)
├── Mime type tracking
├── Bidirectional clipboard mediation
└── Copy-to-clipboard disabling option (disable_copy)

Layer 6: Feature Disabling
├── Download blocking (KCM-336)
├── File browser dialog blocking (KCM-314)
├── Printing control (KCM-376 with user-initiated option)
└── Cookie/session isolation
```

### 8.2 URL-Based Access Control

**Allowed URL Patterns**:
- Pattern matching with wildcards and regex
- Examples:
  ```yaml
  - page: 'https://*.example.com/admin/*'
  - page: 'https://app.example.com'
  - page: 'www.linkedin.com'
  ```
- Enforced at navigation time
- Prevents exploitation of cross-origin requests

**Allowed Resource URL Patterns**:
- Separate pattern list for resource requests
- Applies to stylesheets, scripts, images, fonts
- Prevents XSS via malicious resource loading
- WebSocket and HTTP/HTTPS scheme only (KCM-327)

**Pattern Parser** (KCM-305):
- Regex-based parsing (rewritten from curl in KCM-305)
- Supports numeric domains (KCM-397)
- Truncates URLs to maximum length (KCM-330)
- Requires top-level domain (removed in KCM-420)

### 8.3 Autofill Security

**Autofill Configuration**: `/etc/guacamole/autofill-rules.yml`

```yaml
- page: 'https://*.okta.com/login'
  username-field: '#okta-signin-username'
  password-field: '#okta-signin-password'
  submit: '#okta-signin-submit'
```

**Security Features**:
- CSS/XPath selectors for precise field matching
- Only fills matching form fields
- Submit only to specified elements
- Prevents accidental secret exposure

**Autofill Enhancements**:
- TOTP code generation and autofill (KCM-350)
- TOTP timer refresh on schedule
- Manual interaction doesn't trigger autofill (KCM-431)
- Large rule list optimization (KCM-410)
- Does not interfere with user interaction

### 8.4 SSL/TLS Certificate Handling

**Options**:
- `ignore_initial_ssl_cert`: Skip cert validation for initial URL (KCM-404)
- Useful for self-signed certificates in internal environments
- Warning logged when option is ignored (KCM-458)

---

## 9. Keeper-Specific RBI Features

### 9.1 Keeper Security Manager (KSM) Integration

**Autofill from KSM**:
- Retrieves secrets from Keeper Vault
- Automatically fills credentials into web forms
- TOTP code generation and autofill (KCM-350)

**Connection Parameters**:
```
username: KSM field reference
password: KSM field reference
autofill-rules: Connection-specific autofill rules
allow-url-manipulation: User can change URL?
allowed-url-patterns: Whitelist of accessible URLs
allowed-resource-url-patterns: Whitelist for resources
```

### 9.2 Session Recording for Compliance

**Recording Features**:
- Full session recording for audit trails
- Screen capture of entire session
- Input events recording
- Can replay recorded sessions

**Use Cases**:
- Compliance requirements (HIPAA, SOC 2)
- Incident investigation
- User activity auditing

### 9.3 Clipboard Management

**Enhanced Clipboard** (KCM-405):
- Configurable size limits (256KB to 50MB)
- Balance between memory and usability
- Per-connection buffer management
- Warnings for buffer overflow

**Bidirectional Support**:
- Copy from browser to user (reported_clipboard)
- Paste from user to browser (requested_clipboard)
- Can disable copy direction (disable_copy)

### 9.4 Autofill Rules Storage

**Configuration File**: `/etc/guacamole/autofill-rules.yml`

**Scope**:
- Connection-specific rules override global rules
- Global rules apply to all connections
- Connection rules tried first, then global rules
- Supports multi-stage login forms

---

## 10. Data Flow Architecture

### 10.1 Complete End-to-End Flow

```
INITIATOR SIDE:
═══════════════

1. User Browser
   ├── Opens Guacamole web app
   ├── Authenticates with KCM
   └── Selects RBI connection

2. KCM Web Application (Java/Tomcat)
   ├── Retrieves connection from Keeper Vault
   ├── Connects to guacd (port 4822)
   └── Sends Guacamole handshake

GUACD SIDE:
═══════════

3. guacd Main Process
   ├── Accepts WebSocket from web app
   ├── Creates protocol handler thread
   └── Loads libguac-client-http.so

4. HTTP Protocol Handler (guac_http_client_thread)
   ├── Parses connection parameters
   ├── Creates shared memory structure
   ├── Allocates FIFOs for IPC
   ├── Starts input processing thread
   ├── Starts display processing thread
   └── **fork() → CEF Process**

CEF PROCESS SIDE:
═════════════════

5. CEF Child Process
   ├── Setup isolated DBus (mount namespace)
   ├── Initialize CEF browser engine
   ├── Create hidden browser window (headless Ozone)
   ├── Load initial URL from shared memory
   ├── Connect to CEF render process(es)
   └── Enter main event loop

6. CEF Browser Rendering Loop
   ├── Process keyboard/mouse input from input_fifo
   ├── Navigate URLs (with allowlist check)
   ├── Execute JavaScript
   ├── Autofill credentials (from Keeper)
   ├── Render display to framebuffer
   └── Write paint events to display_fifo

GUACD PROCESSING SIDE:
══════════════════════

7. Input Processing Thread
   ├── Read Guacamole protocol messages
   ├── Convert to input events
   ├── Write to input_fifo
   └── Track focus, clipboard changes

8. Display Processing Thread
   ├── Read display events from display_fifo
   ├── Track dirty rectangles
   ├── Compose layers (main + popup)
   ├── Generate Guacamole paint instructions
   ├── Handle cursor positioning
   └── Write output to socket

9. Audio Processing (if enabled)
   ├── Read audio packets from audio_fifo
   ├── Convert PCM format
   ├── Create audio stream
   └── Write audio output

BACK TO USER:
═════════════

10. Guacamole Protocol Output
    ├── paint: Display updates
    ├── cursor: Cursor position/image
    ├── audio: Audio packets
    ├── clipboard: Clipboard data
    ├── message: Page title/URL
    └── Sent via TCP/WebSocket to web app

11. User Browser Rendering
    ├── Receive Guacamole instructions
    ├── Render display on canvas
    ├── Play audio
    ├── Handle clipboard
    └── Display interactive remote browser
```

### 10.2 Message Routing Example: Keyboard Input

```
User Types "P"
  ↓
Browser JavaScript
  ↓
WebSocket to Guacamole Web App
  ↓
Guacamole Protocol: "key,54,80" (keydown, modifiers, keysym)
  ↓
guacd Input Handler Thread
  ↓
HTTP Protocol Handler (input.h/input.c)
  ├── Parse keyboard event
  ├── Validate focus
  └── Create input event structure
  ↓
Input FIFO (shared memory)
  ↓
CEF Process reads input_fifo (non-blocking poll)
  ↓
CEF Input Handler
  ├── Inject into focused element
  ├── Trigger input validation
  └── Fire input events in DOM
  ↓
Chromium Key Processing
  ├── Fire keydown event
  ├── Execute input handlers
  └── Update form state
  ↓
Form field gets "P"
  ↓
Page renders change
  ↓
CEF Paint Handler
  ├── Detect dirty region
  └── Write to display_fifo
  ↓
guacd Display Thread reads display_fifo
  ↓
Extract pixels from dirty region
  ↓
Compress with WebP/JPEG
  ↓
Generate Guacamole paint instruction
  ↓
Send to user's browser
  ↓
User sees "P" typed in form
```

---

## 11. Integration Points with Rest of System

### 11.1 Guacamole Server Integration

**Protocol Handler Plugin**:
- Loaded by guacd on demand
- Registered as "http" protocol in Guacamole
- Inherits from standard guac_client interface
- Uses shared libraries: libguac, libguac-terminal, Cairo

**Network Communication**:
- Receives input via Guacamole protocol (TCP/ZMQ/WebSocket)
- Sends output via Guacamole protocol
- Stateless design allows connection migration

**Resource Management**:
- Per-connection clipboard buffer management
- Display surface from libguac-display (KCM-297 optimization)
- Recording via guac_recording API

### 11.2 Keeper Connection Manager Integration

**Authentication & Authorization**:
- KCM web app authenticates user
- Retrieves RBI connection config from Keeper Vault
- Enforces license checks for RBI feature

**Secret Injection**:
- KSM retrieves credentials from encrypted vault
- HTTP protocol handler receives credentials
- CEF autofill injects into web forms
- Never exposes secrets to user directly

**Audit & Compliance**:
- Session recording for compliance
- Connection logging
- User activity tracking

### 11.3 Docker Integration

**Container Entrypoint**: `/opt/keeper/share/guacd/entrypoint-guacd.sh`
```bash
exec runuser -u guacd -- /opt/keeper/sbin/guacd -f "$@"
```

**Security Profile**: `/opt/keeper/share/guacd/docker-seccomp.json`
- Restricts system calls
- Prevents container escape

**Environment Variables**:
```bash
AUTOFILL_RULES=/path/to/custom-autofill-rules.yml
LOG_LEVEL=info
```

### 11.4 Configuration Integration

**Main Configuration**: `/etc/guacamole/guacamole.properties`
**RBI-Specific Config**: `/etc/guacamole/autofill-rules.yml`

**Connection Parameters** (in Keeper Vault):
```
protocol: http
url: https://app.example.com
username: <reference to vault username field>
password: <reference to vault password field>
allowed-url-patterns: https://app.example.com*
allow-url-manipulation: false
ignore-initial-ssl-cert: false
disable-audio: false
disable-copy: false
```

---

## 12. Build Dependencies and Library Interaction

### 12.1 Dependency Chain

```
kcm-libguac-client-http.so
    ├── kcm-libcef (Chromium Embedded Framework)
    │   └── libchromium, libcef (precompiled binaries)
    │
    ├── kcm-libguac (Core protocol library)
    │   ├── libguac.so (Guacamole protocol)
    │   └── libguac-terminal.so (Terminal emulation)
    │
    ├── kcm-libcotp (TOTP implementation)
    │   └── TOTP code generation/validation
    │
    ├── System Libraries
    │   ├── libcairo (graphics rendering)
    │   ├── libjpeg-turbo (JPEG compression)
    │   ├── libwebp (WebP compression)
    │   ├── libyaml (autofill rules parsing)
    │   ├── json-c (JSON parsing)
    │   ├── libcurl (URL pattern parsing)
    │   └── libX11, libxcb (X11 display)
    │
    └── Cryptography
        ├── libssl/libcrypto (SSL/TLS)
        └── libgcrypt (cryptographic functions)
```

### 12.2 Major Libraries Used

**CEF (Chromium Embedded Framework)**:
- Headless browser engine
- Version 138.0.15 (Chromium 138)
- Ozone platform for headless rendering
- Custom patches for D-Bus isolation

**Guacamole Libraries**:
- Core protocol implementation
- Display surface for rendering optimization
- Terminal emulation (inherited but not used for HTTP)
- Audio stream handling

**Graphics & Compression**:
- Cairo: Display rendering
- libjpeg-turbo: JPEG compression
- libwebp: WebP compression (modern format support)
- Pango: Text layout and rendering

**Data Format Parsing**:
- libyaml: Autofill rules (YAML format)
- json-c: JSON configuration
- libcurl: URL pattern matching (regex parsing in KCM-305 rewrite)

---

## 13. Relevant KCM-specific Enhancements

### 13.1 List of RBI-Related KCM Issues

| Issue | Description | Impact |
|-------|-------------|--------|
| KCM-164 | Initial RBI implementation | Foundation of HTTP protocol handler |
| KCM-273 | CEF compatibility updates | Chromium version synchronization |
| KCM-297 | Display surface optimization | Dirty rectangle tracking, pixel-level precision |
| KCM-300 | HTTP protocol custom fields | Connection parameter handling |
| KCM-301 | Input field focus handling | Proper focus management in forms |
| KCM-302 | Navigation allowlist | URL pattern enforcement |
| KCM-305 | Pattern parser rewrite | Regex-based matching (improved from curl) |
| KCM-307 | Page title updates | Browser state synchronization |
| KCM-310 | Session recording | RBI session capture for compliance |
| KCM-311 | Input/audio/clipboard disabling | Feature control options |
| KCM-312 | Dropdown rendering | UI element fixes |
| KCM-313 | Page background color | White background instead of black |
| KCM-314 | File browser blocking | Security hardening |
| KCM-315 | Frame loop timing | Latency optimization |
| KCM-316-319 | Multi-stage login/Azure support | Autofill enhancements |
| KCM-320 | ZMQ socket support | Distributed guacd (infrastructure feature) |
| KCM-321 | CEF error page handling | Better error reporting |
| KCM-322 | Persistent browser sessions | Session resumption capability |
| KCM-325 | WebSocket pattern support | Network protocol filtering |
| KCM-326 | Large viewport handling | Viewport size handling |
| KCM-327 | URL scheme restrictions | HTTP/HTTPS/WebSocket only |
| KCM-329 | Memory safety | Checked memory functions |
| KCM-330 | URL truncation | Maximum URL length enforcement |
| KCM-336 | Download blocking | Security hardening |
| KCM-340 | Recording file handling | Append mode for recordings |
| KCM-343 | CEF shutdown | Orderly process termination |
| KCM-346 | Profile storage | Temporary and persistent storage |
| KCM-350 | TOTP autofill | Time-based OTP generation |
| KCM-351 | Autofill rule parsing | YAML format fixes |
| KCM-354 | CSP error handling | Content Security Policy respect |
| KCM-364 | Period in patterns | URL pattern character support |
| KCM-366 | Captcha handling | Autofill blocking condition |
| KCM-372 | Frame synchronization | Display update timing |
| KCM-374 | Transparency handling | Layer transparency fixes |
| KCM-376 | User-initiated printing | Print capability |
| KCM-378 | DevTools protocol | Error page rendering |
| KCM-380 | FIPS compatibility | FIPS compliance |
| KCM-387 | SHM allocation | Failure handling |
| KCM-388 | SHM file descriptor | On-disk storage prevention |
| KCM-393 | CEF updates | Chromium version sync |
| KCM-396 | Allowed URL warnings | User notification |
| KCM-397 | Numeric domain support | IP address URL matching |
| KCM-404 | CEF logging & SSL | Cert validation and logging |
| KCM-405 | Clipboard limits | Configurable buffer size |
| KCM-408 | EL9 support | RHEL 9 compatibility |
| KCM-410 | Large rule lists | Performance optimization |
| KCM-413 | Japanese keyboard | International input support |
| KCM-417 | TOTP crash fix | Timer handling |
| KCM-420 | TLD requirement removal | URL pattern flexibility |
| KCM-425 | iOS touch events | Touch ID mapping |
| KCM-429 | SBOM generation | Software Bill of Materials |
| KCM-431 | Autofill interference | Manual interaction preservation |
| KCM-436 | DBus isolation | User namespace isolation (CAP_SYS_ADMIN removal) |
| KCM-458 | SSL cert warning | User notification |

---

## 14. Key Files Reference

### 14.1 RBI Source Code Structure

**Common Utilities** (`src/common/`):
- `clipboard.c/h`: Clipboard management
- `json.c/h`: JSON parsing
- `string.c/h`: String utilities
- `list.c/h`: Linked list implementation
- `iconv.c/h`: Character encoding conversion

**IPC Infrastructure** (`src/shared_ipc/`):
- `shared_data.c/h`: Shared memory structure definition (50+ fields)
- `display_fifo.c/h`: Display event queue
- `input_fifo.c/h`: Input event queue
- `audio_fifo.c/h`: Audio packet queue
- `log_fifo.c/h`: Log message queue
- `move_fd.c/h`: File descriptor passing
- `transfer_socket.c/h`: Socket transfer
- `shared_utils.c/h`: Utility functions

**HTTP Protocol Handler** (`src/http/` - 910 lines in http.c):
- `http.c/h`: Main protocol handler (guac_http_client_thread)
- `client.c/h`: Client initialization and cleanup
- `settings.c/h`: Connection settings and parameter parsing
- `input.c/h`: Input event processing
- `argv.c/h`: Argument parsing (connection parameters)
- `clipboard.c/h`: Clipboard synchronization
- `size.c/h`: Display size/zoom handling
- `user.c/h`: User management
- `pipe.c/h`: Navigation history pipe
- `dbus.c/h`: DBus isolation setup/cleanup

**CEF Integration** (`src/cef/` - 40+ files):
- `cef_main.c`: CEF process entry point (252 lines)
- `cef_app.c/h`: CEF application initialization
- `cef_client.c/h`: CEF client wrapper
- `cef_browser_process_handler.c/h`: Browser process lifecycle
- `cef_render_handler.c/h`: Display rendering
- `cef_load_handler.c/h`: Page loading
- `cef_request_handler.c/h`: Request interception
- `cef_resource_handler.c/h`: Resource loading
- `cef_resource_request_handler.c/h`: Request/response handling
- `cef_display_handler.c/h`: Display updates
- `cef_dialog_handler.c/h`: Dialog handling
- `cef_download_handler.c/h`: Download blocking
- `cef_life_span_handler.c/h`: Window lifecycle
- `cef_audio_handler.c/h`: Audio processing
- `autofill.c/h`: Credential autofill
- `input.c/h`: Keyboard/mouse input
- `web_encoding.c/h`: Character encoding

**Configuration**:
- `autofill-rules.yml`: Default autofill rules (examples)
- `configure.ac`: Autoconf configuration
- `Makefile.am`: Automake build system

---

## 15. Performance Characteristics

### 15.1 Rendering Performance

**Dirty Rectangle Optimization** (KCM-297):
- Pixel-level dirty rectangle tracking
- Only changed regions transmitted
- Reduces bandwidth by 30-70% on typical use

**Frame Synchronization** (KCM-372):
- Automatic frame pacing (1-60 FPS typical)
- Prevents tearing
- Optimizes for both fast and slow networks

**Compression**:
- WebP (modern, efficient)
- JPEG (compatibility)
- Adaptive based on content

### 15.2 Memory Usage

**Per-Connection Memory**:
- Shared memory: ~50MB+ (display buffer + FIFOs)
- Guacd thread: ~10-20MB
- CEF process: ~150-300MB (browser engine + rendered content)
- Total: ~200-350MB per connection

**Clipboard Limits**:
- Minimum: 256KB
- Default: 50MB
- Configurable per deployment

### 15.3 CPU Usage

**Typical Consumption**:
- Idle connection: <1% CPU
- Active typing: 5-15% CPU
- Video playback: 30-50% CPU
- Complex page: 20-40% CPU

**Parallelization**:
- Multi-threaded (guacd + CEF)
- Efficient on multi-core systems
- Single gua cd process handles 100+ connections

---

## Summary: Key Takeaways

1. **RBI Implementation**: Browser isolation via separate CEF process with multi-layer security
2. **Protocol Integration**: Guacamole protocol handler (plugin architecture)
3. **IPC Mechanism**: Shared memory + FIFOs for guacd ↔ CEF communication
4. **Security**: Process isolation, namespace separation, DBus isolation, URL filtering, autofill security
5. **Rendering**: Dirty rectangle optimization, frame synchronization, multiple compression formats
6. **Sessions**: Persistent profile storage, TOTP autofill, multi-stage login support
7. **Build**: Complex CEF build (30+ mins), libguac-client-http as dynamic plugin
8. **Keeper Integration**: Credential injection from Keeper Vault, compliance recording, session management
9. **Performance**: 200-350MB per connection, 1-50% CPU depending on usage
10. **Extensibility**: Autofill rules, URL patterns, connection parameters, isolated browser profiles
