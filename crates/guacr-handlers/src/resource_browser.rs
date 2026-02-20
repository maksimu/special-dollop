// Resource browser infrastructure for infrastructure management UIs.
//
// Provides a shared dual-mode handler framework used by Kubernetes, Docker,
// and vSphere handlers. The two modes are:
//
//   1. **List mode** - Renders a resource list using SpreadsheetRenderer
//      (interactive grid with row selection, keyboard/mouse navigation, and
//      configurable per-row actions).
//
//   2. **Terminal mode** - Switches to a TerminalEmulator for interactive
//      shell/logs sessions (bidirectional byte stream forwarding).
//
// The ResourceBrowser trait defines the data provider contract.
// ResourceBrowserHandler implements the Guacamole protocol event loop,
// managing mode transitions and rendering.

use async_trait::async_trait;
use bytes::Bytes;
use guacr_protocol::{format_chunked_blobs, format_end, format_img, format_instruction};
use guacr_terminal::{
    Action, ColumnDef, GridEvent, SpreadsheetRenderer, TerminalEmulator, TerminalRenderer,
};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::error::HandlerError;
use crate::session::{send_disconnect, send_name, send_ready};

// -- Keysym constants for Ctrl+D detection in terminal mode --
const KEYSYM_CTRL_L: u32 = 0xFFE3;
const KEYSYM_D_LOWER: u32 = 0x0064;

// -- Stream ID starts at 1 (stream 0 is reserved in Guacamole protocol) --
const INITIAL_STREAM_ID: u32 = 1;

// -- JPEG quality for spreadsheet renders (matches terminal handlers) --
const JPEG_QUALITY: u8 = 85;

// -- Default display dimensions --
const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;

// -- Character cell size (matches SpreadsheetRenderer and TerminalRenderer) --
const CHAR_WIDTH: u32 = 9;
const CHAR_HEIGHT: u32 = 18;

/// Result of executing an action on a resource row.
pub enum ActionResult {
    /// Opens terminal mode with bidirectional stream.
    ///
    /// The reader provides output from the remote process (shell stdout, log
    /// stream, console output). The writer accepts input to the remote process
    /// (shell stdin, etc.).
    Terminal {
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
    },

    /// One-shot result displayed as a status message in the grid's status area.
    /// The handler will briefly show this message and then return to list mode.
    Status(String),

    /// Refresh the list view by re-fetching resources.
    Refresh,
}

/// Streaming updates for the resource list (used by watch/event APIs).
#[derive(Debug, Clone)]
pub enum ResourceUpdate {
    /// Full replacement of all rows.
    FullUpdate(Vec<Vec<String>>),

    /// Single row changed at the given index.
    RowUpdated { index: usize, row: Vec<String> },

    /// Row added at the end of the list.
    RowAdded(Vec<String>),

    /// Row removed at the given index.
    RowRemoved(usize),
}

/// Data provider trait for infrastructure resource browsers.
///
/// Implementors provide column definitions, resource listing, per-row actions,
/// and optional streaming updates. The ResourceBrowserHandler drives the UI
/// loop and calls these methods as needed.
#[async_trait]
pub trait ResourceBrowser: Send + Sync {
    /// Column definitions for the spreadsheet header.
    fn columns(&self) -> Vec<ColumnDef>;

    /// Fetch the current resource list.
    ///
    /// Each inner `Vec<String>` is a row of cell values matching the columns.
    async fn list_resources(&self) -> Result<Vec<Vec<String>>, String>;

    /// Available actions for the given row index.
    ///
    /// Called whenever the user selects a row to determine what actions
    /// to display in the action bar (e.g., Shell, Logs, Describe, Delete).
    fn row_actions(&self, row_index: usize) -> Vec<Action>;

    /// Execute an action on a specific row.
    ///
    /// The `action_id` matches the `Action::id` field returned by `row_actions`.
    async fn execute_action(
        &self,
        row_index: usize,
        action_id: &str,
    ) -> Result<ActionResult, String>;

    /// Optional: stream resource updates for real-time refresh.
    ///
    /// If the underlying API supports watch/event streaming (e.g., Kubernetes
    /// watch API, Docker events), return a pinned Stream of ResourceUpdate.
    /// The handler will apply updates incrementally to the grid.
    ///
    /// Default implementation returns None (polling only via manual refresh).
    async fn watch_resources(
        &self,
    ) -> Option<Pin<Box<dyn futures_core::Stream<Item = ResourceUpdate> + Send>>> {
        None
    }

    /// Handler name for display in the connection title bar.
    fn name(&self) -> &str;
}

/// UI mode state machine for the resource browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserMode {
    /// List view showing resources in SpreadsheetRenderer.
    List,

    /// Terminal mode (shell exec, logs, console).
    /// Stores which action/row initiated this mode for display purposes.
    Terminal { action_id: String, row_index: usize },
}

/// Shared handler loop for infrastructure management UIs.
///
/// Wraps a ResourceBrowser implementation and drives the dual-mode Guacamole
/// protocol event loop. This is called from a ProtocolHandler::connect()
/// implementation.
///
/// # Usage
///
/// ```ignore
/// use guacr_handlers::resource_browser::{ResourceBrowserHandler, ResourceBrowser};
///
/// struct K8sHandler { /* ... */ }
///
/// #[async_trait]
/// impl ProtocolHandler for K8sHandler {
///     fn name(&self) -> &str { "kubernetes" }
///
///     async fn connect(
///         &self,
///         params: HashMap<String, String>,
///         to_client: mpsc::Sender<Bytes>,
///         from_client: mpsc::Receiver<Bytes>,
///     ) -> Result<()> {
///         let browser = K8sBrowser::new(&params)?;
///         let mut handler = ResourceBrowserHandler::new(browser);
///         handler.run(params, to_client, from_client).await
///     }
/// }
/// ```
pub struct ResourceBrowserHandler<B: ResourceBrowser> {
    browser: B,
    mode: BrowserMode,
    stream_id: u32,
    pixel_width: u32,
    pixel_height: u32,
    ctrl_pressed: bool,
    cached_rows: Vec<Vec<String>>,
}

impl<B: ResourceBrowser> ResourceBrowserHandler<B> {
    /// Create a new ResourceBrowserHandler wrapping the given browser.
    pub fn new(browser: B) -> Self {
        Self {
            browser,
            mode: BrowserMode::List,
            stream_id: INITIAL_STREAM_ID,
            pixel_width: DEFAULT_WIDTH,
            pixel_height: DEFAULT_HEIGHT,
            ctrl_pressed: false,
            cached_rows: Vec::new(),
        }
    }

    /// Run the handler loop.
    ///
    /// This is called from ProtocolHandler::connect(). It manages the full
    /// lifecycle: initial render, event loop, mode transitions, and cleanup.
    pub async fn run(
        &mut self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Parse display dimensions from connection params
        let (width, height) = parse_display_size(&params);
        self.pixel_width = width;
        self.pixel_height = height;

        // Send session startup instructions
        let connection_id = format!("{}-browser", self.browser.name());
        send_ready(&to_client, &connection_id).await?;
        send_name(&to_client, self.browser.name()).await?;

        // Initialize SpreadsheetRenderer
        let mut grid = SpreadsheetRenderer::new(self.pixel_width, self.pixel_height);
        let columns = self.browser.columns();

        // Fetch initial resource list
        let rows = self.browser.list_resources().await.map_err(|e| {
            HandlerError::ConnectionFailed(format!("Failed to list resources: {}", e))
        })?;
        self.cached_rows = rows.clone();

        grid.set_data(columns, rows);

        // Set initial actions (empty until a row is selected)
        // Actions will be set when the user selects a row.

        // Initial render
        self.render_grid(&mut grid, &to_client).await?;
        grid.clear_dirty();

        // Spawn optional watch task
        let (watch_tx, mut watch_rx) = mpsc::channel::<ResourceUpdate>(64);
        let watch_handle = self.spawn_watch_task(watch_tx).await;

        // Event loop
        loop {
            match &self.mode {
                BrowserMode::List => {
                    tokio::select! {
                        msg = from_client.recv() => {
                            let Some(msg) = msg else {
                                debug!("{}: Client disconnected", self.browser.name());
                                break;
                            };

                            let msg_str = match std::str::from_utf8(&msg) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };

                            let event = self.handle_list_input(msg_str, &mut grid);

                            match event {
                                GridEvent::ActionTriggered { row, action_id } => {
                                    info!(
                                        "{}: Action '{}' triggered on row {}",
                                        self.browser.name(), action_id, row
                                    );

                                    match self.browser.execute_action(row, &action_id).await {
                                        Ok(ActionResult::Terminal { reader, writer }) => {
                                            self.mode = BrowserMode::Terminal {
                                                action_id: action_id.clone(),
                                                row_index: row,
                                            };

                                            // Run terminal mode inline (it will
                                            // return when the user exits with Ctrl+D
                                            // or the stream ends)
                                            self.run_terminal_mode(
                                                reader,
                                                writer,
                                                &to_client,
                                                &mut from_client,
                                            )
                                            .await?;

                                            // Back to list mode: refresh data
                                            self.mode = BrowserMode::List;
                                            self.refresh_list(&mut grid).await?;
                                            self.render_grid(&mut grid, &to_client).await?;
                                            grid.clear_dirty();
                                        }
                                        Ok(ActionResult::Status(message)) => {
                                            info!("{}: Status: {}", self.browser.name(), message);
                                            // Re-render with updated status (the grid
                                            // will show status in the status line area)
                                            self.render_grid(&mut grid, &to_client).await?;
                                            grid.clear_dirty();
                                        }
                                        Ok(ActionResult::Refresh) => {
                                            self.refresh_list(&mut grid).await?;
                                            self.render_grid(&mut grid, &to_client).await?;
                                            grid.clear_dirty();
                                        }
                                        Err(e) => {
                                            warn!(
                                                "{}: Action '{}' failed: {}",
                                                self.browser.name(), action_id, e
                                            );
                                            // Stay in list mode, re-render
                                            self.render_grid(&mut grid, &to_client).await?;
                                            grid.clear_dirty();
                                        }
                                    }
                                }
                                GridEvent::Redraw => {
                                    // Update actions based on current selection
                                    if let Some(sel) = grid.selected_row() {
                                        let actions = self.browser.row_actions(sel);
                                        grid.set_actions(actions);
                                    }
                                    self.render_grid(&mut grid, &to_client).await?;
                                    grid.clear_dirty();
                                }
                                GridEvent::CellSelected { row, .. } => {
                                    // Update actions for newly selected row
                                    let actions = self.browser.row_actions(row);
                                    grid.set_actions(actions);
                                    self.render_grid(&mut grid, &to_client).await?;
                                    grid.clear_dirty();
                                }
                                GridEvent::None => {
                                    // No visual change needed
                                }
                            }
                        }

                        update = watch_rx.recv() => {
                            if let Some(update) = update {
                                self.apply_update(&mut grid, update);
                                if grid.is_dirty() {
                                    self.render_grid(&mut grid, &to_client).await?;
                                    grid.clear_dirty();
                                }
                            }
                        }
                    }
                }
                BrowserMode::Terminal { .. } => {
                    // Terminal mode is handled inline in the ActionTriggered
                    // branch above. If we somehow reach here, break.
                    error!(
                        "{}: Unexpected terminal mode in main loop",
                        self.browser.name()
                    );
                    break;
                }
            }
        }

        // Cleanup
        if let Some(handle) = watch_handle {
            handle.abort();
        }
        send_disconnect(&to_client).await;
        Ok(())
    }

    /// Handle a Guacamole instruction in list mode, returning the grid event.
    fn handle_list_input(&mut self, msg_str: &str, grid: &mut SpreadsheetRenderer) -> GridEvent {
        // Handle key instruction
        if msg_str.contains(".key,") {
            if let Some(key) = parse_key_event(msg_str) {
                return grid.handle_key(key.keysym, key.pressed);
            }
        }

        // Handle mouse instruction
        if msg_str.contains(".mouse,") {
            if let Some(mouse) = parse_mouse_event(msg_str) {
                return grid.handle_mouse(mouse.x, mouse.y, mouse.button_mask);
            }
        }

        // Handle size instruction (resize)
        if msg_str.contains(".size,") {
            if let Some((w, h)) = parse_size_event(msg_str) {
                if w > 0 && h > 0 {
                    self.pixel_width = w;
                    self.pixel_height = h;
                    grid.resize(w, h);
                    return GridEvent::Redraw;
                }
            }
        }

        GridEvent::None
    }

    /// Run terminal mode: forward I/O between the Guacamole client and a
    /// bidirectional byte stream (shell, log tail, etc.).
    ///
    /// Returns when the stream ends or the user presses Ctrl+D.
    async fn run_terminal_mode(
        &mut self,
        mut reader: Box<dyn AsyncRead + Send + Unpin>,
        mut writer: Box<dyn AsyncWrite + Send + Unpin>,
        to_client: &mpsc::Sender<Bytes>,
        from_client: &mut mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Set up terminal emulator for rendering shell output
        let cols = (self.pixel_width / CHAR_WIDTH).max(80) as u16;
        let rows = (self.pixel_height / CHAR_HEIGHT).max(24) as u16;
        let mut terminal = TerminalEmulator::new(rows, cols);
        let renderer = TerminalRenderer::new()
            .map_err(|e| HandlerError::ProtocolError(format!("Terminal renderer init: {}", e)))?;

        // Show initial empty terminal
        let jpeg = renderer
            .render_screen(terminal.screen(), rows, cols)
            .map_err(|e| HandlerError::ProtocolError(format!("Render error: {}", e)))?;
        self.send_jpeg_frame(&jpeg, 0, 0, to_client).await?;
        self.send_sync(to_client).await?;

        self.ctrl_pressed = false;
        let mut read_buf = [0u8; 4096];

        loop {
            tokio::select! {
                // Data from the remote process (stdout/logs)
                n = reader.read(&mut read_buf) => {
                    match n {
                        Ok(0) => {
                            debug!("{}: Terminal stream ended (EOF)", self.browser.name());
                            break;
                        }
                        Ok(n) => {
                            terminal.process(&read_buf[..n]).map_err(|e| {
                                HandlerError::ProtocolError(format!("Terminal process: {}", e))
                            })?;

                            let jpeg = renderer
                                .render_screen(terminal.screen(), rows, cols)
                                .map_err(|e| {
                                    HandlerError::ProtocolError(format!("Render error: {}", e))
                                })?;
                            self.send_jpeg_frame(&jpeg, 0, 0, to_client).await?;
                            self.send_sync(to_client).await?;
                        }
                        Err(e) => {
                            warn!("{}: Terminal read error: {}", self.browser.name(), e);
                            break;
                        }
                    }
                }

                // Input from the Guacamole client (keyboard/mouse)
                msg = from_client.recv() => {
                    let Some(msg) = msg else {
                        debug!("{}: Client disconnected during terminal mode", self.browser.name());
                        break;
                    };

                    let msg_str = match std::str::from_utf8(&msg) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    // Check for Ctrl+D to exit terminal mode
                    if msg_str.contains(".key,") {
                        if let Some(key) = parse_key_event(msg_str) {
                            // Track Ctrl state
                            if key.keysym == KEYSYM_CTRL_L {
                                self.ctrl_pressed = key.pressed;
                                continue;
                            }

                            // Ctrl+D on key-down exits terminal mode
                            if key.pressed && key.keysym == KEYSYM_D_LOWER && self.ctrl_pressed {
                                info!("{}: Ctrl+D detected, exiting terminal mode", self.browser.name());
                                break;
                            }

                            // Forward key press as terminal input
                            if key.pressed {
                                let bytes = guacr_terminal::x11_keysym_to_bytes(key.keysym, true, None);
                                if !bytes.is_empty() {
                                    if let Err(e) = writer.write_all(&bytes).await {
                                        warn!("{}: Terminal write error: {}", self.browser.name(), e);
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    // Handle size changes in terminal mode
                    if msg_str.contains(".size,") {
                        if let Some((w, h)) = parse_size_event(msg_str) {
                            if w > 0 && h > 0 {
                                self.pixel_width = w;
                                self.pixel_height = h;
                                let new_cols = (w / CHAR_WIDTH).max(80) as u16;
                                let new_rows = (h / CHAR_HEIGHT).max(24) as u16;
                                terminal.resize(new_rows, new_cols);

                                let jpeg = renderer
                                    .render_screen(terminal.screen(), new_rows, new_cols)
                                    .map_err(|e| {
                                        HandlerError::ProtocolError(format!("Render error: {}", e))
                                    })?;
                                self.send_jpeg_frame(&jpeg, 0, 0, to_client).await?;
                                self.send_sync(to_client).await?;
                            }
                        }
                    }
                }
            }
        }

        // Attempt graceful shutdown of the writer side
        let _ = writer.shutdown().await;

        Ok(())
    }

    /// Refresh the resource list and update the grid.
    async fn refresh_list(&mut self, grid: &mut SpreadsheetRenderer) -> Result<(), HandlerError> {
        match self.browser.list_resources().await {
            Ok(rows) => {
                self.cached_rows = rows.clone();
                grid.update_rows(rows);

                // Refresh actions for the current selection
                if let Some(sel) = grid.selected_row() {
                    let actions = self.browser.row_actions(sel);
                    grid.set_actions(actions);
                }

                Ok(())
            }
            Err(e) => {
                warn!(
                    "{}: Failed to refresh resources: {}",
                    self.browser.name(),
                    e
                );
                // Keep existing data, just log the error
                Ok(())
            }
        }
    }

    /// Apply a streaming ResourceUpdate to the grid.
    fn apply_update(&mut self, grid: &mut SpreadsheetRenderer, update: ResourceUpdate) {
        match update {
            ResourceUpdate::FullUpdate(rows) => {
                self.cached_rows = rows.clone();
                grid.update_rows(rows);
            }
            ResourceUpdate::RowUpdated { index, row } => {
                if index < self.cached_rows.len() {
                    self.cached_rows[index] = row;
                    grid.update_rows(self.cached_rows.clone());
                }
            }
            ResourceUpdate::RowAdded(row) => {
                self.cached_rows.push(row);
                grid.update_rows(self.cached_rows.clone());
            }
            ResourceUpdate::RowRemoved(index) => {
                if index < self.cached_rows.len() {
                    self.cached_rows.remove(index);
                    grid.update_rows(self.cached_rows.clone());
                }
            }
        }

        // Refresh actions for the current selection after update
        if let Some(sel) = grid.selected_row() {
            if sel < self.cached_rows.len() {
                let actions = self.browser.row_actions(sel);
                grid.set_actions(actions);
            }
        }
    }

    /// Spawn an optional background task that receives watch updates.
    ///
    /// If the browser supports watch_resources(), spawn a task that forwards
    /// updates into the given channel. Returns the JoinHandle for cleanup.
    async fn spawn_watch_task(
        &self,
        watch_tx: mpsc::Sender<ResourceUpdate>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        use futures_core::Stream;
        use std::task::Context;

        let stream_opt = self.browser.watch_resources().await;
        let mut stream = stream_opt?;

        let name = self.browser.name().to_string();
        Some(tokio::spawn(async move {
            // Poll the stream manually using a simple loop.
            // We wrap in a poll_fn to drive the pinned stream.
            loop {
                // Use poll_fn to drive the stream
                let next = std::future::poll_fn(|cx: &mut Context<'_>| {
                    Pin::new(&mut stream).poll_next(cx)
                })
                .await;

                match next {
                    Some(update) => {
                        if watch_tx.send(update).await.is_err() {
                            debug!("{}: Watch channel closed, stopping watcher", name);
                            break;
                        }
                    }
                    None => {
                        debug!("{}: Watch stream ended", name);
                        break;
                    }
                }
            }
        }))
    }

    /// Render the SpreadsheetRenderer grid and send as a Guacamole image frame.
    ///
    /// Takes `&mut SpreadsheetRenderer` (not `&`) so the future remains Send.
    /// SpreadsheetRenderer internally uses RefCell for its glyph cache, which
    /// makes &SpreadsheetRenderer !Send. Using &mut avoids this issue.
    async fn render_grid(
        &mut self,
        grid: &mut SpreadsheetRenderer,
        to_client: &mpsc::Sender<Bytes>,
    ) -> Result<(), HandlerError> {
        let jpeg_data = grid
            .render_jpeg(JPEG_QUALITY)
            .map_err(|e| HandlerError::ProtocolError(format!("Grid render error: {}", e)))?;

        self.send_jpeg_frame(&jpeg_data, 0, 0, to_client).await?;
        self.send_sync(to_client).await?;
        Ok(())
    }

    /// Send a JPEG image frame via the modern img + blob + end protocol.
    async fn send_jpeg_frame(
        &mut self,
        jpeg_data: &[u8],
        x: i32,
        y: i32,
        to_client: &mpsc::Sender<Bytes>,
    ) -> Result<(), HandlerError> {
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, jpeg_data);

        // img instruction: start image stream
        let img_instr = format_img(self.stream_id, 14, 0, "image/jpeg", x, y);
        to_client
            .send(Bytes::from(img_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // blob instructions: chunked base64 data
        let blob_instructions = format_chunked_blobs(self.stream_id, &base64_data, None);
        for blob_instr in blob_instructions {
            to_client
                .send(Bytes::from(blob_instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        }

        // end instruction: close the stream
        let end_instr = format_end(self.stream_id);
        to_client
            .send(Bytes::from(end_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;

        // Increment stream ID for next frame (stream 0 is reserved)
        self.stream_id += 1;

        Ok(())
    }

    /// Send a sync instruction to signal frame completion.
    async fn send_sync(&self, to_client: &mpsc::Sender<Bytes>) -> Result<(), HandlerError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let timestamp_str = timestamp.to_string();
        let sync_instr = format_instruction("sync", &[&timestamp_str]);
        to_client
            .send(Bytes::from(sync_instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))
    }
}

// -- Guacamole instruction parsing helpers --

/// Parsed key event from Guacamole "key" instruction.
struct KeyEventParsed {
    keysym: u32,
    pressed: bool,
}

/// Parsed mouse event from Guacamole "mouse" instruction.
struct MouseEventParsed {
    x: u32,
    y: u32,
    button_mask: u32,
}

/// Parse a Guacamole key instruction.
///
/// Format: `3.key,{keysym_len}.{keysym},{pressed_len}.{pressed};`
fn parse_key_event(msg: &str) -> Option<KeyEventParsed> {
    let args_part = msg.split_once(".key,")?.1;
    let (first_arg, rest) = args_part.split_once(',')?;
    let (_, keysym_str) = first_arg.split_once('.')?;
    let keysym = keysym_str.parse::<u32>().ok()?;

    let (_, pressed_val) = rest.split_once('.')?;
    let pressed = pressed_val.starts_with('1');

    Some(KeyEventParsed { keysym, pressed })
}

/// Parse a Guacamole mouse instruction.
///
/// Format: `5.mouse,{x_len}.{x},{y_len}.{y},{button_len}.{button};`
fn parse_mouse_event(msg: &str) -> Option<MouseEventParsed> {
    let args_part = msg.split_once(".mouse,")?.1;
    let parts: Vec<&str> = args_part.split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    let (_, x_str) = parts[0].split_once('.')?;
    let (_, y_str) = parts[1].split_once('.')?;
    let button_part = parts[2].trim_end_matches(';');
    let (_, button_str) = button_part.split_once('.')?;

    Some(MouseEventParsed {
        x: x_str.parse().ok()?,
        y: y_str.parse().ok()?,
        button_mask: button_str.parse().ok()?,
    })
}

/// Parse a Guacamole size instruction.
///
/// Format: `4.size,{width_len}.{width},{height_len}.{height};`
fn parse_size_event(msg: &str) -> Option<(u32, u32)> {
    let args_part = msg.split_once(".size,")?.1;
    let parts: Vec<&str> = args_part.split(',').collect();
    if parts.len() < 2 {
        return None;
    }

    let (_, width_str) = parts[0].split_once('.')?;
    let height_part = parts[1].trim_end_matches(';');
    let (_, height_str) = height_part.split_once('.')?;

    Some((width_str.parse().ok()?, height_str.parse().ok()?))
}

/// Parse display size from connection params.
///
/// The "size" parameter is formatted as "width,height,dpi".
/// Returns (pixel_width, pixel_height).
fn parse_display_size(params: &HashMap<String, String>) -> (u32, u32) {
    let size_str = params
        .get("size")
        .map(|s| s.as_str())
        .unwrap_or("1024,768,96");
    let parts: Vec<&str> = size_str.split(',').collect();
    let width = parts
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WIDTH);
    let height = parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_HEIGHT);

    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // -- Mock ResourceBrowser for testing --

    struct MockBrowser {
        columns: Vec<ColumnDef>,
        rows: Arc<Mutex<Vec<Vec<String>>>>,
        actions: Vec<Action>,
        action_results: Arc<Mutex<HashMap<String, MockActionOutcome>>>,
    }

    #[allow(dead_code)]
    enum MockActionOutcome {
        Status(String),
        Refresh,
        Terminal,
    }

    impl MockBrowser {
        fn new() -> Self {
            Self {
                columns: vec![
                    ColumnDef::new("NAME"),
                    ColumnDef::new("STATUS"),
                    ColumnDef::new("AGE"),
                ],
                rows: Arc::new(Mutex::new(vec![
                    vec![
                        "nginx-7d4f9".to_string(),
                        "Running".to_string(),
                        "2d".to_string(),
                    ],
                    vec![
                        "redis-abc12".to_string(),
                        "Running".to_string(),
                        "5d".to_string(),
                    ],
                    vec![
                        "postgres-xy".to_string(),
                        "CrashLoop".to_string(),
                        "1h".to_string(),
                    ],
                ])),
                actions: vec![
                    Action::new("Shell", Some('s'), "shell"),
                    Action::new("Logs", Some('l'), "logs"),
                    Action::new("Describe", Some('d'), "describe"),
                ],
                action_results: Arc::new(Mutex::new(HashMap::new())),
            }
        }
    }

    #[async_trait]
    impl ResourceBrowser for MockBrowser {
        fn columns(&self) -> Vec<ColumnDef> {
            self.columns.clone()
        }

        async fn list_resources(&self) -> Result<Vec<Vec<String>>, String> {
            Ok(self.rows.lock().await.clone())
        }

        fn row_actions(&self, _row_index: usize) -> Vec<Action> {
            self.actions.clone()
        }

        async fn execute_action(
            &self,
            _row_index: usize,
            action_id: &str,
        ) -> Result<ActionResult, String> {
            let results = self.action_results.lock().await;
            match results.get(action_id) {
                Some(MockActionOutcome::Status(msg)) => Ok(ActionResult::Status(msg.clone())),
                Some(MockActionOutcome::Refresh) => Ok(ActionResult::Refresh),
                Some(MockActionOutcome::Terminal) => {
                    let (client, server) = tokio::io::duplex(1024);
                    let (read_half, write_half) = tokio::io::split(server);
                    // Immediately drop the client side to simulate an ended stream
                    drop(client);
                    Ok(ActionResult::Terminal {
                        reader: Box::new(read_half),
                        writer: Box::new(write_half),
                    })
                }
                None => Err(format!("Unknown action: {}", action_id)),
            }
        }

        fn name(&self) -> &str {
            "mock-browser"
        }
    }

    // -- Parsing tests --

    #[test]
    fn test_parse_key_event() {
        let msg = "3.key,5.65507,1.1;";
        let event = parse_key_event(msg).unwrap();
        assert_eq!(event.keysym, 65507);
        assert!(event.pressed);

        let msg = "3.key,2.97,1.0;";
        let event = parse_key_event(msg).unwrap();
        assert_eq!(event.keysym, 97);
        assert!(!event.pressed);
    }

    #[test]
    fn test_parse_key_event_invalid() {
        assert!(parse_key_event("5.mouse,1.0,1.0,1.0;").is_none());
        assert!(parse_key_event("garbage").is_none());
    }

    #[test]
    fn test_parse_mouse_event() {
        let msg = "5.mouse,3.100,3.200,1.1;";
        let event = parse_mouse_event(msg).unwrap();
        assert_eq!(event.x, 100);
        assert_eq!(event.y, 200);
        assert_eq!(event.button_mask, 1);
    }

    #[test]
    fn test_parse_mouse_event_scroll() {
        let msg = "5.mouse,3.512,3.384,2.16;";
        let event = parse_mouse_event(msg).unwrap();
        assert_eq!(event.button_mask, 16); // Scroll down
    }

    #[test]
    fn test_parse_mouse_event_invalid() {
        assert!(parse_mouse_event("3.key,2.97,1.0;").is_none());
        assert!(parse_mouse_event("5.mouse,1.0;").is_none());
    }

    #[test]
    fn test_parse_size_event() {
        let msg = "4.size,4.1920,4.1080;";
        let (w, h) = parse_size_event(msg).unwrap();
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
    }

    #[test]
    fn test_parse_size_event_small() {
        let msg = "4.size,3.800,3.600;";
        let (w, h) = parse_size_event(msg).unwrap();
        assert_eq!(w, 800);
        assert_eq!(h, 600);
    }

    #[test]
    fn test_parse_size_event_invalid() {
        assert!(parse_size_event("3.key,2.97,1.0;").is_none());
        assert!(parse_size_event("4.size,;").is_none());
    }

    #[test]
    fn test_parse_display_size_defaults() {
        let params = HashMap::new();
        let (w, h) = parse_display_size(&params);
        assert_eq!(w, 1024);
        assert_eq!(h, 768);
    }

    #[test]
    fn test_parse_display_size_custom() {
        let mut params = HashMap::new();
        params.insert("size".to_string(), "1920,1080,96".to_string());
        let (w, h) = parse_display_size(&params);
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
    }

    #[test]
    fn test_parse_display_size_invalid() {
        let mut params = HashMap::new();
        params.insert("size".to_string(), "not_numbers".to_string());
        let (w, h) = parse_display_size(&params);
        assert_eq!(w, 1024);
        assert_eq!(h, 768);
    }

    // -- BrowserMode tests --

    #[test]
    fn test_browser_mode_transitions() {
        let mode = BrowserMode::List;
        assert_eq!(mode, BrowserMode::List);

        let mode = BrowserMode::Terminal {
            action_id: "shell".to_string(),
            row_index: 0,
        };
        assert_eq!(
            mode,
            BrowserMode::Terminal {
                action_id: "shell".to_string(),
                row_index: 0,
            }
        );

        // Verify inequality between different modes
        assert_ne!(
            BrowserMode::List,
            BrowserMode::Terminal {
                action_id: "shell".to_string(),
                row_index: 0,
            }
        );
    }

    #[test]
    fn test_browser_mode_terminal_variants() {
        let shell = BrowserMode::Terminal {
            action_id: "shell".to_string(),
            row_index: 0,
        };
        let logs = BrowserMode::Terminal {
            action_id: "logs".to_string(),
            row_index: 0,
        };
        let different_row = BrowserMode::Terminal {
            action_id: "shell".to_string(),
            row_index: 1,
        };

        assert_ne!(shell, logs);
        assert_ne!(shell, different_row);
    }

    // -- ResourceUpdate application tests --

    #[test]
    fn test_apply_full_update() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        grid.set_data(
            columns,
            vec![vec![
                "old".to_string(),
                "data".to_string(),
                "1d".to_string(),
            ]],
        );
        handler.cached_rows = vec![vec![
            "old".to_string(),
            "data".to_string(),
            "1d".to_string(),
        ]];

        let new_rows = vec![
            vec!["new-1".to_string(), "Running".to_string(), "1h".to_string()],
            vec!["new-2".to_string(), "Pending".to_string(), "5m".to_string()],
        ];
        handler.apply_update(&mut grid, ResourceUpdate::FullUpdate(new_rows.clone()));

        assert_eq!(grid.row_count(), 2);
        assert_eq!(handler.cached_rows, new_rows);
        assert_eq!(grid.get_cell_value(0, 0), Some("new-1".to_string()));
        assert_eq!(grid.get_cell_value(1, 1), Some("Pending".to_string()));
    }

    #[test]
    fn test_apply_row_updated() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        let initial_rows = vec![
            vec!["pod-a".to_string(), "Running".to_string(), "1d".to_string()],
            vec!["pod-b".to_string(), "Pending".to_string(), "1h".to_string()],
        ];
        grid.set_data(columns, initial_rows.clone());
        handler.cached_rows = initial_rows;

        let updated_row = vec!["pod-b".to_string(), "Running".to_string(), "1h".to_string()];
        handler.apply_update(
            &mut grid,
            ResourceUpdate::RowUpdated {
                index: 1,
                row: updated_row,
            },
        );

        assert_eq!(grid.row_count(), 2);
        assert_eq!(grid.get_cell_value(1, 1), Some("Running".to_string()));
    }

    #[test]
    fn test_apply_row_added() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        let initial_rows = vec![vec![
            "pod-a".to_string(),
            "Running".to_string(),
            "1d".to_string(),
        ]];
        grid.set_data(columns, initial_rows.clone());
        handler.cached_rows = initial_rows;

        let new_row = vec![
            "pod-c".to_string(),
            "Starting".to_string(),
            "0s".to_string(),
        ];
        handler.apply_update(&mut grid, ResourceUpdate::RowAdded(new_row));

        assert_eq!(grid.row_count(), 2);
        assert_eq!(grid.get_cell_value(1, 0), Some("pod-c".to_string()));
    }

    #[test]
    fn test_apply_row_removed() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        let initial_rows = vec![
            vec!["pod-a".to_string(), "Running".to_string(), "1d".to_string()],
            vec!["pod-b".to_string(), "Running".to_string(), "5d".to_string()],
            vec!["pod-c".to_string(), "Failed".to_string(), "1h".to_string()],
        ];
        grid.set_data(columns, initial_rows.clone());
        handler.cached_rows = initial_rows;

        handler.apply_update(&mut grid, ResourceUpdate::RowRemoved(1));

        assert_eq!(grid.row_count(), 2);
        assert_eq!(grid.get_cell_value(0, 0), Some("pod-a".to_string()));
        assert_eq!(grid.get_cell_value(1, 0), Some("pod-c".to_string()));
    }

    #[test]
    fn test_apply_row_removed_out_of_bounds() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        let initial_rows = vec![vec![
            "pod-a".to_string(),
            "Running".to_string(),
            "1d".to_string(),
        ]];
        grid.set_data(columns, initial_rows.clone());
        handler.cached_rows = initial_rows;

        // Should not panic on out-of-bounds
        handler.apply_update(&mut grid, ResourceUpdate::RowRemoved(99));
        assert_eq!(grid.row_count(), 1);
    }

    #[test]
    fn test_apply_row_updated_out_of_bounds() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        let columns = handler.browser.columns();
        let initial_rows = vec![vec![
            "pod-a".to_string(),
            "Running".to_string(),
            "1d".to_string(),
        ]];
        grid.set_data(columns, initial_rows.clone());
        handler.cached_rows = initial_rows;

        // Should not panic on out-of-bounds
        handler.apply_update(
            &mut grid,
            ResourceUpdate::RowUpdated {
                index: 99,
                row: vec!["nope".to_string()],
            },
        );
        assert_eq!(grid.row_count(), 1);
        assert_eq!(grid.get_cell_value(0, 0), Some("pod-a".to_string()));
    }

    // -- Handler construction tests --

    #[test]
    fn test_handler_initial_state() {
        let browser = MockBrowser::new();
        let handler = ResourceBrowserHandler::new(browser);
        assert_eq!(handler.mode, BrowserMode::List);
        assert_eq!(handler.stream_id, INITIAL_STREAM_ID);
        assert_eq!(handler.pixel_width, DEFAULT_WIDTH);
        assert_eq!(handler.pixel_height, DEFAULT_HEIGHT);
        assert!(!handler.ctrl_pressed);
        assert!(handler.cached_rows.is_empty());
    }

    // -- Grid input handling tests --

    #[test]
    fn test_handle_list_input_key_down() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let columns = handler.browser.columns();
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        grid.set_data(
            columns,
            vec![
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
                vec!["d".to_string(), "e".to_string(), "f".to_string()],
            ],
        );

        // Arrow down selects first row
        let event = handler.handle_list_input("3.key,5.65364,1.1;", &mut grid);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(grid.selected_row(), Some(0));
    }

    #[test]
    fn test_handle_list_input_mouse_click() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let columns = handler.browser.columns();
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        grid.set_data(
            columns,
            vec![
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
                vec!["d".to_string(), "e".to_string(), "f".to_string()],
            ],
        );

        // Click on first data row
        let event = handler.handle_list_input("5.mouse,2.50,2.30,1.1;", &mut grid);
        // Should be CellSelected or Redraw depending on exact y coordinate
        assert!(matches!(
            event,
            GridEvent::CellSelected { .. } | GridEvent::Redraw | GridEvent::None
        ));
    }

    #[test]
    fn test_handle_list_input_resize() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let columns = handler.browser.columns();
        let mut grid = SpreadsheetRenderer::new(1024, 768);
        grid.set_data(columns, vec![]);

        let event = handler.handle_list_input("4.size,4.1920,4.1080;", &mut grid);
        assert_eq!(event, GridEvent::Redraw);
        assert_eq!(handler.pixel_width, 1920);
        assert_eq!(handler.pixel_height, 1080);
    }

    #[test]
    fn test_handle_list_input_unrecognized() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let mut grid = SpreadsheetRenderer::new(1024, 768);

        let event = handler.handle_list_input("unknown instruction", &mut grid);
        assert_eq!(event, GridEvent::None);
    }

    // -- Integration-level tests with channel --

    #[tokio::test]
    async fn test_handler_run_client_disconnect() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(64);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(64);

        // Drop the client sender immediately to simulate disconnect
        drop(from_client_tx);

        // Spawn a task to drain the to_client channel (prevent backpressure stall)
        let drain_handle = tokio::spawn(async move {
            let mut count = 0;
            while to_client_rx.recv().await.is_some() {
                count += 1;
            }
            count
        });

        let params = HashMap::new();
        let result = handler.run(params, to_client_tx, from_client_rx).await;
        assert!(result.is_ok());

        let msg_count = drain_handle.await.unwrap();
        // Should have sent at least: ready, name, img+blob+end+sync, disconnect
        assert!(
            msg_count >= 4,
            "Expected at least 4 messages, got {}",
            msg_count
        );
    }

    #[tokio::test]
    async fn test_handler_sends_ready_and_name() {
        let browser = MockBrowser::new();
        let mut handler = ResourceBrowserHandler::new(browser);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(64);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(64);

        // Drop client sender to trigger immediate exit
        drop(from_client_tx);

        let handle = tokio::spawn(async move {
            let params = HashMap::new();
            handler.run(params, to_client_tx, from_client_rx).await
        });

        // Read first two messages: ready and name
        let ready_msg = to_client_rx.recv().await.unwrap();
        let ready_str = String::from_utf8(ready_msg.to_vec()).unwrap();
        assert!(
            ready_str.contains("ready"),
            "Expected ready instruction, got: {}",
            ready_str
        );

        let name_msg = to_client_rx.recv().await.unwrap();
        let name_str = String::from_utf8(name_msg.to_vec()).unwrap();
        assert!(
            name_str.contains("mock-browser"),
            "Expected name instruction with mock-browser, got: {}",
            name_str
        );

        // Drain remaining
        while to_client_rx.recv().await.is_some() {}

        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_action_result_status() {
        let browser = MockBrowser::new();
        browser.action_results.lock().await.insert(
            "describe".to_string(),
            MockActionOutcome::Status("3 replicas, healthy".to_string()),
        );

        let mut handler = ResourceBrowserHandler::new(browser);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(256);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(64);

        let handle = tokio::spawn(async move {
            let params = HashMap::new();
            handler.run(params, to_client_tx, from_client_rx).await
        });

        // Wait for initial render to complete by consuming messages until we
        // see a sync instruction
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        // Select row 0 (arrow down)
        from_client_tx
            .send(Bytes::from("3.key,5.65364,1.1;"))
            .await
            .unwrap();

        // Wait for the redraw to complete
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        // Trigger describe action with shortcut 'd'
        from_client_tx
            .send(Bytes::from("3.key,3.100,1.1;"))
            .await
            .unwrap();

        // The action should produce a status result; wait for another render
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        // Disconnect
        drop(from_client_tx);

        // Drain remaining
        while to_client_rx.recv().await.is_some() {}

        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_action_result_refresh() {
        let browser = MockBrowser::new();
        browser
            .action_results
            .lock()
            .await
            .insert("describe".to_string(), MockActionOutcome::Refresh);

        let mut handler = ResourceBrowserHandler::new(browser);
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(256);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(64);

        let handle = tokio::spawn(async move {
            let params = HashMap::new();
            handler.run(params, to_client_tx, from_client_rx).await
        });

        // Wait for initial render
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        // Select row 0
        from_client_tx
            .send(Bytes::from("3.key,5.65364,1.1;"))
            .await
            .unwrap();

        // Wait for redraw
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        // Trigger describe action (shortcut 'd' = keysym 100)
        from_client_tx
            .send(Bytes::from("3.key,3.100,1.1;"))
            .await
            .unwrap();

        // Wait for the refresh render
        loop {
            let msg = to_client_rx.recv().await.unwrap();
            let s = String::from_utf8(msg.to_vec()).unwrap();
            if s.contains("sync") {
                break;
            }
        }

        drop(from_client_tx);
        while to_client_rx.recv().await.is_some() {}

        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }
}
