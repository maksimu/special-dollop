//! MySQL packet parser
//!
//! This module provides functions to read and write MySQL protocol packets.
//! Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_basic_packets.html>

use super::packets::*;
use crate::error::{ProxyError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ============================================================================
// Packet Reading
// ============================================================================

/// Read a complete MySQL packet from a stream
///
/// Returns the packet header and payload bytes
pub async fn read_packet<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(PacketHeader, Vec<u8>)> {
    // Read 4-byte header
    let mut header_buf = [0u8; 4];
    reader.read_exact(&mut header_buf).await?;

    // Parse header: 3 bytes length (little-endian) + 1 byte sequence ID
    let payload_length = u32::from_le_bytes([header_buf[0], header_buf[1], header_buf[2], 0]);
    let sequence_id = header_buf[3];

    let header = PacketHeader::new(payload_length, sequence_id);

    // Read payload
    let mut payload = vec![0u8; payload_length as usize];
    reader.read_exact(&mut payload).await?;

    Ok((header, payload))
}

/// Parse a HandshakeV10 packet from payload bytes
pub fn parse_handshake_v10(payload: &[u8]) -> Result<HandshakeV10> {
    let mut cursor = 0;

    // Protocol version (1 byte)
    if payload.is_empty() {
        return Err(ProxyError::Protocol("Empty handshake packet".into()));
    }
    let protocol_version = payload[cursor];
    cursor += 1;

    if protocol_version != 10 {
        return Err(ProxyError::Protocol(format!(
            "Unsupported protocol version: {}",
            protocol_version
        )));
    }

    // Server version (null-terminated string)
    let (server_version, bytes_read) = read_null_terminated_string(&payload[cursor..])?;
    cursor += bytes_read;

    // Connection ID (4 bytes)
    let connection_id = read_u32_le(&payload[cursor..])?;
    cursor += 4;

    // Auth plugin data part 1 (8 bytes)
    let mut auth_plugin_data_part_1 = [0u8; 8];
    auth_plugin_data_part_1.copy_from_slice(&payload[cursor..cursor + 8]);
    cursor += 8;

    // Filler (1 byte, always 0x00)
    cursor += 1;

    // Capability flags lower (2 bytes)
    let capability_flags_lower = read_u16_le(&payload[cursor..])?;
    cursor += 2;

    // The following fields might not be present in older servers
    let mut character_set = 0x21u8;
    let mut status_flags = 0u16;
    let mut capability_flags_upper = 0u16;
    let mut auth_plugin_data_length = 0u8;
    let mut reserved = [0u8; 10];
    let mut auth_plugin_data_part_2 = Vec::new();
    let mut auth_plugin_name = String::new();

    if cursor < payload.len() {
        // Character set (1 byte)
        character_set = payload[cursor];
        cursor += 1;

        // Status flags (2 bytes)
        status_flags = read_u16_le(&payload[cursor..])?;
        cursor += 2;

        // Capability flags upper (2 bytes)
        capability_flags_upper = read_u16_le(&payload[cursor..])?;
        cursor += 2;

        // Auth plugin data length (1 byte) or 0x00
        auth_plugin_data_length = payload[cursor];
        cursor += 1;

        // Reserved (10 bytes)
        if cursor + 10 <= payload.len() {
            reserved.copy_from_slice(&payload[cursor..cursor + 10]);
            cursor += 10;
        }

        // Auth plugin data part 2 (if CLIENT_SECURE_CONNECTION)
        let combined_caps = (capability_flags_upper as u32) << 16 | capability_flags_lower as u32;
        if combined_caps & CLIENT_SECURE_CONNECTION != 0 {
            // Length is max(13, auth_plugin_data_length - 8)
            let part2_len = if auth_plugin_data_length > 8 {
                (auth_plugin_data_length - 8) as usize
            } else {
                13
            };
            let actual_len = std::cmp::min(part2_len, payload.len() - cursor);
            auth_plugin_data_part_2 = payload[cursor..cursor + actual_len].to_vec();
            // Remove trailing null if present
            if auth_plugin_data_part_2.last() == Some(&0) {
                auth_plugin_data_part_2.pop();
            }
            cursor += actual_len;
        }

        // Auth plugin name (if CLIENT_PLUGIN_AUTH)
        if combined_caps & CLIENT_PLUGIN_AUTH != 0 && cursor < payload.len() {
            let (name, _) = read_null_terminated_string(&payload[cursor..])?;
            auth_plugin_name = name;
        }
    }

    Ok(HandshakeV10 {
        protocol_version,
        server_version,
        connection_id,
        auth_plugin_data_part_1,
        capability_flags_lower,
        character_set,
        status_flags,
        capability_flags_upper,
        auth_plugin_data_length,
        reserved,
        auth_plugin_data_part_2,
        auth_plugin_name,
    })
}

/// Parse a HandshakeResponse41 packet from payload bytes
pub fn parse_handshake_response41(payload: &[u8]) -> Result<HandshakeResponse41> {
    let mut cursor = 0;

    // Capability flags (4 bytes)
    let capability_flags = read_u32_le(&payload[cursor..])?;
    cursor += 4;

    // Max packet size (4 bytes)
    let max_packet_size = read_u32_le(&payload[cursor..])?;
    cursor += 4;

    // Character set (1 byte)
    let character_set = payload[cursor];
    cursor += 1;

    // Reserved (23 bytes)
    let mut reserved = [0u8; 23];
    reserved.copy_from_slice(&payload[cursor..cursor + 23]);
    cursor += 23;

    // Username (null-terminated)
    let (username, bytes_read) = read_null_terminated_string(&payload[cursor..])?;
    cursor += bytes_read;

    // Auth response
    let auth_response = if capability_flags & CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA != 0 {
        // Length-encoded string
        let (len, len_bytes) = read_length_encoded_int(&payload[cursor..])?;
        cursor += len_bytes;
        let data = payload[cursor..cursor + len as usize].to_vec();
        cursor += len as usize;
        data
    } else if capability_flags & CLIENT_SECURE_CONNECTION != 0 {
        // Length-prefixed (1 byte length)
        let len = payload[cursor] as usize;
        cursor += 1;
        let data = payload[cursor..cursor + len].to_vec();
        cursor += len;
        data
    } else {
        // Null-terminated
        let (data_str, bytes_read) = read_null_terminated_string(&payload[cursor..])?;
        cursor += bytes_read;
        data_str.into_bytes()
    };

    // Database (if CLIENT_CONNECT_WITH_DB)
    let database = if capability_flags & CLIENT_CONNECT_WITH_DB != 0 && cursor < payload.len() {
        let (db, bytes_read) = read_null_terminated_string(&payload[cursor..])?;
        cursor += bytes_read;
        Some(db)
    } else {
        None
    };

    // Auth plugin name (if CLIENT_PLUGIN_AUTH)
    let auth_plugin_name = if capability_flags & CLIENT_PLUGIN_AUTH != 0 && cursor < payload.len() {
        let (name, bytes_read) = read_null_terminated_string(&payload[cursor..])?;
        cursor += bytes_read;
        Some(name)
    } else {
        None
    };

    // Connection attributes (if CLIENT_CONNECT_ATTRS)
    // Format: length-encoded total length, followed by pairs of length-encoded strings
    let has_connect_attrs_flag = capability_flags & CLIENT_CONNECT_ATTRS != 0;
    let has_remaining_data = cursor < payload.len();
    let connect_attrs = if has_connect_attrs_flag && has_remaining_data {
        let remaining = &payload[cursor..];
        match parse_connect_attrs(remaining) {
            Ok(attrs) => {
                debug!(
                    attrs_count = attrs.len(),
                    attrs = ?attrs,
                    "Parsed MySQL connect_attrs"
                );
                Some(attrs)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    remaining_bytes = remaining.len(),
                    "Failed to parse MySQL connect_attrs"
                );
                None
            }
        }
    } else {
        debug!(
            has_connect_attrs_flag,
            has_remaining_data,
            cursor,
            payload_len = payload.len(),
            "Skipping connect_attrs parsing"
        );
        None
    };

    Ok(HandshakeResponse41 {
        capability_flags,
        max_packet_size,
        character_set,
        reserved,
        username,
        auth_response,
        database,
        auth_plugin_name,
        connect_attrs,
    })
}

/// Parse an OK packet from payload bytes
pub fn parse_ok_packet(payload: &[u8], capabilities: u32) -> Result<OkPacket> {
    let mut cursor = 0;

    // Header (1 byte, 0x00 or 0xFE)
    let header = payload[cursor];
    cursor += 1;

    if header != 0x00 && header != 0xFE {
        return Err(ProxyError::Protocol(format!(
            "Invalid OK packet header: 0x{:02X}",
            header
        )));
    }

    // Affected rows (length-encoded int)
    let (affected_rows, bytes_read) = read_length_encoded_int(&payload[cursor..])?;
    cursor += bytes_read;

    // Last insert ID (length-encoded int)
    let (last_insert_id, bytes_read) = read_length_encoded_int(&payload[cursor..])?;
    cursor += bytes_read;

    // Status flags and warnings (if CLIENT_PROTOCOL_41)
    let (status_flags, warnings) = if capabilities & CLIENT_PROTOCOL_41 != 0 {
        let status = read_u16_le(&payload[cursor..])?;
        cursor += 2;
        let warns = read_u16_le(&payload[cursor..])?;
        cursor += 2;
        (status, warns)
    } else {
        (0, 0)
    };

    // Info string (rest of packet)
    let info = if cursor < payload.len() {
        String::from_utf8_lossy(&payload[cursor..]).to_string()
    } else {
        String::new()
    };

    Ok(OkPacket {
        header,
        affected_rows,
        last_insert_id,
        status_flags,
        warnings,
        info,
    })
}

/// Parse an ERR packet from payload bytes
pub fn parse_err_packet(payload: &[u8], capabilities: u32) -> Result<ErrPacket> {
    let mut cursor = 0;

    // Header (1 byte, 0xFF)
    let header = payload[cursor];
    cursor += 1;

    if header != 0xFF {
        return Err(ProxyError::Protocol(format!(
            "Invalid ERR packet header: 0x{:02X}",
            header
        )));
    }

    // Error code (2 bytes)
    let error_code = read_u16_le(&payload[cursor..])?;
    cursor += 2;

    // SQL state marker and state (if CLIENT_PROTOCOL_41)
    let (sql_state_marker, sql_state) = if capabilities & CLIENT_PROTOCOL_41 != 0 {
        let marker = payload[cursor] as char;
        cursor += 1;
        let mut state = [0u8; 5];
        state.copy_from_slice(&payload[cursor..cursor + 5]);
        cursor += 5;
        (marker, state)
    } else {
        ('#', *b"HY000")
    };

    // Error message (rest of packet)
    let error_message = String::from_utf8_lossy(&payload[cursor..]).to_string();

    Ok(ErrPacket {
        header,
        error_code,
        sql_state_marker,
        sql_state,
        error_message,
    })
}

/// Check if a packet is an OK packet
pub fn is_ok_packet(payload: &[u8]) -> bool {
    !payload.is_empty() && (payload[0] == 0x00 || payload[0] == 0xFE)
}

/// Check if a packet is an ERR packet
pub fn is_err_packet(payload: &[u8]) -> bool {
    !payload.is_empty() && payload[0] == 0xFF
}

/// Check if a packet is an EOF packet
pub fn is_eof_packet(payload: &[u8]) -> bool {
    !payload.is_empty() && payload[0] == 0xFE && payload.len() < 9
}

/// Parse connection attributes from the remaining handshake response bytes.
///
/// MySQL clients send attributes like `_program_name`, `_client_name`, `_pid`, etc.
/// These are formatted as length-encoded string pairs.
///
/// Format:
/// - Total length (length-encoded integer)
/// - Key-value pairs: each is (length-encoded string, length-encoded string)
fn parse_connect_attrs(data: &[u8]) -> Result<Vec<(String, String)>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut cursor = 0;
    let mut attrs = Vec::new();

    // Read total length of all attributes
    let (total_len, len_bytes) = read_length_encoded_int(data)?;
    cursor += len_bytes;

    if total_len == 0 {
        return Ok(attrs);
    }

    let end_pos = cursor + total_len as usize;
    if end_pos > data.len() {
        // Truncated - just return what we can parse
        return Ok(attrs);
    }

    // Parse key-value pairs
    while cursor < end_pos {
        // Read key (length-encoded string)
        let (key_len, key_len_bytes) = read_length_encoded_int(&data[cursor..])?;
        cursor += key_len_bytes;

        if cursor + key_len as usize > end_pos {
            break; // Truncated
        }

        let key = String::from_utf8_lossy(&data[cursor..cursor + key_len as usize]).to_string();
        cursor += key_len as usize;

        // Read value (length-encoded string)
        let (val_len, val_len_bytes) = read_length_encoded_int(&data[cursor..])?;
        cursor += val_len_bytes;

        if cursor + val_len as usize > end_pos {
            break; // Truncated
        }

        let value = String::from_utf8_lossy(&data[cursor..cursor + val_len as usize]).to_string();
        cursor += val_len as usize;

        attrs.push((key, value));
    }

    Ok(attrs)
}

// ============================================================================
// Helper Functions - Reading
// ============================================================================

/// Read a null-terminated string from a byte slice
/// Returns the string and the number of bytes consumed (including null terminator)
fn read_null_terminated_string(data: &[u8]) -> Result<(String, usize)> {
    let null_pos = data
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| ProxyError::Protocol("Missing null terminator".into()))?;

    let s = String::from_utf8_lossy(&data[..null_pos]).to_string();
    Ok((s, null_pos + 1))
}

/// Read a little-endian u16
fn read_u16_le(data: &[u8]) -> Result<u16> {
    if data.len() < 2 {
        return Err(ProxyError::Protocol("Not enough bytes for u16".into()));
    }
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

/// Read a little-endian u32
fn read_u32_le(data: &[u8]) -> Result<u32> {
    if data.len() < 4 {
        return Err(ProxyError::Protocol("Not enough bytes for u32".into()));
    }
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

/// Read a length-encoded integer
/// Returns the value and the number of bytes consumed
fn read_length_encoded_int(data: &[u8]) -> Result<(u64, usize)> {
    if data.is_empty() {
        return Err(ProxyError::Protocol(
            "Empty data for length-encoded int".into(),
        ));
    }

    match data[0] {
        // NULL (only in row data)
        0xFB => Ok((0, 1)),
        // 2-byte integer
        0xFC => {
            if data.len() < 3 {
                return Err(ProxyError::Protocol(
                    "Not enough bytes for 2-byte length".into(),
                ));
            }
            Ok((u16::from_le_bytes([data[1], data[2]]) as u64, 3))
        }
        // 3-byte integer
        0xFD => {
            if data.len() < 4 {
                return Err(ProxyError::Protocol(
                    "Not enough bytes for 3-byte length".into(),
                ));
            }
            Ok((u32::from_le_bytes([data[1], data[2], data[3], 0]) as u64, 4))
        }
        // 8-byte integer
        0xFE => {
            if data.len() < 9 {
                return Err(ProxyError::Protocol(
                    "Not enough bytes for 8-byte length".into(),
                ));
            }
            Ok((
                u64::from_le_bytes([
                    data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
                ]),
                9,
            ))
        }
        // 0xFF is reserved for ERR packet header
        0xFF => Err(ProxyError::Protocol(
            "Invalid length-encoded int marker 0xFF".into(),
        )),
        // 1-byte integer (0x00-0xFA)
        n => Ok((n as u64, 1)),
    }
}

// ============================================================================
// Packet Writing (Task 7)
// ============================================================================

/// Write a MySQL packet to a stream
pub async fn write_packet<W: AsyncWrite + Unpin>(
    writer: &mut W,
    sequence_id: u8,
    payload: &[u8],
) -> Result<()> {
    // Build header
    let len = payload.len() as u32;
    let header = [
        (len & 0xFF) as u8,
        ((len >> 8) & 0xFF) as u8,
        ((len >> 16) & 0xFF) as u8,
        sequence_id,
    ];

    // Write header and payload
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;

    Ok(())
}

/// Build a HandshakeV10 packet payload
pub fn build_handshake_v10(handshake: &HandshakeV10) -> Vec<u8> {
    let mut payload = Vec::with_capacity(128);

    // Protocol version
    payload.push(handshake.protocol_version);

    // Server version (null-terminated)
    payload.extend_from_slice(handshake.server_version.as_bytes());
    payload.push(0);

    // Connection ID
    payload.extend_from_slice(&handshake.connection_id.to_le_bytes());

    // Auth plugin data part 1
    payload.extend_from_slice(&handshake.auth_plugin_data_part_1);

    // Filler
    payload.push(0);

    // Capability flags lower
    payload.extend_from_slice(&handshake.capability_flags_lower.to_le_bytes());

    // Character set
    payload.push(handshake.character_set);

    // Status flags
    payload.extend_from_slice(&handshake.status_flags.to_le_bytes());

    // Capability flags upper
    payload.extend_from_slice(&handshake.capability_flags_upper.to_le_bytes());

    // Auth plugin data length
    payload.push(handshake.auth_plugin_data_length);

    // Reserved
    payload.extend_from_slice(&handshake.reserved);

    // Auth plugin data part 2
    payload.extend_from_slice(&handshake.auth_plugin_data_part_2);
    payload.push(0); // null terminator

    // Auth plugin name (null-terminated)
    payload.extend_from_slice(handshake.auth_plugin_name.as_bytes());
    payload.push(0);

    payload
}

/// Build a HandshakeResponse41 packet payload
pub fn build_handshake_response41(response: &HandshakeResponse41) -> Vec<u8> {
    let mut payload = Vec::with_capacity(128);

    // Capability flags
    payload.extend_from_slice(&response.capability_flags.to_le_bytes());

    // Max packet size
    payload.extend_from_slice(&response.max_packet_size.to_le_bytes());

    // Character set
    payload.push(response.character_set);

    // Reserved (23 bytes)
    payload.extend_from_slice(&response.reserved);

    // Username (null-terminated)
    payload.extend_from_slice(response.username.as_bytes());
    payload.push(0);

    // Auth response
    if response.capability_flags & CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA != 0 {
        // Length-encoded
        write_length_encoded_int(&mut payload, response.auth_response.len() as u64);
        payload.extend_from_slice(&response.auth_response);
    } else if response.capability_flags & CLIENT_SECURE_CONNECTION != 0 {
        // Length-prefixed (1 byte)
        payload.push(response.auth_response.len() as u8);
        payload.extend_from_slice(&response.auth_response);
    } else {
        // Null-terminated
        payload.extend_from_slice(&response.auth_response);
        payload.push(0);
    }

    // Database (if CLIENT_CONNECT_WITH_DB)
    if let Some(ref db) = response.database {
        if response.capability_flags & CLIENT_CONNECT_WITH_DB != 0 {
            payload.extend_from_slice(db.as_bytes());
            payload.push(0);
        }
    }

    // Auth plugin name (if CLIENT_PLUGIN_AUTH)
    if let Some(ref name) = response.auth_plugin_name {
        if response.capability_flags & CLIENT_PLUGIN_AUTH != 0 {
            payload.extend_from_slice(name.as_bytes());
            payload.push(0);
        }
    }

    payload
}

/// Build an OK packet payload
pub fn build_ok_packet(ok: &OkPacket, capabilities: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(32);

    // Header
    payload.push(ok.header);

    // Affected rows
    write_length_encoded_int(&mut payload, ok.affected_rows);

    // Last insert ID
    write_length_encoded_int(&mut payload, ok.last_insert_id);

    // Status flags and warnings (if CLIENT_PROTOCOL_41)
    if capabilities & CLIENT_PROTOCOL_41 != 0 {
        payload.extend_from_slice(&ok.status_flags.to_le_bytes());
        payload.extend_from_slice(&ok.warnings.to_le_bytes());
    }

    // Info string
    if !ok.info.is_empty() {
        payload.extend_from_slice(ok.info.as_bytes());
    }

    payload
}

/// Build an ERR packet payload
pub fn build_err_packet(err: &ErrPacket, capabilities: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(64);

    // Header
    payload.push(err.header);

    // Error code
    payload.extend_from_slice(&err.error_code.to_le_bytes());

    // SQL state marker and state (if CLIENT_PROTOCOL_41)
    if capabilities & CLIENT_PROTOCOL_41 != 0 {
        payload.push(err.sql_state_marker as u8);
        payload.extend_from_slice(&err.sql_state);
    }

    // Error message
    payload.extend_from_slice(err.error_message.as_bytes());

    payload
}

// ============================================================================
// Helper Functions - Writing
// ============================================================================

/// Write a length-encoded integer
fn write_length_encoded_int(buf: &mut Vec<u8>, value: u64) {
    if value < 251 {
        buf.push(value as u8);
    } else if value < 65536 {
        buf.push(0xFC);
        buf.extend_from_slice(&(value as u16).to_le_bytes());
    } else if value < 16777216 {
        buf.push(0xFD);
        buf.push((value & 0xFF) as u8);
        buf.push(((value >> 8) & 0xFF) as u8);
        buf.push(((value >> 16) & 0xFF) as u8);
    } else {
        buf.push(0xFE);
        buf.extend_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_length_encoded_int() {
        // Single byte (0-250)
        assert_eq!(read_length_encoded_int(&[0x00]).unwrap(), (0, 1));
        assert_eq!(read_length_encoded_int(&[0x05]).unwrap(), (5, 1));
        assert_eq!(read_length_encoded_int(&[0xFA]).unwrap(), (250, 1));

        // NULL marker (0xFB)
        assert_eq!(read_length_encoded_int(&[0xFB]).unwrap(), (0, 1));

        // Two bytes (0xFC)
        assert_eq!(
            read_length_encoded_int(&[0xFC, 0x01, 0x02]).unwrap(),
            (0x0201, 3)
        );
        assert_eq!(
            read_length_encoded_int(&[0xFC, 0xFF, 0xFF]).unwrap(),
            (65535, 3)
        );

        // Three bytes (0xFD)
        assert_eq!(
            read_length_encoded_int(&[0xFD, 0x01, 0x02, 0x03]).unwrap(),
            (0x030201, 4)
        );

        // Eight bytes (0xFE)
        assert_eq!(
            read_length_encoded_int(&[0xFE, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08])
                .unwrap(),
            (0x0807060504030201, 9)
        );

        // Error case (0xFF)
        assert!(read_length_encoded_int(&[0xFF]).is_err());

        // Empty data
        assert!(read_length_encoded_int(&[]).is_err());
    }

    #[test]
    fn test_null_terminated_string() {
        let data = b"hello\x00world";
        let (s, len) = read_null_terminated_string(data).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(len, 6);

        // Empty string
        let data = b"\x00rest";
        let (s, len) = read_null_terminated_string(data).unwrap();
        assert_eq!(s, "");
        assert_eq!(len, 1);

        // No null terminator
        assert!(read_null_terminated_string(b"no null").is_err());
    }

    #[test]
    fn test_handshake_v10_roundtrip() {
        let mut handshake = HandshakeV10 {
            server_version: "8.0.32".to_string(),
            connection_id: 12345,
            auth_plugin_data_part_1: [1, 2, 3, 4, 5, 6, 7, 8],
            auth_plugin_data_part_2: vec![9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20],
            auth_plugin_name: "mysql_native_password".to_string(),
            ..HandshakeV10::default()
        };
        handshake.set_capability_flags(DEFAULT_SERVER_CAPABILITIES);

        // Build packet
        let payload = build_handshake_v10(&handshake);

        // Parse it back
        let parsed = parse_handshake_v10(&payload).unwrap();

        assert_eq!(parsed.protocol_version, 10);
        assert_eq!(parsed.server_version, "8.0.32");
        assert_eq!(parsed.connection_id, 12345);
        assert_eq!(parsed.auth_plugin_data_part_1, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(parsed.auth_plugin_name, "mysql_native_password");
    }

    #[test]
    fn test_handshake_response41_roundtrip() {
        let response = HandshakeResponse41 {
            capability_flags: DEFAULT_CLIENT_CAPABILITIES | CLIENT_CONNECT_WITH_DB,
            username: "testuser".to_string(),
            auth_response: vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ],
            database: Some("testdb".to_string()),
            auth_plugin_name: Some("mysql_native_password".to_string()),
            ..HandshakeResponse41::default()
        };

        // Build packet
        let payload = build_handshake_response41(&response);

        // Parse it back
        let parsed = parse_handshake_response41(&payload).unwrap();

        assert_eq!(parsed.username, "testuser");
        assert_eq!(parsed.auth_response.len(), 20);
        assert_eq!(parsed.database, Some("testdb".to_string()));
        assert_eq!(
            parsed.auth_plugin_name,
            Some("mysql_native_password".to_string())
        );
    }

    #[test]
    fn test_ok_packet_roundtrip() {
        let ok = OkPacket {
            header: 0x00,
            affected_rows: 5,
            last_insert_id: 100,
            status_flags: 0x0002,
            warnings: 1,
            info: "Records: 5".to_string(),
        };

        let payload = build_ok_packet(&ok, CLIENT_PROTOCOL_41);
        let parsed = parse_ok_packet(&payload, CLIENT_PROTOCOL_41).unwrap();

        assert_eq!(parsed.header, 0x00);
        assert_eq!(parsed.affected_rows, 5);
        assert_eq!(parsed.last_insert_id, 100);
        assert_eq!(parsed.status_flags, 0x0002);
        assert_eq!(parsed.warnings, 1);
        assert_eq!(parsed.info, "Records: 5");
    }

    #[test]
    fn test_err_packet_roundtrip() {
        let err = ErrPacket::new(1045, "Access denied for user 'test'@'localhost'");

        let payload = build_err_packet(&err, CLIENT_PROTOCOL_41);
        let parsed = parse_err_packet(&payload, CLIENT_PROTOCOL_41).unwrap();

        assert_eq!(parsed.header, 0xFF);
        assert_eq!(parsed.error_code, 1045);
        assert_eq!(
            parsed.error_message,
            "Access denied for user 'test'@'localhost'"
        );
    }

    #[test]
    fn test_packet_type_detection() {
        // OK packet
        assert!(is_ok_packet(&[0x00, 0x00, 0x00]));
        assert!(is_ok_packet(&[0xFE, 0x00, 0x00, 0x00, 0x00])); // EOF as OK

        // ERR packet
        assert!(is_err_packet(&[0xFF, 0x00, 0x00]));

        // EOF packet (small 0xFE packet)
        assert!(is_eof_packet(&[0xFE, 0x00, 0x00, 0x02, 0x00]));
        assert!(!is_eof_packet(&[
            0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
        ])); // Too long

        // Empty payload
        assert!(!is_ok_packet(&[]));
        assert!(!is_err_packet(&[]));
        assert!(!is_eof_packet(&[]));
    }

    #[test]
    fn test_write_length_encoded_int() {
        let mut buf = Vec::new();

        // Single byte
        write_length_encoded_int(&mut buf, 100);
        assert_eq!(buf, vec![100]);

        // Two bytes
        buf.clear();
        write_length_encoded_int(&mut buf, 1000);
        assert_eq!(buf[0], 0xFC);

        // Three bytes
        buf.clear();
        write_length_encoded_int(&mut buf, 100000);
        assert_eq!(buf[0], 0xFD);

        // Eight bytes
        buf.clear();
        write_length_encoded_int(&mut buf, 0x1_0000_0000);
        assert_eq!(buf[0], 0xFE);
    }
}
