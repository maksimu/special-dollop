//! PostgreSQL protocol constants
//!
//! This module defines constants for the PostgreSQL wire protocol version 3.0.
//! Reference: <https://www.postgresql.org/docs/current/protocol.html>

// ============================================================================
// Protocol Version
// ============================================================================

/// PostgreSQL protocol version 3.0 (major=3, minor=0)
/// Encoded as (major << 16) | minor = 196608
pub const PROTOCOL_VERSION_3_0: u32 = 196608;

// ============================================================================
// Special Request Codes (used in startup-like messages)
// ============================================================================

/// SSL request code - sent instead of StartupMessage to request SSL
/// Value: 80877103 (0x04D2162F)
pub const SSL_REQUEST_CODE: u32 = 80877103;

/// Cancel request code - sent to cancel a running query
/// Value: 80877102 (0x04D2162E)
pub const CANCEL_REQUEST_CODE: u32 = 80877102;

// ============================================================================
// Frontend Message Types (client -> server)
// ============================================================================

/// Password message ('p') - sends password or SASL response
pub const MSG_PASSWORD: u8 = b'p';

/// Simple query ('Q') - executes a SQL query string
pub const MSG_QUERY: u8 = b'Q';

/// Terminate ('X') - client requests connection close
pub const MSG_TERMINATE: u8 = b'X';

/// Parse ('P') - prepare a statement (extended query protocol)
pub const MSG_PARSE: u8 = b'P';

/// Bind ('B') - bind parameters to prepared statement
pub const MSG_BIND: u8 = b'B';

/// Execute ('E') - execute a prepared statement
pub const MSG_EXECUTE: u8 = b'E';

/// Describe ('D') - request description of statement or portal
pub const MSG_DESCRIBE: u8 = b'D';

/// Sync ('S') - sync point in extended query protocol
pub const MSG_SYNC: u8 = b'S';

/// Flush ('H') - flush output buffer
pub const MSG_FLUSH: u8 = b'H';

/// Close ('C') - close a prepared statement or portal
pub const MSG_CLOSE: u8 = b'C';

/// Copy data ('d') - data for COPY operation
pub const MSG_COPY_DATA: u8 = b'd';

/// Copy done ('c') - COPY operation complete
pub const MSG_COPY_DONE: u8 = b'c';

/// Copy fail ('f') - COPY operation failed
pub const MSG_COPY_FAIL: u8 = b'f';

// ============================================================================
// Backend Message Types (server -> client)
// ============================================================================

/// Authentication request ('R') - various auth-related messages
pub const MSG_AUTH_REQUEST: u8 = b'R';

/// Backend key data ('K') - process ID and secret key for cancellation
pub const MSG_BACKEND_KEY_DATA: u8 = b'K';

/// Parameter status ('S') - server configuration parameter
pub const MSG_PARAMETER_STATUS: u8 = b'S';

/// Ready for query ('Z') - server is ready for a new query
pub const MSG_READY_FOR_QUERY: u8 = b'Z';

/// Row description ('T') - describes columns in query result
pub const MSG_ROW_DESCRIPTION: u8 = b'T';

/// Data row ('D') - a row of query result data
pub const MSG_DATA_ROW: u8 = b'D';

/// Command complete ('C') - query execution complete
pub const MSG_COMMAND_COMPLETE: u8 = b'C';

/// Empty query response ('I') - query string was empty
pub const MSG_EMPTY_QUERY: u8 = b'I';

/// Error response ('E') - error occurred
pub const MSG_ERROR_RESPONSE: u8 = b'E';

/// Notice response ('N') - warning or informational message
pub const MSG_NOTICE_RESPONSE: u8 = b'N';

/// Parse complete ('1') - Parse command succeeded
pub const MSG_PARSE_COMPLETE: u8 = b'1';

/// Bind complete ('2') - Bind command succeeded
pub const MSG_BIND_COMPLETE: u8 = b'2';

/// Close complete ('3') - Close command succeeded
pub const MSG_CLOSE_COMPLETE: u8 = b'3';

/// No data ('n') - no data available
pub const MSG_NO_DATA: u8 = b'n';

/// Portal suspended ('s') - execution suspended
pub const MSG_PORTAL_SUSPENDED: u8 = b's';

/// Parameter description ('t') - describes parameters
pub const MSG_PARAMETER_DESCRIPTION: u8 = b't';

/// Notification response ('A') - async notification
pub const MSG_NOTIFICATION: u8 = b'A';

/// Copy in response ('G') - server ready for COPY data
pub const MSG_COPY_IN_RESPONSE: u8 = b'G';

/// Copy out response ('H') - server sending COPY data
pub const MSG_COPY_OUT_RESPONSE: u8 = b'H';

/// Copy both response ('W') - bidirectional COPY
pub const MSG_COPY_BOTH_RESPONSE: u8 = b'W';

// ============================================================================
// Authentication Types (subtypes of 'R' message)
// ============================================================================

/// Authentication OK - authentication successful
pub const AUTH_OK: u32 = 0;

/// Kerberos V5 authentication required (deprecated)
pub const AUTH_KERBEROS_V5: u32 = 2;

/// Cleartext password required
pub const AUTH_CLEARTEXT_PASSWORD: u32 = 3;

/// MD5 password required (includes 4-byte salt)
pub const AUTH_MD5_PASSWORD: u32 = 5;

/// SCM credentials required (Unix only)
pub const AUTH_SCM_CREDENTIAL: u32 = 6;

/// GSSAPI authentication required
pub const AUTH_GSS: u32 = 7;

/// GSSAPI continuation data
pub const AUTH_GSS_CONTINUE: u32 = 8;

/// SSPI authentication required
pub const AUTH_SSPI: u32 = 9;

/// SASL authentication required (lists mechanisms)
pub const AUTH_SASL: u32 = 10;

/// SASL continuation (server challenge/response)
pub const AUTH_SASL_CONTINUE: u32 = 11;

/// SASL final (server signature)
pub const AUTH_SASL_FINAL: u32 = 12;

// ============================================================================
// Error/Notice Field Types
// ============================================================================

/// Severity (localized) - ERROR, FATAL, PANIC, WARNING, NOTICE, DEBUG, INFO, LOG
pub const ERROR_FIELD_SEVERITY: u8 = b'S';

/// Severity (non-localized) - same values, but always in English
pub const ERROR_FIELD_SEVERITY_V: u8 = b'V';

/// SQLSTATE code - 5-character error code
pub const ERROR_FIELD_CODE: u8 = b'C';

/// Message - primary human-readable error message
pub const ERROR_FIELD_MESSAGE: u8 = b'M';

/// Detail - optional secondary error message
pub const ERROR_FIELD_DETAIL: u8 = b'D';

/// Hint - optional suggestion for fixing the problem
pub const ERROR_FIELD_HINT: u8 = b'H';

/// Position - cursor position (1-based) in query string
pub const ERROR_FIELD_POSITION: u8 = b'P';

/// Internal position - cursor position in internally-generated query
pub const ERROR_FIELD_INTERNAL_POSITION: u8 = b'p';

/// Internal query - the internally-generated query
pub const ERROR_FIELD_INTERNAL_QUERY: u8 = b'q';

/// Where - context in which the error occurred
pub const ERROR_FIELD_WHERE: u8 = b'W';

/// Schema name - if applicable
pub const ERROR_FIELD_SCHEMA: u8 = b's';

/// Table name - if applicable
pub const ERROR_FIELD_TABLE: u8 = b't';

/// Column name - if applicable
pub const ERROR_FIELD_COLUMN: u8 = b'c';

/// Data type name - if applicable
pub const ERROR_FIELD_DATATYPE: u8 = b'd';

/// Constraint name - if applicable
pub const ERROR_FIELD_CONSTRAINT: u8 = b'n';

/// File - source file where error was reported
pub const ERROR_FIELD_FILE: u8 = b'F';

/// Line - source line number
pub const ERROR_FIELD_LINE: u8 = b'L';

/// Routine - source function name
pub const ERROR_FIELD_ROUTINE: u8 = b'R';

// ============================================================================
// Transaction Status (in ReadyForQuery message)
// ============================================================================

/// Idle - not in a transaction block
pub const TXN_STATUS_IDLE: u8 = b'I';

/// In transaction - inside a transaction block
pub const TXN_STATUS_IN_TRANSACTION: u8 = b'T';

/// Failed transaction - inside a failed transaction block
pub const TXN_STATUS_FAILED: u8 = b'E';

// ============================================================================
// SASL Mechanism Names
// ============================================================================

/// SCRAM-SHA-256 mechanism name
pub const SASL_MECHANISM_SCRAM_SHA_256: &str = "SCRAM-SHA-256";

/// SCRAM-SHA-256-PLUS mechanism name (with channel binding)
pub const SASL_MECHANISM_SCRAM_SHA_256_PLUS: &str = "SCRAM-SHA-256-PLUS";

// ============================================================================
// Common SQLSTATE Codes (for proxy-generated errors)
// ============================================================================

/// Successful completion
pub const SQLSTATE_SUCCESSFUL: &str = "00000";

/// Invalid password
pub const SQLSTATE_INVALID_PASSWORD: &str = "28P01";

/// Invalid authorization specification
pub const SQLSTATE_INVALID_AUTHORIZATION: &str = "28000";

/// Connection failure
pub const SQLSTATE_CONNECTION_FAILURE: &str = "08006";

/// Connection does not exist
pub const SQLSTATE_CONNECTION_DOES_NOT_EXIST: &str = "08003";

/// Protocol violation
pub const SQLSTATE_PROTOCOL_VIOLATION: &str = "08P01";

/// Internal error
pub const SQLSTATE_INTERNAL_ERROR: &str = "XX000";

/// Admin shutdown
pub const SQLSTATE_ADMIN_SHUTDOWN: &str = "57P01";

/// Query canceled
pub const SQLSTATE_QUERY_CANCELED: &str = "57014";

/// Too many connections
pub const SQLSTATE_TOO_MANY_CONNECTIONS: &str = "53300";

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if a message type is a query command that should be counted
/// for session query limits.
#[inline]
pub fn is_query_command(msg_type: u8) -> bool {
    matches!(msg_type, MSG_QUERY | MSG_EXECUTE | MSG_PARSE)
}

/// Check if a message type is a termination command.
#[inline]
pub fn is_terminate_command(msg_type: u8) -> bool {
    msg_type == MSG_TERMINATE
}

/// Get a human-readable name for a backend (server->client) message type.
///
/// Note: Some message type bytes are shared between frontend and backend.
/// This function returns the backend interpretation.
pub fn backend_message_name(msg_type: u8) -> &'static str {
    match msg_type {
        MSG_AUTH_REQUEST => "Authentication",
        MSG_BACKEND_KEY_DATA => "BackendKeyData",
        MSG_PARAMETER_STATUS => "ParameterStatus",
        MSG_READY_FOR_QUERY => "ReadyForQuery",
        MSG_ROW_DESCRIPTION => "RowDescription",
        MSG_DATA_ROW => "DataRow",
        MSG_COMMAND_COMPLETE => "CommandComplete",
        MSG_EMPTY_QUERY => "EmptyQueryResponse",
        MSG_ERROR_RESPONSE => "ErrorResponse",
        MSG_NOTICE_RESPONSE => "NoticeResponse",
        MSG_PARSE_COMPLETE => "ParseComplete",
        MSG_BIND_COMPLETE => "BindComplete",
        MSG_CLOSE_COMPLETE => "CloseComplete",
        MSG_NO_DATA => "NoData",
        MSG_PORTAL_SUSPENDED => "PortalSuspended",
        MSG_PARAMETER_DESCRIPTION => "ParameterDescription",
        MSG_NOTIFICATION => "NotificationResponse",
        MSG_COPY_IN_RESPONSE => "CopyInResponse",
        MSG_COPY_OUT_RESPONSE => "CopyOutResponse",
        MSG_COPY_BOTH_RESPONSE => "CopyBothResponse",
        _ => "Unknown",
    }
}

/// Get a human-readable name for a frontend (client->server) message type.
///
/// Note: Some message type bytes are shared between frontend and backend.
/// This function returns the frontend interpretation.
pub fn frontend_message_name(msg_type: u8) -> &'static str {
    match msg_type {
        MSG_PASSWORD => "Password",
        MSG_QUERY => "Query",
        MSG_TERMINATE => "Terminate",
        MSG_PARSE => "Parse",
        MSG_BIND => "Bind",
        MSG_EXECUTE => "Execute",
        MSG_DESCRIBE => "Describe",
        MSG_SYNC => "Sync",
        MSG_FLUSH => "Flush",
        MSG_CLOSE => "Close",
        MSG_COPY_DATA => "CopyData",
        MSG_COPY_DONE => "CopyDone",
        MSG_COPY_FAIL => "CopyFail",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_version() {
        // Protocol version 3.0 should be (3 << 16) | 0 = 196608
        assert_eq!(PROTOCOL_VERSION_3_0, 196608);
        assert_eq!(PROTOCOL_VERSION_3_0, 3 << 16);
    }

    #[test]
    fn test_ssl_request_code() {
        // SSL request code is a magic number
        assert_eq!(SSL_REQUEST_CODE, 80877103);
        assert_eq!(SSL_REQUEST_CODE, 0x04D2162F);
    }

    #[test]
    fn test_cancel_request_code() {
        assert_eq!(CANCEL_REQUEST_CODE, 80877102);
        assert_eq!(CANCEL_REQUEST_CODE, 0x04D2162E);
    }

    #[test]
    fn test_is_query_command() {
        assert!(is_query_command(MSG_QUERY));
        assert!(is_query_command(MSG_EXECUTE));
        assert!(is_query_command(MSG_PARSE));
        assert!(!is_query_command(MSG_TERMINATE));
        assert!(!is_query_command(MSG_SYNC));
    }

    #[test]
    fn test_is_terminate_command() {
        assert!(is_terminate_command(MSG_TERMINATE));
        assert!(!is_terminate_command(MSG_QUERY));
    }

    #[test]
    fn test_backend_message_names() {
        assert_eq!(backend_message_name(MSG_AUTH_REQUEST), "Authentication");
        assert_eq!(backend_message_name(MSG_ERROR_RESPONSE), "ErrorResponse");
        assert_eq!(backend_message_name(MSG_READY_FOR_QUERY), "ReadyForQuery");
        assert_eq!(backend_message_name(0xFF), "Unknown");
    }

    #[test]
    fn test_frontend_message_names() {
        assert_eq!(frontend_message_name(MSG_QUERY), "Query");
        assert_eq!(frontend_message_name(MSG_TERMINATE), "Terminate");
        assert_eq!(frontend_message_name(MSG_PASSWORD), "Password");
        assert_eq!(frontend_message_name(0xFF), "Unknown");
    }

    #[test]
    fn test_auth_types() {
        assert_eq!(AUTH_OK, 0);
        assert_eq!(AUTH_MD5_PASSWORD, 5);
        assert_eq!(AUTH_SASL, 10);
        assert_eq!(AUTH_SASL_CONTINUE, 11);
        assert_eq!(AUTH_SASL_FINAL, 12);
    }

    #[test]
    fn test_transaction_status() {
        assert_eq!(TXN_STATUS_IDLE, b'I');
        assert_eq!(TXN_STATUS_IN_TRANSACTION, b'T');
        assert_eq!(TXN_STATUS_FAILED, b'E');
    }
}
