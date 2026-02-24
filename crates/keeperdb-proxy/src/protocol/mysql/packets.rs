//! MySQL packet structures
//!
//! This module defines the wire protocol structures for MySQL communication.
//! Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_basic_packets.html>

/// MySQL packet header (4 bytes)
#[derive(Debug, Clone)]
pub struct PacketHeader {
    /// Payload length (3 bytes, max 16MB - 1)
    pub payload_length: u32,
    /// Sequence ID (1 byte)
    pub sequence_id: u8,
}

impl PacketHeader {
    /// Maximum payload size (2^24 - 1)
    pub const MAX_PAYLOAD_LENGTH: u32 = 0xFF_FF_FF;

    /// Create a new packet header
    pub fn new(payload_length: u32, sequence_id: u8) -> Self {
        Self {
            payload_length,
            sequence_id,
        }
    }
}

/// MySQL Handshake V10 packet (server -> client)
/// Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_connection_phase_packets_protocol_handshake_v10.html>
#[derive(Debug, Clone)]
pub struct HandshakeV10 {
    /// Protocol version (always 10)
    pub protocol_version: u8,
    /// Server version string (null-terminated)
    pub server_version: String,
    /// Connection ID
    pub connection_id: u32,
    /// First 8 bytes of auth plugin data (scramble)
    pub auth_plugin_data_part_1: [u8; 8],
    /// Capability flags (lower 2 bytes)
    pub capability_flags_lower: u16,
    /// Character set
    pub character_set: u8,
    /// Status flags
    pub status_flags: u16,
    /// Capability flags (upper 2 bytes)
    pub capability_flags_upper: u16,
    /// Length of auth plugin data (if CLIENT_PLUGIN_AUTH)
    pub auth_plugin_data_length: u8,
    /// Reserved (10 bytes of zeros)
    pub reserved: [u8; 10],
    /// Rest of auth plugin data (if CLIENT_SECURE_CONNECTION)
    pub auth_plugin_data_part_2: Vec<u8>,
    /// Auth plugin name (if CLIENT_PLUGIN_AUTH)
    pub auth_plugin_name: String,
}

impl Default for HandshakeV10 {
    fn default() -> Self {
        Self {
            protocol_version: 10,
            server_version: "5.7.0-keeperdb-proxy".to_string(),
            connection_id: 1,
            auth_plugin_data_part_1: [0u8; 8],
            capability_flags_lower: 0,
            character_set: 0x21,  // utf8_general_ci
            status_flags: 0x0002, // SERVER_STATUS_AUTOCOMMIT
            capability_flags_upper: 0,
            auth_plugin_data_length: 21,
            reserved: [0u8; 10],
            auth_plugin_data_part_2: vec![0u8; 12],
            auth_plugin_name: "mysql_native_password".to_string(),
        }
    }
}

impl HandshakeV10 {
    /// Get the full 20-byte scramble (auth_plugin_data_part_1 + auth_plugin_data_part_2)
    pub fn get_scramble(&self) -> Vec<u8> {
        let mut scramble = Vec::with_capacity(20);
        scramble.extend_from_slice(&self.auth_plugin_data_part_1);
        // Take first 12 bytes of part 2 to make 20 total
        let part2_len = std::cmp::min(12, self.auth_plugin_data_part_2.len());
        scramble.extend_from_slice(&self.auth_plugin_data_part_2[..part2_len]);
        scramble
    }

    /// Get combined capability flags (32-bit)
    pub fn capability_flags(&self) -> u32 {
        (self.capability_flags_upper as u32) << 16 | self.capability_flags_lower as u32
    }

    /// Set capability flags from 32-bit value
    pub fn set_capability_flags(&mut self, flags: u32) {
        self.capability_flags_lower = (flags & 0xFFFF) as u16;
        self.capability_flags_upper = ((flags >> 16) & 0xFFFF) as u16;
    }
}

/// MySQL Handshake Response 41 packet (client -> server)
/// Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_connection_phase_packets_protocol_handshake_response.html>
#[derive(Debug, Clone)]
pub struct HandshakeResponse41 {
    /// Capability flags (4 bytes)
    pub capability_flags: u32,
    /// Max packet size
    pub max_packet_size: u32,
    /// Character set
    pub character_set: u8,
    /// Reserved (23 bytes of zeros)
    pub reserved: [u8; 23],
    /// Username (null-terminated)
    pub username: String,
    /// Auth response (length-encoded)
    pub auth_response: Vec<u8>,
    /// Database name (if CLIENT_CONNECT_WITH_DB)
    pub database: Option<String>,
    /// Auth plugin name (if CLIENT_PLUGIN_AUTH)
    pub auth_plugin_name: Option<String>,
    /// Connection attributes (if CLIENT_CONNECT_ATTRS)
    pub connect_attrs: Option<Vec<(String, String)>>,
}

impl Default for HandshakeResponse41 {
    fn default() -> Self {
        Self {
            capability_flags: 0,
            max_packet_size: 0x00FF_FFFF,
            character_set: 0x21, // utf8_general_ci
            reserved: [0u8; 23],
            username: String::new(),
            auth_response: Vec::new(),
            database: None,
            auth_plugin_name: None,
            connect_attrs: None,
        }
    }
}

impl HandshakeResponse41 {
    /// Get the program/client name from connect attributes.
    ///
    /// Checks for `program_name` first (e.g., "mysql", "MySQLWorkbench"),
    /// then falls back to `_client_name` (e.g., "MySQL Connector/J" for JDBC clients).
    /// This is the MySQL equivalent of PostgreSQL's `application_name`.
    pub fn program_name(&self) -> Option<&str> {
        self.connect_attrs.as_ref().and_then(|attrs| {
            // First try program_name (set by native clients like mysql CLI, MySQL Workbench)
            attrs
                .iter()
                .find(|(key, _)| key == "program_name")
                .map(|(_, value)| value.as_str())
                // Fall back to _client_name (set by JDBC drivers like MySQL Connector/J)
                .or_else(|| {
                    attrs
                        .iter()
                        .find(|(key, _)| key == "_client_name")
                        .map(|(_, value)| value.as_str())
                })
        })
    }

    /// Generate a client fingerprint from all connect attributes.
    ///
    /// # Security Model
    ///
    /// MySQL fingerprinting is **robust** because MySQL clients send rich
    /// `connect_attrs` through the `CLIENT_CONNECT_ATTRS` capability flag.
    ///
    /// Creates a unique identifier by combining key attributes:
    /// - `program_name` or `_client_name`: Primary application identifier
    /// - `_client_version` / `_connector_version`: Version number
    /// - `_platform` / `_os`: Operating system
    /// - `_runtime_vendor`: Java vendor (distinguishes different JDBC apps)
    ///
    /// # Comparison with PostgreSQL
    ///
    /// Unlike PostgreSQL (which only has the optional `application_name` parameter),
    /// MySQL's `connect_attrs` provide multiple independent identifying attributes.
    /// This means:
    /// - Different tools almost always produce different fingerprints
    /// - Even different versions of the same tool can be distinguished
    /// - The fingerprint is harder (though not impossible) to spoof
    ///
    /// # Returns
    ///
    /// - `Some(fingerprint)` with colon-separated attributes (e.g., "DBeaver:v23.0:x86_64")
    /// - `None` if no identifying attributes are present (rare)
    pub fn client_fingerprint(&self) -> Option<String> {
        let attrs = self.connect_attrs.as_ref()?;

        let mut parts: Vec<String> = Vec::new();

        // Primary identifier: program_name or _client_name
        if let Some(name) = self.program_name() {
            parts.push(name.to_string());
        }

        // Add version info
        for key in &["_client_version", "_connector_version"] {
            if let Some((_, value)) = attrs.iter().find(|(k, _)| k == *key) {
                parts.push(format!("v{}", value));
                break; // Only use one version
            }
        }

        // Add platform/OS info
        for key in &["_platform", "_os"] {
            if let Some((_, value)) = attrs.iter().find(|(k, _)| k == *key) {
                parts.push(value.to_string());
                break; // Only use one
            }
        }

        // For JDBC clients, add runtime vendor (distinguishes different Java apps)
        if let Some((_, value)) = attrs.iter().find(|(k, _)| k == "_runtime_vendor") {
            parts.push(value.to_string());
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(":"))
        }
    }

    /// Get any connect attribute by key.
    pub fn connect_attr(&self, key: &str) -> Option<&str> {
        self.connect_attrs.as_ref().and_then(|attrs| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, value)| value.as_str())
        })
    }
}

/// MySQL OK Packet (server -> client)
/// Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_basic_ok_packet.html>
#[derive(Debug, Clone)]
pub struct OkPacket {
    /// Header (0x00 or 0xFE)
    pub header: u8,
    /// Affected rows (length-encoded int)
    pub affected_rows: u64,
    /// Last insert ID (length-encoded int)
    pub last_insert_id: u64,
    /// Status flags (if CLIENT_PROTOCOL_41)
    pub status_flags: u16,
    /// Warnings (if CLIENT_PROTOCOL_41)
    pub warnings: u16,
    /// Session state info (if CLIENT_SESSION_TRACK)
    pub info: String,
}

impl Default for OkPacket {
    fn default() -> Self {
        Self {
            header: 0x00,
            affected_rows: 0,
            last_insert_id: 0,
            status_flags: 0x0002, // SERVER_STATUS_AUTOCOMMIT
            warnings: 0,
            info: String::new(),
        }
    }
}

/// MySQL ERR Packet (server -> client)
/// Reference: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_basic_err_packet.html>
#[derive(Debug, Clone)]
pub struct ErrPacket {
    /// Header (0xFF)
    pub header: u8,
    /// Error code
    pub error_code: u16,
    /// SQL state marker (if CLIENT_PROTOCOL_41)
    pub sql_state_marker: char,
    /// SQL state (5 characters, if CLIENT_PROTOCOL_41)
    pub sql_state: [u8; 5],
    /// Error message
    pub error_message: String,
}

impl Default for ErrPacket {
    fn default() -> Self {
        Self {
            header: 0xFF,
            error_code: 0,
            sql_state_marker: '#',
            sql_state: *b"HY000",
            error_message: String::new(),
        }
    }
}

impl ErrPacket {
    /// Create a new error packet with the given code and message
    pub fn new(error_code: u16, error_message: impl Into<String>) -> Self {
        Self {
            header: 0xFF,
            error_code,
            sql_state_marker: '#',
            sql_state: *b"HY000",
            error_message: error_message.into(),
        }
    }

    /// Access denied error (1045)
    pub fn access_denied(user: &str, host: &str) -> Self {
        Self::new(
            1045,
            format!(
                "Access denied for user '{}'@'{}' (using password: YES)",
                user, host
            ),
        )
    }

    /// Connection refused error
    pub fn connection_refused(host: &str, port: u16) -> Self {
        Self::new(
            2003,
            format!("Can't connect to MySQL server on '{}:{}'", host, port),
        )
    }

    /// Unknown database error (1049)
    pub fn unknown_database(db: &str) -> Self {
        Self::new(1049, format!("Unknown database '{}'", db))
    }
}

// ============================================================================
// Capability Flags
// Reference: https://dev.mysql.com/doc/dev/mysql-server/latest/group__group__cs__capabilities__flags.html
// ============================================================================

/// Client can handle long passwords
pub const CLIENT_LONG_PASSWORD: u32 = 0x0000_0001;
/// Found instead of affected rows
pub const CLIENT_FOUND_ROWS: u32 = 0x0000_0002;
/// Get all column flags
pub const CLIENT_LONG_FLAG: u32 = 0x0000_0004;
/// Can specify db on connect
pub const CLIENT_CONNECT_WITH_DB: u32 = 0x0000_0008;
/// Don't allow database.table.column
pub const CLIENT_NO_SCHEMA: u32 = 0x0000_0010;
/// Can use compression protocol
pub const CLIENT_COMPRESS: u32 = 0x0000_0020;
/// ODBC client
pub const CLIENT_ODBC: u32 = 0x0000_0040;
/// Can use LOAD DATA LOCAL
pub const CLIENT_LOCAL_FILES: u32 = 0x0000_0080;
/// Ignore spaces before '('
pub const CLIENT_IGNORE_SPACE: u32 = 0x0000_0100;
/// New 4.1 protocol
pub const CLIENT_PROTOCOL_41: u32 = 0x0000_0200;
/// This is an interactive client
pub const CLIENT_INTERACTIVE: u32 = 0x0000_0400;
/// Switch to SSL after handshake
pub const CLIENT_SSL: u32 = 0x0000_0800;
/// Ignore sigpipes
pub const CLIENT_IGNORE_SIGPIPE: u32 = 0x0000_1000;
/// Client knows about transactions
pub const CLIENT_TRANSACTIONS: u32 = 0x0000_2000;
/// Old flag for 4.1 protocol (deprecated)
pub const CLIENT_RESERVED: u32 = 0x0000_4000;
/// New 4.1 authentication (deprecated, use CLIENT_PLUGIN_AUTH)
pub const CLIENT_RESERVED2: u32 = 0x0000_8000;
/// Enable/disable multi-stmt support
pub const CLIENT_MULTI_STATEMENTS: u32 = 0x0001_0000;
/// Enable/disable multi-results
pub const CLIENT_MULTI_RESULTS: u32 = 0x0002_0000;
/// Multi-results in PS-protocol
pub const CLIENT_PS_MULTI_RESULTS: u32 = 0x0004_0000;
/// Client supports plugin authentication
pub const CLIENT_PLUGIN_AUTH: u32 = 0x0008_0000;
/// Client supports connection attributes
pub const CLIENT_CONNECT_ATTRS: u32 = 0x0010_0000;
/// Length of auth response can be > 255
pub const CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 0x0020_0000;
/// Don't close connection for user account with expired password
pub const CLIENT_CAN_HANDLE_EXPIRED_PASSWORDS: u32 = 0x0040_0000;
/// Capable of handling server state change info
pub const CLIENT_SESSION_TRACK: u32 = 0x0080_0000;
/// Client no longer needs EOF packet
pub const CLIENT_DEPRECATE_EOF: u32 = 0x0100_0000;
/// Client supports optional trailing zeroes
pub const CLIENT_OPTIONAL_RESULTSET_METADATA: u32 = 0x0200_0000;
/// Compression extended protocol
pub const CLIENT_ZSTD_COMPRESSION_ALGORITHM: u32 = 0x0400_0000;
/// Query attributes
pub const CLIENT_QUERY_ATTRIBUTES: u32 = 0x0800_0000;
/// Multi-factor authentication
pub const CLIENT_MULTI_FACTOR_AUTHENTICATION: u32 = 0x1000_0000;
/// Capability extension
pub const CLIENT_CAPABILITY_EXTENSION: u32 = 0x2000_0000;
/// Verify server certificate
pub const CLIENT_SSL_VERIFY_SERVER_CERT: u32 = 0x4000_0000;
/// Don't reset options after failed connect
pub const CLIENT_REMEMBER_OPTIONS: u32 = 0x8000_0000;

/// Older flag name for secure connection
pub const CLIENT_SECURE_CONNECTION: u32 = CLIENT_RESERVED2;

/// Default capability flags for our proxy
pub const DEFAULT_SERVER_CAPABILITIES: u32 = CLIENT_LONG_PASSWORD
    | CLIENT_FOUND_ROWS
    | CLIENT_LONG_FLAG
    | CLIENT_CONNECT_WITH_DB
    | CLIENT_PROTOCOL_41
    | CLIENT_TRANSACTIONS
    | CLIENT_SECURE_CONNECTION
    | CLIENT_MULTI_STATEMENTS
    | CLIENT_MULTI_RESULTS
    | CLIENT_PS_MULTI_RESULTS
    | CLIENT_PLUGIN_AUTH
    | CLIENT_CONNECT_ATTRS; // Required for application name tracking

/// Default client capabilities
pub const DEFAULT_CLIENT_CAPABILITIES: u32 = CLIENT_LONG_PASSWORD
    | CLIENT_LONG_FLAG
    | CLIENT_CONNECT_WITH_DB
    | CLIENT_PROTOCOL_41
    | CLIENT_TRANSACTIONS
    | CLIENT_SECURE_CONNECTION
    | CLIENT_PLUGIN_AUTH
    | CLIENT_CONNECT_ATTRS;

// ============================================================================
// Status Flags
// ============================================================================

/// Server status: in transaction
pub const SERVER_STATUS_IN_TRANS: u16 = 0x0001;
/// Server status: auto-commit enabled
pub const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;
/// Server status: more results available
pub const SERVER_MORE_RESULTS_EXISTS: u16 = 0x0008;

// ============================================================================
// MySQL Command Types
// Reference: https://dev.mysql.com/doc/dev/mysql-server/latest/my__command_8h.html
// ============================================================================

/// Quit connection (COM_QUIT)
pub const COM_QUIT: u8 = 0x01;

/// Switch database (COM_INIT_DB)
pub const COM_INIT_DB: u8 = 0x02;

/// Execute SQL query (COM_QUERY)
pub const COM_QUERY: u8 = 0x03;

/// Get field list (COM_FIELD_LIST) - deprecated but still used
pub const COM_FIELD_LIST: u8 = 0x04;

/// Get server statistics (COM_STATISTICS)
pub const COM_STATISTICS: u8 = 0x09;

/// Ping server (COM_PING)
pub const COM_PING: u8 = 0x0e;

/// Prepare statement (COM_STMT_PREPARE)
pub const COM_STMT_PREPARE: u8 = 0x16;

/// Execute prepared statement (COM_STMT_EXECUTE)
pub const COM_STMT_EXECUTE: u8 = 0x17;

/// Close prepared statement (COM_STMT_CLOSE)
pub const COM_STMT_CLOSE: u8 = 0x19;

/// Fetch cursor row (COM_STMT_FETCH)
pub const COM_STMT_FETCH: u8 = 0x1c;

/// Reset connection (COM_RESET_CONNECTION)
pub const COM_RESET_CONNECTION: u8 = 0x1f;

/// Check if a command byte is a query command that should be counted.
///
/// Query commands are those that potentially access data or modify state.
/// These include SQL queries, prepared statement execution, and schema queries.
///
/// # Arguments
///
/// * `cmd` - The command byte from the MySQL packet
///
/// # Returns
///
/// `true` if this command should count toward query limits
///
/// # Example
///
/// ```
/// use keeperdb_proxy::protocol::mysql::packets::{is_query_command, COM_QUERY, COM_PING};
///
/// assert!(is_query_command(COM_QUERY));
/// assert!(!is_query_command(COM_PING));
/// ```
#[inline]
pub fn is_query_command(cmd: u8) -> bool {
    matches!(
        cmd,
        COM_QUERY
            | COM_STMT_EXECUTE
            | COM_STMT_PREPARE
            | COM_INIT_DB
            | COM_FIELD_LIST
            | COM_STMT_FETCH
    )
}

/// Check if a command byte is an administrative command that should NOT be counted.
///
/// Administrative commands are maintenance operations that don't access user data.
/// These include ping, quit, statistics, and connection reset.
///
/// # Arguments
///
/// * `cmd` - The command byte from the MySQL packet
///
/// # Returns
///
/// `true` if this is an administrative command
///
/// # Example
///
/// ```
/// use keeperdb_proxy::protocol::mysql::packets::{is_admin_command, COM_QUERY, COM_PING};
///
/// assert!(is_admin_command(COM_PING));
/// assert!(!is_admin_command(COM_QUERY));
/// ```
#[inline]
pub fn is_admin_command(cmd: u8) -> bool {
    matches!(
        cmd,
        COM_QUIT | COM_PING | COM_STATISTICS | COM_RESET_CONNECTION | COM_STMT_CLOSE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_query_command_com_query() {
        assert!(is_query_command(COM_QUERY));
    }

    #[test]
    fn test_is_query_command_com_stmt_execute() {
        assert!(is_query_command(COM_STMT_EXECUTE));
    }

    #[test]
    fn test_is_query_command_com_stmt_prepare() {
        assert!(is_query_command(COM_STMT_PREPARE));
    }

    #[test]
    fn test_is_query_command_com_init_db() {
        assert!(is_query_command(COM_INIT_DB));
    }

    #[test]
    fn test_is_query_command_com_field_list() {
        assert!(is_query_command(COM_FIELD_LIST));
    }

    #[test]
    fn test_is_query_command_com_stmt_fetch() {
        assert!(is_query_command(COM_STMT_FETCH));
    }

    #[test]
    fn test_is_query_command_com_ping_false() {
        assert!(!is_query_command(COM_PING));
    }

    #[test]
    fn test_is_query_command_com_quit_false() {
        assert!(!is_query_command(COM_QUIT));
    }

    #[test]
    fn test_is_admin_command_com_quit() {
        assert!(is_admin_command(COM_QUIT));
    }

    #[test]
    fn test_is_admin_command_com_ping() {
        assert!(is_admin_command(COM_PING));
    }

    #[test]
    fn test_is_admin_command_com_statistics() {
        assert!(is_admin_command(COM_STATISTICS));
    }

    #[test]
    fn test_is_admin_command_com_reset_connection() {
        assert!(is_admin_command(COM_RESET_CONNECTION));
    }

    #[test]
    fn test_is_admin_command_com_stmt_close() {
        assert!(is_admin_command(COM_STMT_CLOSE));
    }

    #[test]
    fn test_is_admin_command_com_query_false() {
        assert!(!is_admin_command(COM_QUERY));
    }

    #[test]
    fn test_command_constants_values() {
        // Verify command constants match MySQL protocol spec
        assert_eq!(COM_QUIT, 0x01);
        assert_eq!(COM_INIT_DB, 0x02);
        assert_eq!(COM_QUERY, 0x03);
        assert_eq!(COM_FIELD_LIST, 0x04);
        assert_eq!(COM_STATISTICS, 0x09);
        assert_eq!(COM_PING, 0x0e);
        assert_eq!(COM_STMT_PREPARE, 0x16);
        assert_eq!(COM_STMT_EXECUTE, 0x17);
        assert_eq!(COM_STMT_CLOSE, 0x19);
        assert_eq!(COM_STMT_FETCH, 0x1c);
        assert_eq!(COM_RESET_CONNECTION, 0x1f);
    }
}
