//! Protocol detection
//!
//! This module detects the database protocol based on connection behavior:
//! - MySQL: Server speaks first (no initial client data within timeout)
//! - PostgreSQL: Client sends SSLRequest or StartupMessage first
//! - SQL Server: Client sends TDS prelogin first

use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Detected protocol type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedProtocol {
    /// MySQL - server speaks first
    MySQL,
    /// PostgreSQL - client sends startup message first
    PostgreSQL,
    /// Microsoft SQL Server - client sends TDS prelogin first
    SQLServer,
    /// Oracle - client sends TNS CONNECT first
    Oracle,
    /// Gateway handshake - length-prefixed instruction format
    GatewayHandshake,
    /// Unknown protocol
    Unknown,
}

impl std::fmt::Display for DetectedProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetectedProtocol::MySQL => write!(f, "MySQL"),
            DetectedProtocol::PostgreSQL => write!(f, "PostgreSQL"),
            DetectedProtocol::SQLServer => write!(f, "SQL Server"),
            DetectedProtocol::Oracle => write!(f, "Oracle"),
            DetectedProtocol::GatewayHandshake => write!(f, "Gateway Handshake"),
            DetectedProtocol::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Protocol detector
pub struct ProtocolDetector {
    /// Detection timeout in milliseconds
    detection_timeout_ms: u64,
}

impl Default for ProtocolDetector {
    fn default() -> Self {
        Self {
            detection_timeout_ms: 100,
        }
    }
}

impl ProtocolDetector {
    /// Create a new protocol detector with the given timeout
    pub fn new(detection_timeout_ms: u64) -> Self {
        Self {
            detection_timeout_ms,
        }
    }

    /// Detect the protocol based on initial connection behavior
    ///
    /// This uses a peek-based approach:
    /// - Wait for the client to send data (with timeout)
    /// - If timeout expires with no data -> MySQL (server speaks first)
    /// - If data received, check the first byte to determine protocol
    ///
    /// Returns the detected protocol and any data that was peeked
    pub async fn detect(&self, stream: &TcpStream) -> (DetectedProtocol, Vec<u8>) {
        let mut buf = vec![0u8; 8];

        // Try to peek data from the client with a timeout
        let peek_result = timeout(
            Duration::from_millis(self.detection_timeout_ms),
            stream.peek(&mut buf),
        )
        .await;

        match peek_result {
            // Timeout - no data from client within timeout period
            // This indicates MySQL protocol (server speaks first)
            Err(_) => (DetectedProtocol::MySQL, Vec::new()),

            // Got data - analyze to determine protocol
            Ok(Ok(n)) if n > 0 => {
                let first_byte = buf[0];
                let protocol = self.detect_from_first_byte(first_byte, &buf[..n]);
                (protocol, buf[..n].to_vec())
            }

            // Connection closed or error
            Ok(Ok(_)) | Ok(Err(_)) => (DetectedProtocol::Unknown, Vec::new()),
        }
    }

    /// Detect protocol from the first byte of client data
    fn detect_from_first_byte(&self, first_byte: u8, data: &[u8]) -> DetectedProtocol {
        match first_byte {
            // Gateway handshake: first byte is ASCII digit (length prefix)
            // Format: <length>.<opcode>,... e.g., "6.select,5.mysql;"
            b'0'..=b'9' => {
                if self.looks_like_gateway_instruction(data) {
                    return DetectedProtocol::GatewayHandshake;
                }
                DetectedProtocol::Unknown
            }

            // Could be PostgreSQL or Oracle TNS
            // PostgreSQL SSLRequest or StartupMessage:
            //   - SSLRequest: length (4 bytes) = 8, then 80877103 (0x04D2162F)
            //   - StartupMessage: length (4 bytes), then protocol version 196608 (0x00030000)
            // Oracle TNS CONNECT:
            //   - Packet length in bytes 0-1 (big-endian, typically 50-500)
            //   - Packet type 0x01 at byte 4
            0x00 => {
                if data.len() >= 8 {
                    // Check for PostgreSQL SSLRequest magic number
                    let code = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                    if code == 80877103 {
                        return DetectedProtocol::PostgreSQL;
                    }
                    // Check for PostgreSQL StartupMessage protocol version (3.0)
                    if code == 196608 {
                        return DetectedProtocol::PostgreSQL;
                    }

                    // Check for Oracle TNS CONNECT
                    // TNS CONNECT has packet type 0x01 at byte 4
                    // Length in bytes 0-1 should be reasonable (34-2000 for CONNECT)
                    if data[4] == 0x01 {
                        let length = u16::from_be_bytes([data[0], data[1]]);
                        if (34..=2000).contains(&length) {
                            return DetectedProtocol::Oracle;
                        }
                    }
                }
                // Default to PostgreSQL for 0x00 start
                DetectedProtocol::PostgreSQL
            }

            // PostgreSQL CancelRequest starts with length then 80877102
            0x04 => DetectedProtocol::PostgreSQL,

            // SQL Server TDS prelogin
            // TDS packet type 0x12 = prelogin
            0x12 => DetectedProtocol::SQLServer,

            // Other first bytes could be various things
            _ => {
                // Check for Oracle TNS CONNECT with larger length
                // (first byte > 0 means length > 255, which is common for CONNECT)
                if data.len() >= 8 && data[4] == 0x01 {
                    let length = u16::from_be_bytes([data[0], data[1]]);
                    // CONNECT packets are typically 50-500 bytes
                    if (34..=2000).contains(&length) {
                        return DetectedProtocol::Oracle;
                    }
                }
                DetectedProtocol::Unknown
            }
        }
    }

    /// Check if data looks like a Gateway instruction.
    ///
    /// Gateway instructions start with a length prefix (digits) followed by a dot,
    /// then the element content. Example: "6.select,5.mysql;"
    fn looks_like_gateway_instruction(&self, data: &[u8]) -> bool {
        // Need at least a few bytes to detect the pattern
        if data.len() < 3 {
            return false;
        }

        // Find the dot separator
        let dot_pos = data.iter().position(|&b| b == b'.');

        match dot_pos {
            // Dot found - check that everything before it is digits
            Some(pos) if pos > 0 && pos < 5 => {
                // Length prefix should be 1-4 digits (supporting up to 9999 byte elements)
                data[..pos].iter().all(|&b| b.is_ascii_digit())
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_display() {
        assert_eq!(DetectedProtocol::MySQL.to_string(), "MySQL");
        assert_eq!(DetectedProtocol::PostgreSQL.to_string(), "PostgreSQL");
        assert_eq!(DetectedProtocol::SQLServer.to_string(), "SQL Server");
        assert_eq!(DetectedProtocol::Oracle.to_string(), "Oracle");
        assert_eq!(
            DetectedProtocol::GatewayHandshake.to_string(),
            "Gateway Handshake"
        );
        assert_eq!(DetectedProtocol::Unknown.to_string(), "Unknown");
    }

    #[test]
    fn test_detect_from_first_byte() {
        let detector = ProtocolDetector::default();

        // PostgreSQL SSLRequest
        let ssl_request = [0x00, 0x00, 0x00, 0x08, 0x04, 0xD2, 0x16, 0x2F];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &ssl_request),
            DetectedProtocol::PostgreSQL
        );

        // PostgreSQL StartupMessage
        let startup = [0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00, 0x00];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &startup),
            DetectedProtocol::PostgreSQL
        );

        // SQL Server TDS prelogin
        assert_eq!(
            detector.detect_from_first_byte(0x12, &[0x12]),
            DetectedProtocol::SQLServer
        );
    }

    #[test]
    fn test_detect_gateway_handshake() {
        let detector = ProtocolDetector::default();

        // Valid Gateway select instruction
        let select = b"6.select,5.mysql;";
        assert_eq!(
            detector.detect_from_first_byte(b'6', select),
            DetectedProtocol::GatewayHandshake
        );

        // Valid Gateway with single digit length
        let args = b"4.args,4.host;";
        assert_eq!(
            detector.detect_from_first_byte(b'4', args),
            DetectedProtocol::GatewayHandshake
        );

        // Valid Gateway with multi-digit length
        let connect = b"10.somecommand;";
        assert_eq!(
            detector.detect_from_first_byte(b'1', connect),
            DetectedProtocol::GatewayHandshake
        );
    }

    #[test]
    fn test_detect_gateway_invalid() {
        let detector = ProtocolDetector::default();

        // Too short (only 2 bytes, need at least 3)
        assert_eq!(
            detector.detect_from_first_byte(b'6', b"6."),
            DetectedProtocol::Unknown
        );

        // No dot - should fail
        assert_eq!(
            detector.detect_from_first_byte(b'6', b"6select"),
            DetectedProtocol::Unknown
        );

        // Letter after digits before dot
        assert_eq!(
            detector.detect_from_first_byte(b'6', b"6a.select"),
            DetectedProtocol::Unknown
        );
    }

    #[test]
    fn test_looks_like_gateway_instruction() {
        let detector = ProtocolDetector::default();

        // Valid patterns
        assert!(detector.looks_like_gateway_instruction(b"6.select"));
        assert!(detector.looks_like_gateway_instruction(b"1.a"));
        assert!(detector.looks_like_gateway_instruction(b"123.test"));
        assert!(detector.looks_like_gateway_instruction(b"9999.x"));

        // Invalid patterns
        assert!(!detector.looks_like_gateway_instruction(b""));
        assert!(!detector.looks_like_gateway_instruction(b"ab"));
        assert!(!detector.looks_like_gateway_instruction(b".select"));
        assert!(!detector.looks_like_gateway_instruction(b"select.6"));
        assert!(!detector.looks_like_gateway_instruction(b"6select"));
        assert!(!detector.looks_like_gateway_instruction(b"12345.x")); // 5 digits, too long
    }

    #[test]
    fn test_detect_oracle_tns_connect() {
        let detector = ProtocolDetector::default();

        // Oracle TNS CONNECT packet (typical)
        // Length = 100 (0x0064), checksum = 0, type = 0x01 (CONNECT), flags = 0
        let tns_connect = [
            0x00, 0x64, // length = 100 (big-endian)
            0x00, 0x00, // packet checksum
            0x01, // packet type = CONNECT
            0x00, // flags
            0x00, 0x00, // header checksum
        ];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &tns_connect),
            DetectedProtocol::Oracle
        );

        // Oracle TNS CONNECT with larger length (first byte > 0)
        // Length = 300 (0x012C)
        let tns_connect_large = [
            0x01, 0x2C, // length = 300 (big-endian)
            0x00, 0x00, // packet checksum
            0x01, // packet type = CONNECT
            0x00, // flags
            0x00, 0x00, // header checksum
        ];
        assert_eq!(
            detector.detect_from_first_byte(0x01, &tns_connect_large),
            DetectedProtocol::Oracle
        );
    }

    #[test]
    fn test_detect_oracle_not_connect() {
        let detector = ProtocolDetector::default();

        // TNS DATA packet (type = 0x06) should not be detected as Oracle
        // (we only detect CONNECT for initial detection)
        let tns_data = [
            0x00, 0x64, // length = 100
            0x00, 0x00, // checksum
            0x06, // type = DATA (not CONNECT)
            0x00, 0x00, 0x00,
        ];
        // 0x00 first byte with non-CONNECT type will fall through to PostgreSQL default
        assert_eq!(
            detector.detect_from_first_byte(0x00, &tns_data),
            DetectedProtocol::PostgreSQL
        );
    }

    #[test]
    fn test_detect_oracle_vs_postgresql() {
        let detector = ProtocolDetector::default();

        // PostgreSQL SSLRequest should NOT be detected as Oracle
        let ssl_request = [0x00, 0x00, 0x00, 0x08, 0x04, 0xD2, 0x16, 0x2F];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &ssl_request),
            DetectedProtocol::PostgreSQL
        );

        // PostgreSQL StartupMessage should NOT be detected as Oracle
        let startup = [0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00, 0x00];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &startup),
            DetectedProtocol::PostgreSQL
        );
    }

    #[test]
    fn test_detect_oracle_length_bounds() {
        let detector = ProtocolDetector::default();

        // Length too small (33 < 34) - should not detect as Oracle
        let too_small = [
            0x00, 0x21, // length = 33
            0x00, 0x00, 0x01, // CONNECT
            0x00, 0x00, 0x00,
        ];
        assert_eq!(
            detector.detect_from_first_byte(0x00, &too_small),
            DetectedProtocol::PostgreSQL // Falls through to default
        );

        // Length too large (2001 > 2000) - should not detect as Oracle
        let too_large = [
            0x07, 0xD1, // length = 2001
            0x00, 0x00, 0x01, // CONNECT
            0x00, 0x00, 0x00,
        ];
        assert_eq!(
            detector.detect_from_first_byte(0x07, &too_large),
            DetectedProtocol::Unknown // _ catch-all returns Unknown
        );
    }
}
