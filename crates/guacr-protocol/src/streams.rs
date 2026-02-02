// Stream instruction formatting for Guacamole protocol
//
// Supports: audio, video, blob, end

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
}
