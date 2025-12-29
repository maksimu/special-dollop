# Remote Browser Isolation (RBI) Implementation

## Overview

RBI provides isolated browser sessions using headless Chrome/Chromium. Browser rendering is captured and streamed as images over WebRTC using the Guacamole protocol.

**Status**: Complete implementation with chromiumoxide integration (v0.7)

## Architecture

### Browser Backend

**Current**: chromiumoxide (headless Chrome)
- Full web compatibility
- Chrome DevTools Protocol (CDP) support
- 200-500MB RAM per instance
- Production-ready

**Future**: Servo (Rust-native browser)
- Experimental (v0.0.3 as of Dec 2025, monthly releases)
- 50-100MB RAM target
- Has embedding API (`libservo` with `WebView`/`WebViewBuilder`)
- Limited web compatibility - not production-ready yet

### Process Pooling

To reduce resource usage, browsers can be pooled:
- 5 browser instances × 10 tabs each = 50 concurrent sessions
- 200MB per browser / 10 tabs = ~20MB per session
- 6-15x resource savings vs dedicated instances

### Popup Handling

**Default**: Block all popups
```rust
PopupHandling::Block  // Most secure
```

**Options**:
- `Block` - Block all popups (default)
- `AllowList(domains)` - Allow specific domains (e.g., OAuth)
- `NavigateMainWindow` - Navigate main window to popup URL

### File Downloads

**Default**: Block all downloads (like KCM)
```rust
DownloadConfig {
    enabled: false,  // Block by default for security
    max_file_size_mb: 10,
    allowed_extensions: vec!["pdf", "txt", "csv"],
    blocked_extensions: vec!["exe", "bat", "sh", "dmg"],
    require_approval: true,
}
```

**Security Controls**:
1. Size limits (10MB default)
2. File type restrictions (block executables)
3. Rate limiting (5 downloads per session)
4. User approval required
5. Virus scanning (future)

**File Transfer via Guacamole Protocol**:
```
file,<stream>,<mimetype>,<filename>;
blob,<stream>,<data>;
end,<stream>;
```

## Implementation

### Key Files

- `crates/guacr-rbi/src/handler.rs` - Main RBI handler
- `crates/guacr-rbi/src/browser_client.rs` - Browser wrapper
- `crates/guacr-rbi/src/chrome_session.rs` - Chrome session management
- `crates/guacr-rbi/src/cdp_client.rs` - CDP protocol client

### Configuration

```rust
pub struct RbiConfig {
    pub backend: RbiBackend,
    pub default_width: u32,
    pub default_height: u32,
    pub chromium_path: String,
    pub popup_handling: PopupHandling,
    pub resource_limits: ResourceLimits,
    pub capture_fps: u32,
    pub download_config: DownloadConfig,
}

pub struct ResourceLimits {
    pub max_memory_mb: usize,      // 500MB default
    pub max_cpu_percent: u32,      // 80% default
    pub timeout_seconds: u64,      // 3600s (1 hour) default
}
```

## Critical: Pixel-Perfect Dimension Alignment

To avoid blurry rendering from browser scaling:

1. Browser sends size request: `width_px`, `height_px`
2. Set browser viewport: `browser.set_window_size(width_px, height_px)`
3. Send `size` instruction: `size,0,width_px,height_px;`
4. Capture screenshot: `screenshot(width_px, height_px)`

**Result**: PNG size = layer size = no scaling = crisp rendering

**NEVER** use mismatched dimensions - causes browser to scale and blur the image.

## chromiumoxide Integration

### Browser Launch
```rust
use chromiumoxide::{Browser, BrowserConfig};

let config = BrowserConfig::builder()
    .with_head()
    .args(vec![
        "--no-first-run",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--disable-popup-blocking",  // Keep popup blocking enabled
    ])
    .build()?;

let (browser, mut handler) = Browser::launch(config).await?;
```

### Screenshot Capture
```rust
let page = browser.new_page(url).await?;
let screenshot = page.screenshot(None).await?;
```

### Input Injection
```rust
// Mouse
page.click((x, y)).await?;

// Keyboard
page.type_str("text").await?;
```

## Performance

### Capture Loop
- 30-60 FPS capture rate (configurable)
- Dirty checking (only send changed screenshots)
- Zero-copy via `bytes::Bytes`

### Resource Monitoring
- Track memory usage per session
- Kill browser if exceeds limits (cgroups on Linux)
- CPU throttling if exceeds threshold

## Security

### Isolation
- Dedicated Chrome instance per session (no sharing)
- Sandbox mode enabled
- Resource limits enforced (cgroups)

### Popup Blocking
- Default: Block all popups
- JavaScript injection to override `window.open()`
- Optional allow-list for OAuth/login

### Download Restrictions
- Default: Block all downloads
- Optional: Controlled downloads with security checks
- File type and size restrictions
- Rate limiting

## Future Enhancements

### Servo Integration
When Servo matures:
- Rust-native embedding
- 50-100MB RAM vs 200-500MB
- Memory safe
- Better security isolation

### Advanced Features
- Process pooling (6-15x resource savings)
- Tab bar support (multiple tabs per session)
- Grid view (multiple tabs simultaneously)
- Virus scanning for downloads
- User approval UI for downloads

## Testing

```bash
cargo test -p guacr-rbi
```

## References

- chromiumoxide: https://github.com/mattsse/chromiumoxide
- Chrome DevTools Protocol: https://chromedevtools.github.io/devtools-protocol/
- Guacamole Protocol: `docs/GUACAMOLE_PROTOCOL_COVERAGE.md`
