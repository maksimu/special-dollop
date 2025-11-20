// Database query executor with result rendering
// Executes SQL queries and formats results for display

use crate::sql_terminal::SqlTerminal;
use crate::{DatabaseError, Result};
use bytes::Bytes;
use guacr_protocol::GuacamoleParser;

/// Database query executor
///
/// Handles SQL query execution and result rendering for database handlers.
pub struct QueryExecutor {
    pub terminal: SqlTerminal, // Public so handlers can write results
    pub input_buffer: String,  // Public so handlers can check current input
    stream_id: u32,
}

impl QueryExecutor {
    /// Create a new query executor
    pub fn new(prompt: &str) -> Result<Self> {
        Ok(Self {
            terminal: SqlTerminal::new(24, 80, prompt)?,
            input_buffer: String::new(),
            stream_id: 1,
        })
    }

    /// Process keyboard input and execute queries
    ///
    /// Returns (needs_render, instructions, pending_query)
    /// pending_query is Some(query) when Enter was pressed
    pub async fn process_input(
        &mut self,
        instruction: &Bytes,
    ) -> Result<(bool, Vec<Bytes>, Option<String>)> {
        let instr = GuacamoleParser::parse_instruction(instruction)
            .map_err(|e| DatabaseError::QueryError(format!("Parse error: {}", e)))?;

        if instr.opcode != "key" {
            return Ok((false, vec![], None));
        }

        // Parse key event
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

        // Handle special keys
        match keysym {
            0xFF0D => {
                // Enter key - return query for execution
                let query = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                self.terminal.write_line("")?; // New line

                if !query.is_empty() {
                    // Write query to terminal
                    self.terminal.write_line(&format!("Executing: {}", query))?;
                    let (_, instructions) = self.render_screen().await?;
                    return Ok((true, instructions, Some(query)));
                } else {
                    // Empty query, just new prompt
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

    /// Render terminal screen and return Guacamole instructions
    pub async fn render_screen(&mut self) -> Result<(bool, Vec<Bytes>)> {
        let png = self.terminal.render_png()?;
        let img_instructions = self.terminal.format_img_instructions(&png, self.stream_id);

        let instructions: Vec<Bytes> = img_instructions.into_iter().map(Bytes::from).collect();

        Ok((true, instructions))
    }
}

/// Query result structure
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl QueryResult {
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn add_row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_result() {
        let mut result = QueryResult::new(vec!["id".to_string(), "name".to_string()]);
        result.add_row(vec!["1".to_string(), "test".to_string()]);

        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 1);
    }
}
