//! 3270 Data Stream parser.
//!
//! Parses binary data streams from the IBM 3270 host as documented in the
//! IBM 3270 Data Stream Programmer's Reference (GA23-0059). The data stream
//! consists of a write command, a Write Control Character (WCC), followed by
//! a sequence of orders and character data.
//!
//! This module handles both standard (non-SNA) and SNA command codes.

use thiserror::Error;

/// Errors that can occur while parsing a 3270 data stream.
#[derive(Debug, Error)]
pub enum Tn3270Error {
    #[error("data stream is empty")]
    EmptyDataStream,

    #[error("unknown write command: 0x{0:02X}")]
    UnknownWriteCommand(u8),

    #[error("unexpected end of data at offset {0}: expected {1} more bytes")]
    UnexpectedEnd(usize, usize),

    #[error("invalid buffer address encoding at offset {0}")]
    InvalidBufferAddress(usize),

    #[error("unknown order code 0x{0:02X} at offset {1}")]
    UnknownOrder(u8, usize),

    #[error("SFE pair count is zero at offset {0}")]
    InvalidSfeCount(usize),
}

// -- Write commands (first byte of data stream) --

/// 3270 write command types.
///
/// These appear as the first byte of a host-to-terminal data stream.
/// Both standard (non-SNA) and SNA variants are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCommand {
    /// Write (W) - write data to the current screen without erasing.
    /// Standard: 0x01, SNA: 0xF1
    Write,
    /// Erase/Write (EW) - erase the screen then write data.
    /// Standard: 0x05, SNA: 0xF5
    EraseWrite,
    /// Erase/Write Alternate (EWA) - erase and switch to alternate screen size.
    /// Standard: 0x0D, SNA: 0x7E
    EraseWriteAlternate,
    /// Write Structured Field (WSF) - structured field data follows.
    /// Standard: 0x11, SNA: 0xF3
    WriteStructuredField,
}

/// Standard (non-SNA) write command bytes.
const CMD_WRITE: u8 = 0x01;
const CMD_ERASE_WRITE: u8 = 0x05;
const CMD_ERASE_WRITE_ALTERNATE: u8 = 0x0D;
const CMD_WRITE_STRUCTURED_FIELD: u8 = 0x11;

/// SNA write command bytes.
const CMD_WRITE_SNA: u8 = 0xF1;
const CMD_ERASE_WRITE_SNA: u8 = 0xF5;
const CMD_ERASE_WRITE_ALTERNATE_SNA: u8 = 0x7E;
const CMD_WRITE_STRUCTURED_FIELD_SNA: u8 = 0xF3;

impl WriteCommand {
    /// Parse a write command byte (handles both standard and SNA encodings).
    pub fn from_byte(byte: u8) -> Result<Self, Tn3270Error> {
        match byte {
            CMD_WRITE | CMD_WRITE_SNA => Ok(WriteCommand::Write),
            CMD_ERASE_WRITE | CMD_ERASE_WRITE_SNA => Ok(WriteCommand::EraseWrite),
            CMD_ERASE_WRITE_ALTERNATE | CMD_ERASE_WRITE_ALTERNATE_SNA => {
                Ok(WriteCommand::EraseWriteAlternate)
            }
            CMD_WRITE_STRUCTURED_FIELD | CMD_WRITE_STRUCTURED_FIELD_SNA => {
                Ok(WriteCommand::WriteStructuredField)
            }
            _ => Err(Tn3270Error::UnknownWriteCommand(byte)),
        }
    }
}

// -- Write Control Character (WCC) --

/// Write Control Character - the second byte after a write command.
///
/// Controls terminal behavior after data is written. Bit assignments follow
/// GA23-0059 Section 4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wcc {
    /// Bit 1: Reset Modified Data Tags in all fields.
    pub reset_mdt: bool,
    /// Bit 2: Restore the keyboard (unlock it).
    pub restore_keyboard: bool,
    /// Bit 6: Sound the terminal alarm.
    pub alarm: bool,
}

impl Wcc {
    /// Parse a WCC byte.
    ///
    /// Bit numbering follows IBM convention (bit 0 = MSB = 0x80).
    /// - Bit 1 (0x40): Reset MDT
    /// - Bit 2 (0x20): Restore keyboard (type-ahead)
    /// - Bit 6 (0x02): Sound alarm
    pub fn from_byte(byte: u8) -> Self {
        Wcc {
            reset_mdt: (byte & 0x40) != 0,
            restore_keyboard: (byte & 0x20) != 0,
            alarm: (byte & 0x02) != 0,
        }
    }

    /// Encode this WCC back to a byte.
    pub fn to_byte(&self) -> u8 {
        let mut b = 0u8;
        if self.reset_mdt {
            b |= 0x40;
        }
        if self.restore_keyboard {
            b |= 0x20;
        }
        if self.alarm {
            b |= 0x02;
        }
        b
    }
}

// -- Field attributes --

/// Field display intensity / visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intensity {
    /// Normal display intensity.
    Normal,
    /// Non-display (invisible) field.
    Invisible,
    /// High-intensity (bright) display.
    Intensified,
}

/// Field attribute byte, parsed from the SF (Start Field) order.
///
/// The attribute byte layout (GA23-0059 Section 4.4.4):
/// - Bits 0-1: Reserved
/// - Bit 2: Protected (1) / Unprotected (0)
/// - Bit 3: Numeric (1) / Alphanumeric (0)
/// - Bits 4-5: Intensity/visibility
///   - 00 = Normal, non-pen-detectable
///   - 01 = Normal, pen-detectable
///   - 10 = High intensity, pen-detectable
///   - 11 = Non-display
/// - Bit 6: Reserved
/// - Bit 7: Modified Data Tag (MDT)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldAttribute {
    /// Whether the field is protected (read-only).
    pub protected: bool,
    /// Whether the field is numeric-only.
    pub numeric: bool,
    /// Display intensity/visibility.
    pub intensity: Intensity,
    /// Modified Data Tag - set when field has been modified.
    pub modified: bool,
}

impl FieldAttribute {
    /// Parse a field attribute byte.
    pub fn from_byte(byte: u8) -> Self {
        let protected = (byte & 0x20) != 0;
        let numeric = (byte & 0x10) != 0;

        let intensity_bits = (byte >> 2) & 0x03;
        let intensity = match intensity_bits {
            0b00 | 0b01 => Intensity::Normal,
            0b10 => Intensity::Intensified,
            0b11 => Intensity::Invisible,
            _ => unreachable!(),
        };

        let modified = (byte & 0x01) != 0;

        FieldAttribute {
            protected,
            numeric,
            intensity,
            modified,
        }
    }

    /// Encode this field attribute back to a byte.
    pub fn to_byte(&self) -> u8 {
        let mut b = 0u8;
        if self.protected {
            b |= 0x20;
        }
        if self.numeric {
            b |= 0x10;
        }
        match self.intensity {
            Intensity::Normal => {} // bits 4-5 = 00
            Intensity::Intensified => b |= 0x08,
            Intensity::Invisible => b |= 0x0C,
        }
        if self.modified {
            b |= 0x01;
        }
        b
    }

    /// Returns true if this field can accept user input.
    pub fn is_unprotected(&self) -> bool {
        !self.protected
    }

    /// Returns true if this field is a skip field (protected + numeric).
    pub fn is_skip(&self) -> bool {
        self.protected && self.numeric
    }
}

// -- Extended attributes --

/// Extended attribute types used by SFE (Start Field Extended) and SA (Set Attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedAttribute {
    /// 3270 field attribute (type 0xC0).
    FieldAttribute(FieldAttribute),
    /// Extended highlighting (type 0x41).
    Highlighting(Highlighting),
    /// Foreground color (type 0x42).
    ForegroundColor(Color3270),
    /// Background color (type 0x45).
    BackgroundColor(Color3270),
    /// Character set (type 0x43).
    CharacterSet(u8),
    /// Unknown/unsupported attribute type.
    Unknown { attr_type: u8, value: u8 },
}

/// Extended highlighting modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlighting {
    /// Default (no highlighting).
    Default,
    /// Blinking.
    Blink,
    /// Reverse video.
    ReverseVideo,
    /// Underscore.
    Underscore,
    /// Intensified (same as field attribute intensified).
    Intensified,
}

impl Highlighting {
    fn from_byte(byte: u8) -> Self {
        match byte {
            0x00 | 0xF0 => Highlighting::Default,
            0xF1 => Highlighting::Blink,
            0xF2 => Highlighting::ReverseVideo,
            0xF4 => Highlighting::Underscore,
            0xF8 => Highlighting::Intensified,
            _ => Highlighting::Default,
        }
    }
}

/// 3270 color values used in extended color attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color3270 {
    Default,
    Blue,
    Red,
    Pink,
    Green,
    Turquoise,
    Yellow,
    White,
}

impl Color3270 {
    fn from_byte(byte: u8) -> Self {
        match byte {
            0x00 | 0xF0 => Color3270::Default,
            0xF1 => Color3270::Blue,
            0xF2 => Color3270::Red,
            0xF3 => Color3270::Pink,
            0xF4 => Color3270::Green,
            0xF5 => Color3270::Turquoise,
            0xF6 => Color3270::Yellow,
            0xF7 => Color3270::White,
            _ => Color3270::Default,
        }
    }

    /// Convert to a byte value for encoding.
    pub fn to_byte(&self) -> u8 {
        match self {
            Color3270::Default => 0x00,
            Color3270::Blue => 0xF1,
            Color3270::Red => 0xF2,
            Color3270::Pink => 0xF3,
            Color3270::Green => 0xF4,
            Color3270::Turquoise => 0xF5,
            Color3270::Yellow => 0xF6,
            Color3270::White => 0xF7,
        }
    }
}

impl ExtendedAttribute {
    /// Parse an extended attribute type-value pair.
    fn from_pair(attr_type: u8, value: u8) -> Self {
        match attr_type {
            0xC0 => ExtendedAttribute::FieldAttribute(FieldAttribute::from_byte(value)),
            0x41 => ExtendedAttribute::Highlighting(Highlighting::from_byte(value)),
            0x42 => ExtendedAttribute::ForegroundColor(Color3270::from_byte(value)),
            0x45 => ExtendedAttribute::BackgroundColor(Color3270::from_byte(value)),
            0x43 => ExtendedAttribute::CharacterSet(value),
            _ => ExtendedAttribute::Unknown { attr_type, value },
        }
    }
}

// -- Orders --

/// 3270 data stream order codes.
const ORDER_SBA: u8 = 0x11; // Set Buffer Address
const ORDER_SF: u8 = 0x1D; // Start Field
const ORDER_SFE: u8 = 0x29; // Start Field Extended
const ORDER_SA: u8 = 0x28; // Set Attribute
const ORDER_IC: u8 = 0x13; // Insert Cursor
const ORDER_PT: u8 = 0x05; // Program Tab
const ORDER_RA: u8 = 0x3C; // Repeat to Address
const ORDER_EUA: u8 = 0x12; // Erase Unprotected to Address
const ORDER_MF: u8 = 0x2C; // Modify Field

/// 3270 data stream orders.
///
/// Orders are interspersed with character data in the data stream and control
/// positioning, field attributes, and other display properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Order {
    /// Set Buffer Address - moves the current buffer position.
    /// The u16 is the linear buffer address (row * cols + col).
    Sba(u16),
    /// Start Field - begins a new field with the given attribute.
    Sf(FieldAttribute),
    /// Start Field Extended - begins a field with extended attributes.
    Sfe(Vec<ExtendedAttribute>),
    /// Set Attribute - changes a display attribute without starting a field.
    Sa(ExtendedAttribute),
    /// Insert Cursor - sets the cursor position to the current buffer address.
    Ic,
    /// Program Tab - advance to the next unprotected field.
    Pt,
    /// Repeat to Address - fill from current position to the target address
    /// with the given EBCDIC character byte.
    Ra(u16, u8),
    /// Erase Unprotected to Address - clear unprotected fields from current
    /// position to the target address.
    Eua(u16),
    /// Modify Field - modify attributes of an existing field.
    Mf(Vec<ExtendedAttribute>),
}

// -- Buffer addressing --

/// Decode a 2-byte 3270 buffer address.
///
/// The 3270 uses two encoding schemes depending on screen size:
///
/// 12-bit encoding (for screens <= 4096 positions):
///   If bits 0-1 of byte 1 are not both 1, the address is encoded as:
///   byte1 bits 0-5 = high 6 bits, byte2 bits 0-5 = low 6 bits.
///   Each byte uses a 6-bit encoding where the actual address bits are
///   in positions 0-5, and the top 2 bits are set/clear per a lookup pattern.
///
/// 14-bit encoding (for larger screens):
///   If bits 6-7 of byte 1 are both set (0b11xxxxxx), the address is:
///   byte1 bits 0-5 = high 6 bits, byte2 = low 8 bits = full 14-bit address.
///
/// In practice: if (b1 & 0xC0) == 0x00, it is the 14-bit form.
/// Otherwise it is the 12-bit 6+6 form.
pub fn decode_buffer_address(b1: u8, b2: u8) -> u16 {
    if (b1 & 0xC0) == 0x00 {
        // 14-bit addressing: top 6 bits from b1, full 8 bits from b2
        ((b1 as u16 & 0x3F) << 8) | (b2 as u16)
    } else {
        // 12-bit addressing: 6 bits from each byte
        ((b1 as u16 & 0x3F) << 6) | (b2 as u16 & 0x3F)
    }
}

/// Encode a buffer address into two bytes using 12-bit encoding.
///
/// Uses the standard 6+6 bit encoding suitable for screens up to 4096
/// positions (which covers 24x80=1920 and 32x80=2560 and 43x80=3440).
///
/// Each 6-bit value is encoded with specific high bits per the 3270 address
/// translation table.
pub fn encode_buffer_address(addr: u16) -> (u8, u8) {
    let high = ((addr >> 6) & 0x3F) as u8;
    let low = (addr & 0x3F) as u8;
    (encode_address_byte(high), encode_address_byte(low))
}

/// Encode a single 6-bit value into a 3270 address byte.
///
/// The encoding table maps 6-bit values (0-63) to specific byte patterns
/// as defined in GA23-0059. The top two bits follow a specific pattern:
/// 0x00-0x0F -> 0x40+val, 0x10-0x1F -> val-0x10+0x50, etc.
///
/// The standard pattern is:
///   0-63 maps to the well-known 3270 address bytes.
fn encode_address_byte(val: u8) -> u8 {
    const ADDRESS_TABLE: [u8; 64] = [
        0x40, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, // 0-7
        0xC8, 0xC9, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, // 8-15
        0x50, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, // 16-23
        0xD8, 0xD9, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, // 24-31
        0x60, 0x61, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, // 32-39
        0xE8, 0xE9, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, // 40-47
        0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, // 48-55
        0xF8, 0xF9, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F, // 56-63
    ];
    ADDRESS_TABLE[val as usize & 0x3F]
}

/// Encode a buffer address into two bytes using 14-bit encoding.
///
/// Used for screens larger than 4096 positions. The first byte has its
/// top two bits cleared (0b00xxxxxx), and the second byte contains the
/// low 8 bits of the address.
pub fn encode_buffer_address_14bit(addr: u16) -> (u8, u8) {
    let high = ((addr >> 8) & 0x3F) as u8;
    let low = (addr & 0xFF) as u8;
    (high, low)
}

// -- Data stream items --

/// A single item in a parsed 3270 data stream (either an order or character data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataStreamItem {
    /// A 3270 order (SBA, SF, IC, etc.)
    Order(Order),
    /// An EBCDIC character byte to be placed at the current buffer position.
    Character(u8),
}

/// A fully parsed 3270 data stream from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataStream {
    /// The write command type.
    pub command: WriteCommand,
    /// The Write Control Character.
    pub wcc: Wcc,
    /// The sequence of orders and character data.
    pub orders: Vec<DataStreamItem>,
}

// -- AID (Attention Identifier) bytes --

/// Attention Identifier (AID) codes sent from the terminal to the host.
///
/// These identify which key the user pressed to submit data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aid {
    /// No AID (initial state).
    None,
    /// Enter key.
    Enter,
    /// PF1-PF24 keys.
    Pf(u8),
    /// PA1-PA3 keys (program attention, do not transmit modified fields).
    Pa(u8),
    /// Clear key.
    Clear,
    /// Short-read structured field.
    StructuredField,
}

impl Aid {
    /// The AID byte value for Enter.
    pub const ENTER: u8 = 0x7D;
    /// The AID byte value for Clear.
    pub const CLEAR: u8 = 0x6D;
    /// The AID byte value for PA1.
    pub const PA1: u8 = 0x6C;
    /// The AID byte value for PA2.
    pub const PA2: u8 = 0x6E;
    /// The AID byte value for PA3.
    pub const PA3: u8 = 0x6B;

    /// PF key AID bytes (PF1 through PF24).
    const PF_AIDS: [u8; 24] = [
        0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0x7A, 0x7B, 0x7C, // PF1-12
        0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0x4A, 0x4B, 0x4C, // PF13-24
    ];

    /// Convert an AID enum to its byte value.
    pub fn to_byte(&self) -> u8 {
        match self {
            Aid::None => 0x60,
            Aid::Enter => Self::ENTER,
            Aid::Clear => Self::CLEAR,
            Aid::Pa(1) => Self::PA1,
            Aid::Pa(2) => Self::PA2,
            Aid::Pa(3) => Self::PA3,
            Aid::Pa(_) => Self::PA1, // fallback
            Aid::Pf(n) if *n >= 1 && *n <= 24 => Self::PF_AIDS[(*n - 1) as usize],
            Aid::Pf(_) => Self::PF_AIDS[0], // fallback
            Aid::StructuredField => 0x88,
        }
    }

    /// Parse an AID byte into an Aid enum.
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0x7D => Aid::Enter,
            0x6D => Aid::Clear,
            0x6C => Aid::Pa(1),
            0x6E => Aid::Pa(2),
            0x6B => Aid::Pa(3),
            0x60 => Aid::None,
            0x88 => Aid::StructuredField,
            _ => {
                // Check PF keys
                for (i, &aid_byte) in Self::PF_AIDS.iter().enumerate() {
                    if byte == aid_byte {
                        return Aid::Pf((i + 1) as u8);
                    }
                }
                Aid::None
            }
        }
    }
}

// -- Parser --

/// Returns true if the given byte is a 3270 order code.
fn is_order(byte: u8) -> bool {
    matches!(
        byte,
        ORDER_SBA
            | ORDER_SF
            | ORDER_SFE
            | ORDER_SA
            | ORDER_IC
            | ORDER_PT
            | ORDER_RA
            | ORDER_EUA
            | ORDER_MF
    )
}

/// Parse a complete 3270 data stream from raw bytes.
///
/// The input must contain at least the command byte and WCC byte.
/// For WriteStructuredField commands, the WCC is not present and the
/// remaining data is returned as raw character bytes.
pub fn parse_data_stream(data: &[u8]) -> Result<DataStream, Tn3270Error> {
    if data.is_empty() {
        return Err(Tn3270Error::EmptyDataStream);
    }

    let command = WriteCommand::from_byte(data[0])?;

    // WSF has no WCC - the rest is structured field data
    if command == WriteCommand::WriteStructuredField {
        let orders: Vec<DataStreamItem> = data[1..]
            .iter()
            .map(|&b| DataStreamItem::Character(b))
            .collect();
        return Ok(DataStream {
            command,
            wcc: Wcc {
                reset_mdt: false,
                restore_keyboard: false,
                alarm: false,
            },
            orders,
        });
    }

    if data.len() < 2 {
        return Err(Tn3270Error::UnexpectedEnd(1, 1));
    }

    let wcc = Wcc::from_byte(data[1]);
    let mut orders = Vec::new();
    let mut pos = 2;

    while pos < data.len() {
        let byte = data[pos];

        if is_order(byte) {
            match byte {
                ORDER_SBA => {
                    // SBA: 2-byte buffer address follows
                    if pos + 2 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 2));
                    }
                    let addr = decode_buffer_address(data[pos + 1], data[pos + 2]);
                    orders.push(DataStreamItem::Order(Order::Sba(addr)));
                    pos += 3;
                }
                ORDER_SF => {
                    // SF: 1-byte attribute follows
                    if pos + 1 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 1));
                    }
                    let attr = FieldAttribute::from_byte(data[pos + 1]);
                    orders.push(DataStreamItem::Order(Order::Sf(attr)));
                    pos += 2;
                }
                ORDER_SFE => {
                    // SFE: count byte, then count * (type, value) pairs
                    if pos + 1 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 1));
                    }
                    let count = data[pos + 1] as usize;
                    if count == 0 {
                        return Err(Tn3270Error::InvalidSfeCount(pos));
                    }
                    let pairs_len = count * 2;
                    if pos + 2 + pairs_len > data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, pairs_len + 2));
                    }
                    let mut attrs = Vec::with_capacity(count);
                    for i in 0..count {
                        let attr_type = data[pos + 2 + i * 2];
                        let attr_value = data[pos + 2 + i * 2 + 1];
                        attrs.push(ExtendedAttribute::from_pair(attr_type, attr_value));
                    }
                    orders.push(DataStreamItem::Order(Order::Sfe(attrs)));
                    pos += 2 + pairs_len;
                }
                ORDER_SA => {
                    // SA: 2 bytes (type, value)
                    if pos + 2 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 2));
                    }
                    let attr = ExtendedAttribute::from_pair(data[pos + 1], data[pos + 2]);
                    orders.push(DataStreamItem::Order(Order::Sa(attr)));
                    pos += 3;
                }
                ORDER_IC => {
                    // IC: no operands
                    orders.push(DataStreamItem::Order(Order::Ic));
                    pos += 1;
                }
                ORDER_PT => {
                    // PT: no operands
                    orders.push(DataStreamItem::Order(Order::Pt));
                    pos += 1;
                }
                ORDER_RA => {
                    // RA: 2-byte address + 1-byte character
                    if pos + 3 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 3));
                    }
                    let addr = decode_buffer_address(data[pos + 1], data[pos + 2]);
                    let ch = data[pos + 3];
                    orders.push(DataStreamItem::Order(Order::Ra(addr, ch)));
                    pos += 4;
                }
                ORDER_EUA => {
                    // EUA: 2-byte buffer address follows
                    if pos + 2 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 2));
                    }
                    let addr = decode_buffer_address(data[pos + 1], data[pos + 2]);
                    orders.push(DataStreamItem::Order(Order::Eua(addr)));
                    pos += 3;
                }
                ORDER_MF => {
                    // MF: count byte, then count * (type, value) pairs
                    if pos + 1 >= data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, 1));
                    }
                    let count = data[pos + 1] as usize;
                    if count == 0 {
                        return Err(Tn3270Error::InvalidSfeCount(pos));
                    }
                    let pairs_len = count * 2;
                    if pos + 2 + pairs_len > data.len() {
                        return Err(Tn3270Error::UnexpectedEnd(pos, pairs_len + 2));
                    }
                    let mut attrs = Vec::with_capacity(count);
                    for i in 0..count {
                        let attr_type = data[pos + 2 + i * 2];
                        let attr_value = data[pos + 2 + i * 2 + 1];
                        attrs.push(ExtendedAttribute::from_pair(attr_type, attr_value));
                    }
                    orders.push(DataStreamItem::Order(Order::Mf(attrs)));
                    pos += 2 + pairs_len;
                }
                _ => {
                    return Err(Tn3270Error::UnknownOrder(byte, pos));
                }
            }
        } else {
            // Character data - EBCDIC byte to be written at current position
            orders.push(DataStreamItem::Character(byte));
            pos += 1;
        }
    }

    Ok(DataStream {
        command,
        wcc,
        orders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- WriteCommand tests --

    #[test]
    fn test_write_command_standard() {
        assert_eq!(WriteCommand::from_byte(0x01).unwrap(), WriteCommand::Write);
        assert_eq!(
            WriteCommand::from_byte(0x05).unwrap(),
            WriteCommand::EraseWrite
        );
        assert_eq!(
            WriteCommand::from_byte(0x0D).unwrap(),
            WriteCommand::EraseWriteAlternate
        );
        assert_eq!(
            WriteCommand::from_byte(0x11).unwrap(),
            WriteCommand::WriteStructuredField
        );
    }

    #[test]
    fn test_write_command_sna() {
        assert_eq!(WriteCommand::from_byte(0xF1).unwrap(), WriteCommand::Write);
        assert_eq!(
            WriteCommand::from_byte(0xF5).unwrap(),
            WriteCommand::EraseWrite
        );
        assert_eq!(
            WriteCommand::from_byte(0x7E).unwrap(),
            WriteCommand::EraseWriteAlternate
        );
        assert_eq!(
            WriteCommand::from_byte(0xF3).unwrap(),
            WriteCommand::WriteStructuredField
        );
    }

    #[test]
    fn test_write_command_unknown() {
        assert!(WriteCommand::from_byte(0xFF).is_err());
        assert!(WriteCommand::from_byte(0x00).is_err());
    }

    // -- WCC tests --

    #[test]
    fn test_wcc_all_bits_set() {
        let wcc = Wcc::from_byte(0x62);
        assert!(wcc.reset_mdt);
        assert!(wcc.restore_keyboard);
        assert!(wcc.alarm);
    }

    #[test]
    fn test_wcc_no_bits_set() {
        let wcc = Wcc::from_byte(0x00);
        assert!(!wcc.reset_mdt);
        assert!(!wcc.restore_keyboard);
        assert!(!wcc.alarm);
    }

    #[test]
    fn test_wcc_reset_mdt_only() {
        let wcc = Wcc::from_byte(0x40);
        assert!(wcc.reset_mdt);
        assert!(!wcc.restore_keyboard);
        assert!(!wcc.alarm);
    }

    #[test]
    fn test_wcc_restore_keyboard_only() {
        let wcc = Wcc::from_byte(0x20);
        assert!(!wcc.reset_mdt);
        assert!(wcc.restore_keyboard);
        assert!(!wcc.alarm);
    }

    #[test]
    fn test_wcc_alarm_only() {
        let wcc = Wcc::from_byte(0x02);
        assert!(!wcc.reset_mdt);
        assert!(!wcc.restore_keyboard);
        assert!(wcc.alarm);
    }

    #[test]
    fn test_wcc_roundtrip() {
        let original = Wcc {
            reset_mdt: true,
            restore_keyboard: true,
            alarm: true,
        };
        let byte = original.to_byte();
        let parsed = Wcc::from_byte(byte);
        assert_eq!(parsed, original);
    }

    // -- FieldAttribute tests --

    #[test]
    fn test_field_attribute_unprotected_normal() {
        let attr = FieldAttribute::from_byte(0x00);
        assert!(!attr.protected);
        assert!(!attr.numeric);
        assert_eq!(attr.intensity, Intensity::Normal);
        assert!(!attr.modified);
    }

    #[test]
    fn test_field_attribute_protected() {
        let attr = FieldAttribute::from_byte(0x20);
        assert!(attr.protected);
        assert!(!attr.numeric);
    }

    #[test]
    fn test_field_attribute_numeric() {
        let attr = FieldAttribute::from_byte(0x10);
        assert!(!attr.protected);
        assert!(attr.numeric);
    }

    #[test]
    fn test_field_attribute_skip() {
        // Protected + numeric = skip field
        let attr = FieldAttribute::from_byte(0x30);
        assert!(attr.protected);
        assert!(attr.numeric);
        assert!(attr.is_skip());
    }

    #[test]
    fn test_field_attribute_intensified() {
        // Intensity bits 10 = intensified (bit positions 2-3)
        let attr = FieldAttribute::from_byte(0x08);
        assert_eq!(attr.intensity, Intensity::Intensified);
    }

    #[test]
    fn test_field_attribute_invisible() {
        // Intensity bits 11 = invisible (bit positions 2-3)
        let attr = FieldAttribute::from_byte(0x0C);
        assert_eq!(attr.intensity, Intensity::Invisible);
    }

    #[test]
    fn test_field_attribute_modified() {
        let attr = FieldAttribute::from_byte(0x01);
        assert!(attr.modified);
    }

    #[test]
    fn test_field_attribute_roundtrip() {
        let original = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Intensified,
            modified: true,
        };
        let byte = original.to_byte();
        let parsed = FieldAttribute::from_byte(byte);
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_field_attribute_all_combinations() {
        // Verify roundtrip for all reasonable combinations
        for &prot in &[false, true] {
            for &num in &[false, true] {
                for &intensity in &[
                    Intensity::Normal,
                    Intensity::Intensified,
                    Intensity::Invisible,
                ] {
                    for &modified in &[false, true] {
                        let original = FieldAttribute {
                            protected: prot,
                            numeric: num,
                            intensity,
                            modified,
                        };
                        let byte = original.to_byte();
                        let parsed = FieldAttribute::from_byte(byte);
                        assert_eq!(parsed, original, "Roundtrip failed for {:?}", original);
                    }
                }
            }
        }
    }

    // -- Buffer address tests --

    #[test]
    fn test_decode_buffer_address_12bit() {
        // 12-bit address: position 0 = (0x40, 0x40) in the address table,
        // but more directly: high6=0, low6=0
        // 0x40 has bits 7-6 = 01 (not 00), so it's 12-bit encoding
        // 0x40 & 0x3F = 0x00, so address = (0 << 6) | 0 = 0
        assert_eq!(decode_buffer_address(0x40, 0x40), 0);

        // Position 80 (row 1, col 0 in 80-col mode):
        // 80 = 0b_000001_010000 -> high6=1, low6=16
        // Encoded: high6=1 -> 0xC1, low6=16 -> 0x50
        // 0xC1 & 0x3F = 0x01, 0x50 & 0x3F = 0x10
        // (0x01 << 6) | 0x10 = 64 + 16 = 80
        assert_eq!(decode_buffer_address(0xC1, 0x50), 80);
    }

    #[test]
    fn test_decode_buffer_address_14bit() {
        // 14-bit address: b1 has bits 7-6 = 00
        // Address 256: b1 = 0x01, b2 = 0x00
        // (0x01 & 0x3F) << 8 | 0x00 = 256
        assert_eq!(decode_buffer_address(0x01, 0x00), 256);

        // Address 1920 (24 * 80): b1 = 0x07, b2 = 0x80
        // (0x07 & 0x3F) << 8 | 0x80 = 1792 + 128 = 1920
        assert_eq!(decode_buffer_address(0x07, 0x80), 1920);
    }

    #[test]
    fn test_encode_buffer_address_12bit() {
        // Position 0
        let (b1, b2) = encode_buffer_address(0);
        assert_eq!(decode_buffer_address(b1, b2), 0);

        // Position 80
        let (b1, b2) = encode_buffer_address(80);
        assert_eq!(decode_buffer_address(b1, b2), 80);

        // Position 1919 (last position in 24x80)
        let (b1, b2) = encode_buffer_address(1919);
        assert_eq!(decode_buffer_address(b1, b2), 1919);
    }

    #[test]
    fn test_encode_buffer_address_14bit() {
        let (b1, b2) = encode_buffer_address_14bit(256);
        assert_eq!(decode_buffer_address(b1, b2), 256);

        let (b1, b2) = encode_buffer_address_14bit(1920);
        assert_eq!(decode_buffer_address(b1, b2), 1920);
    }

    #[test]
    fn test_encode_decode_address_roundtrip() {
        // Test all valid 12-bit addresses (0-4095)
        for addr in 0..4096u16 {
            let (b1, b2) = encode_buffer_address(addr);
            let decoded = decode_buffer_address(b1, b2);
            assert_eq!(decoded, addr, "Roundtrip failed for address {}", addr);
        }
    }

    // -- Data stream parsing tests --

    #[test]
    fn test_parse_empty() {
        assert!(parse_data_stream(&[]).is_err());
    }

    #[test]
    fn test_parse_write_no_data() {
        // Write command + WCC, no orders
        let data = [CMD_WRITE, 0x00];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.command, WriteCommand::Write);
        assert!(!ds.wcc.reset_mdt);
        assert!(!ds.wcc.restore_keyboard);
        assert!(!ds.wcc.alarm);
        assert!(ds.orders.is_empty());
    }

    #[test]
    fn test_parse_erase_write_with_wcc() {
        let data = [CMD_ERASE_WRITE, 0x62]; // reset_mdt + restore_keyboard + alarm
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.command, WriteCommand::EraseWrite);
        assert!(ds.wcc.reset_mdt);
        assert!(ds.wcc.restore_keyboard);
        assert!(ds.wcc.alarm);
    }

    #[test]
    fn test_parse_character_data() {
        // Write + WCC + some EBCDIC characters (not order codes)
        let data = [CMD_WRITE, 0x00, 0xC8, 0xC5, 0xD3, 0xD3, 0xD6]; // "HELLO"
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 5);
        assert_eq!(ds.orders[0], DataStreamItem::Character(0xC8));
        assert_eq!(ds.orders[1], DataStreamItem::Character(0xC5));
        assert_eq!(ds.orders[2], DataStreamItem::Character(0xD3));
        assert_eq!(ds.orders[3], DataStreamItem::Character(0xD3));
        assert_eq!(ds.orders[4], DataStreamItem::Character(0xD6));
    }

    #[test]
    fn test_parse_sba_order() {
        // Write + WCC + SBA(address=80)
        // Address 80: high6=1, low6=16 -> encoded as 0xC1, 0x50
        let data = [CMD_WRITE, 0x00, ORDER_SBA, 0xC1, 0x50];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Sba(80)));
    }

    #[test]
    fn test_parse_sf_order() {
        // Write + WCC + SF(protected, normal intensity)
        let data = [CMD_WRITE, 0x00, ORDER_SF, 0x20];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        match &ds.orders[0] {
            DataStreamItem::Order(Order::Sf(attr)) => {
                assert!(attr.protected);
                assert!(!attr.numeric);
                assert_eq!(attr.intensity, Intensity::Normal);
                assert!(!attr.modified);
            }
            other => panic!("Expected SF order, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_ic_order() {
        let data = [CMD_WRITE, 0x00, ORDER_IC];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Ic));
    }

    #[test]
    fn test_parse_pt_order() {
        let data = [CMD_WRITE, 0x00, ORDER_PT];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Pt));
    }

    #[test]
    fn test_parse_ra_order() {
        // RA: repeat EBCDIC space (0x00) to address 160
        // Address 160: high6=2, low6=32 -> encoded
        let (b1, b2) = encode_buffer_address(160);
        let data = [CMD_WRITE, 0x00, ORDER_RA, b1, b2, 0x00];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Ra(160, 0x00)));
    }

    #[test]
    fn test_parse_eua_order() {
        let (b1, b2) = encode_buffer_address(240);
        let data = [CMD_WRITE, 0x00, ORDER_EUA, b1, b2];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Eua(240)));
    }

    #[test]
    fn test_parse_sfe_order() {
        // SFE: 1 pair (field attribute type 0xC0, value 0x20 = protected)
        let data = [CMD_WRITE, 0x00, ORDER_SFE, 0x01, 0xC0, 0x20];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        match &ds.orders[0] {
            DataStreamItem::Order(Order::Sfe(attrs)) => {
                assert_eq!(attrs.len(), 1);
                match attrs[0] {
                    ExtendedAttribute::FieldAttribute(fa) => {
                        assert!(fa.protected);
                    }
                    _ => panic!("Expected FieldAttribute"),
                }
            }
            other => panic!("Expected SFE order, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_sfe_multiple_pairs() {
        // SFE: 2 pairs (field attribute + foreground color)
        let data = [
            CMD_WRITE, 0x00, ORDER_SFE, 0x02, // count=2
            0xC0, 0x20, // field attr: protected
            0x42, 0xF2, // foreground: red
        ];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        match &ds.orders[0] {
            DataStreamItem::Order(Order::Sfe(attrs)) => {
                assert_eq!(attrs.len(), 2);
                match attrs[0] {
                    ExtendedAttribute::FieldAttribute(fa) => assert!(fa.protected),
                    _ => panic!("Expected FieldAttribute"),
                }
                match attrs[1] {
                    ExtendedAttribute::ForegroundColor(c) => assert_eq!(c, Color3270::Red),
                    _ => panic!("Expected ForegroundColor"),
                }
            }
            other => panic!("Expected SFE order, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_sa_order() {
        // SA: foreground green
        let data = [CMD_WRITE, 0x00, ORDER_SA, 0x42, 0xF4];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.orders.len(), 1);
        match &ds.orders[0] {
            DataStreamItem::Order(Order::Sa(ExtendedAttribute::ForegroundColor(c))) => {
                assert_eq!(*c, Color3270::Green);
            }
            other => panic!("Expected SA order with green, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_complex_stream() {
        // Simulate a typical mainframe login screen fragment:
        // EW + WCC(reset+restore) + SBA(0) + SF(protected) + "LOGON" + SBA(80) + SF(unprotected) + IC
        let (sba0_b1, sba0_b2) = encode_buffer_address(0);
        let (sba80_b1, sba80_b2) = encode_buffer_address(80);
        let data = [
            CMD_ERASE_WRITE,
            0x60, // WCC: reset_mdt + restore_keyboard
            ORDER_SBA,
            sba0_b1,
            sba0_b2, // SBA(0)
            ORDER_SF,
            0x20, // SF(protected)
            0xD3,
            0xD6,
            0xC7,
            0xD6,
            0xD5, // "LOGON"
            ORDER_SBA,
            sba80_b1,
            sba80_b2, // SBA(80)
            ORDER_SF,
            0x00, // SF(unprotected)
            ORDER_IC,
        ];

        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.command, WriteCommand::EraseWrite);
        assert!(ds.wcc.reset_mdt);
        assert!(ds.wcc.restore_keyboard);
        assert!(!ds.wcc.alarm);

        // Expected: SBA(0), SF(protected), 'L','O','G','O','N', SBA(80), SF(unprotected), IC
        assert_eq!(ds.orders.len(), 10);
        assert_eq!(ds.orders[0], DataStreamItem::Order(Order::Sba(0)));
        match &ds.orders[1] {
            DataStreamItem::Order(Order::Sf(attr)) => assert!(attr.protected),
            other => panic!("Expected SF, got {:?}", other),
        }
        // Characters L, O, G, O, N
        assert_eq!(ds.orders[2], DataStreamItem::Character(0xD3));
        assert_eq!(ds.orders[3], DataStreamItem::Character(0xD6));
        assert_eq!(ds.orders[4], DataStreamItem::Character(0xC7));
        assert_eq!(ds.orders[5], DataStreamItem::Character(0xD6));
        assert_eq!(ds.orders[6], DataStreamItem::Character(0xD5));
        assert_eq!(ds.orders[7], DataStreamItem::Order(Order::Sba(80)));
        match &ds.orders[8] {
            DataStreamItem::Order(Order::Sf(attr)) => assert!(!attr.protected),
            other => panic!("Expected SF, got {:?}", other),
        }
        assert_eq!(ds.orders[9], DataStreamItem::Order(Order::Ic));
    }

    #[test]
    fn test_parse_sna_erase_write() {
        let data = [CMD_ERASE_WRITE_SNA, 0x00];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.command, WriteCommand::EraseWrite);
    }

    #[test]
    fn test_parse_wsf() {
        // WSF has no WCC; remaining bytes are treated as raw data
        let data = [CMD_WRITE_STRUCTURED_FIELD, 0xAA, 0xBB, 0xCC];
        let ds = parse_data_stream(&data).unwrap();
        assert_eq!(ds.command, WriteCommand::WriteStructuredField);
        assert_eq!(ds.orders.len(), 3);
        assert_eq!(ds.orders[0], DataStreamItem::Character(0xAA));
    }

    #[test]
    fn test_parse_truncated_sba() {
        // SBA needs 2 bytes of address but only 1 is available
        let data = [CMD_WRITE, 0x00, ORDER_SBA, 0x40];
        assert!(parse_data_stream(&data).is_err());
    }

    #[test]
    fn test_parse_truncated_sf() {
        // SF needs 1 byte of attribute but none available
        let data = [CMD_WRITE, 0x00, ORDER_SF];
        assert!(parse_data_stream(&data).is_err());
    }

    #[test]
    fn test_parse_truncated_ra() {
        // RA needs 3 bytes but only 2 available
        let data = [CMD_WRITE, 0x00, ORDER_RA, 0x40, 0x40];
        assert!(parse_data_stream(&data).is_err());
    }

    // -- AID tests --

    #[test]
    fn test_aid_enter() {
        assert_eq!(Aid::from_byte(Aid::ENTER), Aid::Enter);
        assert_eq!(Aid::Enter.to_byte(), Aid::ENTER);
    }

    #[test]
    fn test_aid_clear() {
        assert_eq!(Aid::from_byte(Aid::CLEAR), Aid::Clear);
        assert_eq!(Aid::Clear.to_byte(), Aid::CLEAR);
    }

    #[test]
    fn test_aid_pa_keys() {
        assert_eq!(Aid::from_byte(Aid::PA1), Aid::Pa(1));
        assert_eq!(Aid::from_byte(Aid::PA2), Aid::Pa(2));
        assert_eq!(Aid::from_byte(Aid::PA3), Aid::Pa(3));
        assert_eq!(Aid::Pa(1).to_byte(), Aid::PA1);
        assert_eq!(Aid::Pa(2).to_byte(), Aid::PA2);
        assert_eq!(Aid::Pa(3).to_byte(), Aid::PA3);
    }

    #[test]
    fn test_aid_pf_keys() {
        // PF1 = 0xF1
        assert_eq!(Aid::from_byte(0xF1), Aid::Pf(1));
        assert_eq!(Aid::Pf(1).to_byte(), 0xF1);

        // PF12 = 0x7C
        assert_eq!(Aid::from_byte(0x7C), Aid::Pf(12));
        assert_eq!(Aid::Pf(12).to_byte(), 0x7C);

        // PF13 = 0xC1
        assert_eq!(Aid::from_byte(0xC1), Aid::Pf(13));
        assert_eq!(Aid::Pf(13).to_byte(), 0xC1);

        // PF24 = 0x4C
        assert_eq!(Aid::from_byte(0x4C), Aid::Pf(24));
        assert_eq!(Aid::Pf(24).to_byte(), 0x4C);
    }

    #[test]
    fn test_aid_roundtrip_all_pf() {
        for n in 1..=24u8 {
            let byte = Aid::Pf(n).to_byte();
            let parsed = Aid::from_byte(byte);
            assert_eq!(parsed, Aid::Pf(n), "PF{} roundtrip failed", n);
        }
    }

    // -- Color tests --

    #[test]
    fn test_color_from_byte() {
        assert_eq!(Color3270::from_byte(0x00), Color3270::Default);
        assert_eq!(Color3270::from_byte(0xF1), Color3270::Blue);
        assert_eq!(Color3270::from_byte(0xF2), Color3270::Red);
        assert_eq!(Color3270::from_byte(0xF3), Color3270::Pink);
        assert_eq!(Color3270::from_byte(0xF4), Color3270::Green);
        assert_eq!(Color3270::from_byte(0xF5), Color3270::Turquoise);
        assert_eq!(Color3270::from_byte(0xF6), Color3270::Yellow);
        assert_eq!(Color3270::from_byte(0xF7), Color3270::White);
    }

    #[test]
    fn test_color_roundtrip() {
        let colors = [
            Color3270::Blue,
            Color3270::Red,
            Color3270::Pink,
            Color3270::Green,
            Color3270::Turquoise,
            Color3270::Yellow,
            Color3270::White,
        ];
        for &color in &colors {
            assert_eq!(Color3270::from_byte(color.to_byte()), color);
        }
    }

    // -- Highlighting tests --

    #[test]
    fn test_highlighting_from_byte() {
        assert_eq!(Highlighting::from_byte(0x00), Highlighting::Default);
        assert_eq!(Highlighting::from_byte(0xF0), Highlighting::Default);
        assert_eq!(Highlighting::from_byte(0xF1), Highlighting::Blink);
        assert_eq!(Highlighting::from_byte(0xF2), Highlighting::ReverseVideo);
        assert_eq!(Highlighting::from_byte(0xF4), Highlighting::Underscore);
        assert_eq!(Highlighting::from_byte(0xF8), Highlighting::Intensified);
    }

    // -- Extended attribute tests --

    #[test]
    fn test_extended_attribute_field() {
        let ea = ExtendedAttribute::from_pair(0xC0, 0x20);
        match ea {
            ExtendedAttribute::FieldAttribute(fa) => {
                assert!(fa.protected);
            }
            other => panic!("Expected FieldAttribute, got {:?}", other),
        }
    }

    #[test]
    fn test_extended_attribute_highlighting() {
        let ea = ExtendedAttribute::from_pair(0x41, 0xF2);
        match ea {
            ExtendedAttribute::Highlighting(h) => {
                assert_eq!(h, Highlighting::ReverseVideo);
            }
            other => panic!("Expected Highlighting, got {:?}", other),
        }
    }

    #[test]
    fn test_extended_attribute_foreground() {
        let ea = ExtendedAttribute::from_pair(0x42, 0xF4);
        match ea {
            ExtendedAttribute::ForegroundColor(c) => {
                assert_eq!(c, Color3270::Green);
            }
            other => panic!("Expected ForegroundColor, got {:?}", other),
        }
    }

    #[test]
    fn test_extended_attribute_background() {
        let ea = ExtendedAttribute::from_pair(0x45, 0xF1);
        match ea {
            ExtendedAttribute::BackgroundColor(c) => {
                assert_eq!(c, Color3270::Blue);
            }
            other => panic!("Expected BackgroundColor, got {:?}", other),
        }
    }

    #[test]
    fn test_extended_attribute_charset() {
        let ea = ExtendedAttribute::from_pair(0x43, 0xF1);
        match ea {
            ExtendedAttribute::CharacterSet(v) => {
                assert_eq!(v, 0xF1);
            }
            other => panic!("Expected CharacterSet, got {:?}", other),
        }
    }

    #[test]
    fn test_extended_attribute_unknown() {
        let ea = ExtendedAttribute::from_pair(0xFF, 0xAB);
        match ea {
            ExtendedAttribute::Unknown { attr_type, value } => {
                assert_eq!(attr_type, 0xFF);
                assert_eq!(value, 0xAB);
            }
            other => panic!("Expected Unknown, got {:?}", other),
        }
    }
}
