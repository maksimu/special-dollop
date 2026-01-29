# RBI Implementation - File and Code Reference

## Complete File Inventory

### Main RBI Package
```
/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/
├── kcm-libguac-client-http.spec (420 lines) - RPM build spec
├── EXCLUDED_ARCHITECTURES - Build exclusions
├── EXCLUDED_BUILDS - Build exclusions
├── KCM_BUILD_REQUIRES - Additional dependencies
└── extra/libguac-client-http/
    ├── Makefile.am - Build configuration
    ├── configure.ac - Autoconf script
    ├── NOTICE - License
    ├── autofill-rules.yml - Example autofill rules
    ├── m4/ - Autoconf macros
    ├── util/generate-test-runner.pl - Test runner generator
    └── src/
        ├── common/ (8 files)
        │   ├── clipboard.c/h - Clipboard management
        │   ├── json.c/h - JSON parsing
        │   ├── string.c/h - String utilities
        │   ├── list.c/h - Linked list
        │   ├── io.c/h - I/O operations
        │   ├── iconv.c/h - Character encoding
        │   └── common/
        │       ├── clipboard.h - Header
        │       ├── defaults.h - Default values
        │       ├── iconv.h - Encoding header
        │       ├── io.h - I/O header
        │       ├── json.h - JSON header
        │       ├── list.h - List header
        │       └── string.h - String header
        │
        ├── shared_ipc/ (14 files) - IPC Infrastructure
        │   ├── shared_data.c/h - Shared memory structure (50+ fields)
        │   ├── display_fifo.c/h - Display event queue
        │   ├── input_fifo.c/h - Input event queue
        │   ├── audio_fifo.c/h - Audio packet queue
        │   ├── log_fifo.c/h - Log message queue
        │   ├── shared_limits.h - Memory limits (MAX_CLIPBOARD_LENGTH, etc)
        │   ├── shared_utils.c/h - Utility functions
        │   ├── move_fd.c/h - File descriptor passing
        │   ├── transfer_socket.c/h - Socket transfer
        │   └── tests/
        │       ├── test_shared_utils.c - Unit tests
        │       └── Makefile.am - Test build config
        │
        ├── http/ (12 files) - HTTP/RBI Protocol Handler
        │   ├── http.c (910 lines) - Main handler (guac_http_client_thread)
        │   ├── http.h - HTTP client structure definition
        │   ├── client.c/h - Client initialization & cleanup
        │   ├── settings.c/h - Connection settings parsing
        │   ├── input.c/h - Input event processing
        │   ├── argv.c/h - Argument/parameter parsing
        │   ├── clipboard.c/h - Clipboard synchronization
        │   ├── size.c/h - Display size/zoom handling
        │   ├── user.c/h - User management
        │   ├── pipe.c/h - Navigation history pipe
        │   ├── dbus.c/h - DBus isolation (KCM-436)
        │   └── Makefile.am - Build configuration
        │
        └── cef/ (38 files) - CEF/Chromium Integration
            ├── cef_main.c (252 lines) - CEF process entry point
            ├── cef_app.c/h - CEF application initialization
            ├── cef_base.c/h - Base CEF functionality
            ├── cef_client.c/h - CEF client wrapper
            ├── cef_browser_process_handler.c/h - Browser lifecycle
            ├── cef_render_handler.c/h (critical) - Display rendering
            ├── cef_load_handler.c/h - Page loading
            ├── cef_request_handler.c/h - Request interception
            ├── cef_resource_handler.c/h - Resource loading
            ├── cef_resource_request_handler.c/h - Request/response
            ├── cef_display_handler.c/h - Display updates
            ├── cef_dialog_handler.c/h - Dialog handling (blocks file browser)
            ├── cef_download_handler.c/h - Download blocking (KCM-336)
            ├── cef_life_span_handler.c/h - Window lifecycle
            ├── cef_audio_handler.c/h - Audio processing
            ├── cef_devtools_message_observer.c/h - DevTools protocol (KCM-378)
            ├── cef_shutdown_task.c/h - Orderly shutdown (KCM-343)
            ├── autofill.c/h (critical) - Credential injection (KCM-319, KCM-350)
            ├── autofill.js - JavaScript autofill helper
            ├── autofill_js.h - JS code embedding
            ├── input.c/h - Keyboard/mouse input handling
            ├── web_encoding.c/h - Character encoding
            ├── browser_templates.h - CEF templates
            ├── include/
            │   └── internal/kcm_clipboard.h - Clipboard interface
            └── tests/
                ├── test_autofill.c - Autofill unit tests
                ├── test_web_encoding.c - Encoding tests
                └── Makefile.am - Test build config
```

### CEF (Chromium Embedded Framework) Package
```
/Users/mroberts/Documents/kcm/core/packages/kcm-libcef/
├── kcm-libcef.spec - RPM build spec for CEF
├── EXCLUDED_BUILDS - Build exclusions
├── DO_NOT_PARALLELIZE - Memory constraint marker
└── extra/
    ├── gn-defines.gn - Custom Chromium GN defines
    ├── include/
    │   └── internal/kcm_clipboard.h - Clipboard interface
    ├── libcef/
    │   └── common/kcm_clipboard.cc - Clipboard implementation
    ├── cef-patches/ - CEF-specific patches
    │   └── Add-Clipboard-to-CEF-Build.patch
    └── chromium-patches/ - Chromium core patches
        ├── Disable-usage-of-BlueZ-related-code-when-use_dbus-tr.patch
        └── Fix-Chromium-Build-issues-on-linux.patch
```

### Configuration Files
```
/Users/mroberts/Documents/kcm/core/packages/kcm-libguac-client-http/extra/libguac-client-http/
├── autofill-rules.yml - Example autofill configuration
│   - Page pattern matching examples
│   - CSS/XPath selector examples
│   - Multi-stage login examples
│   - Social media login examples
```

### Related Patches

**RBI-specific patches**:
```
/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-client/patches/
└── 0013-KCM-425-Fix-RBI-handling-of-touch-events-on-iOS-devi.patch
    - Maps iOS randomized touch IDs to sequential integers
    - Implements Guacamole.IntegerPool
    - 127 lines, modifies Touch.js module
```

**Server patches affecting RBI**:
```
/Users/mroberts/Documents/kcm/core/packages/kcm-guacamole-server/patches/
├── 0001-KCM-418-Add-AES-GCM-to-fix-FIPS-SSH-connection.patch
├── 0002-KCM-405-Add-ability-to-configure-clipboard-limits.patch
├── 0003-KCM-405-Improve-warnings-about-a-wrong-clipboard-buf.patch
├── 0004-KCM-439-Correct-IPv4-IPv6-processing-of-addresses-fo.patch
├── 0005-KCM-446-Backport-fixes-necessary-for-unit-tests-to-p.patch
├── 0006-KCM-455-Support-clicking-within-scrollbar-track-in-t.patch
├── 0007-KCM-461-Improvements-to-terminal-selection-behavior.patch
├── 0008-KCM-459-Improve-warning-when-WoL-is-enabled-but-no-M.patch
├── 0009-KCM-472-Backport-generalization-of-recording-typescr.patch
└── 5000-KCM-320-Add-ZMQ-guac_socket-with-libzmq-and-libczmq.patch
    - ZeroMQ socket support for distributed guacd
```

### Documentation
```
/Users/mroberts/Documents/kcm/
├── README.md - Main KCM documentation
├── GUACD_ARCHITECTURE_DEEP_DIVE.md - guacd architecture (15K)
├── GUACD_KEY_FILES_REFERENCE.md - guacd key files (5K)
├── DATABASE_PLUGINS_ANALYSIS.md - Database plugin analysis
├── DATABASE_PLUGINS_QUICK_REFERENCE.md - Database quick ref
├── RBI_IMPLEMENTATION_DEEP_DIVE.md - This comprehensive guide (35K)
└── RBI_QUICK_REFERENCE.md - Quick reference guide
```

---

## Key File Sizes and Line Counts

| File | Location | Lines | Purpose |
|------|----------|-------|---------|
| http.c | src/http/ | 910 | Main RBI handler |
| cef_main.c | src/cef/ | 252 | CEF process entry |
| shared_data.h | src/shared_ipc/ | 600+ | IPC structure |
| cef_render_handler.c | src/cef/ | Major | Display rendering |
| autofill.c | src/cef/ | Major | Autofill logic |
| dbus.c | src/http/ | Major | DBus isolation |
| kcm-libguac-client-http.spec | . | 420 | RPM build spec |
| kcm-libcef.spec | ../kcm-libcef/ | Major | CEF build spec |

---

## Code Structure Overview

### Main Protocol Handler Entry Point
**File**: `src/http/http.c`
```c
// Thread function that starts input/display threads
// Creates shared memory with CEF process
// Manages connection lifecycle
void* guac_http_client_thread(void* data)
```

### HTTP Client Structure
**File**: `src/http/http.h`
```c
typedef struct guac_http_client {
    guac_http_settings* settings;        // Connection settings
    pthread_t client_thread;             // Handler thread
    shm_memory* shm;                     // Shared memory ptr
    guac_display* display;               // Display surface
    guac_display_layer* popup_layer;     // Popup rendering
    guac_audio_stream* audio;            // Audio output
    guac_common_clipboard* clipboard;    // Clipboard
    int frames_received;                 // Frame counter
    guac_recording* recording;           // Session recording
} guac_http_client;
```

### Shared Memory Structure
**File**: `src/shared_ipc/shared_data.h`
```c
typedef struct {
    guac_flag browser_state;             // Browser state flags
    display_event_fifo display_fifo;     // Display updates queue
    input_event_fifo input_fifo;         // Input events queue
    log_message_fifo log_fifo;           // Log messages queue
    int disable_audio;                   // Audio disable flag
    audio_packet_fifo audio_fifo;        // Audio packets queue
    audio_output_format audio_format;    // PCM configuration
    int disable_copy;                    // Copy disable flag
    char reported_clipboard[MAX_CLIPBOARD_LENGTH];
    size_t reported_clipboard_length;
    char requested_clipboard[MAX_CLIPBOARD_LENGTH];
    size_t requested_clipboard_length;
    char url[MAX_URL_LENGTH + 1];
    int ignore_initial_ssl_cert;
    char profile_storage_directory[MAX_PROFILE_STORAGE_DIRECTORY_LENGTH + 1];
    int can_go_back;
    // ... 50+ more fields
    pcre* allowed_url_patterns[MAX_ALLOWED_URL_PATTERNS];
    pcre* allowed_resource_url_patterns[MAX_ALLOWED_URL_PATTERNS];
} shm_memory;
```

### Settings Structure
**File**: `src/http/settings.h`
```c
typedef struct guac_http_settings {
    char* url;                          // Initial URL
    bool ignore_initial_ssl_cert;       // Cert validation option
    bool allow_url_manipulation;        // User can change URL
    char* allowed_url_patterns[MAX_ALLOWED_URL_PATTERNS + 1];
    char* allowed_resource_url_patterns[MAX_ALLOWED_URL_PATTERNS + 1];
    char* username;                     // From KSM
    char* password;                     // From KSM
    char* autofill_rules;               // YAML autofill config
    bool disable_audio;                 // Disable audio output
    bool disable_copy;                  // Disable clipboard copy
    bool disable_input;                 // Disable input
    char* profile_storage_directory;    // Session storage path
    int enable_printing;                // Printing option
    // ... more fields
} guac_http_settings;
```

### CEF Application Structure
**File**: `src/cef/cef_app.h`
```c
typedef struct _guac_http_cef_app_t {
    cef_app_t base;                      // Base CEF app
    shm_memory* shm;                     // Shared memory ref
    guac_http_cef_client_t* client;      // Browser client
    guac_http_cef_browser_process_handler_t* browser_process_handler;
} guac_http_cef_app_t;
```

### Autofill Rules Example
**File**: `autofill-rules.yml`
```yaml
# Simple login form
- page: 'https://*.okta.com/login'
  username-field: '#okta-signin-username'
  password-field: '#okta-signin-password'
  submit: '#okta-signin-submit'

# Multi-stage login
- page: 'https://*.okta.com/'
  username-field: '#idp-discovery-username'
  submit: '#idp-discovery-submit'
- page: 'https://*.okta.com/login'
  password-field: '#okta-signin-password'
  submit: '#okta-signin-submit'
```

### DBus Isolation
**File**: `src/http/dbus.h`
```c
// Sets up isolated DBus environment using mount namespaces
int guac_http_setup_isolated_dbus(guac_client* client, shm_memory* shm);

// Cleans up isolated DBus environment
int guac_http_cleanup_isolated_dbus(guac_client* client, shm_memory* shm);
```

---

## Build Configuration Files

### RPM Spec File
**File**: `kcm-libguac-client-http.spec`

**Key Sections**:
- Name: `kcm-libguac-client-http`
- Version: `1.6.0`
- Release: `6` (with dist suffix)
- BuildRequires: 50+ dependencies (CEF, graphics libs, etc)
- %build: Runs autoreconf, configure, make
- %check: Runs `make check` (unit tests)
- %install: Installs binaries, config, creates DBus dir
- %files: Lists installed files and permissions

**Key Output**:
- `/opt/keeper/lib/libguac-client-http.so` - Main library
- `/opt/keeper/libexec/cef_process` - CEF wrapper
- `/etc/guacamole/autofill-rules.yml` - Default rules
- `/var/run/dbus/` - DBus mount point

### Autoconf Configuration
**File**: `configure.ac`
```bash
AC_INIT([libguac-client-http], [1.6.0])
AC_CONFIG_SUBDIRS([src/common src/shared_ipc src/http src/cef])
# Defines check for libraries (CEF, YAML, etc)
# Tests for required headers and functions
```

### Automake Configuration
**File**: `Makefile.am`
```makefile
ACLOCAL_AMFLAGS = -I m4
DIST_SUBDIRS = src/common src/shared_ipc src/http src/cef
SUBDIRS = src/common src/shared_ipc src/http src/cef
EXTRA_DIST = util/generate-test-runner.pl
```

---

## IPC FIFO Structures

### Display FIFO (CEF → guacd)
```c
// Display update events
typedef struct {
    int x, y;           // Position
    int width, height;  // Size (dirty rectangle)
    uint8_t* pixels;    // RGBA pixel data
} display_event;
```

### Input FIFO (guacd → CEF)
```c
// Input event types
#define INPUT_EVENT_KEYBOARD    1
#define INPUT_EVENT_MOUSE       2
#define INPUT_EVENT_CLIPBOARD   3
#define INPUT_EVENT_NAVIGATION  4

typedef struct {
    int type;
    union {
        struct { int keysym; int mods; } keyboard;
        struct { int x, y, buttons; } mouse;
        struct { char* text; } clipboard;
        struct { int direction; } navigation;
    } data;
} input_event;
```

### Audio FIFO (CEF → guacd)
```c
// PCM audio packets
typedef struct {
    int16_t* samples;   // PCM samples
    int sample_count;   // Number of samples
    int channels;       // 1=mono, 2=stereo
    int sample_rate;    // Hz
} audio_packet;
```

### Log FIFO (CEF → guacd)
```c
// Log messages from CEF
typedef struct {
    int level;          // Info, warning, error, etc
    char message[256];  // Log message text
} log_message;
```

---

## Configuration Integration Points

### Connection Parameters (in Keeper Vault)
```
protocol: http
url: https://app.example.com
username: <vault-field>
password: <vault-field>
allowed-url-patterns: https://app.example.com*|https://api.*.com*
allowed-resource-url-patterns: https://*.example.com/*
allow-url-manipulation: false
disable-audio: false
disable-copy: false
disable-input: false
ignore-initial-ssl-cert: false
profile-storage-directory: /var/lib/guacamole/profiles/myapp/
enable-printing: true
```

### Global Autofill Rules
**Path**: `/etc/guacamole/autofill-rules.yml`
- Contains default autofill rules for common services
- Can be customized per deployment
- Connection-specific rules override global rules

### Environment Variables (Docker)
```bash
AUTOFILL_RULES=/etc/guacamole/autofill-rules.yml
LOG_LEVEL=info
ENABLE_RBI=true
```

---

## Testing Infrastructure

### Unit Tests
**Location**: `src/shared_ipc/tests/`
```
test_shared_utils.c - Shared utility function tests
Makefile.am - Test build configuration
```

**Run with**:
```bash
make check
```

### Test Coverage
- Shared memory operations
- FIFO queue operations
- Input event processing
- String/encoding functions
- JSON parsing

---

## Compilation Process

### Step 1: Generate Configure Script
```bash
autoreconf -fi
```

### Step 2: Configure
```bash
./configure --disable-static \
    --with-cef="/usr/share/cef" \
    LDFLAGS="-L/opt/keeper/lib" \
    CPPFLAGS="-I/opt/keeper/include"
```

### Step 3: Build
```bash
make V=1 -j$(nproc)
```

### Step 4: Test
```bash
make check
```

### Step 5: Install
```bash
make install DESTDIR=$RPM_BUILD_ROOT
```

---

## Performance-Critical Code Sections

### Dirty Rectangle Optimization (KCM-297)
**File**: `src/cef/cef_render_handler.c`
- Tracks pixel-level dirty rectangles
- Only transmits changed regions
- Reduces bandwidth by 30-70%

### Display Surface
**Uses**: `guac_display` (from libguac-display)
- High-performance rendering surface
- Layer composition
- Automatic dirty tracking

### Frame Synchronization (KCM-372)
**File**: `src/cef/cef_render_handler.c`
- Automatic frame deferring
- Frame pacing (1-60 FPS)
- Cursor position correction

### Autofill Matching (KCM-431)
**File**: `src/cef/autofill.c`
- CSS/XPath selector matching
- Manual interaction detection
- Prevents accidental triggers

---

## Security-Critical Code Sections

### DBus Isolation (KCM-436)
**File**: `src/http/dbus.c`
- User namespace setup
- Mount namespace isolation
- Eliminated CAP_SYS_ADMIN requirement

### URL Pattern Enforcement
**File**: `src/http/settings.c`
- Regex-based pattern matching (PCRE)
- Whitelist enforcement
- Resource pattern filtering

### Autofill Protection
**File**: `src/cef/autofill.c`
- Secure field matching
- No secret exposure to UI
- Timing-based manual detection

### Download Blocking (KCM-336)
**File**: `src/cef/cef_download_handler.c`
- Intercepts download requests
- Returns error to browser

---

