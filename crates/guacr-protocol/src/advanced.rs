// Advanced instruction formatting for Guacamole protocol
//
// Supports: transfer, nest, pipe, ack, error

use crate::format_instruction;

// ============================================================================
// Guacamole Protocol Status Codes
// ============================================================================
// These match the official Apache Guacamole protocol specification
// and are used with the `error` instruction to communicate failures to clients.

/// Internal server error (0x0100)
pub const STATUS_SERVER_ERROR: u32 = 256;

/// General upstream error (0x0200)
pub const STATUS_UPSTREAM_ERROR: u32 = 512;

/// Remote server unavailable (0x0201)
pub const STATUS_UPSTREAM_UNAVAILABLE: u32 = 513;

/// Remote server timeout (0x0202)
pub const STATUS_UPSTREAM_TIMEOUT: u32 = 514;

/// Resource not found on remote server (0x0203)
pub const STATUS_UPSTREAM_NOT_FOUND: u32 = 515;

/// Resource conflict (0x0204)
pub const STATUS_RESOURCE_CONFLICT: u32 = 516;

/// Resource closed - connection ended (0x0205)
pub const STATUS_RESOURCE_CLOSED: u32 = 517;

/// Bad request from client (0x0300)
pub const STATUS_CLIENT_BAD_REQUEST: u32 = 768;

/// Client unauthorized (0x0301)
pub const STATUS_CLIENT_UNAUTHORIZED: u32 = 769;

/// Client forbidden (0x0303)
pub const STATUS_CLIENT_FORBIDDEN: u32 = 771;

/// Client timeout (0x0304)
pub const STATUS_CLIENT_TIMEOUT: u32 = 772;

/// Client sent too much data (0x0305)
pub const STATUS_CLIENT_OVERRUN: u32 = 773;

/// Client sent bad type (0x0308)
pub const STATUS_CLIENT_BAD_TYPE: u32 = 776;

/// Too many clients (0x030D)
pub const STATUS_CLIENT_TOO_MANY: u32 = 781;

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

/// Format `error` instruction - Send error message to client
///
/// Format: `5.error,{message},{status_code};`
///
/// Guacamole protocol status codes:
/// - 512 (0x0200): UPSTREAM_ERROR - Remote server error
/// - 513 (0x0201): UPSTREAM_UNAVAILABLE - Remote server unavailable
/// - 514 (0x0202): UPSTREAM_TIMEOUT - Remote server timeout
/// - 515 (0x0203): UPSTREAM_NOT_FOUND - Resource not found
/// - 516 (0x0204): RESOURCE_CONFLICT - Resource conflict
/// - 517 (0x0205): RESOURCE_CLOSED - Resource closed
/// - 768 (0x0300): CLIENT_BAD_REQUEST - Bad request
/// - 769 (0x0301): CLIENT_UNAUTHORIZED - Unauthorized
/// - 771 (0x0303): CLIENT_FORBIDDEN - Forbidden
/// - 772 (0x0304): CLIENT_TIMEOUT - Client timeout
/// - 773 (0x0305): CLIENT_OVERRUN - Too much data
/// - 776 (0x0308): CLIENT_BAD_TYPE - Bad type
/// - 781 (0x030D): CLIENT_TOO_MANY - Too many clients
/// - 256 (0x0100): SERVER_ERROR - Internal server error
/// - 512 (0x0200): UPSTREAM_ERROR - General upstream error
///
/// # Arguments
/// - `message`: Error message to display to user
/// - `status_code`: Guacamole protocol status code
///
/// # Example
/// ```
/// use guacr_protocol::format_error;
/// let error_instr = format_error("Authentication failed", 769);
/// assert_eq!(error_instr, "5.error,21.Authentication failed,3.769;");
/// ```
pub fn format_error(message: &str, status_code: u32) -> String {
    let status_str = status_code.to_string();
    format_instruction("error", &[message, &status_str])
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

    #[test]
    fn test_format_error() {
        let instr = format_error("Authentication failed", 769);
        assert_eq!(instr, "5.error,21.Authentication failed,3.769;");

        let instr2 = format_error("Connection timeout", 514);
        assert_eq!(instr2, "5.error,18.Connection timeout,3.514;");
    }
}
