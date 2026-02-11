// Shared helper functions for database handler boilerplate.
//
// These functions extract the identical patterns that appear across all
// database handlers (Redis, Cassandra, DynamoDB, Elasticsearch, ODBC):
//   - Display size parsing
//   - Connection error/success rendering
//   - Help text rendering
//   - Quit handling
//   - Render-and-send shorthand

use bytes::Bytes;
use guacr_handlers::HandlerError;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::query_executor::QueryExecutor;

/// Parse display size from connection params and calculate terminal dimensions.
///
/// The "size" parameter is formatted as "width,height,dpi" (e.g. "1024,768,96").
/// Returns (pixel_width, pixel_height, cols, rows) where cols/rows are calculated
/// from a 9x18 pixel character cell size.
pub fn parse_display_size(params: &HashMap<String, String>) -> (u32, u32, u16, u16) {
    let size_params = params
        .get("size")
        .map(|s| s.as_str())
        .unwrap_or("1024,768,96");
    let size_parts: Vec<&str> = size_params.split(',').collect();
    let width: u32 = size_parts
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    let height: u32 = size_parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(768);

    // Calculate terminal dimensions (9x18 pixels per character cell)
    let cols = (width / 9).max(80) as u16;
    let rows = (height / 18).max(24) as u16;

    (width, height, cols, rows)
}

/// Render connection error with troubleshooting tips, then drain the client channel.
///
/// This is the standard pattern used when a database connection fails:
/// 1. Display the error message
/// 2. List numbered troubleshooting tips
/// 3. Render the screen and send to client
/// 4. Drain remaining client messages (so the handler can exit cleanly)
pub async fn render_connection_error(
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    from_client: &mut mpsc::Receiver<Bytes>,
    error_msg: &str,
    tips: &[&str],
) -> Result<(), HandlerError> {
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_error(error_msg)
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("Troubleshooting:")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    for (i, tip) in tips.iter().enumerate() {
        executor
            .terminal
            .write_line(&format!("  {}. {}", i + 1, tip))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    }
    executor
        .terminal
        .write_prompt()
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    send_render(executor, to_client).await?;

    // Drain remaining client messages so the handler can exit cleanly
    while from_client.recv().await.is_some() {}

    Ok(())
}

/// Render connection success banner and initial screen.
///
/// Displays a list of informational lines (e.g. "Connected to X at host:port",
/// "Database: foo", "Type 'help' for available commands."), followed by a prompt,
/// then renders and sends to client.
pub async fn render_connection_success(
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    info_lines: &[&str],
) -> Result<(), HandlerError> {
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    for line in info_lines {
        executor
            .terminal
            .write_line(line)
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    }
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_prompt()
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    send_render(executor, to_client).await?;

    Ok(())
}

/// A section in the help text, consisting of a title and a list of (command, description) pairs.
pub struct HelpSection {
    pub title: &'static str,
    pub commands: Vec<(&'static str, &'static str)>,
}

/// Render help text from structured sections.
///
/// Displays a header line ("HandlerName - Available commands:"), then each
/// section with its commands formatted as "  command_padded  description",
/// followed by "Type 'quit' to disconnect".
pub async fn render_help(
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
    handler_name: &str,
    sections: &[HelpSection],
) -> Result<(), HandlerError> {
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line(&format!("{} - Available commands:", handler_name))
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    for section in sections {
        executor
            .terminal
            .write_line(&format!("{}:", section.title))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        for (cmd, desc) in &section.commands {
            executor
                .terminal
                .write_line(&format!("  {:<20} {}", cmd, desc))
                .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        }
        executor
            .terminal
            .write_line("")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    }

    executor
        .terminal
        .write_line("Type 'quit' to disconnect")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_line("")
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    executor
        .terminal
        .write_prompt()
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

    send_render(executor, to_client).await?;

    Ok(())
}

/// Handle quit/exit command -- render "Bye" and return a Disconnected error.
///
/// The caller should propagate the returned error to terminate the handler.
pub async fn handle_quit(
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
) -> HandlerError {
    let _ = executor.terminal.write_line("Bye");
    if let Ok((_, instructions)) = executor.render_screen().await {
        for instr in instructions {
            let _ = to_client.send(instr).await;
        }
    }
    HandlerError::Disconnected("User requested disconnect".to_string())
}

/// Render the screen and send all resulting instructions to the client.
///
/// This is a shorthand for the pattern that appears dozens of times across
/// all database handlers:
/// ```ignore
/// let (_, instructions) = executor.render_screen().await
///     .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
/// for instr in instructions {
///     to_client.send(instr).await
///         .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
/// }
/// ```
pub async fn send_render(
    executor: &mut QueryExecutor,
    to_client: &mpsc::Sender<Bytes>,
) -> Result<(), HandlerError> {
    let (_, instructions) = executor
        .render_screen()
        .await
        .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
    for instr in instructions {
        to_client
            .send(instr)
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_display_size_default() {
        let params = HashMap::new();
        let (w, h, cols, rows) = parse_display_size(&params);
        assert_eq!(w, 1024);
        assert_eq!(h, 768);
        assert_eq!(cols, (1024 / 9) as u16);
        assert_eq!(rows, (768 / 18) as u16);
    }

    #[test]
    fn test_parse_display_size_custom() {
        let mut params = HashMap::new();
        params.insert("size".to_string(), "1920,1080,96".to_string());
        let (w, h, cols, rows) = parse_display_size(&params);
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);
        assert_eq!(cols, (1920 / 9) as u16);
        assert_eq!(rows, (1080 / 18) as u16);
    }

    #[test]
    fn test_parse_display_size_small() {
        let mut params = HashMap::new();
        // Very small size should be clamped to minimums
        params.insert("size".to_string(), "100,100,96".to_string());
        let (w, h, cols, rows) = parse_display_size(&params);
        assert_eq!(w, 100);
        assert_eq!(h, 100);
        assert_eq!(cols, 80); // (100/9)=11, clamped to 80
        assert_eq!(rows, 24); // (100/18)=5, clamped to 24
    }

    #[test]
    fn test_parse_display_size_invalid() {
        let mut params = HashMap::new();
        params.insert("size".to_string(), "not_a_number".to_string());
        let (w, h, cols, rows) = parse_display_size(&params);
        // Falls back to defaults
        assert_eq!(w, 1024);
        assert_eq!(h, 768);
        assert_eq!(cols, (1024 / 9) as u16);
        assert_eq!(rows, (768 / 18) as u16);
    }
}
