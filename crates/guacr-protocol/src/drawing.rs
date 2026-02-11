// Drawing instruction formatting for Guacamole protocol
//
// Supports: rect, cfill, line, arc, curve, shade, img

use crate::format_instruction;

/// Format `img` instruction - Start image stream (modern protocol)
///
/// Format: `3.img,{stream},{mask},{layer},{mimetype},{x},{y};`
///
/// This starts a modern image stream. Must be followed by `blob` chunks and `end` instruction.
/// Use `format_chunked_blobs` from the streams module to send the data.
///
/// # Arguments
/// - `stream`: Stream ID (must be unique per image)
/// - `mask`: Channel mask (15 = RGBA, 3 = RGB)
/// - `layer`: Layer index
/// - `mimetype`: MIME type (e.g., "image/jpeg", "image/png")
/// - `x`: X coordinate
/// - `y`: Y coordinate
pub fn format_img(stream: u32, mask: u32, layer: i32, mimetype: &str, x: i32, y: i32) -> String {
    let stream_str = stream.to_string();
    let mask_str = mask.to_string();
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();

    format_instruction(
        "img",
        &[&stream_str, &mask_str, &layer_str, mimetype, &x_str, &y_str],
    )
}

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

/// Format `cfill` instruction - Fill current path with color
///
/// Format: `5.cfill,{mask},{layer},{r},{g},{b},{a};`
///
/// # Arguments
/// - `mask`: Compositing operation (14 = GUAC_COMP_OVER, 12 = GUAC_COMP_SRC)
/// - `layer`: Layer index
/// - `r`: Red component (0-255)
/// - `g`: Green component (0-255)
/// - `b`: Blue component (0-255)
/// - `a`: Alpha component (0-255, typically 255 for opaque)
pub fn format_cfill(mask: u32, layer: i32, r: u8, g: u8, b: u8, a: u8) -> String {
    let mask_str = mask.to_string();
    let layer_str = layer.to_string();
    let r_str = r.to_string();
    let g_str = g.to_string();
    let b_str = b.to_string();
    let a_str = a.to_string();

    format_instruction(
        "cfill",
        &[&mask_str, &layer_str, &r_str, &g_str, &b_str, &a_str],
    )
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

/// Format `jpeg` instruction - Display JPEG image (legacy format)
///
/// Format: `4.jpeg,{mask},{layer},{x},{y},{base64_data};`
///
/// # Arguments
/// - `mask`: Channel mask (15 for RGBA, 7 for RGB)
/// - `layer`: Layer index (typically 0 for default layer)
/// - `x`: X coordinate in pixels
/// - `y`: Y coordinate in pixels
/// - `base64_data`: Base64-encoded JPEG image data
///
/// # Note
/// This is the legacy format that passes the entire base64-encoded image
/// directly in the instruction. Use this instead of img + blob + end for
/// compatibility with older clients.
pub fn format_jpeg(mask: u32, layer: i32, x: i32, y: i32, base64_data: &str) -> String {
    let mask_str = mask.to_string();
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();

    format_instruction(
        "jpeg",
        &[&mask_str, &layer_str, &x_str, &y_str, base64_data],
    )
}

/// Format `png` instruction - Display PNG image (legacy format)
///
/// Format: `3.png,{mask},{layer},{x},{y},{base64_data};`
///
/// # Arguments
/// - `mask`: Channel mask (15 for RGBA, 7 for RGB)
/// - `layer`: Layer index (typically 0 for default layer)
/// - `x`: X coordinate in pixels
/// - `y`: Y coordinate in pixels
/// - `base64_data`: Base64-encoded PNG image data
///
/// # Note
/// This is the legacy format that passes the entire base64-encoded image
/// directly in the instruction. Use this instead of img + blob + end for
/// compatibility with older clients.
pub fn format_png(mask: u32, layer: i32, x: i32, y: i32, base64_data: &str) -> String {
    let mask_str = mask.to_string();
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();

    format_instruction("png", &[&mask_str, &layer_str, &x_str, &y_str, base64_data])
}

/// Format `webp` instruction - Display WebP image (legacy format)
///
/// Format: `4.webp,{mask},{layer},{x},{y},{base64_data};`
///
/// # Arguments
/// - `mask`: Channel mask (15 for RGBA, 7 for RGB)
/// - `layer`: Layer index (typically 0 for default layer)
/// - `x`: X coordinate in pixels
/// - `y`: Y coordinate in pixels
/// - `base64_data`: Base64-encoded WebP image data
///
/// # Note
/// This is the legacy format that passes the entire base64-encoded image
/// directly in the instruction. Use this instead of img + blob + end for
/// compatibility with older clients.
pub fn format_webp(mask: u32, layer: i32, x: i32, y: i32, base64_data: &str) -> String {
    let mask_str = mask.to_string();
    let layer_str = layer.to_string();
    let x_str = x.to_string();
    let y_str = y.to_string();

    format_instruction(
        "webp",
        &[&mask_str, &layer_str, &x_str, &y_str, base64_data],
    )
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

/// Format `copy` instruction - Copy pixels between layers
///
/// Format: `4.copy,{srclayer},{srcx},{srcy},{srcw},{srch},{mask},{dstlayer},{dstx},{dsty};`
///
/// The JS client handles this as a canvas drawImage operation, which is GPU-accelerated.
/// This is extremely cheap (~50 bytes) compared to re-encoding and transmitting image data.
///
/// # Arguments
/// - `src_layer`: Source layer index (0 = default layer)
/// - `src_x`: Source X coordinate
/// - `src_y`: Source Y coordinate
/// - `width`: Width of region to copy
/// - `height`: Height of region to copy
/// - `mask`: Compositing operation (12 = GUAC_COMP_SRC, 14 = GUAC_COMP_OVER)
/// - `dst_layer`: Destination layer index (0 = default layer)
/// - `dst_x`: Destination X coordinate
/// - `dst_y`: Destination Y coordinate
#[allow(clippy::too_many_arguments)]
pub fn format_copy(
    src_layer: i32,
    src_x: u32,
    src_y: u32,
    width: u32,
    height: u32,
    mask: u32,
    dst_layer: i32,
    dst_x: u32,
    dst_y: u32,
) -> String {
    let src_layer_str = src_layer.to_string();
    let src_x_str = src_x.to_string();
    let src_y_str = src_y.to_string();
    let width_str = width.to_string();
    let height_str = height.to_string();
    let mask_str = mask.to_string();
    let dst_layer_str = dst_layer.to_string();
    let dst_x_str = dst_x.to_string();
    let dst_y_str = dst_y.to_string();

    format_instruction(
        "copy",
        &[
            &src_layer_str,
            &src_x_str,
            &src_y_str,
            &width_str,
            &height_str,
            &mask_str,
            &dst_layer_str,
            &dst_x_str,
            &dst_y_str,
        ],
    )
}

/// Format `cursor` instruction - Set client cursor
///
/// Format: `6.cursor,{x},{y},{srclayer},{srcx},{srcy},{srcwidth},{srcheight};`
///
/// Sets the client's cursor to the image data from the specified rectangle of a layer,
/// with the specified hotspot coordinates.
///
/// # Arguments
/// - `hotspot_x`: X coordinate of the cursor's hotspot (click point)
/// - `hotspot_y`: Y coordinate of the cursor's hotspot (click point)
/// - `src_layer`: Layer index to copy cursor image from
/// - `src_x`: X coordinate of upper-left corner of source rectangle
/// - `src_y`: Y coordinate of upper-left corner of source rectangle
/// - `src_width`: Width of the cursor image
/// - `src_height`: Height of the cursor image
///
/// # Example
/// ```
/// use guacr_protocol::format_cursor;
/// // Set cursor to 32x32 image from layer 1 at (0,0), with hotspot at (16,16)
/// let instr = format_cursor(16, 16, 1, 0, 0, 32, 32);
/// assert_eq!(instr, "6.cursor,2.16,2.16,1.1,1.0,1.0,2.32,2.32;");
/// ```
pub fn format_cursor(
    hotspot_x: i32,
    hotspot_y: i32,
    src_layer: i32,
    src_x: i32,
    src_y: i32,
    src_width: u32,
    src_height: u32,
) -> String {
    let hotspot_x_str = hotspot_x.to_string();
    let hotspot_y_str = hotspot_y.to_string();
    let src_layer_str = src_layer.to_string();
    let src_x_str = src_x.to_string();
    let src_y_str = src_y.to_string();
    let src_width_str = src_width.to_string();
    let src_height_str = src_height.to_string();

    format_instruction(
        "cursor",
        &[
            &hotspot_x_str,
            &hotspot_y_str,
            &src_layer_str,
            &src_x_str,
            &src_y_str,
            &src_width_str,
            &src_height_str,
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
        let instr = format_cfill(14, 0, 255, 0, 0, 255);
        assert_eq!(instr, "5.cfill,2.14,1.0,3.255,1.0,1.0,3.255;");
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

    #[test]
    fn test_format_copy() {
        let instr = format_copy(0, 10, 20, 100, 50, 12, 0, 30, 40);
        assert_eq!(instr, "4.copy,1.0,2.10,2.20,3.100,2.50,2.12,1.0,2.30,2.40;");
    }

    #[test]
    fn test_format_img() {
        let instr = format_img(42, 15, 0, "image/jpeg", 100, 200);
        assert_eq!(instr, "3.img,2.42,2.15,1.0,10.image/jpeg,3.100,3.200;");
    }
}
