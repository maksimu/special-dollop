//! TNS packet structures
//!
//! This module defines the packet structures for the TNS protocol.
//! All multi-byte fields in TNS are big-endian.

use super::constants::*;

/// TNS packet header (8 bytes)
///
/// All TNS packets start with this header structure:
/// ```text
/// ┌──────────────┬──────────────┬──────────┬──────────┬──────────────────┐
/// │ Packet Length│ Packet Cksum │   Type   │ Reserved │  Header Cksum    │
/// │   (2 bytes)  │  (2 bytes)   │ (1 byte) │ (1 byte) │    (2 bytes)     │
/// │  Big-endian  │ Usually 0x00 │          │  Flags   │   Usually 0x00   │
/// └──────────────┴──────────────┴──────────┴──────────┴──────────────────┘
///       [0:2]         [2:4]        [4]        [5]          [6:8]
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TnsHeader {
    /// Total packet length including header (big-endian)
    pub length: u16,
    /// Packet checksum (usually 0)
    pub packet_checksum: u16,
    /// Packet type (see packet_type constants)
    pub packet_type: u8,
    /// Reserved/flags byte
    pub flags: u8,
    /// Header checksum (usually 0)
    pub header_checksum: u16,
}

impl TnsHeader {
    /// Create a new TNS header with the given type and payload length
    pub fn new(packet_type: u8, payload_length: usize) -> Self {
        Self {
            length: (TNS_HEADER_SIZE + payload_length) as u16,
            packet_checksum: 0,
            packet_type,
            flags: 0,
            header_checksum: 0,
        }
    }

    /// Get the payload length (total length minus header)
    pub fn payload_length(&self) -> usize {
        (self.length as usize).saturating_sub(TNS_HEADER_SIZE)
    }

    /// Check if this is a CONNECT packet
    pub fn is_connect(&self) -> bool {
        self.packet_type == packet_type::CONNECT
    }

    /// Check if this is an ACCEPT packet
    pub fn is_accept(&self) -> bool {
        self.packet_type == packet_type::ACCEPT
    }

    /// Check if this is a REFUSE packet
    pub fn is_refuse(&self) -> bool {
        self.packet_type == packet_type::REFUSE
    }

    /// Check if this is a DATA packet
    pub fn is_data(&self) -> bool {
        self.packet_type == packet_type::DATA
    }

    /// Check if this is a REDIRECT packet
    pub fn is_redirect(&self) -> bool {
        self.packet_type == packet_type::REDIRECT
    }

    /// Get packet type name for logging
    pub fn type_name(&self) -> &'static str {
        match self.packet_type {
            packet_type::CONNECT => "CONNECT",
            packet_type::ACCEPT => "ACCEPT",
            packet_type::ACK => "ACK",
            packet_type::REFUSE => "REFUSE",
            packet_type::REDIRECT => "REDIRECT",
            packet_type::DATA => "DATA",
            packet_type::NULL => "NULL",
            packet_type::ABORT => "ABORT",
            packet_type::RESEND => "RESEND",
            packet_type::MARKER => "MARKER",
            packet_type::ATTENTION => "ATTENTION",
            packet_type::CONTROL => "CONTROL",
            _ => "UNKNOWN",
        }
    }
}

impl Default for TnsHeader {
    fn default() -> Self {
        Self {
            length: TNS_HEADER_SIZE as u16,
            packet_checksum: 0,
            packet_type: packet_type::DATA,
            flags: 0,
            header_checksum: 0,
        }
    }
}

impl std::fmt::Display for TnsHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TNS {} (len={}, flags=0x{:02x})",
            self.type_name(),
            self.length,
            self.flags
        )
    }
}

/// Size of CONNECT packet fixed fields (after 8-byte TNS header)
pub const CONNECT_PACKET_FIXED_SIZE: usize = 26;

/// Minimum CONNECT packet size (header + fixed fields)
pub const CONNECT_PACKET_MIN_SIZE: usize = TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE;

/// Size of ACCEPT packet fixed fields (after 8-byte TNS header)
pub const ACCEPT_PACKET_FIXED_SIZE: usize = 16;

/// Size of REFUSE packet fixed fields (after 8-byte TNS header)
pub const REFUSE_PACKET_FIXED_SIZE: usize = 4;

/// TNS CONNECT packet
///
/// Sent by client to initiate a connection. Contains version negotiation
/// parameters and the connect string (DESCRIPTION=...).
///
/// ```text
/// ┌───────────────┬─────────────────────┬───────────────────┬─────────────────┐
/// │  TNS Header   │  Connection Params  │   Connect Flags   │  Connect Data   │
/// │   (8 bytes)   │     (20 bytes)      │    (6 bytes)      │   (variable)    │
/// └───────────────┴─────────────────────┴───────────────────┴─────────────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectPacket {
    /// TNS header
    pub header: TnsHeader,
    /// TNS protocol version
    pub version: u16,
    /// Minimum compatible version
    pub version_compatible: u16,
    /// Service options flags
    pub service_options: u16,
    /// Session Data Unit size (typically 8192)
    pub session_data_unit_size: u16,
    /// Maximum Transmission Data Unit size (typically 65535)
    pub maximum_transmission_data_unit_size: u16,
    /// NT protocol characteristics
    pub nt_protocol_characteristics: u16,
    /// Line turnaround value
    pub line_turnaround_value: u16,
    /// Value of one in hardware (byte order indicator)
    pub value_of_one_in_hardware: u16,
    /// Length of connect data
    pub connect_data_length: u16,
    /// Offset to connect data from packet start
    pub connect_data_offset: u16,
    /// Maximum connect data receive size
    pub connect_data_max_recv: u32,
    /// Connect flags byte 0
    pub connect_flags_0: u8,
    /// Connect flags byte 1
    pub connect_flags_1: u8,
    /// The connect string (DESCRIPTION=...)
    pub connect_data: String,
}

impl ConnectPacket {
    /// Create a new CONNECT packet with default parameters
    pub fn new(connect_data: String) -> Self {
        let connect_data_length = connect_data.len() as u16;
        let connect_data_offset = (TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE) as u16;
        let total_length = connect_data_offset as usize + connect_data.len();

        Self {
            header: TnsHeader::new(packet_type::CONNECT, total_length - TNS_HEADER_SIZE),
            version: version::DEFAULT,
            version_compatible: version::MIN_SUPPORTED,
            service_options: 0,
            session_data_unit_size: DEFAULT_SDU_SIZE,
            maximum_transmission_data_unit_size: DEFAULT_TDU_SIZE,
            nt_protocol_characteristics: 0,
            line_turnaround_value: 0,
            value_of_one_in_hardware: 0x0100, // Big-endian indicator
            connect_data_length,
            connect_data_offset,
            connect_data_max_recv: 512,
            connect_flags_0: 0,
            connect_flags_1: 0,
            connect_data,
        }
    }

    /// Get the connect data (the connect string)
    pub fn connect_string(&self) -> &str {
        &self.connect_data
    }
}

impl std::fmt::Display for ConnectPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CONNECT v{} (SDU={}, connect_data_len={})",
            self.version, self.session_data_unit_size, self.connect_data_length
        )
    }
}

/// TNS ACCEPT packet
///
/// Sent by server to accept a connection. Contains negotiated parameters.
///
/// ```text
/// ┌───────────────┬─────────────────────┬───────────────────┬─────────────────┐
/// │  TNS Header   │   Accept Params     │   Accept Flags    │   Accept Data   │
/// │   (8 bytes)   │     (12 bytes)      │    (4 bytes)      │   (variable)    │
/// └───────────────┴─────────────────────┴───────────────────┴─────────────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptPacket {
    /// TNS header
    pub header: TnsHeader,
    /// Accepted TNS version
    pub version: u16,
    /// Service options flags
    pub service_options: u16,
    /// Negotiated Session Data Unit size
    pub session_data_unit_size: u16,
    /// Negotiated Maximum Transmission Data Unit size
    pub maximum_transmission_data_unit_size: u16,
    /// Value of one in hardware
    pub value_of_one_in_hardware: u16,
    /// Length of accept data
    pub accept_data_length: u16,
    /// Offset to accept data from packet start
    pub accept_data_offset: u16,
    /// Connect flags byte 0
    pub connect_flags_0: u8,
    /// Connect flags byte 1
    pub connect_flags_1: u8,
    /// Accept data (optional, usually empty)
    pub accept_data: Vec<u8>,
}

impl AcceptPacket {
    /// Create a new ACCEPT packet with default parameters
    pub fn new(version: u16, sdu_size: u16, tdu_size: u16) -> Self {
        Self {
            header: TnsHeader::new(packet_type::ACCEPT, ACCEPT_PACKET_FIXED_SIZE),
            version,
            service_options: 0,
            session_data_unit_size: sdu_size,
            maximum_transmission_data_unit_size: tdu_size,
            value_of_one_in_hardware: 0x0100,
            accept_data_length: 0,
            accept_data_offset: (TNS_HEADER_SIZE + ACCEPT_PACKET_FIXED_SIZE) as u16,
            connect_flags_0: 0,
            connect_flags_1: 0,
            accept_data: Vec::new(),
        }
    }
}

impl std::fmt::Display for AcceptPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ACCEPT v{} (SDU={}, TDU={})",
            self.version, self.session_data_unit_size, self.maximum_transmission_data_unit_size
        )
    }
}

/// TNS REFUSE packet
///
/// Sent by server to refuse a connection with a reason.
///
/// ```text
/// ┌───────────────┬──────────────┬──────────────┬───────────────────────────┐
/// │  TNS Header   │  User Reason │ System Reason│      Refuse Data          │
/// │   (8 bytes)   │   (1 byte)   │   (1 byte)   │       (variable)          │
/// └───────────────┴──────────────┴──────────────┴───────────────────────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusePacket {
    /// TNS header
    pub header: TnsHeader,
    /// User-level reason code
    pub user_reason: u8,
    /// System-level reason code
    pub system_reason: u8,
    /// Length of refuse data
    pub refuse_data_length: u16,
    /// Refuse message/data
    pub refuse_data: String,
}

impl RefusePacket {
    /// Create a new REFUSE packet with the given reason and message
    pub fn new(user_reason: u8, system_reason: u8, message: &str) -> Self {
        let refuse_data_length = message.len() as u16;
        let payload_size = REFUSE_PACKET_FIXED_SIZE + message.len();

        Self {
            header: TnsHeader::new(packet_type::REFUSE, payload_size),
            user_reason,
            system_reason,
            refuse_data_length,
            refuse_data: message.to_string(),
        }
    }

    /// Create a REFUSE packet for invalid credentials
    pub fn invalid_credentials(message: &str) -> Self {
        Self::new(refuse_reason::INVALID_CREDENTIALS, 0, message)
    }

    /// Create a REFUSE packet for service unavailable
    pub fn service_unavailable(message: &str) -> Self {
        Self::new(refuse_reason::SERVICE_UNAVAILABLE, 0, message)
    }
}

impl std::fmt::Display for RefusePacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "REFUSE (user_reason={}, system_reason={}, msg={})",
            self.user_reason, self.system_reason, &self.refuse_data
        )
    }
}

/// TNS DATA packet
///
/// Used for all data transfer after connection is established.
/// This includes authentication messages and query/result data.
///
/// ```text
/// ┌───────────────┬──────────────┬────────────────────────────────────────────┐
/// │  TNS Header   │  Data Flags  │               Payload                      │
/// │   (8 bytes)   │  (2 bytes)   │              (variable)                    │
/// └───────────────┴──────────────┴────────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPacket {
    /// TNS header
    pub header: TnsHeader,
    /// Data flags (see data_flags module)
    pub data_flags: u16,
    /// Packet payload
    pub payload: Vec<u8>,
}

impl DataPacket {
    /// Create a new DATA packet with the given flags and payload
    pub fn new(data_flags: u16, payload: Vec<u8>) -> Self {
        let payload_len = 2 + payload.len(); // 2 bytes for data_flags + payload

        Self {
            header: TnsHeader::new(packet_type::DATA, payload_len),
            data_flags,
            payload,
        }
    }

    /// Create a simple DATA packet with no flags
    pub fn simple(payload: Vec<u8>) -> Self {
        Self::new(0, payload)
    }

    /// Check if this packet has the EOF flag set
    pub fn is_eof(&self) -> bool {
        self.data_flags & data_flags::EOF != 0
    }

    /// Check if more data is expected
    pub fn has_more_data(&self) -> bool {
        self.data_flags & data_flags::MORE_DATA != 0
    }
}

impl std::fmt::Display for DataPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DATA (flags=0x{:04x}, payload_len={})",
            self.data_flags,
            self.payload.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_new() {
        let header = TnsHeader::new(packet_type::CONNECT, 100);
        assert_eq!(header.length, 108); // 8 + 100
        assert_eq!(header.packet_type, packet_type::CONNECT);
        assert_eq!(header.packet_checksum, 0);
        assert_eq!(header.header_checksum, 0);
    }

    #[test]
    fn test_header_payload_length() {
        let header = TnsHeader::new(packet_type::DATA, 50);
        assert_eq!(header.payload_length(), 50);

        let empty = TnsHeader::new(packet_type::ACK, 0);
        assert_eq!(empty.payload_length(), 0);
    }

    #[test]
    fn test_header_type_checks() {
        let connect = TnsHeader::new(packet_type::CONNECT, 0);
        assert!(connect.is_connect());
        assert!(!connect.is_accept());

        let accept = TnsHeader::new(packet_type::ACCEPT, 0);
        assert!(accept.is_accept());
        assert!(!accept.is_connect());

        let data = TnsHeader::new(packet_type::DATA, 0);
        assert!(data.is_data());
    }

    #[test]
    fn test_header_type_name() {
        assert_eq!(
            TnsHeader::new(packet_type::CONNECT, 0).type_name(),
            "CONNECT"
        );
        assert_eq!(TnsHeader::new(packet_type::ACCEPT, 0).type_name(), "ACCEPT");
        assert_eq!(TnsHeader::new(packet_type::REFUSE, 0).type_name(), "REFUSE");
        assert_eq!(TnsHeader::new(packet_type::DATA, 0).type_name(), "DATA");
        assert_eq!(TnsHeader::new(0xFF, 0).type_name(), "UNKNOWN");
    }

    #[test]
    fn test_header_display() {
        let header = TnsHeader::new(packet_type::CONNECT, 100);
        let display = format!("{}", header);
        assert!(display.contains("CONNECT"));
        assert!(display.contains("108"));
    }

    #[test]
    fn test_header_default() {
        let header = TnsHeader::default();
        assert_eq!(header.length, TNS_HEADER_SIZE as u16);
        assert_eq!(header.packet_type, packet_type::DATA);
    }

    #[test]
    fn test_connect_packet_new() {
        let connect_string = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=localhost)(PORT=1521)))";
        let packet = ConnectPacket::new(connect_string.to_string());

        assert_eq!(packet.header.packet_type, packet_type::CONNECT);
        assert_eq!(packet.version, version::DEFAULT);
        assert_eq!(packet.version_compatible, version::MIN_SUPPORTED);
        assert_eq!(packet.session_data_unit_size, DEFAULT_SDU_SIZE);
        assert_eq!(packet.maximum_transmission_data_unit_size, DEFAULT_TDU_SIZE);
        assert_eq!(packet.connect_data_length, connect_string.len() as u16);
        assert_eq!(
            packet.connect_data_offset,
            (TNS_HEADER_SIZE + CONNECT_PACKET_FIXED_SIZE) as u16
        );
        assert_eq!(packet.connect_string(), connect_string);
    }

    #[test]
    fn test_connect_packet_display() {
        let packet = ConnectPacket::new("(DESCRIPTION=...)".to_string());
        let display = format!("{}", packet);
        assert!(display.contains("CONNECT"));
        assert!(display.contains(&packet.version.to_string()));
    }

    #[test]
    fn test_accept_packet_new() {
        let packet = AcceptPacket::new(318, 8192, 65535);

        assert_eq!(packet.header.packet_type, packet_type::ACCEPT);
        assert_eq!(packet.version, 318);
        assert_eq!(packet.session_data_unit_size, 8192);
        assert_eq!(packet.maximum_transmission_data_unit_size, 65535);
        assert!(packet.accept_data.is_empty());
    }

    #[test]
    fn test_accept_packet_display() {
        let packet = AcceptPacket::new(318, 8192, 65535);
        let display = format!("{}", packet);
        assert!(display.contains("ACCEPT"));
        assert!(display.contains("318"));
        assert!(display.contains("8192"));
    }

    #[test]
    fn test_refuse_packet_new() {
        let packet = RefusePacket::new(1, 2, "Connection refused");

        assert_eq!(packet.header.packet_type, packet_type::REFUSE);
        assert_eq!(packet.user_reason, 1);
        assert_eq!(packet.system_reason, 2);
        assert_eq!(packet.refuse_data, "Connection refused");
        assert_eq!(packet.refuse_data_length, 18);
    }

    #[test]
    fn test_refuse_packet_helpers() {
        let invalid = RefusePacket::invalid_credentials("Bad password");
        assert_eq!(invalid.user_reason, refuse_reason::INVALID_CREDENTIALS);

        let unavail = RefusePacket::service_unavailable("Service down");
        assert_eq!(unavail.user_reason, refuse_reason::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_refuse_packet_display() {
        let packet = RefusePacket::new(1, 0, "Test error");
        let display = format!("{}", packet);
        assert!(display.contains("REFUSE"));
        assert!(display.contains("Test error"));
    }

    #[test]
    fn test_data_packet_new() {
        let payload = vec![0x01, 0x02, 0x03, 0x04];
        let packet = DataPacket::new(data_flags::EOF, payload.clone());

        assert_eq!(packet.header.packet_type, packet_type::DATA);
        assert_eq!(packet.data_flags, data_flags::EOF);
        assert_eq!(packet.payload, payload);
        assert!(packet.is_eof());
        assert!(!packet.has_more_data());
    }

    #[test]
    fn test_data_packet_simple() {
        let payload = vec![0xAA, 0xBB];
        let packet = DataPacket::simple(payload.clone());

        assert_eq!(packet.data_flags, 0);
        assert_eq!(packet.payload, payload);
        assert!(!packet.is_eof());
    }

    #[test]
    fn test_data_packet_flags() {
        let packet = DataPacket::new(data_flags::MORE_DATA | data_flags::EOF, vec![]);
        assert!(packet.is_eof());
        assert!(packet.has_more_data());
    }

    #[test]
    fn test_data_packet_display() {
        let packet = DataPacket::new(0x0040, vec![1, 2, 3]);
        let display = format!("{}", packet);
        assert!(display.contains("DATA"));
        assert!(display.contains("0040"));
        assert!(display.contains("3"));
    }
}
