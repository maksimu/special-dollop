// Pipe stream support for native terminal display
//
// Implements the Guacamole pipe stream mechanism that allows terminal output
// to be sent to native terminal applications instead of being rendered as images.
//
// Reference: KCM-505 patches to guacd (guacamole-server)
// - 5001-KCM-505-Add-terminal-flag-for-raw-mode-pipe-streams.patch
// - 5002-KCM-505-Open-STDOUT-pipe-stream-by-default.patch

use base64::Engine;
use bytes::Bytes;
use std::collections::HashMap;

// ============================================================================
// Pipe Stream Flags (matching guacd terminal.h)
// ============================================================================

/// Flag which specifies that pipe output should still be rendered to the
/// terminal display. If not set, output sent to the pipe will not be
/// displayed on screen.
pub const PIPE_INTERPRET_OUTPUT: u32 = 1;

/// Flag which specifies that the pipe stream should be flushed automatically
/// after each write. If not set, data may be buffered for efficiency.
pub const PIPE_AUTOFLUSH: u32 = 2;

/// Flag which specifies that all terminal output should be sent to the pipe,
/// including escape sequences (ANSI codes). By default, escape sequences are
/// filtered from output to the pipe, sending only visible text.
///
/// This flag is essential for native terminal display - without it, colors,
/// cursor movement, and other terminal features won't work.
pub const PIPE_RAW: u32 = 4;

// ============================================================================
// Well-Known Pipe Names
// ============================================================================

/// The standard pipe name for terminal output (server to client)
pub const PIPE_NAME_STDOUT: &str = "STDOUT";

/// The standard pipe name for terminal input (client to server)
pub const PIPE_NAME_STDIN: &str = "STDIN";

// ============================================================================
// Stream ID allocation
// ============================================================================

/// Base stream ID for pipe streams (to avoid collision with image streams)
/// Image streams typically use IDs 1-99, so we start at 100
pub const PIPE_STREAM_BASE: u32 = 100;

/// Stream ID for STDOUT pipe
pub const PIPE_STREAM_STDOUT: u32 = PIPE_STREAM_BASE;

/// Stream ID for STDIN pipe
pub const PIPE_STREAM_STDIN: u32 = PIPE_STREAM_BASE + 1;

// ============================================================================
// Pipe Stream State
// ============================================================================

/// State for a single pipe stream
#[derive(Debug, Clone)]
pub struct PipeStream {
    /// Stream ID used in Guacamole protocol
    pub stream_id: u32,

    /// Pipe name (e.g., "STDOUT", "STDIN")
    pub name: String,

    /// MIME type for the pipe data
    pub mimetype: String,

    /// Pipe flags (PIPE_INTERPRET_OUTPUT, PIPE_AUTOFLUSH, PIPE_RAW)
    pub flags: u32,

    /// Whether the pipe is currently open
    pub is_open: bool,

    /// Buffer for pending data (used when PIPE_AUTOFLUSH is not set)
    pub buffer: Vec<u8>,
}

impl PipeStream {
    /// Create a new pipe stream
    pub fn new(stream_id: u32, name: &str, mimetype: &str, flags: u32) -> Self {
        Self {
            stream_id,
            name: name.to_string(),
            mimetype: mimetype.to_string(),
            flags,
            is_open: false,
            buffer: Vec::new(),
        }
    }

    /// Create a STDOUT pipe with default settings for native terminal display
    pub fn stdout() -> Self {
        Self::new(
            PIPE_STREAM_STDOUT,
            PIPE_NAME_STDOUT,
            "application/octet-stream",
            PIPE_INTERPRET_OUTPUT | PIPE_AUTOFLUSH | PIPE_RAW,
        )
    }

    /// Create a STDIN pipe
    pub fn stdin() -> Self {
        Self::new(
            PIPE_STREAM_STDIN,
            PIPE_NAME_STDIN,
            "application/octet-stream",
            0,
        )
    }

    /// Check if raw mode is enabled (includes escape sequences)
    pub fn is_raw(&self) -> bool {
        self.flags & PIPE_RAW != 0
    }

    /// Check if auto-flush is enabled
    pub fn is_autoflush(&self) -> bool {
        self.flags & PIPE_AUTOFLUSH != 0
    }

    /// Check if output should still be interpreted/displayed
    pub fn interpret_output(&self) -> bool {
        self.flags & PIPE_INTERPRET_OUTPUT != 0
    }
}

// ============================================================================
// Pipe Stream Manager
// ============================================================================

/// Manages pipe streams for a terminal session
#[derive(Debug, Default)]
pub struct PipeStreamManager {
    /// Active pipe streams by name
    streams: HashMap<String, PipeStream>,

    /// Whether STDOUT pipe is enabled (send terminal output to client)
    stdout_enabled: bool,
}

impl PipeStreamManager {
    /// Create a new pipe stream manager
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            stdout_enabled: false,
        }
    }

    /// Enable STDOUT pipe (call during handler initialization)
    ///
    /// Returns the pipe instruction to send to the client
    pub fn enable_stdout(&mut self) -> String {
        let pipe = PipeStream::stdout();
        let instruction = format_pipe_instruction(&pipe);
        self.streams.insert(PIPE_NAME_STDOUT.to_string(), pipe);
        self.stdout_enabled = true;
        instruction
    }

    /// Check if STDOUT pipe is enabled
    pub fn is_stdout_enabled(&self) -> bool {
        self.stdout_enabled
    }

    /// Get the STDOUT pipe stream if enabled
    pub fn stdout(&self) -> Option<&PipeStream> {
        self.streams.get(PIPE_NAME_STDOUT)
    }

    /// Register an incoming pipe stream (from client, e.g., STDIN)
    pub fn register_incoming(&mut self, stream_id: u32, name: &str, mimetype: &str) {
        let mut pipe = PipeStream::new(stream_id, name, mimetype, 0);
        pipe.is_open = true;
        self.streams.insert(name.to_string(), pipe);
    }

    /// Get a pipe stream by name
    pub fn get(&self, name: &str) -> Option<&PipeStream> {
        self.streams.get(name)
    }

    /// Get a pipe stream by stream ID
    pub fn get_by_stream_id(&self, stream_id: u32) -> Option<&PipeStream> {
        self.streams.values().find(|p| p.stream_id == stream_id)
    }

    /// Check if a stream ID belongs to STDIN
    pub fn is_stdin_stream(&self, stream_id: u32) -> bool {
        self.streams
            .get(PIPE_NAME_STDIN)
            .map(|p| p.stream_id == stream_id)
            .unwrap_or(false)
    }

    /// Close a pipe stream
    pub fn close(&mut self, name: &str) -> Option<String> {
        if let Some(pipe) = self.streams.get_mut(name) {
            if pipe.is_open {
                pipe.is_open = false;
                return Some(format_end_instruction(pipe.stream_id));
            }
        }
        None
    }

    /// Close all pipe streams, returning end instructions
    pub fn close_all(&mut self) -> Vec<String> {
        let mut instructions = Vec::new();
        for pipe in self.streams.values_mut() {
            if pipe.is_open {
                pipe.is_open = false;
                instructions.push(format_end_instruction(pipe.stream_id));
            }
        }
        instructions
    }
}

// ============================================================================
// Instruction Formatting
// ============================================================================

/// Format a `pipe` instruction to open a named pipe stream
///
/// Format: `4.pipe,{stream_len}.{stream},{mimetype_len}.{mimetype},{name_len}.{name};`
pub fn format_pipe_instruction(pipe: &PipeStream) -> String {
    let stream_str = pipe.stream_id.to_string();
    format!(
        "4.pipe,{}.{},{}.{},{}.{};",
        stream_str.len(),
        stream_str,
        pipe.mimetype.len(),
        pipe.mimetype,
        pipe.name.len(),
        pipe.name
    )
}

/// Format a `blob` instruction to send data on a pipe stream
///
/// Data is base64-encoded before sending.
pub fn format_pipe_blob(stream_id: u32, data: &[u8]) -> String {
    let stream_str = stream_id.to_string();
    let base64_data = base64::engine::general_purpose::STANDARD.encode(data);
    format!(
        "4.blob,{}.{},{}.{};",
        stream_str.len(),
        stream_str,
        base64_data.len(),
        base64_data
    )
}

/// Format a `blob` instruction for raw (non-base64) text data
///
/// Use this when the data is already a valid Guacamole string (no encoding needed)
pub fn format_pipe_blob_raw(stream_id: u32, data: &str) -> String {
    let stream_str = stream_id.to_string();
    format!(
        "4.blob,{}.{},{}.{};",
        stream_str.len(),
        stream_str,
        data.len(),
        data
    )
}

/// Format an `end` instruction to close a pipe stream
pub fn format_end_instruction(stream_id: u32) -> String {
    let stream_str = stream_id.to_string();
    format!("3.end,{}.{};", stream_str.len(), stream_str)
}

/// Format an `ack` instruction to acknowledge pipe data
pub fn format_ack_instruction(stream_id: u32, message: &str, status: u32) -> String {
    let stream_str = stream_id.to_string();
    let status_str = status.to_string();
    format!(
        "3.ack,{}.{},{}.{},{}.{};",
        stream_str.len(),
        stream_str,
        message.len(),
        message,
        status_str.len(),
        status_str
    )
}

// ============================================================================
// Instruction Parsing
// ============================================================================

/// Parsed pipe instruction from client
#[derive(Debug, Clone)]
pub struct ParsedPipeInstruction {
    pub stream_id: u32,
    pub mimetype: String,
    pub name: String,
}

/// Parse a `pipe` instruction from the client
///
/// Format: `4.pipe,{stream_len}.{stream},{mimetype_len}.{mimetype},{name_len}.{name};`
pub fn parse_pipe_instruction(msg: &str) -> Option<ParsedPipeInstruction> {
    if !msg.contains(".pipe,") {
        return None;
    }

    let args_part = msg.split_once(".pipe,")?.1;
    let args_part = args_part.trim_end_matches(';');
    let parts: Vec<&str> = args_part.split(',').collect();

    if parts.len() < 3 {
        return None;
    }

    // Parse stream ID
    let stream_id = parts[0].split_once('.')?.1.parse().ok()?;

    // Parse mimetype
    let mimetype = parts[1].split_once('.')?.1.to_string();

    // Parse name
    let name = parts[2].split_once('.')?.1.to_string();

    Some(ParsedPipeInstruction {
        stream_id,
        mimetype,
        name,
    })
}

/// Parsed blob instruction from client
#[derive(Debug, Clone)]
pub struct ParsedBlobInstruction {
    pub stream_id: u32,
    pub data: Vec<u8>,
}

/// Parse a `blob` instruction from the client
///
/// Format: `4.blob,{stream_len}.{stream},{data_len}.{base64_data};`
pub fn parse_blob_instruction(msg: &str) -> Option<ParsedBlobInstruction> {
    if !msg.contains(".blob,") {
        return None;
    }

    let args_part = msg.split_once(".blob,")?.1;
    let args_part = args_part.trim_end_matches(';');
    let parts: Vec<&str> = args_part.split(',').collect();

    if parts.len() < 2 {
        return None;
    }

    // Parse stream ID
    let stream_id = parts[0].split_once('.')?.1.parse().ok()?;

    // Parse base64 data
    let base64_data = parts[1].split_once('.')?.1;
    let data = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()?;

    Some(ParsedBlobInstruction { stream_id, data })
}

/// Parse an `end` instruction from the client
pub fn parse_end_instruction(msg: &str) -> Option<u32> {
    if !msg.contains(".end,") {
        return None;
    }

    let args_part = msg.split_once(".end,")?.1;
    let args_part = args_part.trim_end_matches(';');

    // Parse stream ID
    args_part.split_once('.')?.1.parse().ok()
}

// ============================================================================
// Helper: Send pipe data as Bytes
// ============================================================================

/// Create a Bytes object containing a blob instruction for pipe data
pub fn pipe_blob_bytes(stream_id: u32, data: &[u8]) -> Bytes {
    Bytes::from(format_pipe_blob(stream_id, data))
}

/// Create a Bytes object containing a pipe open instruction
pub fn pipe_open_bytes(pipe: &PipeStream) -> Bytes {
    Bytes::from(format_pipe_instruction(pipe))
}

/// Create a Bytes object containing a pipe end instruction
pub fn pipe_end_bytes(stream_id: u32) -> Bytes {
    Bytes::from(format_end_instruction(stream_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_stream_flags() {
        let pipe = PipeStream::stdout();
        assert!(pipe.is_raw());
        assert!(pipe.is_autoflush());
        assert!(pipe.interpret_output());
    }

    #[test]
    fn test_format_pipe_instruction() {
        let pipe = PipeStream::stdout();
        let instr = format_pipe_instruction(&pipe);
        assert!(instr.starts_with("4.pipe,"));
        assert!(instr.contains("STDOUT"));
        assert!(instr.contains("application/octet-stream"));
    }

    #[test]
    fn test_format_pipe_blob() {
        let instr = format_pipe_blob(100, b"hello");
        assert!(instr.starts_with("4.blob,"));
        assert!(instr.contains("100"));
        // "hello" base64 = "aGVsbG8="
        assert!(instr.contains("aGVsbG8="));
    }

    #[test]
    fn test_format_end_instruction() {
        let instr = format_end_instruction(100);
        assert_eq!(instr, "3.end,3.100;");
    }

    #[test]
    fn test_parse_pipe_instruction() {
        let parsed = parse_pipe_instruction("4.pipe,3.100,24.application/octet-stream,6.STDOUT;");
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, 100);
        assert_eq!(parsed.mimetype, "application/octet-stream");
        assert_eq!(parsed.name, "STDOUT");
    }

    #[test]
    fn test_parse_blob_instruction() {
        // "hello" base64 = "aGVsbG8="
        let parsed = parse_blob_instruction("4.blob,3.100,8.aGVsbG8=;");
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.stream_id, 100);
        assert_eq!(parsed.data, b"hello");
    }

    #[test]
    fn test_parse_end_instruction() {
        let stream_id = parse_end_instruction("3.end,3.100;");
        assert_eq!(stream_id, Some(100));
    }

    #[test]
    fn test_pipe_stream_manager() {
        let mut manager = PipeStreamManager::new();

        // Enable STDOUT
        let instr = manager.enable_stdout();
        assert!(instr.contains("STDOUT"));
        assert!(manager.is_stdout_enabled());

        // Register STDIN
        manager.register_incoming(101, "STDIN", "application/octet-stream");
        assert!(manager.is_stdin_stream(101));
        assert!(!manager.is_stdin_stream(100));

        // Get by name
        assert!(manager.get("STDOUT").is_some());
        assert!(manager.get("STDIN").is_some());
        assert!(manager.get("UNKNOWN").is_none());
    }
}
