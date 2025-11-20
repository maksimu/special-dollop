// guacr-protocol: Guacamole protocol instruction formatting
//
// Provides utilities for formatting Guacamole protocol instructions
// according to the official Apache Guacamole protocol specification.

mod advanced;
mod binary;
mod drawing;
mod layers;
mod parser;
mod streams;
mod text_optimized;

pub use advanced::*;
pub use binary::{
    BinaryEncoder, Opcode, BINARY_PROTOCOL_OVERHEAD, FLAG_COMPRESSED, FLAG_ENCRYPTED,
    FLAG_FIRST_FRAGMENT, FLAG_FRAGMENTED, FLAG_LAST_FRAGMENT, FRAME_PROTOCOL_OVERHEAD,
    MAX_ENCODER_FRAME_SIZE, MAX_SAFE_PAYLOAD_SIZE, TOTAL_PROTOCOL_OVERHEAD,
};
pub use drawing::*;
pub use layers::*;
pub use parser::{GuacamoleParser, Instruction, ParseError};
pub use streams::*;
pub use text_optimized::TextProtocolEncoder;

/// Format a Guacamole protocol instruction
///
/// Helper function to format instructions with proper length prefixes
pub fn format_instruction(opcode: &str, args: &[&str]) -> String {
    let mut result = String::new();

    // Opcode with length prefix
    result.push_str(&opcode.len().to_string());
    result.push('.');
    result.push_str(opcode);

    // Arguments with length prefixes
    for arg in args {
        result.push(',');
        result.push_str(&arg.len().to_string());
        result.push('.');
        result.push_str(arg);
    }

    result.push(';');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_instruction() {
        let instr = format_instruction("key", &["65507", "1"]);
        assert_eq!(instr, "3.key,5.65507,1.1;");
    }

    #[test]
    fn test_format_instruction_empty_args() {
        let instr = format_instruction("sync", &[]);
        assert_eq!(instr, "4.sync;");
    }
}
