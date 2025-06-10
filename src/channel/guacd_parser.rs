use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use smallvec::SmallVec;
use std::str;
use tracing::error;

// Guacamole protocol constants
pub const INST_TERM: u8 = b';';
pub const ARG_SEP: u8 = b',';
pub const ELEM_SEP: u8 = b'.';

/// Represents a fully parsed Guacamole protocol instruction with owned Strings.
/// This is typically used for error messages or when detailed inspection is needed,
/// or when an owned version of the instruction is required.
#[derive(Debug, Clone, PartialEq)]
pub struct GuacdInstruction {
    pub opcode: String,
    pub args: Vec<String>,
}

impl GuacdInstruction {
    pub fn new(opcode: String, args: Vec<String>) -> Self {
        Self { opcode, args }
    }
}

/// Error type for full Guacamole protocol instruction content parsing.
/// This is used when converting a content slice to an owned GuacdInstruction.
#[derive(Debug, thiserror::Error)]
pub enum GuacdParserError {
    #[error("Invalid instruction format: {0}")]
    InvalidFormat(String),
    #[error("UTF-8 error in instruction content: {0}")]
    Utf8Error(#[from] str::Utf8Error),
}

/// Information about a Guacamole instruction peeked from a buffer, using borrowed slices.
#[derive(Debug, PartialEq)]
pub struct PeekedInstruction<'a> {
    /// Slice of the opcode.
    pub opcode: &'a str,
    /// Vector of slices for each argument. Uses SmallVec to avoid heap allocation for up to 4 args.
    pub args: SmallVec<[&'a str; 4]>,
    /// The total length of this instruction (including the terminator ';') in the input buffer.
    pub total_length_in_buffer: usize,
    /// True if the opcode is "error".
    pub is_error_opcode: bool,
}

// Common opcode constants for fast comparison
pub const ERROR_OPCODE: &str = "error";
pub const ERROR_OPCODE_BYTES: &[u8] = b"error";
#[allow(dead_code)] // Kept for API consistency and future use
pub const SIZE_OPCODE: &str = "size";
pub const SIZE_OPCODE_BYTES: &[u8] = b"size";

/// Special opcodes that need custom processing beyond normal batching
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialOpcode {
    Error,
    Size,
    // Future opcodes can be added here:
    // Disconnect,
    // Mouse,
    // Key,
    // Clipboard,
}

impl SpecialOpcode {
    /// Fast byte-level comparison for hot path performance
    #[inline(always)]
    fn matches_bytes(&self, opcode_bytes: &[u8]) -> bool {
        match self {
            SpecialOpcode::Error => opcode_bytes == ERROR_OPCODE_BYTES,
            SpecialOpcode::Size => opcode_bytes == SIZE_OPCODE_BYTES,
            // Add more cases as needed:
            // SpecialOpcode::Disconnect => opcode_bytes == b"disconnect",
        }
    }

    /// Get the string representation (for logging)
    #[allow(dead_code)] // Useful for future logging/debugging
    pub fn as_str(&self) -> &'static str {
        match self {
            SpecialOpcode::Error => ERROR_OPCODE,
            SpecialOpcode::Size => SIZE_OPCODE,
            // SpecialOpcode::Disconnect => "disconnect",
        }
    }
}

/// Result of opcode analysis - what type of special processing is needed
#[derive(Debug, PartialEq)]
pub enum OpcodeAction {
    /// Normal instruction - batch with others
    Normal,
    /// Error instruction - close connection immediately  
    CloseConnection,
    /// Special instruction needing custom processing
    ProcessSpecial(SpecialOpcode),
}

/// Error type for the peeking operation.
#[derive(Debug, PartialEq, Clone)]
pub enum PeekError {
    /// Not enough data in the buffer to form a complete instruction.
    Incomplete,
    /// The instruction format is invalid.
    InvalidFormat(String),
    /// UTF-8 error encountered while trying to interpret parts of the instruction as string slices.
    Utf8Error(String), // Store problem string for context
}

// Convert std::str::Utf8Error to PeekError for convenience in peek_instruction
impl From<str::Utf8Error> for PeekError {
    fn from(err: str::Utf8Error) -> Self {
        PeekError::Utf8Error(format!("UTF-8 conversion error: {}", err))
    }
}

/// A stateless parser for the Guacamole protocol.
/// Methods operate on provided buffer slices.
pub struct GuacdParser;

impl GuacdParser {
    /// Fast search for a delimiter byte in a slice
    /// Uses platform-specific optimizations when available
    #[inline(always)]
    fn find_delimiter(slice: &[u8], delimiter: u8) -> Option<usize> {
        // For small slices, use the standard iterator
        // For larger slices, memchr crate would be faster, but we'll use the standard approach
        slice.iter().position(|&b| b == delimiter)
    }

    /// Fast integer parsing for small numbers (optimized for lengths)
    /// Most Guacd instruction lengths are < 100
    #[inline(always)]
    fn parse_length(slice: &[u8]) -> Result<usize, ()> {
        if slice.is_empty() {
            return Err(());
        }

        // Handle single-digit optimizations
        if slice.len() == 1 {
            let b = slice[0];
            if b.is_ascii_digit() {
                return Ok((b - b'0') as usize);
            }
            return Err(());
        }

        let mut result = 0usize;
        for &b in slice {
            if !b.is_ascii_digit() {
                return Err(());
            }
            result = result * 10 + (b - b'0') as usize;
            // Prevent overflow for reasonable instruction sizes
            if result > 1_000_000 {
                return Err(());
            }
        }
        Ok(result)
    }

    /// Peeks at the beginning of the `buffer_slice` to find the first complete Guacamole instruction.
    /// If successful, returns a `PeekedInstruction` containing slices that borrow from `buffer_slice`.
    /// This operation aims to be zero-copy for the instruction's string data.
    ///
    /// # Arguments
    /// * `buffer_slice`: The byte slice to peek into. This slice might contain multiple instructions
    ///   or partial instructions.
    ///
    /// # Returns
    /// * `Ok(PeekedInstruction)`: If a complete instruction is found at the beginning of the slice.
    /// * `Err(PeekError::Incomplete)`: If no terminating ';' is found, or data is too short
    ///   to form a valid instruction structure before a potential terminator.
    /// * `Err(PeekError::InvalidFormat)`: If the instruction structure is malformed.
    /// * `Err(PeekError::Utf8Error)`: If string parts are not valid UTF-8.
    ///   Returns borrowed data that references the input buffer
    ///   Returns Ok with instruction details and total length, or Err if incomplete/invalid
    #[inline(always)]
    pub fn peek_instruction(buffer_slice: &[u8]) -> Result<PeekedInstruction, PeekError> {
        // **BOLD WARNING: HOT PATH - CALLED FOR EVERY GUACD INSTRUCTION**
        // **NO STRING ALLOCATIONS, NO HEAP ALLOCATIONS**
        // **RETURN ONLY BORROWED SLICES FROM INPUT BUFFER**
        // **USE SmallVec TO AVOID HEAP ALLOCATION FOR ARGS**

        if buffer_slice.is_empty() {
            return Err(PeekError::Incomplete);
        }

        // **PERFORMANCE: Fast path for sync instruction (most common)**
        // sync is "4.sync;" which is 7 bytes
        if buffer_slice.len() >= 7 && &buffer_slice[0..7] == b"4.sync;" {
            return Ok(PeekedInstruction {
                opcode: "sync",
                args: SmallVec::new(),
                total_length_in_buffer: 7,
                is_error_opcode: false,
            });
        }

        let mut pos = 0;
        let mut arg_slices_vec: SmallVec<[&str; 4]> = SmallVec::new();

        // Parse opcode
        let initial_pos_for_opcode_len = pos;
        // Check for opcode length delimiter '.'
        let length_end_op_rel =
            Self::find_delimiter(&buffer_slice[initial_pos_for_opcode_len..], ELEM_SEP)
                .ok_or_else(|| {
                    // If no '.', it's incomplete unless a ';' is found immediately (malformed)
                    if Self::find_delimiter(&buffer_slice[initial_pos_for_opcode_len..], INST_TERM)
                        .is_some()
                    {
                        PeekError::InvalidFormat(
                            "Malformed opcode: no length delimiter before instruction end."
                                .to_string(),
                        )
                    } else {
                        PeekError::Incomplete
                    }
                })?;

        // Ensure the buffer is long enough for the length string itself
        if initial_pos_for_opcode_len + length_end_op_rel >= buffer_slice.len() {
            return Err(PeekError::Incomplete); // Not enough for the "L." part
        }

        let opcode_len_slice = &buffer_slice
            [initial_pos_for_opcode_len..initial_pos_for_opcode_len + length_end_op_rel];

        // **PERFORMANCE: Use fast integer parsing**
        let length_op: usize = Self::parse_length(opcode_len_slice).map_err(|_| {
            PeekError::InvalidFormat(format!(
                "Opcode length not an integer: '{}'",
                str::from_utf8(opcode_len_slice).unwrap_or("<invalid>")
            ))
        })?;

        pos = initial_pos_for_opcode_len + length_end_op_rel + 1; // Move past length and ELEM_SEP

        // Ensure the buffer is long enough for opcode value
        if pos + length_op > buffer_slice.len() {
            return Err(PeekError::Incomplete); // Not enough for opcode value
        }
        let opcode_value_slice = &buffer_slice[pos..pos + length_op];
        // Special case for "0.;" - opcode is empty string
        // Check if length_op is 0.
        // The opcode_value_slice will be empty in this case.
        let opcode_str_slice = if length_op == 0 {
            // This handles the "0." part of "0.;"
            ""
        } else {
            str::from_utf8(opcode_value_slice)?
        };
        pos += length_op;

        // Parse arguments
        while pos < buffer_slice.len() && buffer_slice[pos] == ARG_SEP {
            pos += 1; // Skip ARG_SEP

            // Check for data after comma
            if pos >= buffer_slice.len() {
                // Dangling comma implies incomplete if no further terminator,
                // or malformed if terminator is next.
                // If we only have a dangling comma and then end of buffer_slice, it's incomplete.
                return Err(PeekError::Incomplete);
            }

            let initial_pos_for_arg_len = pos;
            let length_end_arg_rel =
                Self::find_delimiter(&buffer_slice[initial_pos_for_arg_len..], ELEM_SEP)
                    .ok_or_else(|| {
                        if Self::find_delimiter(&buffer_slice[initial_pos_for_arg_len..], INST_TERM)
                            .is_some()
                        {
                            PeekError::InvalidFormat(
                                "Malformed argument: no length delimiter before instruction end."
                                    .to_string(),
                            )
                        } else {
                            PeekError::Incomplete
                        }
                    })?;

            if initial_pos_for_arg_len + length_end_arg_rel >= buffer_slice.len() {
                return Err(PeekError::Incomplete); // Not enough for the "L." part of arg
            }

            let arg_len_slice = &buffer_slice
                [initial_pos_for_arg_len..initial_pos_for_arg_len + length_end_arg_rel];

            // **PERFORMANCE: Use fast integer parsing**
            let length_arg: usize = Self::parse_length(arg_len_slice).map_err(|_| {
                PeekError::InvalidFormat(format!(
                    "Argument length not an integer: '{}'",
                    str::from_utf8(arg_len_slice).unwrap_or("<invalid>")
                ))
            })?;

            pos = initial_pos_for_arg_len + length_end_arg_rel + 1; // Move past length and ELEM_SEP for arg

            if pos + length_arg > buffer_slice.len() {
                return Err(PeekError::Incomplete); // Not enough for argument value
            }
            let arg_value_slice = &buffer_slice[pos..pos + length_arg];
            let arg_str_slice = str::from_utf8(arg_value_slice)?;
            arg_slices_vec.push(arg_str_slice);
            pos += length_arg;
        }

        // After parsing opcode and all args, the current `pos` should be at the terminator
        if pos == buffer_slice.len() {
            // Buffer ends exactly where terminator should be
            return Err(PeekError::Incomplete); // Missing terminator / instruction abruptly ends
        }

        // We have at least one more character at buffer_slice[pos]
        if buffer_slice[pos] == INST_TERM {
            // Correctly terminated instruction
            // Handles "0.;" specifically to ensure opcode is empty and args are empty
            if length_op == 0 && opcode_value_slice.is_empty() && arg_slices_vec.is_empty() {
                return Ok(PeekedInstruction {
                    opcode: "",
                    args: SmallVec::new(),
                    total_length_in_buffer: pos + 1,
                    is_error_opcode: false,
                });
            }

            let is_err_op = opcode_str_slice == ERROR_OPCODE;
            Ok(PeekedInstruction {
                opcode: opcode_str_slice,
                args: arg_slices_vec,
                total_length_in_buffer: pos + 1,
                is_error_opcode: is_err_op,
            })
        } else {
            // Found a character, but it's not the correct terminator
            // The `pos` here is the location of the unexpected character.
            // The content parsed so far is `&buffer_slice[..pos]`.
            Err(PeekError::InvalidFormat(format!(
                "Expected instruction terminator ';' but found '{}' at buffer position {} (instruction content was: '{}')",
                buffer_slice[pos] as char, pos, str::from_utf8(&buffer_slice[..pos]).unwrap_or("<invalid_utf8>")
            )))
        }
    }

    /// Parses a Guacamole instruction content slice (must NOT include the trailing ';')
    /// into an owned `GuacdInstruction` with `String`s.
    pub fn parse_instruction_content(
        content_slice: &[u8],
    ) -> Result<GuacdInstruction, GuacdParserError> {
        let mut args_owned = Vec::new();
        let mut pos = 0;

        if content_slice.is_empty() {
            // Corresponds to "0.;" if the terminator was removed
            return Ok(GuacdInstruction::new("".to_string(), vec![]));
        }
        // "0." is the content of "0.;"
        if content_slice.len() == 2 && content_slice[0] == b'0' && content_slice[1] == ELEM_SEP {
            return Ok(GuacdInstruction::new("".to_string(), vec![]));
        }

        // Parse opcode
        let length_end_op = content_slice[pos..]
            .iter()
            .position(|&b| b == ELEM_SEP)
            .ok_or_else(|| {
                GuacdParserError::InvalidFormat("Malformed opcode: no length delimiter".to_string())
            })?;

        let length_str_op = str::from_utf8(&content_slice[pos..pos + length_end_op])?;

        let length_op: usize = length_str_op.parse().map_err(|e| {
            GuacdParserError::InvalidFormat(format!(
                "Opcode length not an integer: {}. Original: '{}'",
                e, length_str_op
            ))
        })?;

        pos += length_end_op + 1;

        if pos + length_op > content_slice.len() {
            return Err(GuacdParserError::InvalidFormat(
                "Opcode value goes beyond instruction content".to_string(),
            ));
        }
        let opcode_str = str::from_utf8(&content_slice[pos..pos + length_op])?.to_string();
        pos += length_op;

        // Parse arguments
        while pos < content_slice.len() {
            if content_slice[pos] != ARG_SEP {
                return Err(GuacdParserError::InvalidFormat(format!(
                    "Expected argument separator ',' but found '{}' at content position {}",
                    content_slice[pos] as char, pos
                )));
            }
            pos += 1; // Skip ARG_SEP

            if pos >= content_slice.len() {
                return Err(GuacdParserError::InvalidFormat(
                    "Dangling comma at end of instruction content".to_string(),
                ));
            }

            let length_end_arg = content_slice[pos..]
                .iter()
                .position(|&b| b == ELEM_SEP)
                .ok_or_else(|| {
                    GuacdParserError::InvalidFormat(
                        "Malformed argument: no length delimiter".to_string(),
                    )
                })?;

            let length_str_arg = str::from_utf8(&content_slice[pos..pos + length_end_arg])?;

            let length_arg: usize = length_str_arg.parse().map_err(|e| {
                GuacdParserError::InvalidFormat(format!(
                    "Argument length not an integer: {}. Original: '{}'",
                    e, length_str_arg
                ))
            })?;

            pos += length_end_arg + 1;

            if pos + length_arg > content_slice.len() {
                return Err(GuacdParserError::InvalidFormat(
                    "Argument value goes beyond instruction content".to_string(),
                ));
            }
            let arg_str = str::from_utf8(&content_slice[pos..pos + length_arg])?.to_string();
            args_owned.push(arg_str);
            pos += length_arg;
        }

        Ok(GuacdInstruction::new(opcode_str, args_owned))
    }

    /// Encode an instruction into Guacamole protocol format using BytesMut.
    pub fn guacd_encode_instruction(instruction: &GuacdInstruction) -> Bytes {
        let estimated_size = instruction.opcode.len()
            + instruction
                .args
                .iter()
                .map(|arg| arg.len() + 10)
                .sum::<usize>()
            + instruction.args.len() * 2
            + 10; // Approximation for lengths and separators
        let mut buffer = BytesMut::with_capacity(estimated_size);
        buffer.put_slice(instruction.opcode.len().to_string().as_bytes());
        buffer.put_u8(ELEM_SEP);
        buffer.put_slice(instruction.opcode.as_bytes());
        for arg in &instruction.args {
            buffer.put_u8(ARG_SEP);
            buffer.put_slice(arg.len().to_string().as_bytes());
            buffer.put_u8(ELEM_SEP);
            buffer.put_slice(arg.as_bytes());
        }
        buffer.put_u8(INST_TERM);
        buffer.freeze()
    }

    /// Helper for tests or specific cases: decodes a complete raw instruction slice into GuacdInstruction.
    /// The input slice should be a single, complete Guacamole instruction *without* the final semicolon.
    #[cfg(test)]
    pub(crate) fn guacd_decode_for_test(
        data_without_terminator: &[u8],
    ) -> Result<GuacdInstruction, GuacdParserError> {
        Self::parse_instruction_content(data_without_terminator)
    }

    /// **ULTRA-FAST PATH: Validate format and detect special opcodes**
    /// Returns (total_bytes, action_needed) with zero allocations
    #[inline(always)]
    pub fn validate_and_detect_special(
        buffer_slice: &[u8],
    ) -> Result<(usize, OpcodeAction), PeekError> {
        if buffer_slice.is_empty() {
            return Err(PeekError::Incomplete);
        }

        // Fast path for common "4.sync;" instruction
        if buffer_slice.len() >= 7 && &buffer_slice[0..7] == b"4.sync;" {
            return Ok((7, OpcodeAction::Normal));
        }

        let mut pos = 0;

        // Parse opcode length
        let length_end =
            Self::find_delimiter(&buffer_slice[pos..], ELEM_SEP).ok_or(PeekError::Incomplete)?;

        if pos + length_end >= buffer_slice.len() {
            return Err(PeekError::Incomplete);
        }

        let opcode_len = Self::parse_length(&buffer_slice[pos..pos + length_end])
            .map_err(|_| PeekError::InvalidFormat("Invalid opcode length".to_string()))?;

        pos += length_end + 1; // Skip past length and '.'

        // Check if we have enough bytes for the opcode
        if pos + opcode_len > buffer_slice.len() {
            return Err(PeekError::Incomplete);
        }

        // **OPTIMIZED: Check all special opcodes with single pass**
        let opcode_bytes = &buffer_slice[pos..pos + opcode_len];

        // Check for special opcodes using fast byte comparison
        let action = if SpecialOpcode::Error.matches_bytes(opcode_bytes) {
            OpcodeAction::CloseConnection
        } else if SpecialOpcode::Size.matches_bytes(opcode_bytes) {
            OpcodeAction::ProcessSpecial(SpecialOpcode::Size)
        } else {
            // Add more checks as needed:
            // } else if SpecialOpcode::Disconnect.matches_bytes(opcode_bytes) {
            //     OpcodeAction::ProcessSpecial(SpecialOpcode::Disconnect)
            OpcodeAction::Normal
        };

        pos += opcode_len;

        // Skip through arguments without parsing
        while pos < buffer_slice.len() && buffer_slice[pos] == ARG_SEP {
            pos += 1; // Skip ','

            if pos >= buffer_slice.len() {
                return Err(PeekError::Incomplete);
            }

            // Find argument length delimiter
            let arg_len_end = Self::find_delimiter(&buffer_slice[pos..], ELEM_SEP)
                .ok_or(PeekError::Incomplete)?;

            if pos + arg_len_end >= buffer_slice.len() {
                return Err(PeekError::Incomplete);
            }

            let arg_len = Self::parse_length(&buffer_slice[pos..pos + arg_len_end])
                .map_err(|_| PeekError::InvalidFormat("Invalid argument length".to_string()))?;

            pos += arg_len_end + 1; // Skip past length and '.'

            if pos + arg_len > buffer_slice.len() {
                return Err(PeekError::Incomplete);
            }

            pos += arg_len; // Skip argument value
        }

        // Check for terminator
        if pos >= buffer_slice.len() || buffer_slice[pos] != INST_TERM {
            return if pos >= buffer_slice.len() {
                Err(PeekError::Incomplete)
            } else {
                Err(PeekError::InvalidFormat("Missing terminator".to_string()))
            };
        }

        Ok((pos + 1, action))
    }
}
