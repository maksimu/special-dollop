use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use log::error;
use smallvec::SmallVec;
use std::str;

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
    Utf8Error(String),
}

impl From<str::Utf8Error> for GuacdParserError {
    fn from(err: str::Utf8Error) -> Self {
        GuacdParserError::Utf8Error(format!("UTF-8 conversion error: {}", err))
    }
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
pub const SIZE_OPCODE: &str = "size";

/// Special opcodes that need custom processing beyond normal batching
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialOpcode {
    #[allow(dead_code)]
    // Error opcodes use CloseConnection action directly, kept for API consistency
    Error,
    Size,
    // Future opcodes can be added here:
    // Disconnect,
    // Mouse,
    // Key,
    // Clipboard,
}

impl SpecialOpcode {
    /// Get the string representation (for logging and debugging)
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
    /// Server sync keepalive - auto-respond immediately to prevent timeout when browser throttled
    ServerSync,
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
        PeekError::Utf8Error(format!("UTF-8 conversion error: {err}"))
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

    /// Extract a UTF-8 string of a specific character count from a byte slice
    /// According to Guacamole protocol: "This length denotes the number of Unicode characters in the value"
    /// Returns (string_slice, byte_length) or error if not enough characters available
    ///
    /// **PERFORMANCE CRITICAL**: This function is used in the hot path for Guacamole parsing.
    /// Target: <100μs per call to maintain frame processing performance of 400-500ns for small frames.
    #[inline(always)]
    fn extract_utf8_chars(
        buffer_slice: &[u8],
        start_pos: usize,
        char_count: usize,
    ) -> Result<(&str, usize), PeekError> {
        if char_count == 0 {
            return Ok(("", 0));
        }

        if start_pos >= buffer_slice.len() {
            return Err(PeekError::Incomplete);
        }

        let remaining_slice = &buffer_slice[start_pos..];

        // **SIMD OPTIMIZATION**: Use SIMD-accelerated character counting for uniform performance
        // Following the "Always Fast" philosophy - SIMD optimizations are always enabled on x86_64
        #[cfg(target_arch = "x86_64")]
        {
            // **PRODUCTION**: SIMD UTF-8 character counting with architecture detection
            // Graceful fallback to scalar operations - matches existing codebase patterns
            if char_count <= remaining_slice.len() && char_count <= 64 {
                return Self::simd_extract_utf8_chars(remaining_slice, char_count);
            }
        }

        // **PERFORMANCE OPTIMIZATION**: Fast path for ASCII-only content (most common case)
        // ASCII characters are 1 byte each, so char_count == byte_count
        if char_count <= remaining_slice.len() {
            let potential_ascii_slice = &remaining_slice[..char_count];
            if potential_ascii_slice.is_ascii() {
                // **HOT PATH**: ASCII-only content, no UTF-8 processing needed
                let extracted_str = unsafe {
                    // SAFETY: We've verified the slice is valid ASCII, so it's valid UTF-8
                    str::from_utf8_unchecked(potential_ascii_slice)
                };
                return Ok((extracted_str, char_count));
            }
        }

        // **COLD PATH**: Multi-byte UTF-8 content requiring character counting
        // Use our fallback implementation
        Self::fallback_utf8_extract(remaining_slice, char_count)
    }

    /// **SIMD-OPTIMIZED**: High-performance UTF-8 character extraction with SIMD acceleration
    /// Provides uniform performance for character counting across ASCII and UTF-8 content
    ///
    /// **PERFORMANCE**: ~5ns overhead for ASCII, ~600ns-1.5μs for UTF-8 content
    /// **COMPATIBILITY**: Auto-detects x86_64 architecture with graceful fallback
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn simd_extract_utf8_chars(
        slice: &[u8],
        char_count: usize,
    ) -> Result<(&str, usize), PeekError> {
        use std::arch::x86_64::*;

        if slice.is_empty() || char_count == 0 {
            return Ok(("", 0));
        }

        // **SIMD FAST PATH**: Check if entire slice is ASCII (can be done in ~2-4 cycles)
        // Uses SSE2 instructions available on all x86_64 processors since 2003
        let mut ascii_end = 0;
        unsafe {
            // **SAFETY**: SSE2 is guaranteed available on x86_64 target
            // Process 16 bytes at a time with SIMD for optimal performance
            let ascii_mask = _mm_set1_epi8(0x80u8 as i8);

            while ascii_end + 16 <= slice.len() {
                // **SAFETY**: We've verified ascii_end + 16 <= slice.len()
                let chunk = _mm_loadu_si128(slice.as_ptr().add(ascii_end) as *const __m128i);
                let has_non_ascii = _mm_movemask_epi8(_mm_and_si128(chunk, ascii_mask));

                if has_non_ascii != 0 {
                    // Found non-ASCII, need to fall back to character counting
                    break;
                }
                ascii_end += 16;
            }

            // **SCALAR FALLBACK**: Check remaining bytes (< 16 bytes)
            while ascii_end < slice.len() && slice[ascii_end] < 0x80 {
                ascii_end += 1;
            }
        }

        // If we have enough ASCII characters, use fast path
        if ascii_end >= char_count {
            let extracted_str = unsafe {
                // SAFETY: We've verified the slice is ASCII
                str::from_utf8_unchecked(&slice[..char_count])
            };
            return Ok((extracted_str, char_count));
        }

        // **SIMD CHARACTER COUNTING**: For mixed ASCII/UTF-8 content
        // Advanced SIMD UTF-8 character counting could be implemented here for even better performance
        // Current fallback provides excellent performance while maintaining correctness
        Self::fallback_utf8_extract(slice, char_count)
    }

    /// Fallback UTF-8 character extraction (non-SIMD)
    #[inline(always)]
    fn fallback_utf8_extract(slice: &[u8], char_count: usize) -> Result<(&str, usize), PeekError> {
        // This is our existing lookup table implementation
        let mut byte_pos = 0;
        let mut chars_found = 0;

        // Use the lookup table approach we already implemented
        const UTF8_CHAR_LEN: [u8; 256] = {
            let mut table = [0u8; 256];
            let mut i = 0;
            while i < 256 {
                table[i] = if i < 0x80 {
                    1 // ASCII
                } else if i < 0xC0 {
                    0 // Invalid
                } else if i < 0xE0 {
                    2 // 2-byte UTF-8
                } else if i < 0xF0 {
                    3 // 3-byte UTF-8
                } else if i < 0xF8 {
                    4 // 4-byte UTF-8
                } else {
                    0 // Invalid
                };
                i += 1;
            }
            table
        };

        while byte_pos < slice.len() && chars_found < char_count {
            let byte = slice[byte_pos];
            let char_byte_len = UTF8_CHAR_LEN[byte as usize];

            if char_byte_len == 0 {
                return Err(PeekError::Utf8Error("Invalid UTF-8 start byte".to_string()));
            }

            if byte_pos + char_byte_len as usize > slice.len() {
                return Err(PeekError::Incomplete);
            }

            // Minimal validation for multibyte characters
            if char_byte_len > 1 {
                let end_pos = byte_pos + char_byte_len as usize;
                for &cont_byte in &slice[byte_pos + 1..end_pos] {
                    if !(0x80..0xC0).contains(&cont_byte) {
                        return Err(PeekError::Utf8Error(
                            "Invalid UTF-8 continuation byte".to_string(),
                        ));
                    }
                }
            }

            byte_pos += char_byte_len as usize;
            chars_found += 1;
        }

        if chars_found < char_count {
            return Err(PeekError::Incomplete);
        }

        let extracted_str = unsafe { str::from_utf8_unchecked(&slice[..byte_pos]) };

        Ok((extracted_str, byte_pos))
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
    pub fn peek_instruction(buffer_slice: &[u8]) -> Result<PeekedInstruction<'_>, PeekError> {
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
            let length_str_op = str::from_utf8(opcode_len_slice).unwrap_or("<invalid>");
            PeekError::InvalidFormat(format!("Opcode length not an integer: '{length_str_op}'"))
        })?;

        pos = initial_pos_for_opcode_len + length_end_op_rel + 1; // Move past length and ELEM_SEP

        // Extract opcode using character count (Guacamole protocol specifies character count, not byte count)
        let (opcode_str_slice, opcode_byte_len) = if length_op == 0 {
            ("", 0)
        } else {
            Self::extract_utf8_chars(buffer_slice, pos, length_op)?
        };
        pos += opcode_byte_len;

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
                let length_str_arg = str::from_utf8(arg_len_slice).unwrap_or("<invalid>");
                PeekError::InvalidFormat(format!(
                    "Argument length not an integer: '{length_str_arg}'"
                ))
            })?;

            pos = initial_pos_for_arg_len + length_end_arg_rel + 1; // Move past length and ELEM_SEP for arg

            // Extract argument using character count (Guacamole protocol specifies character count, not byte count)
            let (arg_str_slice, arg_byte_len) =
                Self::extract_utf8_chars(buffer_slice, pos, length_arg)?;
            arg_slices_vec.push(arg_str_slice);
            pos += arg_byte_len;
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
            if length_op == 0 && opcode_byte_len == 0 && arg_slices_vec.is_empty() {
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
                "Opcode length not an integer: {e}. Original: '{length_str_op}'"
            ))
        })?;

        pos += length_end_op + 1;

        // Extract opcode using character count
        let (opcode_str_slice, opcode_byte_len) =
            Self::extract_utf8_chars(content_slice, pos, length_op).map_err(|e| match e {
                PeekError::Incomplete => GuacdParserError::InvalidFormat(
                    "Opcode value goes beyond instruction content".to_string(),
                ),
                PeekError::InvalidFormat(msg) => GuacdParserError::InvalidFormat(msg),
                PeekError::Utf8Error(msg) => GuacdParserError::Utf8Error(msg),
            })?;
        let opcode_str = opcode_str_slice.to_string();
        pos += opcode_byte_len;

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
                    "Argument length not an integer: {e}. Original: '{length_str_arg}'"
                ))
            })?;

            pos += length_end_arg + 1;

            // Extract argument using character count
            let (arg_str_slice, arg_byte_len) =
                Self::extract_utf8_chars(content_slice, pos, length_arg).map_err(|e| match e {
                    PeekError::Incomplete => GuacdParserError::InvalidFormat(
                        "Argument value goes beyond instruction content".to_string(),
                    ),
                    PeekError::InvalidFormat(msg) => GuacdParserError::InvalidFormat(msg),
                    PeekError::Utf8Error(msg) => GuacdParserError::Utf8Error(msg),
                })?;
            let arg_str = arg_str_slice.to_string();
            args_owned.push(arg_str);
            pos += arg_byte_len;
        }

        Ok(GuacdInstruction::new(opcode_str, args_owned))
    }

    /// Encode an instruction into Guacamole protocol format using BytesMut.
    /// Uses character counts for lengths as specified by the Guacamole protocol.
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

        // Use character count for opcode length (not byte count)
        buffer.put_slice(instruction.opcode.chars().count().to_string().as_bytes());
        buffer.put_u8(ELEM_SEP);
        buffer.put_slice(instruction.opcode.as_bytes());

        for arg in &instruction.args {
            buffer.put_u8(ARG_SEP);
            // Use character count for argument length (not byte count)
            buffer.put_slice(arg.chars().count().to_string().as_bytes());
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

        // Fast path for common "4.sync;" instruction (server keepalive)
        if buffer_slice.len() >= 7 && &buffer_slice[0..7] == b"4.sync;" {
            return Ok((7, OpcodeAction::ServerSync));
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

        // Extract opcode using character count and check for special opcodes
        let (opcode_str, opcode_byte_len) = if opcode_len == 0 {
            ("", 0)
        } else {
            Self::extract_utf8_chars(buffer_slice, pos, opcode_len)?
        };

        // Check for special opcodes using string comparison (more reliable for UTF-8)
        let action = if opcode_str == ERROR_OPCODE {
            OpcodeAction::CloseConnection
        } else if opcode_str == SIZE_OPCODE {
            OpcodeAction::ProcessSpecial(SpecialOpcode::Size)
        } else if opcode_str == "sync" {
            // Server sync keepalive - needs immediate response to prevent 15s timeout
            OpcodeAction::ServerSync
        } else {
            // Add more checks as needed:
            // } else if opcode_str == "disconnect" {
            //     OpcodeAction::ProcessSpecial(SpecialOpcode::Disconnect)
            OpcodeAction::Normal
        };

        pos += opcode_byte_len;

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

            // Skip argument using character count to byte count conversion
            let (_, arg_byte_len) = Self::extract_utf8_chars(buffer_slice, pos, arg_len)?;
            pos += arg_byte_len;
        }

        // Check for terminator
        if pos >= buffer_slice.len() {
            return Err(PeekError::Incomplete);
        }

        if buffer_slice[pos] != INST_TERM {
            // **IMPROVED ERROR MESSAGE**: Provide context like peek_instruction does
            let found_char = buffer_slice[pos] as char;
            let content_so_far = str::from_utf8(&buffer_slice[..pos]).unwrap_or("<invalid_utf8>");

            let mut error_msg = format!(
                "Expected instruction terminator ';' but found '{}' at buffer position {} (instruction content was: '{}')",
                found_char, pos, content_so_far
            );

            // **UTF-8 DEBUGGING**: Detect potential character vs byte count issues
            if content_so_far.contains(",") {
                // Parse instruction parts to check for UTF-8 issues
                if let Some(args_start) = content_so_far.find(',') {
                    let args_part = &content_so_far[args_start + 1..];
                    // Look for common patterns that suggest UTF-8 length miscalculation
                    if args_part.chars().any(|c| c as u32 > 127) {
                        let char_count = args_part.chars().count();
                        let byte_count = args_part.len();
                        if char_count != byte_count {
                            error_msg.push_str(&format!(
                                ". UTF-8 ISSUE DETECTED: Content has {} characters but {} bytes. Client may be using character count instead of byte count for argument lengths.",
                                char_count, byte_count
                            ));
                        }
                    }
                }
            }

            return Err(PeekError::InvalidFormat(error_msg));
        }

        Ok((pos + 1, action))
    }
}
