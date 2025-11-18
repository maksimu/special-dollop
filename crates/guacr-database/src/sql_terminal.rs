// SQL terminal emulation for database handlers

use crate::Result;
use guacr_terminal::{TerminalEmulator, TerminalRenderer};

/// SQL terminal for interactive database sessions
pub struct SqlTerminal {
    terminal: TerminalEmulator,
    renderer: TerminalRenderer,
    prompt: String,
    input_buffer: String,
}

impl SqlTerminal {
    pub fn new(rows: u16, cols: u16, prompt: &str) -> Result<Self> {
        Ok(Self {
            terminal: TerminalEmulator::new(rows, cols),
            renderer: TerminalRenderer::new()?,
            prompt: prompt.to_string(),
            input_buffer: String::new(),
        })
    }

    pub fn write_prompt(&mut self) -> Result<()> {
        let prompt_bytes = self.prompt.to_string();
        self.terminal.process(prompt_bytes.as_bytes())?;
        Ok(())
    }

    pub fn write_line(&mut self, text: &str) -> Result<()> {
        let line = format!("{}\r\n", text);
        self.terminal.process(line.as_bytes())?;
        Ok(())
    }

    pub fn write_error(&mut self, error: &str) -> Result<()> {
        self.write_line(&format!("ERROR: {}", error))
    }

    pub fn write_table(&mut self, columns: &[String], rows: &[Vec<String>]) -> Result<()> {
        if columns.is_empty() {
            return Ok(());
        }

        // Calculate column widths
        let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();

        for row in rows {
            for (i, val) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(val.len());
                }
            }
        }

        // Write header
        let header = columns
            .iter()
            .zip(&widths)
            .map(|(col, width)| format!("{:<width$}", col, width = width))
            .collect::<Vec<_>>()
            .join(" | ");

        self.write_line(&header)?;

        // Write separator
        let separator = widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-");

        self.write_line(&separator)?;

        // Write rows
        for row in rows {
            let row_str = row
                .iter()
                .zip(&widths)
                .map(|(val, width)| format!("{:<width$}", val, width = width))
                .collect::<Vec<_>>()
                .join(" | ");

            self.write_line(&row_str)?;
        }

        self.write_line(&format!("\n{} rows", rows.len()))?;

        Ok(())
    }

    pub fn add_char(&mut self, c: char) -> Result<()> {
        self.input_buffer.push(c);
        self.terminal.process(&[c as u8])?;
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<()> {
        if !self.input_buffer.is_empty() {
            self.input_buffer.pop();
            self.terminal.process(b"\x08 \x08")?; // Backspace, space, backspace
        }
        Ok(())
    }

    pub fn get_input(&mut self) -> String {
        let input = self.input_buffer.clone();
        self.input_buffer.clear();
        input
    }

    pub fn render_png(&mut self) -> Result<Vec<u8>> {
        let (rows, cols) = self.terminal.size();
        let png = self
            .renderer
            .render_screen(self.terminal.screen(), rows, cols)?;
        self.terminal.clear_dirty();
        Ok(png)
    }

    pub fn is_dirty(&self) -> bool {
        self.terminal.is_dirty()
    }

    #[allow(deprecated)]
    pub fn format_img_instructions(&self, png_data: &[u8], stream_id: u32) -> Vec<String> {
        self.renderer
            .format_img_instructions(png_data, stream_id, 0, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sql_terminal_new() {
        let term = SqlTerminal::new(24, 80, "mysql> ");
        assert!(term.is_ok());
    }

    #[test]
    fn test_write_line() {
        let mut term = SqlTerminal::new(24, 80, "mysql> ").unwrap();
        term.write_line("Hello, Database!").unwrap();
        assert!(term.is_dirty());
    }

    #[test]
    fn test_write_table() {
        let mut term = SqlTerminal::new(24, 80, "mysql> ").unwrap();

        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec!["1".to_string(), "Alice".to_string()],
            vec!["2".to_string(), "Bob".to_string()],
        ];

        term.write_table(&columns, &rows).unwrap();
        assert!(term.is_dirty());
    }

    #[test]
    fn test_input_buffer() {
        let mut term = SqlTerminal::new(24, 80, "mysql> ").unwrap();

        term.add_char('S').unwrap();
        term.add_char('H').unwrap();
        term.add_char('O').unwrap();
        term.add_char('W').unwrap();

        let input = term.get_input();
        assert_eq!(input, "SHOW");
    }

    #[test]
    fn test_backspace() {
        let mut term = SqlTerminal::new(24, 80, "mysql> ").unwrap();

        term.add_char('A').unwrap();
        term.add_char('B').unwrap();
        term.backspace().unwrap();

        let input = term.get_input();
        assert_eq!(input, "A");
    }
}
