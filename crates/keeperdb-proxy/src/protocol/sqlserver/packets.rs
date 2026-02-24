//! TDS packet structures
//!
//! This module defines the wire protocol structures for TDS communication.
//! Reference: MS-TDS specification (Microsoft Open Specifications)

use super::constants::{EncryptionMode, TDS_HEADER_SIZE};

/// TDS packet header (8 bytes)
///
/// All TDS packets begin with this fixed-size header.
/// Note: Length field is big-endian, unlike most TDS data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TdsHeader {
    /// Packet type (PRELOGIN=0x12, LOGIN7=0x10, SQL_BATCH=0x01, etc.)
    pub packet_type: u8,
    /// Status flags (0x01 = EOM - End of Message)
    pub status: u8,
    /// Total packet length including header (big-endian)
    pub length: u16,
    /// Server Process ID (0 from client, assigned by server)
    pub spid: u16,
    /// Packet ID (incrementing counter, wraps at 255)
    pub packet_id: u8,
    /// Window (always 0, reserved)
    pub window: u8,
}

impl TdsHeader {
    /// Create a new TDS header
    pub fn new(packet_type: u8, status: u8, length: u16) -> Self {
        Self {
            packet_type,
            status,
            length,
            spid: 0,
            packet_id: 1,
            window: 0,
        }
    }

    /// Parse header from bytes
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < TDS_HEADER_SIZE {
            return None;
        }

        Some(Self {
            packet_type: data[0],
            status: data[1],
            length: u16::from_be_bytes([data[2], data[3]]),
            spid: u16::from_be_bytes([data[4], data[5]]),
            packet_id: data[6],
            window: data[7],
        })
    }

    /// Serialize header to bytes
    pub fn serialize(&self) -> [u8; TDS_HEADER_SIZE] {
        let length_bytes = self.length.to_be_bytes();
        let spid_bytes = self.spid.to_be_bytes();

        [
            self.packet_type,
            self.status,
            length_bytes[0],
            length_bytes[1],
            spid_bytes[0],
            spid_bytes[1],
            self.packet_id,
            self.window,
        ]
    }

    /// Check if this is the last packet in the message
    pub fn is_end_of_message(&self) -> bool {
        (self.status & super::constants::status::EOM) != 0
    }

    /// Get the payload length (total length minus header)
    pub fn payload_length(&self) -> usize {
        self.length.saturating_sub(TDS_HEADER_SIZE as u16) as usize
    }
}

impl Default for TdsHeader {
    fn default() -> Self {
        Self {
            packet_type: 0,
            status: super::constants::status::EOM,
            length: TDS_HEADER_SIZE as u16,
            spid: 0,
            packet_id: 1,
            window: 0,
        }
    }
}

/// PRELOGIN packet data
///
/// Used during initial connection negotiation before LOGIN7.
/// Contains option tokens for version, encryption, etc.
#[derive(Debug, Clone, Default)]
pub struct Prelogin {
    /// TDS version (major, minor, build)
    pub version: PreloginVersion,
    /// Encryption mode
    pub encryption: EncryptionMode,
    /// Instance name (optional)
    pub instance: Option<String>,
    /// Thread ID (optional)
    pub thread_id: Option<u32>,
    /// MARS enabled (optional)
    pub mars: Option<bool>,
    /// Trace ID (optional, 36 bytes)
    pub trace_id: Option<Vec<u8>>,
}

/// PRELOGIN version information
#[derive(Debug, Clone, Copy, Default)]
pub struct PreloginVersion {
    /// Major version
    pub major: u8,
    /// Minor version
    pub minor: u8,
    /// Build number (2 bytes)
    pub build: u16,
    /// Sub-build number (2 bytes)
    pub sub_build: u16,
}

impl PreloginVersion {
    /// Create version for SQL Server 2019 (TDS 7.4)
    pub fn sql_server_2019() -> Self {
        Self {
            major: 15,
            minor: 0,
            build: 2000,
            sub_build: 0,
        }
    }

    /// Create version for proxy identification
    pub fn proxy_version() -> Self {
        Self {
            major: 0,
            minor: 1,
            build: 0,
            sub_build: 0,
        }
    }
}

/// LOGIN7 packet structure
///
/// Contains authentication information sent after PRELOGIN.
/// All strings are UTF-16LE encoded in the wire format.
#[derive(Debug, Clone)]
pub struct Login7 {
    /// Total length of LOGIN7 packet (not including TDS header)
    pub length: u32,
    /// TDS version (0x74000004 for TDS 7.4)
    pub tds_version: u32,
    /// Requested packet size
    pub packet_size: u32,
    /// Client program version
    pub client_prog_ver: u32,
    /// Client process ID
    pub client_pid: u32,
    /// Connection ID (0 for new connection)
    pub connection_id: u32,
    /// Option flags 1
    pub option_flags1: u8,
    /// Option flags 2
    pub option_flags2: u8,
    /// Type flags
    pub type_flags: u8,
    /// Option flags 3
    pub option_flags3: u8,
    /// Client timezone (minutes from UTC)
    pub client_timezone: i32,
    /// Client LCID (locale ID)
    pub client_lcid: u32,

    // Variable-length fields
    /// Client hostname
    pub hostname: String,
    /// Username (for SQL authentication)
    pub username: String,
    /// Password (encoded per TDS spec) - will be zeroized
    pub password: Vec<u8>,
    /// Application name
    pub app_name: String,
    /// Server name
    pub server_name: String,
    /// Unused extension field
    pub extension: Vec<u8>,
    /// Client interface library name
    pub library_name: String,
    /// Language
    pub language: String,
    /// Initial database
    pub database: String,
    /// Client ID (MAC address, 6 bytes)
    pub client_id: [u8; 6],
    /// SSPI data (for Windows auth, empty for SQL auth)
    pub sspi: Vec<u8>,
    /// Attach database file
    pub attach_db_file: String,
    /// New password (for password change)
    pub new_password: Vec<u8>,
}

impl Default for Login7 {
    fn default() -> Self {
        Self {
            length: 0,
            tds_version: super::constants::version::TDS_7_4,
            packet_size: super::constants::DEFAULT_PACKET_SIZE,
            client_prog_ver: 0x0700_0000, // Version 7.0.0.0
            client_pid: 0,
            connection_id: 0,
            option_flags1: 0x00,
            option_flags2: super::constants::option_flags2::ODBC_ON,
            type_flags: super::constants::type_flags::SQL_TSQL,
            option_flags3: 0x00,
            client_timezone: 0,
            client_lcid: 0x0409, // en-US
            hostname: String::new(),
            username: String::new(),
            password: Vec::new(),
            app_name: String::from("keeperdb-proxy"),
            server_name: String::new(),
            extension: Vec::new(),
            library_name: String::from("Rust TDS"),
            language: String::new(),
            database: String::new(),
            client_id: [0u8; 6],
            sspi: Vec::new(),
            attach_db_file: String::new(),
            new_password: Vec::new(),
        }
    }
}

impl Login7 {
    /// Create a new LOGIN7 with default settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Set SQL authentication credentials
    ///
    /// Note: Password will be encoded during serialization.
    /// The raw password should be passed here.
    pub fn with_credentials(mut self, username: &str, password: &str) -> Self {
        self.username = username.to_string();
        // Password is encoded during serialization, store raw for now
        // This will be encoded in the parser module
        self.password = password.as_bytes().to_vec();
        self
    }

    /// Set the target database
    pub fn with_database(mut self, database: &str) -> Self {
        self.database = database.to_string();
        self
    }

    /// Set the application name
    pub fn with_app_name(mut self, app_name: &str) -> Self {
        self.app_name = app_name.to_string();
        self
    }

    /// Set the hostname
    pub fn with_hostname(mut self, hostname: &str) -> Self {
        self.hostname = hostname.to_string();
        self
    }
}

/// LOGINACK token structure
///
/// Returned by server after successful authentication.
#[derive(Debug, Clone)]
pub struct LoginAck {
    /// Interface type (0 = SQL_DFLT, 1 = SQL_TSQL)
    pub interface: u8,
    /// TDS version from server
    pub tds_version: u32,
    /// Server program name
    pub prog_name: String,
    /// Server version (major, minor, build_hi, build_lo)
    pub prog_version: [u8; 4],
}

impl Default for LoginAck {
    fn default() -> Self {
        Self {
            interface: 1, // SQL_TSQL
            tds_version: super::constants::version::TDS_7_4,
            prog_name: String::new(),
            prog_version: [0; 4],
        }
    }
}

/// Environment change token
#[derive(Debug, Clone)]
pub struct EnvChange {
    /// Type of environment change
    pub env_type: u8,
    /// New value
    pub new_value: String,
    /// Old value
    pub old_value: String,
}

/// Error/Info message token
#[derive(Debug, Clone)]
pub struct Message {
    /// Error/info number
    pub number: i32,
    /// State
    pub state: u8,
    /// Severity (class)
    pub class: u8,
    /// Message text
    pub message: String,
    /// Server name
    pub server_name: String,
    /// Procedure name (if applicable)
    pub proc_name: String,
    /// Line number
    pub line_number: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tds_header_parse_serialize() {
        let header = TdsHeader {
            packet_type: 0x12, // PRELOGIN
            status: 0x01,      // EOM
            length: 256,
            spid: 0,
            packet_id: 1,
            window: 0,
        };

        let bytes = header.serialize();
        let parsed = TdsHeader::parse(&bytes).unwrap();

        assert_eq!(parsed.packet_type, header.packet_type);
        assert_eq!(parsed.status, header.status);
        assert_eq!(parsed.length, header.length);
        assert_eq!(parsed.spid, header.spid);
        assert_eq!(parsed.packet_id, header.packet_id);
        assert_eq!(parsed.window, header.window);
    }

    #[test]
    fn test_tds_header_big_endian_length() {
        // Length 0x0100 (256) should be serialized as [0x01, 0x00] (big-endian)
        let header = TdsHeader::new(0x12, 0x01, 256);
        let bytes = header.serialize();

        assert_eq!(bytes[2], 0x01); // High byte
        assert_eq!(bytes[3], 0x00); // Low byte
    }

    #[test]
    fn test_tds_header_is_eom() {
        let header_eom = TdsHeader::new(0x01, 0x01, 100);
        let header_not_eom = TdsHeader::new(0x01, 0x00, 100);

        assert!(header_eom.is_end_of_message());
        assert!(!header_not_eom.is_end_of_message());
    }

    #[test]
    fn test_tds_header_payload_length() {
        let header = TdsHeader::new(0x01, 0x01, 100);
        assert_eq!(header.payload_length(), 92); // 100 - 8 (header size)
    }

    #[test]
    fn test_login7_builder() {
        let login = Login7::new()
            .with_credentials("testuser", "testpass")
            .with_database("testdb")
            .with_app_name("TestApp")
            .with_hostname("testhost");

        assert_eq!(login.username, "testuser");
        assert_eq!(login.database, "testdb");
        assert_eq!(login.app_name, "TestApp");
        assert_eq!(login.hostname, "testhost");
    }

    #[test]
    fn test_prelogin_version() {
        let version = PreloginVersion::sql_server_2019();
        assert_eq!(version.major, 15);
        assert_eq!(version.minor, 0);
    }

    #[test]
    fn test_tds_header_parse_short_buffer() {
        // Buffer shorter than 8 bytes should return None
        let short = [0u8; 4];
        assert!(TdsHeader::parse(&short).is_none());
    }

    #[test]
    fn test_tds_header_default() {
        let header = TdsHeader::default();
        assert_eq!(header.packet_type, 0);
        assert_eq!(header.status, super::super::constants::status::EOM);
        assert_eq!(header.length, TDS_HEADER_SIZE as u16);
        assert_eq!(header.spid, 0);
        assert_eq!(header.packet_id, 1);
        assert_eq!(header.window, 0);
    }

    #[test]
    fn test_tds_header_payload_length_underflow() {
        // If length is less than header size, should return 0 (saturating_sub)
        let header = TdsHeader {
            length: 4, // Less than TDS_HEADER_SIZE (8)
            ..Default::default()
        };
        assert_eq!(header.payload_length(), 0);
    }

    #[test]
    fn test_login7_default_values() {
        let login = Login7::default();
        assert_eq!(login.tds_version, super::super::constants::version::TDS_7_4);
        assert_eq!(
            login.packet_size,
            super::super::constants::DEFAULT_PACKET_SIZE
        );
        assert_eq!(login.app_name, "keeperdb-proxy");
        assert_eq!(login.library_name, "Rust TDS");
    }

    #[test]
    fn test_loginack_default() {
        let ack = LoginAck::default();
        assert_eq!(ack.interface, 1); // SQL_TSQL
        assert_eq!(ack.tds_version, super::super::constants::version::TDS_7_4);
    }

    #[test]
    fn test_prelogin_default() {
        let prelogin = Prelogin::default();
        assert_eq!(prelogin.encryption, EncryptionMode::Off);
        assert!(prelogin.instance.is_none());
        assert!(prelogin.thread_id.is_none());
        assert!(prelogin.mars.is_none());
        assert!(prelogin.trace_id.is_none());
    }

    #[test]
    fn test_login7_builder_chaining() {
        let login = Login7::new()
            .with_credentials("user", "pass")
            .with_database("db")
            .with_app_name("app")
            .with_hostname("host");

        assert_eq!(login.username, "user");
        assert_eq!(login.database, "db");
        assert_eq!(login.app_name, "app");
        assert_eq!(login.hostname, "host");
    }

    #[test]
    fn test_tds_header_serialize_parse_identity() {
        // Test that serialize followed by parse gives the same values
        for packet_type in [0x01, 0x10, 0x12] {
            for status in [0x00, 0x01] {
                for length in [8u16, 100, 1000, 32767] {
                    let original = TdsHeader::new(packet_type, status, length);
                    let bytes = original.serialize();
                    let parsed = TdsHeader::parse(&bytes).unwrap();

                    assert_eq!(parsed.packet_type, original.packet_type);
                    assert_eq!(parsed.status, original.status);
                    assert_eq!(parsed.length, original.length);
                }
            }
        }
    }

    #[test]
    fn test_prelogin_version_proxy() {
        let version = PreloginVersion::proxy_version();
        assert_eq!(version.major, 0);
        assert_eq!(version.minor, 1);
        assert_eq!(version.build, 0);
    }
}
