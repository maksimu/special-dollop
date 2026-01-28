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
    // Matches format used in TerminalRenderer::format_img_instructions exactly
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
}
