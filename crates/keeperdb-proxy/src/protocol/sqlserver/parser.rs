//! TDS packet parsing and serialization
//!
//! This module provides functions to parse and serialize TDS packets
//! for SQL Server communication.

use super::auth::{encode_password, string_to_utf16le, utf16le_to_string};
use super::constants::{
    packet_type, prelogin_token, status, version, EncryptionMode, LOGIN7_HEADER_SIZE,
    TDS_HEADER_SIZE,
};
use super::packets::{Login7, LoginAck, Prelogin, PreloginVersion, TdsHeader};

/// Error type for TDS parsing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TdsParseError {
    /// Packet too short
    TooShort,
    /// Invalid packet type
    InvalidPacketType(u8),
    /// Invalid token
    InvalidToken(u8),
    /// Invalid length
    InvalidLength,
    /// Invalid UTF-16 string
    InvalidString,
    /// Missing required field
    MissingField(&'static str),
    /// Offset out of bounds
    OffsetOutOfBounds,
}

impl std::fmt::Display for TdsParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TdsParseError::TooShort => write!(f, "Packet too short"),
            TdsParseError::InvalidPacketType(t) => write!(f, "Invalid packet type: 0x{:02X}", t),
            TdsParseError::InvalidToken(t) => write!(f, "Invalid token: 0x{:02X}", t),
            TdsParseError::InvalidLength => write!(f, "Invalid length field"),
            TdsParseError::InvalidString => write!(f, "Invalid UTF-16 string"),
            TdsParseError::MissingField(field) => write!(f, "Missing required field: {}", field),
            TdsParseError::OffsetOutOfBounds => write!(f, "Offset out of bounds"),
        }
    }
}

impl std::error::Error for TdsParseError {}

pub type Result<T> = std::result::Result<T, TdsParseError>;

// ============================================================================
// PRELOGIN Parsing and Serialization
// ============================================================================

/// Parse a PRELOGIN packet payload (without TDS header)
pub fn parse_prelogin(data: &[u8]) -> Result<Prelogin> {
    let mut prelogin = Prelogin::default();
    let mut offset = 0;

    // Parse token headers first to get offsets
    let mut tokens: Vec<(u8, u16, u16)> = Vec::new(); // (token, offset, length)

    while offset < data.len() {
        let token = data[offset];
        if token == prelogin_token::TERMINATOR {
            break;
        }

        if offset + 5 > data.len() {
            return Err(TdsParseError::TooShort);
        }

        let data_offset = u16::from_be_bytes([data[offset + 1], data[offset + 2]]);
        let data_length = u16::from_be_bytes([data[offset + 3], data[offset + 4]]);

        tokens.push((token, data_offset, data_length));
        offset += 5;
    }

    // Now parse each token's data
    for (token, data_offset, data_length) in tokens {
        let start = data_offset as usize;
        let end = start + data_length as usize;

        if end > data.len() {
            return Err(TdsParseError::OffsetOutOfBounds);
        }

        let token_data = &data[start..end];

        match token {
            prelogin_token::VERSION => {
                if token_data.len() >= 6 {
                    prelogin.version = PreloginVersion {
                        major: token_data[0],
                        minor: token_data[1],
                        build: u16::from_be_bytes([token_data[2], token_data[3]]),
                        sub_build: u16::from_be_bytes([token_data[4], token_data[5]]),
                    };
                }
            }
            prelogin_token::ENCRYPTION => {
                if !token_data.is_empty() {
                    prelogin.encryption =
                        EncryptionMode::from_byte(token_data[0]).unwrap_or(EncryptionMode::Off);
                }
            }
            prelogin_token::INSTOPT => {
                // Instance name is null-terminated ASCII
                if let Some(end) = token_data.iter().position(|&b| b == 0) {
                    prelogin.instance =
                        Some(String::from_utf8_lossy(&token_data[..end]).into_owned());
                }
            }
            prelogin_token::THREADID => {
                if token_data.len() >= 4 {
                    prelogin.thread_id = Some(u32::from_be_bytes([
                        token_data[0],
                        token_data[1],
                        token_data[2],
                        token_data[3],
                    ]));
                }
            }
            prelogin_token::MARS => {
                if !token_data.is_empty() {
                    prelogin.mars = Some(token_data[0] != 0);
                }
            }
            prelogin_token::TRACEID => {
                prelogin.trace_id = Some(token_data.to_vec());
            }
            _ => {
                // Ignore unknown tokens
            }
        }
    }

    Ok(prelogin)
}

/// Serialize a PRELOGIN packet (returns full packet including TDS header)
///
/// For client->server PRELOGIN, use packet_type::PRELOGIN (0x12)
/// For server->client PRELOGIN response, use packet_type::TABULAR_RESULT (0x04)
pub fn serialize_prelogin_with_type(prelogin: &Prelogin, pkt_type: u8) -> Vec<u8> {
    let mut tokens: Vec<u8> = Vec::new();
    let mut data: Vec<u8> = Vec::new();

    // Calculate starting offset for data (after all token headers + terminator)
    // Each token header is 5 bytes, plus 1 for terminator
    // We always include: VERSION, ENCRYPTION, INSTOPT, THREADID
    // Optionally include: MARS (if set)
    let mut token_count = 4;
    if prelogin.mars.is_some() {
        token_count += 1;
    }

    let data_start_offset = token_count * 5 + 1; // +1 for terminator
    let mut current_offset = data_start_offset;

    // VERSION token (required)
    let version_data = [
        prelogin.version.major,
        prelogin.version.minor,
        (prelogin.version.build >> 8) as u8,
        (prelogin.version.build & 0xFF) as u8,
        (prelogin.version.sub_build >> 8) as u8,
        (prelogin.version.sub_build & 0xFF) as u8,
    ];
    tokens.push(prelogin_token::VERSION);
    tokens.extend_from_slice(&(current_offset as u16).to_be_bytes());
    tokens.extend_from_slice(&(version_data.len() as u16).to_be_bytes());
    data.extend_from_slice(&version_data);
    current_offset += version_data.len();

    // ENCRYPTION token (required)
    let encryption_data = [prelogin.encryption.to_byte()];
    tokens.push(prelogin_token::ENCRYPTION);
    tokens.extend_from_slice(&(current_offset as u16).to_be_bytes());
    tokens.extend_from_slice(&(encryption_data.len() as u16).to_be_bytes());
    data.extend_from_slice(&encryption_data);
    current_offset += encryption_data.len();

    // INSTOPT token (instance name - null terminated, some drivers require this)
    let instopt_data = if let Some(ref instance) = prelogin.instance {
        let mut inst = instance.as_bytes().to_vec();
        inst.push(0); // null terminator
        inst
    } else {
        vec![0] // just null terminator for default instance
    };
    tokens.push(prelogin_token::INSTOPT);
    tokens.extend_from_slice(&(current_offset as u16).to_be_bytes());
    tokens.extend_from_slice(&(instopt_data.len() as u16).to_be_bytes());
    data.extend_from_slice(&instopt_data);
    current_offset += instopt_data.len();

    // THREADID token (process ID - some drivers expect this)
    let threadid_data = prelogin.thread_id.unwrap_or(0).to_be_bytes();
    tokens.push(prelogin_token::THREADID);
    tokens.extend_from_slice(&(current_offset as u16).to_be_bytes());
    tokens.extend_from_slice(&(threadid_data.len() as u16).to_be_bytes());
    data.extend_from_slice(&threadid_data);
    current_offset += threadid_data.len();

    // MARS token (optional - Multiple Active Result Sets)
    if let Some(mars_enabled) = prelogin.mars {
        let mars_data = [if mars_enabled { 1u8 } else { 0u8 }];
        tokens.push(prelogin_token::MARS);
        tokens.extend_from_slice(&(current_offset as u16).to_be_bytes());
        tokens.extend_from_slice(&(mars_data.len() as u16).to_be_bytes());
        data.extend_from_slice(&mars_data);
        // current_offset += mars_data.len(); // Not needed after last token
    }

    // Terminator
    tokens.push(prelogin_token::TERMINATOR);

    // Combine tokens and data
    let mut payload = tokens;
    payload.extend_from_slice(&data);

    // Create full packet with TDS header
    let total_length = TDS_HEADER_SIZE + payload.len();
    let header = TdsHeader::new(pkt_type, status::EOM, total_length as u16);

    let mut packet = Vec::with_capacity(total_length);
    packet.extend_from_slice(&header.serialize());
    packet.extend_from_slice(&payload);

    packet
}

/// Serialize a client PRELOGIN packet (convenience function, uses packet type 0x12)
pub fn serialize_prelogin(prelogin: &Prelogin) -> Vec<u8> {
    serialize_prelogin_with_type(prelogin, packet_type::PRELOGIN)
}

/// Serialize a server PRELOGIN response packet (uses packet type 0x04 TABULAR_RESULT)
pub fn serialize_prelogin_response(prelogin: &Prelogin) -> Vec<u8> {
    serialize_prelogin_with_type(prelogin, packet_type::TABULAR_RESULT)
}

// ============================================================================
// LOGIN7 Parsing and Serialization
// ============================================================================

/// Parse LOGIN7 packet payload (without TDS header)
///
/// Note: This extracts fields but does NOT decode the password.
/// The password remains encoded in the returned struct.
#[allow(clippy::field_reassign_with_default)]
pub fn parse_login7(data: &[u8]) -> Result<Login7> {
    if data.len() < LOGIN7_HEADER_SIZE {
        return Err(TdsParseError::TooShort);
    }

    let mut login = Login7::default();

    // Fixed header fields (all little-endian except where noted)
    login.length = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    login.tds_version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    login.packet_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    login.client_prog_ver = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    login.client_pid = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    login.connection_id = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    login.option_flags1 = data[24];
    login.option_flags2 = data[25];
    login.type_flags = data[26];
    login.option_flags3 = data[27];
    login.client_timezone = i32::from_le_bytes([data[28], data[29], data[30], data[31]]);
    login.client_lcid = u32::from_le_bytes([data[32], data[33], data[34], data[35]]);

    // Variable-length field offsets and lengths (each is 2 bytes offset + 2 bytes length)
    // Starting at offset 36

    let read_offset_length = |offset: usize| -> (u16, u16) {
        let off = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        (off, len)
    };

    let read_string = |off: u16, len: u16| -> Result<String> {
        let start = off as usize;
        let byte_len = (len as usize) * 2; // Length is in characters, not bytes
        let end = start + byte_len;
        if end > data.len() {
            return Err(TdsParseError::OffsetOutOfBounds);
        }
        utf16le_to_string(&data[start..end]).ok_or(TdsParseError::InvalidString)
    };

    // Parse variable-length fields
    // Offsets are from the start of the LOGIN7 packet

    // HostName: offset 36
    let (host_off, host_len) = read_offset_length(36);
    if host_len > 0 {
        login.hostname = read_string(host_off, host_len)?;
    }

    // UserName: offset 40
    let (user_off, user_len) = read_offset_length(40);
    if user_len > 0 {
        login.username = read_string(user_off, user_len)?;
    }

    // Password: offset 44 (stored encoded, we keep it as bytes)
    let (pass_off, pass_len) = read_offset_length(44);
    if pass_len > 0 {
        let start = pass_off as usize;
        let byte_len = (pass_len as usize) * 2;
        let end = start + byte_len;
        if end > data.len() {
            return Err(TdsParseError::OffsetOutOfBounds);
        }
        login.password = data[start..end].to_vec();
    }

    // AppName: offset 48
    let (app_off, app_len) = read_offset_length(48);
    if app_len > 0 {
        login.app_name = read_string(app_off, app_len)?;
    }

    // ServerName: offset 52
    let (srv_off, srv_len) = read_offset_length(52);
    if srv_len > 0 {
        login.server_name = read_string(srv_off, srv_len)?;
    }

    // Extension: offset 56 (skip for now)

    // LibraryName: offset 60
    let (lib_off, lib_len) = read_offset_length(60);
    if lib_len > 0 {
        login.library_name = read_string(lib_off, lib_len)?;
    }

    // Language: offset 64
    let (lang_off, lang_len) = read_offset_length(64);
    if lang_len > 0 {
        login.language = read_string(lang_off, lang_len)?;
    }

    // Database: offset 68
    let (db_off, db_len) = read_offset_length(68);
    if db_len > 0 {
        login.database = read_string(db_off, db_len)?;
    }

    // ClientID: offset 72 (6 bytes, fixed)
    login.client_id.copy_from_slice(&data[72..78]);

    // SSPI: offset 78
    let (sspi_off, sspi_len) = read_offset_length(78);
    if sspi_len > 0 {
        let start = sspi_off as usize;
        let end = start + sspi_len as usize;
        if end > data.len() {
            return Err(TdsParseError::OffsetOutOfBounds);
        }
        login.sspi = data[start..end].to_vec();
    }

    // AttachDBFile: offset 82
    let (attach_off, attach_len) = read_offset_length(82);
    if attach_len > 0 {
        login.attach_db_file = read_string(attach_off, attach_len)?;
    }

    // NewPassword: offset 86 (skip for now)
    // SSPILong: offset 90 (skip for now)

    Ok(login)
}

/// Serialize a LOGIN7 packet (returns full packet including TDS header)
///
/// This function encodes the password using TDS password encoding.
pub fn serialize_login7(login: &Login7) -> Vec<u8> {
    // Build variable-length data section
    let mut var_data: Vec<u8> = Vec::new();

    // Helper to add a string field and return (offset, length in chars)
    let add_string = |s: &str, var_data: &mut Vec<u8>| -> (u16, u16) {
        if s.is_empty() {
            return (0, 0);
        }
        let offset = LOGIN7_HEADER_SIZE + var_data.len();
        let utf16 = string_to_utf16le(s);
        let char_len = s.encode_utf16().count();
        var_data.extend_from_slice(&utf16);
        (offset as u16, char_len as u16)
    };

    // Add all variable-length fields in order
    let (host_off, host_len) = add_string(&login.hostname, &mut var_data);
    let (user_off, user_len) = add_string(&login.username, &mut var_data);

    // Password is special - needs encoding
    let (pass_off, pass_len) = if login.password.is_empty() {
        (0u16, 0u16)
    } else {
        let offset = LOGIN7_HEADER_SIZE + var_data.len();
        // Check if password is already encoded (from parsing) or raw (from builder)
        // If from builder, it's raw bytes that need encoding
        // We'll assume if it came from parse_login7, it's already encoded
        // For new logins, we need to encode
        let password_str = String::from_utf8_lossy(&login.password);
        let encoded = encode_password(&password_str);
        let char_len = password_str.encode_utf16().count();
        var_data.extend_from_slice(&encoded);
        (offset as u16, char_len as u16)
    };

    let (app_off, app_len) = add_string(&login.app_name, &mut var_data);
    let (srv_off, srv_len) = add_string(&login.server_name, &mut var_data);

    // Extension offset (we don't use extensions currently)
    let ext_off = 0u16;
    let ext_len = 0u16;

    let (lib_off, lib_len) = add_string(&login.library_name, &mut var_data);
    let (lang_off, lang_len) = add_string(&login.language, &mut var_data);
    let (db_off, db_len) = add_string(&login.database, &mut var_data);

    // SSPI (empty for SQL auth)
    let sspi_off = 0u16;
    let sspi_len = 0u16;

    // AttachDBFile
    let (attach_off, attach_len) = add_string(&login.attach_db_file, &mut var_data);

    // NewPassword (not used)
    let new_pass_off = 0u16;
    let new_pass_len = 0u16;

    // SSPILong (not used)
    let sspi_long = 0u32;

    // Calculate total length
    let total_login7_length = LOGIN7_HEADER_SIZE + var_data.len();

    // Build fixed header
    let mut header: Vec<u8> = Vec::with_capacity(LOGIN7_HEADER_SIZE);

    // Length (4 bytes)
    header.extend_from_slice(&(total_login7_length as u32).to_le_bytes());
    // TDS version (4 bytes)
    header.extend_from_slice(&login.tds_version.to_le_bytes());
    // Packet size (4 bytes)
    header.extend_from_slice(&login.packet_size.to_le_bytes());
    // Client program version (4 bytes)
    header.extend_from_slice(&login.client_prog_ver.to_le_bytes());
    // Client PID (4 bytes)
    header.extend_from_slice(&login.client_pid.to_le_bytes());
    // Connection ID (4 bytes)
    header.extend_from_slice(&login.connection_id.to_le_bytes());
    // Option flags (4 bytes)
    header.push(login.option_flags1);
    header.push(login.option_flags2);
    header.push(login.type_flags);
    header.push(login.option_flags3);
    // Client timezone (4 bytes)
    header.extend_from_slice(&login.client_timezone.to_le_bytes());
    // Client LCID (4 bytes)
    header.extend_from_slice(&login.client_lcid.to_le_bytes());

    // Variable-length field offsets and lengths
    // HostName
    header.extend_from_slice(&host_off.to_le_bytes());
    header.extend_from_slice(&host_len.to_le_bytes());
    // UserName
    header.extend_from_slice(&user_off.to_le_bytes());
    header.extend_from_slice(&user_len.to_le_bytes());
    // Password
    header.extend_from_slice(&pass_off.to_le_bytes());
    header.extend_from_slice(&pass_len.to_le_bytes());
    // AppName
    header.extend_from_slice(&app_off.to_le_bytes());
    header.extend_from_slice(&app_len.to_le_bytes());
    // ServerName
    header.extend_from_slice(&srv_off.to_le_bytes());
    header.extend_from_slice(&srv_len.to_le_bytes());
    // Extension
    header.extend_from_slice(&ext_off.to_le_bytes());
    header.extend_from_slice(&ext_len.to_le_bytes());
    // LibraryName
    header.extend_from_slice(&lib_off.to_le_bytes());
    header.extend_from_slice(&lib_len.to_le_bytes());
    // Language
    header.extend_from_slice(&lang_off.to_le_bytes());
    header.extend_from_slice(&lang_len.to_le_bytes());
    // Database
    header.extend_from_slice(&db_off.to_le_bytes());
    header.extend_from_slice(&db_len.to_le_bytes());
    // ClientID (6 bytes)
    header.extend_from_slice(&login.client_id);
    // SSPI
    header.extend_from_slice(&sspi_off.to_le_bytes());
    header.extend_from_slice(&sspi_len.to_le_bytes());
    // AttachDBFile
    header.extend_from_slice(&attach_off.to_le_bytes());
    header.extend_from_slice(&attach_len.to_le_bytes());
    // NewPassword
    header.extend_from_slice(&new_pass_off.to_le_bytes());
    header.extend_from_slice(&new_pass_len.to_le_bytes());
    // SSPILong
    header.extend_from_slice(&sspi_long.to_le_bytes());

    // Combine header and variable data
    let mut login7_payload = header;
    login7_payload.extend_from_slice(&var_data);

    // Create full packet with TDS header
    let total_packet_length = TDS_HEADER_SIZE + login7_payload.len();
    let tds_header = TdsHeader::new(packet_type::LOGIN7, status::EOM, total_packet_length as u16);

    let mut packet = Vec::with_capacity(total_packet_length);
    packet.extend_from_slice(&tds_header.serialize());
    packet.extend_from_slice(&login7_payload);

    packet
}

/// Modify a LOGIN7 packet in-place to replace username, password, and optionally database.
///
/// This preserves the original packet structure including extension data,
/// only modifying the username, password, and database fields. Returns a new packet
/// with updated credentials and recalculated offsets.
pub fn modify_login7_credentials(
    original_packet: &[u8], // Full packet including TDS header
    new_username: &str,
    new_password: &str,
    new_database: Option<&str>, // Optional database override
) -> std::result::Result<Vec<u8>, TdsParseError> {
    if original_packet.len() < TDS_HEADER_SIZE + LOGIN7_HEADER_SIZE {
        return Err(TdsParseError::TooShort);
    }

    let tds_header = &original_packet[..TDS_HEADER_SIZE];
    let login7_data = &original_packet[TDS_HEADER_SIZE..];

    // Parse offset/length pairs from original packet
    let read_u16 = |offset: usize| -> u16 {
        u16::from_le_bytes([login7_data[offset], login7_data[offset + 1]])
    };

    // Original field locations (offsets are from LOGIN7 start, not packet start)
    let ib_hostname = read_u16(36);
    let cch_hostname = read_u16(38);
    let _ib_username = read_u16(40); // Not used - replaced with new credentials
    let _cch_username = read_u16(42); // Not used - replaced with new credentials
    let _ib_password = read_u16(44); // Not used - replaced with new credentials
    let _cch_password = read_u16(46); // Not used - replaced with new credentials
    let ib_appname = read_u16(48);
    let cch_appname = read_u16(50);
    let ib_servername = read_u16(52);
    let cch_servername = read_u16(54);
    // Extension at 56-59
    let ib_cltintname = read_u16(60);
    let cch_cltintname = read_u16(62);
    let ib_language = read_u16(64);
    let cch_language = read_u16(66);
    let ib_database = read_u16(68);
    let cch_database = read_u16(70);
    // ClientID at 72-77 (6 bytes)
    let _ib_sspi = read_u16(78); // Not used - SQL auth doesn't use SSPI
    let _cch_sspi = read_u16(80); // Not used - SQL auth doesn't use SSPI
    let _ib_atchdbfile = read_u16(82); // Not used - not passing AttachDBFile
    let _cch_atchdbfile = read_u16(84); // Not used - not passing AttachDBFile
                                        // NewPassword at 86-87/88-89
                                        // SSPILong at 90-93

    // Extension data: For credential injection, we disable extensions to avoid
    // complex offset recalculation issues. Clear the fExtension bit and don't
    // copy extension data. Basic SQL authentication works without extensions.
    // Features like session recovery won't be available through the proxy.
    let option_flags3 = login7_data[27] & !0x10; // Clear fExtension bit

    // Helper to extract UTF-16LE string from original packet
    let extract_string = |ib: u16, cch: u16| -> Vec<u8> {
        if cch == 0 {
            return Vec::new();
        }
        let start = ib as usize;
        let end = start + (cch as usize * 2);
        if end <= login7_data.len() {
            login7_data[start..end].to_vec()
        } else {
            Vec::new()
        }
    };

    // Extract original strings we want to preserve
    let hostname_bytes = extract_string(ib_hostname, cch_hostname);
    let appname_bytes = extract_string(ib_appname, cch_appname);
    let servername_bytes = extract_string(ib_servername, cch_servername);
    let cltintname_bytes = extract_string(ib_cltintname, cch_cltintname);
    let language_bytes = extract_string(ib_language, cch_language);
    // Use new database if provided, otherwise use original from client
    let database_bytes = if let Some(db) = new_database {
        if !db.is_empty() {
            string_to_utf16le(db)
        } else {
            extract_string(ib_database, cch_database)
        }
    } else {
        extract_string(ib_database, cch_database)
    };
    // Note: sspi_bytes and atchdbfile_bytes are intentionally not extracted
    // since we set them to 0,0 for SQL authentication

    // Create new credential bytes
    let new_username_utf16 = string_to_utf16le(new_username);
    let new_password_encoded = encode_password(new_password);

    // Build variable data section with new credentials
    let mut var_data: Vec<u8> = Vec::new();

    // Track current offset (starts after LOGIN7 fixed header)
    let mut current_offset = LOGIN7_HEADER_SIZE as u16;

    // Helper to add bytes and return offset/length
    let add_bytes = |data: &[u8], var_data: &mut Vec<u8>, current: &mut u16| -> (u16, u16) {
        if data.is_empty() {
            return (0, 0);
        }
        let off = *current;
        let char_len = (data.len() / 2) as u16; // UTF-16 char count
        var_data.extend_from_slice(data);
        *current += data.len() as u16;
        (off, char_len)
    };

    // Add all fields in order
    let (new_ib_hostname, new_cch_hostname) =
        add_bytes(&hostname_bytes, &mut var_data, &mut current_offset);
    let (new_ib_username, new_cch_username) =
        add_bytes(&new_username_utf16, &mut var_data, &mut current_offset);
    let (new_ib_password, new_cch_password) =
        add_bytes(&new_password_encoded, &mut var_data, &mut current_offset);
    let (new_ib_appname, new_cch_appname) =
        add_bytes(&appname_bytes, &mut var_data, &mut current_offset);
    let (new_ib_servername, new_cch_servername) =
        add_bytes(&servername_bytes, &mut var_data, &mut current_offset);
    let (new_ib_cltintname, new_cch_cltintname) =
        add_bytes(&cltintname_bytes, &mut var_data, &mut current_offset);
    let (new_ib_language, new_cch_language) =
        add_bytes(&language_bytes, &mut var_data, &mut current_offset);
    let (new_ib_database, new_cch_database) =
        add_bytes(&database_bytes, &mut var_data, &mut current_offset);
    // SSPI and AttachDBFile - keep as 0,0 for SQL auth
    let (new_ib_sspi, new_cch_sspi) = (0u16, 0u16);
    let (new_ib_atchdbfile, new_cch_atchdbfile) = (0u16, 0u16);

    // Extension offset is 0 since we disabled extensions
    let extension_offset = 0u16;

    // Build new LOGIN7 fixed header
    let total_login7_length = LOGIN7_HEADER_SIZE + var_data.len();

    let mut new_header: Vec<u8> = Vec::with_capacity(LOGIN7_HEADER_SIZE);

    // Copy first 36 bytes (up to variable field offsets), but update length
    // option_flags3 has fExtension bit cleared to disable extensions
    new_header.extend_from_slice(&(total_login7_length as u32).to_le_bytes()); // Length
    new_header.extend_from_slice(&login7_data[4..24]); // TDSVersion through ConnectionID
    new_header.push(login7_data[24]); // option_flags1
    new_header.push(login7_data[25]); // option_flags2
    new_header.push(login7_data[26]); // type_flags
    new_header.push(option_flags3); // option_flags3 - fExtension bit cleared
    new_header.extend_from_slice(&login7_data[28..36]); // timezone and LCID

    // Write new offset/length pairs
    new_header.extend_from_slice(&new_ib_hostname.to_le_bytes());
    new_header.extend_from_slice(&new_cch_hostname.to_le_bytes());
    new_header.extend_from_slice(&new_ib_username.to_le_bytes());
    new_header.extend_from_slice(&new_cch_username.to_le_bytes());
    new_header.extend_from_slice(&new_ib_password.to_le_bytes());
    new_header.extend_from_slice(&new_cch_password.to_le_bytes());
    new_header.extend_from_slice(&new_ib_appname.to_le_bytes());
    new_header.extend_from_slice(&new_cch_appname.to_le_bytes());
    new_header.extend_from_slice(&new_ib_servername.to_le_bytes());
    new_header.extend_from_slice(&new_cch_servername.to_le_bytes());
    new_header.extend_from_slice(&extension_offset.to_le_bytes()); // Extension offset (updated if we have extension data)
    new_header.extend_from_slice(&0u16.to_le_bytes()); // Extension length (ignored, data has embedded length)
    new_header.extend_from_slice(&new_ib_cltintname.to_le_bytes());
    new_header.extend_from_slice(&new_cch_cltintname.to_le_bytes());
    new_header.extend_from_slice(&new_ib_language.to_le_bytes());
    new_header.extend_from_slice(&new_cch_language.to_le_bytes());
    new_header.extend_from_slice(&new_ib_database.to_le_bytes());
    new_header.extend_from_slice(&new_cch_database.to_le_bytes());
    new_header.extend_from_slice(&login7_data[72..78]); // ClientID (6 bytes)
    new_header.extend_from_slice(&new_ib_sspi.to_le_bytes());
    new_header.extend_from_slice(&new_cch_sspi.to_le_bytes());
    new_header.extend_from_slice(&new_ib_atchdbfile.to_le_bytes());
    new_header.extend_from_slice(&new_cch_atchdbfile.to_le_bytes());
    new_header.extend_from_slice(&0u16.to_le_bytes()); // NewPassword offset
    new_header.extend_from_slice(&0u16.to_le_bytes()); // NewPassword length
    new_header.extend_from_slice(&0u32.to_le_bytes()); // SSPILong

    // Combine header and variable data (no extension data)
    let mut login7_payload = new_header;
    login7_payload.extend_from_slice(&var_data);

    // Create new TDS header
    let total_packet_length = TDS_HEADER_SIZE + login7_payload.len();
    let mut new_packet = Vec::with_capacity(total_packet_length);

    // Copy original TDS header but update length
    new_packet.push(tds_header[0]); // packet_type
    new_packet.push(tds_header[1]); // status
    new_packet.extend_from_slice(&(total_packet_length as u16).to_be_bytes()); // length
    new_packet.extend_from_slice(&tds_header[4..8]); // spid, packet_id, window

    new_packet.extend_from_slice(&login7_payload);

    Ok(new_packet)
}

/// Create a LOGIN7 packet with credentials for injection
///
/// This is a convenience function for the proxy's credential injection.
pub fn create_login7_with_credentials(
    username: &str,
    password: &str,
    database: Option<&str>,
    app_name: &str,
    hostname: &str,
) -> Vec<u8> {
    let login = Login7 {
        username: username.to_string(),
        password: password.as_bytes().to_vec(), // Will be encoded during serialization
        app_name: app_name.to_string(),
        hostname: hostname.to_string(),
        database: database.unwrap_or("").to_string(),
        tds_version: version::TDS_7_4,
        ..Default::default()
    };

    serialize_login7(&login)
}

// ============================================================================
// LOGINACK Parsing
// ============================================================================

/// Parse a LOGINACK token from the response stream
///
/// Note: This parses just the LOGINACK token, not the full packet.
/// The token type byte should already be consumed.
pub fn parse_loginack(data: &[u8]) -> Result<LoginAck> {
    if data.len() < 10 {
        return Err(TdsParseError::TooShort);
    }

    let length = u16::from_le_bytes([data[0], data[1]]) as usize;
    if data.len() < length + 2 {
        return Err(TdsParseError::TooShort);
    }

    let interface = data[2];
    let tds_version = u32::from_le_bytes([data[3], data[4], data[5], data[6]]);

    // Program name length (1 byte) followed by UTF-16LE name
    let prog_name_len = data[7] as usize;
    let prog_name_start = 8;
    let prog_name_end = prog_name_start + prog_name_len * 2;

    if prog_name_end + 4 > data.len() {
        return Err(TdsParseError::TooShort);
    }

    let prog_name = utf16le_to_string(&data[prog_name_start..prog_name_end])
        .ok_or(TdsParseError::InvalidString)?;

    let version_start = prog_name_end;
    let prog_version = [
        data[version_start],
        data[version_start + 1],
        data[version_start + 2],
        data[version_start + 3],
    ];

    Ok(LoginAck {
        interface,
        tds_version,
        prog_name,
        prog_version,
    })
}

// ============================================================================
// Error Packet Building
// ============================================================================

use super::constants::token_type;

/// Build a TDS error response packet
///
/// This creates a complete TDS packet with ERROR and DONE tokens,
/// suitable for sending as a login failure response.
///
/// # Arguments
/// * `error_number` - SQL Server error number (e.g., 17809 for connection limit)
/// * `message` - Human-readable error message
/// * `server_name` - Server name to include in error
/// * `severity` - Error severity/class (14 = permission, 16 = general, 20 = fatal)
///
/// # Returns
/// Complete TDS packet bytes ready to send to client
pub fn build_error_packet(
    error_number: i32,
    message: &str,
    server_name: &str,
    severity: u8,
) -> Vec<u8> {
    // Build ERROR token (0xAA)
    let mut error_token = Vec::new();
    error_token.push(token_type::ERROR);

    // Message as UTF-16LE
    let msg_utf16: Vec<u8> = message
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();

    // Server name as UTF-16LE
    let server_name_utf16: Vec<u8> = server_name
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();

    // Proc name (empty)
    let proc_name_utf16: Vec<u8> = Vec::new();

    // Calculate token length (everything after length field)
    // Number(4) + State(1) + Class(1) + MsgLen(2) + Msg + ServerLen(1) + Server + ProcLen(1) + Proc + LineNum(4)
    let token_len: u16 = (4
        + 1
        + 1
        + 2
        + msg_utf16.len()
        + 1
        + server_name_utf16.len()
        + 1
        + proc_name_utf16.len()
        + 4) as u16;

    // Length (little-endian)
    error_token.extend_from_slice(&token_len.to_le_bytes());

    // Number (little-endian, signed i32)
    error_token.extend_from_slice(&error_number.to_le_bytes());

    // State
    error_token.push(1);

    // Class/Severity
    error_token.push(severity);

    // Message length (in characters, not bytes) and message
    let msg_char_len = (msg_utf16.len() / 2) as u16;
    error_token.extend_from_slice(&msg_char_len.to_le_bytes());
    error_token.extend_from_slice(&msg_utf16);

    // Server name length (in characters) and server name
    let server_char_len = (server_name_utf16.len() / 2) as u8;
    error_token.push(server_char_len);
    error_token.extend_from_slice(&server_name_utf16);

    // Proc name length (in characters) and proc name
    let proc_char_len = (proc_name_utf16.len() / 2) as u8;
    error_token.push(proc_char_len);
    error_token.extend_from_slice(&proc_name_utf16);

    // Line number (little-endian, 4 bytes for TDS 7.2+)
    error_token.extend_from_slice(&0u32.to_le_bytes());

    // Build DONE token (0xFD)
    // Format: Token(1) + Status(2) + CurCmd(2) + DoneRowCount(8 for TDS 7.2+)
    let mut done_token = Vec::new();
    done_token.push(token_type::DONE);
    done_token.extend_from_slice(&0x0002u16.to_le_bytes()); // Status: DONE_ERROR
    done_token.extend_from_slice(&0u16.to_le_bytes()); // CurCmd
    done_token.extend_from_slice(&0u64.to_le_bytes()); // DoneRowCount (8 bytes for TDS 7.2+)

    // Combine tokens
    let mut payload = error_token;
    payload.extend_from_slice(&done_token);

    // Build full TDS packet
    let total_len = TDS_HEADER_SIZE + payload.len();
    let header = TdsHeader::new(packet_type::TABULAR_RESULT, status::EOM, total_len as u16);

    let mut packet = header.serialize().to_vec();
    packet.extend_from_slice(&payload);

    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prelogin_serialize_parse_roundtrip() {
        let original = Prelogin {
            version: PreloginVersion::sql_server_2019(),
            encryption: EncryptionMode::Off,
            mars: Some(false),
            ..Default::default()
        };

        let packet = serialize_prelogin(&original);

        // Skip TDS header (8 bytes) to get payload
        let payload = &packet[TDS_HEADER_SIZE..];
        let parsed = parse_prelogin(payload).unwrap();

        assert_eq!(parsed.version.major, original.version.major);
        assert_eq!(parsed.version.minor, original.version.minor);
        assert_eq!(parsed.encryption, original.encryption);
        assert_eq!(parsed.mars, original.mars);
    }

    #[test]
    fn test_prelogin_packet_structure() {
        let prelogin = Prelogin {
            version: PreloginVersion::proxy_version(),
            encryption: EncryptionMode::Off,
            mars: Some(false),
            ..Default::default()
        };

        let packet = serialize_prelogin(&prelogin);

        // Check TDS header
        assert_eq!(packet[0], packet_type::PRELOGIN);
        assert_eq!(packet[1], status::EOM);

        // Length should be big-endian
        let length = u16::from_be_bytes([packet[2], packet[3]]);
        assert_eq!(length as usize, packet.len());
    }

    #[test]
    fn test_login7_builder_serialize() {
        let login = Login7::new()
            .with_credentials("testuser", "testpass")
            .with_database("testdb")
            .with_app_name("TestApp")
            .with_hostname("testhost");

        let packet = serialize_login7(&login);

        // Check TDS header
        assert_eq!(packet[0], packet_type::LOGIN7);
        assert_eq!(packet[1], status::EOM);

        // Packet should be reasonably sized
        assert!(packet.len() > TDS_HEADER_SIZE + LOGIN7_HEADER_SIZE);
    }

    #[test]
    fn test_create_login7_with_credentials() {
        let packet = create_login7_with_credentials(
            "sa",
            "Password123!",
            Some("master"),
            "TestApp",
            "testhost",
        );

        // Should be a valid TDS packet
        assert!(packet.len() > TDS_HEADER_SIZE);
        assert_eq!(packet[0], packet_type::LOGIN7);
    }

    // ========================================================================
    // Error Path Tests
    // ========================================================================

    #[test]
    fn test_parse_prelogin_too_short() {
        // Empty is valid (just terminator missing), but very short with partial token should fail
        let partial = [prelogin_token::VERSION, 0x00]; // Incomplete token header
        let result = parse_prelogin(&partial);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TdsParseError::TooShort));
    }

    #[test]
    fn test_parse_prelogin_offset_out_of_bounds() {
        // Create a token header pointing to an offset beyond the data
        let data = vec![
            prelogin_token::VERSION,
            0x00,
            0xFF, // Offset 255 (way beyond data)
            0x00,
            0x06, // Length 6
            prelogin_token::TERMINATOR,
        ];

        let result = parse_prelogin(&data);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TdsParseError::OffsetOutOfBounds
        ));
    }

    #[test]
    fn test_parse_prelogin_with_terminator_only() {
        // Just a terminator is valid (minimal prelogin)
        let data = [prelogin_token::TERMINATOR];
        let result = parse_prelogin(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_login7_too_short() {
        // Data shorter than LOGIN7_HEADER_SIZE should fail
        let short_data = vec![0u8; LOGIN7_HEADER_SIZE - 1];
        let result = parse_login7(&short_data);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TdsParseError::TooShort));
    }

    #[test]
    fn test_parse_login7_offset_out_of_bounds() {
        // Create a minimal LOGIN7 header with an offset pointing beyond data
        let mut data = vec![0u8; LOGIN7_HEADER_SIZE];

        // Set length field (bytes 0-3)
        let length = (LOGIN7_HEADER_SIZE as u32).to_le_bytes();
        data[0..4].copy_from_slice(&length);

        // Set hostname offset (bytes 36-37) to point way beyond data
        data[36] = 0xFF;
        data[37] = 0x00;
        // Set hostname length (bytes 38-39) to non-zero
        data[38] = 0x01;
        data[39] = 0x00;

        let result = parse_login7(&data);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TdsParseError::OffsetOutOfBounds
        ));
    }

    #[test]
    fn test_parse_loginack_too_short() {
        // LOGINACK needs at least 10 bytes
        let short_data = vec![0u8; 5];
        let result = parse_loginack(&short_data);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TdsParseError::TooShort));
    }

    #[test]
    fn test_parse_loginack_truncated_prog_name() {
        // Create a LOGINACK with program name length that exceeds data
        let mut data = vec![0u8; 12];
        data[0] = 20; // Length low byte
        data[1] = 0; // Length high byte
        data[2] = 1; // Interface
                     // TDS version (bytes 3-6)
        data[3..7].copy_from_slice(&version::TDS_7_4.to_le_bytes());
        data[7] = 50; // Prog name length = 50 chars (100 bytes needed)

        let result = parse_loginack(&data);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TdsParseError::TooShort));
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_login7_with_empty_credentials() {
        let login = Login7::new()
            .with_credentials("", "")
            .with_database("")
            .with_app_name("")
            .with_hostname("");

        let packet = serialize_login7(&login);

        // Should still produce a valid packet
        assert!(packet.len() >= TDS_HEADER_SIZE + LOGIN7_HEADER_SIZE);
        assert_eq!(packet[0], packet_type::LOGIN7);
    }

    #[test]
    fn test_login7_with_special_characters() {
        let login = Login7::new()
            .with_credentials("user@domain.com", "P@$$w0rd!#%^&*()")
            .with_database("test-db_2024")
            .with_app_name("My App v1.0")
            .with_hostname("host.example.com");

        let packet = serialize_login7(&login);
        assert!(packet.len() > TDS_HEADER_SIZE + LOGIN7_HEADER_SIZE);
    }

    #[test]
    fn test_login7_with_unicode_password() {
        let login = Login7::new()
            .with_credentials("testuser", "пароль密码كلمة")
            .with_database("testdb");

        let packet = serialize_login7(&login);
        assert!(packet.len() > TDS_HEADER_SIZE + LOGIN7_HEADER_SIZE);
    }

    #[test]
    fn test_prelogin_with_all_optional_fields() {
        let prelogin = Prelogin {
            version: PreloginVersion::sql_server_2019(),
            encryption: EncryptionMode::Required,
            instance: Some("MSSQLSERVER".to_string()),
            thread_id: Some(12345),
            mars: Some(true),
            trace_id: Some(vec![0u8; 36]),
        };

        let packet = serialize_prelogin(&prelogin);
        let payload = &packet[TDS_HEADER_SIZE..];
        let parsed = parse_prelogin(payload).unwrap();

        assert_eq!(parsed.encryption, EncryptionMode::Required);
        assert_eq!(parsed.mars, Some(true));
    }

    #[test]
    fn test_prelogin_encryption_modes() {
        for mode in [
            EncryptionMode::Off,
            EncryptionMode::On,
            EncryptionMode::NotSupported,
            EncryptionMode::Required,
        ] {
            let prelogin = Prelogin {
                encryption: mode,
                ..Default::default()
            };

            let packet = serialize_prelogin(&prelogin);
            let payload = &packet[TDS_HEADER_SIZE..];
            let parsed = parse_prelogin(payload).unwrap();

            assert_eq!(parsed.encryption, mode);
        }
    }

    #[test]
    fn test_login7_preserves_tds_version() {
        let login = Login7 {
            tds_version: version::TDS_7_4,
            ..Login7::default()
        };

        let packet = serialize_login7(&login);
        let payload = &packet[TDS_HEADER_SIZE..];
        let parsed = parse_login7(payload).unwrap();

        assert_eq!(parsed.tds_version, version::TDS_7_4);
    }

    #[test]
    fn test_login7_parse_serialize_roundtrip() {
        let original = Login7::new()
            .with_credentials("testuser", "testpass")
            .with_database("master")
            .with_app_name("TestApp")
            .with_hostname("DESKTOP-TEST");

        let packet = serialize_login7(&original);
        let payload = &packet[TDS_HEADER_SIZE..];
        let parsed = parse_login7(payload).unwrap();

        assert_eq!(parsed.username, original.username);
        assert_eq!(parsed.database, original.database);
        assert_eq!(parsed.app_name, original.app_name);
        assert_eq!(parsed.hostname, original.hostname);
    }

    // ========================================================================
    // Security Tests
    // ========================================================================

    #[test]
    fn test_password_is_encoded_in_packet() {
        let password = "SecretPassword123";
        let login = Login7::new().with_credentials("testuser", password);

        let packet = serialize_login7(&login);

        // The plaintext password should NOT appear in the packet
        let plaintext_utf16 = string_to_utf16le(password);

        // Search for plaintext password bytes in packet
        assert!(
            !packet
                .windows(plaintext_utf16.len())
                .any(|w| w == plaintext_utf16.as_slice()),
            "Plaintext password found in packet - encoding failed!"
        );
    }

    // ========================================================================
    // Error Packet Tests
    // ========================================================================

    #[test]
    fn test_build_error_packet_structure() {
        let packet = build_error_packet(17809, "Too many connections", "proxy", 16);

        // Verify TDS header
        assert_eq!(packet[0], packet_type::TABULAR_RESULT); // Packet type
        assert_eq!(packet[1], status::EOM); // Status - end of message

        // Verify ERROR token starts at offset 8 (after TDS header)
        assert_eq!(packet[8], token_type::ERROR);

        // Verify packet length in header matches actual length
        let header_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
        assert_eq!(header_len, packet.len());
    }

    #[test]
    fn test_build_error_packet_contains_message() {
        let message = "Connection rejected";
        let packet = build_error_packet(17809, message, "proxy", 16);

        // Message should be in UTF-16LE in the packet
        let msg_utf16: Vec<u8> = message
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();

        // Find the message in the packet
        assert!(
            packet
                .windows(msg_utf16.len())
                .any(|w| w == msg_utf16.as_slice()),
            "Error message not found in packet"
        );
    }

    #[test]
    fn test_build_error_packet_contains_done_token() {
        let packet = build_error_packet(17809, "Test error", "proxy", 16);

        // DONE token (0xFD) should be in the packet
        assert!(
            packet.contains(&token_type::DONE),
            "DONE token not found in packet"
        );
    }
}
