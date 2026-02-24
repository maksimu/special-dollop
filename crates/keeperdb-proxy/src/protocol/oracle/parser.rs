//! TNS packet parsing and serialization
//!
//! This module provides functions to parse and serialize TNS packets.
//! All multi-byte fields are big-endian.

use super::constants::*;
use super::packets::{
    AcceptPacket, ConnectPacket, DataPacket, RefusePacket, TnsHeader, ACCEPT_PACKET_FIXED_SIZE,
    CONNECT_PACKET_FIXED_SIZE, CONNECT_PACKET_MIN_SIZE, REFUSE_PACKET_FIXED_SIZE,
};
use crate::error::{ProxyError, Result};

/// Parse a TNS header from bytes
///
/// # Arguments
/// * `data` - At least 8 bytes of TNS packet data
///
/// # Returns
/// * `Ok(TnsHeader)` - Parsed header
/// * `Err` - If data is too short or invalid
pub fn parse_header(data: &[u8]) -> Result<TnsHeader> {
    if data.len() < TNS_HEADER_SIZE {
        return Err(ProxyError::Protocol(format!(
            "TNS header too short: {} bytes, need {}",
            data.len(),
            TNS_HEADER_SIZE
        )));
    }

    let length = u16::from_be_bytes([data[0], data[1]]);
    let packet_checksum = u16::from_be_bytes([data[2], data[3]]);
    let packet_type = data[4];
    let flags = data[5];
    let header_checksum = u16::from_be_bytes([data[6], data[7]]);

    // Validate length
    if (length as usize) < TNS_HEADER_SIZE {
        return Err(ProxyError::Protocol(format!(
            "TNS packet length too small: {}",
            length
        )));
    }

    if length as usize > TNS_MAX_PACKET_SIZE {
        return Err(ProxyError::Protocol(format!(
            "TNS packet length too large: {}",
            length
        )));
    }

    Ok(TnsHeader {
        length,
        packet_checksum,
        packet_type,
        flags,
        header_checksum,
    })
}

/// Serialize a TNS header to bytes
///
/// # Arguments
/// * `header` - The header to serialize
///
/// # Returns
/// * 8-byte vector containing the serialized header
pub fn serialize_header(header: &TnsHeader) -> Vec<u8> {
    let mut buf = Vec::with_capacity(TNS_HEADER_SIZE);
    buf.extend_from_slice(&header.length.to_be_bytes());
    buf.extend_from_slice(&header.packet_checksum.to_be_bytes());
    buf.push(header.packet_type);
    buf.push(header.flags);
    buf.extend_from_slice(&header.header_checksum.to_be_bytes());
    buf
}

/// Read a complete TNS packet from a buffer
///
/// # Arguments
/// * `data` - Buffer containing TNS packet data
///
/// # Returns
/// * `Ok((header, payload))` - Parsed header and payload bytes
/// * `Err` - If packet is incomplete or invalid
pub fn read_packet(data: &[u8]) -> Result<(TnsHeader, &[u8])> {
    let header = parse_header(data)?;
    let total_length = header.length as usize;

    if data.len() < total_length {
        return Err(ProxyError::Protocol(format!(
            "Incomplete TNS packet: have {} bytes, need {}",
            data.len(),
            total_length
        )));
    }

    let payload = &data[TNS_HEADER_SIZE..total_length];
    Ok((header, payload))
}

/// Build a complete TNS packet with header and payload
///
/// # Arguments
/// * `packet_type` - The packet type
/// * `payload` - The packet payload
///
/// # Returns
/// * Complete packet bytes including header
pub fn build_packet(packet_type: u8, payload: &[u8]) -> Vec<u8> {
    let header = TnsHeader::new(packet_type, payload.len());
    let mut packet = serialize_header(&header);
    packet.extend_from_slice(payload);
    packet
}

/// Check if a buffer contains a complete TNS packet
///
/// # Arguments
/// * `data` - Buffer to check
///
/// # Returns
/// * `Some(length)` - If complete packet present, returns total packet length
/// * `None` - If more data needed
pub fn packet_complete(data: &[u8]) -> Option<usize> {
    if data.len() < TNS_HEADER_SIZE {
        return None;
    }

    let length = u16::from_be_bytes([data[0], data[1]]) as usize;

    if !(TNS_HEADER_SIZE..=TNS_MAX_PACKET_SIZE).contains(&length) {
        // Invalid length - return header size to allow error handling
        return Some(TNS_HEADER_SIZE);
    }

    if data.len() >= length {
        Some(length)
    } else {
        None
    }
}

/// Update the length field in a TNS packet header
///
/// # Arguments
/// * `packet` - Mutable packet buffer (must be at least 2 bytes)
/// * `new_length` - New total packet length
pub fn update_packet_length(packet: &mut [u8], new_length: u16) {
    if packet.len() >= 2 {
        let bytes = new_length.to_be_bytes();
        packet[0] = bytes[0];
        packet[1] = bytes[1];
    }
}

/// Parse a TNS CONNECT packet from bytes
///
/// # Arguments
/// * `data` - Complete CONNECT packet data including header
///
/// # Returns
/// * `Ok(ConnectPacket)` - Parsed CONNECT packet
/// * `Err` - If packet is too short or malformed
pub fn parse_connect(data: &[u8]) -> Result<ConnectPacket> {
    if data.len() < CONNECT_PACKET_MIN_SIZE {
        return Err(ProxyError::Protocol(format!(
            "CONNECT packet too short: {} bytes, need at least {}",
            data.len(),
            CONNECT_PACKET_MIN_SIZE
        )));
    }

    let header = parse_header(data)?;

    if header.packet_type != packet_type::CONNECT {
        return Err(ProxyError::Protocol(format!(
            "Expected CONNECT packet (0x01), got 0x{:02x}",
            header.packet_type
        )));
    }

    let payload = &data[TNS_HEADER_SIZE..];

    // Parse fixed fields (all big-endian)
    let version = u16::from_be_bytes([payload[0], payload[1]]);
    let version_compatible = u16::from_be_bytes([payload[2], payload[3]]);
    let service_options = u16::from_be_bytes([payload[4], payload[5]]);
    let session_data_unit_size = u16::from_be_bytes([payload[6], payload[7]]);
    let maximum_transmission_data_unit_size = u16::from_be_bytes([payload[8], payload[9]]);
    let nt_protocol_characteristics = u16::from_be_bytes([payload[10], payload[11]]);
    let line_turnaround_value = u16::from_be_bytes([payload[12], payload[13]]);
    let value_of_one_in_hardware = u16::from_be_bytes([payload[14], payload[15]]);
    let connect_data_length = u16::from_be_bytes([payload[16], payload[17]]);
    let connect_data_offset = u16::from_be_bytes([payload[18], payload[19]]);
    let connect_data_max_recv =
        u32::from_be_bytes([payload[20], payload[21], payload[22], payload[23]]);
    let connect_flags_0 = payload[24];
    let connect_flags_1 = payload[25];

    // Extract connect data from offset
    let offset = connect_data_offset as usize;
    let length = connect_data_length as usize;

    if offset > data.len() || offset + length > data.len() {
        return Err(ProxyError::Protocol(format!(
            "CONNECT data offset/length invalid: offset={}, length={}, packet_len={}",
            offset,
            length,
            data.len()
        )));
    }

    let connect_data = if length > 0 {
        let raw = &data[offset..offset + length];
        // Trim leading and trailing non-printable bytes - JDBC thin clients may include
        // binary metadata before/after the actual connect descriptor text
        let start = raw
            .iter()
            .position(|&b| (0x20..=0x7E).contains(&b))
            .unwrap_or(raw.len());
        let end = raw
            .iter()
            .rposition(|&b| (0x20..=0x7E).contains(&b))
            .map_or(0, |p| p + 1);
        if start < end {
            String::from_utf8_lossy(&raw[start..end]).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    Ok(ConnectPacket {
        header,
        version,
        version_compatible,
        service_options,
        session_data_unit_size,
        maximum_transmission_data_unit_size,
        nt_protocol_characteristics,
        line_turnaround_value,
        value_of_one_in_hardware,
        connect_data_length,
        connect_data_offset,
        connect_data_max_recv,
        connect_flags_0,
        connect_flags_1,
        connect_data,
    })
}

/// Serialize a CONNECT packet to bytes
///
/// # Arguments
/// * `packet` - The CONNECT packet to serialize
///
/// # Returns
/// * Complete packet bytes including header
pub fn serialize_connect(packet: &ConnectPacket) -> Vec<u8> {
    let connect_data_bytes = packet.connect_data.as_bytes();
    let total_size = TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE + connect_data_bytes.len();

    let mut buf = Vec::with_capacity(total_size);

    // Build header with correct length
    let mut header = packet.header;
    header.length = total_size as u16;
    buf.extend_from_slice(&serialize_header(&header));

    // Fixed fields
    buf.extend_from_slice(&packet.version.to_be_bytes());
    buf.extend_from_slice(&packet.version_compatible.to_be_bytes());
    buf.extend_from_slice(&packet.service_options.to_be_bytes());
    buf.extend_from_slice(&packet.session_data_unit_size.to_be_bytes());
    buf.extend_from_slice(&packet.maximum_transmission_data_unit_size.to_be_bytes());
    buf.extend_from_slice(&packet.nt_protocol_characteristics.to_be_bytes());
    buf.extend_from_slice(&packet.line_turnaround_value.to_be_bytes());
    buf.extend_from_slice(&packet.value_of_one_in_hardware.to_be_bytes());
    buf.extend_from_slice(&(connect_data_bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(&((TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE) as u16).to_be_bytes());
    buf.extend_from_slice(&packet.connect_data_max_recv.to_be_bytes());
    buf.push(packet.connect_flags_0);
    buf.push(packet.connect_flags_1);

    // Connect data
    buf.extend_from_slice(connect_data_bytes);

    buf
}

/// Parse a TNS ACCEPT packet from bytes
///
/// # Arguments
/// * `data` - Complete ACCEPT packet data including header
///
/// # Returns
/// * `Ok(AcceptPacket)` - Parsed ACCEPT packet
/// * `Err` - If packet is too short or malformed
pub fn parse_accept(data: &[u8]) -> Result<AcceptPacket> {
    let min_size = TNS_HEADER_SIZE + ACCEPT_PACKET_FIXED_SIZE;
    if data.len() < min_size {
        return Err(ProxyError::Protocol(format!(
            "ACCEPT packet too short: {} bytes, need at least {}",
            data.len(),
            min_size
        )));
    }

    let header = parse_header(data)?;

    if header.packet_type != packet_type::ACCEPT {
        return Err(ProxyError::Protocol(format!(
            "Expected ACCEPT packet (0x02), got 0x{:02x}",
            header.packet_type
        )));
    }

    let payload = &data[TNS_HEADER_SIZE..];

    let version = u16::from_be_bytes([payload[0], payload[1]]);
    let service_options = u16::from_be_bytes([payload[2], payload[3]]);
    let session_data_unit_size = u16::from_be_bytes([payload[4], payload[5]]);
    let maximum_transmission_data_unit_size = u16::from_be_bytes([payload[6], payload[7]]);
    let value_of_one_in_hardware = u16::from_be_bytes([payload[8], payload[9]]);
    let accept_data_length = u16::from_be_bytes([payload[10], payload[11]]);
    let accept_data_offset = u16::from_be_bytes([payload[12], payload[13]]);
    let connect_flags_0 = payload[14];
    let connect_flags_1 = payload[15];

    // Extract accept data if present
    let accept_data = if accept_data_length > 0 {
        let offset = accept_data_offset as usize;
        let length = accept_data_length as usize;
        if offset + length <= data.len() {
            data[offset..offset + length].to_vec()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Ok(AcceptPacket {
        header,
        version,
        service_options,
        session_data_unit_size,
        maximum_transmission_data_unit_size,
        value_of_one_in_hardware,
        accept_data_length,
        accept_data_offset,
        connect_flags_0,
        connect_flags_1,
        accept_data,
    })
}

/// Serialize an ACCEPT packet to bytes
///
/// # Arguments
/// * `packet` - The ACCEPT packet to serialize
///
/// # Returns
/// * Complete packet bytes including header
pub fn serialize_accept(packet: &AcceptPacket) -> Vec<u8> {
    let total_size = TNS_HEADER_SIZE + ACCEPT_PACKET_FIXED_SIZE + packet.accept_data.len();

    let mut buf = Vec::with_capacity(total_size);

    // Build header with correct length
    let mut header = packet.header;
    header.length = total_size as u16;
    buf.extend_from_slice(&serialize_header(&header));

    // Fixed fields
    buf.extend_from_slice(&packet.version.to_be_bytes());
    buf.extend_from_slice(&packet.service_options.to_be_bytes());
    buf.extend_from_slice(&packet.session_data_unit_size.to_be_bytes());
    buf.extend_from_slice(&packet.maximum_transmission_data_unit_size.to_be_bytes());
    buf.extend_from_slice(&packet.value_of_one_in_hardware.to_be_bytes());
    buf.extend_from_slice(&(packet.accept_data.len() as u16).to_be_bytes());
    buf.extend_from_slice(&((TNS_HEADER_SIZE + ACCEPT_PACKET_FIXED_SIZE) as u16).to_be_bytes());
    buf.push(packet.connect_flags_0);
    buf.push(packet.connect_flags_1);

    // Accept data
    if !packet.accept_data.is_empty() {
        buf.extend_from_slice(&packet.accept_data);
    }

    buf
}

/// Parse a TNS REFUSE packet from bytes
///
/// # Arguments
/// * `data` - Complete REFUSE packet data including header
///
/// # Returns
/// * `Ok(RefusePacket)` - Parsed REFUSE packet
/// * `Err` - If packet is too short or malformed
pub fn parse_refuse(data: &[u8]) -> Result<RefusePacket> {
    let min_size = TNS_HEADER_SIZE + REFUSE_PACKET_FIXED_SIZE;
    if data.len() < min_size {
        return Err(ProxyError::Protocol(format!(
            "REFUSE packet too short: {} bytes, need at least {}",
            data.len(),
            min_size
        )));
    }

    let header = parse_header(data)?;

    if header.packet_type != packet_type::REFUSE {
        return Err(ProxyError::Protocol(format!(
            "Expected REFUSE packet (0x04), got 0x{:02x}",
            header.packet_type
        )));
    }

    let payload = &data[TNS_HEADER_SIZE..];

    let user_reason = payload[0];
    let system_reason = payload[1];
    let refuse_data_length = u16::from_be_bytes([payload[2], payload[3]]);

    // Extract refuse data
    let refuse_data = if refuse_data_length > 0 {
        let start = REFUSE_PACKET_FIXED_SIZE;
        let end = start + refuse_data_length as usize;
        if end <= payload.len() {
            String::from_utf8_lossy(&payload[start..end]).to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    Ok(RefusePacket {
        header,
        user_reason,
        system_reason,
        refuse_data_length,
        refuse_data,
    })
}

/// Serialize a REFUSE packet to bytes
///
/// # Arguments
/// * `packet` - The REFUSE packet to serialize
///
/// # Returns
/// * Complete packet bytes including header
pub fn serialize_refuse(packet: &RefusePacket) -> Vec<u8> {
    let refuse_data_bytes = packet.refuse_data.as_bytes();
    let total_size = TNS_HEADER_SIZE + REFUSE_PACKET_FIXED_SIZE + refuse_data_bytes.len();

    let mut buf = Vec::with_capacity(total_size);

    // Build header with correct length
    let mut header = packet.header;
    header.length = total_size as u16;
    buf.extend_from_slice(&serialize_header(&header));

    // Fixed fields
    buf.push(packet.user_reason);
    buf.push(packet.system_reason);
    buf.extend_from_slice(&(refuse_data_bytes.len() as u16).to_be_bytes());

    // Refuse data
    buf.extend_from_slice(refuse_data_bytes);

    buf
}

/// Parse a TNS DATA packet from bytes
///
/// # Arguments
/// * `data` - Complete DATA packet data including header
///
/// # Returns
/// * `Ok(DataPacket)` - Parsed DATA packet
/// * `Err` - If packet is too short or malformed
pub fn parse_data(data: &[u8]) -> Result<DataPacket> {
    let min_size = TNS_HEADER_SIZE + 2; // header + data_flags
    if data.len() < min_size {
        return Err(ProxyError::Protocol(format!(
            "DATA packet too short: {} bytes, need at least {}",
            data.len(),
            min_size
        )));
    }

    let header = parse_header(data)?;

    if header.packet_type != packet_type::DATA {
        return Err(ProxyError::Protocol(format!(
            "Expected DATA packet (0x06), got 0x{:02x}",
            header.packet_type
        )));
    }

    let payload = &data[TNS_HEADER_SIZE..];

    let data_flags = u16::from_be_bytes([payload[0], payload[1]]);
    let data_payload = payload[2..].to_vec();

    Ok(DataPacket {
        header,
        data_flags,
        payload: data_payload,
    })
}

/// Serialize a DATA packet to bytes
///
/// # Arguments
/// * `packet` - The DATA packet to serialize
///
/// # Returns
/// * Complete packet bytes including header
pub fn serialize_data(packet: &DataPacket) -> Vec<u8> {
    let total_size = TNS_HEADER_SIZE + 2 + packet.payload.len();

    let mut buf = Vec::with_capacity(total_size);

    // Build header with correct length
    let mut header = packet.header;
    header.length = total_size as u16;
    buf.extend_from_slice(&serialize_header(&header));

    // Data flags
    buf.extend_from_slice(&packet.data_flags.to_be_bytes());

    // Payload
    buf.extend_from_slice(&packet.payload);

    buf
}

/// Parsed components from an Oracle connect string
///
/// Contains the extracted connection parameters from either
/// DESCRIPTION format or Easy Connect syntax.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectStringParts {
    /// Network protocol (usually TCP)
    pub protocol: Option<String>,
    /// Database host
    pub host: Option<String>,
    /// Database port (default 1521)
    pub port: Option<u16>,
    /// Service name (preferred over SID)
    pub service_name: Option<String>,
    /// System Identifier (legacy, use service_name if available)
    pub sid: Option<String>,
    /// Username from CID section
    pub user: Option<String>,
    /// Program name from CID section
    pub program: Option<String>,
    /// Full connect string (for modification)
    pub raw: String,
}

impl ConnectStringParts {
    /// Get the effective port (default 1521 if not specified)
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(1521)
    }

    /// Get the service identifier (service_name preferred over sid)
    pub fn service_identifier(&self) -> Option<&str> {
        self.service_name.as_deref().or(self.sid.as_deref())
    }
}

/// Parse an Oracle connect string (DESCRIPTION or Easy Connect format)
///
/// # Arguments
/// * `connect_string` - The connect string to parse
///
/// # Returns
/// * `Ok(ConnectStringParts)` - Parsed components
/// * `Err` - If the format is invalid
///
/// # Supported Formats
///
/// ## DESCRIPTION format
/// ```text
/// (DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db.example.com)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)))
/// ```
///
/// ## Easy Connect format
/// ```text
/// host:port/service_name
/// host/service_name
/// host:port
/// ```
pub fn parse_connect_string(connect_string: &str) -> Result<ConnectStringParts> {
    let trimmed = connect_string.trim();

    if trimmed.starts_with('(') {
        parse_description_format(trimmed)
    } else {
        parse_easy_connect(trimmed)
    }
}

/// Parse DESCRIPTION format connect string
fn parse_description_format(connect_string: &str) -> Result<ConnectStringParts> {
    let mut parts = ConnectStringParts {
        raw: connect_string.to_string(),
        ..Default::default()
    };

    // Extract ADDRESS section content
    if let Some(addr_content) = extract_section_content(connect_string, "ADDRESS") {
        // Extract PROTOCOL
        parts.protocol = extract_value_from_section(&addr_content, "PROTOCOL");

        // Extract HOST
        parts.host = extract_value_from_section(&addr_content, "HOST");

        // Extract PORT
        if let Some(port_str) = extract_value_from_section(&addr_content, "PORT") {
            parts.port = port_str.parse().ok();
        }
    }

    // Extract CONNECT_DATA section content
    if let Some(cd_content) = extract_section_content(connect_string, "CONNECT_DATA") {
        // Extract SERVICE_NAME
        parts.service_name = extract_value_from_section(&cd_content, "SERVICE_NAME");

        // Extract SID (legacy)
        parts.sid = extract_value_from_section(&cd_content, "SID");

        // Extract CID components
        if let Some(cid_content) = extract_section_content(&cd_content, "CID") {
            parts.user = extract_value_from_section(&cid_content, "USER");
            parts.program = extract_value_from_section(&cid_content, "PROGRAM");
        }
    }

    Ok(parts)
}

/// Parse Easy Connect format: host[:port][/service_name]
fn parse_easy_connect(connect_string: &str) -> Result<ConnectStringParts> {
    let mut parts = ConnectStringParts {
        protocol: Some("TCP".to_string()),
        raw: connect_string.to_string(),
        ..Default::default()
    };

    // Split on / to get service name
    let (host_port, service) = if let Some(slash_pos) = connect_string.find('/') {
        let hp = &connect_string[..slash_pos];
        let svc = &connect_string[slash_pos + 1..];
        (hp, Some(svc))
    } else {
        (connect_string, None)
    };

    // Split on : to get port
    if let Some(colon_pos) = host_port.find(':') {
        parts.host = Some(host_port[..colon_pos].to_string());
        if let Ok(port) = host_port[colon_pos + 1..].parse() {
            parts.port = Some(port);
        }
    } else {
        parts.host = Some(host_port.to_string());
    }

    if let Some(svc) = service {
        if !svc.is_empty() {
            parts.service_name = Some(svc.to_string());
        }
    }

    Ok(parts)
}

/// Extract the content of a named section: (NAME=content)
/// Returns the content between the '=' and the matching ')'
fn extract_section_content(s: &str, section_name: &str) -> Option<String> {
    let search = format!("({}=", section_name);
    let start = s.find(&search)?;
    let content_start = start + search.len();
    let remaining = &s[content_start..];

    // Find the matching closing paren
    let mut depth = 1;
    for (i, c) in remaining.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(remaining[..i].to_string());
                }
            }
            _ => {}
        }
    }

    // Return all if unbalanced
    Some(remaining.to_string())
}

/// Extract a simple value from a (KEY=value) pair
fn extract_value_from_section(section: &str, key: &str) -> Option<String> {
    let search = format!("({}=", key);
    let key_start = section.find(&search)?;
    let value_start = key_start + search.len();
    let remaining = &section[value_start..];

    // Find the closing paren for this key-value pair
    // Handle potential nested parens in the value
    let mut depth = 1;
    for (i, c) in remaining.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let value = remaining[..i].trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                    return None;
                }
            }
            _ => {}
        }
    }

    // If we get here, take everything
    let value = remaining.trim();
    if !value.is_empty() {
        Some(value.to_string())
    } else {
        None
    }
}

/// Modify a connect string to inject or replace the USER in the CID section
///
/// # Arguments
/// * `connect_string` - The original connect string
/// * `new_user` - The username to inject
///
/// # Returns
/// * Modified connect string with USER replaced/added
pub fn modify_connect_string_user(connect_string: &str, new_user: &str) -> String {
    let trimmed = connect_string.trim();

    if !trimmed.starts_with('(') {
        // Easy Connect format - convert to DESCRIPTION format
        if let Ok(parts) = parse_easy_connect(trimmed) {
            return build_description_connect_string(&parts, Some(new_user));
        }
        return trimmed.to_string();
    }

    // Oracle TNS descriptors are case-insensitive, so use uppercase for searching
    let upper = trimmed.to_ascii_uppercase();

    // Check if CID section exists (case-insensitive)
    if let Some(cid_start) = upper.find("(CID=") {
        // Check if USER exists in CID
        if let Some(user_start) = upper[cid_start..].find("(USER=") {
            let abs_user_start = cid_start + user_start;
            // Find the closing paren for USER
            if let Some(user_end) = trimmed[abs_user_start..].find(')') {
                let abs_user_end = abs_user_start + user_end + 1;
                // Replace USER value
                let mut result = String::with_capacity(trimmed.len() + new_user.len());
                result.push_str(&trimmed[..abs_user_start]);
                result.push_str(&format!("(USER={})", new_user));
                result.push_str(&trimmed[abs_user_end..]);
                return result;
            }
        }

        // CID exists but no USER - add USER to CID
        // Find the closing paren for CID
        let cid_content_start = cid_start + 5; // "(CID=" is 5 chars, same in any case
        let section = &trimmed[cid_content_start..];
        if let Some(cid_end) = find_matching_paren(section) {
            let abs_cid_end = cid_content_start + cid_end;
            let mut result = String::with_capacity(trimmed.len() + new_user.len() + 10);
            result.push_str(&trimmed[..abs_cid_end]);
            result.push_str(&format!("(USER={})", new_user));
            result.push_str(&trimmed[abs_cid_end..]);
            return result;
        }
    }

    // Check if CONNECT_DATA exists - add CID with USER (case-insensitive)
    if let Some(cd_start) = upper.find("(CONNECT_DATA=") {
        let cd_content_start = cd_start + 14; // "(CONNECT_DATA=" is 14 chars
        let section = &trimmed[cd_content_start..];
        if let Some(cd_end) = find_matching_paren(section) {
            let abs_cd_end = cd_content_start + cd_end;
            let mut result = String::with_capacity(trimmed.len() + new_user.len() + 20);
            result.push_str(&trimmed[..abs_cd_end]);
            result.push_str(&format!("(CID=(USER={}))", new_user));
            result.push_str(&trimmed[abs_cd_end..]);
            return result;
        }
    }

    // No CONNECT_DATA - can't add user without it
    trimmed.to_string()
}

/// Modify a CONNECT packet's raw bytes to replace the USER in the CID section.
/// Operates directly on raw bytes to avoid UTF-8 conversion artifacts (e.g. CONNECTION_ID
/// values with binary data being corrupted by from_utf8_lossy round-trip).
///
/// Returns Some(modified_packet) on success, None if CID/USER not found in raw bytes.
pub fn modify_connect_packet_user(
    packet: &[u8],
    connect_data_offset: usize,
    connect_data_length: usize,
    new_user: &str,
) -> Option<Vec<u8>> {
    if connect_data_offset + connect_data_length > packet.len() {
        return None;
    }

    let connect_data = &packet[connect_data_offset..connect_data_offset + connect_data_length];

    // Create uppercase version for case-insensitive search
    let upper: Vec<u8> = connect_data
        .iter()
        .map(|b| b.to_ascii_uppercase())
        .collect();

    // Find CID section
    let cid_pos = upper.windows(5).position(|w| w == b"(CID=")?;

    // Find USER within CID section
    let cid_region = &upper[cid_pos..];
    let user_rel = cid_region.windows(6).position(|w| w == b"(USER=")?;
    let user_start = cid_pos + user_rel;

    // Find closing paren for USER value
    let close = connect_data[user_start..].iter().position(|&b| b == b')')?;
    let user_end = user_start + close + 1;

    let new_user_bytes = format!("(USER={})", new_user);
    let len_diff = new_user_bytes.len() as isize - (user_end - user_start) as isize;
    let new_total = (packet.len() as isize + len_diff) as usize;

    let abs_user_start = connect_data_offset + user_start;
    let abs_user_end = connect_data_offset + user_end;

    let mut result = Vec::with_capacity(new_total);
    result.extend_from_slice(&packet[..abs_user_start]);
    result.extend_from_slice(new_user_bytes.as_bytes());
    result.extend_from_slice(&packet[abs_user_end..]);

    // Update TNS header length (bytes 0-1, big-endian)
    let len_bytes = (result.len() as u16).to_be_bytes();
    result[0] = len_bytes[0];
    result[1] = len_bytes[1];

    // Update connect_data_length (TNS_HEADER_SIZE + 16 = byte 24)
    let new_cd_len = (connect_data_length as isize + len_diff) as u16;
    let cd_len_bytes = new_cd_len.to_be_bytes();
    let cd_len_offset = TNS_HEADER_SIZE + 16;
    result[cd_len_offset] = cd_len_bytes[0];
    result[cd_len_offset + 1] = cd_len_bytes[1];

    Some(result)
}

/// Modify a CONNECT packet's raw bytes to replace the ADDRESS section
/// (PROTOCOL, HOST, PORT) with the actual target values, and optionally
/// add a SECURITY section for TLS connections.
///
/// This is needed when the proxy connects to a different host than what
/// the client originally specified (e.g., client connects to localhost proxy,
/// proxy forwards to Oracle ADB at a remote host over TLS).
///
/// Reads connect_data_offset and connect_data_length directly from the packet header.
/// Returns Some(modified_packet) on success, None if ADDRESS section not found.
pub fn modify_connect_packet_address(
    packet: &[u8],
    target_host: &str,
    target_port: u16,
    target_protocol: &str,
    add_security: bool,
) -> Option<Vec<u8>> {
    // Read connect_data_offset and connect_data_length from packet header
    if packet.len() < TNS_HEADER_SIZE + 20 {
        return None;
    }
    let cd_len_offset = TNS_HEADER_SIZE + 16;
    let cd_off_offset = TNS_HEADER_SIZE + 18;
    let connect_data_length =
        u16::from_be_bytes([packet[cd_len_offset], packet[cd_len_offset + 1]]) as usize;
    let connect_data_offset =
        u16::from_be_bytes([packet[cd_off_offset], packet[cd_off_offset + 1]]) as usize;

    if connect_data_offset + connect_data_length > packet.len() {
        return None;
    }

    let connect_data = &packet[connect_data_offset..connect_data_offset + connect_data_length];

    // Create uppercase version for case-insensitive search
    let upper: Vec<u8> = connect_data
        .iter()
        .map(|b| b.to_ascii_uppercase())
        .collect();

    // Find ADDRESS section: (ADDRESS=...)
    let addr_keyword = b"(ADDRESS=";
    let addr_start = upper
        .windows(addr_keyword.len())
        .position(|w| w == addr_keyword)?;

    // Find matching close paren for the ADDRESS section
    let mut depth = 1;
    let search_start = addr_start + addr_keyword.len();
    let mut addr_end = None;
    for (offset, &byte) in connect_data[search_start..].iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    addr_end = Some(search_start + offset + 1); // include the closing paren
                    break;
                }
            }
            _ => {}
        }
    }
    let addr_end = addr_end?;

    // Build new ADDRESS section
    let new_address = format!(
        "(ADDRESS=(PROTOCOL={})(HOST={})(PORT={}))",
        target_protocol, target_host, target_port
    );

    // Check if SECURITY section already exists
    let has_security = upper.windows(10).any(|w| w == b"(SECURITY=");

    // Build the security section string to add (empty if not needed)
    let security_addition = if add_security && !has_security {
        "(SECURITY=(SSL_SERVER_DN_MATCH=YES))"
    } else {
        ""
    };

    // Calculate size changes
    let old_addr_len = addr_end - addr_start;
    let new_content_len = new_address.len() + security_addition.len();
    let len_diff = new_content_len as isize - old_addr_len as isize;
    let new_total = (packet.len() as isize + len_diff) as usize;

    let abs_addr_start = connect_data_offset + addr_start;
    let abs_addr_end = connect_data_offset + addr_end;

    let mut result = Vec::with_capacity(new_total);
    result.extend_from_slice(&packet[..abs_addr_start]);
    result.extend_from_slice(new_address.as_bytes());
    if !security_addition.is_empty() {
        result.extend_from_slice(security_addition.as_bytes());
    }
    result.extend_from_slice(&packet[abs_addr_end..]);

    // Update TNS header length (bytes 0-1, big-endian)
    let len_bytes = (result.len() as u16).to_be_bytes();
    result[0] = len_bytes[0];
    result[1] = len_bytes[1];

    // Update connect_data_length
    let new_cd_len = (connect_data_length as isize + len_diff) as u16;
    let cd_len_bytes = new_cd_len.to_be_bytes();
    result[cd_len_offset] = cd_len_bytes[0];
    result[cd_len_offset + 1] = cd_len_bytes[1];

    Some(result)
}

/// Rewrite SERVICE_NAME (or SID) in a CONNECT packet's CONNECT_DATA section.
///
/// When the proxy's target database differs from what the client specified
/// (e.g., client says SERVICE_NAME=XE but ephemeral user is in XEPDB1),
/// this rewrites the service identifier so Oracle routes to the correct container.
///
/// Handles both `(SERVICE_NAME=xxx)` and `(SID=xxx)` in CONNECT_DATA.
/// Returns Some(modified_packet) on success, None if neither found.
pub fn modify_connect_packet_service_name(packet: &[u8], target_database: &str) -> Option<Vec<u8>> {
    if packet.len() < TNS_HEADER_SIZE + 20 {
        return None;
    }
    let cd_len_offset = TNS_HEADER_SIZE + 16;
    let cd_off_offset = TNS_HEADER_SIZE + 18;
    let connect_data_length =
        u16::from_be_bytes([packet[cd_len_offset], packet[cd_len_offset + 1]]) as usize;
    let connect_data_offset =
        u16::from_be_bytes([packet[cd_off_offset], packet[cd_off_offset + 1]]) as usize;

    if connect_data_offset + connect_data_length > packet.len() {
        return None;
    }

    let connect_data = &packet[connect_data_offset..connect_data_offset + connect_data_length];

    // Create uppercase version for case-insensitive search
    let upper: Vec<u8> = connect_data
        .iter()
        .map(|b| b.to_ascii_uppercase())
        .collect();

    // Try SERVICE_NAME first, then SID
    let (key_start, value_start) =
        if let Some(pos) = upper.windows(14).position(|w| w == b"(SERVICE_NAME=") {
            (pos, pos + 14)
        } else if let Some(pos) = upper.windows(5).position(|w| w == b"(SID=") {
            (pos, pos + 5)
        } else {
            return None;
        };

    // Find the closing paren for this value
    let value_end = connect_data[value_start..]
        .iter()
        .position(|&b| b == b')')?;
    let abs_value_end = value_start + value_end; // index of ')'

    // Build replacement: keep original key, replace just the value
    let original_key = &connect_data[key_start..value_start];
    let replacement = format!(
        "{}{})",
        std::str::from_utf8(original_key).ok()?,
        target_database
    );

    let old_section_len = abs_value_end + 1 - key_start; // includes closing ')'
    let len_diff = replacement.len() as isize - old_section_len as isize;
    let new_total = (packet.len() as isize + len_diff) as usize;

    let abs_key_start = connect_data_offset + key_start;
    let abs_section_end = connect_data_offset + abs_value_end + 1;

    let mut result = Vec::with_capacity(new_total);
    result.extend_from_slice(&packet[..abs_key_start]);
    result.extend_from_slice(replacement.as_bytes());
    result.extend_from_slice(&packet[abs_section_end..]);

    // Update TNS header length (bytes 0-1, big-endian)
    let len_bytes = (result.len() as u16).to_be_bytes();
    result[0] = len_bytes[0];
    result[1] = len_bytes[1];

    // Update connect_data_length
    let new_cd_len = (connect_data_length as isize + len_diff) as u16;
    let cd_len_bytes = new_cd_len.to_be_bytes();
    result[cd_len_offset] = cd_len_bytes[0];
    result[cd_len_offset + 1] = cd_len_bytes[1];

    Some(result)
}

/// Strip CID and CONNECTION_ID sections from a CONNECT packet's raw bytes.
///
/// Oracle ADB over TCPS has a ~304 byte CONNECT packet size limit. The CID section
/// (containing PROGRAM, HOST, USER) and CONNECTION_ID are informational only — they're
/// not needed for authentication (which happens in the O5LOGON phase). Removing them
/// reduces the packet size enough to fit under the ADB limit.
///
/// Reads connect_data_offset and connect_data_length from the packet header.
/// Returns Some(modified_packet) if any sections were stripped, None if nothing to strip.
pub fn strip_connect_packet_cid_and_connid(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < TNS_HEADER_SIZE + 20 {
        return None;
    }
    let cd_len_offset = TNS_HEADER_SIZE + 16;
    let cd_off_offset = TNS_HEADER_SIZE + 18;
    let connect_data_length =
        u16::from_be_bytes([packet[cd_len_offset], packet[cd_len_offset + 1]]) as usize;
    let connect_data_offset =
        u16::from_be_bytes([packet[cd_off_offset], packet[cd_off_offset + 1]]) as usize;

    if connect_data_offset + connect_data_length > packet.len() {
        return None;
    }

    let connect_data = &packet[connect_data_offset..connect_data_offset + connect_data_length];
    let upper: Vec<u8> = connect_data
        .iter()
        .map(|b| b.to_ascii_uppercase())
        .collect();

    // Collect ranges to remove (relative to connect_data start)
    let mut removals: Vec<(usize, usize)> = Vec::new();

    // Find and mark CID section for removal: (CID=...)
    if let Some(cid_start) = upper.windows(5).position(|w| w == b"(CID=") {
        let mut depth = 1;
        let mut cid_end = None;
        for (offset, &byte) in connect_data[(cid_start + 5)..].iter().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        cid_end = Some(cid_start + 5 + offset + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end) = cid_end {
            removals.push((cid_start, end));
        }
    }

    // Find and mark CONNECTION_ID section for removal: (CONNECTION_ID=...)
    let connid_keyword = b"(CONNECTION_ID=";
    if let Some(connid_start) = upper
        .windows(connid_keyword.len())
        .position(|w| w == connid_keyword)
    {
        // CONNECTION_ID value may not have a closing paren (JDBC thin driver behavior)
        // Find the closing paren or use the rest of the connect data
        if let Some(rel_end) = connect_data[connid_start..].iter().position(|&b| b == b')') {
            removals.push((connid_start, connid_start + rel_end + 1));
        } else {
            // No closing paren — remove to end of connect data
            removals.push((connid_start, connect_data.len()));
        }
    }

    if removals.is_empty() {
        return None;
    }

    // Sort removals by start position (descending) so we can remove from end to start
    removals.sort_by(|a, b| b.0.cmp(&a.0));

    // Build new connect data with sections removed
    let mut new_cd = connect_data.to_vec();
    for (start, end) in &removals {
        new_cd.drain(*start..*end);
    }

    // Fix unbalanced parentheses: count open vs close parens in the connect string
    // (skip NSD preamble bytes which are non-ASCII). The original packet often has
    // CONNECTION_ID at the end without closing parens, and stripping it loses the
    // implicit end-of-data. We need to add closing parens for CONNECT_DATA and DESCRIPTION.
    let mut depth: i32 = 0;
    for &b in &new_cd {
        if b == b'(' {
            depth += 1;
        }
        if b == b')' {
            depth -= 1;
        }
    }
    if depth > 0 {
        new_cd.extend(std::iter::repeat_n(b')', depth as usize));
    }

    // Build new packet
    let mut result = Vec::with_capacity(connect_data_offset + new_cd.len());
    result.extend_from_slice(&packet[..connect_data_offset]);
    result.extend_from_slice(&new_cd);
    // Any data after connect data (unlikely but safe)
    let original_end = connect_data_offset + connect_data_length;
    if original_end < packet.len() {
        result.extend_from_slice(&packet[original_end..]);
    }

    // Update TNS header length
    let len_bytes = (result.len() as u16).to_be_bytes();
    result[0] = len_bytes[0];
    result[1] = len_bytes[1];

    // Update connect_data_length
    let new_cd_len = new_cd.len() as u16;
    let cd_len_bytes = new_cd_len.to_be_bytes();
    result[cd_len_offset] = cd_len_bytes[0];
    result[cd_len_offset + 1] = cd_len_bytes[1];

    debug!(
        "Stripped CID/CONNECTION_ID from CONNECT packet: {} -> {} bytes",
        connect_data_length,
        result.len()
    );

    Some(result)
}

/// Strip the NSD (Native Service Data) preamble from a CONNECT packet's connect data.
///
/// Oracle JDBC thin drivers prepend an NSD preamble (typically 10 bytes starting with
/// `01 0c 00 00 06 ...` or `01 34 00 00 06 ...`) before the connect string in the
/// connect data section. This is used for ANO (Advanced Networking Option) negotiation
/// of encryption and authentication services.
///
/// Over TCPS connections (TLS), TLS handles encryption/auth, so the NSD preamble is
/// not needed. Oracle ADB's TCPS listener treats NSD bytes as part of the connect
/// descriptor, causing ERR=1153 ("listener could not resolve connect descriptor").
///
/// This function removes any bytes before the first `(` in the connect data, effectively
/// stripping the NSD preamble. It updates the TNS packet length and cd_len fields.
///
/// Returns Some(modified_packet) if NSD was stripped, None if no NSD was present.
pub fn strip_connect_packet_nsd_preamble(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < TNS_HEADER_SIZE + 20 {
        return None;
    }

    let cd_len_offset = TNS_HEADER_SIZE + 16;
    let cd_off_offset = TNS_HEADER_SIZE + 18;
    let connect_data_length =
        u16::from_be_bytes([packet[cd_len_offset], packet[cd_len_offset + 1]]) as usize;
    let connect_data_offset =
        u16::from_be_bytes([packet[cd_off_offset], packet[cd_off_offset + 1]]) as usize;

    if connect_data_offset + connect_data_length > packet.len() {
        return None;
    }

    let connect_data = &packet[connect_data_offset..connect_data_offset + connect_data_length];

    // Check if connect data starts with '(' — if so, no NSD preamble present
    if connect_data.first() == Some(&b'(') {
        return None;
    }

    // Find where the connect string starts (first '(' character)
    let paren_pos = match connect_data.iter().position(|&b| b == b'(') {
        Some(pos) => pos,
        None => {
            return None;
        }
    };

    // Build new packet without the NSD preamble
    let new_cd = &connect_data[paren_pos..];
    let new_cd_len = new_cd.len();

    let mut result = Vec::with_capacity(connect_data_offset + new_cd_len);
    result.extend_from_slice(&packet[..connect_data_offset]);
    result.extend_from_slice(new_cd);

    // Update TNS header length
    let len_bytes = (result.len() as u16).to_be_bytes();
    result[0] = len_bytes[0];
    result[1] = len_bytes[1];

    // Update connect_data_length
    let cd_len_bytes = (new_cd_len as u16).to_be_bytes();
    result[cd_len_offset] = cd_len_bytes[0];
    result[cd_len_offset + 1] = cd_len_bytes[1];

    debug!(
        "Stripped {} byte NSD preamble from CONNECT packet",
        paren_pos
    );

    Some(result)
}

/// Find the position of the matching closing parenthesis
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Build a DESCRIPTION format connect string from parts
fn build_description_connect_string(parts: &ConnectStringParts, user: Option<&str>) -> String {
    let protocol = parts.protocol.as_deref().unwrap_or("TCP");
    let host = parts.host.as_deref().unwrap_or("localhost");
    let port = parts.effective_port();

    let mut result = format!(
        "(DESCRIPTION=(ADDRESS=(PROTOCOL={})(HOST={})(PORT={}))(CONNECT_DATA=",
        protocol, host, port
    );

    if let Some(svc) = &parts.service_name {
        result.push_str(&format!("(SERVICE_NAME={})", svc));
    } else if let Some(sid) = &parts.sid {
        result.push_str(&format!("(SID={})", sid));
    }

    if let Some(u) = user {
        result.push_str(&format!("(CID=(USER={}))", u));
    }

    result.push_str("))");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_header_valid() {
        // Create a valid header: length=58, checksum=0, type=CONNECT, flags=0, header_cksum=0
        let data = [
            0x00, 0x3A, // length = 58 (big-endian)
            0x00, 0x00, // packet checksum
            0x01, // type = CONNECT
            0x00, // flags
            0x00, 0x00, // header checksum
        ];

        let header = parse_header(&data).unwrap();
        assert_eq!(header.length, 58);
        assert_eq!(header.packet_checksum, 0);
        assert_eq!(header.packet_type, packet_type::CONNECT);
        assert_eq!(header.flags, 0);
        assert_eq!(header.header_checksum, 0);
    }

    #[test]
    fn test_parse_header_too_short() {
        let data = [0x00, 0x3A, 0x00]; // Only 3 bytes
        let result = parse_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_header_length_too_small() {
        let data = [
            0x00, 0x05, // length = 5 (less than header size)
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        let result = parse_header(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_header() {
        let header = TnsHeader {
            length: 100,
            packet_checksum: 0,
            packet_type: packet_type::DATA,
            flags: 0x20,
            header_checksum: 0,
        };

        let bytes = serialize_header(&header);
        assert_eq!(bytes.len(), TNS_HEADER_SIZE);
        assert_eq!(bytes[0], 0x00); // length high byte
        assert_eq!(bytes[1], 0x64); // length low byte (100)
        assert_eq!(bytes[4], packet_type::DATA);
        assert_eq!(bytes[5], 0x20); // flags
    }

    #[test]
    fn test_header_roundtrip() {
        let original = TnsHeader {
            length: 256,
            packet_checksum: 0x1234,
            packet_type: packet_type::ACCEPT,
            flags: 0x41,
            header_checksum: 0x5678,
        };

        let serialized = serialize_header(&original);
        let parsed = parse_header(&serialized).unwrap();

        assert_eq!(original, parsed);
    }

    #[test]
    fn test_read_packet() {
        let mut data = vec![
            0x00,
            0x0C, // length = 12 (8 header + 4 payload)
            0x00,
            0x00,
            packet_type::DATA,
            0x00,
            0x00,
            0x00,
            // payload
            0x01,
            0x02,
            0x03,
            0x04,
        ];

        let (header, payload) = read_packet(&data).unwrap();
        assert_eq!(header.length, 12);
        assert_eq!(payload, &[0x01, 0x02, 0x03, 0x04]);

        // Test incomplete packet
        data.pop();
        let result = read_packet(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_packet() {
        let payload = vec![0xAA, 0xBB, 0xCC];
        let packet = build_packet(packet_type::DATA, &payload);

        assert_eq!(packet.len(), TNS_HEADER_SIZE + 3);

        let (header, parsed_payload) = read_packet(&packet).unwrap();
        assert_eq!(header.packet_type, packet_type::DATA);
        assert_eq!(header.length, 11);
        assert_eq!(parsed_payload, &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn test_packet_complete() {
        // Too short for header
        assert_eq!(packet_complete(&[0x00, 0x10]), None);

        // Header says 16 bytes, but only 10 present
        let incomplete = [0x00, 0x10, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x02];
        assert_eq!(packet_complete(&incomplete), None);

        // Complete packet
        let complete = [0x00, 0x0A, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x02];
        assert_eq!(packet_complete(&complete), Some(10));
    }

    #[test]
    fn test_update_packet_length() {
        let mut packet = vec![0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        update_packet_length(&mut packet, 256);
        assert_eq!(packet[0], 0x01);
        assert_eq!(packet[1], 0x00);
    }

    #[test]
    fn test_connect_packet_roundtrip() {
        let connect_string = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=localhost)(PORT=1521)))";
        let original = ConnectPacket::new(connect_string.to_string());

        let serialized = serialize_connect(&original);
        let parsed = parse_connect(&serialized).unwrap();

        assert_eq!(parsed.version, original.version);
        assert_eq!(parsed.version_compatible, original.version_compatible);
        assert_eq!(
            parsed.session_data_unit_size,
            original.session_data_unit_size
        );
        assert_eq!(parsed.connect_data, original.connect_data);
    }

    #[test]
    fn test_parse_connect_too_short() {
        let data = vec![0x00, 0x10, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let result = parse_connect(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_connect_wrong_type() {
        // Create a packet with DATA type instead of CONNECT
        let mut data = vec![0; CONNECT_PACKET_MIN_SIZE];
        data[0] = 0x00;
        data[1] = 0x22; // length = 34
        data[4] = packet_type::DATA; // wrong type
        let result = parse_connect(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_accept_packet_roundtrip() {
        let original = AcceptPacket::new(318, 8192, 65535);

        let serialized = serialize_accept(&original);
        let parsed = parse_accept(&serialized).unwrap();

        assert_eq!(parsed.version, original.version);
        assert_eq!(
            parsed.session_data_unit_size,
            original.session_data_unit_size
        );
        assert_eq!(
            parsed.maximum_transmission_data_unit_size,
            original.maximum_transmission_data_unit_size
        );
    }

    #[test]
    fn test_parse_accept_too_short() {
        let data = vec![0x00, 0x10, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
        let result = parse_accept(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_refuse_packet_roundtrip() {
        let original = RefusePacket::new(1, 2, "Connection refused by server");

        let serialized = serialize_refuse(&original);
        let parsed = parse_refuse(&serialized).unwrap();

        assert_eq!(parsed.user_reason, original.user_reason);
        assert_eq!(parsed.system_reason, original.system_reason);
        assert_eq!(parsed.refuse_data, original.refuse_data);
    }

    #[test]
    fn test_refuse_packet_empty_message() {
        let original = RefusePacket::new(3, 0, "");

        let serialized = serialize_refuse(&original);
        let parsed = parse_refuse(&serialized).unwrap();

        assert_eq!(parsed.user_reason, 3);
        assert!(parsed.refuse_data.is_empty());
    }

    #[test]
    fn test_data_packet_roundtrip() {
        let payload = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let original = DataPacket::new(data_flags::EOF | data_flags::SEND_TOKEN, payload);

        let serialized = serialize_data(&original);
        let parsed = parse_data(&serialized).unwrap();

        assert_eq!(parsed.data_flags, original.data_flags);
        assert_eq!(parsed.payload, original.payload);
        assert!(parsed.is_eof());
    }

    #[test]
    fn test_data_packet_empty_payload() {
        let original = DataPacket::simple(vec![]);

        let serialized = serialize_data(&original);
        let parsed = parse_data(&serialized).unwrap();

        assert_eq!(parsed.data_flags, 0);
        assert!(parsed.payload.is_empty());
    }

    #[test]
    fn test_parse_data_wrong_type() {
        // Build an ACCEPT packet and try to parse as DATA
        let packet = AcceptPacket::new(318, 8192, 65535);
        let serialized = serialize_accept(&packet);
        let result = parse_data(&serialized);
        assert!(result.is_err());
    }

    // Connect String Parsing Tests

    #[test]
    fn test_parse_description_format_full() {
        let cs = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db.example.com)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(PROGRAM=sqlplus)(USER=scott))))";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.protocol, Some("TCP".to_string()));
        assert_eq!(parts.host, Some("db.example.com".to_string()));
        assert_eq!(parts.port, Some(1521));
        assert_eq!(parts.service_name, Some("ORCL".to_string()));
        assert_eq!(parts.user, Some("scott".to_string()));
        assert_eq!(parts.program, Some("sqlplus".to_string()));
    }

    #[test]
    fn test_parse_description_format_minimal() {
        let cs =
            "(DESCRIPTION=(ADDRESS=(HOST=localhost)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=XE)))";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.host, Some("localhost".to_string()));
        assert_eq!(parts.port, Some(1521));
        assert_eq!(parts.service_name, Some("XE".to_string()));
        assert_eq!(parts.user, None);
    }

    #[test]
    fn test_parse_description_with_sid() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=dbhost)(PORT=1522))(CONNECT_DATA=(SID=TESTDB)))";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.host, Some("dbhost".to_string()));
        assert_eq!(parts.port, Some(1522));
        assert_eq!(parts.sid, Some("TESTDB".to_string()));
        assert_eq!(parts.service_name, None);
        assert_eq!(parts.service_identifier(), Some("TESTDB"));
    }

    #[test]
    fn test_parse_easy_connect_full() {
        let cs = "db.example.com:1521/ORCL";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.protocol, Some("TCP".to_string()));
        assert_eq!(parts.host, Some("db.example.com".to_string()));
        assert_eq!(parts.port, Some(1521));
        assert_eq!(parts.service_name, Some("ORCL".to_string()));
    }

    #[test]
    fn test_parse_easy_connect_no_port() {
        let cs = "db.example.com/ORCL";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.host, Some("db.example.com".to_string()));
        assert_eq!(parts.port, None);
        assert_eq!(parts.effective_port(), 1521); // Default
        assert_eq!(parts.service_name, Some("ORCL".to_string()));
    }

    #[test]
    fn test_parse_easy_connect_host_only() {
        let cs = "localhost";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.host, Some("localhost".to_string()));
        assert_eq!(parts.port, None);
        assert_eq!(parts.service_name, None);
    }

    #[test]
    fn test_parse_easy_connect_host_port() {
        let cs = "localhost:1522";
        let parts = parse_connect_string(cs).unwrap();

        assert_eq!(parts.host, Some("localhost".to_string()));
        assert_eq!(parts.port, Some(1522));
        assert_eq!(parts.service_name, None);
    }

    #[test]
    fn test_modify_connect_string_replace_user() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(USER=olduser))))";
        let modified = modify_connect_string_user(cs, "newuser");

        assert!(modified.contains("(USER=newuser)"));
        assert!(!modified.contains("(USER=olduser)"));
    }

    #[test]
    fn test_modify_connect_string_add_user_to_cid() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(PROGRAM=test))))";
        let modified = modify_connect_string_user(cs, "testuser");

        assert!(modified.contains("(USER=testuser)"));
        assert!(modified.contains("(PROGRAM=test)"));
    }

    #[test]
    fn test_modify_connect_string_add_cid() {
        let cs = "(DESCRIPTION=(ADDRESS=(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=ORCL)))";
        let modified = modify_connect_string_user(cs, "newuser");

        assert!(modified.contains("(CID=(USER=newuser))"));
    }

    #[test]
    fn test_modify_easy_connect_converts_to_description() {
        let cs = "db.example.com:1521/ORCL";
        let modified = modify_connect_string_user(cs, "testuser");

        assert!(modified.starts_with("(DESCRIPTION="));
        assert!(modified.contains("(HOST=db.example.com)"));
        assert!(modified.contains("(PORT=1521)"));
        assert!(modified.contains("(SERVICE_NAME=ORCL)"));
        assert!(modified.contains("(USER=testuser)"));
    }

    #[test]
    fn test_connect_string_parts_effective_port() {
        let mut parts = ConnectStringParts::default();
        assert_eq!(parts.effective_port(), 1521);

        parts.port = Some(1522);
        assert_eq!(parts.effective_port(), 1522);
    }

    #[test]
    fn test_connect_string_parts_service_identifier() {
        let mut parts = ConnectStringParts::default();
        assert_eq!(parts.service_identifier(), None);

        parts.sid = Some("TESTSID".to_string());
        assert_eq!(parts.service_identifier(), Some("TESTSID"));

        // service_name takes precedence
        parts.service_name = Some("TESTSVC".to_string());
        assert_eq!(parts.service_identifier(), Some("TESTSVC"));
    }

    #[test]
    fn test_modify_connect_packet_address() {
        // Build a minimal CONNECT packet with known ADDRESS section
        let connect_data = b"(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=127.0.0.1)(PORT=11521))(CONNECT_DATA=(SERVICE_NAME=ORCL)(CID=(USER=test))))";
        let cd_offset: u16 = (TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE) as u16;
        let cd_len: u16 = connect_data.len() as u16;

        // Build packet: TNS header + connect fixed fields + connect data
        let total_len = cd_offset as usize + connect_data.len();
        let mut packet = vec![0u8; total_len];
        // TNS header: length (2 bytes BE)
        let len_bytes = (total_len as u16).to_be_bytes();
        packet[0] = len_bytes[0];
        packet[1] = len_bytes[1];
        // Packet type = CONNECT (1)
        packet[4] = 1;
        // connect_data_length at TNS_HEADER_SIZE + 16
        let cd_len_off = TNS_HEADER_SIZE + 16;
        packet[cd_len_off] = (cd_len >> 8) as u8;
        packet[cd_len_off + 1] = cd_len as u8;
        // connect_data_offset at TNS_HEADER_SIZE + 18
        let cd_off_off = TNS_HEADER_SIZE + 18;
        packet[cd_off_off] = (cd_offset >> 8) as u8;
        packet[cd_off_off + 1] = cd_offset as u8;
        // Copy connect data
        packet[cd_offset as usize..].copy_from_slice(connect_data);

        // Modify ADDRESS to point to ADB
        let result = modify_connect_packet_address(
            &packet,
            "adb.us-sanjose-1.oraclecloud.com",
            1522,
            "TCPS",
            true,
        );
        assert!(result.is_some());
        let modified = result.unwrap();

        // Extract modified connect data
        let new_cd_len =
            u16::from_be_bytes([modified[cd_len_off], modified[cd_len_off + 1]]) as usize;
        let new_cd = &modified[cd_offset as usize..cd_offset as usize + new_cd_len];
        let cd_str = std::str::from_utf8(new_cd).unwrap();

        // Verify ADDRESS was replaced
        assert!(
            cd_str.contains(
                "(ADDRESS=(PROTOCOL=TCPS)(HOST=adb.us-sanjose-1.oraclecloud.com)(PORT=1522))"
            ),
            "ADDRESS not updated: {}",
            cd_str
        );
        // Verify SECURITY was added
        assert!(
            cd_str.contains("(SECURITY=(SSL_SERVER_DN_MATCH=YES))"),
            "SECURITY not added: {}",
            cd_str
        );
        // Verify CONNECT_DATA preserved
        assert!(
            cd_str.contains("(SERVICE_NAME=ORCL)"),
            "SERVICE_NAME lost: {}",
            cd_str
        );
        assert!(cd_str.contains("(USER=test)"), "USER lost: {}", cd_str);
        // Verify TNS header length is correct
        let tns_len = u16::from_be_bytes([modified[0], modified[1]]) as usize;
        assert_eq!(tns_len, modified.len());
    }

    #[test]
    fn test_strip_connect_packet_cid_and_connid() {
        // Exact packet from debug log (354 bytes after ADDRESS modification)
        let header_hex = "01 62 00 00 01 00 00 00 01 3e 01 2c 0c 41 20 00 ff ff 4f 98 00 00 00 01 01 18 00 4a 00 00 00 00 88 88 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 20 00 00 20 00 00 00 00 00 00 00 00 00 01";
        let cd_hex = "01 0c 00 00 06 00 00 00 00 00 28 44 45 53 43 52 49 50 54 49 4f 4e 3d 28 41 44 44 52 45 53 53 3d 28 50 52 4f 54 4f 43 4f 4c 3d 54 43 50 53 29 28 48 4f 53 54 3d 61 64 62 2e 75 73 2d 73 61 6e 6a 6f 73 65 2d 31 2e 6f 72 61 63 6c 65 63 6c 6f 75 64 2e 63 6f 6d 29 28 50 4f 52 54 3d 31 35 32 32 29 29 28 43 4f 4e 4e 45 43 54 5f 44 41 54 41 3d 28 43 49 44 3d 28 50 52 4f 47 52 41 4d 3d 44 42 65 61 76 65 72 20 32 35 3f 33 3f 34 20 3f 20 4d 61 69 6e 29 28 48 4f 53 54 3d 5f 5f 6a 64 62 63 5f 5f 29 28 55 53 45 52 3d 41 44 4d 49 4e 29 29 28 53 45 52 56 49 43 45 5f 4e 41 4d 45 3d 67 30 37 63 63 33 38 31 61 32 33 36 62 65 36 5f 73 70 72 64 63 69 68 6e 6c 34 67 75 39 76 38 37 5f 68 69 67 68 2e 61 64 62 2e 6f 72 61 63 6c 65 63 6c 6f 75 64 2e 63 6f 6d 29 28 43 4f 4e 4e 45 43 54 49 4f 4e 5f 49 44 3d 73 56 4e 79 58 79 71 36 51 46 4f 4a 6d 54 79 6c 31";

        let mut packet = Vec::new();
        for byte_str in header_hex.split_whitespace() {
            packet.push(u8::from_str_radix(byte_str, 16).unwrap());
        }
        for byte_str in cd_hex.split_whitespace() {
            packet.push(u8::from_str_radix(byte_str, 16).unwrap());
        }
        assert_eq!(packet.len(), 354, "Input packet should be 354 bytes");

        let result = strip_connect_packet_cid_and_connid(&packet);
        assert!(
            result.is_some(),
            "strip should return Some for packet with CID and CONNECTION_ID"
        );

        let stripped = result.unwrap();
        // Verify size reduction
        assert!(
            stripped.len() < 304,
            "Stripped packet should be under 304 bytes, got {}",
            stripped.len()
        );

        // Verify TNS header length matches
        let tns_len = u16::from_be_bytes([stripped[0], stripped[1]]) as usize;
        assert_eq!(
            tns_len,
            stripped.len(),
            "TNS header length should match packet length"
        );

        // Verify connect data doesn't contain CID or CONNECTION_ID
        let cd_off = u16::from_be_bytes([stripped[26], stripped[27]]) as usize;
        let cd_len = u16::from_be_bytes([stripped[24], stripped[25]]) as usize;
        let cd = &stripped[cd_off..cd_off + cd_len];
        let cd_text = String::from_utf8_lossy(cd);
        assert!(!cd_text.contains("CID="), "CID should be removed");
        assert!(
            !cd_text.contains("CONNECTION_ID="),
            "CONNECTION_ID should be removed"
        );
        assert!(
            cd_text.contains("SERVICE_NAME="),
            "SERVICE_NAME should be preserved"
        );

        // Verify parentheses are balanced (count in ASCII portion only)
        let mut depth: i32 = 0;
        for &b in cd {
            if b == b'(' {
                depth += 1;
            }
            if b == b')' {
                depth -= 1;
            }
        }
        assert_eq!(
            depth, 0,
            "Parentheses should be balanced, got depth={}",
            depth
        );

        // Verify ends with proper closing
        assert!(
            cd_text.ends_with("))"),
            "Connect data should end with )) for CONNECT_DATA and DESCRIPTION"
        );
    }
}
