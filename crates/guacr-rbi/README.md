# guacr-rbi: Remote Browser Isolation

Remote Browser Isolation (RBI) handler for the Guacamole protocol. Provides isolated browser sessions using Chrome/Chromium via DevTools Protocol (CDP).

## Overview

This crate implements the HTTP protocol handler for `guacr`, enabling web browsing sessions to be rendered and streamed to Guacamole clients.

## Quick Start

```bash
# Build with Chrome/CDP backend
cargo build -p guacr-rbi
```

**Requirements**: Chrome or Chromium must be installed on the system at runtime.

## Features

| Feature | Chrome/CDP |
|---------|------------|
| Screenshot capture | ✅ CDP |
| Keyboard input | ✅ |
| Mouse input | ✅ |
| Touch input | ✅ |
| Clipboard sync | ✅ |
| Cursor tracking | ✅ |
| URL whitelisting | ✅ |
| Popup blocking | ✅ |
| Downloads | ✅ |
| Multi-tab | ✅ |
| Autofill | ✅ |
| File uploads | ✅ |
| JavaScript dialogs | ✅ |
| **Session isolation** | ✅ |
| **Profile locking** | ✅ |

## Architecture

### Chrome/CDP Backend

Uses stock Chrome/Chromium via DevTools Protocol:

```
┌─────────────────────────────────────────────────────────┐
│                    Guacamole Client                     │
│                  (Browser/Web Client)                   │
└─────────────────────────────────────────────────────────┘
                            │
                            │ Guacamole Protocol
                            │ (PNG images, input events)
                            ▼
┌─────────────────────────────────────────────────────────┐
│                      guacr-rbi                          │
│                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐ │
│  │ RbiHandler  │─▶│ ChromeSession│─▶│BrowserClient │ │
│  └──────────────┘  └──────────────┘  └──────────────┘ │
│                          │                      │       │
│                   │  PNG Encode │        │  CDP API  │ │
│                   └─────────────┘        └───────────┘ │
└─────────────────────────────────────────────────────────┘
                            │
                            │ Chrome DevTools Protocol
                            ▼
┌─────────────────────────────────────────────────────────┐
│             Chrome (Chromium via chromiumoxide)         │
│                  Headless Browser Process               │
└─────────────────────────────────────────────────────────┘
```

**Key Components:**

| Component | Description |
|-----------|-------------|
| `handler.rs` | RBI protocol handler entry point |
| `browser_client.rs` | Browser lifecycle and event management |
| `chrome_session.rs` | Chrome browser/handlers integration |
| `input.rs` | Keyboard/mouse input handling |
| `clipboard.rs` | Clipboard synchronization |
| `cursor.rs` | Cursor position tracking |
| `screencast.rs` | Screenshot capture via CDP |
| `profile_isolation.rs` | Session isolation and profile locking |
| `autofill.rs` | Form autofill and password management |
| `file_upload.rs` | File upload handling |
| `js_dialog.rs` | JavaScript alert/confirm/prompt |
| `tabs.rs` | Multi-tab management |

## Security Features

### Session Isolation

Each RBI session gets:
- **Dedicated Chrome instance** - No process sharing between sessions
- **Isolated profile directory** - Separate cookies, cache, localStorage per session
- **Profile locking** - Prevents concurrent access to same profile
- **Automatic cleanup** - Profile deleted when session ends

```rust
// Each session creates isolated profile
let profile_dir = tempfile::tempdir()?;
let chrome = ChromeSession::new(1920, 1080, 30, &chromium_path);
chrome.launch(&url, &profile_dir.path()).await?;
// Profile automatically deleted when session ends
```

### URL Whitelisting

Restrict browsing to approved domains:

```rust
let config = RbiConfig {
    allowed_domains: vec![
        "example.com".to_string(),
        "trusted-site.org".to_string(),
    ],
    ..Default::default()
};
```

### Popup Blocking

Block all popups except from whitelisted domains:

```rust
let config = RbiConfig {
    popup_whitelist: vec!["trusted-site.com".to_string()],
    ..Default::default()
};
```

## Configuration

```rust
use guacr_rbi::{RbiConfig, RbiBackend, ResourceLimits};

let config = RbiConfig {
    // Browser backend
    backend: RbiBackend::Chrome,
    chromium_path: "/usr/bin/chromium".to_string(),
    
    // Display settings
    width: 1920,
    height: 1080,
    fps: 30,
    
    // Security
    allowed_domains: vec!["example.com".to_string()],
    popup_whitelist: vec![],
    
    // Resource limits
    resource_limits: ResourceLimits {
        max_memory_mb: 512,
        max_cpu_percent: 80.0,
    },
    
    // Features
    enable_autofill: true,
    enable_downloads: true,
    enable_file_uploads: true,
};
```

## Usage

### As a Protocol Handler

```rust
use guacr_rbi::RbiHandler;
use guacr_handlers::ProtocolHandler;

let handler = RbiHandler::new(config);
handler.connect(params, to_client, from_client).await?;
```

### Standalone

```rust
use guacr_rbi::ChromeSession;

let mut chrome = ChromeSession::new(1920, 1080, 30, "/usr/bin/chromium");
chrome.launch("https://example.com").await?;

// Capture screenshot
let png_data = chrome.capture_screenshot().await?;

// Inject input
chrome.inject_keyboard(0xFF0D, true).await?; // Enter key
chrome.inject_mouse(100, 200, 1, true).await?; // Left click
```

## Testing

```bash
# Unit tests
cargo test -p guacr-rbi

# Integration tests (requires Chrome)
cargo test -p guacr-rbi --test integration_test -- --include-ignored
```

### Chrome Installation

**Linux:**
```bash
# Debian/Ubuntu
sudo apt install chromium-browser

# Fedora
sudo dnf install chromium
```

**macOS:**
```bash
brew install --cask google-chrome
```

**Docker:**
```dockerfile
FROM zenika/alpine-chrome:with-node
# Chrome pre-installed at /usr/bin/chromium-browser
```

## Performance

| Metric | Chrome/CDP |
|--------|------------|
| Startup time | 2-5 seconds |
| Memory per session | 200-500 MB |
| CPU usage (idle) | 1-5% |
| CPU usage (active) | 10-30% |
| Screenshot latency | 50-200ms |

## Troubleshooting

### Chrome Not Found

```
Error: Chrome/Chromium not found
```

**Solution**: Install Chrome or set `chromium_path` in config.

### Profile Lock Error

```
Error: Profile is locked by another process
```

**Solution**: Ensure no other Chrome instances are using the same profile. guacr-rbi creates unique profiles per session to avoid this.

### High Memory Usage

Chrome can use 200-500MB per session. To limit:

```rust
let config = RbiConfig {
    resource_limits: ResourceLimits {
        max_memory_mb: 256, // Kill session if exceeds 256MB
        ..Default::default()
    },
    ..Default::default()
};
```

## Comparison to KCM

| Aspect | KCM (Patched CEF) | guacr-rbi (Chrome/CDP) |
|--------|-------------------|------------------------|
| Browser | Patched CEF | Stock Chrome |
| Audio | Full streaming | Not supported |
| Build complexity | High (CEF patches) | Low (uses installed Chrome) |
| Runtime size | ~300MB (CEF binaries) | ~200MB (Chrome process) |
| Clipboard | CEF patch | JavaScript polling |
| Setup | Hours (build CEF) | Minutes (install Chrome) |

## License

Licensed under the same terms as the parent `guacr` project.
