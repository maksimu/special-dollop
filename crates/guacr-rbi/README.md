# guacr-rbi: Remote Browser Isolation

Remote Browser Isolation (RBI) handler for the Guacamole protocol. Provides isolated browser sessions with two backends:

- **CEF**: Full Chromium Embedded Framework with audio streaming support
- **Chrome/CDP**: Stock Chrome via DevTools Protocol (experimental audio via Web Audio API)

## Overview

This crate implements the HTTP protocol handler for `guacr`, enabling web browsing sessions to be rendered and streamed to Guacamole clients.

## Backends

### CEF Backend (Full Audio Support)

Uses the Chromium Embedded Framework, the same browser engine used by KCM. Provides:
- ✅ Full audio streaming via `AudioHandler` callbacks
- ✅ Direct pixel access via `RenderHandler`
- ✅ No external browser process needed (embedded)
- ✅ Full KCM feature parity

```bash
cargo build -p guacr-rbi --features cef
```

**Requirements**: CEF binaries must be downloaded and `CEF_PATH` environment variable set.

### Chrome/CDP Backend (Easy Setup)

Uses stock Chrome/Chromium via DevTools Protocol. Useful when you can't use CEF:
- ✅ Works with any installed Chrome/Chromium
- ✅ Easy development/testing
- ⚠️ Experimental audio via Web Audio API hooks (limited)

```bash
cargo build -p guacr-rbi --features chrome
```

**Requirements**: Chrome or Chromium must be installed on the system at runtime.

## Feature Flags

| Feature | Description | Audio Support |
|---------|-------------|---------------|
| `cef` | CEF with binaries | ✅ Full audio streaming |
| `chrome` | Chrome via CDP | ⚠️ Experimental (Web Audio API) |

```bash
# Chrome/CDP backend (easy setup)
cargo build -p guacr-rbi --features chrome

# CEF backend (requires CEF binaries)
CEF_PATH=/path/to/cef cargo build -p guacr-rbi --features cef
```

## Features

| Feature | CEF | Chrome/CDP |
|---------|-----|------------|
| Screenshot capture | ✅ RenderHandler | ✅ CDP |
| Keyboard input | ✅ | ✅ |
| Mouse input | ✅ | ✅ |
| Touch input | ✅ | ✅ |
| Clipboard sync | ✅ | ✅ |
| Cursor tracking | ✅ | ✅ |
| **Audio streaming** | ✅ Full | ⚠️ Experimental |
| URL whitelisting | ✅ | ✅ |
| Popup blocking | ✅ | ✅ |
| Downloads | ✅ | ✅ |
| Multi-tab | ✅ | ✅ |
| Autofill | ✅ | ✅ |
| File uploads | ✅ | ✅ |
| JavaScript dialogs | ✅ | ✅ |
| **Session isolation** | ✅ | ✅ |
| **Profile locking** | ✅ | ✅ |
| **DBus isolation** | ✅ (Linux) | N/A |

## Audio Support

### CEF Backend (Recommended for Audio)

CEF provides full audio streaming just like KCM:

```rust
// CEF's AudioHandler receives raw float samples from Chromium
impl ImplAudioHandler for GuacAudioHandler {
    fn on_audio_stream_packet(&self, browser, data: &[&[f32]], frames, pts) {
        // Convert float to PCM and stream to client
        let pcm = float_to_pcm_interleaved(data, frames, channels, 16);
        // Send via Guacamole audio protocol
    }
}
```

### Chrome/CDP Backend (Experimental Audio)

The Chrome/CDP backend has **experimental** audio support via Web Audio API:

```rust
// Audio capture options for CDP:
// 1. Web Audio API hooks - captures audio from pages using Web Audio
// 2. Chrome tabCapture extension - requires browser extension
// 3. Disabled - many RBI use cases don't need audio

let config = RbiConfig {
    audio_config: AudioConfig {
        enabled: true,  // Enable experimental Web Audio capture
        sample_rate: 44100,
        bits_per_sample: 16,
        channels: 2,
    },
    ..Default::default()
};
```

**Note**: CDP does not expose Chrome's raw audio pipeline. Audio capture is limited to:
- Pages that use Web Audio API (games, media players)
- Chrome extensions with `tabCapture` permission

For full audio support, use the CEF backend.

## Session Isolation

Both backends implement security isolation to prevent data leakage between sessions:

### Automatic Profile Isolation

Each RBI session gets an isolated browser profile:

```rust
// Automatic: Creates unique temp directory per session
let session = ChromeSession::new(1920, 1080, 30, "/usr/bin/chrome");
// Profile: /tmp/guacr-rbi-profile-XXXXXX (auto-deleted on session end)
```

### Persistent Profiles with Locking

For persistent sessions, exclusive file locking prevents concurrent access:

```rust
let config = RbiConfig {
    profile_storage_directory: Some("/var/lib/guacr/profiles/user1".to_string()),
    create_profile_directory: true, // Auto-create if missing
    ..Default::default()
};

// If another session tries to use the same profile:
// Error: "Profile is already in use: /var/lib/guacr/profiles/user1"
```

### DBus Isolation (Linux + CEF)

On Linux with CEF, DBus is isolated using namespaces to prevent cross-session IPC:

- Creates user namespace (for mount permissions)
- Creates mount namespace
- Bind-mounts private DBus socket
- Runs isolated `dbus-daemon` per session

This prevents CEF processes in different sessions from communicating.

## Architecture

### CEF Backend (with Audio)

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Guacamole Client                             │
└───────────────────────────────────────┬─────────────────────────────┘
                                        │ WebSocket
┌───────────────────────────────────────▼─────────────────────────────┐
│                           guacr-rbi                                  │
│  ┌─────────────┐  ┌──────────────────┐  ┌──────────────┐            │
│  │ RbiHandler  │─▶│ CefBrowserClient │─▶│  CefSession  │            │
│  └─────────────┘  └──────────────────┘  └──────┬───────┘            │
│                          │                      │ CEF API            │
│                   ┌──────▼──────┐        ┌──────▼──────┐            │
│                   │  PNG Encode │        │ CEF Handlers│            │
│                   │  (display)  │        │ - Render    │ ◀─ pixels  │
│                   └─────────────┘        │ - Audio     │ ◀─ PCM     │
│                                          │ - LifeSpan  │            │
│                                          └──────┬──────┘            │
└─────────────────────────────────────────────────┼───────────────────┘
                                                  │
┌─────────────────────────────────────────────────▼───────────────────┐
│             CEF (Chromium Embedded Framework) - Bundled              │
└─────────────────────────────────────────────────────────────────────┘
```

### Chrome/CDP Backend

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Guacamole Client                             │
└───────────────────────────────────────┬─────────────────────────────┘
                                        │ WebSocket
┌───────────────────────────────────────▼─────────────────────────────┐
│                           guacr-rbi                                  │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────┐                 │
│  │ RbiHandler  │─▶│ BrowserClient│─▶│ChromeSession│                 │
│  └─────────────┘  └──────────────┘  └──────┬──────┘                 │
│                                            │ CDP                     │
│  ┌─────────────┐  ┌──────────────┐  ┌──────▼──────┐                 │
│  │ Autofill    │  │ RbiClipboard │  │chromiumoxide│                 │
│  │ Manager     │  │              │  │             │                 │
│  └─────────────┘  └──────────────┘  └──────┬──────┘                 │
└───────────────────────────────────────────┼─────────────────────────┘
                                            │
┌───────────────────────────────────────────▼─────────────────────────┐
│                    Headless Chrome/Chromium (External)               │
└─────────────────────────────────────────────────────────────────────┘
```

## Modules

| Module | Description |
|--------|-------------|
| `handler.rs` | Main RBI protocol handler and configuration |
| `cef_browser_client.rs` | CEF browser session management (with audio) |
| `cef_session.rs` | CEF browser/handlers integration |
| `browser_client.rs` | Chrome/CDP session management |
| `chrome_session.rs` | Chrome CDP integration via chromiumoxide |
| `profile_isolation.rs` | Profile locking and DBus isolation |
| `input.rs` | Keyboard/mouse/touch handling (1000+ key mappings) |
| `clipboard.rs` | Clipboard sync with buffer management |
| `clipboard_polling.rs` | JavaScript-based clipboard monitoring |
| `cursor.rs` | Cursor style tracking and sync |
| `tabs.rs` | Multi-tab management (up to 10 tabs) |
| `audio.rs` | Audio streaming infrastructure |
| `autofill.rs` | Credential autofill with TOTP support |
| `file_upload.rs` | File upload handling with restrictions |
| `js_dialog.rs` | JavaScript alert/confirm/prompt handling |
| `events.rs` | Display and input event types |

## Downloads & Uploads

Both downloads and uploads are **disabled by default** for security. Enable them in config:

```rust
use guacr_rbi::{RbiConfig, DownloadConfig, UploadConfig};

let config = RbiConfig {
    // Downloads: browser → client
    download_config: DownloadConfig {
        enabled: true,
        max_file_size_mb: 50,
        allowed_extensions: vec!["pdf".into(), "txt".into(), "csv".into()],
        blocked_extensions: vec!["exe".into(), "bat".into(), "sh".into()],
        max_downloads_per_session: 10,
        ..Default::default()
    },
    
    // Uploads: client → browser
    upload_config: UploadConfig {
        enabled: true,
        max_size: 100 * 1024 * 1024, // 100MB
        allowed_extensions: vec!["pdf".into(), "png".into(), "jpg".into()],
        blocked_extensions: vec!["exe".into(), "bat".into(), "js".into()],
        max_concurrent: 3,
    },
    
    ..Default::default()
};
```

### Download Flow
1. Browser triggers download (user clicks link, JS initiates)
2. CDP intercepts download event  
3. `chrome_session.handle_download()` validates size/type
4. File streamed to client via Guacamole `file` + `blob` instructions

### Upload Flow
1. Browser shows file input (user clicks `<input type="file">`)
2. CDP detects file chooser via `poll_file_chooser()`
3. Client receives `upload-request` pipe instruction
4. Client sends `file`, `upload-blob`, `upload-end` instructions
5. `upload_engine` validates and buffers data
6. `chrome_session.submit_upload_files()` injects files into browser

## Usage

```rust
use guacr_rbi::{RbiHandler, RbiConfig, PopupHandling, AutofillRule};

let config = RbiConfig {
    default_width: 1920,
    default_height: 1080,
    chromium_path: "/usr/bin/chromium".to_string(),
    popup_handling: PopupHandling::Block,
    
    // URL restrictions
    allowed_url_patterns: vec!["*.company.com".to_string()],
    
    // Clipboard security
    disable_paste: true, // Block paste to browser
    clipboard_buffer_size: 1024 * 1024, // 1MB
    
    // Localization
    timezone: Some("America/New_York".to_string()),
    accept_language: Some("en-US".to_string()),
    
    // Autofill
    autofill_rules: vec![
        AutofillRule {
            page_pattern: Some("https://myapp.com/login".to_string()),
            username_field: Some("#email".to_string()),
            password_field: Some("#password".to_string()),
            submit: Some("button[type=submit]".to_string()),
            ..Default::default()
        }
    ],
    
    // Profile persistence (with exclusive locking)
    profile_storage_directory: Some("/var/lib/guacr/profiles/user1".to_string()),
    create_profile_directory: true,
    
    ..Default::default()
};

let handler = RbiHandler::new(config);
```

## Autofill Configuration

Autofill supports CSS and XPath selectors:

```rust
use guacr_rbi::{AutofillRule, AutofillCredentials, TotpConfig};

// Define rules
let rules = vec![AutofillRule {
    page_pattern: Some(r"https://.*\.example\.com/login".to_string()),
    username_field: Some("#email".to_string()),
    password_field: Some("//input[@type='password']".to_string()), // XPath
    totp_field: Some("#otp".to_string()),
    submit: Some("form".to_string()),
    cannot_submit: Some(vec!["#captcha".to_string()]), // Don't auto-submit if captcha
}];

// Set credentials
let credentials = AutofillCredentials {
    username: Some("user@example.com".to_string()),
    password: Some("secret".to_string()),
    totp_config: Some(TotpConfig {
        secret: "JBSWY3DPEHPK3PXP".to_string(),
        period: 30,
        digits: 6,
        ..Default::default()
    }),
};
```

## CEF Setup (for Full Audio Support)

The CEF backend provides full audio streaming like KCM. To use it:

### Step 1: Download CEF Binaries

```bash
# Download from https://cef-builds.spotifycdn.com/index.html
# Choose "Standard Distribution" for your platform
# Extract to a known location (e.g., /opt/cef or ~/cef)
```

### Step 2: Set Environment Variable

```bash
# Point to CEF distribution
export CEF_PATH=/path/to/cef_binary_XXX

# Verify structure (should contain Release/, Resources/, include/)
ls $CEF_PATH
```

### Step 3: Build with CEF

```bash
cargo build -p guacr-rbi --features cef
```

### CEF in Docker

```dockerfile
FROM rust:1.75-slim AS builder

# Install CEF dependencies
RUN apt-get update && apt-get install -y \
    wget xz-utils \
    libx11-dev libxrandr-dev libxinerama-dev libxcursor-dev \
    libgl1-mesa-dev libnss3-dev libatk1.0-dev libatk-bridge2.0-dev \
    libcups2-dev libdrm-dev libxcomposite-dev libxdamage-dev \
    libxfixes-dev libgbm-dev libasound2-dev libpangocairo-1.0-0

# Download CEF (update version as needed)
RUN wget -q "https://cef-builds.spotifycdn.com/cef_binary_130.1.16%2Bg5765e4f%2Bchromium-130.0.6723.119_linux64.tar.bz2" \
    && tar -xjf cef_binary_*.tar.bz2 \
    && mv cef_binary_* /opt/cef

ENV CEF_PATH=/opt/cef

WORKDIR /app
COPY . .
RUN cargo build --release -p your-app --features cef

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    libx11-6 libnss3 libatk1.0-0 libatk-bridge2.0-0 \
    libcups2 libdrm2 libxcomposite1 libxdamage1 \
    libxfixes3 libgbm1 libasound2 libpangocairo-1.0-0

COPY --from=builder /opt/cef/Release /opt/cef/Release
COPY --from=builder /opt/cef/Resources /opt/cef/Resources
COPY --from=builder /app/target/release/your-app /usr/local/bin/

ENV LD_LIBRARY_PATH=/opt/cef/Release
CMD ["your-app"]
```

## CEF vs Chrome Comparison

| Aspect | CEF (`--features cef`) | Chrome (`--features chrome`) |
|--------|------------------------|------------------------------|
| Audio streaming | ✅ Full support | ⚠️ Experimental (Web Audio) |
| Build complexity | High (CEF binaries ~300MB) | Low (uses installed Chrome) |
| Binary size | ~200MB added | ~0 (Chrome external) |
| Development speed | Slower build | Faster build |
| Runtime dependency | None (embedded) | Chrome must be installed |
| Session isolation | ✅ Full + DBus | ✅ Profile isolation |
| KCM feature parity | ✅ Full | ⚠️ Limited audio |

**Recommendation**: 
- Use `--features chrome` for development and when audio is not required
- Use `--features cef` for production when full audio streaming is needed

## Testing

```bash
# Run all tests
cargo test -p guacr-rbi

# Run with Chrome feature
cargo test -p guacr-rbi --features chrome

# Run with CEF feature
cargo test -p guacr-rbi --features cef
```

## Security Features

- **Session Isolation**: Unique temp profile per session, auto-cleaned
- **Profile Locking**: Exclusive `flock()` on persistent profiles
- **DBus Isolation**: Linux namespace isolation for CEF
- **URL Whitelisting**: Regex patterns for allowed URLs
- **Popup Blocking**: Block all or use allowlist
- **Clipboard Restrictions**: Disable copy and/or paste
- **Download Control**: Extension/size limits, rate limiting
- **Resource Limits**: Memory cap, session timeout
- **Read-only Mode**: Drop all user input

## Chrome/Chromium Deployment Options

RBI with Chrome backend requires Chrome/Chromium to be installed.

### Option 1: Require Chrome Installed (Development)

```rust
let config = RbiConfig {
    chromium_path: "/usr/bin/chromium".to_string(),
    ..Default::default()
};
```

### Option 2: Bundle Chromium (Production Docker)

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    chromium fonts-liberation libnss3 libxss1 libasound2 \
    --no-install-recommends && rm -rf /var/lib/apt/lists/*

ENV CHROMIUM_PATH=/usr/bin/chromium
```

### Option 3: Alpine Chromium Image (Smallest)

```dockerfile
FROM zenika/alpine-chrome:with-node
ENV CHROMIUM_PATH=/usr/bin/chromium-browser
```

Size: ~150MB compressed

## Dependencies

- `chromiumoxide` (chrome feature) - Chrome DevTools Protocol
- `cef` (cef feature) - Chromium Embedded Framework bindings
- `guacr-handlers` - Protocol handler traits
- `guacr-terminal` - Shared clipboard/input resources
- `guacr-protocol` - Guacamole protocol encoding
- `tempfile` - Secure temporary directories for session isolation
- `libc` (unix) - System calls for profile locking

## Comparison with KCM

| Aspect | KCM (C) | guacr-rbi (Rust) |
|--------|---------|------------------|
| Browser | Patched CEF | Stock Chrome or CEF |
| Patches | 8 required | 0 required |
| Build time | Hours (CEF) | Minutes (Chrome) or Hours (CEF) |
| Memory | ~200MB | ~150MB |
| Tests | Manual | 116 automated |
| Async | Fork/shared mem | Tokio async |
| Session isolation | ✅ | ✅ (identical approach) |

## Future Enhancements

- [ ] Chrome extension for tab audio capture
- [ ] WebRTC-based screen streaming
- [ ] Browser dev tools forwarding
