// Layer management instruction formatting for Guacamole protocol
//
// Supports: dispose, move

use crate::format_instruction;

/// Format `dispose` instruction - Dispose/destroy layer
///
/// Format: `6.dispose,{layer};`
///
/// # Arguments
/// - `layer`: Layer index to dispose
pub fn format_dispose(layer: i32) -> String {
    let layer_str = layer.to_string();
    format_instruction("dispose", &[&layer_str])
}

/// Format `move` instruction - Move layer
///
/// Format: `4.move,{layer},{parent},{x},{y},{z};`
///
/// # Arguments
/// - `layer`: Layer index to move
/// - `parent`: Parent layer index (or -1 for default)
/// - `x`: New X coordinate
/// - `y`: New Y coordinate
/// - `z`: Z-order (stacking order)
pub fn format_move(layer: i32, parent: i32, x: u32, y: u32, z: i32) -> String {
    let layer_str = layer.to_string();
    let parent_str = parent.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();
    let z_str = z.to_string();

    format_instruction("move", &[&layer_str, &parent_str, &x_str, &y_str, &z_str])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_dispose() {
        let instr = format_dispose(0);
        // "dispose" is 7 characters, but instruction format is: <opcode_len>.<opcode>,...
        // So it's "7.dispose,1.0;"
        assert_eq!(instr, "7.dispose,1.0;");
    }

    #[test]
    fn test_format_move() {
        let instr = format_move(1, -1, 10, 20, 0);
        assert_eq!(instr, "4.move,1.1,2.-1,2.10,2.20,1.0;");
    }
}
