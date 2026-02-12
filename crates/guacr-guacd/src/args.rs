// Protocol argument descriptors for guacd handshake
//
// These define the parameters each protocol accepts during the guacd
// select/args/connect handshake. They match the official Apache Guacamole
// guacd parameter names exactly.

/// Argument descriptor for protocol handlers
#[derive(Debug, Clone)]
pub struct ArgDescriptor {
    /// Argument name (e.g., "hostname", "port", "username")
    pub name: &'static str,
    /// Whether this argument is required
    pub required: bool,
}

impl ArgDescriptor {
    pub const fn required(name: &'static str) -> Self {
        Self {
            name,
            required: true,
        }
    }

    pub const fn optional(name: &'static str) -> Self {
        Self {
            name,
            required: false,
        }
    }
}

/// Standard argument sets for common protocols
/// These match the official Apache Guacamole guacd parameter names exactly.
/// See: <https://github.com/apache/guacamole-server/tree/main/src/protocols>
pub mod protocol_args {
    use super::ArgDescriptor;

    /// SSH protocol arguments (matches guacd libguac-client-ssh exactly)
    /// Order matters - must match guacd's GUAC_SSH_CLIENT_ARGS array
    pub const SSH: &[ArgDescriptor] = &[
        // Connection
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("host-key"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        // Authentication
        ArgDescriptor::required("username"),
        ArgDescriptor::optional("password"),
        // Display
        ArgDescriptor::optional("font-name"),
        ArgDescriptor::optional("font-size"),
        // SFTP
        ArgDescriptor::optional("enable-sftp"),
        ArgDescriptor::optional("sftp-root-directory"),
        ArgDescriptor::optional("sftp-disable-download"),
        ArgDescriptor::optional("sftp-disable-upload"),
        // Key authentication
        ArgDescriptor::optional("private-key"),
        ArgDescriptor::optional("passphrase"),
        ArgDescriptor::optional("public-key"),
        // SSH agent (conditional in guacd, always included here)
        ArgDescriptor::optional("enable-agent"),
        // Terminal
        ArgDescriptor::optional("color-scheme"),
        ArgDescriptor::optional("command"),
        // Recording - typescript
        ArgDescriptor::optional("typescript-path"),
        ArgDescriptor::optional("typescript-name"),
        ArgDescriptor::optional("create-typescript-path"),
        ArgDescriptor::optional("typescript-write-existing"),
        // Recording - session
        ArgDescriptor::optional("recording-path"),
        ArgDescriptor::optional("recording-name"),
        ArgDescriptor::optional("recording-exclude-output"),
        ArgDescriptor::optional("recording-exclude-mouse"),
        ArgDescriptor::optional("recording-include-keys"),
        ArgDescriptor::optional("create-recording-path"),
        ArgDescriptor::optional("recording-write-existing"),
        // Security
        ArgDescriptor::optional("read-only"),
        // Terminal settings
        ArgDescriptor::optional("server-alive-interval"),
        ArgDescriptor::optional("backspace"),
        ArgDescriptor::optional("func-keys-and-keypad"),
        ArgDescriptor::optional("terminal-type"),
        ArgDescriptor::optional("scrollback"),
        ArgDescriptor::optional("locale"),
        ArgDescriptor::optional("timezone"),
        // Clipboard
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        // Wake-on-LAN
        ArgDescriptor::optional("wol-send-packet"),
        ArgDescriptor::optional("wol-mac-addr"),
        ArgDescriptor::optional("wol-broadcast-addr"),
        ArgDescriptor::optional("wol-udp-port"),
        ArgDescriptor::optional("wol-wait-time"),
    ];

    /// Telnet protocol arguments (matches guacd libguac-client-telnet exactly)
    pub const TELNET: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::optional("username"),
        ArgDescriptor::optional("username-regex"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("password-regex"),
        ArgDescriptor::optional("font-name"),
        ArgDescriptor::optional("font-size"),
        ArgDescriptor::optional("color-scheme"),
        ArgDescriptor::optional("typescript-path"),
        ArgDescriptor::optional("typescript-name"),
        ArgDescriptor::optional("create-typescript-path"),
        ArgDescriptor::optional("typescript-write-existing"),
        ArgDescriptor::optional("recording-path"),
        ArgDescriptor::optional("recording-name"),
        ArgDescriptor::optional("recording-exclude-output"),
        ArgDescriptor::optional("recording-exclude-mouse"),
        ArgDescriptor::optional("recording-include-keys"),
        ArgDescriptor::optional("create-recording-path"),
        ArgDescriptor::optional("recording-write-existing"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("backspace"),
        ArgDescriptor::optional("func-keys-and-keypad"),
        ArgDescriptor::optional("terminal-type"),
        ArgDescriptor::optional("scrollback"),
        ArgDescriptor::optional("login-success-regex"),
        ArgDescriptor::optional("login-failure-regex"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("wol-send-packet"),
        ArgDescriptor::optional("wol-mac-addr"),
        ArgDescriptor::optional("wol-broadcast-addr"),
        ArgDescriptor::optional("wol-udp-port"),
        ArgDescriptor::optional("wol-wait-time"),
    ];

    /// RDP protocol arguments (matches guacd libguac-client-rdp exactly)
    pub const RDP: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::optional("domain"),
        ArgDescriptor::optional("username"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("width"),
        ArgDescriptor::optional("height"),
        ArgDescriptor::optional("dpi"),
        ArgDescriptor::optional("initial-program"),
        ArgDescriptor::optional("color-depth"),
        ArgDescriptor::optional("disable-audio"),
        ArgDescriptor::optional("enable-printing"),
        ArgDescriptor::optional("printer-name"),
        ArgDescriptor::optional("enable-drive"),
        ArgDescriptor::optional("drive-name"),
        ArgDescriptor::optional("drive-path"),
        ArgDescriptor::optional("create-drive-path"),
        ArgDescriptor::optional("disable-download"),
        ArgDescriptor::optional("disable-upload"),
        ArgDescriptor::optional("console"),
        ArgDescriptor::optional("console-audio"),
        ArgDescriptor::optional("server-layout"),
        ArgDescriptor::optional("security"),
        ArgDescriptor::optional("ignore-cert"),
        ArgDescriptor::optional("cert-tofu"),
        ArgDescriptor::optional("cert-fingerprints"),
        ArgDescriptor::optional("disable-auth"),
        ArgDescriptor::optional("remote-app"),
        ArgDescriptor::optional("remote-app-dir"),
        ArgDescriptor::optional("remote-app-args"),
        ArgDescriptor::optional("static-channels"),
        ArgDescriptor::optional("client-name"),
        ArgDescriptor::optional("enable-wallpaper"),
        ArgDescriptor::optional("enable-theming"),
        ArgDescriptor::optional("enable-font-smoothing"),
        ArgDescriptor::optional("enable-full-window-drag"),
        ArgDescriptor::optional("enable-desktop-composition"),
        ArgDescriptor::optional("enable-menu-animations"),
        ArgDescriptor::optional("disable-bitmap-caching"),
        ArgDescriptor::optional("disable-offscreen-caching"),
        ArgDescriptor::optional("disable-glyph-caching"),
        ArgDescriptor::optional("disable-gfx"),
        ArgDescriptor::optional("preconnection-id"),
        ArgDescriptor::optional("preconnection-blob"),
        ArgDescriptor::optional("timezone"),
        ArgDescriptor::optional("enable-sftp"),
        ArgDescriptor::optional("sftp-hostname"),
        ArgDescriptor::optional("sftp-host-key"),
        ArgDescriptor::optional("sftp-port"),
        ArgDescriptor::optional("sftp-timeout"),
        ArgDescriptor::optional("sftp-username"),
        ArgDescriptor::optional("sftp-password"),
        ArgDescriptor::optional("sftp-private-key"),
        ArgDescriptor::optional("sftp-passphrase"),
        ArgDescriptor::optional("sftp-public-key"),
        ArgDescriptor::optional("sftp-directory"),
        ArgDescriptor::optional("sftp-root-directory"),
        ArgDescriptor::optional("sftp-server-alive-interval"),
        ArgDescriptor::optional("sftp-disable-download"),
        ArgDescriptor::optional("sftp-disable-upload"),
        ArgDescriptor::optional("recording-path"),
        ArgDescriptor::optional("recording-name"),
        ArgDescriptor::optional("recording-exclude-output"),
        ArgDescriptor::optional("recording-exclude-mouse"),
        ArgDescriptor::optional("recording-exclude-touch"),
        ArgDescriptor::optional("recording-include-keys"),
        ArgDescriptor::optional("create-recording-path"),
        ArgDescriptor::optional("recording-write-existing"),
        ArgDescriptor::optional("resize-method"),
        ArgDescriptor::optional("enable-audio-input"),
        ArgDescriptor::optional("enable-touch"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("gateway-hostname"),
        ArgDescriptor::optional("gateway-port"),
        ArgDescriptor::optional("gateway-domain"),
        ArgDescriptor::optional("gateway-username"),
        ArgDescriptor::optional("gateway-password"),
        ArgDescriptor::optional("load-balance-info"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("wol-send-packet"),
        ArgDescriptor::optional("wol-mac-addr"),
        ArgDescriptor::optional("wol-broadcast-addr"),
        ArgDescriptor::optional("wol-udp-port"),
        ArgDescriptor::optional("wol-wait-time"),
        ArgDescriptor::optional("force-lossless"),
        ArgDescriptor::optional("normalize-clipboard"),
    ];

    /// VNC protocol arguments (matches guacd libguac-client-vnc exactly)
    pub const VNC: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("disable-display-resize"),
        ArgDescriptor::optional("encodings"),
        ArgDescriptor::optional("username"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("swap-red-blue"),
        ArgDescriptor::optional("color-depth"),
        ArgDescriptor::optional("cursor"),
        ArgDescriptor::optional("autoretry"),
        ArgDescriptor::optional("clipboard-encoding"),
        ArgDescriptor::optional("dest-host"),
        ArgDescriptor::optional("dest-port"),
        ArgDescriptor::optional("enable-audio"),
        ArgDescriptor::optional("audio-servername"),
        ArgDescriptor::optional("reverse-connect"),
        ArgDescriptor::optional("listen-timeout"),
        ArgDescriptor::optional("enable-sftp"),
        ArgDescriptor::optional("sftp-hostname"),
        ArgDescriptor::optional("sftp-host-key"),
        ArgDescriptor::optional("sftp-port"),
        ArgDescriptor::optional("sftp-timeout"),
        ArgDescriptor::optional("sftp-username"),
        ArgDescriptor::optional("sftp-password"),
        ArgDescriptor::optional("sftp-private-key"),
        ArgDescriptor::optional("sftp-passphrase"),
        ArgDescriptor::optional("sftp-public-key"),
        ArgDescriptor::optional("sftp-directory"),
        ArgDescriptor::optional("sftp-root-directory"),
        ArgDescriptor::optional("sftp-server-alive-interval"),
        ArgDescriptor::optional("sftp-disable-download"),
        ArgDescriptor::optional("sftp-disable-upload"),
        ArgDescriptor::optional("recording-path"),
        ArgDescriptor::optional("recording-name"),
        ArgDescriptor::optional("recording-exclude-output"),
        ArgDescriptor::optional("recording-exclude-mouse"),
        ArgDescriptor::optional("recording-include-keys"),
        ArgDescriptor::optional("create-recording-path"),
        ArgDescriptor::optional("recording-write-existing"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-server-input"),
        ArgDescriptor::optional("wol-send-packet"),
        ArgDescriptor::optional("wol-mac-addr"),
        ArgDescriptor::optional("wol-broadcast-addr"),
        ArgDescriptor::optional("wol-udp-port"),
        ArgDescriptor::optional("wol-wait-time"),
        ArgDescriptor::optional("force-lossless"),
        ArgDescriptor::optional("compress-level"),
        ArgDescriptor::optional("quality-level"),
    ];

    /// MySQL/MariaDB protocol arguments
    pub const MYSQL: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::required("username"),
        ArgDescriptor::required("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-csv-export"),
        ArgDescriptor::optional("disable-csv-import"),
    ];

    /// PostgreSQL protocol arguments
    pub const POSTGRESQL: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::required("username"),
        ArgDescriptor::required("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-csv-export"),
        ArgDescriptor::optional("disable-csv-import"),
    ];

    /// SQL Server protocol arguments
    pub const SQLSERVER: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::required("username"),
        ArgDescriptor::required("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-csv-export"),
        ArgDescriptor::optional("disable-csv-import"),
    ];

    /// Redis protocol arguments
    pub const REDIS: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
    ];

    /// MongoDB protocol arguments
    pub const MONGODB: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::optional("username"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("auth-database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
    ];

    /// Oracle protocol arguments
    pub const ORACLE: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::required("username"),
        ArgDescriptor::required("password"),
        ArgDescriptor::optional("database"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-csv-export"),
        ArgDescriptor::optional("disable-csv-import"),
    ];

    /// SFTP protocol arguments
    pub const SFTP: &[ArgDescriptor] = &[
        ArgDescriptor::required("hostname"),
        ArgDescriptor::optional("host-key"),
        ArgDescriptor::optional("port"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::required("username"),
        ArgDescriptor::optional("password"),
        ArgDescriptor::optional("private-key"),
        ArgDescriptor::optional("passphrase"),
        ArgDescriptor::optional("public-key"),
        ArgDescriptor::optional("root-directory"),
        ArgDescriptor::optional("disable-download"),
        ArgDescriptor::optional("disable-upload"),
    ];

    /// HTTP/RBI protocol arguments
    pub const HTTP: &[ArgDescriptor] = &[
        ArgDescriptor::required("url"),
        ArgDescriptor::optional("timeout"),
        ArgDescriptor::optional("read-only"),
        ArgDescriptor::optional("clipboard-buffer-size"),
        ArgDescriptor::optional("disable-copy"),
        ArgDescriptor::optional("disable-paste"),
        ArgDescriptor::optional("disable-download"),
        ArgDescriptor::optional("disable-upload"),
        ArgDescriptor::optional("disable-print"),
        ArgDescriptor::optional("url-allowlist"),
        ArgDescriptor::optional("url-blocklist"),
    ];
}

/// Get argument descriptors for a protocol by name
pub fn get_protocol_args(protocol: &str) -> Option<&'static [ArgDescriptor]> {
    match protocol {
        "ssh" => Some(protocol_args::SSH),
        "telnet" => Some(protocol_args::TELNET),
        "rdp" => Some(protocol_args::RDP),
        "vnc" => Some(protocol_args::VNC),
        "mysql" | "mariadb" => Some(protocol_args::MYSQL),
        "postgresql" | "postgres" => Some(protocol_args::POSTGRESQL),
        "sqlserver" | "mssql" => Some(protocol_args::SQLSERVER),
        "redis" => Some(protocol_args::REDIS),
        "mongodb" | "mongo" => Some(protocol_args::MONGODB),
        "oracle" => Some(protocol_args::ORACLE),
        "sftp" => Some(protocol_args::SFTP),
        "http" | "https" | "rbi" => Some(protocol_args::HTTP),
        _ => None,
    }
}

/// Get argument names for a protocol (for the args instruction)
pub fn get_protocol_arg_names(protocol: &str) -> Vec<&'static str> {
    get_protocol_args(protocol)
        .map(|args| args.iter().map(|a| a.name).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_protocol_args() {
        assert!(get_protocol_args("ssh").is_some());
        assert!(get_protocol_args("rdp").is_some());
        assert!(get_protocol_args("vnc").is_some());
        assert!(get_protocol_args("mysql").is_some());
        assert!(get_protocol_args("unknown").is_none());
    }

    #[test]
    fn test_get_protocol_arg_names() {
        let ssh_args = get_protocol_arg_names("ssh");
        assert!(ssh_args.contains(&"hostname"));
        assert!(ssh_args.contains(&"username"));
        assert!(ssh_args.contains(&"password"));

        let rdp_args = get_protocol_arg_names("rdp");
        assert!(rdp_args.contains(&"hostname"));
        assert!(rdp_args.contains(&"domain"));
    }

    #[test]
    fn test_arg_descriptor() {
        let required = ArgDescriptor::required("hostname");
        assert!(required.required);
        assert_eq!(required.name, "hostname");

        let optional = ArgDescriptor::optional("port");
        assert!(!optional.required);
        assert_eq!(optional.name, "port");
    }
}
