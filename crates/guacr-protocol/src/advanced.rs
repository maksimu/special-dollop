// Advanced instruction formatting for Guacamole protocol
//
// Supports: transfer, nest, pipe, ack

use crate::format_instruction;

/// Format `transfer` instruction - Copy/transfer image data between layers
///
/// Format: `7.transfer,{src_layer},{src_x},{src_y},{width},{height},{function},{dst_layer},{dst_x},{dst_y};`
///
/// # Arguments
/// - `src_layer`: Source layer index
/// - `src_x`, `src_y`: Source coordinates
/// - `width`, `height`: Dimensions to transfer
/// - `function`: Transfer function (0-15, typically 12 for SRC)
/// - `dst_layer`: Destination layer index
/// - `dst_x`, `dst_y`: Destination coordinates
#[allow(clippy::too_many_arguments)]
pub fn format_transfer(
    src_layer: i32,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    function: u8,
    dst_layer: i32,
    dst_x: u32,
    dst_y: u32,
) -> String {
    let src_layer_str = src_layer.to_string();
    let src_x_str = src_x.to_string();
    let src_y_str = src_y.to_string();
    let width_str = width.to_string();
    let height_str = height.to_string();
    let function_str = function.to_string();
    let dst_layer_str = dst_layer.to_string();
    let dst_x_str = dst_x.to_string();
    let dst_y_str = dst_y.to_string();

    format_instruction(
        "transfer",
        &[
            &src_layer_str,
            &src_x_str,
            &src_y_str,
            &width_str,
            &height_str,
            &function_str,
            &dst_layer_str,
            &dst_x_str,
            &dst_y_str,
        ],
    )
}

/// Format `nest` instruction - Nest another connection
///
/// Format: `4.nest,{connection_id},{layer},{x},{y};`
///
/// # Arguments
/// - `connection_id`: Connection ID to nest
/// - `layer`: Layer index
/// - `x`, `y`: Position
pub fn format_nest(connection_id: &str, layer: i32, x: u32, y: u32) -> String {
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();

    format_instruction("nest", &[connection_id, &layer_str, &x_str, &y_str])
}

/// Format `pipe` instruction - Named pipe stream
///
/// Format: `4.pipe,{stream},{mimetype},{name};`
///
/// # Arguments
/// - `stream`: Stream index
/// - `mimetype`: MIME type
/// - `name`: Pipe name
pub fn format_pipe(stream: u32, mimetype: &str, name: &str) -> String {
    let stream_str = stream.to_string();
    format_instruction("pipe", &[&stream_str, mimetype, name])
}

/// Format `ack` instruction - Acknowledge stream data
///
/// Format: `3.ack,{stream},{message};`
///
/// # Arguments
/// - `stream`: Stream index
/// - `message`: Acknowledgment message (typically "ok" or error)
pub fn format_ack(stream: u32, message: &str) -> String {
    let stream_str = stream.to_string();
    format_instruction("ack", &[&stream_str, message])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_transfer() {
        let instr = format_transfer(0, 0, 0, 100, 50, 12, 0, 10, 20);
        // "transfer" is 8 characters
        assert!(instr.starts_with("8.transfer,"));
        assert!(instr.contains("100"));
    }

    #[test]
    fn test_format_nest() {
        let instr = format_nest("conn-123", 0, 10, 20);
        assert!(instr.starts_with("4.nest,"));
        assert!(instr.contains("conn-123"));
    }

    #[test]
    fn test_format_pipe() {
        let instr = format_pipe(1, "application/octet-stream", "mypipe");
        assert!(instr.starts_with("4.pipe,"));
        assert!(instr.contains("mypipe"));
    }

    #[test]
    fn test_format_ack() {
        let instr = format_ack(1, "ok");
        assert_eq!(instr, "3.ack,1.1,2.ok;");
    }
}
