//! TNS protocol constants
//!
//! This module defines constants for the TNS (Transparent Network Substrate)
//! protocol used by Oracle Database. Reference: Oracle Net Services documentation.

/// TNS packet types
pub mod packet_type {
    /// Connection request from client
    pub const CONNECT: u8 = 0x01;
    /// Connection accepted by server
    pub const ACCEPT: u8 = 0x02;
    /// Acknowledgment
    pub const ACK: u8 = 0x03;
    /// Connection refused by server
    pub const REFUSE: u8 = 0x04;
    /// Redirect to different address
    pub const REDIRECT: u8 = 0x05;
    /// Data packet (queries, auth, results)
    pub const DATA: u8 = 0x06;
    /// Null/empty packet
    pub const NULL: u8 = 0x07;
    /// Abort connection
    pub const ABORT: u8 = 0x09;
    /// Request packet resend
    pub const RESEND: u8 = 0x0B;
    /// Attention marker
    pub const MARKER: u8 = 0x0C;
    /// Attention signal
    pub const ATTENTION: u8 = 0x0D;
    /// Control packet
    pub const CONTROL: u8 = 0x0E;
}

/// TNS header size in bytes
pub const TNS_HEADER_SIZE: usize = 8;

/// Minimum valid TNS packet size (header only)
pub const TNS_MIN_PACKET_SIZE: usize = TNS_HEADER_SIZE;

/// Maximum TNS packet size (64KB - 1)
pub const TNS_MAX_PACKET_SIZE: usize = 65535;

/// Default Session Data Unit size
pub const DEFAULT_SDU_SIZE: u16 = 8192;

/// Default Maximum Transmission Data Unit size
pub const DEFAULT_TDU_SIZE: u16 = 65535;

/// TNS protocol versions
pub mod version {
    /// TNS version for Oracle 8i
    pub const TNS_VERSION_300: u16 = 300;
    /// TNS version for Oracle 9i
    pub const TNS_VERSION_310: u16 = 310;
    /// TNS version for Oracle 10g
    pub const TNS_VERSION_313: u16 = 313;
    /// TNS version for Oracle 11g
    pub const TNS_VERSION_314: u16 = 314;
    /// TNS version for Oracle 12c
    pub const TNS_VERSION_315: u16 = 315;
    /// TNS version for Oracle 18c+
    pub const TNS_VERSION_316: u16 = 316;
    /// TNS version for Oracle 19c+
    pub const TNS_VERSION_318: u16 = 318;
    /// TNS version for Oracle 21c+
    pub const TNS_VERSION_320: u16 = 320;

    /// Minimum version we support
    pub const MIN_SUPPORTED: u16 = TNS_VERSION_313;
    /// Default version to advertise
    pub const DEFAULT: u16 = TNS_VERSION_318;
}

/// Data packet flags (in DATA packet header)
pub mod data_flags {
    /// Send token flag
    pub const SEND_TOKEN: u16 = 0x0001;
    /// Request confirmation
    pub const REQUEST_CONFIRMATION: u16 = 0x0002;
    /// Confirmation flag
    pub const CONFIRMATION: u16 = 0x0004;
    /// Reserved flag
    pub const RESERVED: u16 = 0x0008;
    /// More data to follow
    pub const MORE_DATA: u16 = 0x0020;
    /// End of file/data
    pub const EOF: u16 = 0x0040;
    /// Data includes length prefix
    pub const DATA_LENGTH_INCLUDED: u16 = 0x0080;
    /// Out of band data
    pub const OOB_DATA: u16 = 0x0100;
    /// Do not request confirmation
    pub const NO_CONFIRM: u16 = 0x0200;
}

/// Connect flags byte 0
pub mod connect_flags_0 {
    /// Services wanted
    pub const SERVICES_WANTED: u8 = 0x01;
    /// Interchange involved
    pub const INTERCHANGE_INVOLVED: u8 = 0x02;
    /// Services enabled
    pub const SERVICES_ENABLED: u8 = 0x04;
    /// Services linked in
    pub const SERVICES_LINKED_IN: u8 = 0x08;
    /// Services required
    pub const SERVICES_REQUIRED: u8 = 0x10;
    /// Can send attention
    pub const CAN_SEND_ATTENTION: u8 = 0x20;
    /// Can receive attention
    pub const CAN_RECEIVE_ATTENTION: u8 = 0x40;
}

/// Connect flags byte 1
pub mod connect_flags_1 {
    /// Full duplex
    pub const FULL_DUPLEX: u8 = 0x01;
    /// Half duplex
    pub const HALF_DUPLEX: u8 = 0x02;
    /// Unknown / reserved
    pub const UNKNOWN: u8 = 0x04;
    /// TCPS support (TLS)
    pub const TCPS: u8 = 0x08;
}

/// Refuse reason codes
pub mod refuse_reason {
    /// Invalid username/password
    pub const INVALID_CREDENTIALS: u8 = 0x01;
    /// Service unavailable
    pub const SERVICE_UNAVAILABLE: u8 = 0x02;
    /// Connection limit reached
    pub const CONNECTION_LIMIT: u8 = 0x03;
    /// Protocol version mismatch
    pub const VERSION_MISMATCH: u8 = 0x04;
}

/// Authentication types
pub mod auth_type {
    /// O5LOGON (Oracle 5 style logon - challenge/response)
    pub const O5LOGON: u8 = 0x76; // 'v' in ASCII
    /// Oracle 11g+ authentication
    pub const O11LOGON: u8 = 0x77;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_types() {
        assert_eq!(packet_type::CONNECT, 0x01);
        assert_eq!(packet_type::ACCEPT, 0x02);
        assert_eq!(packet_type::REFUSE, 0x04);
        assert_eq!(packet_type::DATA, 0x06);
    }

    #[test]
    fn test_header_size() {
        assert_eq!(TNS_HEADER_SIZE, 8);
    }

    #[test]
    fn test_versions() {
        const _: () = assert!(version::MIN_SUPPORTED <= version::DEFAULT);
        const _: () = assert!(version::TNS_VERSION_313 < version::TNS_VERSION_318);
    }
}
