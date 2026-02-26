//! Protocol-specific query extraction from wire protocol bytes.
//!
//! Each database protocol encodes SQL queries differently in its wire format.
//! The [`QueryExtractor`] trait provides a unified interface for extracting
//! query information and response metadata from raw relay traffic.

use super::log_entry::SqlCommandType;

/// Information extracted from a client-to-server query packet.
#[derive(Debug, Clone)]
pub struct QueryInfo {
    /// Detected SQL command type.
    pub command_type: SqlCommandType,
    /// SQL query text, if available and enabled.
    /// `None` when query text capture is disabled or the protocol
    /// doesn't expose query text (e.g., Oracle TNS binary protocol).
    pub query_text: Option<String>,
    /// Whether this is a prepared statement execution (no raw SQL in packet).
    pub is_prepared: bool,
}

/// Information extracted from a server-to-client response packet.
#[derive(Debug, Clone)]
pub struct ResponseInfo {
    /// Whether the query succeeded.
    pub success: bool,
    /// Number of rows affected (for INSERT/UPDATE/DELETE).
    pub rows_affected: Option<u64>,
    /// Database-specific error code on failure.
    pub error_code: Option<u32>,
    /// Error message on failure.
    pub error_message: Option<String>,
}

/// Extracts SQL query information from raw protocol bytes.
///
/// Each database protocol implements this differently based on its wire format.
/// The extractor is called during the relay loop on every client/server read,
/// so implementations must be fast (< 1ms budget).
pub trait QueryExtractor: Send + Sync {
    /// Check if a client-to-server buffer contains a query command.
    ///
    /// Returns `Some(QueryInfo)` if the data contains a query,
    /// `None` if it's not a query (admin command, auth packet, etc.).
    ///
    /// The buffer may contain a partial or complete protocol packet.
    /// Implementations should handle both cases gracefully.
    fn extract_query(&self, client_data: &[u8]) -> Option<QueryInfo>;

    /// Check if a server-to-client buffer contains a query response.
    ///
    /// Returns `Some(ResponseInfo)` if the data contains result metadata
    /// (OK/ERR for MySQL, CommandComplete/ErrorResponse for PostgreSQL, etc.).
    ///
    /// Returns `None` if the data is part of a result set or not a final response.
    fn extract_response(&self, server_data: &[u8]) -> Option<ResponseInfo>;

    /// Protocol name for logging context (e.g., "mysql", "postgresql").
    fn protocol_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// MySQL QueryExtractor
// ---------------------------------------------------------------------------

use crate::protocol::mysql::packets::{
    is_query_command, COM_INIT_DB, COM_QUERY, COM_STMT_EXECUTE, COM_STMT_FETCH, COM_STMT_PREPARE,
};

/// MySQL query extractor.
///
/// MySQL packet format:
/// - Bytes 0-2: Payload length (3 bytes, little-endian)
/// - Byte 3: Sequence ID
/// - Byte 4: Command byte (COM_QUERY = 0x03, etc.)
/// - Bytes 5+: Payload (query text for COM_QUERY)
pub struct MysqlQueryExtractor {
    include_query_text: bool,
    max_query_length: usize,
}

impl MysqlQueryExtractor {
    pub fn new(include_query_text: bool, max_query_length: usize) -> Self {
        Self {
            include_query_text,
            max_query_length,
        }
    }

    /// Extract query text from a COM_QUERY or COM_STMT_PREPARE packet.
    /// The query starts at byte 5 (after 4-byte header + 1-byte command).
    fn extract_text(&self, data: &[u8]) -> Option<String> {
        if !self.include_query_text || data.len() <= 5 {
            return None;
        }

        let text = String::from_utf8_lossy(&data[5..]);
        let text = if text.len() > self.max_query_length {
            format!("{}...[truncated]", &text[..self.max_query_length])
        } else {
            text.into_owned()
        };
        Some(text)
    }
}

impl QueryExtractor for MysqlQueryExtractor {
    fn extract_query(&self, client_data: &[u8]) -> Option<QueryInfo> {
        // Need at least 5 bytes: 4-byte header + 1-byte command
        if client_data.len() < 5 {
            return None;
        }

        let cmd = client_data[4];

        // Skip non-query commands
        if !is_query_command(cmd) {
            return None;
        }

        match cmd {
            COM_QUERY => Some(QueryInfo {
                command_type: self
                    .extract_text(client_data)
                    .as_deref()
                    .map(SqlCommandType::from_query_text)
                    .unwrap_or(SqlCommandType::Unknown),
                query_text: self.extract_text(client_data),
                is_prepared: false,
            }),
            COM_STMT_PREPARE => Some(QueryInfo {
                command_type: self
                    .extract_text(client_data)
                    .as_deref()
                    .map(SqlCommandType::from_query_text)
                    .unwrap_or(SqlCommandType::PreparedStatement),
                query_text: self.extract_text(client_data),
                is_prepared: true,
            }),
            COM_STMT_EXECUTE | COM_STMT_FETCH => Some(QueryInfo {
                command_type: SqlCommandType::PreparedStatement,
                query_text: None, // No raw SQL in execute/fetch packets
                is_prepared: true,
            }),
            COM_INIT_DB => {
                let db_name = if client_data.len() > 5 {
                    Some(String::from_utf8_lossy(&client_data[5..]).into_owned())
                } else {
                    None
                };
                Some(QueryInfo {
                    command_type: SqlCommandType::Other("USE".to_string()),
                    query_text: db_name.map(|db| format!("USE {}", db)),
                    is_prepared: false,
                })
            }
            _ => Some(QueryInfo {
                command_type: SqlCommandType::Unknown,
                query_text: None,
                is_prepared: false,
            }),
        }
    }

    fn extract_response(&self, server_data: &[u8]) -> Option<ResponseInfo> {
        // Need at least 5 bytes: 4-byte header + 1-byte response type
        if server_data.len() < 5 {
            return None;
        }

        let response_type = server_data[4];

        match response_type {
            // OK packet (0x00) - query succeeded
            0x00 => {
                let affected_rows = read_lenenc_int(&server_data[5..]);
                Some(ResponseInfo {
                    success: true,
                    rows_affected: affected_rows,
                    error_code: None,
                    error_message: None,
                })
            }
            // EOF packet as OK (0xFE with payload < 9 bytes) - deprecated OK
            0xFE if server_data.len() < 13 => Some(ResponseInfo {
                success: true,
                rows_affected: None,
                error_code: None,
                error_message: None,
            }),
            // ERR packet (0xFF) - query failed
            0xFF if server_data.len() >= 7 => {
                let error_code = u16::from_le_bytes([server_data[5], server_data[6]]) as u32;

                // Error message starts after error_code (2 bytes) + optional sql_state_marker (1) + sql_state (5)
                let msg_offset = if server_data.len() > 12 && server_data[7] == b'#' {
                    13 // header(4) + type(1) + code(2) + marker(1) + state(5)
                } else {
                    7 // header(4) + type(1) + code(2)
                };

                let error_message = if server_data.len() > msg_offset {
                    Some(String::from_utf8_lossy(&server_data[msg_offset..]).into_owned())
                } else {
                    None
                };

                Some(ResponseInfo {
                    success: false,
                    rows_affected: None,
                    error_code: Some(error_code),
                    error_message,
                })
            }
            // Result set header (column count) - this is a SELECT response.
            // The first byte is a length-encoded column count (not 0x00, 0xFE, or 0xFF).
            // We treat it as success; rows_affected not applicable for SELECTs.
            _ => Some(ResponseInfo {
                success: true,
                rows_affected: None,
                error_code: None,
                error_message: None,
            }),
        }
    }

    fn protocol_name(&self) -> &str {
        "mysql"
    }
}

// ---------------------------------------------------------------------------
// PostgreSQL QueryExtractor
// ---------------------------------------------------------------------------

use crate::protocol::postgres::constants::{
    MSG_BIND, MSG_COMMAND_COMPLETE, MSG_EMPTY_QUERY, MSG_ERROR_RESPONSE, MSG_EXECUTE, MSG_PARSE,
    MSG_QUERY, MSG_TERMINATE,
};

/// PostgreSQL query extractor.
///
/// PostgreSQL wire protocol (v3):
/// - Byte 0: Message type (1 byte)
/// - Bytes 1-4: Length (4 bytes, big-endian, includes length field itself)
/// - Bytes 5+: Payload
///
/// Client messages of interest:
/// - 'Q' (Simple Query): type + length + null-terminated SQL string
/// - 'P' (Parse): type + length + stmt_name\0 + query\0 + param_count + param_oids...
/// - 'B' (Bind): type + length + portal\0 + stmt_name\0 + ... (no query text)
/// - 'E' (Execute): type + length + portal\0 + max_rows (no query text)
pub struct PostgresQueryExtractor {
    include_query_text: bool,
    max_query_length: usize,
}

impl PostgresQueryExtractor {
    pub fn new(include_query_text: bool, max_query_length: usize) -> Self {
        Self {
            include_query_text,
            max_query_length,
        }
    }

    /// Read a null-terminated string from a byte slice starting at `offset`.
    /// Returns the string and the offset past the null terminator.
    fn read_cstring(data: &[u8], offset: usize) -> Option<(&str, usize)> {
        let remaining = data.get(offset..)?;
        let null_pos = remaining.iter().position(|&b| b == 0)?;
        let s = std::str::from_utf8(&remaining[..null_pos]).ok()?;
        Some((s, offset + null_pos + 1))
    }

    /// Extract and optionally truncate query text.
    fn maybe_capture_text(&self, text: &str) -> Option<String> {
        if !self.include_query_text {
            return None;
        }
        if text.len() > self.max_query_length {
            Some(format!("{}...[truncated]", &text[..self.max_query_length]))
        } else {
            Some(text.to_string())
        }
    }

    /// Parse a CommandComplete tag to extract rows affected.
    ///
    /// PostgreSQL command tags:
    /// - "SELECT n" → n rows
    /// - "INSERT oid n" → n rows
    /// - "UPDATE n" / "DELETE n" / "MOVE n" / "FETCH n" / "COPY n" → n rows
    fn parse_command_tag(tag: &str) -> Option<u64> {
        // The last space-separated token is the row count for DML commands
        let parts: Vec<&str> = tag.split_whitespace().collect();
        match parts.first().copied() {
            Some("SELECT" | "UPDATE" | "DELETE" | "MOVE" | "FETCH" | "COPY") => {
                parts.last().and_then(|s| s.parse::<u64>().ok())
            }
            Some("INSERT") => {
                // INSERT oid count - last token is the count
                parts.last().and_then(|s| s.parse::<u64>().ok())
            }
            _ => None,
        }
    }
}

impl QueryExtractor for PostgresQueryExtractor {
    fn extract_query(&self, client_data: &[u8]) -> Option<QueryInfo> {
        // Need at least 5 bytes: type(1) + length(4)
        if client_data.len() < 5 {
            return None;
        }

        let msg_type = client_data[0];

        match msg_type {
            MSG_QUERY => {
                // Simple Query: type(1) + length(4) + query\0
                let (query_text, _) = Self::read_cstring(client_data, 5)?;
                let command_type = SqlCommandType::from_query_text(query_text);
                Some(QueryInfo {
                    command_type,
                    query_text: self.maybe_capture_text(query_text),
                    is_prepared: false,
                })
            }
            MSG_PARSE => {
                // Parse: type(1) + length(4) + stmt_name\0 + query\0 + ...
                let (_, after_name) = Self::read_cstring(client_data, 5)?;
                let (query_text, _) = Self::read_cstring(client_data, after_name)?;
                let command_type = SqlCommandType::from_query_text(query_text);
                Some(QueryInfo {
                    command_type,
                    query_text: self.maybe_capture_text(query_text),
                    is_prepared: true,
                })
            }
            MSG_BIND => {
                // Bind: prepared statement execution, no query text available
                Some(QueryInfo {
                    command_type: SqlCommandType::PreparedStatement,
                    query_text: None,
                    is_prepared: true,
                })
            }
            MSG_EXECUTE => {
                // Execute: prepared statement execution, no query text available
                Some(QueryInfo {
                    command_type: SqlCommandType::PreparedStatement,
                    query_text: None,
                    is_prepared: true,
                })
            }
            MSG_TERMINATE => None,
            _ => None,
        }
    }

    fn extract_response(&self, server_data: &[u8]) -> Option<ResponseInfo> {
        // Scan through all PostgreSQL messages in the buffer.
        // PostgreSQL wire protocol packs multiple messages per TCP read:
        //   RowDescription(T) + DataRow(D)* + CommandComplete(C) + ReadyForQuery(Z)
        // We must iterate past RowDescription/DataRow to find the terminal message
        // (CommandComplete, ErrorResponse, or EmptyQueryResponse).
        let mut offset = 0;

        while offset + 5 <= server_data.len() {
            let msg_type = server_data[offset];
            let msg_len = u32::from_be_bytes([
                server_data[offset + 1],
                server_data[offset + 2],
                server_data[offset + 3],
                server_data[offset + 4],
            ]) as usize;

            // Length includes itself (4 bytes) but not the type byte.
            // Minimum valid length is 4.
            if msg_len < 4 {
                break;
            }
            let total_msg_size = 1 + msg_len;

            match msg_type {
                MSG_COMMAND_COMPLETE => {
                    // CommandComplete: type(1) + length(4) + tag\0
                    let msg_data = &server_data[offset..];
                    let (tag, _) = Self::read_cstring(msg_data, 5)?;
                    let rows_affected = Self::parse_command_tag(tag);
                    return Some(ResponseInfo {
                        success: true,
                        rows_affected,
                        error_code: None,
                        error_message: None,
                    });
                }
                MSG_ERROR_RESPONSE => {
                    let msg_end = (offset + total_msg_size).min(server_data.len());
                    let msg_data = &server_data[offset..msg_end];

                    let mut error_code: Option<u32> = None;
                    let mut error_message: Option<String> = None;
                    let mut sqlstate: Option<String> = None;

                    let mut field_offset = 5;
                    while field_offset < msg_data.len() {
                        let field_type = msg_data[field_offset];
                        field_offset += 1;

                        if field_type == 0 {
                            break;
                        }

                        if let Some((value, next_offset)) =
                            Self::read_cstring(msg_data, field_offset)
                        {
                            field_offset = next_offset;
                            match field_type {
                                b'C' => {
                                    sqlstate = Some(value.to_string());
                                }
                                b'M' => {
                                    error_message = Some(value.to_string());
                                }
                                _ => {}
                            }
                        } else {
                            break;
                        }
                    }

                    if let Some(ref state) = sqlstate {
                        let bytes = state.as_bytes();
                        let code = if bytes.len() >= 4 {
                            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                        } else {
                            0
                        };
                        error_code = Some(code);
                    }

                    return Some(ResponseInfo {
                        success: false,
                        rows_affected: None,
                        error_code,
                        error_message,
                    });
                }
                MSG_EMPTY_QUERY => {
                    return Some(ResponseInfo {
                        success: true,
                        rows_affected: Some(0),
                        error_code: None,
                        error_message: None,
                    });
                }
                _ => {
                    // Skip non-terminal messages (RowDescription, DataRow,
                    // ReadyForQuery, ParseComplete, BindComplete, etc.)
                    if offset + total_msg_size > server_data.len() {
                        break; // Partial message at end of buffer
                    }
                    offset += total_msg_size;
                }
            }
        }

        None
    }

    fn protocol_name(&self) -> &str {
        "postgresql"
    }
}

// ---------------------------------------------------------------------------
// SQL Server (TDS) QueryExtractor
// ---------------------------------------------------------------------------

use crate::protocol::sqlserver::auth::utf16le_to_string;
use crate::protocol::sqlserver::constants::{
    packet_type as tds_packet, token_type as tds_token, TDS_HEADER_SIZE,
};

/// SQL Server TDS query extractor.
///
/// TDS packet format (8-byte header):
/// - Byte 0: Packet type (SQL_BATCH=0x01, RPC=0x03, TABULAR_RESULT=0x04, etc.)
/// - Byte 1: Status (EOM=0x01 for last packet)
/// - Bytes 2-3: Total packet length (big-endian, includes header)
/// - Bytes 4-5: SPID (big-endian)
/// - Byte 6: Packet ID
/// - Byte 7: Window (always 0)
///
/// SQL_BATCH payload: ALL_HEADERS section + UTF-16LE query text
/// TABULAR_RESULT payload: token stream (DONE, ERROR, ROW, etc.)
pub struct TdsQueryExtractor {
    include_query_text: bool,
    max_query_length: usize,
}

impl TdsQueryExtractor {
    pub fn new(include_query_text: bool, max_query_length: usize) -> Self {
        Self {
            include_query_text,
            max_query_length,
        }
    }

    /// Skip the ALL_HEADERS section in a SQL_BATCH payload.
    ///
    /// ALL_HEADERS format:
    /// - 4 bytes: TotalLength (LE u32, includes itself)
    ///
    /// Returns the offset past ALL_HEADERS, or None if data is too short.
    fn skip_all_headers(payload: &[u8]) -> Option<usize> {
        if payload.len() < 4 {
            return None;
        }
        let total_len =
            u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;

        if total_len < 4 || total_len > payload.len() {
            // If total_len is unreasonable, assume no ALL_HEADERS section
            // (older TDS versions might not include it)
            return Some(0);
        }
        Some(total_len)
    }

    /// Extract UTF-16LE query text from a SQL_BATCH payload (after ALL_HEADERS).
    fn extract_sql_text(&self, payload: &[u8]) -> Option<String> {
        if !self.include_query_text || payload.is_empty() {
            return None;
        }

        let text = utf16le_to_string(payload)?;
        // Trim trailing null characters
        let text = text.trim_end_matches('\0');
        if text.is_empty() {
            return None;
        }

        if text.len() > self.max_query_length {
            Some(format!("{}...[truncated]", &text[..self.max_query_length]))
        } else {
            Some(text.to_string())
        }
    }
}

impl QueryExtractor for TdsQueryExtractor {
    fn extract_query(&self, client_data: &[u8]) -> Option<QueryInfo> {
        if client_data.len() < TDS_HEADER_SIZE + 1 {
            return None;
        }

        let packet_type = client_data[0];

        match packet_type {
            tds_packet::SQL_BATCH => {
                let payload = &client_data[TDS_HEADER_SIZE..];
                let offset = Self::skip_all_headers(payload).unwrap_or(0);
                let sql_data = payload.get(offset..)?;

                if sql_data.is_empty() {
                    return None;
                }

                let text = self.extract_sql_text(sql_data);
                let command_type = text
                    .as_deref()
                    .map(SqlCommandType::from_query_text)
                    .unwrap_or(SqlCommandType::Unknown);

                Some(QueryInfo {
                    command_type,
                    query_text: text,
                    is_prepared: false,
                })
            }
            tds_packet::RPC => {
                // RPC calls (stored procedures) - no SQL text directly available
                Some(QueryInfo {
                    command_type: SqlCommandType::PreparedStatement,
                    query_text: None,
                    is_prepared: true,
                })
            }
            _ => None,
        }
    }

    fn extract_response(&self, server_data: &[u8]) -> Option<ResponseInfo> {
        if server_data.len() < TDS_HEADER_SIZE + 1 {
            return None;
        }

        let packet_type = server_data[0];
        if packet_type != tds_packet::TABULAR_RESULT {
            return None;
        }

        // Check the first token in the token stream for DONE/ERROR
        let payload = &server_data[TDS_HEADER_SIZE..];
        if payload.is_empty() {
            return None;
        }

        let token = payload[0];
        match token {
            tds_token::DONE | tds_token::DONEPROC | tds_token::DONEINPROC => {
                // DONE token: type(1) + status(2) + curcmd(2) + rowcount(8) = 13 bytes
                if payload.len() < 13 {
                    return None;
                }
                let status = u16::from_le_bytes([payload[1], payload[2]]);
                let row_count = u64::from_le_bytes([
                    payload[5],
                    payload[6],
                    payload[7],
                    payload[8],
                    payload[9],
                    payload[10],
                    payload[11],
                    payload[12],
                ]);

                let has_error = status & 0x0002 != 0; // DONE_ERROR bit
                let has_count = status & 0x0010 != 0; // DONE_COUNT bit

                Some(ResponseInfo {
                    success: !has_error,
                    rows_affected: if has_count { Some(row_count) } else { None },
                    error_code: None,
                    error_message: None,
                })
            }
            tds_token::ERROR => {
                // ERROR token: type(1) + length(2) + number(4) + state(1) + class(1)
                //   + msglen(2) + message(UTF-16LE) + ...
                if payload.len() < 9 {
                    return None;
                }
                let error_number =
                    u32::from_le_bytes([payload[3], payload[4], payload[5], payload[6]]);

                // Message length in characters
                if payload.len() < 11 {
                    return Some(ResponseInfo {
                        success: false,
                        rows_affected: None,
                        error_code: Some(error_number),
                        error_message: None,
                    });
                }

                let msg_chars = u16::from_le_bytes([payload[9], payload[10]]) as usize;
                let msg_bytes = msg_chars * 2;
                let msg_start = 11;
                let msg_end = msg_start + msg_bytes;

                let error_message = if msg_end <= payload.len() {
                    utf16le_to_string(&payload[msg_start..msg_end])
                } else {
                    None
                };

                Some(ResponseInfo {
                    success: false,
                    rows_affected: None,
                    error_code: Some(error_number),
                    error_message,
                })
            }
            _ => None,
        }
    }

    fn protocol_name(&self) -> &str {
        "sqlserver"
    }
}

// ---------------------------------------------------------------------------
// Oracle TNS QueryExtractor (metadata-only)
// ---------------------------------------------------------------------------

use crate::protocol::oracle::constants::{
    packet_type as tns_packet, TNS_HEADER_SIZE as TNS_HDR_SIZE,
};

/// Oracle TNS query extractor (metadata-only).
///
/// Oracle TNS uses a binary protocol where SQL query text is not directly
/// visible in the wire format. This extractor provides metadata-only logging:
/// - Counts client DATA packets as query operations
/// - Detects basic success/failure from server DATA packet patterns
///
/// TNS header format (8 bytes):
/// - Bytes 0-1: Packet length (big-endian)
/// - Bytes 2-3: Packet checksum
/// - Byte 4: Packet type (DATA=0x06, etc.)
/// - Byte 5: Reserved
/// - Bytes 6-7: Header checksum
///
/// DATA packet payload:
/// - Bytes 8-9: Data flags (2 bytes)
/// - Bytes 10+: Data content (binary, protocol-specific)
///
/// Oracle TNS is a binary protocol where SQL text is not directly visible
/// in most packet types. This extractor detects query activity based on
/// TNS packet types (DATA packets with specific data flags) and marks
/// the command type as [`SqlCommandType::Unknown`] since the actual SQL
/// cannot be reliably extracted from the binary wire format.
pub struct OracleQueryExtractor;

impl Default for OracleQueryExtractor {
    fn default() -> Self {
        Self
    }
}

impl OracleQueryExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl QueryExtractor for OracleQueryExtractor {
    fn extract_query(&self, client_data: &[u8]) -> Option<QueryInfo> {
        // Need at least TNS header (8 bytes) + data flags (2 bytes) + 1 byte data
        if client_data.len() < TNS_HDR_SIZE + 3 {
            return None;
        }

        let packet_type = client_data[4];

        if packet_type != tns_packet::DATA {
            return None;
        }

        // DATA packets from client are query/command operations.
        // We can't extract the SQL text from the binary TNS protocol.
        Some(QueryInfo {
            command_type: SqlCommandType::Unknown,
            query_text: None,
            is_prepared: false,
        })
    }

    fn extract_response(&self, server_data: &[u8]) -> Option<ResponseInfo> {
        if server_data.len() < TNS_HDR_SIZE + 3 {
            return None;
        }

        let packet_type = server_data[4];

        if packet_type != tns_packet::DATA {
            return None;
        }

        // For Oracle, we can only detect that a response occurred.
        // Without deep TNS token parsing, we assume success.
        // Error detection would require parsing the binary response tokens,
        // which is deferred to a future iteration.
        Some(ResponseInfo {
            success: true,
            rows_affected: None,
            error_code: None,
            error_message: None,
        })
    }

    fn protocol_name(&self) -> &str {
        "oracle"
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read a MySQL length-encoded integer from a byte slice.
/// Returns the value if parseable, None otherwise.
fn read_lenenc_int(data: &[u8]) -> Option<u64> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        // Single byte value (0-250)
        n if n < 0xFB => Some(n as u64),
        // NULL (0xFB)
        0xFB => None,
        // 2-byte integer (0xFC)
        0xFC if data.len() >= 3 => Some(u16::from_le_bytes([data[1], data[2]]) as u64),
        // 3-byte integer (0xFD)
        0xFD if data.len() >= 4 => Some(u32::from_le_bytes([data[1], data[2], data[3], 0]) as u64),
        // 8-byte integer (0xFE)
        0xFE if data.len() >= 9 => Some(u64::from_le_bytes([
            data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
        ])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::mysql::packets::COM_QUIT;

    fn mysql_extractor() -> MysqlQueryExtractor {
        MysqlQueryExtractor::new(true, 10_000)
    }

    fn mysql_no_text() -> MysqlQueryExtractor {
        MysqlQueryExtractor::new(false, 10_000)
    }

    /// Build a MySQL packet: 3-byte length + 1-byte seq + payload
    fn build_mysql_packet(seq: u8, payload: &[u8]) -> Vec<u8> {
        let len = payload.len() as u32;
        let mut pkt = Vec::with_capacity(4 + payload.len());
        pkt.push((len & 0xFF) as u8);
        pkt.push(((len >> 8) & 0xFF) as u8);
        pkt.push(((len >> 16) & 0xFF) as u8);
        pkt.push(seq);
        pkt.extend_from_slice(payload);
        pkt
    }

    // -----------------------------------------------------------------------
    // extract_query tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_mysql_com_query_select() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(
            0,
            &[COM_QUERY, b'S', b'E', b'L', b'E', b'C', b'T', b' ', b'1'],
        );
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Select);
        assert_eq!(info.query_text.as_deref(), Some("SELECT 1"));
        assert!(!info.is_prepared);
    }

    #[test]
    fn test_mysql_com_query_insert() {
        let ext = mysql_extractor();
        let sql = b"INSERT INTO t VALUES (1)";
        let mut payload = vec![COM_QUERY];
        payload.extend_from_slice(sql);
        let pkt = build_mysql_packet(0, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Insert);
        assert_eq!(info.query_text.as_deref(), Some("INSERT INTO t VALUES (1)"));
    }

    #[test]
    fn test_mysql_com_query_no_text_mode() {
        let ext = mysql_no_text();
        let pkt = build_mysql_packet(
            0,
            &[COM_QUERY, b'S', b'E', b'L', b'E', b'C', b'T', b' ', b'1'],
        );
        let info = ext.extract_query(&pkt).unwrap();
        // Command type still detected even without text (from query text when available,
        // but since text is disabled, we get Unknown)
        assert!(info.query_text.is_none());
    }

    #[test]
    fn test_mysql_com_stmt_prepare() {
        let ext = mysql_extractor();
        let sql = b"SELECT * FROM users WHERE id = ?";
        let mut payload = vec![COM_STMT_PREPARE];
        payload.extend_from_slice(sql);
        let pkt = build_mysql_packet(0, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert!(info.is_prepared);
        assert!(info.query_text.is_some());
    }

    #[test]
    fn test_mysql_com_stmt_execute() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(0, &[COM_STMT_EXECUTE, 0x01, 0x00, 0x00, 0x00]);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::PreparedStatement);
        assert!(info.query_text.is_none()); // No raw SQL in execute
        assert!(info.is_prepared);
    }

    #[test]
    fn test_mysql_com_init_db() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(0, &[COM_INIT_DB, b'm', b'y', b'd', b'b']);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Other("USE".to_string()));
        assert_eq!(info.query_text.as_deref(), Some("USE mydb"));
    }

    #[test]
    fn test_mysql_com_ping_returns_none() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(0, &[0x0e]); // COM_PING
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_mysql_com_quit_returns_none() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(0, &[COM_QUIT]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_mysql_too_short_returns_none() {
        let ext = mysql_extractor();
        assert!(ext.extract_query(&[0x01, 0x00, 0x00]).is_none()); // only 3 bytes
        assert!(ext.extract_query(&[]).is_none());
    }

    #[test]
    fn test_mysql_query_truncation() {
        let ext = MysqlQueryExtractor::new(true, 10);
        let sql = b"SELECT * FROM very_long_table_name WHERE condition";
        let mut payload = vec![COM_QUERY];
        payload.extend_from_slice(sql);
        let pkt = build_mysql_packet(0, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        let text = info.query_text.unwrap();
        assert!(text.contains("...[truncated]"));
        assert!(text.starts_with("SELECT * F")); // first 10 chars
    }

    // -----------------------------------------------------------------------
    // extract_response tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_mysql_ok_packet() {
        let ext = mysql_extractor();
        // OK packet: header(4) + 0x00 + affected_rows(1) + last_insert_id(1) + status(2) + warnings(2)
        let pkt = build_mysql_packet(1, &[0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x00]);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(3));
        assert!(resp.error_code.is_none());
    }

    #[test]
    fn test_mysql_ok_zero_affected() {
        let ext = mysql_extractor();
        let pkt = build_mysql_packet(1, &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(0));
    }

    #[test]
    fn test_mysql_err_packet() {
        let ext = mysql_extractor();
        // ERR: 0xFF + error_code(2) + '#' + sql_state(5) + message
        let mut payload = vec![0xFF];
        payload.extend_from_slice(&1045u16.to_le_bytes()); // error_code = 1045
        payload.push(b'#');
        payload.extend_from_slice(b"28000"); // SQLSTATE
        payload.extend_from_slice(b"Access denied");
        let pkt = build_mysql_packet(1, &payload);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error_code, Some(1045));
        assert_eq!(resp.error_message.as_deref(), Some("Access denied"));
    }

    #[test]
    fn test_mysql_err_without_sqlstate() {
        let ext = mysql_extractor();
        // ERR without SQLSTATE marker
        let mut payload = vec![0xFF];
        payload.extend_from_slice(&2003u16.to_le_bytes());
        payload.extend_from_slice(b"Connection refused");
        let pkt = build_mysql_packet(1, &payload);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error_code, Some(2003));
        assert_eq!(resp.error_message.as_deref(), Some("Connection refused"));
    }

    #[test]
    fn test_mysql_result_set_header() {
        let ext = mysql_extractor();
        // Result set: column count = 3 (lenenc int)
        let pkt = build_mysql_packet(1, &[0x03]);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert!(resp.rows_affected.is_none()); // SELECT doesn't have affected rows
    }

    #[test]
    fn test_mysql_response_too_short() {
        let ext = mysql_extractor();
        assert!(ext.extract_response(&[0x01, 0x00, 0x00]).is_none());
    }

    // -----------------------------------------------------------------------
    // read_lenenc_int tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_lenenc_single_byte() {
        assert_eq!(read_lenenc_int(&[0]), Some(0));
        assert_eq!(read_lenenc_int(&[42]), Some(42));
        assert_eq!(read_lenenc_int(&[250]), Some(250));
    }

    #[test]
    fn test_lenenc_two_byte() {
        assert_eq!(read_lenenc_int(&[0xFC, 0x00, 0x01]), Some(256));
        assert_eq!(read_lenenc_int(&[0xFC, 0xFF, 0xFF]), Some(65535));
    }

    #[test]
    fn test_lenenc_three_byte() {
        assert_eq!(read_lenenc_int(&[0xFD, 0x01, 0x00, 0x00]), Some(1));
    }

    #[test]
    fn test_lenenc_null() {
        assert_eq!(read_lenenc_int(&[0xFB]), None);
    }

    #[test]
    fn test_lenenc_empty() {
        assert_eq!(read_lenenc_int(&[]), None);
    }

    #[test]
    fn test_protocol_name() {
        let ext = mysql_extractor();
        assert_eq!(ext.protocol_name(), "mysql");
    }

    // =======================================================================
    // PostgreSQL QueryExtractor tests
    // =======================================================================

    fn pg_extractor() -> PostgresQueryExtractor {
        PostgresQueryExtractor::new(true, 10_000)
    }

    fn pg_no_text() -> PostgresQueryExtractor {
        PostgresQueryExtractor::new(false, 10_000)
    }

    /// Build a PostgreSQL message: type(1) + length(4, big-endian, includes self) + payload
    fn build_pg_message(msg_type: u8, payload: &[u8]) -> Vec<u8> {
        let length = (payload.len() as u32) + 4; // length includes itself
        let mut pkt = Vec::with_capacity(1 + 4 + payload.len());
        pkt.push(msg_type);
        pkt.extend_from_slice(&length.to_be_bytes());
        pkt.extend_from_slice(payload);
        pkt
    }

    // -----------------------------------------------------------------------
    // PostgreSQL extract_query tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_pg_simple_query_select() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'Q', b"SELECT * FROM users\0");
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Select);
        assert_eq!(info.query_text.as_deref(), Some("SELECT * FROM users"));
        assert!(!info.is_prepared);
    }

    #[test]
    fn test_pg_simple_query_insert() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'Q', b"INSERT INTO t VALUES (1)\0");
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Insert);
        assert_eq!(info.query_text.as_deref(), Some("INSERT INTO t VALUES (1)"));
    }

    #[test]
    fn test_pg_simple_query_no_text_mode() {
        let ext = pg_no_text();
        let pkt = build_pg_message(b'Q', b"SELECT 1\0");
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Select);
        assert!(info.query_text.is_none());
    }

    #[test]
    fn test_pg_parse_message() {
        let ext = pg_extractor();
        // Parse: stmt_name\0 + query\0 + param_count(2) + param_oids...
        let mut payload = Vec::new();
        payload.extend_from_slice(b"stmt1\0");
        payload.extend_from_slice(b"SELECT * FROM users WHERE id = $1\0");
        payload.extend_from_slice(&0u16.to_be_bytes()); // 0 parameter types
        let pkt = build_pg_message(b'P', &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Select);
        assert!(info.is_prepared);
        assert!(info.query_text.unwrap().contains("SELECT * FROM users"));
    }

    #[test]
    fn test_pg_parse_unnamed_statement() {
        let ext = pg_extractor();
        // Unnamed statement: empty string \0 + query\0
        let mut payload = Vec::new();
        payload.push(0); // empty statement name
        payload.extend_from_slice(b"UPDATE t SET x = 1\0");
        payload.extend_from_slice(&0u16.to_be_bytes());
        let pkt = build_pg_message(b'P', &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Update);
        assert!(info.is_prepared);
    }

    #[test]
    fn test_pg_bind_message() {
        let ext = pg_extractor();
        // Bind: portal\0 + stmt_name\0 + ... (we just need a minimal valid one)
        let pkt = build_pg_message(b'B', b"\0stmt1\0\x00\x00\x00\x00\x00\x00");
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::PreparedStatement);
        assert!(info.query_text.is_none());
        assert!(info.is_prepared);
    }

    #[test]
    fn test_pg_execute_message() {
        let ext = pg_extractor();
        // Execute: portal\0 + max_rows(4)
        let mut payload = Vec::new();
        payload.push(0); // unnamed portal
        payload.extend_from_slice(&0u32.to_be_bytes()); // 0 = no limit
        let pkt = build_pg_message(b'E', &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::PreparedStatement);
        assert!(info.is_prepared);
    }

    #[test]
    fn test_pg_terminate_returns_none() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'X', &[]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_pg_sync_returns_none() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'S', &[]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_pg_too_short_returns_none() {
        let ext = pg_extractor();
        assert!(ext.extract_query(&[b'Q', 0x00, 0x00]).is_none());
        assert!(ext.extract_query(&[]).is_none());
    }

    #[test]
    fn test_pg_query_truncation() {
        let ext = PostgresQueryExtractor::new(true, 10);
        let pkt = build_pg_message(
            b'Q',
            b"SELECT * FROM very_long_table_name WHERE condition\0",
        );
        let info = ext.extract_query(&pkt).unwrap();
        let text = info.query_text.unwrap();
        assert!(text.contains("...[truncated]"));
        assert!(text.starts_with("SELECT * F")); // first 10 chars
    }

    // -----------------------------------------------------------------------
    // PostgreSQL extract_response tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_pg_command_complete_select() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'C', b"SELECT 42\0");
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(42));
    }

    #[test]
    fn test_pg_command_complete_insert() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'C', b"INSERT 0 5\0");
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(5));
    }

    #[test]
    fn test_pg_command_complete_update() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'C', b"UPDATE 3\0");
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(3));
    }

    #[test]
    fn test_pg_command_complete_delete() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'C', b"DELETE 0\0");
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(0));
    }

    #[test]
    fn test_pg_command_complete_create_table() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'C', b"CREATE TABLE\0");
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert!(resp.rows_affected.is_none()); // CREATE doesn't have row count
    }

    #[test]
    fn test_pg_error_response() {
        let ext = pg_extractor();
        // ErrorResponse: field_type + value\0 + ... + \0
        let mut payload = Vec::new();
        payload.push(b'S'); // Severity
        payload.extend_from_slice(b"ERROR\0");
        payload.push(b'C'); // Code (SQLSTATE)
        payload.extend_from_slice(b"42P01\0");
        payload.push(b'M'); // Message
        payload.extend_from_slice(b"relation \"foo\" does not exist\0");
        payload.push(0); // terminator
        let pkt = build_pg_message(b'E', &payload);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
        assert!(resp.error_code.is_some());
        assert_eq!(
            resp.error_message.as_deref(),
            Some("relation \"foo\" does not exist")
        );
    }

    #[test]
    fn test_pg_error_response_auth_failure() {
        let ext = pg_extractor();
        let mut payload = Vec::new();
        payload.push(b'S');
        payload.extend_from_slice(b"FATAL\0");
        payload.push(b'C');
        payload.extend_from_slice(b"28P01\0");
        payload.push(b'M');
        payload.extend_from_slice(b"password authentication failed\0");
        payload.push(0);
        let pkt = build_pg_message(b'E', &payload);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
        assert_eq!(
            resp.error_message.as_deref(),
            Some("password authentication failed")
        );
    }

    #[test]
    fn test_pg_empty_query_response() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'I', &[]);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(0));
    }

    #[test]
    fn test_pg_ready_for_query_returns_none() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'Z', b"I"); // Idle
        assert!(ext.extract_response(&pkt).is_none());
    }

    #[test]
    fn test_pg_data_row_returns_none() {
        let ext = pg_extractor();
        let pkt = build_pg_message(b'D', &[0x00, 0x01, 0x00, 0x00, 0x00, 0x01, b'x']);
        assert!(ext.extract_response(&pkt).is_none());
    }

    #[test]
    fn test_pg_response_too_short() {
        let ext = pg_extractor();
        assert!(ext.extract_response(&[b'C', 0x00]).is_none());
    }

    #[test]
    fn test_pg_protocol_name() {
        let ext = pg_extractor();
        assert_eq!(ext.protocol_name(), "postgresql");
    }

    #[test]
    fn test_pg_command_tag_parsing() {
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("SELECT 100"),
            Some(100)
        );
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("INSERT 0 5"),
            Some(5)
        );
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("UPDATE 10"),
            Some(10)
        );
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("DELETE 0"),
            Some(0)
        );
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("COPY 50"),
            Some(50)
        );
        assert_eq!(
            PostgresQueryExtractor::parse_command_tag("CREATE TABLE"),
            None
        );
        assert_eq!(PostgresQueryExtractor::parse_command_tag("BEGIN"), None);
    }

    #[test]
    fn test_pg_multi_message_select_response() {
        // Simulates a real PostgreSQL SELECT response buffer:
        // RowDescription(T) + DataRow(D) + CommandComplete(C) + ReadyForQuery(Z)
        let ext = pg_extractor();
        let mut buf = Vec::new();
        buf.extend_from_slice(&build_pg_message(
            b'T',
            &[
                0x00, 0x01, /* ... column desc */ 0x00, 0x00, 0x00, 0x00, 0x00, 0x17, 0x00,
                0x04, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00,
            ],
        ));
        buf.extend_from_slice(&build_pg_message(
            b'D',
            &[0x00, 0x01, 0x00, 0x00, 0x00, 0x01, b'1'],
        ));
        buf.extend_from_slice(&build_pg_message(b'C', b"SELECT 1\0"));
        buf.extend_from_slice(&build_pg_message(b'Z', b"I"));
        let resp = ext.extract_response(&buf).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(1));
    }

    #[test]
    fn test_pg_multi_message_error_after_row_desc() {
        // RowDescription followed by ErrorResponse
        let ext = pg_extractor();
        let mut buf = Vec::new();
        buf.extend_from_slice(&build_pg_message(b'T', &[0x00, 0x00]));
        let mut err_payload = Vec::new();
        err_payload.push(b'M');
        err_payload.extend_from_slice(b"division by zero\0");
        err_payload.push(0);
        buf.extend_from_slice(&build_pg_message(b'E', &err_payload));
        let resp = ext.extract_response(&buf).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error_message.as_deref(), Some("division by zero"));
    }

    #[test]
    fn test_pg_only_row_description_returns_none() {
        // Buffer contains only RowDescription - CommandComplete not yet arrived
        let ext = pg_extractor();
        let buf = build_pg_message(
            b'T',
            &[
                0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x17, 0x00, 0x04, 0xFF, 0xFF, 0xFF, 0xFF,
                0x00, 0x00,
            ],
        );
        assert!(ext.extract_response(&buf).is_none());
    }

    // =======================================================================
    // SQL Server (TDS) QueryExtractor tests
    // =======================================================================

    use crate::protocol::sqlserver::auth::string_to_utf16le;

    fn tds_extractor() -> TdsQueryExtractor {
        TdsQueryExtractor::new(true, 10_000)
    }

    fn tds_no_text() -> TdsQueryExtractor {
        TdsQueryExtractor::new(false, 10_000)
    }

    /// Build a TDS packet: 8-byte header + payload
    fn build_tds_packet(packet_type: u8, payload: &[u8]) -> Vec<u8> {
        let total_len = (TDS_HEADER_SIZE + payload.len()) as u16;
        let mut pkt = Vec::with_capacity(TDS_HEADER_SIZE + payload.len());
        pkt.push(packet_type); // type
        pkt.push(0x01); // status: EOM
        pkt.extend_from_slice(&total_len.to_be_bytes()); // length (BE)
        pkt.extend_from_slice(&[0, 0]); // SPID
        pkt.push(1); // packet ID
        pkt.push(0); // window
        pkt.extend_from_slice(payload);
        pkt
    }

    /// Build a SQL_BATCH payload with ALL_HEADERS + UTF-16LE query
    fn build_sql_batch_payload(query: &str) -> Vec<u8> {
        let query_utf16 = string_to_utf16le(query);
        // Simple ALL_HEADERS: total_length(4) = 22, one header for transaction descriptor
        let all_headers_total: u32 = 22;
        let header_len: u32 = 18;
        let header_type: u16 = 0x0002; // Transaction descriptor
        let mut payload = Vec::new();
        payload.extend_from_slice(&all_headers_total.to_le_bytes());
        payload.extend_from_slice(&header_len.to_le_bytes());
        payload.extend_from_slice(&header_type.to_le_bytes());
        payload.extend_from_slice(&[0u8; 12]); // transaction descriptor + outstanding requests
        payload.extend_from_slice(&query_utf16);
        payload
    }

    // -----------------------------------------------------------------------
    // TDS extract_query tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tds_sql_batch_select() {
        let ext = tds_extractor();
        let payload = build_sql_batch_payload("SELECT * FROM users");
        let pkt = build_tds_packet(tds_packet::SQL_BATCH, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Select);
        assert_eq!(info.query_text.as_deref(), Some("SELECT * FROM users"));
        assert!(!info.is_prepared);
    }

    #[test]
    fn test_tds_sql_batch_insert() {
        let ext = tds_extractor();
        let payload = build_sql_batch_payload("INSERT INTO t VALUES (1)");
        let pkt = build_tds_packet(tds_packet::SQL_BATCH, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Insert);
        assert_eq!(info.query_text.as_deref(), Some("INSERT INTO t VALUES (1)"));
    }

    #[test]
    fn test_tds_sql_batch_no_text_mode() {
        let ext = tds_no_text();
        let payload = build_sql_batch_payload("SELECT 1");
        let pkt = build_tds_packet(tds_packet::SQL_BATCH, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        assert!(info.query_text.is_none());
        assert_eq!(info.command_type, SqlCommandType::Unknown); // no text → unknown
    }

    #[test]
    fn test_tds_rpc_request() {
        let ext = tds_extractor();
        // Minimal RPC payload (just enough to be recognized)
        let pkt = build_tds_packet(tds_packet::RPC, &[0; 10]);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::PreparedStatement);
        assert!(info.query_text.is_none());
        assert!(info.is_prepared);
    }

    #[test]
    fn test_tds_login_returns_none() {
        let ext = tds_extractor();
        let pkt = build_tds_packet(tds_packet::LOGIN7, &[0; 20]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_tds_prelogin_returns_none() {
        let ext = tds_extractor();
        let pkt = build_tds_packet(tds_packet::PRELOGIN, &[0; 10]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_tds_too_short_returns_none() {
        let ext = tds_extractor();
        assert!(ext.extract_query(&[0x01, 0x01, 0x00, 0x08]).is_none()); // only 4 bytes
        assert!(ext.extract_query(&[]).is_none());
    }

    #[test]
    fn test_tds_query_truncation() {
        let ext = TdsQueryExtractor::new(true, 10);
        let payload = build_sql_batch_payload("SELECT * FROM very_long_table_name WHERE condition");
        let pkt = build_tds_packet(tds_packet::SQL_BATCH, &payload);
        let info = ext.extract_query(&pkt).unwrap();
        let text = info.query_text.unwrap();
        assert!(text.contains("...[truncated]"));
        assert!(text.starts_with("SELECT * F"));
    }

    // -----------------------------------------------------------------------
    // TDS extract_response tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tds_done_success_with_count() {
        let ext = tds_extractor();
        // DONE token: type(1) + status(2) + curcmd(2) + rowcount(8)
        let mut tokens = Vec::new();
        tokens.push(tds_token::DONE);
        tokens.extend_from_slice(&0x0010u16.to_le_bytes()); // DONE_COUNT set
        tokens.extend_from_slice(&0x00C1u16.to_le_bytes()); // curcmd (SELECT)
        tokens.extend_from_slice(&5u64.to_le_bytes()); // 5 rows
        let pkt = build_tds_packet(tds_packet::TABULAR_RESULT, &tokens);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(5));
    }

    #[test]
    fn test_tds_done_success_no_count() {
        let ext = tds_extractor();
        let mut tokens = Vec::new();
        tokens.push(tds_token::DONE);
        tokens.extend_from_slice(&0x0000u16.to_le_bytes()); // no flags
        tokens.extend_from_slice(&0x0000u16.to_le_bytes()); // curcmd
        tokens.extend_from_slice(&0u64.to_le_bytes());
        let pkt = build_tds_packet(tds_packet::TABULAR_RESULT, &tokens);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert!(resp.rows_affected.is_none());
    }

    #[test]
    fn test_tds_done_with_error_flag() {
        let ext = tds_extractor();
        let mut tokens = Vec::new();
        tokens.push(tds_token::DONE);
        tokens.extend_from_slice(&0x0002u16.to_le_bytes()); // DONE_ERROR
        tokens.extend_from_slice(&0x0000u16.to_le_bytes());
        tokens.extend_from_slice(&0u64.to_le_bytes());
        let pkt = build_tds_packet(tds_packet::TABULAR_RESULT, &tokens);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
    }

    #[test]
    fn test_tds_doneproc() {
        let ext = tds_extractor();
        let mut tokens = Vec::new();
        tokens.push(tds_token::DONEPROC);
        tokens.extend_from_slice(&0x0010u16.to_le_bytes()); // DONE_COUNT
        tokens.extend_from_slice(&0x0000u16.to_le_bytes());
        tokens.extend_from_slice(&3u64.to_le_bytes());
        let pkt = build_tds_packet(tds_packet::TABULAR_RESULT, &tokens);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert_eq!(resp.rows_affected, Some(3));
    }

    #[test]
    fn test_tds_error_token() {
        let ext = tds_extractor();
        let msg = "Invalid column name 'foo'";
        let msg_utf16 = string_to_utf16le(msg);
        let msg_chars = msg.encode_utf16().count() as u16;

        let mut tokens = Vec::new();
        tokens.push(tds_token::ERROR);
        // Length: 4(number) + 1(state) + 1(class) + 2(msglen) + msg_bytes + ...
        let token_len = (4 + 1 + 1 + 2 + msg_utf16.len()) as u16;
        tokens.extend_from_slice(&token_len.to_le_bytes());
        tokens.extend_from_slice(&207u32.to_le_bytes()); // error number 207
        tokens.push(1); // state
        tokens.push(16); // class (severity)
        tokens.extend_from_slice(&msg_chars.to_le_bytes());
        tokens.extend_from_slice(&msg_utf16);

        let pkt = build_tds_packet(tds_packet::TABULAR_RESULT, &tokens);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error_code, Some(207));
        assert_eq!(
            resp.error_message.as_deref(),
            Some("Invalid column name 'foo'")
        );
    }

    #[test]
    fn test_tds_non_tabular_result_returns_none() {
        let ext = tds_extractor();
        // A SQL_BATCH is not a server response
        let pkt = build_tds_packet(tds_packet::SQL_BATCH, &[0; 10]);
        assert!(ext.extract_response(&pkt).is_none());
    }

    #[test]
    fn test_tds_response_too_short() {
        let ext = tds_extractor();
        assert!(ext.extract_response(&[0x04, 0x01, 0x00]).is_none());
    }

    #[test]
    fn test_tds_protocol_name() {
        let ext = tds_extractor();
        assert_eq!(ext.protocol_name(), "sqlserver");
    }

    // =======================================================================
    // Oracle TNS QueryExtractor tests
    // =======================================================================

    /// Build a TNS packet: 8-byte header + payload
    /// Header: length(2 BE) + checksum(2) + type(1) + reserved(1) + header_checksum(2)
    fn build_tns_packet(packet_type: u8, payload: &[u8]) -> Vec<u8> {
        let total_len = (TNS_HDR_SIZE + payload.len()) as u16;
        let mut pkt = Vec::with_capacity(TNS_HDR_SIZE + payload.len());
        pkt.extend_from_slice(&total_len.to_be_bytes()); // length (BE)
        pkt.extend_from_slice(&[0, 0]); // packet checksum
        pkt.push(packet_type); // packet type
        pkt.push(0); // reserved
        pkt.extend_from_slice(&[0, 0]); // header checksum
        pkt.extend_from_slice(payload);
        pkt
    }

    #[test]
    fn test_oracle_data_packet_detected() {
        let ext = OracleQueryExtractor::new();
        // DATA packet with data flags + some payload
        let pkt = build_tns_packet(tns_packet::DATA, &[0x00, 0x00, 0x01, 0x02, 0x03]);
        let info = ext.extract_query(&pkt).unwrap();
        assert_eq!(info.command_type, SqlCommandType::Unknown);
        assert!(info.query_text.is_none());
        assert!(!info.is_prepared);
    }

    #[test]
    fn test_oracle_connect_returns_none() {
        let ext = OracleQueryExtractor::new();
        let pkt = build_tns_packet(tns_packet::CONNECT, &[0; 20]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_oracle_accept_returns_none() {
        let ext = OracleQueryExtractor::new();
        let pkt = build_tns_packet(tns_packet::ACCEPT, &[0; 10]);
        assert!(ext.extract_query(&pkt).is_none());
    }

    #[test]
    fn test_oracle_too_short_returns_none() {
        let ext = OracleQueryExtractor::new();
        assert!(ext.extract_query(&[0x00, 0x08, 0x00, 0x00, 0x06]).is_none());
        assert!(ext.extract_query(&[]).is_none());
    }

    #[test]
    fn test_oracle_response_data_packet() {
        let ext = OracleQueryExtractor::new();
        let pkt = build_tns_packet(tns_packet::DATA, &[0x00, 0x00, 0x08, 0x00, 0x00]);
        let resp = ext.extract_response(&pkt).unwrap();
        assert!(resp.success);
        assert!(resp.rows_affected.is_none());
    }

    #[test]
    fn test_oracle_response_non_data_returns_none() {
        let ext = OracleQueryExtractor::new();
        let pkt = build_tns_packet(tns_packet::MARKER, &[0x00, 0x00, 0x01]);
        assert!(ext.extract_response(&pkt).is_none());
    }

    #[test]
    fn test_oracle_protocol_name() {
        let ext = OracleQueryExtractor::new();
        assert_eq!(ext.protocol_name(), "oracle");
    }
}
