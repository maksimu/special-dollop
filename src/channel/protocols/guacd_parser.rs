use bytes::{Bytes, BytesMut, BufMut, Buf};
use crate::buffer_pool::BufferPool;
use std::str;

// Guacamole protocol constants
pub const BUFFER_TRUNCATION_THRESHOLD: usize = 8192;
pub const INST_TERM: u8 = b';';
pub const ARG_SEP: u8 = b',';
pub const ELEM_SEP: u8 = b'.';

/// Represents a parsed Guacamole instruction
#[derive(Debug, Clone)]
pub struct InstructionObject {
    /// The operation code (like "size", "audio", etc.)
    pub opcode: String,
    /// List of arguments for this instruction
    pub args: Vec<String>,
}

impl InstructionObject {
    pub fn new(opcode: String, args: Vec<String>) -> Self {
        Self { opcode, args }
    }
}

/// Error type for invalid Guacamole protocol instructions
#[derive(Debug, thiserror::Error)]
pub enum GuacdError {
    #[error("Invalid instruction: {0}")]
    InvalidInstruction(String),
    
    #[error("Parse error: {0}")]
    ParseError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("UTF-8 error: {0}")]
    Utf8Error(#[from] str::Utf8Error),
}

/// Guacamole protocol parser
pub struct GuacdParser {
    buffer: BytesMut,
    element_buffer: Vec<String>,
    element_end: isize,
    start_index: usize,
    element_codepoints: usize,
    instructions: Vec<InstructionObject>,
    buffer_pool: Option<BufferPool>,
}

impl GuacdParser {
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::with_capacity(4096),
            element_buffer: Vec::new(),
            element_end: -1,
            start_index: 0,
            element_codepoints: 0,
            instructions: Vec::new(),
            buffer_pool: None,
        }
    }

    pub fn with_buffer_pool(pool: BufferPool) -> Self {
        Self {
            buffer: pool.acquire(),
            element_buffer: Vec::new(),
            element_end: -1,
            start_index: 0,
            element_codepoints: 0,
            instructions: Vec::new(),
            buffer_pool: Some(pool),
        }
    }

    /// Process received data and then parse into instructions
    pub fn receive(&mut self, connection_no: u32, packet: &[u8]) -> Result<(), GuacdError> {
        // Truncate the buffer if needed
        if BUFFER_TRUNCATION_THRESHOLD < self.start_index && self.start_index <= self.element_end as usize {
            // Create a new buffer if using buffer pool
            if let Some(pool) = &self.buffer_pool {
                let mut new_buf = pool.acquire();
                new_buf.put_slice(&self.buffer[self.start_index..]);
                self.element_end -= self.start_index as isize;
                self.start_index = 0;
                
                // Release old buffer back to the pool
                let old_buf = std::mem::replace(&mut self.buffer, new_buf);
                pool.release(old_buf);
            } else {
                // Otherwise, just advance the buffer and keep memory
                self.buffer.advance(self.start_index);
                self.element_end -= self.start_index as isize;
                self.start_index = 0;
            }
        }
        
        // Append new data to the buffer
        self.buffer.extend_from_slice(packet);

        // Parse instructions from the buffer
        while self.start_index < self.buffer.len() {
            if self.element_end < 0 {
                // We need to find the next element length marker
                let mut found_sep = false;
                let mut length_end = 0;
                
                // Search for ELEM_SEP from start_index
                for i in self.start_index..self.buffer.len() {
                    if self.buffer[i] == ELEM_SEP {
                        length_end = i;
                        found_sep = true;
                        break;
                    }
                }
                
                if found_sep {
                    if length_end > self.start_index {
                        // Parse element length from bytes
                        match str::from_utf8(&self.buffer[self.start_index..length_end]) {
                            Ok(len_str) => {
                                match len_str.parse::<usize>() {
                                    Ok(codepoints) => {
                                        self.element_codepoints = codepoints;
                                        self.start_index = length_end + 1;
                                        self.element_end = (self.start_index + self.element_codepoints) as isize;
                                    },
                                    Err(e) => {
                                        return Err(GuacdError::ParseError(format!("Error parsing element codepoint: {}", e)));
                                    }
                                }
                            },
                            Err(e) => {
                                return Err(GuacdError::Utf8Error(e));
                            }
                        }
                    } else {
                        // Handle case where length_end == start_index
                        self.start_index = length_end + 1;
                        continue;
                    }
                } else {
                    // No more length markers found, wait for more data
                    break;
                }
            }

            // Check if we have enough data to process the current element
            if (self.element_end as usize) < self.buffer.len() {
                // Extract an element and check terminator
                match str::from_utf8(&self.buffer[self.start_index..(self.element_end as usize)]) {
                    Ok(element) => {
                        // Get terminator character
                        let terminator = if (self.element_end as usize) < self.buffer.len() {
                            Some(self.buffer[self.element_end as usize])
                        } else {
                            None
                        };
                        
                        self.element_buffer.push(element.to_string());
                        self.start_index = self.element_end as usize + 1;
                        self.element_end = -1;

                        match terminator {
                            Some(INST_TERM) => {
                                // Instruction complete
                                if !self.element_buffer.is_empty() {
                                    let opcode = self.element_buffer.first().cloned().unwrap_or_default();
                                    let args = self.element_buffer.iter().skip(1).cloned().collect();
                                    let instruction = InstructionObject::new(opcode, args);
                                    self.instructions.push(instruction);
                                    self.element_buffer.clear();
                                }
                                
                                // Update buffer if we have more data to process
                                if self.start_index < self.buffer.len() {
                                    if let Some(pool) = &self.buffer_pool {
                                        let mut new_buf = pool.acquire();
                                        new_buf.put_slice(&self.buffer[self.start_index..]);
                                        
                                        // Release old buffer back to the pool
                                        let old_buf = std::mem::replace(&mut self.buffer, new_buf);
                                        pool.release(old_buf);
                                        
                                        self.start_index = 0;
                                    } else {
                                        self.buffer.advance(self.start_index);
                                        self.start_index = 0;
                                    }
                                } else {
                                    // Clear buffer when fully processed
                                    self.buffer.clear();
                                    self.start_index = 0;
                                    break;
                                }
                            },
                            Some(ARG_SEP) => {
                                // More elements to read
                                // Continue to next element
                            },
                            Some(c) => {
                                // Invalid terminator - this should always be an error
                                return Err(GuacdError::InvalidInstruction(
                                    format!("Endpoint {}: Error parsing instruction didn't find known terminators found: '{}'", 
                                        connection_no, c as char)
                                ));
                            },
                            None => {
                                // No terminator found
                                return Err(GuacdError::InvalidInstruction(
                                    format!("Endpoint {}: Unexpected end of data", connection_no)
                                ));
                            }
                        }
                    },
                    Err(e) => {
                        return Err(GuacdError::Utf8Error(e));
                    }
                }
            } else {
                // Not enough data yet to process this element
                break;
            }
        }

        Ok(())
    }

    /// Get the next parsed instruction
    pub fn get_instruction(&mut self) -> Option<InstructionObject> {
        if !self.instructions.is_empty() {
            Some(self.instructions.remove(0))
        } else {
            None
        }
    }

    /// Encode an instruction to send to guacd
    pub fn guacd_encode(opcode: &str, args: &[&str], pool: Option<&BufferPool>) -> Bytes {
        // Estimate the capacity needed - typical instruction is: "4.size,1.0,3.800,3.600;"
        // Length + separator + value + separator for each element + terminator
        let est_capacity = opcode.len() + args.iter().map(|a| a.len() + 10).sum::<usize>() + 20;
        
        // Use buffer pool if provided, otherwise create a new buffer
        let mut buffer = if let Some(p) = pool {
            let mut buf = p.acquire();
            buf.reserve(est_capacity);
            buf
        } else {
            BytesMut::with_capacity(est_capacity)
        };
        
        // Add the opcode
        buffer.extend_from_slice(opcode.len().to_string().as_bytes());
        buffer.put_u8(ELEM_SEP);
        buffer.extend_from_slice(opcode.as_bytes());
        
        // Add each argument
        for arg in args {
            buffer.put_u8(ARG_SEP);
            buffer.extend_from_slice(arg.len().to_string().as_bytes());
            buffer.put_u8(ELEM_SEP);
            buffer.extend_from_slice(arg.as_bytes());
        }
        
        // Add terminator
        buffer.put_u8(INST_TERM);
        
        // Convert to Bytes
        let bytes = buffer.freeze();
        
        // If we used our own buffer (not from pool), we're done
        if pool.is_none() {
            return bytes;
        }
        
        // If we used a pool buffer, we need to create a copy before release
        // This is unfortunate but necessary since we can't know if the caller will hold onto the Bytes
        let result = Bytes::copy_from_slice(&bytes);
        
        // Return the buffer to the pool
        if let Some(p) = pool {
            p.release(bytes.into());
        }
        
        result
    }

    /// Decode a complete instruction from bytes
    pub fn guacd_decode(instruction: &[u8]) -> Result<Option<InstructionObject>, GuacdError> {
        if instruction.is_empty() {
            return Ok(None);
        }
        
        if instruction[instruction.len() - 1] != INST_TERM {
            return Err(GuacdError::InvalidInstruction("Instruction termination not found".to_string()));
        }
        
        let args = Self::decode_instruction(instruction)?;
        if args.is_empty() {
            return Ok(None);
        }
        
        let opcode = args[0].clone();
        let args = args.into_iter().skip(1).collect();
        
        Ok(Some(InstructionObject::new(opcode, args)))
    }
    
    /// Helper method to recursively decode an instruction
    fn decode_instruction(instruction: &[u8]) -> Result<Vec<String>, GuacdError> {
        if instruction.is_empty() || instruction[instruction.len() - 1] != INST_TERM {
            return Err(GuacdError::InvalidInstruction("Instruction termination not found".to_string()));
        }
        
        // Find the element separator (.)
        let mut sep_pos = None;
        for (i, &b) in instruction.iter().enumerate() {
            if b == ELEM_SEP {
                sep_pos = Some(i);
                break;
            }
        }
        
        let sep_pos = match sep_pos {
            Some(pos) => pos,
            None => return Err(GuacdError::InvalidInstruction("Element separator not found".to_string())),
        };
        
        // Parse argument length
        let arg_size = match str::from_utf8(&instruction[0..sep_pos]) {
            Ok(size_str) => match size_str.parse::<usize>() {
                Ok(size) => size,
                Err(_) => return Err(GuacdError::InvalidInstruction(
                    "Invalid arg length. Possibly due to missing element separator!".to_string()
                )),
            },
            Err(e) => return Err(GuacdError::Utf8Error(e)),
        };
        
        // Extract the argument and the remaining part
        let remaining = &instruction[(sep_pos + 1)..];
        if arg_size > remaining.len() {
            return Err(GuacdError::InvalidInstruction("Argument length exceeds available data".to_string()));
        }
        
        // Convert the argument to a string
        let arg_str = match str::from_utf8(&remaining[..arg_size]) {
            Ok(s) => s.to_string(),
            Err(e) => return Err(GuacdError::Utf8Error(e)),
        };
        
        let remaining = &remaining[arg_size..];
        
        let mut args = vec![arg_str];
        
        // Process the remaining part
        if !remaining.is_empty() && remaining[0] == ARG_SEP {
            // More arguments follow
            let next_args = Self::decode_instruction(&remaining[1..])?;
            args.extend(next_args);
        } else if remaining.len() == 1 && remaining[0] == INST_TERM {
            // End of instruction
        } else {
            return Err(GuacdError::InvalidInstruction(
                format!("Instruction arg has invalid length")
            ));
        }
        
        Ok(args)
    }
    
    /// Parse a single instruction string into an InstructionObject
    pub fn get_instruction_from_string(&mut self, instruction_str: &str) -> Result<Option<InstructionObject>, GuacdError> {
        self.receive(0, instruction_str.as_bytes())?;
        Ok(self.get_instruction())
    }

    /// Clean up resources when done
    pub fn cleanup(&mut self) {
        // If using buffer pool, return the buffer to the pool
        if let Some(pool) = &self.buffer_pool {
            // Take the buffer out and replace with an empty one
            let buf = std::mem::replace(&mut self.buffer, BytesMut::new());
            pool.release(buf);
        }
    }
}

// Implement Drop to ensure proper cleanup
impl Drop for GuacdParser {
    fn drop(&mut self) {
        self.cleanup();
    }
} 