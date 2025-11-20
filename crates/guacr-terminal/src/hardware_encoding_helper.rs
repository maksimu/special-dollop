// Helper function to try hardware encoding first, then fall back to JPEG/PNG
// Used by SSH, Telnet, RDP, VNC, RBI protocols

use crate::hardware_encoder::{HardwareEncoder, HardwareEncoderImpl};
use bytes::Bytes;
use guacr_protocol::BinaryEncoder;
use log::debug;

/// Try hardware encoding first, fall back to JPEG/PNG
///
/// # Arguments
///
/// * `encoder` - Optional hardware encoder (None = use JPEG/PNG)
/// * `pixels` - RGBA pixels (width * height * 4 bytes)
/// * `width` - Frame width
/// * `height` - Frame height
/// * `binary_encoder` - Binary protocol encoder
/// * `stream_id` - Stream ID for video instruction
/// * `video_stream_id` - Optional video stream ID (if already initialized)
/// * `layer` - Layer number
/// * `x` - X coordinate
/// * `y` - Y coordinate
///
/// # Returns
///
/// Tuple of (messages, new_video_stream_id)
/// - messages: Vec<Bytes> of protocol messages
/// - new_video_stream_id: Updated video stream ID (if video was used)
#[allow(clippy::too_many_arguments)]
pub fn try_hardware_encode_then_fallback(
    mut encoder: Option<&mut HardwareEncoderImpl>,
    pixels: &[u8],
    width: u32,
    height: u32,
    binary_encoder: &mut BinaryEncoder,
    stream_id: u32,
    video_stream_id: Option<u32>,
    layer: i32,
    x: i32,
    y: i32,
    fallback_encode: impl FnOnce() -> Result<Bytes, String>,
) -> Result<(Vec<Bytes>, Option<u32>), String> {
    let mut messages = Vec::new();
    let mut new_video_stream_id = video_stream_id;

    // Try hardware encoding first
    if let Some(ref mut encoder) = encoder {
        match encoder.encode_frame(pixels, width, height) {
            Ok(h264_data) => {
                // Hardware encoding succeeded - send video instruction
                let vsid = if let Some(id) = new_video_stream_id {
                    id
                } else {
                    let id = stream_id;
                    new_video_stream_id = Some(id);

                    // Send video instruction (first time only)
                    let video_msg = binary_encoder.encode_video(id, layer, "video/h264");
                    messages.push(video_msg);

                    id
                };

                // Send video data via blob
                let blob_msg = binary_encoder.encode_blob(vsid, h264_data);
                messages.push(blob_msg);

                debug!("Hardware encoding: Sent H.264 frame via video instruction");
                return Ok((messages, new_video_stream_id));
            }
            Err(_) => {
                // Hardware encoding failed, fall through to JPEG/PNG
                debug!("Hardware encoding failed, falling back to JPEG/PNG");
            }
        }
    }

    // Fallback: JPEG/PNG encoding
    let image_data = fallback_encode()?;
    let img_msg = binary_encoder.encode_image(
        stream_id,
        layer,
        x,
        y,
        width as u16,
        height as u16,
        0, // format: PNG (or 1 for JPEG)
        image_data,
    );
    messages.push(img_msg);

    Ok((messages, new_video_stream_id))
}
