// Helper functions for recording: opcode extraction, drawing instruction detection,
// timestamp injection.

use std::time::{SystemTime, UNIX_EPOCH};

/// Extract the opcode from a Guacamole protocol instruction string.
/// Format: "LENGTH.OPCODE,LENGTH.ARG1,...;"
pub fn extract_opcode(instr: &str) -> &str {
    if let Some(dot) = instr.find('.') {
        let after_dot = &instr[dot + 1..];
        let end = after_dot
            .find(',')
            .or_else(|| after_dot.find(';'))
            .unwrap_or(after_dot.len());
        &after_dot[..end]
    } else {
        ""
    }
}

/// Check whether an instruction is a drawing/output instruction that should
/// be excluded when `recording_exclude_output` is set.
/// Sync and layer-0 size are always kept for timing and dimensions.
pub fn is_drawing_instruction(instr: &str) -> bool {
    let opcode = extract_opcode(instr);
    matches!(
        opcode,
        "img"
            | "blob"
            | "end"
            | "copy"
            | "transfer"
            | "rect"
            | "cfill"
            | "cstroke"
            | "cursor"
            | "shade"
            | "dispose"
            | "move"
            | "audio"
            | "video"
            | "png"
            | "arc"
            | "line"
            | "close"
            | "clip"
            | "push"
            | "pop"
            | "body"
    )
}

/// Inject a Guacamole-protocol-encoded timestamp argument into an instruction.
/// Replaces the trailing ";" with ",LEN.TIMESTAMP;".
pub fn inject_timestamp(instr: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .to_string();
    let len = ts.len();
    if let Some(stripped) = instr.strip_suffix(';') {
        format!("{},{}.{};", stripped, len, ts)
    } else {
        // Malformed instruction -- append anyway
        format!("{},{}.{};", instr, len, ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_opcode_normal() {
        assert_eq!(extract_opcode("4.size,1.0,3.800,3.600;"), "size");
        assert_eq!(extract_opcode("3.img,1.0,9.image/png,1.0,1.0;"), "img");
        assert_eq!(extract_opcode("4.sync,13.1234567890123,1.0;"), "sync");
        assert_eq!(extract_opcode("3.key,2.65,1.1;"), "key");
        assert_eq!(extract_opcode("5.mouse,3.100,3.200,1.0;"), "mouse");
    }

    #[test]
    fn test_extract_opcode_no_args() {
        assert_eq!(extract_opcode("3.nop;"), "nop");
    }

    #[test]
    fn test_extract_opcode_no_dot() {
        assert_eq!(extract_opcode("malformed"), "");
    }

    #[test]
    fn test_is_drawing_instruction() {
        assert!(is_drawing_instruction("3.img,1.0,9.image/png,1.0,1.0;"));
        assert!(is_drawing_instruction("4.blob,1.1,16.SGVsbG8gV29ybGQh;"));
        assert!(is_drawing_instruction("3.end,1.1;"));
        assert!(is_drawing_instruction(
            "4.copy,1.0,1.0,1.0,3.100,3.100,1.1,1.0,1.0;"
        ));
        assert!(is_drawing_instruction("4.rect,1.0,1.0,1.0,3.100,3.100;"));
        assert!(is_drawing_instruction(
            "5.cfill,1.0,1.0,3.255,1.0,1.0,3.255;"
        ));
        assert!(is_drawing_instruction(
            "6.cursor,1.0,1.0,1.0,3.100,3.100,2.10,2.10;"
        ));
        assert!(is_drawing_instruction("5.audio,1.0,9.audio/ogg;"));
        assert!(is_drawing_instruction("5.video,1.0,9.video/mp4;"));
        assert!(is_drawing_instruction("3.png,1.0,9.image/png,1.0,1.0;"));
        assert!(is_drawing_instruction(
            "3.arc,1.0,3.100,3.100,2.50,1.0,6.3.1415,1.0;"
        ));

        // Non-drawing instructions
        assert!(!is_drawing_instruction("4.size,1.0,3.800,3.600;"));
        assert!(!is_drawing_instruction("4.sync,13.1234567890123,1.0;"));
        assert!(!is_drawing_instruction("3.key,2.65,1.1;"));
        assert!(!is_drawing_instruction("5.mouse,3.100,3.200,1.0;"));
        assert!(!is_drawing_instruction("4.name,1.0,7.Session;"));
    }

    #[test]
    fn test_inject_timestamp() {
        let instr = "5.mouse,3.100,3.200,1.0;";
        let result = inject_timestamp(instr);
        // Should end with a timestamp argument
        assert!(result.ends_with(';'));
        // Should contain original instruction content
        assert!(result.contains("5.mouse,3.100,3.200,1.0"));
        // Should have an extra comma-separated argument
        let comma_count_orig = instr.matches(',').count();
        let comma_count_new = result.matches(',').count();
        assert_eq!(comma_count_new, comma_count_orig + 1);
    }

    #[test]
    fn test_inject_timestamp_malformed() {
        let instr = "no-semicolon";
        let result = inject_timestamp(instr);
        assert!(result.ends_with(';'));
        assert!(result.contains("no-semicolon"));
    }
}
