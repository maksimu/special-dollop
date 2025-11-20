// Guacamole protocol parser (zero-copy)
// Parses instructions directly from Bytes without allocations

use bytes::Bytes;
use std::str;

/// Parsed Guacamole instruction (zero-copy)
///
/// Uses string slices that reference the original Bytes buffer.
/// No allocations needed for parsing.
#[derive(Debug, Clone)]
pub struct Instruction<'a> {
    pub opcode: &'a str,
    pub args: Vec<&'a str>,
}

/// Guacamole protocol parser (zero-copy)
///
/// Parses Guacamole protocol instructions directly from Bytes without
/// allocating Strings. Uses string slices that reference the original buffer.
pub struct GuacamoleParser;

impl GuacamoleParser {
    /// Parse a single instruction from Bytes (zero-copy)
    ///
    /// Returns instruction with string slices referencing the original buffer.
    /// No allocations needed.
    ///
    /// # Format
    ///
    /// `<opcode_len>.<opcode>,<arg1_len>.<arg1>,<arg2_len>.<arg2>;`
    ///
    /// # Example
    ///
    /// ```
    /// use guacr_protocol::parser::GuacamoleParser;
    /// use bytes::Bytes;
    ///
    /// let data = Bytes::from("3.key,5.65507,1.1;");
    /// let instr = GuacamoleParser::parse_instruction(&data).unwrap();
    /// assert_eq!(instr.opcode, "key");
    /// assert_eq!(instr.args, vec!["65507", "1"]);
    /// ```
    pub fn parse_instruction(data: &Bytes) -> Result<Instruction<'_>, ParseError> {
        // Convert to &str for parsing (zero-copy if Bytes is valid UTF-8)
        let text = str::from_utf8(data.as_ref()).map_err(|_| ParseError::InvalidUtf8)?;

        Self::parse_instruction_str(text)
    }

    /// Parse instruction from string slice (zero-copy)
    ///
    /// This is the core parsing logic. Works with any string slice.
    pub fn parse_instruction_str(text: &str) -> Result<Instruction<'_>, ParseError> {
        // Find semicolon terminator
        let end = text.find(';').ok_or(ParseError::MissingTerminator)?;

        let text = &text[..end];

        // Parse opcode: <len>.<value>
        let (opcode, rest) = Self::parse_length_value(text)?;

        // Parse arguments: <len>.<value>,<len>.<value>,...
        let mut args = Vec::new();
        let mut remaining = rest;

        while !remaining.is_empty() {
            if remaining.starts_with(',') {
                remaining = &remaining[1..];
            }

            let (arg, rest) = Self::parse_length_value(remaining)?;
            args.push(arg);
            remaining = rest;
        }

        Ok(Instruction { opcode, args })
    }

    /// Parse length-value pair: <len>.<value>
    ///
    /// Returns (value_slice, remaining_text)
    fn parse_length_value(text: &str) -> Result<(&str, &str), ParseError> {
        // Find dot separator
        let dot_pos = text.find('.').ok_or(ParseError::InvalidFormat)?;

        // Parse length
        let len_str = &text[..dot_pos];
        let len: usize = len_str.parse().map_err(|_| ParseError::InvalidLength)?;

        // Extract value
        let value_start = dot_pos + 1;
        let value_end = value_start + len;

        if value_end > text.len() {
            return Err(ParseError::InvalidLength);
        }

        let value = &text[value_start..value_end];
        let remaining = &text[value_end..];

        Ok((value, remaining))
    }

    /// Parse multiple instructions from a buffer
    ///
    /// Handles multiple instructions separated by semicolons.
    pub fn parse_instructions(data: &Bytes) -> Result<Vec<Instruction<'_>>, ParseError> {
        let text = str::from_utf8(data.as_ref()).map_err(|_| ParseError::InvalidUtf8)?;

        let mut instructions = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            // Find next semicolon
            if let Some(semi_pos) = remaining.find(';') {
                let instr_text = &remaining[..=semi_pos];
                let instr = Self::parse_instruction_str(instr_text)?;
                instructions.push(instr);
                remaining = &remaining[semi_pos + 1..];
            } else {
                break;
            }
        }

        Ok(instructions)
    }
}

/// Parse error
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    InvalidUtf8,
    MissingTerminator,
    InvalidFormat,
    InvalidLength,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidUtf8 => write!(f, "Invalid UTF-8"),
            ParseError::MissingTerminator => write!(f, "Missing semicolon terminator"),
            ParseError::InvalidFormat => write!(f, "Invalid instruction format"),
            ParseError::InvalidLength => write!(f, "Invalid length value"),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_instruction() {
        let data = Bytes::from("3.key,5.65507,1.1;");
        let instr = GuacamoleParser::parse_instruction(&data).unwrap();

        assert_eq!(instr.opcode, "key");
        assert_eq!(instr.args, vec!["65507", "1"]);
    }

    #[test]
    fn test_parse_mouse_instruction() {
        let data = Bytes::from("5.mouse,1.0,2.10,2.20,1.1;");
        let instr = GuacamoleParser::parse_instruction(&data).unwrap();

        assert_eq!(instr.opcode, "mouse");
        assert_eq!(instr.args, vec!["0", "10", "20", "1"]);
    }

    #[test]
    fn test_parse_img_instruction() {
        let data = Bytes::from("3.img,1.1,1.7,1.0,10.image/jpeg,2.10,2.20;");
        let instr = GuacamoleParser::parse_instruction(&data).unwrap();

        assert_eq!(instr.opcode, "img");
        assert_eq!(instr.args.len(), 6);
        assert_eq!(instr.args[3], "image/jpeg");
    }

    #[test]
    fn test_parse_multiple_instructions() {
        let data = Bytes::from("3.key,5.65507,1.1;4.sync,10.1234567890;");
        let instructions = GuacamoleParser::parse_instructions(&data).unwrap();

        assert_eq!(instructions.len(), 2);
        assert_eq!(instructions[0].opcode, "key");
        assert_eq!(instructions[1].opcode, "sync");
    }

    #[test]
    fn test_parse_error_missing_semicolon() {
        let data = Bytes::from("3.key,5.65507,1.1");
        let result = GuacamoleParser::parse_instruction(&data);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ParseError::MissingTerminator);
    }
}
