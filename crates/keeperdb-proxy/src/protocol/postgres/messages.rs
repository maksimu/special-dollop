//! PostgreSQL protocol message structures
//!
//! This module defines the data structures for PostgreSQL wire protocol messages.
//! Reference: <https://www.postgresql.org/docs/current/protocol-message-formats.html>

use std::collections::HashMap;

// ============================================================================
// Startup Messages (no type byte)
// ============================================================================

/// PostgreSQL startup message sent by client to initiate connection.
///
/// Format: Length (4) + Protocol Version (4) + Parameters (null-terminated pairs) + \0
#[derive(Debug, Clone)]
pub struct StartupMessage {
    /// Protocol version (should be 196608 for v3.0)
    pub protocol_version: u32,
    /// Connection parameters (user, database, options, etc.)
    pub parameters: HashMap<String, String>,
}

impl StartupMessage {
    /// Create a new startup message with the given user.
    pub fn new(user: &str) -> Self {
        let mut parameters = HashMap::new();
        parameters.insert("user".to_string(), user.to_string());
        Self {
            protocol_version: super::constants::PROTOCOL_VERSION_3_0,
            parameters,
        }
    }

    /// Create a startup message with user and database.
    pub fn with_database(user: &str, database: &str) -> Self {
        let mut msg = Self::new(user);
        msg.parameters
            .insert("database".to_string(), database.to_string());
        msg
    }

    /// Get the username from parameters.
    pub fn user(&self) -> Option<&str> {
        self.parameters.get("user").map(|s| s.as_str())
    }

    /// Get the database name from parameters.
    pub fn database(&self) -> Option<&str> {
        self.parameters.get("database").map(|s| s.as_str())
    }

    /// Set a parameter.
    pub fn set_parameter(&mut self, key: &str, value: &str) {
        self.parameters.insert(key.to_string(), value.to_string());
    }

    /// Get the application_name from parameters.
    ///
    /// This identifies the client application (e.g., "pgAdmin 4", "DBeaver", "psql").
    pub fn application_name(&self) -> Option<&str> {
        self.parameters.get("application_name").map(|s| s.as_str())
    }

    /// Generate a client fingerprint from all startup parameters.
    ///
    /// # Security Limitations
    ///
    /// **PostgreSQL fingerprinting is inherently weaker than MySQL.**
    ///
    /// MySQL clients send rich `connect_attrs` containing:
    /// - `program_name` / `_client_name`: Application identifier
    /// - `_client_version`: Version number
    /// - `_platform` / `_os`: Operating system
    /// - `_runtime_vendor`: Java vendor for JDBC clients
    ///
    /// PostgreSQL's startup message contains only session parameters, not client
    /// identification. The only reliable identifier is `application_name`, which:
    /// - Is optional (many clients don't set it, e.g., JetBrains DataGrip)
    /// - Can be easily spoofed by malicious clients
    /// - May be identical across different client installations
    ///
    /// # Fallback Strategy
    ///
    /// When `application_name` is not set, we attempt to build a fingerprint from:
    /// - `client_encoding`: Character encoding (e.g., "UTF8")
    /// - `options`: Command-line options
    /// - `DateStyle`: Date format preference
    /// - `TimeZone`: Client timezone
    /// - `extra_float_digits`: Float precision setting
    ///
    /// **Warning:** These parameters are not designed for client identification and
    /// may produce identical fingerprints for different tools. This is a known
    /// limitation of the PostgreSQL protocol.
    ///
    /// # Recommendations
    ///
    /// For stronger security, consider:
    /// 1. Requiring clients to set `application_name` (may break some tools)
    /// 2. Using network-level controls (IP allowlisting, VPN)
    /// 3. Treating the application lock as defense-in-depth, not primary security
    ///
    /// # Returns
    ///
    /// - `Some(application_name)` if set and non-empty
    /// - `Some(fingerprint)` built from other parameters if available
    /// - `None` if no identifying parameters are present
    pub fn client_fingerprint(&self) -> Option<String> {
        // If application_name is set, use it as the primary identifier
        if let Some(app_name) = self.application_name() {
            if !app_name.is_empty() {
                return Some(app_name.to_string());
            }
        }

        // Build a fingerprint from other available parameters
        let mut parts: Vec<String> = Vec::new();

        // Add client_encoding if present (often set differently by different tools)
        if let Some(encoding) = self.parameters.get("client_encoding") {
            parts.push(format!("enc:{}", encoding));
        }

        // Add options if present (sometimes contains client-specific flags)
        if let Some(options) = self.parameters.get("options") {
            if !options.is_empty() {
                parts.push(format!("opts:{}", options));
            }
        }

        // Add DateStyle if present (some clients set this specifically)
        if let Some(style) = self.parameters.get("DateStyle") {
            parts.push(format!("date:{}", style));
        }

        // Add TimeZone if present (varies by client)
        if let Some(tz) = self.parameters.get("TimeZone") {
            parts.push(format!("tz:{}", tz));
        }

        // Add extra_float_digits if present (varies by client)
        if let Some(extra) = self.parameters.get("extra_float_digits") {
            parts.push(format!("fdig:{}", extra));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(":"))
        }
    }

    /// Get all parameters for debugging/logging.
    pub fn all_parameters(&self) -> &HashMap<String, String> {
        &self.parameters
    }
}

/// SSL request message - requests SSL upgrade before startup.
///
/// Format: Length (4, always 8) + SSL Code (4, always 80877103)
#[derive(Debug, Clone, Copy)]
pub struct SSLRequest;

impl SSLRequest {
    /// The fixed length of an SSL request (8 bytes)
    pub const LENGTH: u32 = 8;

    /// The SSL request code
    pub const CODE: u32 = super::constants::SSL_REQUEST_CODE;
}

/// Cancel request message - requests cancellation of a running query.
///
/// Format: Length (4, always 16) + Cancel Code (4) + Process ID (4) + Secret Key (4)
#[derive(Debug, Clone, Copy)]
pub struct CancelRequest {
    /// Backend process ID
    pub process_id: u32,
    /// Secret key for this connection
    pub secret_key: u32,
}

// ============================================================================
// Authentication Messages
// ============================================================================

/// Authentication request/response from server.
///
/// All authentication messages have type byte 'R'.
#[derive(Debug, Clone)]
pub enum AuthenticationMessage {
    /// Authentication successful (type 0)
    Ok,

    /// Kerberos V5 required (type 2, deprecated)
    KerberosV5,

    /// Cleartext password required (type 3)
    CleartextPassword,

    /// MD5 password required (type 5)
    /// Contains 4-byte salt for MD5 computation
    Md5Password {
        /// 4-byte salt for MD5 hash
        salt: [u8; 4],
    },

    /// SCM credentials required (type 6, Unix only)
    ScmCredential,

    /// GSSAPI authentication required (type 7)
    Gss,

    /// GSSAPI continuation (type 8)
    GssContinue {
        /// GSSAPI data
        data: Vec<u8>,
    },

    /// SSPI authentication required (type 9)
    Sspi,

    /// SASL authentication required (type 10)
    /// Lists available SASL mechanisms
    Sasl {
        /// Available SASL mechanism names
        mechanisms: Vec<String>,
    },

    /// SASL continuation (type 11)
    /// Contains server challenge data
    SaslContinue {
        /// Server challenge/response data
        data: Vec<u8>,
    },

    /// SASL final (type 12)
    /// Contains server signature for verification
    SaslFinal {
        /// Server signature data
        data: Vec<u8>,
    },
}

impl AuthenticationMessage {
    /// Check if this is a successful authentication.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    /// Check if this requires password.
    pub fn requires_password(&self) -> bool {
        matches!(
            self,
            Self::CleartextPassword | Self::Md5Password { .. } | Self::Sasl { .. }
        )
    }
}

// ============================================================================
// Password/SASL Response Messages
// ============================================================================

/// Password message sent by client (type 'p').
///
/// Used for cleartext password, MD5 password, and SASL responses.
#[derive(Debug, Clone)]
pub struct PasswordMessage {
    /// Password or authentication data (null-terminated for password)
    pub data: Vec<u8>,
}

impl PasswordMessage {
    /// Create a password message from a string (adds null terminator).
    pub fn from_password(password: &str) -> Self {
        let mut data = password.as_bytes().to_vec();
        data.push(0);
        Self { data }
    }

    /// Create a password message from raw bytes (for SASL).
    pub fn from_bytes(data: Vec<u8>) -> Self {
        Self { data }
    }
}

/// SASL initial response message (sent via 'p' message).
#[derive(Debug, Clone)]
pub struct SASLInitialResponse {
    /// Selected SASL mechanism name
    pub mechanism: String,
    /// Client-first-message data
    pub data: Vec<u8>,
}

/// SASL response message (sent via 'p' message).
#[derive(Debug, Clone)]
pub struct SASLResponse {
    /// SASL response data (client-final-message)
    pub data: Vec<u8>,
}

// ============================================================================
// Server Information Messages
// ============================================================================

/// Backend key data message (type 'K').
///
/// Provides process ID and secret key for cancel requests.
#[derive(Debug, Clone, Copy)]
pub struct BackendKeyData {
    /// Backend process ID
    pub process_id: u32,
    /// Secret key for cancellation
    pub secret_key: u32,
}

/// Parameter status message (type 'S').
///
/// Reports a server configuration parameter value.
#[derive(Debug, Clone)]
pub struct ParameterStatus {
    /// Parameter name
    pub name: String,
    /// Parameter value
    pub value: String,
}

/// Ready for query message (type 'Z').
///
/// Indicates server is ready for a new query cycle.
#[derive(Debug, Clone, Copy)]
pub struct ReadyForQuery {
    /// Current transaction status
    pub transaction_status: TransactionStatus,
}

/// Transaction status indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    /// Idle (not in transaction)
    Idle,
    /// In a transaction block
    InTransaction,
    /// In a failed transaction block
    Failed,
}

impl TransactionStatus {
    /// Create from the status byte.
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'I' => Some(Self::Idle),
            b'T' => Some(Self::InTransaction),
            b'E' => Some(Self::Failed),
            _ => None,
        }
    }

    /// Convert to status byte.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Idle => b'I',
            Self::InTransaction => b'T',
            Self::Failed => b'E',
        }
    }
}

// ============================================================================
// Error/Notice Messages
// ============================================================================

/// Error or notice response message (types 'E' and 'N').
///
/// Contains a collection of fields describing the error/notice.
#[derive(Debug, Clone, Default)]
pub struct ErrorNoticeResponse {
    /// Error/notice fields keyed by field type byte
    pub fields: HashMap<u8, String>,
}

impl ErrorNoticeResponse {
    /// Create a new empty error/notice response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an error response with the basic fields.
    pub fn error(severity: &str, code: &str, message: &str) -> Self {
        let mut resp = Self::new();
        resp.set_severity(severity);
        resp.set_code(code);
        resp.set_message(message);
        resp
    }

    /// Create a FATAL error response.
    pub fn fatal(code: &str, message: &str) -> Self {
        Self::error("FATAL", code, message)
    }

    /// Get the severity (S field).
    pub fn severity(&self) -> Option<&str> {
        self.fields
            .get(&super::constants::ERROR_FIELD_SEVERITY)
            .map(|s| s.as_str())
    }

    /// Set the severity.
    pub fn set_severity(&mut self, severity: &str) {
        self.fields
            .insert(super::constants::ERROR_FIELD_SEVERITY, severity.to_string());
    }

    /// Get the SQLSTATE code (C field).
    pub fn code(&self) -> Option<&str> {
        self.fields
            .get(&super::constants::ERROR_FIELD_CODE)
            .map(|s| s.as_str())
    }

    /// Set the SQLSTATE code.
    pub fn set_code(&mut self, code: &str) {
        self.fields
            .insert(super::constants::ERROR_FIELD_CODE, code.to_string());
    }

    /// Get the message (M field).
    pub fn message(&self) -> Option<&str> {
        self.fields
            .get(&super::constants::ERROR_FIELD_MESSAGE)
            .map(|s| s.as_str())
    }

    /// Set the message.
    pub fn set_message(&mut self, message: &str) {
        self.fields
            .insert(super::constants::ERROR_FIELD_MESSAGE, message.to_string());
    }

    /// Get the detail (D field).
    pub fn detail(&self) -> Option<&str> {
        self.fields
            .get(&super::constants::ERROR_FIELD_DETAIL)
            .map(|s| s.as_str())
    }

    /// Set the detail.
    pub fn set_detail(&mut self, detail: &str) {
        self.fields
            .insert(super::constants::ERROR_FIELD_DETAIL, detail.to_string());
    }

    /// Get the hint (H field).
    pub fn hint(&self) -> Option<&str> {
        self.fields
            .get(&super::constants::ERROR_FIELD_HINT)
            .map(|s| s.as_str())
    }

    /// Set the hint.
    pub fn set_hint(&mut self, hint: &str) {
        self.fields
            .insert(super::constants::ERROR_FIELD_HINT, hint.to_string());
    }

    /// Check if this is a FATAL error.
    pub fn is_fatal(&self) -> bool {
        self.severity() == Some("FATAL")
    }

    /// Check if this is an ERROR.
    pub fn is_error(&self) -> bool {
        matches!(
            self.severity(),
            Some("ERROR") | Some("FATAL") | Some("PANIC")
        )
    }

    /// Set a field by type byte.
    pub fn set_field(&mut self, field_type: u8, value: &str) {
        self.fields.insert(field_type, value.to_string());
    }

    /// Get a field by type byte.
    pub fn get_field(&self, field_type: u8) -> Option<&str> {
        self.fields.get(&field_type).map(|s| s.as_str())
    }
}

// ============================================================================
// Query Messages
// ============================================================================

/// Simple query message (type 'Q').
#[derive(Debug, Clone)]
pub struct Query {
    /// SQL query string
    pub query: String,
}

impl Query {
    /// Create a new query message.
    pub fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
        }
    }
}

/// Command complete message (type 'C').
#[derive(Debug, Clone)]
pub struct CommandComplete {
    /// Command tag (e.g., "SELECT 5", "INSERT 0 1")
    pub tag: String,
}

/// Row description message (type 'T').
#[derive(Debug, Clone)]
pub struct RowDescription {
    /// Field descriptions
    pub fields: Vec<FieldDescription>,
}

/// Description of a single field/column.
#[derive(Debug, Clone)]
pub struct FieldDescription {
    /// Column name
    pub name: String,
    /// Table OID (0 if not a table column)
    pub table_oid: u32,
    /// Column attribute number (0 if not a table column)
    pub column_id: u16,
    /// Data type OID
    pub type_oid: u32,
    /// Data type size (-1 for variable)
    pub type_size: i16,
    /// Type modifier
    pub type_modifier: i32,
    /// Format code (0 = text, 1 = binary)
    pub format: i16,
}

/// Data row message (type 'D').
#[derive(Debug, Clone)]
pub struct DataRow {
    /// Column values (None = NULL)
    pub values: Vec<Option<Vec<u8>>>,
}

// ============================================================================
// Common Error Responses
// ============================================================================

impl ErrorNoticeResponse {
    /// Authentication failed error.
    pub fn authentication_failed(user: &str) -> Self {
        Self::fatal(
            super::constants::SQLSTATE_INVALID_PASSWORD,
            &format!("password authentication failed for user \"{}\"", user),
        )
    }

    /// Connection failure error.
    pub fn connection_failed(host: &str, port: u16) -> Self {
        Self::fatal(
            super::constants::SQLSTATE_CONNECTION_FAILURE,
            &format!("could not connect to server at \"{}\" port {}", host, port),
        )
    }

    /// Too many connections error.
    pub fn too_many_connections() -> Self {
        Self::fatal(
            super::constants::SQLSTATE_TOO_MANY_CONNECTIONS,
            "too many connections for this session",
        )
    }

    /// Session timeout error.
    pub fn session_timeout() -> Self {
        Self::fatal(
            super::constants::SQLSTATE_ADMIN_SHUTDOWN,
            "session timeout: maximum session duration exceeded",
        )
    }

    /// Query limit exceeded error.
    pub fn query_limit_exceeded() -> Self {
        Self::fatal(
            super::constants::SQLSTATE_QUERY_CANCELED,
            "query limit exceeded: maximum queries per session reached",
        )
    }

    /// Internal proxy error.
    pub fn internal_error(message: &str) -> Self {
        Self::fatal(super::constants::SQLSTATE_INTERNAL_ERROR, message)
    }

    /// Protocol error.
    pub fn protocol_error(message: &str) -> Self {
        Self::fatal(super::constants::SQLSTATE_PROTOCOL_VIOLATION, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_message_new() {
        let msg = StartupMessage::new("testuser");
        assert_eq!(msg.user(), Some("testuser"));
        assert_eq!(msg.database(), None);
        assert_eq!(msg.protocol_version, 196608);
    }

    #[test]
    fn test_startup_message_with_database() {
        let msg = StartupMessage::with_database("testuser", "testdb");
        assert_eq!(msg.user(), Some("testuser"));
        assert_eq!(msg.database(), Some("testdb"));
    }

    #[test]
    fn test_transaction_status() {
        assert_eq!(
            TransactionStatus::from_byte(b'I'),
            Some(TransactionStatus::Idle)
        );
        assert_eq!(
            TransactionStatus::from_byte(b'T'),
            Some(TransactionStatus::InTransaction)
        );
        assert_eq!(
            TransactionStatus::from_byte(b'E'),
            Some(TransactionStatus::Failed)
        );
        assert_eq!(TransactionStatus::from_byte(b'X'), None);

        assert_eq!(TransactionStatus::Idle.to_byte(), b'I');
        assert_eq!(TransactionStatus::InTransaction.to_byte(), b'T');
        assert_eq!(TransactionStatus::Failed.to_byte(), b'E');
    }

    #[test]
    fn test_error_response_basic() {
        let err = ErrorNoticeResponse::error("ERROR", "42000", "syntax error");
        assert_eq!(err.severity(), Some("ERROR"));
        assert_eq!(err.code(), Some("42000"));
        assert_eq!(err.message(), Some("syntax error"));
        assert!(err.is_error());
        assert!(!err.is_fatal());
    }

    #[test]
    fn test_error_response_fatal() {
        let err = ErrorNoticeResponse::fatal("28P01", "authentication failed");
        assert_eq!(err.severity(), Some("FATAL"));
        assert!(err.is_fatal());
        assert!(err.is_error());
    }

    #[test]
    fn test_error_response_common_errors() {
        let auth_err = ErrorNoticeResponse::authentication_failed("bob");
        assert!(auth_err.message().unwrap().contains("bob"));

        let conn_err = ErrorNoticeResponse::connection_failed("localhost", 5432);
        assert!(conn_err.message().unwrap().contains("localhost"));

        let limit_err = ErrorNoticeResponse::too_many_connections();
        assert!(limit_err.is_fatal());
    }

    #[test]
    fn test_authentication_message_checks() {
        assert!(AuthenticationMessage::Ok.is_ok());
        assert!(!AuthenticationMessage::CleartextPassword.is_ok());

        assert!(AuthenticationMessage::CleartextPassword.requires_password());
        assert!(AuthenticationMessage::Md5Password { salt: [0; 4] }.requires_password());
        assert!(AuthenticationMessage::Sasl { mechanisms: vec![] }.requires_password());
        assert!(!AuthenticationMessage::Ok.requires_password());
    }

    #[test]
    fn test_password_message() {
        let pwd = PasswordMessage::from_password("secret");
        assert_eq!(pwd.data, b"secret\0");

        let raw = PasswordMessage::from_bytes(vec![1, 2, 3]);
        assert_eq!(raw.data, vec![1, 2, 3]);
    }

    #[test]
    fn test_query_message() {
        let q = Query::new("SELECT 1");
        assert_eq!(q.query, "SELECT 1");
    }
}
