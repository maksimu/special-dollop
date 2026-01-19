// Database query executor with result rendering
// Unified executor for all database handlers

use crate::{DatabaseError, Result};
use bytes::Bytes;
use guacr_protocol::GuacamoleParser;
use guacr_terminal::{DatabaseTerminal, QueryResult, TerminalInputHandler, TerminalRenderer};
use std::collections::VecDeque;
use std::time::Instant;

// Re-export QueryResult for convenience
pub use guacr_terminal::QueryResult as QueryResultData;

/// Database query executor
///
/// Handles keyboard input processing, query buffering, and result rendering.
/// Used by all database handlers for consistent SQL CLI experience.
pub struct QueryExecutor {
    pub terminal: DatabaseTerminal,
    input_buffer: String,
    stream_id: u32,
    db_type: String,

    // Command history
    command_history: VecDeque<String>,
    history_index: Option<usize>,
    temp_buffer: String,
    history_max_size: usize,

    // Cursor position for line editing
    cursor_position: usize,

    // Multi-line continuation
    in_continuation: bool,
    continuation_prompt: String,

    // Current database context (for dynamic prompts)
    current_database: Option<String>,
    prompt_template: String,

    // Unified input handler for clipboard, mouse, resize
    input_handler: TerminalInputHandler,
}

impl QueryExecutor {
    /// Create a new query executor for a specific database type with default size
    pub fn new(prompt: &str, db_type: &str) -> Result<Self> {
        Self::new_with_size(prompt, db_type, 24, 80)
    }

    /// Create a new query executor with custom terminal dimensions
    pub fn new_with_size(prompt: &str, db_type: &str, rows: u16, cols: u16) -> Result<Self> {
        let continuation_prompt = match db_type {
            "mysql" | "mariadb" => "    -> ".to_string(),
            "postgresql" => "    -> ".to_string(),
            "mongodb" => "... ".to_string(),
            "redis" => "... ".to_string(),
            _ => "    -> ".to_string(),
        };

        Ok(Self {
            terminal: DatabaseTerminal::new(rows, cols, prompt, db_type)?,
            input_buffer: String::new(),
            stream_id: 1,
            db_type: db_type.to_string(),
            command_history: VecDeque::new(),
            history_index: None,
            temp_buffer: String::new(),
            history_max_size: 250,
            cursor_position: 0,
            in_continuation: false,
            continuation_prompt,
            current_database: None,
            prompt_template: prompt.to_string(),
            input_handler: TerminalInputHandler::new_with_scrollback(rows, cols, 1000),
        })
    }

    /// Get the database type
    pub fn db_type(&self) -> &str {
        &self.db_type
    }

    /// Generate Guacamole protocol display initialization instructions
    /// Returns (ready_instruction, size_instruction)
    pub fn create_display_init_instructions(width: u32, height: u32) -> (Bytes, Bytes) {
        // Create ready instruction with protocol name (matches SSH approach)
        let ready = Bytes::from(TerminalRenderer::format_ready_instruction("database"));

        // Create size instruction with layer 0 (matches SSH approach)
        // This tells the client the dimensions of layer 0 where we'll render
        let size = Bytes::from(TerminalRenderer::format_size_instruction(0, width, height));

        (ready, size)
    }

    /// Get terminal size (rows, cols)
    pub fn size(&self) -> (u16, u16) {
        self.terminal.size()
    }

    /// Handle mouse input for selection
    async fn handle_mouse_input(
        &mut self,
        instruction: &str,
    ) -> Result<(bool, Vec<Bytes>, Option<String>)> {
        use guacr_terminal::SelectionResult;

        if let Some(mouse_event) = self.input_handler.parse_mouse_instruction(instruction) {
            // Character dimensions for databases: 9x18 pixels
            const CHAR_WIDTH: u32 = 9;
            const CHAR_HEIGHT: u32 = 18;

            let result = self.input_handler.handle_mouse_event(
                mouse_event,
                self.terminal.emulator(),
                CHAR_WIDTH,
                CHAR_HEIGHT,
            );

            match result {
                SelectionResult::InProgress(overlay_instrs) => {
                    // Send selection overlay
                    let bytes_instrs = overlay_instrs.into_iter().map(Bytes::from).collect();
                    Ok((true, bytes_instrs, None))
                }
                SelectionResult::Complete {
                    text: _,
                    clear_instructions,
                } => {
                    // Send clipboard data to client
                    let mut clipboard_instrs = self
                        .input_handler
                        .get_clipboard_instructions(self.terminal.emulator(), self.stream_id);
                    // Add clear instructions
                    clipboard_instrs.extend(clear_instructions);

                    let bytes_instrs = clipboard_instrs.into_iter().map(Bytes::from).collect();
                    Ok((true, bytes_instrs, None))
                }
                SelectionResult::None => Ok((false, vec![], None)),
            }
        } else {
            Ok((false, vec![], None))
        }
    }

    /// Handle clipboard paste from client
    async fn handle_clipboard_input(
        &mut self,
        instruction: &str,
    ) -> Result<(bool, Vec<Bytes>, Option<String>)> {
        if let Some(clipboard_text) = self.input_handler.parse_clipboard_instruction(instruction) {
            // Insert clipboard text at cursor position
            self.input_buffer
                .insert_str(self.cursor_position, &clipboard_text);
            self.cursor_position += clipboard_text.len();
            self.redraw_input_line()?;

            let (_, instructions) = self.render_screen().await?;
            Ok((true, instructions, None))
        } else {
            Ok((false, vec![], None))
        }
    }

    /// Handle terminal resize
    async fn handle_resize_input(
        &mut self,
        args: &[&str],
    ) -> Result<(bool, Vec<Bytes>, Option<String>)> {
        if args.len() >= 2 {
            let width: u32 = args[0].parse().unwrap_or(1024);
            let height: u32 = args[1].parse().unwrap_or(768);

            // Calculate new terminal dimensions (9x18 pixels per character)
            let cols = (width / 9).max(80) as u16;
            let rows = (height / 18).max(24) as u16;

            // Resize terminal via input handler
            self.input_handler
                .handle_resize(rows, cols, self.terminal.emulator_mut())
                .map_err(|e| DatabaseError::QueryError(format!("Resize error: {}", e)))?;

            // Re-render at new size
            let (_, instructions) = self.render_screen().await?;
            Ok((true, instructions, None))
        } else {
            Ok((false, vec![], None))
        }
    }

    /// Process keyboard input and return query if Enter was pressed
    ///
    /// Returns (needs_render, instructions, pending_query)
    pub async fn process_input(
        &mut self,
        instruction: &Bytes,
    ) -> Result<(bool, Vec<Bytes>, Option<String>)> {
        let instr = GuacamoleParser::parse_instruction(instruction)
            .map_err(|e| DatabaseError::QueryError(format!("Parse error: {}", e)))?;

        // Route to appropriate handler based on opcode
        match instr.opcode {
            "mouse" => {
                return self
                    .handle_mouse_input(&String::from_utf8_lossy(instruction))
                    .await
            }
            "clipboard" => {
                return self
                    .handle_clipboard_input(&String::from_utf8_lossy(instruction))
                    .await
            }
            "size" => return self.handle_resize_input(&instr.args).await,
            "key" => {
                // Continue with key handling below
            }
            _ => return Ok((false, vec![], None)),
        }

        if instr.args.len() < 2 {
            return Ok((false, vec![], None));
        }

        let keysym: u32 = instr.args[0]
            .parse()
            .map_err(|_| DatabaseError::QueryError("Invalid keysym".to_string()))?;
        let pressed = instr.args[1] == "1";

        if !pressed {
            return Ok((false, vec![], None));
        }

        match keysym {
            // Enter key
            0xFF0D => {
                let query = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.terminal.write_line("")?;

                if !query.is_empty() {
                    // Add to history
                    self.add_to_history(&query);

                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, Some(query)));
                } else {
                    if self.in_continuation {
                        self.terminal.process(self.continuation_prompt.as_bytes())?;
                    } else {
                        self.terminal.write_prompt()?;
                    }
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Backspace
            0xFF08 => {
                if self.delete_char_before_cursor().is_ok() {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Escape - clear input
            0xFF1B => {
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.terminal.clear_input();
                self.terminal.write_line("")?;
                self.terminal.write_prompt()?;
                let (_, instructions) = self.render_screen().await?;
                return Ok((true, instructions, None));
            }

            // Up arrow - previous command in history
            0xFF52 => {
                if self.history_previous()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Down arrow - next command in history
            0xFF54 => {
                if self.history_next()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Left arrow - move cursor left
            0xFF51 => {
                if self.move_cursor_left()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Right arrow - move cursor right
            0xFF53 => {
                if self.move_cursor_right()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Home key - beginning of line
            0xFF50 => {
                if self.move_cursor_home()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // End key - end of line
            0xFF57 => {
                if self.move_cursor_end()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Delete key
            0xFFFF => {
                if self.delete_char_at_cursor().is_ok() {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+A - beginning of line
            0x0001 => {
                if self.move_cursor_home()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+E - end of line
            0x0005 => {
                if self.move_cursor_end()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+K - kill to end of line
            0x000B => {
                if self.kill_to_end()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+U - kill entire line
            0x0015 => {
                if self.kill_line()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+W - kill previous word
            0x0017 => {
                if self.kill_word()? {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }

            // Ctrl+C - cancel current input (already handled by Escape above, but add for completeness)
            0x0003 => {
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.terminal.write_line("^C")?;
                self.terminal.write_prompt()?;
                let (_, instructions) = self.render_screen().await?;
                return Ok((true, instructions, None));
            }

            // Regular character
            _ => {
                if let Some(c) = char::from_u32(keysym) {
                    if c.is_ascii() && !c.is_control() {
                        self.insert_char(c)?;
                        let (_, instructions) = self.render_screen().await?;
                        return Ok((true, instructions, None));
                    }
                }
            }
        }

        Ok((false, vec![], None))
    }

    /// Write query result to terminal
    pub fn write_result(&mut self, result: &QueryResult) -> Result<()> {
        self.terminal.write_result(result)?;
        self.terminal.write_prompt()?;
        Ok(())
    }

    /// Write error message to terminal
    pub fn write_error(&mut self, error: &str) -> Result<()> {
        self.terminal.write_error(error)?;
        self.terminal.write_prompt()?;
        Ok(())
    }

    /// Render terminal screen and return Guacamole instructions
    pub async fn render_screen(&mut self) -> Result<(bool, Vec<Bytes>)> {
        let jpeg = self.terminal.render_jpeg()?;
        let img_instructions = self.terminal.format_img_instructions(&jpeg, self.stream_id);

        // Convert image instructions to Bytes
        let mut instructions: Vec<Bytes> = img_instructions.into_iter().map(Bytes::from).collect();

        // Add sync instruction (CRITICAL: tells client to display the buffered instructions)
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let sync_instr = self.terminal.format_sync_instruction(timestamp_ms);
        instructions.push(Bytes::from(sync_instr));

        Ok((true, instructions))
    }

    /// Get current input buffer contents
    pub fn current_input(&self) -> &str {
        &self.input_buffer
    }

    /// Set the current database context (for dynamic prompts)
    pub fn set_current_database(&mut self, database: Option<String>) {
        self.current_database = database;
        self.update_prompt();
    }

    /// Update the prompt based on current context
    fn update_prompt(&mut self) {
        let prompt = if let Some(ref db) = self.current_database {
            match self.db_type.as_str() {
                "mysql" | "mariadb" => format!("mysql [{}]> ", db),
                "postgresql" => format!("{}=# ", db),
                _ => self.prompt_template.clone(),
            }
        } else {
            self.prompt_template.clone()
        };

        self.terminal.set_prompt(&prompt);
    }

    /// Set continuation mode
    pub fn set_continuation(&mut self, in_continuation: bool) {
        self.in_continuation = in_continuation;
    }

    /// Navigate to previous command (Up arrow)
    pub fn history_previous(&mut self) -> Result<bool> {
        if self.command_history.is_empty() {
            return Ok(false);
        }

        // Save current input if starting history navigation
        if self.history_index.is_none() {
            self.temp_buffer = self.input_buffer.clone();
            self.history_index = Some(self.command_history.len() - 1);
        } else if let Some(idx) = self.history_index {
            if idx > 0 {
                self.history_index = Some(idx - 1);
            } else {
                return Ok(false); // Already at oldest
            }
        }

        // Load history entry into input buffer
        if let Some(idx) = self.history_index {
            self.input_buffer = self.command_history[idx].clone();
            self.cursor_position = self.input_buffer.len();
            self.redraw_input_line()?;
        }

        Ok(true)
    }

    /// Navigate to next command (Down arrow)
    pub fn history_next(&mut self) -> Result<bool> {
        if let Some(idx) = self.history_index {
            if idx < self.command_history.len() - 1 {
                self.history_index = Some(idx + 1);
                self.input_buffer = self.command_history[idx + 1].clone();
                self.cursor_position = self.input_buffer.len();
            } else {
                // Restore temp buffer (what user was typing)
                self.history_index = None;
                self.input_buffer = self.temp_buffer.clone();
                self.cursor_position = self.input_buffer.len();
                self.temp_buffer.clear();
            }
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Add command to history
    pub fn add_to_history(&mut self, command: &str) {
        // Don't add empty or whitespace-only commands
        if command.trim().is_empty() {
            return;
        }

        // Don't add if same as last command
        if let Some(last) = self.command_history.back() {
            if last == command {
                return;
            }
        }

        self.command_history.push_back(command.to_string());

        // Maintain max size
        if self.command_history.len() > self.history_max_size {
            self.command_history.pop_front();
        }

        // Reset history navigation
        self.history_index = None;
        self.temp_buffer.clear();
    }

    /// Redraw the current input line
    fn redraw_input_line(&mut self) -> Result<()> {
        // Move to beginning of line and clear
        self.terminal.write_line("\r")?;

        // Write prompt
        if self.in_continuation {
            self.terminal.process(self.continuation_prompt.as_bytes())?;
        } else {
            self.terminal.write_prompt()?;
        }

        // Write input buffer
        for ch in self.input_buffer.chars() {
            self.terminal.add_char(ch)?;
        }

        // Move cursor to correct position if not at end
        if self.cursor_position < self.input_buffer.len() {
            let moves_back = self.input_buffer.len() - self.cursor_position;
            for _ in 0..moves_back {
                self.terminal.process(b"\x08")?; // Move cursor left
            }
        }

        Ok(())
    }

    /// Insert character at cursor position
    fn insert_char(&mut self, c: char) -> Result<()> {
        self.input_buffer.insert(self.cursor_position, c);
        self.cursor_position += 1;
        self.redraw_input_line()?;
        Ok(())
    }

    /// Delete character before cursor (backspace)
    fn delete_char_before_cursor(&mut self) -> Result<()> {
        if self.cursor_position > 0 {
            self.input_buffer.remove(self.cursor_position - 1);
            self.cursor_position -= 1;
            self.redraw_input_line()?;
        }
        Ok(())
    }

    /// Delete character at cursor (delete key)
    fn delete_char_at_cursor(&mut self) -> Result<()> {
        if self.cursor_position < self.input_buffer.len() {
            self.input_buffer.remove(self.cursor_position);
            self.redraw_input_line()?;
        }
        Ok(())
    }

    /// Move cursor left
    fn move_cursor_left(&mut self) -> Result<bool> {
        if self.cursor_position > 0 {
            self.cursor_position -= 1;
            self.terminal.process(b"\x08")?; // Move cursor left
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Move cursor right
    fn move_cursor_right(&mut self) -> Result<bool> {
        if self.cursor_position < self.input_buffer.len() {
            self.cursor_position += 1;
            self.terminal.process(b"\x1B[C")?; // Move cursor right (ANSI escape)
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Move cursor to beginning of line (Home / Ctrl+A)
    fn move_cursor_home(&mut self) -> Result<bool> {
        if self.cursor_position > 0 {
            self.cursor_position = 0;
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Move cursor to end of line (End / Ctrl+E)
    fn move_cursor_end(&mut self) -> Result<bool> {
        if self.cursor_position < self.input_buffer.len() {
            self.cursor_position = self.input_buffer.len();
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Kill to end of line (Ctrl+K)
    fn kill_to_end(&mut self) -> Result<bool> {
        if self.cursor_position < self.input_buffer.len() {
            self.input_buffer.truncate(self.cursor_position);
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Kill entire line (Ctrl+U)
    fn kill_line(&mut self) -> Result<bool> {
        if !self.input_buffer.is_empty() {
            self.input_buffer.clear();
            self.cursor_position = 0;
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Kill previous word (Ctrl+W)
    fn kill_word(&mut self) -> Result<bool> {
        if self.cursor_position > 0 {
            let chars: Vec<char> = self.input_buffer.chars().collect();
            let mut pos = self.cursor_position - 1;

            // Skip trailing whitespace
            while pos > 0 && chars[pos].is_whitespace() {
                pos -= 1;
            }

            // Find start of word
            while pos > 0 && !chars[pos - 1].is_whitespace() {
                pos -= 1;
            }

            self.input_buffer.drain(pos..self.cursor_position);
            self.cursor_position = pos;
            self.redraw_input_line()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Query execution result with timing
pub struct ExecutionResult {
    pub result: QueryResult,
    pub execution_time: std::time::Duration,
}

impl ExecutionResult {
    pub fn new(result: QueryResult, execution_time: std::time::Duration) -> Self {
        Self {
            result,
            execution_time,
        }
    }

    pub fn into_query_result(self) -> QueryResult {
        let mut result = self.result;
        result.execution_time_ms = Some(self.execution_time.as_millis() as u64);
        result
    }
}

/// Helper trait for database query execution
#[async_trait::async_trait]
#[allow(dead_code)]
pub trait DatabaseQueryExecutor: Send + Sync {
    /// Execute a query and return results
    async fn execute(&self, query: &str) -> std::result::Result<QueryResult, String>;

    /// Test connection
    async fn test_connection(&self) -> std::result::Result<(), String>;
}

/// Measure query execution time
pub async fn execute_with_timing<F, Fut>(
    execute_fn: F,
) -> std::result::Result<ExecutionResult, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<QueryResult, String>>,
{
    let start = Instant::now();
    let result = execute_fn().await?;
    let duration = start.elapsed();
    Ok(ExecutionResult::new(result, duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_executor_new() {
        let executor = QueryExecutor::new("mysql> ", "mysql");
        assert!(executor.is_ok());
        assert_eq!(executor.unwrap().db_type(), "mysql");
    }

    #[test]
    fn test_execution_result() {
        let result = QueryResult::new(vec!["id".to_string()]);
        let exec_result = ExecutionResult::new(result, std::time::Duration::from_millis(100));
        let query_result = exec_result.into_query_result();
        assert_eq!(query_result.execution_time_ms, Some(100));
    }

    #[test]
    fn test_command_history() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        // Add commands to history
        executor.add_to_history("SELECT 1");
        executor.add_to_history("SELECT 2");
        executor.add_to_history("SELECT 3");

        assert_eq!(executor.command_history.len(), 3);

        // Navigate up
        assert!(executor.history_previous().unwrap());
        assert_eq!(executor.current_input(), "SELECT 3");

        assert!(executor.history_previous().unwrap());
        assert_eq!(executor.current_input(), "SELECT 2");

        assert!(executor.history_previous().unwrap());
        assert_eq!(executor.current_input(), "SELECT 1");

        // Can't go further back
        assert!(!executor.history_previous().unwrap());

        // Navigate down
        assert!(executor.history_next().unwrap());
        assert_eq!(executor.current_input(), "SELECT 2");

        assert!(executor.history_next().unwrap());
        assert_eq!(executor.current_input(), "SELECT 3");
    }

    #[test]
    fn test_history_deduplication() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        executor.add_to_history("SELECT 1");
        executor.add_to_history("SELECT 1"); // Duplicate
        executor.add_to_history("SELECT 2");

        // Duplicate should not be added
        assert_eq!(executor.command_history.len(), 2);
    }

    #[test]
    fn test_history_max_size() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();
        executor.history_max_size = 3;

        executor.add_to_history("SELECT 1");
        executor.add_to_history("SELECT 2");
        executor.add_to_history("SELECT 3");
        executor.add_to_history("SELECT 4");

        // Should only keep last 3
        assert_eq!(executor.command_history.len(), 3);
        assert_eq!(executor.command_history[0], "SELECT 2");
        assert_eq!(executor.command_history[2], "SELECT 4");
    }

    #[test]
    fn test_cursor_movement() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        executor.input_buffer = "SELECT * FROM users".to_string();
        executor.cursor_position = 19;

        // Move left
        assert!(executor.move_cursor_left().unwrap());
        assert_eq!(executor.cursor_position, 18);

        // Move right
        assert!(executor.move_cursor_right().unwrap());
        assert_eq!(executor.cursor_position, 19);

        // Can't move right past end
        assert!(!executor.move_cursor_right().unwrap());

        // Move to beginning
        assert!(executor.move_cursor_home().unwrap());
        assert_eq!(executor.cursor_position, 0);

        // Can't move left past beginning
        assert!(!executor.move_cursor_left().unwrap());

        // Move to end
        assert!(executor.move_cursor_end().unwrap());
        assert_eq!(executor.cursor_position, 19);
    }

    #[test]
    fn test_insert_char() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        executor.input_buffer = "SELECT FROM users".to_string();
        executor.cursor_position = 7; // After "SELECT "

        executor.insert_char('*').unwrap();
        executor.insert_char(' ').unwrap();

        assert_eq!(executor.input_buffer, "SELECT * FROM users");
        assert_eq!(executor.cursor_position, 9);
    }

    #[test]
    fn test_delete_operations() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        executor.input_buffer = "SELECT * FROM users".to_string();
        executor.cursor_position = 9; // After "SELECT * "

        // Backspace
        executor.delete_char_before_cursor().unwrap();
        assert_eq!(executor.input_buffer, "SELECT *FROM users");
        assert_eq!(executor.cursor_position, 8);

        // Delete at cursor
        executor.delete_char_at_cursor().unwrap();
        assert_eq!(executor.input_buffer, "SELECT *ROM users");
        assert_eq!(executor.cursor_position, 8);
    }

    #[test]
    fn test_kill_operations() {
        let mut executor = QueryExecutor::new("test> ", "test").unwrap();

        // Kill to end
        executor.input_buffer = "SELECT * FROM users".to_string();
        executor.cursor_position = 9;
        executor.kill_to_end().unwrap();
        assert_eq!(executor.input_buffer, "SELECT * ");

        // Kill entire line
        executor.input_buffer = "SELECT * FROM users".to_string();
        executor.cursor_position = 10;
        executor.kill_line().unwrap();
        assert_eq!(executor.input_buffer, "");
        assert_eq!(executor.cursor_position, 0);

        // Kill word
        executor.input_buffer = "SELECT * FROM users".to_string();
        executor.cursor_position = 13; // After "FROM"
        executor.kill_word().unwrap();
        assert_eq!(executor.input_buffer, "SELECT *  users");
    }

    #[test]
    fn test_set_current_database() {
        let mut executor = QueryExecutor::new("mysql> ", "mysql").unwrap();

        executor.set_current_database(Some("testdb".to_string()));
        assert_eq!(executor.current_database, Some("testdb".to_string()));

        // Prompt should be updated
        assert!(executor.terminal.get_prompt().contains("testdb"));
    }

    #[test]
    fn test_continuation_mode() {
        let mut executor = QueryExecutor::new("mysql> ", "mysql").unwrap();

        assert!(!executor.in_continuation);

        executor.set_continuation(true);
        assert!(executor.in_continuation);

        executor.set_continuation(false);
        assert!(!executor.in_continuation);
    }
}
