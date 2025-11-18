# RBI with Servo + Database with whoDB Integration

## Security Concern: No Pooling

**You're absolutely right** - pooling Chrome instances between users is a security risk:
- Memory leaks between sessions
- Cookie/session contamination
- Cache poisoning
- Side-channel attacks

**Solution: Dedicated instances per user, BUT use lighter engine**

---

## Option: Servo Browser Engine

### What is Servo?

**Servo** is a Rust-native browser engine (like WebKit/Blink):
- Written entirely in Rust
- Memory safe by design
- Embeddable components
- Multicore parallelism
- Modular architecture

**Status:** v0.0.1 (experimental, but improving fast)

### Servo for RBI - Deep Dive

**Pros:**
✅ **Rust-native** - Perfect integration with guacr
✅ **Embeddable** - Direct API, no subprocess overhead
✅ **Lighter than Chrome** - Potentially 50-150MB vs 200-500MB
✅ **Memory safe** - No C++ vulnerabilities
✅ **Sandboxed by design** - Each instance truly isolated
✅ **No IPC overhead** - Direct function calls
✅ **Customizable** - Can strip features we don't need
✅ **Future-proof** - Rust ecosystem growing

**Cons:**
❌ **Not production ready** (2024) - Many sites broken
❌ **Limited compatibility** - WebGL, WebRTC, many APIs missing
❌ **No stable API** - Breaking changes between versions
❌ **Performance unoptimized** - Slower than Chrome currently
❌ **Small community** - Limited support

### Servo Integration Approach

```rust
// crates/guacr-rbi/Cargo.toml
[dependencies]
servo = "0.0.1"
servo-url = "0.0.1"

// crates/guacr-rbi/src/servo_backend.rs
use servo::Servo;
use servo::embedder_traits::EventLoopWaker;

pub struct ServoRbiBackend {
    servo: Servo,
    compositor: Compositor,
}

impl ServoRbiBackend {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let mut servo = Servo::new(EmbedderCallbacks {
            width,
            height,
            // Event handlers
        });

        Ok(Self {
            servo,
            compositor: Compositor::new(width, height),
        })
    }

    pub fn navigate(&mut self, url: &str) -> Result<()> {
        self.servo.handle_events(vec![
            EmbedderEvent::LoadUrl(url.parse()?)
        ]);
        Ok(())
    }

    pub fn capture_frame(&mut self) -> Result<Vec<u8>> {
        // Servo renders to OpenGL context
        // Read framebuffer and encode to PNG
        let pixels = self.compositor.read_pixels()?;
        encode_to_png(&pixels)
    }

    pub fn inject_mouse(&mut self, x: i32, y: i32, button: MouseButton) {
        self.servo.handle_events(vec![
            EmbedderEvent::MouseWindowEventClass(
                MouseWindowEvent::Click(button, Point2D::new(x, y))
            )
        ]);
    }

    pub fn inject_key(&mut self, key: Key) {
        self.servo.handle_events(vec![
            EmbedderEvent::Keyboard(KeyboardEvent::KeyPress(key))
        ]);
    }
}
```

**Benefits:**
- No subprocess - direct Rust API
- True isolation (no shared memory)
- Lighter weight
- Security from Rust memory safety

**Risks:**
- Website compatibility issues
- Bugs in Servo
- API instability

### Hybrid Approach: Servo + Chrome Fallback

```rust
pub enum RbiBackend {
    Servo(ServoRbiBackend),
    Chrome(ChromeRbiBackend),
}

impl RbiHandler {
    async fn select_backend(&self, url: &str) -> RbiBackend {
        // Try Servo first for simple sites
        if self.is_servo_compatible(url) {
            RbiBackend::Servo(ServoRbiBackend::new()?)
        } else {
            // Fallback to Chrome for complex sites
            RbiBackend::Chrome(ChromeRbiBackend::new()?)
        }
    }

    fn is_servo_compatible(&self, url: &str) -> bool {
        // Allowlist of known-working sites
        let domain = extract_domain(url);
        matches!(domain,
            "docs.rs" | "github.com" | "stackoverflow.com" |
            // Static documentation sites that work in Servo
        )
    }
}
```

**This gives:**
- Servo for 50-70% of traffic (docs, wikis, simple sites)
- Chrome for complex webapps (Gmail, Office 365, etc.)
- Best of both worlds

---

## whoDB for Database Handlers

### What is whoDB?

- **Lightweight database explorer** (< 50MB)
- Built in **Go** + **React**
- Supports MySQL, PostgreSQL, SQL Server, MongoDB, Redis, etc.
- **Spreadsheet-like interface**
- **AI-powered** natural language to SQL
- **No credential storage** (secure)

### Integration Options

#### Option 1: Embed whoDB Binary

**Architecture:**
```
guacr-database handler
  └─> Spawns whoDB subprocess
      └─> whoDB serves web UI
          └─> Capture whoDB's browser output
              └─> Send via WebRTC
```

**Pros:**
- Reuse existing whoDB features
- Spreadsheet interface (better than SQL terminal)
- AI-powered queries
- Multi-database support

**Cons:**
- Go subprocess (not Rust)
- Extra layer of indirection
- License compatibility?

#### Option 2: Learn from whoDB Architecture

**Build Rust equivalent:**

```rust
// crates/guacr-database/src/spreadsheet.rs

pub struct DatabaseSpreadsheet {
    columns: Vec<ColumnDef>,
    rows: Vec<Vec<CellValue>>,
    dirty_cells: HashSet<(usize, usize)>,
}

impl DatabaseSpreadsheet {
    pub fn from_query_result(result: QueryResult) -> Self {
        // Convert SQL result to spreadsheet
    }

    pub fn render_to_image(&self) -> Vec<u8> {
        // Render grid as image (like Excel)
        // Headers, data cells, scrollbars
    }

    pub fn handle_cell_click(&mut self, row: usize, col: usize) {
        // Allow inline editing
    }
}
```

**This gives:**
- Better UX than SQL terminal
- Pure Rust
- Lightweight
- No subprocess

#### Option 3: Hybrid - whoDB for Complex, SQL Terminal for Simple

```rust
pub enum DatabaseBackend {
    SqlTerminal,    // Simple SQL terminal (what we have)
    WhoDB,          // Full whoDB web UI (via embedded browser)
    Spreadsheet,    // Custom Rust spreadsheet renderer
}

impl DatabaseHandler {
    fn select_backend(&self, params: &HashMap<String, String>) -> DatabaseBackend {
        if params.get("mode") == Some(&"spreadsheet".to_string()) {
            DatabaseBackend::WhoDB
        } else {
            DatabaseBackend::SqlTerminal
        }
    }
}
```

### whoDB Integration Architecture

```rust
use std::process::{Command, Stdio};

pub struct WhoDbBackend {
    process: Child,
    port: u16,
    browser: Browser,  // Headless Chrome viewing whoDB UI
}

impl WhoDbBackend {
    pub async fn new(db_url: &str) -> Result<Self> {
        // 1. Start whoDB subprocess
        let process = Command::new("whodb")
            .arg("--database")
            .arg(db_url)
            .arg("--port")
            .arg("0")  // Random port
            .stdout(Stdio::piped())
            .spawn()?;

        // 2. Extract port from stdout
        let port = parse_port_from_output(&process)?;

        // 3. Launch browser pointing to whoDB
        let browser = Browser::new(LaunchOptions {
            headless: true,
            ..Default::default()
        })?;

        let tab = browser.new_tab()?;
        tab.navigate_to(&format!("http://localhost:{}", port))?;

        Ok(Self { process, port, browser })
    }

    pub fn capture_screenshot(&self) -> Result<Vec<u8>> {
        self.browser.wait_for_element("table.data-grid")?;
        self.browser.capture_screenshot(Format::PNG, None, true)
    }
}

impl Drop for WhoDbBackend {
    fn drop(&mut self) {
        self.process.kill().ok();
    }
}
```

**Resource usage:**
- whoDB: 30-50MB
- Chrome viewing it: 150-200MB
- Total: 180-250MB (vs 300-500MB for full Chrome RBI)

---

## Recommendation: Dual Strategy

### For RBI (Browser Isolation):

**Phase 1: Dedicated Chrome Instances**
```rust
pub struct RbiHandler {
    // One Chrome per user - secure isolation
    browser: Browser,  // 200-500MB per session
    popup_mode: PopupMode::Block,  // Block all popups
}
```

**Phase 2: Add Servo for Simple Sites (Future)**
```rust
pub enum RbiBackend {
    Chrome,  // For complex sites (Gmail, Office365)
    Servo,   // For simple sites (docs, wikis) - when stable
}
```

**Resource savings:**
- Servo: 50-100MB vs Chrome 200-500MB
- Only for compatible sites
- Fallback to Chrome if broken

**Timeline:**
- Chrome now (production ready)
- Servo in 1-2 years (when v1.0)

### For Database Handlers:

**Option A: SQL Terminal (Current)**
- Lightweight (5MB)
- Pure Rust
- Works well for SQL power users

**Option B: Rust Spreadsheet Renderer (Custom)**
- Build spreadsheet view in Rust
- Render as PNG
- Better UX than terminal
- 10-20MB

**Option C: whoDB Integration (Future)**
- Best UX (spreadsheet + AI queries)
- 30-50MB (whoDB) + 150MB (Chrome to view it) = 180-250MB
- Not pure Rust (Go subprocess)
- Licensing concerns

**Recommendation:**
1. **Start with SQL Terminal** (what we have - simple, works)
2. **Add Rust Spreadsheet** later (better UX, pure Rust)
3. **Consider whoDB** if UX critical (requires Go + Chrome)

### Popup Handling (No Pooling)

**With dedicated instances:**
```rust
PopupStrategy::Block {
    // Override window.open
    // Block all popups
    // Optional: Whitelist for OAuth (google.com/oauth, etc.)
}

PopupStrategy::NewWindow {
    // Allow popups, but track them
    // Each popup gets its own Chrome instance (isolated!)
    // Show multiple windows in client (expensive but secure)
}
```

## Updated RBI Recommendation

**For Security (no pooling):**

```rust
pub struct RbiHandler {
    config: RbiConfig,
}

pub struct RbiConfig {
    backend: RbiBackendType,
    popup_handling: PopupHandling,
    resource_limits: ResourceLimits,
}

pub enum RbiBackendType {
    Chrome,          // Production: 200-500MB, full compatibility
    Servo,           // Future: 50-100MB, limited compatibility
    ServoWithFallback, // Try Servo, fallback to Chrome if broken
}

pub enum PopupHandling {
    Block,                    // Block all (simplest, most secure)
    AllowWithIsolation,       // Each popup = new isolated instance
    AllowList(Vec<String>),   // Only allow specific domains (OAuth)
}

pub struct ResourceLimits {
    max_memory_mb: usize,     // Kill if exceeds (default: 500MB)
    max_cpu_percent: u32,     // Throttle if exceeds (default: 80%)
    max_popups: usize,        // Max concurrent popups (default: 3)
}
```

**Implementation:**

```rust
impl RbiHandler {
    async fn connect(...) -> Result<()> {
        // 1. Launch dedicated browser (no sharing!)
        let browser = match self.config.backend {
            RbiBackendType::Servo => self.launch_servo()?,
            RbiBackendType::Chrome => self.launch_chrome()?,
            RbiBackendType::ServoWithFallback => {
                self.launch_servo()
                    .or_else(|_| self.launch_chrome())?
            }
        };

        // 2. Set resource limits
        self.apply_resource_limits(&browser)?;

        // 3. Navigate
        browser.navigate(url)?;

        // 4. Block popups or handle with isolation
        browser.intercept_popups(self.config.popup_handling)?;

        // 5. Capture and stream
        self.capture_loop(browser).await?;

        Ok(())
    }

    fn apply_resource_limits(&self, browser: &Browser) -> Result<()> {
        // cgroups on Linux, job objects on Windows
        #[cfg(target_os = "linux")]
        {
            use cgroups_rs::*;
            let cg = Cgroup::new("guacr-rbi", &self.browser_pid)?;
            cg.set_memory_limit(self.config.resource_limits.max_memory_mb * 1024 * 1024)?;
            cg.set_cpu_quota(self.config.resource_limits.max_cpu_percent)?;
        }

        Ok(())
    }
}
```

## Servo Integration Plan

### Current Servo Status (2024-2025)

**What works:**
- Basic HTML/CSS rendering
- Simple JavaScript
- Static sites
- Documentation sites

**What doesn't work:**
- Complex web apps (Gmail, Office 365)
- Many modern JS frameworks
- WebRTC, WebGL (limited)
- Some CSS features

### Practical Servo Strategy

**Use Servo for allowlist of sites:**

```rust
pub struct ServoCompatibility {
    known_working: HashSet<&'static str>,
    known_broken: HashSet<&'static str>,
}

impl ServoCompatibility {
    fn is_compatible(&self, url: &str) -> bool {
        let domain = extract_domain(url);

        // Check allowlist
        if self.known_working.contains(domain) {
            return true;
        }

        // Check blocklist
        if self.known_broken.contains(domain) {
            return false;
        }

        // Heuristics
        self.likely_works(url)
    }

    fn likely_works(&self, url: &str) -> bool {
        // Static sites, docs, wikis likely work
        url.contains("/docs/") ||
        url.contains("documentation") ||
        url.ends_with(".html") ||
        url.contains("github.com") && !url.contains("/actions/")
    }
}

// In RBI handler
if servo_compat.is_compatible(&url) {
    // Use Servo (50-100MB, Rust-native, fast)
    launch_servo(url)?
} else {
    // Use Chrome (200-500MB, full compatibility)
    launch_chrome(url)?
}
```

**Gradual adoption:**
- Start: 10% traffic to Servo (docs, wikis)
- Monitor: Track compatibility issues
- Grow: 30-50% traffic as Servo matures
- Future: Maybe 80%+ when Servo reaches v1.0

---

## whoDB for Database Handlers

### Integration Option 1: Embed whoDB Binary

**Architecture:**
```
User → WebRTC → guacr-database handler
                    ↓
                Spawns whoDB (Go binary)
                    ↓
                Serves on localhost:RANDOM_PORT
                    ↓
                guacr-rbi captures whoDB UI
                    ↓
                Sends screenshots via WebRTC
```

**Implementation:**
```rust
pub struct WhoDbDatabaseHandler {
    whodb_process: Child,
    rbi_session: RbiSession,  // RBI handler viewing whoDB
}

impl WhoDbDatabaseHandler {
    async fn connect(...) -> Result<()> {
        // 1. Start whoDB with database credentials
        let whodb = Command::new("whodb")
            .arg("--database")
            .arg(format!("postgresql://{}:{}@{}:{}/{}",
                username, password, hostname, port, database))
            .arg("--port")
            .arg("0")  // Random port
            .spawn()?;

        // Parse port from whodb output
        let port = extract_port(&whodb)?;

        // 2. Launch RBI to view whoDB UI
        let rbi_url = format!("http://localhost:{}", port);
        self.rbi_session.navigate(&rbi_url).await?;

        // 3. Capture whoDB UI and send to client
        self.rbi_session.capture_loop().await?;

        Ok(())
    }
}
```

**Pros:**
- Best database UX (spreadsheet, visual query builder, AI)
- No need to build database UI
- Multi-database support

**Cons:**
- Requires Go binary (not pure Rust)
- Extra process overhead
- Chrome still needed to view UI
- License compatibility?

### Integration Option 2: Learn from whoDB, Build in Rust

**Create Rust equivalent:**

```rust
// crates/guacr-database/src/spreadsheet_view.rs

use image::{RgbaImage, Rgba};

pub struct SpreadsheetView {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    scroll_offset: usize,
    selected_cell: Option<(usize, usize)>,
}

impl SpreadsheetView {
    pub fn from_query_result(result: &QueryResult) -> Self {
        Self {
            columns: result.columns.clone(),
            rows: result.rows.clone(),
            scroll_offset: 0,
            selected_cell: None,
        }
    }

    pub fn render_to_image(&self, width: u32, height: u32) -> Vec<u8> {
        let mut img = RgbaImage::new(width, height);

        // Draw grid
        self.draw_headers(&mut img)?;
        self.draw_cells(&mut img)?;
        self.draw_grid_lines(&mut img)?;
        self.draw_scrollbar(&mut img)?;

        encode_to_png(&img)
    }

    pub fn handle_click(&mut self, x: u32, y: u32) {
        let (row, col) = self.pixel_to_cell(x, y);
        self.selected_cell = Some((row, col));
    }

    pub fn handle_scroll(&mut self, delta: i32) {
        self.scroll_offset = (self.scroll_offset as i32 + delta).max(0) as usize;
    }
}
```

**Benefits:**
- Pure Rust
- Lightweight (renders to image, no browser needed)
- Custom-tailored to our needs
- No licensing issues

**Work:**
- Need to build spreadsheet renderer
- ~1-2 weeks of development

### Option 3: Keep SQL Terminal + Add Export

**Enhance current SQL terminal:**

```rust
impl SqlTerminal {
    pub fn execute_query(&mut self, query: &str, db: &Database) -> Result<()> {
        let result = db.query(query).await?;

        // Option 1: Render as table (current)
        self.write_table(&result.columns, &result.rows)?;

        // Option 2: Export as CSV
        let csv = self.format_as_csv(&result)?;
        self.send_file_download("result.csv", &csv)?;

        // Option 3: Render as image grid (simple spreadsheet)
        let grid_image = self.render_grid(&result)?;
        self.send_image(&grid_image)?;

        Ok(())
    }
}
```

---

## Final Recommendations

### For RBI:

**Phase 1: Dedicated Chrome (Secure)**
```rust
RbiConfig {
    backend: RbiBackendType::Chrome,
    popup_handling: PopupHandling::Block,
    resource_limits: ResourceLimits {
        max_memory_mb: 500,
        max_popups: 0,  // Block all
    },
}
```

**Phase 2: Add Servo for Simple Sites (When Stable)**
```rust
RbiConfig {
    backend: RbiBackendType::ServoWithFallback,  // Try Servo, fallback to Chrome
    servo_allowlist: vec!["docs.rs", "github.com", ...],
}
```

**Popup handling:** Block all by default, optional allowlist for OAuth

### For Database:

**Phase 1: SQL Terminal (Current)**
- What we have
- Lightweight, works

**Phase 2: Add Spreadsheet View (Pure Rust)**
- Build custom spreadsheet renderer
- Render to PNG
- Better UX
- Still lightweight (10-20MB)

**Phase 3: Consider whoDB (If UX Critical)**
- Requires Go binary
- Best features
- Requires RBI to view UI

## My Recommendation

**RBI:**
- Use dedicated Chrome instances (no pooling - secure)
- Block popups by default
- Add Servo support when it matures (1-2 years)
- Resource limits via cgroups

**Database:**
- Keep SQL terminal for now (works, lightweight)
- Build custom Rust spreadsheet renderer (pure Rust, better UX)
- Skip whoDB (adds Go dependency, complexity)

Want me to update the RBI handler to use dedicated Chrome with popup blocking and resource limits?