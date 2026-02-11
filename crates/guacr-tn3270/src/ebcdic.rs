//! EBCDIC codec for TN3270 mainframe terminal access.
//!
//! Provides conversion between EBCDIC byte sequences and Unicode characters
//! for the three most common code pages used in TN3270 environments:
//!
//! - CP 037: US/Canada (most common for TN3270)
//! - CP 500: International (CECP for Latin-1 countries)
//! - CP 1047: Unix on z/OS (Open Systems)
//!
//! All lookup tables are derived from IBM's character set references.

/// Supported EBCDIC code pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePage {
    /// US/Canada - the most common code page for TN3270 sessions.
    Cp037,
    /// International (CECP) - used in Latin-1 countries.
    Cp500,
    /// Unix on z/OS - used by Open Systems on IBM mainframes.
    Cp1047,
}

/// EBCDIC space character (0x40 in all supported code pages).
pub const EBCDIC_SPACE: u8 = 0x40;

/// EBCDIC null character (0x00 in all supported code pages).
pub const EBCDIC_NULL: u8 = 0x00;

/// EBCDIC to Unicode lookup table for Code Page 037 (US/Canada).
///
/// Each index corresponds to an EBCDIC byte value; the value at that index
/// is the corresponding Unicode character. Derived from IBM character data
/// tables for CCSID 037.
#[rustfmt::skip]
pub const EBCDIC_TO_UNICODE_037: [char; 256] = [
    // 0x00-0x0F
    '\u{0000}', '\u{0001}', '\u{0002}', '\u{0003}', '\u{009C}', '\u{0009}', '\u{0086}', '\u{007F}',
    '\u{0097}', '\u{008D}', '\u{008E}', '\u{000B}', '\u{000C}', '\u{000D}', '\u{000E}', '\u{000F}',
    // 0x10-0x1F
    '\u{0010}', '\u{0011}', '\u{0012}', '\u{0013}', '\u{009D}', '\u{0085}', '\u{0008}', '\u{0087}',
    '\u{0018}', '\u{0019}', '\u{0092}', '\u{008F}', '\u{001C}', '\u{001D}', '\u{001E}', '\u{001F}',
    // 0x20-0x2F
    '\u{0080}', '\u{0081}', '\u{0082}', '\u{0083}', '\u{0084}', '\u{000A}', '\u{0017}', '\u{001B}',
    '\u{0088}', '\u{0089}', '\u{008A}', '\u{008B}', '\u{008C}', '\u{0005}', '\u{0006}', '\u{0007}',
    // 0x30-0x3F
    '\u{0090}', '\u{0091}', '\u{0016}', '\u{0093}', '\u{0094}', '\u{0095}', '\u{0096}', '\u{0004}',
    '\u{0098}', '\u{0099}', '\u{009A}', '\u{009B}', '\u{0014}', '\u{0015}', '\u{009E}', '\u{001A}',
    // 0x40-0x4F
    '\u{0020}', '\u{00A0}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E1}', '\u{00E3}', '\u{00E5}',
    '\u{00E7}', '\u{00F1}', '\u{00A2}', '\u{002E}', '\u{003C}', '\u{0028}', '\u{002B}', '\u{007C}',
    // 0x50-0x5F
    '\u{0026}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{00EC}', '\u{00DF}', '\u{0021}', '\u{0024}', '\u{002A}', '\u{0029}', '\u{003B}', '\u{00AC}',
    // 0x60-0x6F
    '\u{002D}', '\u{002F}', '\u{00C2}', '\u{00C4}', '\u{00C0}', '\u{00C1}', '\u{00C3}', '\u{00C5}',
    '\u{00C7}', '\u{00D1}', '\u{00A6}', '\u{002C}', '\u{0025}', '\u{005F}', '\u{003E}', '\u{003F}',
    // 0x70-0x7F
    '\u{00F8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{00CC}', '\u{0060}', '\u{003A}', '\u{0023}', '\u{0040}', '\u{0027}', '\u{003D}', '\u{0022}',
    // 0x80-0x8F
    '\u{00D8}', '\u{0061}', '\u{0062}', '\u{0063}', '\u{0064}', '\u{0065}', '\u{0066}', '\u{0067}',
    '\u{0068}', '\u{0069}', '\u{00AB}', '\u{00BB}', '\u{00F0}', '\u{00FD}', '\u{00FE}', '\u{00B1}',
    // 0x90-0x9F
    '\u{00B0}', '\u{006A}', '\u{006B}', '\u{006C}', '\u{006D}', '\u{006E}', '\u{006F}', '\u{0070}',
    '\u{0071}', '\u{0072}', '\u{00AA}', '\u{00BA}', '\u{00E6}', '\u{00B8}', '\u{00C6}', '\u{00A4}',
    // 0xA0-0xAF
    '\u{00B5}', '\u{007E}', '\u{0073}', '\u{0074}', '\u{0075}', '\u{0076}', '\u{0077}', '\u{0078}',
    '\u{0079}', '\u{007A}', '\u{00A1}', '\u{00BF}', '\u{00D0}', '\u{00DD}', '\u{00DE}', '\u{00AE}',
    // 0xB0-0xBF
    '\u{005E}', '\u{00A3}', '\u{00A5}', '\u{00B7}', '\u{00A9}', '\u{00A7}', '\u{00B6}', '\u{00BC}',
    '\u{00BD}', '\u{00BE}', '\u{005B}', '\u{005D}', '\u{00AF}', '\u{00A8}', '\u{00B4}', '\u{00D7}',
    // 0xC0-0xCF
    '\u{007B}', '\u{0041}', '\u{0042}', '\u{0043}', '\u{0044}', '\u{0045}', '\u{0046}', '\u{0047}',
    '\u{0048}', '\u{0049}', '\u{00AD}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00F3}', '\u{00F5}',
    // 0xD0-0xDF
    '\u{007D}', '\u{004A}', '\u{004B}', '\u{004C}', '\u{004D}', '\u{004E}', '\u{004F}', '\u{0050}',
    '\u{0051}', '\u{0052}', '\u{00B9}', '\u{00FB}', '\u{00FC}', '\u{00F9}', '\u{00FA}', '\u{00FF}',
    // 0xE0-0xEF
    '\u{005C}', '\u{00F7}', '\u{0053}', '\u{0054}', '\u{0055}', '\u{0056}', '\u{0057}', '\u{0058}',
    '\u{0059}', '\u{005A}', '\u{00B2}', '\u{00D4}', '\u{00D6}', '\u{00D2}', '\u{00D3}', '\u{00D5}',
    // 0xF0-0xFF
    '\u{0030}', '\u{0031}', '\u{0032}', '\u{0033}', '\u{0034}', '\u{0035}', '\u{0036}', '\u{0037}',
    '\u{0038}', '\u{0039}', '\u{00B3}', '\u{00DB}', '\u{00DC}', '\u{00D9}', '\u{00DA}', '\u{009F}',
];

/// EBCDIC to Unicode lookup table for Code Page 500 (International).
///
/// CP 500 differs from CP 037 primarily in the positions of several
/// punctuation characters: brackets, exclamation, caret, tilde, braces,
/// backslash, pipe, cent sign, and not sign are rearranged.
#[rustfmt::skip]
pub const EBCDIC_TO_UNICODE_500: [char; 256] = [
    // 0x00-0x0F
    '\u{0000}', '\u{0001}', '\u{0002}', '\u{0003}', '\u{009C}', '\u{0009}', '\u{0086}', '\u{007F}',
    '\u{0097}', '\u{008D}', '\u{008E}', '\u{000B}', '\u{000C}', '\u{000D}', '\u{000E}', '\u{000F}',
    // 0x10-0x1F
    '\u{0010}', '\u{0011}', '\u{0012}', '\u{0013}', '\u{009D}', '\u{0085}', '\u{0008}', '\u{0087}',
    '\u{0018}', '\u{0019}', '\u{0092}', '\u{008F}', '\u{001C}', '\u{001D}', '\u{001E}', '\u{001F}',
    // 0x20-0x2F
    '\u{0080}', '\u{0081}', '\u{0082}', '\u{0083}', '\u{0084}', '\u{000A}', '\u{0017}', '\u{001B}',
    '\u{0088}', '\u{0089}', '\u{008A}', '\u{008B}', '\u{008C}', '\u{0005}', '\u{0006}', '\u{0007}',
    // 0x30-0x3F
    '\u{0090}', '\u{0091}', '\u{0016}', '\u{0093}', '\u{0094}', '\u{0095}', '\u{0096}', '\u{0004}',
    '\u{0098}', '\u{0099}', '\u{009A}', '\u{009B}', '\u{0014}', '\u{0015}', '\u{009E}', '\u{001A}',
    // 0x40-0x4F: differs at 0x4A ([ vs cent), 0x4F (! vs |)
    '\u{0020}', '\u{00A0}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E1}', '\u{00E3}', '\u{00E5}',
    '\u{00E7}', '\u{00F1}', '\u{005B}', '\u{002E}', '\u{003C}', '\u{0028}', '\u{002B}', '\u{0021}',
    // 0x50-0x5F: differs at 0x5A (] vs !), 0x5F (^ vs not-sign)
    '\u{0026}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{00EC}', '\u{00DF}', '\u{005D}', '\u{0024}', '\u{002A}', '\u{0029}', '\u{003B}', '\u{005E}',
    // 0x60-0x6F
    '\u{002D}', '\u{002F}', '\u{00C2}', '\u{00C4}', '\u{00C0}', '\u{00C1}', '\u{00C3}', '\u{00C5}',
    '\u{00C7}', '\u{00D1}', '\u{00A6}', '\u{002C}', '\u{0025}', '\u{005F}', '\u{003E}', '\u{003F}',
    // 0x70-0x7F
    '\u{00F8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{00CC}', '\u{0060}', '\u{003A}', '\u{0023}', '\u{0040}', '\u{0027}', '\u{003D}', '\u{0022}',
    // 0x80-0x8F
    '\u{00D8}', '\u{0061}', '\u{0062}', '\u{0063}', '\u{0064}', '\u{0065}', '\u{0066}', '\u{0067}',
    '\u{0068}', '\u{0069}', '\u{00AB}', '\u{00BB}', '\u{00F0}', '\u{00FD}', '\u{00FE}', '\u{00B1}',
    // 0x90-0x9F
    '\u{00B0}', '\u{006A}', '\u{006B}', '\u{006C}', '\u{006D}', '\u{006E}', '\u{006F}', '\u{0070}',
    '\u{0071}', '\u{0072}', '\u{00AA}', '\u{00BA}', '\u{00E6}', '\u{00B8}', '\u{00C6}', '\u{00A4}',
    // 0xA0-0xAF
    '\u{00B5}', '\u{007E}', '\u{0073}', '\u{0074}', '\u{0075}', '\u{0076}', '\u{0077}', '\u{0078}',
    '\u{0079}', '\u{007A}', '\u{00A1}', '\u{00BF}', '\u{00D0}', '\u{00DD}', '\u{00DE}', '\u{00AE}',
    // 0xB0-0xBF: differs at 0xB0 (cent vs ^), 0xBA (not vs [), 0xBB (| vs ])
    '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{00B7}', '\u{00A9}', '\u{00A7}', '\u{00B6}', '\u{00BC}',
    '\u{00BD}', '\u{00BE}', '\u{00AC}', '\u{007C}', '\u{00AF}', '\u{00A8}', '\u{00B4}', '\u{00D7}',
    // 0xC0-0xCF
    '\u{007B}', '\u{0041}', '\u{0042}', '\u{0043}', '\u{0044}', '\u{0045}', '\u{0046}', '\u{0047}',
    '\u{0048}', '\u{0049}', '\u{00AD}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00F3}', '\u{00F5}',
    // 0xD0-0xDF
    '\u{007D}', '\u{004A}', '\u{004B}', '\u{004C}', '\u{004D}', '\u{004E}', '\u{004F}', '\u{0050}',
    '\u{0051}', '\u{0052}', '\u{00B9}', '\u{00FB}', '\u{00FC}', '\u{00F9}', '\u{00FA}', '\u{00FF}',
    // 0xE0-0xEF
    '\u{005C}', '\u{00F7}', '\u{0053}', '\u{0054}', '\u{0055}', '\u{0056}', '\u{0057}', '\u{0058}',
    '\u{0059}', '\u{005A}', '\u{00B2}', '\u{00D4}', '\u{00D6}', '\u{00D2}', '\u{00D3}', '\u{00D5}',
    // 0xF0-0xFF
    '\u{0030}', '\u{0031}', '\u{0032}', '\u{0033}', '\u{0034}', '\u{0035}', '\u{0036}', '\u{0037}',
    '\u{0038}', '\u{0039}', '\u{00B3}', '\u{00DB}', '\u{00DC}', '\u{00D9}', '\u{00DA}', '\u{009F}',
];

/// EBCDIC to Unicode lookup table for Code Page 1047 (Unix on z/OS).
///
/// CP 1047 is designed for Unix environments on z/OS. Key differences from
/// CP 037: LF at 0x15 (instead of NEL), caret at 0x5F, brackets and other
/// punctuation repositioned to better support Unix conventions.
#[rustfmt::skip]
pub const EBCDIC_TO_UNICODE_1047: [char; 256] = [
    // 0x00-0x0F
    '\u{0000}', '\u{0001}', '\u{0002}', '\u{0003}', '\u{009C}', '\u{0009}', '\u{0086}', '\u{007F}',
    '\u{0097}', '\u{008D}', '\u{008E}', '\u{000B}', '\u{000C}', '\u{000D}', '\u{000E}', '\u{000F}',
    // 0x10-0x1F: 0x15 = LF (Unix newline) instead of NEL
    '\u{0010}', '\u{0011}', '\u{0012}', '\u{0013}', '\u{009D}', '\u{000A}', '\u{0008}', '\u{0087}',
    '\u{0018}', '\u{0019}', '\u{0092}', '\u{008F}', '\u{001C}', '\u{001D}', '\u{001E}', '\u{001F}',
    // 0x20-0x2F: 0x25 = NEL (moved from 0x15)
    '\u{0080}', '\u{0081}', '\u{0082}', '\u{0083}', '\u{0084}', '\u{0085}', '\u{0017}', '\u{001B}',
    '\u{0088}', '\u{0089}', '\u{008A}', '\u{008B}', '\u{008C}', '\u{0005}', '\u{0006}', '\u{0007}',
    // 0x30-0x3F
    '\u{0090}', '\u{0091}', '\u{0016}', '\u{0093}', '\u{0094}', '\u{0095}', '\u{0096}', '\u{0004}',
    '\u{0098}', '\u{0099}', '\u{009A}', '\u{009B}', '\u{0014}', '\u{0015}', '\u{009E}', '\u{001A}',
    // 0x40-0x4F
    '\u{0020}', '\u{00A0}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E1}', '\u{00E3}', '\u{00E5}',
    '\u{00E7}', '\u{00F1}', '\u{00A2}', '\u{002E}', '\u{003C}', '\u{0028}', '\u{002B}', '\u{007C}',
    // 0x50-0x5F: 0x5F = ^ (caret) instead of not-sign
    '\u{0026}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{00EC}', '\u{00DF}', '\u{0021}', '\u{0024}', '\u{002A}', '\u{0029}', '\u{003B}', '\u{005E}',
    // 0x60-0x6F
    '\u{002D}', '\u{002F}', '\u{00C2}', '\u{00C4}', '\u{00C0}', '\u{00C1}', '\u{00C3}', '\u{00C5}',
    '\u{00C7}', '\u{00D1}', '\u{00A6}', '\u{002C}', '\u{0025}', '\u{005F}', '\u{003E}', '\u{003F}',
    // 0x70-0x7F
    '\u{00F8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{00CC}', '\u{0060}', '\u{003A}', '\u{0023}', '\u{0040}', '\u{0027}', '\u{003D}', '\u{0022}',
    // 0x80-0x8F
    '\u{00D8}', '\u{0061}', '\u{0062}', '\u{0063}', '\u{0064}', '\u{0065}', '\u{0066}', '\u{0067}',
    '\u{0068}', '\u{0069}', '\u{00AB}', '\u{00BB}', '\u{00F0}', '\u{00FD}', '\u{00FE}', '\u{00B1}',
    // 0x90-0x9F
    '\u{00B0}', '\u{006A}', '\u{006B}', '\u{006C}', '\u{006D}', '\u{006E}', '\u{006F}', '\u{0070}',
    '\u{0071}', '\u{0072}', '\u{00AA}', '\u{00BA}', '\u{00E6}', '\u{00B8}', '\u{00C6}', '\u{00A4}',
    // 0xA0-0xAF: 0xAD = [ (bracket), differs from CP 037
    '\u{00B5}', '\u{007E}', '\u{0073}', '\u{0074}', '\u{0075}', '\u{0076}', '\u{0077}', '\u{0078}',
    '\u{0079}', '\u{007A}', '\u{00A1}', '\u{00BF}', '\u{00D0}', '\u{005B}', '\u{00DE}', '\u{00AE}',
    // 0xB0-0xBF: 0xB0 = not-sign, 0xBA = Y-acute, 0xBD = ] (bracket)
    '\u{00AC}', '\u{00A3}', '\u{00A5}', '\u{00B7}', '\u{00A9}', '\u{00A7}', '\u{00B6}', '\u{00BC}',
    '\u{00BD}', '\u{00BE}', '\u{00DD}', '\u{00A8}', '\u{00AF}', '\u{005D}', '\u{00B4}', '\u{00D7}',
    // 0xC0-0xCF
    '\u{007B}', '\u{0041}', '\u{0042}', '\u{0043}', '\u{0044}', '\u{0045}', '\u{0046}', '\u{0047}',
    '\u{0048}', '\u{0049}', '\u{00AD}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00F3}', '\u{00F5}',
    // 0xD0-0xDF
    '\u{007D}', '\u{004A}', '\u{004B}', '\u{004C}', '\u{004D}', '\u{004E}', '\u{004F}', '\u{0050}',
    '\u{0051}', '\u{0052}', '\u{00B9}', '\u{00FB}', '\u{00FC}', '\u{00F9}', '\u{00FA}', '\u{00FF}',
    // 0xE0-0xEF
    '\u{005C}', '\u{00F7}', '\u{0053}', '\u{0054}', '\u{0055}', '\u{0056}', '\u{0057}', '\u{0058}',
    '\u{0059}', '\u{005A}', '\u{00B2}', '\u{00D4}', '\u{00D6}', '\u{00D2}', '\u{00D3}', '\u{00D5}',
    // 0xF0-0xFF
    '\u{0030}', '\u{0031}', '\u{0032}', '\u{0033}', '\u{0034}', '\u{0035}', '\u{0036}', '\u{0037}',
    '\u{0038}', '\u{0039}', '\u{00B3}', '\u{00DB}', '\u{00DC}', '\u{00D9}', '\u{00DA}', '\u{009F}',
];

/// Returns the EBCDIC-to-Unicode lookup table for the given code page.
fn table_for(code_page: CodePage) -> &'static [char; 256] {
    match code_page {
        CodePage::Cp037 => &EBCDIC_TO_UNICODE_037,
        CodePage::Cp500 => &EBCDIC_TO_UNICODE_500,
        CodePage::Cp1047 => &EBCDIC_TO_UNICODE_1047,
    }
}

/// Convert a single EBCDIC byte to its Unicode character representation.
///
/// Uses the lookup table for the specified code page.
pub fn ebcdic_to_unicode(byte: u8, code_page: CodePage) -> char {
    let table = table_for(code_page);
    table[byte as usize]
}

/// Convert a Unicode character to its EBCDIC byte representation.
///
/// Returns `None` if the character has no mapping in the specified code page.
/// Performs a reverse lookup through the 256-entry table. For bulk encoding,
/// prefer `encode_string` which builds an internal reverse map.
pub fn unicode_to_ebcdic(ch: char, code_page: CodePage) -> Option<u8> {
    let table = table_for(code_page);
    for (i, &table_char) in table.iter().enumerate() {
        if table_char == ch {
            return Some(i as u8);
        }
    }
    None
}

/// Decode a byte slice of EBCDIC data into a Unicode `String`.
///
/// Each byte is independently converted using the lookup table for the
/// specified code page.
pub fn decode_string(bytes: &[u8], code_page: CodePage) -> String {
    let table = table_for(code_page);
    bytes.iter().map(|&b| table[b as usize]).collect()
}

/// Encode a Unicode string into EBCDIC bytes.
///
/// Characters that have no mapping in the specified code page are replaced
/// with EBCDIC 0x3F (SUB). This is the standard EBCDIC substitution behavior.
///
/// Builds a reverse lookup map internally for efficient bulk encoding.
pub fn encode_string(text: &str, code_page: CodePage) -> Vec<u8> {
    let table = table_for(code_page);

    // Build reverse lookup: Unicode char -> EBCDIC byte.
    // For characters that appear at multiple positions, the lowest EBCDIC
    // byte value wins (entry() only inserts if not already present).
    let mut reverse: std::collections::HashMap<char, u8> =
        std::collections::HashMap::with_capacity(256);
    for (i, &ch) in table.iter().enumerate() {
        reverse.entry(ch).or_insert(i as u8);
    }

    const EBCDIC_SUB: u8 = 0x3F;

    text.chars()
        .map(|ch| *reverse.get(&ch).unwrap_or(&EBCDIC_SUB))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CP 037: Basic character mappings ---

    #[test]
    fn test_space_all_code_pages() {
        assert_eq!(ebcdic_to_unicode(0x40, CodePage::Cp037), ' ');
        assert_eq!(ebcdic_to_unicode(0x40, CodePage::Cp500), ' ');
        assert_eq!(ebcdic_to_unicode(0x40, CodePage::Cp1047), ' ');
    }

    #[test]
    fn test_uppercase_a_through_i_037() {
        for (i, expected) in ('A'..='I').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0xC1 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0xC1 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_uppercase_j_through_r_037() {
        for (i, expected) in ('J'..='R').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0xD1 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0xD1 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_uppercase_s_through_z_037() {
        for (i, expected) in ('S'..='Z').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0xE2 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0xE2 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_lowercase_a_through_i_037() {
        for (i, expected) in ('a'..='i').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0x81 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0x81 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_lowercase_j_through_r_037() {
        for (i, expected) in ('j'..='r').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0x91 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0x91 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_lowercase_s_through_z_037() {
        for (i, expected) in ('s'..='z').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0xA2 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0xA2 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_digits_037() {
        for (i, expected) in ('0'..='9').enumerate() {
            assert_eq!(
                ebcdic_to_unicode(0xF0 + i as u8, CodePage::Cp037),
                expected,
                "EBCDIC 0x{:02X} should be '{}'",
                0xF0 + i as u8,
                expected
            );
        }
    }

    #[test]
    fn test_common_punctuation_037() {
        assert_eq!(ebcdic_to_unicode(0x4B, CodePage::Cp037), '.');
        assert_eq!(ebcdic_to_unicode(0x4C, CodePage::Cp037), '<');
        assert_eq!(ebcdic_to_unicode(0x4D, CodePage::Cp037), '(');
        assert_eq!(ebcdic_to_unicode(0x4E, CodePage::Cp037), '+');
        assert_eq!(ebcdic_to_unicode(0x50, CodePage::Cp037), '&');
        assert_eq!(ebcdic_to_unicode(0x5A, CodePage::Cp037), '!');
        assert_eq!(ebcdic_to_unicode(0x5B, CodePage::Cp037), '$');
        assert_eq!(ebcdic_to_unicode(0x5C, CodePage::Cp037), '*');
        assert_eq!(ebcdic_to_unicode(0x5D, CodePage::Cp037), ')');
        assert_eq!(ebcdic_to_unicode(0x5E, CodePage::Cp037), ';');
        assert_eq!(ebcdic_to_unicode(0x60, CodePage::Cp037), '-');
        assert_eq!(ebcdic_to_unicode(0x61, CodePage::Cp037), '/');
        assert_eq!(ebcdic_to_unicode(0x6B, CodePage::Cp037), ',');
        assert_eq!(ebcdic_to_unicode(0x6C, CodePage::Cp037), '%');
        assert_eq!(ebcdic_to_unicode(0x6D, CodePage::Cp037), '_');
        assert_eq!(ebcdic_to_unicode(0x6E, CodePage::Cp037), '>');
        assert_eq!(ebcdic_to_unicode(0x6F, CodePage::Cp037), '?');
        assert_eq!(ebcdic_to_unicode(0x7A, CodePage::Cp037), ':');
        assert_eq!(ebcdic_to_unicode(0x7B, CodePage::Cp037), '#');
        assert_eq!(ebcdic_to_unicode(0x7C, CodePage::Cp037), '@');
        assert_eq!(ebcdic_to_unicode(0x7D, CodePage::Cp037), '\'');
        assert_eq!(ebcdic_to_unicode(0x7E, CodePage::Cp037), '=');
        assert_eq!(ebcdic_to_unicode(0x7F, CodePage::Cp037), '"');
    }

    #[test]
    fn test_braces_and_brackets_037() {
        assert_eq!(ebcdic_to_unicode(0xC0, CodePage::Cp037), '{');
        assert_eq!(ebcdic_to_unicode(0xD0, CodePage::Cp037), '}');
        assert_eq!(ebcdic_to_unicode(0xBA, CodePage::Cp037), '[');
        assert_eq!(ebcdic_to_unicode(0xBB, CodePage::Cp037), ']');
        assert_eq!(ebcdic_to_unicode(0xE0, CodePage::Cp037), '\\');
        assert_eq!(ebcdic_to_unicode(0x4F, CodePage::Cp037), '|');
        assert_eq!(ebcdic_to_unicode(0xB0, CodePage::Cp037), '^');
        assert_eq!(ebcdic_to_unicode(0xA1, CodePage::Cp037), '~');
    }

    // --- Round-trip tests for all code pages ---

    #[test]
    fn test_roundtrip_printable_ascii_cp037() {
        for ch in ' '..='~' {
            let ebcdic = unicode_to_ebcdic(ch, CodePage::Cp037);
            assert!(
                ebcdic.is_some(),
                "'{}' (U+{:04X}) has no EBCDIC mapping in CP 037",
                ch,
                ch as u32
            );
            let back = ebcdic_to_unicode(ebcdic.unwrap(), CodePage::Cp037);
            assert_eq!(
                back,
                ch,
                "Round-trip failed for '{}' via 0x{:02X}",
                ch,
                ebcdic.unwrap()
            );
        }
    }

    #[test]
    fn test_roundtrip_printable_ascii_cp500() {
        for ch in ' '..='~' {
            let ebcdic = unicode_to_ebcdic(ch, CodePage::Cp500);
            assert!(
                ebcdic.is_some(),
                "'{}' (U+{:04X}) has no EBCDIC mapping in CP 500",
                ch,
                ch as u32
            );
            let back = ebcdic_to_unicode(ebcdic.unwrap(), CodePage::Cp500);
            assert_eq!(
                back,
                ch,
                "Round-trip failed for '{}' via 0x{:02X}",
                ch,
                ebcdic.unwrap()
            );
        }
    }

    #[test]
    fn test_roundtrip_printable_ascii_cp1047() {
        for ch in ' '..='~' {
            let ebcdic = unicode_to_ebcdic(ch, CodePage::Cp1047);
            assert!(
                ebcdic.is_some(),
                "'{}' (U+{:04X}) has no EBCDIC mapping in CP 1047",
                ch,
                ch as u32
            );
            let back = ebcdic_to_unicode(ebcdic.unwrap(), CodePage::Cp1047);
            assert_eq!(
                back,
                ch,
                "Round-trip failed for '{}' via 0x{:02X}",
                ch,
                ebcdic.unwrap()
            );
        }
    }

    // --- String encoding/decoding tests ---

    #[test]
    fn test_decode_string_hello_upper() {
        let ebcdic = [0xC8, 0xC5, 0xD3, 0xD3, 0xD6];
        assert_eq!(decode_string(&ebcdic, CodePage::Cp037), "HELLO");
    }

    #[test]
    fn test_decode_string_hello_lower() {
        let ebcdic = [0x88, 0x85, 0x93, 0x93, 0x96];
        assert_eq!(decode_string(&ebcdic, CodePage::Cp037), "hello");
    }

    #[test]
    fn test_decode_string_digits() {
        let ebcdic = [0xF1, 0xF2, 0xF3, 0xF4, 0xF5];
        assert_eq!(decode_string(&ebcdic, CodePage::Cp037), "12345");
    }

    #[test]
    fn test_decode_string_tso() {
        // "TSO" = T(0xE3) S(0xE2) O(0xD6)
        let ebcdic = [0xE3, 0xE2, 0xD6];
        assert_eq!(decode_string(&ebcdic, CodePage::Cp037), "TSO");
    }

    #[test]
    fn test_encode_string_hello() {
        let encoded = encode_string("HELLO", CodePage::Cp037);
        assert_eq!(encoded, vec![0xC8, 0xC5, 0xD3, 0xD3, 0xD6]);
    }

    #[test]
    fn test_encode_string_digits() {
        let encoded = encode_string("12345", CodePage::Cp037);
        assert_eq!(encoded, vec![0xF1, 0xF2, 0xF3, 0xF4, 0xF5]);
    }

    #[test]
    fn test_encode_decode_roundtrip_string() {
        let original = "Hello, World! 123 @#$";
        let encoded = encode_string(original, CodePage::Cp037);
        let decoded = decode_string(&encoded, CodePage::Cp037);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_encode_mixed_case() {
        let original = "AbCdEf";
        let encoded = encode_string(original, CodePage::Cp037);
        let decoded = decode_string(&encoded, CodePage::Cp037);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_encode_unmappable_character() {
        // CJK character has no EBCDIC mapping, should become 0x3F (SUB)
        let encoded = encode_string("\u{4E2D}", CodePage::Cp037);
        assert_eq!(encoded, vec![0x3F]);
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(decode_string(&[], CodePage::Cp037), "");
        assert_eq!(encode_string("", CodePage::Cp037), Vec::<u8>::new());
    }

    // --- Cross-code-page difference tests ---

    #[test]
    fn test_cp500_bracket_positions() {
        // CP 500 has [ at 0x4A, ] at 0x5A
        assert_eq!(ebcdic_to_unicode(0x4A, CodePage::Cp500), '[');
        assert_eq!(ebcdic_to_unicode(0x5A, CodePage::Cp500), ']');
        // CP 037 has cent-sign at 0x4A, ! at 0x5A
        assert_eq!(ebcdic_to_unicode(0x4A, CodePage::Cp037), '\u{00A2}');
        assert_eq!(ebcdic_to_unicode(0x5A, CodePage::Cp037), '!');
    }

    #[test]
    fn test_cp500_exclamation_position() {
        // CP 500 has ! at 0x4F (where CP 037 has |)
        assert_eq!(ebcdic_to_unicode(0x4F, CodePage::Cp500), '!');
        assert_eq!(ebcdic_to_unicode(0x4F, CodePage::Cp037), '|');
    }

    #[test]
    fn test_cp500_caret_position() {
        // CP 500 has ^ at 0x5F (where CP 037 has not-sign)
        assert_eq!(ebcdic_to_unicode(0x5F, CodePage::Cp500), '^');
        assert_eq!(ebcdic_to_unicode(0x5F, CodePage::Cp037), '\u{00AC}');
    }

    #[test]
    fn test_cp1047_newline_position() {
        // CP 1047 has LF (0x0A) at position 0x15 (Unix newline)
        assert_eq!(ebcdic_to_unicode(0x15, CodePage::Cp1047), '\u{000A}');
        // CP 037 has NEL (0x85) at position 0x15
        assert_eq!(ebcdic_to_unicode(0x15, CodePage::Cp037), '\u{0085}');
    }

    #[test]
    fn test_cp1047_bracket_positions() {
        // CP 1047 has [ at 0xAD, ] at 0xBD
        assert_eq!(ebcdic_to_unicode(0xAD, CodePage::Cp1047), '[');
        assert_eq!(ebcdic_to_unicode(0xBD, CodePage::Cp1047), ']');
        // CP 037 has soft-hyphen at 0xAD (via 0xCA offset area)
        assert_eq!(ebcdic_to_unicode(0xAD, CodePage::Cp037), '\u{00DD}');
    }

    #[test]
    fn test_cp1047_caret_position() {
        // CP 1047 has ^ at 0x5F
        assert_eq!(ebcdic_to_unicode(0x5F, CodePage::Cp1047), '^');
    }

    // --- Table completeness tests ---

    #[test]
    fn test_all_tables_fully_populated() {
        // Every byte value should produce a valid char (no panics)
        for byte in 0..=255u8 {
            let _ = ebcdic_to_unicode(byte, CodePage::Cp037);
            let _ = ebcdic_to_unicode(byte, CodePage::Cp500);
            let _ = ebcdic_to_unicode(byte, CodePage::Cp1047);
        }
    }

    #[test]
    fn test_unicode_to_ebcdic_not_found() {
        assert_eq!(unicode_to_ebcdic('\u{1F600}', CodePage::Cp037), None);
        assert_eq!(unicode_to_ebcdic('\u{4E00}', CodePage::Cp500), None);
    }

    // --- Digits are the same across all code pages ---

    #[test]
    fn test_digits_same_across_code_pages() {
        for i in 0..10u8 {
            let expected = char::from(b'0' + i);
            let byte = 0xF0 + i;
            assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp037), expected);
            assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp500), expected);
            assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp1047), expected);
        }
    }

    // --- Letters are the same across all code pages ---

    #[test]
    fn test_uppercase_letters_same_across_code_pages() {
        let ranges: &[(u8, u8, char)] = &[
            (0xC1, 0xC9, 'A'), // A-I
            (0xD1, 0xD9, 'J'), // J-R
            (0xE2, 0xE9, 'S'), // S-Z
        ];
        for &(start, end, first_char) in ranges {
            for offset in 0..=(end - start) {
                let expected = char::from(first_char as u8 + offset);
                let byte = start + offset;
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp037), expected);
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp500), expected);
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp1047), expected);
            }
        }
    }

    #[test]
    fn test_lowercase_letters_same_across_code_pages() {
        let ranges: &[(u8, u8, char)] = &[
            (0x81, 0x89, 'a'), // a-i
            (0x91, 0x99, 'j'), // j-r
            (0xA2, 0xA9, 's'), // s-z
        ];
        for &(start, end, first_char) in ranges {
            for offset in 0..=(end - start) {
                let expected = char::from(first_char as u8 + offset);
                let byte = start + offset;
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp037), expected);
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp500), expected);
                assert_eq!(ebcdic_to_unicode(byte, CodePage::Cp1047), expected);
            }
        }
    }
}
