// Drawing instruction formatting for Guacamole protocol
//
// Supports: rect, cfill, line, arc, curve, shade

use crate::format_instruction;

/// Format `rect` instruction - Draw rectangle
///
/// Format: `4.rect,{layer},{x},{y},{width},{height};`
///
/// # Arguments
/// - `layer`: Layer index (typically 0 for default layer)
/// - `x`: X coordinate in pixels
/// - `y`: Y coordinate in pixels
/// - `width`: Width in pixels
/// - `height`: Height in pixels
pub fn format_rect(layer: i32, x: u32, y: u32, width: u32, height: u32) -> String {
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();
    let width_str = width.to_string();
    let height_str = height.to_string();

    format_instruction(
        "rect",
        &[&layer_str, &x_str, &y_str, &width_str, &height_str],
    )
}

/// Format `cfill` instruction - Fill with color
///
/// Format: `5.cfill,{layer},{r},{g},{b},{a};`
///
/// # Arguments
/// - `layer`: Layer index
/// - `r`: Red component (0-255)
/// - `g`: Green component (0-255)
/// - `b`: Blue component (0-255)
/// - `a`: Alpha component (0-255, typically 255 for opaque)
pub fn format_cfill(layer: i32, r: u8, g: u8, b: u8, a: u8) -> String {
    let layer_str = layer.to_string();
    let r_str = r.to_string();
    let g_str = g.to_string();
    let b_str = b.to_string();
    let a_str = a.to_string();

    format_instruction("cfill", &[&layer_str, &r_str, &g_str, &b_str, &a_str])
}

/// Format `line` instruction - Draw line
///
/// Format: `4.line,{layer},{x1},{y1},{x2},{y2};`
///
/// # Arguments
/// - `layer`: Layer index
/// - `x1`: Start X coordinate
/// - `y1`: Start Y coordinate
/// - `x2`: End X coordinate
/// - `y2`: End Y coordinate
pub fn format_line(layer: i32, x1: u32, y1: u32, x2: u32, y2: u32) -> String {
    let layer_str = layer.to_string();
    let x1_str = x1.to_string();
    let y1_str = y1.to_string();
    let x2_str = x2.to_string();
    let y2_str = y2.to_string();

    format_instruction("line", &[&layer_str, &x1_str, &y1_str, &x2_str, &y2_str])
}

/// Format `arc` instruction - Draw arc/ellipse
///
/// Format: `3.arc,{layer},{x},{y},{radius_x},{radius_y},{start_angle},{end_angle};`
///
/// # Arguments
/// - `layer`: Layer index
/// - `x`: Center X coordinate
/// - `y`: Center Y coordinate
/// - `radius_x`: Horizontal radius
/// - `radius_y`: Vertical radius
/// - `start_angle`: Start angle in radians
/// - `end_angle`: End angle in radians
pub fn format_arc(
    layer: i32,
    x: u32,
    y: u32,
    radius_x: u32,
    radius_y: u32,
    start_angle: f64,
    end_angle: f64,
) -> String {
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();
    let rx_str = radius_x.to_string();
    let ry_str = radius_y.to_string();
    let start_str = start_angle.to_string();
    let end_str = end_angle.to_string();

    format_instruction(
        "arc",
        &[
            &layer_str, &x_str, &y_str, &rx_str, &ry_str, &start_str, &end_str,
        ],
    )
}

/// Format `curve` instruction - Draw cubic Bezier curve
///
/// Format: `5.curve,{layer},{x1},{y1},{x2},{y2},{x3},{y3};`
///
/// # Arguments
/// - `layer`: Layer index
/// - `x1`, `y1`: First control point
/// - `x2`, `y2`: Second control point
/// - `x3`, `y3`: Third control point (end point)
pub fn format_curve(layer: i32, x1: u32, y1: u32, x2: u32, y2: u32, x3: u32, y3: u32) -> String {
    let layer_str = layer.to_string();
    let x1_str = x1.to_string();
    let y1_str = y1.to_string();
    let x2_str = x2.to_string();
    let y2_str = y2.to_string();
    let x3_str = x3.to_string();
    let y3_str = y3.to_string();

    format_instruction(
        "curve",
        &[
            &layer_str, &x1_str, &y1_str, &x2_str, &y2_str, &x3_str, &y3_str,
        ],
    )
}

/// Format `shade` instruction - Draw shaded rectangle (gradient)
///
/// Format: `5.shade,{layer},{x},{y},{width},{height},{r1},{g1},{b1},{a1},{r2},{g2},{b2},{a2};`
///
/// # Arguments
/// - `layer`: Layer index
/// - `x`, `y`: Top-left corner
/// - `width`, `height`: Rectangle dimensions
/// - `r1`, `g1`, `b1`, `a1`: Start color (top)
/// - `r2`, `g2`, `b2`, `a2`: End color (bottom)
#[allow(clippy::too_many_arguments)]
pub fn format_shade(
    layer: i32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    r1: u8,
    g1: u8,
    b1: u8,
    a1: u8,
    r2: u8,
    g2: u8,
    b2: u8,
    a2: u8,
) -> String {
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();
    let width_str = width.to_string();
    let height_str = height.to_string();
    let r1_str = r1.to_string();
    let g1_str = g1.to_string();
    let b1_str = b1.to_string();
    let a1_str = a1.to_string();
    let r2_str = r2.to_string();
    let g2_str = g2.to_string();
    let b2_str = b2.to_string();
    let a2_str = a2.to_string();

    format_instruction(
        "shade",
        &[
            &layer_str,
            &x_str,
            &y_str,
            &width_str,
            &height_str,
            &r1_str,
            &g1_str,
            &b1_str,
            &a1_str,
            &r2_str,
            &g2_str,
            &b2_str,
            &a2_str,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_rect() {
        let instr = format_rect(0, 10, 20, 100, 50);
        assert_eq!(instr, "4.rect,1.0,2.10,2.20,3.100,2.50;");
    }

    #[test]
    fn test_format_cfill() {
        let instr = format_cfill(0, 255, 0, 0, 255);
        assert_eq!(instr, "5.cfill,1.0,3.255,1.0,1.0,3.255;");
    }

    #[test]
    fn test_format_line() {
        let instr = format_line(0, 0, 0, 100, 100);
        assert_eq!(instr, "4.line,1.0,1.0,1.0,3.100,3.100;");
    }

    #[test]
    fn test_format_arc() {
        let instr = format_arc(0, 50, 50, 25, 25, 0.0, std::f64::consts::PI);
        assert!(instr.starts_with("3.arc,"));
        assert!(instr.contains("50"));
    }

    #[test]
    fn test_format_curve() {
        let instr = format_curve(0, 0, 0, 50, 50, 100, 100);
        assert_eq!(instr, "5.curve,1.0,1.0,1.0,2.50,2.50,3.100,3.100;");
    }

    #[test]
    fn test_format_shade() {
        let instr = format_shade(0, 0, 0, 100, 50, 255, 0, 0, 255, 0, 0, 255, 255);
        assert!(instr.starts_with("5.shade,"));
        assert!(instr.contains("255"));
    }
}
