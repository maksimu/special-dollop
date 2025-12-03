// Database query executor with result rendering
// Unified executor for all database handlers

use crate::{DatabaseError, Result};
use bytes::Bytes;
use guacr_protocol::GuacamoleParser;
use guacr_terminal::{DatabaseTerminal, QueryResult};
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
}

impl QueryExecutor {
    /// Create a new query executor for a specific database type
    pub fn new(prompt: &str, db_type: &str) -> Result<Self> {
        Ok(Self {
            terminal: DatabaseTerminal::new(24, 80, prompt, db_type)?,
            input_buffer: String::new(),
            stream_id: 1,
            db_type: db_type.to_string(),
        })
    }

    /// Get the database type
    pub fn db_type(&self) -> &str {
        &self.db_type
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

        if instr.opcode != "key" {
            return Ok((false, vec![], None));
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
            0xFF0D => {
                // Enter key
                let query = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                self.terminal.write_line("")?;

                if !query.is_empty() {
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, Some(query)));
                } else {
                    self.terminal.write_prompt()?;
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, None));
                }
            }
            0xFF08 => {
                // Backspace
                if !self.input_buffer.is_empty() {
                    self.input_buffer.pop();
                    self.terminal.backspace()?;
                }
                let (_, instructions) = self.render_screen().await?;
                return Ok((true, instructions, None));
            }
            0xFF1B => {
                // Escape - clear input
                self.input_buffer.clear();
                self.terminal.clear_input();
                self.terminal.write_line("")?;
                self.terminal.write_prompt()?;
                let (_, instructions) = self.render_screen().await?;
                return Ok((true, instructions, None));
            }
            0xFF52 => {
                // Up arrow - TODO: command history
                return Ok((false, vec![], None));
            }
            0xFF54 => {
                // Down arrow - TODO: command history
                return Ok((false, vec![], None));
            }
            _ => {
                // Regular character
                if let Some(c) = char::from_u32(keysym) {
                    if c.is_ascii() && !c.is_control() {
                        self.input_buffer.push(c);
                        self.terminal.add_char(c)?;
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
        let instructions: Vec<Bytes> = img_instructions.into_iter().map(Bytes::from).collect();
        Ok((true, instructions))
    }

    /// Get current input buffer contents
    pub fn current_input(&self) -> &str {
        &self.input_buffer
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
}
