// Stream instruction formatting for Guacamole protocol
//
// Supports: audio, video, blob, end
//
// ## Standard Pattern for Sending Images (ALL handlers should use this)
//
// ```rust
// use guacr_protocol::{format_img, format_chunked_blobs};
//
// // 1. Encode image to JPEG/PNG/WebP and base64
// let image_data: Vec<u8> = encode_jpeg(...);
// let base64_data = base64::encode(&image_data);
//
// // 2. Send img instruction with metadata
// let img_instr = format_img(stream_id, 15, 0, "image/jpeg", x, y);
// to_client.send(img_instr).await?;
//
// // 3. Send blob chunks + end instruction (protocol crate handles chunking)
// let blob_instructions = format_chunked_blobs(stream_id, &base64_data, None);
// for instr in blob_instructions {
//     to_client.send(instr).await?;
// }
//
// // 4. Increment stream_id for next image
// stream_id += 1;
// ```

use crate::format_instruction;

/// Format `audio` instruction - Audio stream
///
/// Format: `5.audio,{stream},{mimetype};`
///
/// # Arguments
/// - `stream`: Stream index
/// - `mimetype`: MIME type (e.g., "audio/wav", "audio/ogg")
pub fn format_audio(stream: u32, mimetype: &str) -> String {
    let stream_str = stream.to_string();
    format_instruction("audio", &[&stream_str, mimetype])
}

/// Format `blob` instruction - Stream data chunk
///
/// Format: `4.blob,{stream},{length}.{data};`
///
/// # Arguments
/// - `stream`: Stream index
/// - `data`: Base64-encoded data chunk
pub fn format_blob(stream: u32, data: &str) -> String {
    let stream_str = stream.to_string();
    // blob instruction format: blob,<stream_len>.<stream>,<data_len>.<data>;
    format!(
        "4.blob,{}.{},{}.{};",
        stream_str.len(),
        stream_str,
        data.len(),
        data
    )
}

/// Format `end` instruction - End stream
///
/// Format: `3.end,{stream};`
///
/// # Arguments
/// - `stream`: Stream index to close
pub fn format_end(stream: u32) -> String {
    let stream_str = stream.to_string();
    format_instruction("end", &[&stream_str])
}

/// Format `video` instruction - Video stream
///
/// Format: `5.video,{stream},{mimetype};`
///
/// # Arguments
/// - `stream`: Stream index
/// - `mimetype`: MIME type (e.g., "video/mp4", "video/webm")
pub fn format_video(stream: u32, mimetype: &str) -> String {
    let stream_str = stream.to_string();
    format_instruction("video", &[&stream_str, mimetype])
}

/// Helper: Format complete audio stream (BEL beep)
///
/// Returns a vector of instructions: [audio, blob, end]
pub fn format_bell_audio(stream: u32) -> Vec<String> {
    vec![
        format_audio(stream, "audio/wav"),
        // Empty blob (client will generate beep)
        format_blob(stream, ""),
        format_end(stream),
    ]
}

/// Helper: Send large base64-encoded data as chunked blobs
///
/// This is the standard pattern for sending large images/video/audio over WebRTC.
/// WebRTC data channels have a 64KB limit, so we chunk at 6KB for safety.
///
/// Pattern:
/// 1. Send `img` instruction with metadata (caller's responsibility)
/// 2. Send base64 data as multiple `blob` instructions (this function)
/// 3. Send `end` instruction to close stream (this function)
///
/// # Arguments
/// - `stream`: Stream index
/// - `base64_data`: Base64-encoded data to send
/// - `chunk_size`: Size of each blob chunk (default: 6144 bytes = 6KB)
///
/// # Returns
/// Vector of blob instructions ready to send, ending with `end` instruction
///
/// # Example
/// ```
/// use guacr_protocol::streams::format_chunked_blobs;
///
/// // After sending img instruction with metadata:
/// let blobs = format_chunked_blobs(1, &base64_data, None);
/// for blob_instr in blobs {
///     // Send each blob instruction to client
/// }
/// ```
pub fn format_chunked_blobs(
    stream: u32,
    base64_data: &str,
    chunk_size: Option<usize>,
) -> Vec<String> {
    const DEFAULT_CHUNK_SIZE: usize = 6144; // 6KB chunks (safe for 64KB WebRTC limit)
    let chunk_size = chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);

    let total_size = base64_data.len();
    let num_chunks = total_size.div_ceil(chunk_size);

    let mut instructions = Vec::with_capacity(num_chunks + 1); // +1 for end instruction

    // Send blob chunks
    for chunk in base64_data.as_bytes().chunks(chunk_size) {
        let chunk_str = std::str::from_utf8(chunk).expect("Base64 should be valid UTF-8");
        instructions.push(format_blob(stream, chunk_str));
    }

    // Send end instruction
    instructions.push(format_end(stream));

    instructions
}

/// Format a `clipboard` instruction sequence: clipboard + blob + end
///
/// This is the standard pattern for sending clipboard data to the client.
/// Supports any MIME type (text/plain, text/html, application/octet-stream, etc.)
///
/// # Arguments
/// - `stream`: Stream index for this clipboard operation
/// - `mimetype`: MIME type of the clipboard data (e.g., "text/plain")
/// - `data`: Raw clipboard data bytes
///
/// # Returns
/// Vector of 3 instructions: [clipboard, blob (base64-encoded), end]
pub fn format_clipboard(stream: u32, mimetype: &str, data: &[u8]) -> Vec<String> {
    use base64::Engine;
    let stream_str = stream.to_string();
    let base64_data = base64::engine::general_purpose::STANDARD.encode(data);

    vec![
        format_instruction("clipboard", &[&stream_str, mimetype]),
        format_blob(stream, &base64_data),
        format_end(stream),
    ]
}

/// Format a text clipboard instruction sequence (convenience for text/plain)
///
/// Shortcut for `format_clipboard(stream, "text/plain", text.as_bytes())`
pub fn format_clipboard_text(stream: u32, text: &str) -> Vec<String> {
    format_clipboard(stream, "text/plain", text.as_bytes())
}

/// Parse clipboard blob data from a Guacamole blob instruction
///
/// Format: `4.blob,LENGTH.STREAM_ID,LENGTH.BASE64DATA;`
///
/// Returns the decoded text, or None if the instruction is not a blob
/// or the data is empty.
pub fn parse_clipboard_blob(msg: &str) -> Option<String> {
    if !msg.contains(".blob,") {
        return None;
    }

    let args_part = msg.split_once(".blob,")?.1;
    let parts: Vec<&str> = args_part.split(',').collect();
    if parts.len() < 2 {
        return None;
    }

    // parts[0] = "1.0" (stream ID), parts[1] = "44.base64data;" (length.data)
    let (_, data_part) = parts[1].split_once('.')?;
    let data_str = data_part.trim_end_matches(';');

    use base64::Engine;
    let clipboard_data = base64::engine::general_purpose::STANDARD
        .decode(data_str)
        .ok()?;
    let clipboard_text = String::from_utf8(clipboard_data).ok()?;

    if clipboard_text.is_empty() {
        None
    } else {
        Some(clipboard_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_audio() {
        let instr = format_audio(1, "audio/wav");
        assert_eq!(instr, "5.audio,1.1,9.audio/wav;");
    }

    #[test]
    fn test_format_blob() {
        let instr = format_blob(1, "dGVzdA==");
        // Format: blob,<stream_len>.<stream>,<data_len>.<data>;
        // stream=1 (len=1), data_len=8 (len=1), data="dGVzdA=="
        // Note: This matches TerminalRenderer format: 4.blob,1.1,8.dGVzdA==;
        assert_eq!(instr, "4.blob,1.1,8.dGVzdA==;");
    }

    #[test]
    fn test_format_end() {
        let instr = format_end(1);
        assert_eq!(instr, "3.end,1.1;");
    }

    #[test]
    fn test_format_video() {
        let instr = format_video(2, "video/mp4");
        assert_eq!(instr, "5.video,1.2,9.video/mp4;");
    }

    #[test]
    fn test_format_bell_audio() {
        let instrs = format_bell_audio(1);
        assert_eq!(instrs.len(), 3);
        assert!(instrs[0].contains("audio"));
        assert!(instrs[1].contains("blob"));
        assert!(instrs[2].contains("end"));
    }

    #[test]
    fn test_format_chunked_blobs_small() {
        // Small data - should be 1 blob + 1 end
        let data = "dGVzdA=="; // 8 bytes
        let instrs = format_chunked_blobs(1, data, Some(100)); // 100 byte chunks
        assert_eq!(instrs.len(), 2); // 1 blob + 1 end
        assert!(instrs[0].contains("blob"));
        assert!(instrs[0].contains(data));
        assert!(instrs[1].contains("end"));
    }

    #[test]
    fn test_format_chunked_blobs_large() {
        // Large data - should be multiple blobs + 1 end
        let data = "A".repeat(20000); // 20KB
        let instrs = format_chunked_blobs(1, &data, Some(6144)); // 6KB chunks
        assert_eq!(instrs.len(), 5); // 4 blobs + 1 end (20000 / 6144 = 3.26 -> 4 chunks)

        // Verify all but last are blobs
        for instr in instrs.iter().take(4) {
            assert!(instr.contains("blob"));
        }

        // Verify last is end
        assert!(instrs[4].contains("end"));
    }

    #[test]
    fn test_format_chunked_blobs_default_chunk_size() {
        // Test default chunk size (6KB)
        let data = "B".repeat(20000);
        let instrs = format_chunked_blobs(1, &data, None); // Use default
        assert_eq!(instrs.len(), 5); // 4 blobs + 1 end
    }

    #[test]
    fn test_format_clipboard_text() {
        let instrs = format_clipboard_text(10, "Hello");
        assert_eq!(instrs.len(), 3);
        assert!(instrs[0].contains("clipboard"));
        assert!(instrs[0].contains("text/plain"));
        assert!(instrs[1].contains("blob"));
        assert!(instrs[2].contains("end"));
    }

    #[test]
    fn test_format_clipboard_binary() {
        let instrs = format_clipboard(5, "text/html", b"<b>bold</b>");
        assert_eq!(instrs.len(), 3);
        assert!(instrs[0].contains("clipboard"));
        assert!(instrs[0].contains("text/html"));
    }

    #[test]
    fn test_parse_clipboard_blob() {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(b"Hello World");
        let msg = format!("4.blob,2.10,{}.{};", data.len(), data);
        let result = parse_clipboard_blob(&msg);
        assert_eq!(result, Some("Hello World".to_string()));
    }

    #[test]
    fn test_parse_clipboard_blob_empty() {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(b"");
        let msg = format!("4.blob,2.10,{}.{};", data.len(), data);
        let result = parse_clipboard_blob(&msg);
        assert_eq!(result, None); // Empty clipboard returns None
    }

    #[test]
    fn test_parse_clipboard_blob_not_blob() {
        let result = parse_clipboard_blob("3.key,2.65,1.1;");
        assert_eq!(result, None);
    }
}
