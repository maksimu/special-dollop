//! TDS protocol constants
//!
//! This module defines constants for the TDS (Tabular Data Stream) protocol
//! used by Microsoft SQL Server. Reference: MS-TDS specification.

/// TDS packet types
pub mod packet_type {
    /// SQL batch (query)
    pub const SQL_BATCH: u8 = 0x01;
    /// RPC request (stored procedure call)
    pub const RPC: u8 = 0x03;
    /// Tabular result
    pub const TABULAR_RESULT: u8 = 0x04;
    /// Attention signal (cancel)
    pub const ATTENTION: u8 = 0x06;
    /// Bulk load data
    pub const BULK_LOAD: u8 = 0x07;
    /// Federated authentication token
    pub const FED_AUTH_TOKEN: u8 = 0x08;
    /// Transaction manager request
    pub const TRANS_MGR_REQ: u8 = 0x0E;
    /// TDS7 login
    pub const LOGIN7: u8 = 0x10;
    /// SSPI message
    pub const SSPI: u8 = 0x11;
    /// Pre-login
    pub const PRELOGIN: u8 = 0x12;
    /// SSL/TLS Payload (used during TDS-wrapped TLS handshake)
    pub const TLS_PAYLOAD: u8 = 0x14;
}

/// TDS packet status flags
pub mod status {
    /// Normal message
    pub const NORMAL: u8 = 0x00;
    /// End of message (last packet in request)
    pub const EOM: u8 = 0x01;
    /// Ignore this event (async)
    pub const IGNORE: u8 = 0x02;
    /// Reset connection
    pub const RESET_CONNECTION: u8 = 0x08;
    /// Reset connection keeping transaction state
    pub const RESET_CONNECTION_KEEP_TRAN: u8 = 0x10;
}

/// TDS protocol versions
pub mod version {
    /// TDS 7.0 (SQL Server 7.0)
    pub const TDS_7_0: u32 = 0x7000_0000;
    /// TDS 7.1 (SQL Server 2000)
    pub const TDS_7_1: u32 = 0x7100_0000;
    /// TDS 7.2 (SQL Server 2005)
    pub const TDS_7_2: u32 = 0x7200_0000;
    /// TDS 7.3 (SQL Server 2008)
    pub const TDS_7_3: u32 = 0x7300_000B;
    /// TDS 7.4 (SQL Server 2012+) - Our target version
    pub const TDS_7_4: u32 = 0x7400_0004;
}

/// PRELOGIN token types
pub mod prelogin_token {
    /// Version information
    pub const VERSION: u8 = 0x00;
    /// Encryption setting
    pub const ENCRYPTION: u8 = 0x01;
    /// Instance name
    pub const INSTOPT: u8 = 0x02;
    /// Thread ID
    pub const THREADID: u8 = 0x03;
    /// MARS (Multiple Active Result Sets)
    pub const MARS: u8 = 0x04;
    /// Trace ID
    pub const TRACEID: u8 = 0x05;
    /// Federated authentication required
    pub const FEDAUTHREQUIRED: u8 = 0x06;
    /// Nonce option
    pub const NONCEOPT: u8 = 0x07;
    /// Terminator
    pub const TERMINATOR: u8 = 0xFF;
}

/// Encryption modes for PRELOGIN negotiation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum EncryptionMode {
    /// Encryption is off (no encryption)
    #[default]
    Off = 0x00,
    /// Encryption is on (full encryption after login)
    On = 0x01,
    /// Encryption is not supported by client/server
    NotSupported = 0x02,
    /// Encryption is required
    Required = 0x03,
}

impl EncryptionMode {
    /// Parse encryption mode from byte
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x00 => Some(EncryptionMode::Off),
            0x01 => Some(EncryptionMode::On),
            0x02 => Some(EncryptionMode::NotSupported),
            0x03 => Some(EncryptionMode::Required),
            _ => None,
        }
    }

    /// Convert to byte
    pub fn to_byte(self) -> u8 {
        self as u8
    }
}

/// LOGIN7 option flags 1
pub mod option_flags1 {
    /// Byte order: 0 = little-endian (Intel)
    pub const BYTE_ORDER_INTEL: u8 = 0x00;
    /// Character set: 0 = ASCII
    pub const CHAR_ASCII: u8 = 0x00;
    /// Float type: 0 = IEEE 754
    pub const FLOAT_IEEE_754: u8 = 0x00;
    /// Dump/load: 0 = on (use default)
    pub const DUMP_LOAD_ON: u8 = 0x00;
    /// Use DB: 0 = notify on, 1 = notify off
    pub const USE_DB_NOTIFY_ON: u8 = 0x00;
    /// Database: 0 = warning, 1 = fatal
    pub const DATABASE_WARNING: u8 = 0x00;
    /// Set language: 0 = warning
    pub const SET_LANG_WARNING: u8 = 0x00;
}

/// LOGIN7 option flags 2
pub mod option_flags2 {
    /// Language: 0 = init warning, 1 = init fatal
    pub const LANGUAGE_INIT_WARNING: u8 = 0x00;
    /// ODBC: 0 = not ODBC, 1 = ODBC
    pub const ODBC_ON: u8 = 0x01;
    /// User type: 0 = normal, 1 = server, 2 = remuser, 3 = sqlrepl
    pub const USER_NORMAL: u8 = 0x00;
    /// Integrated security: 0 = off, 1 = on (SSPI)
    pub const INT_SECURITY_OFF: u8 = 0x00;
    pub const INT_SECURITY_ON: u8 = 0x80;
}

/// LOGIN7 type flags
pub mod type_flags {
    /// SQL type: 0 = default SQL, 1 = T-SQL
    pub const SQL_TSQL: u8 = 0x01;
    /// OLEDB: 0 = off, 1 = on
    pub const OLEDB_OFF: u8 = 0x00;
    /// Read-only intent: 0 = read-write, 1 = read-only
    pub const READ_ONLY_INTENT: u8 = 0x20;
}

/// LOGIN7 option flags 3
pub mod option_flags3 {
    /// Change password: 0 = no, 1 = yes
    pub const CHANGE_PASSWORD_NO: u8 = 0x00;
    /// Send YUKON binary XML
    pub const SEND_YUKON_BINARY_XML: u8 = 0x02;
    /// User instance: 0 = not user instance
    pub const USER_INSTANCE_OFF: u8 = 0x00;
    /// Unknown collation handling: 0 = as error
    pub const UNKNOWN_COLLATION_ERROR: u8 = 0x00;
    /// Extension used: 0 = no extension
    pub const EXTENSION_NOT_USED: u8 = 0x00;
}

/// Token types in tabular result stream
pub mod token_type {
    /// Column metadata
    pub const COLMETADATA: u8 = 0x81;
    /// Error message
    pub const ERROR: u8 = 0xAA;
    /// Info message
    pub const INFO: u8 = 0xAB;
    /// Return status
    pub const RETURNSTATUS: u8 = 0x79;
    /// Return value
    pub const RETURNVALUE: u8 = 0xAC;
    /// Login acknowledgement
    pub const LOGINACK: u8 = 0xAD;
    /// Feature extension acknowledgement
    pub const FEATUREEXTACK: u8 = 0xAE;
    /// Row data
    pub const ROW: u8 = 0xD1;
    /// NBC (Null Bitmap Compression) row
    pub const NBCROW: u8 = 0xD2;
    /// Environment change
    pub const ENVCHANGE: u8 = 0xE3;
    /// Session state
    pub const SESSIONSTATE: u8 = 0xE4;
    /// SSPI message
    pub const SSPI: u8 = 0xED;
    /// Done
    pub const DONE: u8 = 0xFD;
    /// Done in proc
    pub const DONEINPROC: u8 = 0xFF;
    /// Done proc
    pub const DONEPROC: u8 = 0xFE;
    /// Order by
    pub const ORDER: u8 = 0xA9;
}

/// Environment change types
pub mod env_change_type {
    /// Database changed
    pub const DATABASE: u8 = 0x01;
    /// Language changed
    pub const LANGUAGE: u8 = 0x02;
    /// Character set changed
    pub const CHARSET: u8 = 0x03;
    /// Packet size changed
    pub const PACKET_SIZE: u8 = 0x04;
    /// Sort order changed
    pub const SORT_ORDER: u8 = 0x05;
    /// Collation changed
    pub const COLLATION: u8 = 0x07;
    /// Begin transaction
    pub const BEGIN_TRAN: u8 = 0x08;
    /// Commit transaction
    pub const COMMIT_TRAN: u8 = 0x09;
    /// Rollback transaction
    pub const ROLLBACK_TRAN: u8 = 0x0A;
    /// Routing changed
    pub const ROUTING: u8 = 0x14;
}

/// TDS header size in bytes
pub const TDS_HEADER_SIZE: usize = 8;

/// Default TDS packet size
pub const DEFAULT_PACKET_SIZE: u32 = 4096;

/// Maximum TDS packet size
pub const MAX_PACKET_SIZE: u32 = 32767;

/// LOGIN7 header size (fixed portion before variable data)
pub const LOGIN7_HEADER_SIZE: usize = 94;

/// Maximum LOGIN7 packet size
pub const MAX_LOGIN7_SIZE: usize = 65535;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_mode_roundtrip() {
        assert_eq!(EncryptionMode::from_byte(0x00), Some(EncryptionMode::Off));
        assert_eq!(EncryptionMode::from_byte(0x01), Some(EncryptionMode::On));
        assert_eq!(
            EncryptionMode::from_byte(0x02),
            Some(EncryptionMode::NotSupported)
        );
        assert_eq!(
            EncryptionMode::from_byte(0x03),
            Some(EncryptionMode::Required)
        );
        assert_eq!(EncryptionMode::from_byte(0x04), None);

        assert_eq!(EncryptionMode::Off.to_byte(), 0x00);
        assert_eq!(EncryptionMode::On.to_byte(), 0x01);
        assert_eq!(EncryptionMode::Required.to_byte(), 0x03);
    }

    #[test]
    fn test_tds_version_values() {
        // TDS 7.4 should be 0x74000004
        assert_eq!(version::TDS_7_4, 0x7400_0004);
    }

    #[test]
    fn test_encryption_mode_default() {
        assert_eq!(EncryptionMode::default(), EncryptionMode::Off);
    }

    #[test]
    fn test_all_tds_versions_ordered() {
        // Versions should be in ascending order
        const _: () = assert!(version::TDS_7_0 < version::TDS_7_1);
        const _: () = assert!(version::TDS_7_1 < version::TDS_7_2);
        const _: () = assert!(version::TDS_7_2 < version::TDS_7_3);
        const _: () = assert!(version::TDS_7_3 < version::TDS_7_4);
    }

    #[test]
    fn test_packet_types_are_distinct() {
        // Ensure no collision between packet types we use
        let types = [
            packet_type::SQL_BATCH,
            packet_type::RPC,
            packet_type::TABULAR_RESULT,
            packet_type::ATTENTION,
            packet_type::LOGIN7,
            packet_type::PRELOGIN,
        ];

        for i in 0..types.len() {
            for j in (i + 1)..types.len() {
                assert_ne!(types[i], types[j], "Packet types {} and {} collide", i, j);
            }
        }
    }

    #[test]
    fn test_prelogin_token_terminator() {
        assert_eq!(prelogin_token::TERMINATOR, 0xFF);
    }

    #[test]
    fn test_token_types_for_auth() {
        // Verify token types used in authentication
        assert_eq!(token_type::LOGINACK, 0xAD);
        assert_eq!(token_type::ERROR, 0xAA);
        assert_eq!(token_type::ENVCHANGE, 0xE3);
    }

    #[test]
    fn test_header_size_constant() {
        assert_eq!(TDS_HEADER_SIZE, 8);
    }

    #[test]
    fn test_login7_header_size() {
        assert_eq!(LOGIN7_HEADER_SIZE, 94);
    }
}
