// Shared security settings for all protocol handlers
//
// Implements security features that must be consistent across SSH, Telnet, RDP, VNC,
// Database, SFTP, and RBI handlers. Based on KCM's kcm-libguac-client-db security model.
//
// Reference: ~/Documents/kcm/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/db/settings.c

use std::collections::HashMap;

/// Unified security settings for all protocol handlers
///
/// These settings control access restrictions and must be enforced by every handler.
/// Parsed from connection parameters sent by the Guacamole client.
#[derive(Debug, Clone)]
pub struct HandlerSecuritySettings {
    // ========================================================================
    // Read-Only Mode
    // ========================================================================
    /// Prevent any user input that could modify the remote system
    ///
    /// Behavior by protocol:
    /// - SSH/Telnet: Block all keyboard input except Ctrl+C
    /// - RDP/VNC: Block keyboard and mouse clicks (allow mouse move for viewing)
    /// - Database: Block INSERT/UPDATE/DELETE/DROP/ALTER/TRUNCATE/CREATE queries
    /// - SFTP: Block upload/delete/rename/mkdir operations
    /// - RBI: Block all input
    pub read_only: bool,

    // ========================================================================
    // Clipboard Controls
    // ========================================================================
    /// Disable copying data FROM the remote session TO the client
    /// Prevents data exfiltration
    pub disable_copy: bool,

    /// Disable pasting data FROM the client TO the remote session
    /// Prevents injection of malicious content
    pub disable_paste: bool,

    /// Maximum clipboard buffer size in bytes
    /// Default: 1MB (1048576), Max: 50MB, Min: 256KB
    /// Based on KCM's GUAC_COMMON_CLIPBOARD_MIN/MAX_LENGTH
    pub clipboard_buffer_size: usize,

    // ========================================================================
    // Connection Timeouts & Limits
    // ========================================================================
    /// Connection timeout in seconds (0 = no timeout)
    /// How long to wait for initial connection
    pub connection_timeout_secs: u64,

    /// Idle timeout in seconds (0 = no timeout)
    /// Disconnect if no user activity for this duration
    pub idle_timeout_secs: u64,

    /// Maximum session duration in seconds (0 = no limit)
    /// Force disconnect after this duration regardless of activity
    pub max_session_duration_secs: u64,

    // ========================================================================
    // Wake-on-LAN (WoL)
    // ========================================================================
    /// Send WoL magic packet before connecting
    pub wol_send_packet: bool,

    /// MAC address of target system (required if wol_send_packet is true)
    pub wol_mac_addr: Option<String>,

    /// Broadcast address for WoL packet
    /// Default: "255.255.255.255"
    pub wol_broadcast_addr: String,

    /// UDP port for WoL packet
    /// Default: 9
    pub wol_udp_port: u16,

    /// Seconds to wait after WoL before attempting connection
    /// Default: 0
    pub wol_wait_time: u32,
}

// Clipboard buffer size limits (from KCM patches)
pub const CLIPBOARD_MIN_SIZE: usize = 256 * 1024; // 256KB
pub const CLIPBOARD_MAX_SIZE: usize = 50 * 1024 * 1024; // 50MB
pub const CLIPBOARD_DEFAULT_SIZE: usize = 1024 * 1024; // 1MB

impl Default for HandlerSecuritySettings {
    fn default() -> Self {
        Self {
            read_only: false,
            disable_copy: false,
            disable_paste: false,
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,
            connection_timeout_secs: crate::DEFAULT_CONNECTION_TIMEOUT_SECS,
            idle_timeout_secs: 0,
            max_session_duration_secs: 0,
            wol_send_packet: false,
            wol_mac_addr: None,
            wol_broadcast_addr: "255.255.255.255".to_string(),
            wol_udp_port: 9,
            wol_wait_time: 0,
        }
    }
}

impl HandlerSecuritySettings {
    /// Parse security settings from connection parameters
    ///
    /// Parameter names match Guacamole/KCM conventions:
    /// - read-only: "true" or "1" to enable
    /// - disable-copy: "true" or "1" to enable
    /// - disable-paste: "true" or "1" to enable
    /// - clipboard-buffer-size: bytes (clamped to 256KB-50MB)
    /// - connection-timeout: seconds
    /// - idle-timeout: seconds
    /// - max-session-duration: seconds
    /// - wol-send-packet: "true" or "1" to enable
    /// - wol-mac-addr: MAC address (required if WoL enabled)
    /// - wol-broadcast-addr: broadcast IP (default: 255.255.255.255)
    /// - wol-udp-port: UDP port (default: 9)
    /// - wol-wait-time: seconds to wait after WoL
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let clipboard_buffer_size = params
            .get("clipboard-buffer-size")
            .and_then(|v| v.parse().ok())
            .unwrap_or(CLIPBOARD_DEFAULT_SIZE)
            .clamp(CLIPBOARD_MIN_SIZE, CLIPBOARD_MAX_SIZE);

        Self {
            read_only: parse_bool(params.get("read-only")),
            disable_copy: parse_bool(params.get("disable-copy")),
            disable_paste: parse_bool(params.get("disable-paste")),
            clipboard_buffer_size,
            connection_timeout_secs: params
                .get("timeout") // guacd name
                .or_else(|| params.get("connection-timeout")) // legacy name
                .and_then(|v| v.parse().ok())
                .unwrap_or(crate::DEFAULT_CONNECTION_TIMEOUT_SECS),
            idle_timeout_secs: params
                .get("idle-timeout")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            max_session_duration_secs: params
                .get("max-session-duration")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            wol_send_packet: parse_bool(params.get("wol-send-packet")),
            wol_mac_addr: params.get("wol-mac-addr").cloned(),
            wol_broadcast_addr: params
                .get("wol-broadcast-addr")
                .cloned()
                .unwrap_or_else(|| "255.255.255.255".to_string()),
            wol_udp_port: params
                .get("wol-udp-port")
                .and_then(|v| v.parse().ok())
                .unwrap_or(9),
            wol_wait_time: params
                .get("wol-wait-time")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }

    /// Check if clipboard copy is allowed
    pub fn is_copy_allowed(&self) -> bool {
        !self.disable_copy
    }

    /// Check if clipboard paste is allowed
    pub fn is_paste_allowed(&self) -> bool {
        !self.disable_paste && !self.read_only
    }

    /// Check if keyboard input is allowed
    ///
    /// In read-only mode, most keyboard input is blocked.
    /// Some protocols allow Ctrl+C for interrupt.
    pub fn is_keyboard_allowed(&self) -> bool {
        !self.read_only
    }

    /// Check if mouse click is allowed
    ///
    /// In read-only mode for graphical protocols:
    /// - Mouse movement is allowed (for viewing)
    /// - Mouse clicks are blocked (to prevent modifications)
    pub fn is_mouse_click_allowed(&self) -> bool {
        !self.read_only
    }

    /// Check if file upload is allowed (SFTP, file transfer)
    pub fn is_upload_allowed(&self) -> bool {
        !self.read_only
    }

    /// Check if file modification is allowed (SFTP delete, rename, mkdir)
    pub fn is_file_modification_allowed(&self) -> bool {
        !self.read_only
    }
}

/// Protocol-specific read-only behavior
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadOnlyBehavior {
    /// Block all keyboard input except Ctrl+C (SSH, Telnet)
    TerminalReadOnly,

    /// Block keyboard and mouse clicks, allow mouse move (RDP, VNC)
    GraphicalReadOnly,

    /// Block modifying queries (Database)
    DatabaseReadOnly,

    /// Block upload/delete/rename/mkdir (SFTP)
    SftpReadOnly,

    /// Block all input (RBI)
    BrowserReadOnly,
}

impl ReadOnlyBehavior {
    /// Get the appropriate read-only behavior for a protocol
    pub fn for_protocol(protocol: &str) -> Self {
        match protocol.to_lowercase().as_str() {
            "ssh" | "telnet" => Self::TerminalReadOnly,
            "rdp" | "vnc" => Self::GraphicalReadOnly,
            "mysql" | "postgresql" | "postgres" | "sqlserver" | "mssql" | "oracle" | "mongodb"
            | "redis" => Self::DatabaseReadOnly,
            "sftp" => Self::SftpReadOnly,
            "rbi" | "http" | "browser" => Self::BrowserReadOnly,
            _ => Self::TerminalReadOnly, // Default to terminal behavior
        }
    }
}

/// Check if a keyboard event should be allowed in read-only mode
///
/// For terminal protocols (SSH/Telnet), we allow:
/// - Ctrl+C (keysym 0x63 with Ctrl modifier, or raw 0x03)
///
/// This allows users to interrupt long-running commands even in read-only mode.
pub fn is_keyboard_event_allowed_readonly(keysym: u32, ctrl_pressed: bool) -> bool {
    // Allow Ctrl+C (interrupt)
    if ctrl_pressed && (keysym == 0x63 || keysym == 0x43) {
        return true;
    }

    // Allow raw Ctrl+C character
    if keysym == 0x03 {
        return true;
    }

    false
}

/// Check if a mouse event should be allowed in read-only mode
///
/// For graphical protocols (RDP/VNC), we allow:
/// - Mouse movement (button_mask == 0)
/// - Scroll wheel for viewing (button_mask & 0x18)
///
/// We block:
/// - Left/middle/right clicks (button_mask & 0x07)
pub fn is_mouse_event_allowed_readonly(button_mask: u32) -> bool {
    // Block left, middle, right clicks
    if (button_mask & 0x07) != 0 {
        return false;
    }

    // Allow movement and scroll
    true
}

fn parse_bool(value: Option<&String>) -> bool {
    value.map(|v| v == "true" || v == "1").unwrap_or(false)
}

// ============================================================================
// Protocol-Specific Security Extensions
// ============================================================================

/// Database-specific security settings
///
/// Extends HandlerSecuritySettings with database-specific controls.
/// Used by MySQL, PostgreSQL, SQL Server, Oracle, MongoDB, Redis handlers.
#[derive(Debug, Clone, Default)]
pub struct DatabaseSecuritySettings {
    /// Base security settings
    pub base: HandlerSecuritySettings,

    /// Disable CSV export (downloading query results)
    pub disable_csv_export: bool,

    /// Disable CSV import (uploading data files)
    pub disable_csv_import: bool,
}

impl DatabaseSecuritySettings {
    /// Parse from connection parameters
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        Self {
            base: HandlerSecuritySettings::from_params(params),
            disable_csv_export: parse_bool(params.get("disable-csv-export")),
            disable_csv_import: parse_bool(params.get("disable-csv-import")),
        }
    }

    /// Check if CSV export is allowed
    pub fn is_csv_export_allowed(&self) -> bool {
        !self.disable_csv_export
    }

    /// Check if CSV import is allowed
    pub fn is_csv_import_allowed(&self) -> bool {
        !self.disable_csv_import && !self.base.read_only
    }
}

/// SQL query classifier for read-only mode enforcement
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueryType {
    /// SELECT, SHOW, DESCRIBE, EXPLAIN - allowed in read-only mode
    ReadOnly,
    /// INSERT, UPDATE, DELETE, etc - blocked in read-only mode
    Modifying,
    /// Unknown or unparseable query
    Unknown,
}

/// Classify a SQL query for security enforcement
///
/// Returns the query type to determine if it should be blocked in read-only mode.
pub fn classify_sql_query(query: &str) -> QueryType {
    let query_trimmed = query.trim();
    if query_trimmed.is_empty() {
        return QueryType::Unknown;
    }

    // Get first word (command)
    let first_word = query_trimmed
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_uppercase();

    match first_word.as_str() {
        // Read-only commands
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "HELP" | "USE" => QueryType::ReadOnly,

        // PostgreSQL-specific read-only
        "\\D" | "\\DT" | "\\L" | "\\C" | "\\?" | "\\H" => QueryType::ReadOnly,

        // Modifying commands
        "INSERT" | "UPDATE" | "DELETE" | "DROP" | "ALTER" | "CREATE" | "TRUNCATE" | "REPLACE"
        | "GRANT" | "REVOKE" | "RENAME" | "LOAD" | "CALL" | "EXEC" | "EXECUTE" | "MERGE"
        | "UPSERT" => QueryType::Modifying,

        // Database administration (modifying)
        "SET" | "RESET" | "KILL" | "FLUSH" | "OPTIMIZE" | "ANALYZE" | "REPAIR" | "VACUUM"
        | "REINDEX" | "CLUSTER" => QueryType::Modifying,

        // Transaction control - allow these for usability
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => QueryType::ReadOnly,

        _ => QueryType::Unknown,
    }
}

/// Check if a SQL query is allowed based on security settings
pub fn check_sql_query_allowed(
    query: &str,
    settings: &DatabaseSecuritySettings,
) -> Result<(), String> {
    if !settings.base.read_only {
        return Ok(());
    }

    match classify_sql_query(query) {
        QueryType::ReadOnly => Ok(()),
        QueryType::Modifying => Err(
            "Query blocked: read-only mode is enabled. Modifying queries are not permitted.".to_string()
        ),
        QueryType::Unknown => Err(
            "Query blocked: unrecognized command in read-only mode. Only SELECT, SHOW, DESCRIBE, and EXPLAIN are permitted.".to_string()
        ),
    }
}

/// Detect MySQL export queries (INTO OUTFILE)
pub fn is_mysql_export_query(query: &str) -> bool {
    let query_upper = query.to_uppercase();
    query_upper.contains("INTO OUTFILE") || query_upper.contains("INTO LOCAL OUTFILE")
}

/// Detect MySQL import queries (LOAD DATA)
pub fn is_mysql_import_query(query: &str) -> bool {
    let query_upper = query.to_uppercase();
    query_upper.contains("LOAD DATA") || query_upper.contains("LOAD LOCAL DATA")
}

/// Detect PostgreSQL COPY export
pub fn is_postgres_copy_out(query: &str) -> bool {
    let query_upper = query.to_uppercase();
    query_upper.starts_with("COPY") && query_upper.contains("TO STDOUT")
}

/// Detect PostgreSQL COPY import
pub fn is_postgres_copy_in(query: &str) -> bool {
    let query_upper = query.to_uppercase();
    query_upper.starts_with("COPY") && query_upper.contains("FROM STDIN")
}

/// SFTP-specific security settings
///
/// Extends HandlerSecuritySettings with SFTP-specific controls.
#[derive(Debug, Clone, Default)]
pub struct SftpSecuritySettings {
    /// Base security settings
    pub base: HandlerSecuritySettings,

    /// Root directory to restrict file access (chroot-like)
    pub root_directory: Option<String>,

    /// Disable file download
    pub disable_download: bool,

    /// Disable file upload
    pub disable_upload: bool,

    /// Maximum file size for upload (bytes, 0 = no limit)
    pub max_upload_size: u64,
}

impl SftpSecuritySettings {
    /// Parse from connection parameters
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        Self {
            base: HandlerSecuritySettings::from_params(params),
            root_directory: params.get("sftp-root-directory").cloned(),
            disable_download: parse_bool(params.get("sftp-disable-download"))
                || parse_bool(params.get("disable-copy")), // disable-copy affects download
            disable_upload: parse_bool(params.get("sftp-disable-upload"))
                || parse_bool(params.get("read-only")), // read-only blocks upload
            max_upload_size: params
                .get("sftp-max-upload-size")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }

    /// Check if download is allowed
    pub fn is_download_allowed(&self) -> bool {
        !self.disable_download
    }

    /// Check if upload is allowed
    pub fn is_upload_allowed(&self) -> bool {
        !self.disable_upload && !self.base.read_only
    }

    /// Check if a file operation is allowed
    pub fn is_file_operation_allowed(&self, operation: SftpOperation) -> bool {
        match operation {
            SftpOperation::Read | SftpOperation::List | SftpOperation::Stat => true,
            SftpOperation::Download => self.is_download_allowed(),
            SftpOperation::Upload => self.is_upload_allowed(),
            SftpOperation::Delete
            | SftpOperation::Rename
            | SftpOperation::Mkdir
            | SftpOperation::Rmdir => !self.base.read_only,
        }
    }
}

/// SFTP file operations
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SftpOperation {
    Read,
    List,
    Stat,
    Download,
    Upload,
    Delete,
    Rename,
    Mkdir,
    Rmdir,
}

/// RBI (Remote Browser Isolation) specific security settings
///
/// Extends HandlerSecuritySettings with browser-specific controls.
#[derive(Debug, Clone, Default)]
pub struct RbiSecuritySettings {
    /// Base security settings
    pub base: HandlerSecuritySettings,

    /// Disable file downloads from browser
    pub disable_download: bool,

    /// Disable file uploads to browser
    pub disable_upload: bool,

    /// Disable printing
    pub disable_print: bool,

    /// URL allowlist (if set, only these URLs are accessible)
    pub url_allowlist: Vec<String>,

    /// URL blocklist (these URLs are blocked)
    pub url_blocklist: Vec<String>,
}

impl RbiSecuritySettings {
    /// Parse from connection parameters
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let url_allowlist = params
            .get("rbi-url-allowlist")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        let url_blocklist = params
            .get("rbi-url-blocklist")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        Self {
            base: HandlerSecuritySettings::from_params(params),
            disable_download: parse_bool(params.get("rbi-disable-download")),
            disable_upload: parse_bool(params.get("rbi-disable-upload"))
                || parse_bool(params.get("read-only")),
            disable_print: parse_bool(params.get("rbi-disable-print")),
            url_allowlist,
            url_blocklist,
        }
    }

    /// Check if a URL is allowed
    pub fn is_url_allowed(&self, url: &str) -> bool {
        // If allowlist is set, URL must match one of the patterns
        if !self.url_allowlist.is_empty() {
            return self
                .url_allowlist
                .iter()
                .any(|pattern| url.contains(pattern));
        }

        // If blocklist is set, URL must not match any pattern
        if !self.url_blocklist.is_empty() {
            return !self
                .url_blocklist
                .iter()
                .any(|pattern| url.contains(pattern));
        }

        true
    }

    /// Check if download is allowed
    pub fn is_download_allowed(&self) -> bool {
        !self.disable_download
    }

    /// Check if upload is allowed
    pub fn is_upload_allowed(&self) -> bool {
        !self.disable_upload && !self.base.read_only
    }

    /// Check if printing is allowed
    pub fn is_print_allowed(&self) -> bool {
        !self.disable_print
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = HandlerSecuritySettings::default();
        assert!(!settings.read_only);
        assert!(!settings.disable_copy);
        assert!(!settings.disable_paste);
        assert_eq!(settings.clipboard_buffer_size, CLIPBOARD_DEFAULT_SIZE);
        assert_eq!(
            settings.connection_timeout_secs,
            crate::DEFAULT_CONNECTION_TIMEOUT_SECS
        );
    }

    #[test]
    fn test_from_params() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-copy".to_string(), "1".to_string());
        params.insert("clipboard-buffer-size".to_string(), "2097152".to_string()); // 2MB

        let settings = HandlerSecuritySettings::from_params(&params);

        assert!(settings.read_only);
        assert!(settings.disable_copy);
        assert!(!settings.disable_paste);
        assert_eq!(settings.clipboard_buffer_size, 2097152);
    }

    #[test]
    fn test_clipboard_size_clamping() {
        let mut params = HashMap::new();

        // Too small - should clamp to minimum
        params.insert("clipboard-buffer-size".to_string(), "100".to_string());
        let settings = HandlerSecuritySettings::from_params(&params);
        assert_eq!(settings.clipboard_buffer_size, CLIPBOARD_MIN_SIZE);

        // Too large - should clamp to maximum
        params.insert(
            "clipboard-buffer-size".to_string(),
            "100000000000".to_string(),
        );
        let settings = HandlerSecuritySettings::from_params(&params);
        assert_eq!(settings.clipboard_buffer_size, CLIPBOARD_MAX_SIZE);
    }

    #[test]
    fn test_wol_settings() {
        let mut params = HashMap::new();
        params.insert("wol-send-packet".to_string(), "true".to_string());
        params.insert("wol-mac-addr".to_string(), "AA:BB:CC:DD:EE:FF".to_string());
        params.insert("wol-udp-port".to_string(), "7".to_string());
        params.insert("wol-wait-time".to_string(), "30".to_string());

        let settings = HandlerSecuritySettings::from_params(&params);

        assert!(settings.wol_send_packet);
        assert_eq!(settings.wol_mac_addr, Some("AA:BB:CC:DD:EE:FF".to_string()));
        assert_eq!(settings.wol_udp_port, 7);
        assert_eq!(settings.wol_wait_time, 30);
    }

    #[test]
    fn test_permission_checks() {
        let mut settings = HandlerSecuritySettings::default();

        assert!(settings.is_copy_allowed());
        assert!(settings.is_paste_allowed());
        assert!(settings.is_keyboard_allowed());
        assert!(settings.is_mouse_click_allowed());

        settings.read_only = true;
        assert!(settings.is_copy_allowed()); // Copy still allowed in read-only
        assert!(!settings.is_paste_allowed()); // Paste blocked
        assert!(!settings.is_keyboard_allowed());
        assert!(!settings.is_mouse_click_allowed());

        settings.read_only = false;
        settings.disable_copy = true;
        assert!(!settings.is_copy_allowed());
        assert!(settings.is_paste_allowed());
    }

    #[test]
    fn test_readonly_keyboard_allowed() {
        // Ctrl+C should be allowed
        assert!(is_keyboard_event_allowed_readonly(0x63, true)); // 'c' with Ctrl
        assert!(is_keyboard_event_allowed_readonly(0x43, true)); // 'C' with Ctrl
        assert!(is_keyboard_event_allowed_readonly(0x03, false)); // Raw Ctrl+C

        // Regular keys should be blocked
        assert!(!is_keyboard_event_allowed_readonly(0x61, false)); // 'a'
        assert!(!is_keyboard_event_allowed_readonly(0xFF0D, false)); // Enter
    }

    #[test]
    fn test_readonly_mouse_allowed() {
        // Movement allowed
        assert!(is_mouse_event_allowed_readonly(0x00));

        // Scroll allowed
        assert!(is_mouse_event_allowed_readonly(0x08)); // Scroll up
        assert!(is_mouse_event_allowed_readonly(0x10)); // Scroll down

        // Clicks blocked
        assert!(!is_mouse_event_allowed_readonly(0x01)); // Left click
        assert!(!is_mouse_event_allowed_readonly(0x02)); // Middle click
        assert!(!is_mouse_event_allowed_readonly(0x04)); // Right click
    }

    #[test]
    fn test_protocol_behavior() {
        assert_eq!(
            ReadOnlyBehavior::for_protocol("ssh"),
            ReadOnlyBehavior::TerminalReadOnly
        );
        assert_eq!(
            ReadOnlyBehavior::for_protocol("rdp"),
            ReadOnlyBehavior::GraphicalReadOnly
        );
        assert_eq!(
            ReadOnlyBehavior::for_protocol("mysql"),
            ReadOnlyBehavior::DatabaseReadOnly
        );
        assert_eq!(
            ReadOnlyBehavior::for_protocol("sftp"),
            ReadOnlyBehavior::SftpReadOnly
        );
        assert_eq!(
            ReadOnlyBehavior::for_protocol("rbi"),
            ReadOnlyBehavior::BrowserReadOnly
        );
    }

    // ========================================================================
    // Database Security Tests
    // ========================================================================

    #[test]
    fn test_database_security_settings() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-csv-export".to_string(), "true".to_string());

        let settings = DatabaseSecuritySettings::from_params(&params);

        assert!(settings.base.read_only);
        assert!(settings.disable_csv_export);
        assert!(!settings.disable_csv_import);
        assert!(!settings.is_csv_export_allowed());
        assert!(!settings.is_csv_import_allowed()); // read_only blocks import
    }

    #[test]
    fn test_classify_sql_query() {
        assert_eq!(
            classify_sql_query("SELECT * FROM users"),
            QueryType::ReadOnly
        );
        assert_eq!(classify_sql_query("SHOW DATABASES"), QueryType::ReadOnly);
        assert_eq!(
            classify_sql_query("INSERT INTO users VALUES (1)"),
            QueryType::Modifying
        );
        assert_eq!(classify_sql_query("DROP TABLE users"), QueryType::Modifying);
        assert_eq!(classify_sql_query("BEGIN"), QueryType::ReadOnly);
    }

    #[test]
    fn test_check_sql_query_allowed() {
        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(check_sql_query_allowed("SELECT * FROM users", &settings).is_ok());
        assert!(check_sql_query_allowed("INSERT INTO users VALUES (1)", &settings).is_err());
    }

    #[test]
    fn test_mysql_import_export_detection() {
        assert!(is_mysql_export_query(
            "SELECT * INTO OUTFILE '/tmp/x.csv' FROM users"
        ));
        assert!(!is_mysql_export_query("SELECT * FROM users"));
        assert!(is_mysql_import_query(
            "LOAD DATA INFILE '/tmp/x.csv' INTO TABLE users"
        ));
        assert!(!is_mysql_import_query("SELECT * FROM users"));
    }

    #[test]
    fn test_postgres_copy_detection() {
        assert!(is_postgres_copy_out("COPY users TO STDOUT"));
        assert!(is_postgres_copy_in("COPY users FROM STDIN"));
        assert!(!is_postgres_copy_out("SELECT * FROM users"));
    }

    // ========================================================================
    // SFTP Security Tests
    // ========================================================================

    #[test]
    fn test_sftp_security_settings() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("sftp-root-directory".to_string(), "/home/user".to_string());

        let settings = SftpSecuritySettings::from_params(&params);

        assert!(settings.base.read_only);
        assert_eq!(settings.root_directory, Some("/home/user".to_string()));
        assert!(!settings.is_upload_allowed());
        assert!(settings.is_download_allowed());
    }

    #[test]
    fn test_sftp_operation_allowed() {
        let settings = SftpSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(settings.is_file_operation_allowed(SftpOperation::Read));
        assert!(settings.is_file_operation_allowed(SftpOperation::List));
        assert!(settings.is_file_operation_allowed(SftpOperation::Download));
        assert!(!settings.is_file_operation_allowed(SftpOperation::Upload));
        assert!(!settings.is_file_operation_allowed(SftpOperation::Delete));
        assert!(!settings.is_file_operation_allowed(SftpOperation::Mkdir));
    }

    // ========================================================================
    // RBI Security Tests
    // ========================================================================

    #[test]
    fn test_rbi_security_settings() {
        let mut params = HashMap::new();
        params.insert("rbi-disable-download".to_string(), "true".to_string());
        params.insert(
            "rbi-url-blocklist".to_string(),
            "facebook.com, twitter.com".to_string(),
        );

        let settings = RbiSecuritySettings::from_params(&params);

        assert!(!settings.is_download_allowed());
        assert!(settings.is_upload_allowed());
        assert_eq!(settings.url_blocklist.len(), 2);
    }

    #[test]
    fn test_rbi_url_filtering() {
        let settings = RbiSecuritySettings {
            url_blocklist: vec!["facebook.com".to_string(), "twitter.com".to_string()],
            ..Default::default()
        };

        assert!(!settings.is_url_allowed("https://facebook.com/page"));
        assert!(!settings.is_url_allowed("https://twitter.com/user"));
        assert!(settings.is_url_allowed("https://google.com"));

        let settings_allowlist = RbiSecuritySettings {
            url_allowlist: vec!["internal.corp".to_string()],
            ..Default::default()
        };

        assert!(settings_allowlist.is_url_allowed("https://internal.corp/app"));
        assert!(!settings_allowlist.is_url_allowed("https://google.com"));
    }

    // ========================================================================
    // Comprehensive Integration-Style Security Tests
    // ========================================================================

    #[test]
    fn test_all_sql_query_types_classified() {
        // Read-only queries
        let readonly_queries = [
            "SELECT * FROM users",
            "select id from t",
            "SHOW DATABASES",
            "SHOW TABLES",
            "DESCRIBE users",
            "DESC users",
            "EXPLAIN SELECT 1",
            "HELP",
            "USE database_name",
            "BEGIN",
            "START TRANSACTION",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT test",
            "RELEASE SAVEPOINT test",
            // PostgreSQL meta-commands
            "\\d",
            "\\dt",
            "\\l",
            "\\c",
            "\\?",
            "\\h",
        ];

        for query in readonly_queries {
            assert_eq!(
                classify_sql_query(query),
                QueryType::ReadOnly,
                "Query '{}' should be classified as ReadOnly",
                query
            );
        }

        // Modifying queries
        let modifying_queries = [
            "INSERT INTO users VALUES (1)",
            "UPDATE users SET name='x'",
            "DELETE FROM users",
            "DROP TABLE users",
            "DROP DATABASE test",
            "ALTER TABLE users ADD col INT",
            "CREATE TABLE new_table (id INT)",
            "CREATE DATABASE test",
            "TRUNCATE TABLE users",
            "REPLACE INTO users VALUES (1)",
            "GRANT SELECT ON users TO 'user'",
            "REVOKE SELECT ON users FROM 'user'",
            "RENAME TABLE old TO new",
            "LOAD DATA INFILE '/tmp/x.csv' INTO TABLE users",
            "CALL stored_proc()",
            "EXEC stored_proc",
            "EXECUTE stored_proc",
            "MERGE INTO users USING source",
            "UPSERT INTO users VALUES (1)",
            // Admin commands
            "SET GLOBAL var = 1",
            "RESET QUERY CACHE",
            "KILL 123",
            "FLUSH TABLES",
            "OPTIMIZE TABLE users",
            "ANALYZE TABLE users",
            "REPAIR TABLE users",
            "VACUUM",
            "REINDEX TABLE users",
            "CLUSTER users",
        ];

        for query in modifying_queries {
            assert_eq!(
                classify_sql_query(query),
                QueryType::Modifying,
                "Query '{}' should be classified as Modifying",
                query
            );
        }
    }

    #[test]
    fn test_security_settings_full_workflow() {
        // Simulate a connection with all security parameters
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-copy".to_string(), "true".to_string());
        params.insert("disable-paste".to_string(), "true".to_string());
        params.insert("clipboard-buffer-size".to_string(), "512000".to_string()); // 500KB
        params.insert("connection-timeout".to_string(), "60".to_string());
        params.insert("idle-timeout".to_string(), "300".to_string());
        params.insert("wol-send-packet".to_string(), "true".to_string());
        params.insert("wol-mac-addr".to_string(), "00:11:22:33:44:55".to_string());

        let settings = HandlerSecuritySettings::from_params(&params);

        // Verify all settings were parsed
        assert!(settings.read_only);
        assert!(settings.disable_copy);
        assert!(settings.disable_paste);
        assert_eq!(settings.clipboard_buffer_size, 512000);
        assert_eq!(settings.connection_timeout_secs, 60);
        assert_eq!(settings.idle_timeout_secs, 300);
        assert!(settings.wol_send_packet);
        assert_eq!(settings.wol_mac_addr, Some("00:11:22:33:44:55".to_string()));

        // Verify permission checks
        assert!(!settings.is_copy_allowed());
        assert!(!settings.is_paste_allowed());
        assert!(!settings.is_keyboard_allowed());
        assert!(!settings.is_mouse_click_allowed());
    }

    #[test]
    fn test_database_security_full_workflow() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-copy".to_string(), "true".to_string());
        params.insert("disable-csv-export".to_string(), "true".to_string());
        params.insert("disable-csv-import".to_string(), "true".to_string());
        params.insert("clipboard-buffer-size".to_string(), "2097152".to_string()); // 2MB

        let settings = DatabaseSecuritySettings::from_params(&params);

        // Base settings inherited
        assert!(settings.base.read_only);
        assert!(settings.base.disable_copy);
        assert_eq!(settings.base.clipboard_buffer_size, 2097152);

        // Database-specific settings
        assert!(settings.disable_csv_export);
        assert!(settings.disable_csv_import);
        assert!(!settings.is_csv_export_allowed());
        assert!(!settings.is_csv_import_allowed());

        // Test query blocking
        assert!(check_sql_query_allowed("SELECT * FROM users", &settings).is_ok());
        assert!(check_sql_query_allowed("INSERT INTO users VALUES (1)", &settings).is_err());
        assert!(check_sql_query_allowed("DROP TABLE users", &settings).is_err());

        // Test MySQL import/export detection
        assert!(is_mysql_export_query(
            "SELECT * INTO OUTFILE '/tmp/x.csv' FROM users"
        ));
        assert!(is_mysql_import_query(
            "LOAD DATA INFILE '/tmp/x.csv' INTO TABLE users"
        ));
    }

    #[test]
    fn test_sftp_security_full_workflow() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert(
            "sftp-root-directory".to_string(),
            "/home/restricted".to_string(),
        );
        params.insert("sftp-disable-download".to_string(), "true".to_string());
        params.insert("sftp-max-upload-size".to_string(), "10485760".to_string()); // 10MB

        let settings = SftpSecuritySettings::from_params(&params);

        // Verify settings
        assert!(settings.base.read_only);
        assert_eq!(
            settings.root_directory,
            Some("/home/restricted".to_string())
        );
        assert!(settings.disable_download);
        assert_eq!(settings.max_upload_size, 10485760);

        // Verify operation permissions
        assert!(settings.is_file_operation_allowed(SftpOperation::Read));
        assert!(settings.is_file_operation_allowed(SftpOperation::List));
        assert!(settings.is_file_operation_allowed(SftpOperation::Stat));
        assert!(!settings.is_file_operation_allowed(SftpOperation::Download)); // disabled
        assert!(!settings.is_file_operation_allowed(SftpOperation::Upload)); // read_only
        assert!(!settings.is_file_operation_allowed(SftpOperation::Delete)); // read_only
        assert!(!settings.is_file_operation_allowed(SftpOperation::Rename)); // read_only
        assert!(!settings.is_file_operation_allowed(SftpOperation::Mkdir)); // read_only
        assert!(!settings.is_file_operation_allowed(SftpOperation::Rmdir)); // read_only
    }

    #[test]
    fn test_rbi_security_full_workflow() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("rbi-disable-download".to_string(), "true".to_string());
        params.insert("rbi-disable-print".to_string(), "true".to_string());
        // Use substring patterns (URL matching uses contains, not glob patterns)
        params.insert(
            "rbi-url-allowlist".to_string(),
            "example.com, internal.corp".to_string(),
        );

        let settings = RbiSecuritySettings::from_params(&params);

        // Verify settings
        assert!(settings.base.read_only);
        assert!(settings.disable_download);
        assert!(settings.disable_print);
        assert!(!settings.is_upload_allowed()); // read_only blocks upload

        // Verify URL filtering (uses substring matching)
        assert!(settings.is_url_allowed("https://app.example.com/page"));
        assert!(settings.is_url_allowed("https://internal.corp/dashboard"));
        assert!(!settings.is_url_allowed("https://facebook.com"));
        assert!(!settings.is_url_allowed("https://malware-site.com"));
    }

    #[test]
    fn test_keyboard_readonly_edge_cases() {
        // Navigation keys should be blocked in read-only
        assert!(!is_keyboard_event_allowed_readonly(0xFF08, false)); // Backspace
        assert!(!is_keyboard_event_allowed_readonly(0xFF09, false)); // Tab
        assert!(!is_keyboard_event_allowed_readonly(0xFF0D, false)); // Enter
        assert!(!is_keyboard_event_allowed_readonly(0xFF1B, false)); // Escape
        assert!(!is_keyboard_event_allowed_readonly(0xFFFF, false)); // Delete

        // Arrow keys (for text selection) - currently blocked too
        assert!(!is_keyboard_event_allowed_readonly(0xFF51, false)); // Left
        assert!(!is_keyboard_event_allowed_readonly(0xFF52, false)); // Up
        assert!(!is_keyboard_event_allowed_readonly(0xFF53, false)); // Right
        assert!(!is_keyboard_event_allowed_readonly(0xFF54, false)); // Down

        // Modifier keys alone should be allowed (they don't produce input)
        // Note: This depends on implementation - currently they're blocked
    }

    #[test]
    fn test_mouse_readonly_edge_cases() {
        // Combined button presses should be blocked
        assert!(!is_mouse_event_allowed_readonly(0x03)); // Left + Middle
        assert!(!is_mouse_event_allowed_readonly(0x05)); // Left + Right
        assert!(!is_mouse_event_allowed_readonly(0x07)); // Left + Middle + Right

        // Scroll with buttons should be blocked
        assert!(!is_mouse_event_allowed_readonly(0x09)); // Left + Scroll up
        assert!(!is_mouse_event_allowed_readonly(0x11)); // Left + Scroll down

        // Pure scroll should be allowed
        assert!(is_mouse_event_allowed_readonly(0x08)); // Scroll up only
        assert!(is_mouse_event_allowed_readonly(0x10)); // Scroll down only
        assert!(is_mouse_event_allowed_readonly(0x18)); // Scroll up + down (edge case)
    }
}
