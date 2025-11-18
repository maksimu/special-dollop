# RBI Browser Engine Options Analysis

## The Problem

Remote Browser Isolation (RBI) needs:
1. Render web pages in isolated environment
2. Capture screen at 30-60 FPS
3. Handle multiple tabs and popups
4. Inject mouse/keyboard input
5. Be lightweight (not consume excessive resources)

**Current concern:** Headless Chrome is resource-heavy

## Browser Engine Options

### Option 1: Headless Chrome/Chromium (Current Plan)

**Pros:**
- Most complete web compatibility
- Chrome DevTools Protocol (CDP) well documented
- Mature headless mode
- Screenshot API built-in
- Input injection supported
- Used by original guacd RBI

**Cons:**
- Resource heavy: 200-500MB RAM per instance
- CPU intensive
- Large binary (100MB+)
- Not embeddable

**Rust Integration:**
- `headless_chrome` crate
- `chromiumoxide` crate (async)
- `fantoccini` (WebDriver)

### Option 2: Firefox Headless

**Pros:**
- Good web compatibility
- Lighter than Chrome (~150-300MB)
- Firefox DevTools Protocol
- Mature headless mode

**Cons:**
- Still resource heavy
- Less documentation than Chrome
- Slower updates

**Rust Integration:**
- `fantoccini` (WebDriver)
- Limited CDP-like protocol support

### Option 3: Servo (Rust-Native Browser Engine)

**Pros:**
- Written in Rust (perfect fit!)
- Embeddable engine
- Modular components
- Memory safe
- Multicore by design
- Potentially lightweight

**Cons:**
- NOT PRODUCTION READY (v0.0.1, experimental)
- Many websites broken
- Missing features
- Performance not optimized yet
- No stable API

**Status:** Too experimental for production (2024)

### Option 4: WebKit (via webkitgtk-rs)

**Pros:**
- Lighter than Chrome (~100-200MB)
- Good compatibility (Safari engine)
- Embeddable
- Mature and stable

**Cons:**
- Linux/GTK dependency
- Harder to run headless
- Less automation API than Chrome
- Cross-platform challenges

**Rust Integration:**
- `webkit2gtk-rs` bindings
- Requires GTK stack

### Option 5: playwright + Browser Process Pool

**Architecture:**
```
RBI Manager
├── Browser Process Pool (3-5 Chrome instances)
├── Tab allocation (round-robin)
└── Resource limits per browser
```

**Pros:**
- Reuses browser instances
- Shares memory across tabs
- Better resource utilization
- Production-proven approach

**Cons:**
- Still Chrome underneath
- Complex tab management
- Isolation between users sharing instances

### Option 6: Lightweight Rendering (No Full Browser)

**Use case-specific rendering:**

```
If URL is:
  - Image (PNG/JPEG/GIF) → Direct image display (no browser)
  - PDF → PDF renderer (not browser)
  - Simple HTML → Simple renderer (no JS)
  - Complex webapp → Full browser (Chrome)
```

**Pros:**
- Minimal resources for simple cases
- Progressive enhancement
- Only use Chrome when needed

**Cons:**
- Complex routing logic
- User expectation mismatch

## Recommendation: Hybrid Approach

### Architecture

```rust
pub enum BrowserEngine {
    Chrome,          // Full Chrome - for complex sites
    ChromePooled,    // Shared Chrome instance - for multiple tabs
    SimpleRenderer,  // For static content
}

pub struct RbiHandler {
    engine_selector: EngineSelector,
    chrome_pool: ChromePool,
}

impl RbiHandler {
    async fn select_engine(&self, url: &str) -> BrowserEngine {
        if self.is_static_content(url) {
            BrowserEngine::SimpleRenderer
        } else if self.config.allow_tab_sharing {
            BrowserEngine::ChromePooled
        } else {
            BrowserEngine::Chrome
        }
    }
}
```

### Chrome Process Pool

**Manage multiple tabs in fewer processes:**

```rust
pub struct ChromePool {
    browsers: Vec<Arc<Browser>>,
    tab_allocator: TabAllocator,
    max_tabs_per_browser: usize,
}

impl ChromePool {
    pub async fn new(pool_size: usize) -> Result<Self> {
        let mut browsers = Vec::new();

        for _ in 0..pool_size {
            let browser = Browser::new(LaunchOptions {
                headless: true,
                sandbox: true,
                // Limit resources per browser
                args: vec![
                    "--disable-gpu",
                    "--disable-dev-shm-usage",
                    "--disable-software-rasterizer",
                    "--memory-pressure-off",
                ],
                ..Default::default()
            })?;

            browsers.push(Arc::new(browser));
        }

        Ok(Self {
            browsers,
            tab_allocator: TabAllocator::new(),
            max_tabs_per_browser: 10,
        })
    }

    pub async fn allocate_tab(&self, url: &str) -> Result<IsolatedTab> {
        // Find browser with available capacity
        for browser in &self.browsers {
            if self.tab_allocator.get_tab_count(browser) < self.max_tabs_per_browser {
                let tab = browser.new_tab()?;
                tab.navigate_to(url)?;

                return Ok(IsolatedTab {
                    tab,
                    browser: Arc::clone(browser),
                });
            }
        }

        Err(RbiError::PoolExhausted)
    }
}
```

**Resource savings:**
- 5 browsers × 200MB = 1GB (vs 50 tabs × 200MB = 10GB)
- Shared renderer processes
- Shared GPU context

### Handling Popups and Multiple Tabs

**Strategy 1: Block Popups (Simplest)**

```rust
// Chrome launch args
args: vec![
    "--disable-popup-blocking=false",  // Actually enable popup blocking
    "--block-new-web-contents",
    // Intercept window.open() at JS level
],
```

**Strategy 2: Capture and Merge (Current guacd RBI Approach)**

```rust
impl RbiHandler {
    async fn handle_popup(&mut self, popup_url: &str) -> Result<()> {
        // Option A: Open in same tab (navigate)
        self.main_tab.navigate_to(popup_url)?;

        // Option B: Track as secondary tab, merge into single view
        let popup_tab = self.browser.new_tab()?;
        popup_tab.navigate_to(popup_url)?;

        // Capture both tabs, composite into single framebuffer
        self.composite_tabs(vec![&self.main_tab, &popup_tab])?;
    }

    fn composite_tabs(&mut self, tabs: Vec<&Tab>) -> Result<()> {
        // Layout tabs in grid or stacked windows
        // User sees all tabs in single view
        // Click coordinates routed to correct tab
    }
}
```

**Strategy 3: Expose Tab Bar (Best UX)**

```rust
// Send custom Guacamole instruction for tab creation
guac_tx.send(Instruction {
    opcode: "tab-create".to_string(),
    args: vec![
        tab_id.to_string(),
        popup_url.to_string(),
    ],
}).await?;

// Browser client shows tab bar
// User can switch tabs
// Only active tab is captured/sent
```

### Popup Detection

```rust
// Listen for CDP Target.created events
tab.get_browser().await?.on_target_created(|target| {
    if target.get_type() == TargetType::Page {
        // New tab/popup created
        handle_new_target(target);
    }
});

// Or inject JavaScript to intercept
tab.evaluate(r#"
    window.open = function(url, name, specs) {
        // Send to backend instead
        sendToBackend('popup', {url, name, specs});
        return null;  // Block actual popup
    };
"#, false)?;
```

## Alternative: Tauri + WebView2 (Hybrid)

**For Windows RBI:**

```rust
use tauri::webview::WebViewBuilder;

// Use system WebView2 (Edge-based on Windows)
// Much lighter than full Chrome
let webview = WebViewBuilder::new(window)
    .with_url(url)?
    .build()?;
```

**Pros:**
- Uses system browser engine (Edge/WebKit/Blink)
- 10-50MB RAM (vs 200-500MB for Chrome)
- Native integration

**Cons:**
- Platform-specific (different engines per OS)
- Limited headless support
- Harder to capture screenshots

## Recommendation: Tiered Approach

### Tier 1: Lightweight (for simple sites)

```rust
if is_static_or_simple(url) {
    // Use simple HTML renderer
    use scraper::Html;
    let html = reqwest::get(url).await?.text().await?;
    render_html_to_image(&html)?;  // Simple rendering, no JS
}
```

**Resource:** 10-20MB
**Use case:** Documentation, wikis, static sites

### Tier 2: Pooled Chrome (for normal sites)

```rust
// Shared Chrome instance, 5-10 tabs
let tab = chrome_pool.allocate_tab(url).await?;
```

**Resource:** 200MB / 10 tabs = 20MB per session
**Use case:** Most websites

### Tier 3: Dedicated Chrome (for sensitive/heavy sites)

```rust
// Dedicated browser instance
let browser = Browser::new_isolated(url).await?;
```

**Resource:** 200-500MB
**Use case:** Banking, heavy web apps

### Popup/Tab Handling Strategy

**Recommended: Tab Notifications + User Control**

```rust
pub struct RbiSession {
    main_tab: Tab,
    popup_tabs: Vec<(String, Tab)>,  // (url, tab)
    active_tab_index: usize,
}

impl RbiSession {
    async fn handle_popup(&mut self, url: &str) -> Result<()> {
        // 1. Notify client
        self.send_notification(PopupNotification {
            url: url.to_string(),
            allow_popup: true,  // Or false to block
        }).await?;

        // 2. If allowed, create tab
        let popup_tab = self.browser.new_tab()?;
        popup_tab.navigate_to(url)?;
        self.popup_tabs.push((url.to_string(), popup_tab));

        // 3. Send tab bar update to client
        self.send_tab_list_update().await?;

        Ok(())
    }

    async fn switch_tab(&mut self, index: usize) -> Result<()> {
        self.active_tab_index = index;
        // Only capture/send active tab
        self.capture_active_tab().await?;
        Ok(())
    }
}
```

**Client-side (browser):**
```javascript
// Show tab bar in UI
onTabCreated((tabId, url) => {
    addTabToUI(tabId, url);
});

// User clicks tab
onTabClick((tabId) => {
    sendTabSwitch(tabId);
});
```

## Final Recommendation for guacr-rbi

### Phase 1: Chrome with Pooling

**Implementation:**
```rust
pub struct RbiHandler {
    chrome_pool: Arc<ChromePool>,
    config: RbiConfig,
}

pub struct RbiConfig {
    pool_size: usize,              // Default: 3-5 browser instances
    max_tabs_per_browser: usize,   // Default: 10
    popup_handling: PopupMode,     // Block, Merge, or Tabs
    capture_fps: u32,              // Default: 30
}

pub enum PopupMode {
    Block,          // Block all popups
    MergeToMain,    // Navigate main tab to popup URL
    ShowAsTabs,     // Expose tab bar to user
}
```

### Phase 2: Add Lightweight Tier

For static content, use simple rendering:
```rust
// Check if URL is static
if url.ends_with(".html") || is_documentation_site(url) {
    use simple_html_renderer::render;
    let image = render(html).await?;
    // Send image, no browser needed
}
```

### Phase 3: Consider Servo (Future)

When Servo matures (1-2 years):
- Rust-native
- Embeddable
- Potentially much lighter
- Better security

## Implementation Plan for guacr-rbi

```rust
// crates/guacr-rbi/src/handler.rs

use headless_chrome::{Browser, LaunchOptions, Tab};
use std::sync::Arc;
use parking_lot::Mutex;

pub struct RbiHandler {
    chrome_pool: Arc<ChromePool>,
    config: RbiConfig,
    sessions: Arc<Mutex<HashMap<String, RbiSession>>>,
}

pub struct RbiSession {
    browser_ref: Arc<Browser>,  // From pool or dedicated
    main_tab: Arc<Tab>,
    popups: Vec<PopupTab>,
    last_screenshot: Option<Vec<u8>>,
}

pub struct PopupTab {
    url: String,
    tab: Arc<Tab>,
    tab_id: String,
}

impl RbiHandler {
    async fn connect(...) -> Result<()> {
        // 1. Allocate browser/tab from pool
        let session = self.chrome_pool.allocate_session(url).await?;

        // 2. Set up popup intercept
        session.main_tab.evaluate(r#"
            window.open = function(url, name, specs) {
                window.postMessage({type: 'popup', url}, '*');
                return null;
            };
        "#)?;

        // 3. Listen for popup events
        session.main_tab.on_event(|event| {
            if let Event::WindowOpen(url) = event {
                handle_popup(url);
            }
        });

        // 4. Capture loop
        let mut interval = tokio::time::interval(Duration::from_millis(33)); // 30 FPS

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Capture screenshot
                    let screenshot = session.main_tab.capture_screenshot(
                        Format::PNG,
                        None,
                        true,  // from_surface
                    )?;

                    // Detect changes
                    if has_changed(&screenshot, &session.last_screenshot) {
                        // Send to client
                        send_image(&screenshot).await?;
                        session.last_screenshot = Some(screenshot);
                    }
                }

                Some(msg) = from_client.recv() => {
                    // Handle input, tab switching
                }
            }
        }
    }

    async fn handle_popup(&mut self, session_id: &str, url: &str) -> Result<()> {
        match self.config.popup_handling {
            PopupMode::Block => {
                // Do nothing - already blocked by window.open override
                Ok(())
            }
            PopupMode::MergeToMain => {
                // Navigate main tab
                let session = self.sessions.lock().get(session_id)?;
                session.main_tab.navigate_to(url)?;
                Ok(())
            }
            PopupMode::ShowAsTabs => {
                // Create new tab
                let session = self.sessions.lock().get_mut(session_id)?;
                let popup_tab = session.browser_ref.new_tab()?;
                popup_tab.navigate_to(url)?;

                session.popups.push(PopupTab {
                    url: url.to_string(),
                    tab: Arc::new(popup_tab),
                    tab_id: uuid::Uuid::new_v4().to_string(),
                });

                // Notify client
                send_tab_created_notification(...).await?;
                Ok(())
            }
        }
    }
}
```

## Resource Usage Comparison

| Approach | RAM per Session | Concurrent Sessions | Total RAM |
|----------|----------------|---------------------|-----------|
| Dedicated Chrome | 300MB | 50 | 15GB |
| Pooled Chrome (5 browsers, 10 tabs/each) | 50MB | 50 | 2.5GB |
| Simple renderer + Chrome | 20MB avg | 50 | 1GB |
| Servo (future) | 50-100MB | 50 | 2.5-5GB |

**Savings: 6-15x with pooling**

## Popup Handling Recommendation

### Default: Block + Notification

```rust
PopupConfig {
    default_action: PopupAction::Block,
    notify_user: true,
    allow_list: vec![
        "oauth.google.com",  // Allow OAuth popups
        "login.microsoft.com",
    ],
}
```

**User sees:**
```
[Popup blocked: https://ad-network.com]
[Allow] [Block Forever]
```

### Advanced: Tab Bar (Like Browser)

Send custom protocol message:
```
OPCODE: TAB_CREATED
Args: [tab_id, url, title]
```

Browser client shows Chrome-like tab bar.

## Implementation Priority

### Phase 1 (MVP):
- Single Chrome instance per session
- Block all popups
- Simple screenshot capture

### Phase 2 (Optimization):
- Chrome process pool (5 browsers)
- Tab allocation
- 6x resource savings

### Phase 3 (Advanced):
- Popup allow-list
- Tab bar in client
- Multi-tab support

### Phase 4 (Future):
- Evaluate Servo (when mature)
- Simple renderer for static content
- Adaptive engine selection

## Answer to Your Questions

**"Is Chrome the best option?"**
- For now, YES - most compatible, best tooling
- BUT use pooling to reduce resources (6-15x savings)
- Future: Consider Servo when mature

**"How do we handle popups?"**
- Default: Block all (override window.open)
- Advanced: Allow-list for OAuth/login
- Optional: Expose tab bar to users

**"How do we handle multiple tabs?"**
- Option 1: Block (force single tab)
- Option 2: Tab switching (show tab bar in client)
- Option 3: Grid view (show all tabs simultaneously)

**Recommended for guacr:**
- Use headless Chrome with process pooling
- Block popups by default with allow-list
- Single tab per session (simplest)
- Add tab support later if needed

Want me to implement this pooled Chrome approach?
