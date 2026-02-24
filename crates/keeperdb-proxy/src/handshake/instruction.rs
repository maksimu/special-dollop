//! Gateway protocol instruction parser and encoder.
//!
//! Instructions use a length-prefixed format:
//! ```text
//! <length>.<element>,<length>.<element>,...;
//! ```
//!
//! Where the first element is the opcode and subsequent elements are arguments.
//!
//! # Examples
//!
//! ```text
//! 6.select,5.mysql;           -> select("mysql")
//! 4.args,4.host,4.port;       -> args("host", "port")
//! 5.ready,36.abc-123-uuid;    -> ready("abc-123-uuid")
//! ```

use std::io::{self, Write};

use crate::error::{ProxyError, Result};

/// Instruction terminator
const INST_TERM: u8 = b';';
/// Argument separator
const ARG_SEP: u8 = b',';
/// Length/element separator
const ELEM_SEP: u8 = b'.';

/// A Gateway protocol instruction with opcode and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// The instruction opcode (e.g., "select", "args", "connect", "ready")
    pub opcode: String,
    /// The instruction arguments
    pub args: Vec<String>,
}

impl Instruction {
    /// Create a new instruction with the given opcode and arguments.
    pub fn new(opcode: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            opcode: opcode.into(),
            args,
        }
    }

    /// Create a `select` instruction for protocol selection.
    ///
    /// # Arguments
    ///
    /// * `protocol` - Database type: "mysql" or "postgresql"
    pub fn select(protocol: &str) -> Self {
        Self::new("select", vec![protocol.to_string()])
    }

    /// Create an `args` instruction listing required parameters.
    ///
    /// # Arguments
    ///
    /// * `params` - List of parameter names the client must provide
    pub fn args(params: &[&str]) -> Self {
        Self::new("args", params.iter().map(|s| s.to_string()).collect())
    }

    /// Create a `connect` instruction with parameter values.
    ///
    /// # Arguments
    ///
    /// * `values` - Parameter values in the order specified by `args`
    pub fn connect(values: Vec<String>) -> Self {
        Self::new("connect", values)
    }

    /// Create a `ready` instruction indicating successful handshake.
    ///
    /// # Arguments
    ///
    /// * `session_id` - Unique session identifier for this connection
    pub fn ready(session_id: &str) -> Self {
        Self::new("ready", vec![session_id.to_string()])
    }

    /// Create an `error` instruction for handshake failures.
    ///
    /// # Arguments
    ///
    /// * `code` - Error code (e.g., "PROTOCOL", "AUTH", "CONFIG")
    /// * `message` - Human-readable error message
    pub fn error(code: &str, message: &str) -> Self {
        Self::new("error", vec![code.to_string(), message.to_string()])
    }

    /// Parse an instruction from a byte buffer.
    ///
    /// Returns the parsed instruction and the number of bytes consumed.
    ///
    /// # Errors
    ///
    /// Returns `ProxyError::Protocol` if:
    /// - The buffer is incomplete (needs more data)
    /// - The format is invalid (missing separators, invalid lengths)
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        if data.is_empty() {
            return Err(ProxyError::Protocol("Empty buffer".into()));
        }

        // Find instruction terminator
        let term_pos = data
            .iter()
            .position(|&b| b == INST_TERM)
            .ok_or_else(|| ProxyError::Protocol("Incomplete instruction (no terminator)".into()))?;

        let instruction_data = &data[..term_pos];
        let mut elements = Vec::new();
        let mut pos = 0;

        // Parse length-prefixed elements
        while pos < instruction_data.len() {
            // Parse length
            let dot_pos = instruction_data[pos..]
                .iter()
                .position(|&b| b == ELEM_SEP)
                .ok_or_else(|| {
                    ProxyError::Protocol("Invalid format (missing length separator)".into())
                })?;

            let length_str = std::str::from_utf8(&instruction_data[pos..pos + dot_pos])
                .map_err(|_| ProxyError::Protocol("Invalid length encoding".into()))?;

            let length: usize = length_str.parse().map_err(|_| {
                ProxyError::Protocol(format!("Invalid length value: {}", length_str))
            })?;

            pos += dot_pos + 1; // Skip past the dot

            // Extract element content
            if pos + length > instruction_data.len() {
                return Err(ProxyError::Protocol(format!(
                    "Element length {} exceeds available data {}",
                    length,
                    instruction_data.len() - pos
                )));
            }

            let element = std::str::from_utf8(&instruction_data[pos..pos + length])
                .map_err(|_| ProxyError::Protocol("Invalid UTF-8 in element".into()))?
                .to_string();

            elements.push(element);
            pos += length;

            // Skip separator (comma) if not at end
            if pos < instruction_data.len() {
                if instruction_data[pos] != ARG_SEP {
                    return Err(ProxyError::Protocol(format!(
                        "Expected argument separator at position {}, got {:02X}",
                        pos, instruction_data[pos]
                    )));
                }
                pos += 1;
            }
        }

        if elements.is_empty() {
            return Err(ProxyError::Protocol("No opcode in instruction".into()));
        }

        let opcode = elements.remove(0);
        let args = elements;

        Ok((Self { opcode, args }, term_pos + 1))
    }

    /// Encode the instruction to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.encode_to(&mut buffer)
            .expect("Vec write should not fail");
        buffer
    }

    /// Encode the instruction to a writer.
    pub fn encode_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        // Write opcode
        write!(writer, "{}.{}", self.opcode.len(), self.opcode)?;

        // Write arguments
        for arg in &self.args {
            write!(writer, ",{}.{}", arg.len(), arg)?;
        }

        // Write terminator
        writer.write_all(&[INST_TERM])?;
        Ok(())
    }

    /// Check if this is a specific opcode.
    pub fn is(&self, opcode: &str) -> bool {
        self.opcode == opcode
    }

    /// Get the first argument, if any.
    pub fn first_arg(&self) -> Option<&str> {
        self.args.first().map(|s| s.as_str())
    }

    /// Get an argument by index.
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(|s| s.as_str())
    }
}

impl std::fmt::Display for Instruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}(", self.opcode)?;
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            // Redact password-like arguments
            if self.opcode == "connect" && (i == 3 || arg.contains("pass")) {
                write!(f, "[REDACTED]")?;
            } else if arg.len() > 50 {
                write!(f, "{}...", &arg[..47])?;
            } else {
                write!(f, "{:?}", arg)?;
            }
        }
        write!(f, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_select() {
        let data = b"6.select,5.mysql;";
        let (inst, consumed) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "select");
        assert_eq!(inst.args, vec!["mysql"]);
        assert_eq!(consumed, data.len());
    }

    #[test]
    fn test_parse_args() {
        let data = b"4.args,11.target_host,11.target_port,8.username,8.password;";
        let (inst, consumed) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "args");
        assert_eq!(
            inst.args,
            vec!["target_host", "target_port", "username", "password"]
        );
        assert_eq!(consumed, data.len());
    }

    #[test]
    fn test_parse_connect() {
        let data = b"7.connect,9.localhost,4.3306,4.root,6.secret;";
        let (inst, consumed) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "connect");
        assert_eq!(inst.args, vec!["localhost", "3306", "root", "secret"]);
        assert_eq!(consumed, data.len());
    }

    #[test]
    fn test_parse_ready() {
        let data = b"5.ready,36.550e8400-e29b-41d4-a716-446655440000;";
        let (inst, consumed) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "ready");
        assert_eq!(inst.args, vec!["550e8400-e29b-41d4-a716-446655440000"]);
        assert_eq!(consumed, data.len());
    }

    #[test]
    fn test_parse_error() {
        let data = b"5.error,8.PROTOCOL,16.Invalid database;";
        let (inst, consumed) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "error");
        assert_eq!(inst.args, vec!["PROTOCOL", "Invalid database"]);
        assert_eq!(consumed, data.len());
    }

    #[test]
    fn test_parse_empty_args() {
        let data = b"7.connect,0.,4.3306,0.,0.;";
        let (inst, _) = Instruction::parse(data).unwrap();

        assert_eq!(inst.opcode, "connect");
        assert_eq!(inst.args, vec!["", "3306", "", ""]);
    }

    #[test]
    fn test_parse_multiple_instructions() {
        let data = b"6.select,5.mysql;5.ready,4.uuid;";

        let (inst1, consumed1) = Instruction::parse(data).unwrap();
        assert_eq!(inst1.opcode, "select");

        let (inst2, _) = Instruction::parse(&data[consumed1..]).unwrap();
        assert_eq!(inst2.opcode, "ready");
    }

    #[test]
    fn test_parse_incomplete() {
        let data = b"6.select,5.mysql"; // Missing terminator
        let result = Instruction::parse(data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_length() {
        let data = b"abc.select;";
        let result = Instruction::parse(data);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_select() {
        let inst = Instruction::select("mysql");
        let encoded = inst.encode();
        assert_eq!(encoded, b"6.select,5.mysql;");
    }

    #[test]
    fn test_encode_args() {
        let inst = Instruction::args(&["host", "port"]);
        let encoded = inst.encode();
        assert_eq!(encoded, b"4.args,4.host,4.port;");
    }

    #[test]
    fn test_encode_ready() {
        let inst = Instruction::ready("test-uuid");
        let encoded = inst.encode();
        assert_eq!(encoded, b"5.ready,9.test-uuid;");
    }

    #[test]
    fn test_encode_error() {
        let inst = Instruction::error("AUTH", "Bad password");
        let encoded = inst.encode();
        assert_eq!(encoded, b"5.error,4.AUTH,12.Bad password;");
    }

    #[test]
    fn test_roundtrip() {
        let original = Instruction::new(
            "connect",
            vec![
                "db.example.com".into(),
                "3306".into(),
                "user".into(),
                "pass".into(),
            ],
        );

        let encoded = original.encode();
        let (decoded, _) = Instruction::parse(&encoded).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_display_redacts_password() {
        let inst = Instruction::new(
            "connect",
            vec![
                "host".into(),
                "3306".into(),
                "user".into(),
                "secret123".into(),
            ],
        );
        let display = format!("{}", inst);

        assert!(display.contains("[REDACTED]"));
        assert!(!display.contains("secret123"));
    }
}
