//! RDP connection settings
//!
//! Parses connection parameters from the Guacamole protocol into
//! FreeRDP settings structures.

use crate::error::{RdpError, Result};
use crate::CLIPBOARD_DEFAULT_SIZE;
use std::collections::HashMap;

/// RDP connection settings parsed from connection parameters
#[derive(Debug, Clone)]
pub struct RdpSettings {
    // Connection
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: Option<String>,

    // Display
    pub width: u32,
    pub height: u32,
    pub color_depth: u8,
    pub dpi: u32,

    // Security
    pub security_mode: SecurityMode,
    pub ignore_certificate: bool,

    // Features
    pub disable_audio: bool,
    pub enable_printing: bool,
    pub enable_drive: bool,
    pub drive_path: Option<String>,

    // Clipboard
    pub disable_copy: bool,
    pub disable_paste: bool,
    pub clipboard_buffer_size: usize,

    // Performance
    pub enable_wallpaper: bool,
    pub enable_theming: bool,
    pub enable_font_smoothing: bool,
    pub enable_full_window_drag: bool,
    pub enable_desktop_composition: bool,
    pub enable_menu_animations: bool,

    // RemoteApp
    pub remote_app: Option<String>,
    pub remote_app_dir: Option<String>,
    pub remote_app_args: Option<String>,

    // Gateway
    pub gateway_hostname: Option<String>,
    pub gateway_port: Option<u16>,
    pub gateway_username: Option<String>,
    pub gateway_password: Option<String>,
    pub gateway_domain: Option<String>,

    // Resize behavior
    pub resize_method: ResizeMethod,

    // Recording
    pub recording_path: Option<String>,
    pub recording_name: Option<String>,
}

/// RDP security mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecurityMode {
    /// Any security mode (auto-negotiate)
    Any,
    /// Network Level Authentication (most secure, default)
    #[default]
    Nla,
    /// TLS security
    Tls,
    /// RDP security (legacy, least secure)
    Rdp,
    /// Extended NLA
    NlaExt,
}

impl SecurityMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "any" => SecurityMode::Any,
            "nla" => SecurityMode::Nla,
            "tls" => SecurityMode::Tls,
            "rdp" => SecurityMode::Rdp,
            "nla-ext" | "nlaext" => SecurityMode::NlaExt,
            _ => SecurityMode::Nla,
        }
    }
}

/// How to handle display resize requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeMethod {
    /// No resize support
    #[default]
    None,
    /// Reconnect on resize
    Reconnect,
    /// Use display update channel (requires RDP 8.1+)
    DisplayUpdate,
}

impl ResizeMethod {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "reconnect" => ResizeMethod::Reconnect,
            "display-update" | "displayupdate" => ResizeMethod::DisplayUpdate,
            _ => ResizeMethod::None,
        }
    }
}

impl Default for RdpSettings {
    fn default() -> Self {
        Self {
            hostname: String::new(),
            port: 3389,
            username: String::new(),
            password: String::new(),
            domain: None,

            width: 1024,
            height: 768,
            color_depth: 32,
            dpi: 96,

            security_mode: SecurityMode::Nla,
            ignore_certificate: false,

            disable_audio: false,
            enable_printing: false,
            enable_drive: false,
            drive_path: None,

            disable_copy: false,
            disable_paste: false,
            clipboard_buffer_size: CLIPBOARD_DEFAULT_SIZE,

            enable_wallpaper: false,
            enable_theming: true,
            enable_font_smoothing: true,
            enable_full_window_drag: false,
            enable_desktop_composition: false,
            enable_menu_animations: false,

            remote_app: None,
            remote_app_dir: None,
            remote_app_args: None,

            gateway_hostname: None,
            gateway_port: None,
            gateway_username: None,
            gateway_password: None,
            gateway_domain: None,

            resize_method: ResizeMethod::None,

            recording_path: None,
            recording_name: None,
        }
    }
}

impl RdpSettings {
    /// Parse RDP settings from connection parameters
    ///
    /// Required parameters:
    /// - `hostname`: Target RDP server hostname/IP
    /// - `username`: Username for authentication
    /// - `password`: Password for authentication
    ///
    /// Optional parameters:
    /// - `port`: RDP port (default: 3389)
    /// - `domain`: Windows domain
    /// - `width`, `height`: Display dimensions
    /// - `dpi`: Display DPI
    /// - `security`: Security mode (any, nla, tls, rdp)
    /// - `ignore-cert`: Ignore certificate errors (true/false)
    /// - And many more...
    pub fn from_params(params: &HashMap<String, String>) -> Result<Self> {
        let hostname = params
            .get("hostname")
            .ok_or_else(|| RdpError::InvalidSettings("Missing hostname".to_string()))?
            .clone();

        let username = params
            .get("username")
            .ok_or_else(|| RdpError::InvalidSettings("Missing username".to_string()))?
            .clone();

        let password = params
            .get("password")
            .ok_or_else(|| RdpError::InvalidSettings("Missing password".to_string()))?
            .clone();

        let mut settings = Self {
            hostname,
            username,
            password,
            ..Default::default()
        };

        // Parse optional parameters
        if let Some(port) = params.get("port") {
            settings.port = port.parse().unwrap_or(3389);
        }

        if let Some(domain) = params.get("domain") {
            settings.domain = Some(domain.clone());
        }

        // Display settings - support "size" format or separate width/height
        if let Some(size) = params.get("size") {
            // Format: "width,height,dpi" or "width,height"
            let parts: Vec<&str> = size.split(',').collect();
            if parts.len() >= 2 {
                settings.width = parts[0].parse().unwrap_or(1024);
                settings.height = parts[1].parse().unwrap_or(768);
                if parts.len() >= 3 {
                    settings.dpi = parts[2].parse().unwrap_or(96);
                }
            }
        } else {
            if let Some(width) = params.get("width") {
                settings.width = width.parse().unwrap_or(1024);
            }
            if let Some(height) = params.get("height") {
                settings.height = height.parse().unwrap_or(768);
            }
        }

        if let Some(dpi) = params.get("dpi") {
            settings.dpi = dpi.parse().unwrap_or(96);
        }

        if let Some(depth) = params.get("color-depth") {
            settings.color_depth = depth.parse().unwrap_or(32);
        }

        // Security
        if let Some(security) = params.get("security") {
            settings.security_mode = SecurityMode::from_str(security);
        }

        if let Some(ignore) = params.get("ignore-cert") {
            settings.ignore_certificate = ignore == "true" || ignore == "1";
        }

        // Features
        if let Some(audio) = params.get("disable-audio") {
            settings.disable_audio = audio == "true" || audio == "1";
        }

        if let Some(printing) = params.get("enable-printing") {
            settings.enable_printing = printing == "true" || printing == "1";
        }

        if let Some(drive) = params.get("enable-drive") {
            settings.enable_drive = drive == "true" || drive == "1";
        }

        if let Some(path) = params.get("drive-path") {
            settings.drive_path = Some(path.clone());
        }

        // Clipboard
        if let Some(copy) = params.get("disable-copy") {
            settings.disable_copy = copy == "true" || copy == "1";
        }

        if let Some(paste) = params.get("disable-paste") {
            settings.disable_paste = paste == "true" || paste == "1";
        }

        // Performance flags
        if let Some(v) = params.get("enable-wallpaper") {
            settings.enable_wallpaper = v == "true" || v == "1";
        }
        if let Some(v) = params.get("enable-theming") {
            settings.enable_theming = v == "true" || v == "1";
        }
        if let Some(v) = params.get("enable-font-smoothing") {
            settings.enable_font_smoothing = v == "true" || v == "1";
        }
        if let Some(v) = params.get("enable-full-window-drag") {
            settings.enable_full_window_drag = v == "true" || v == "1";
        }
        if let Some(v) = params.get("enable-desktop-composition") {
            settings.enable_desktop_composition = v == "true" || v == "1";
        }
        if let Some(v) = params.get("enable-menu-animations") {
            settings.enable_menu_animations = v == "true" || v == "1";
        }

        // RemoteApp
        if let Some(app) = params.get("remote-app") {
            settings.remote_app = Some(app.clone());
        }
        if let Some(dir) = params.get("remote-app-dir") {
            settings.remote_app_dir = Some(dir.clone());
        }
        if let Some(args) = params.get("remote-app-args") {
            settings.remote_app_args = Some(args.clone());
        }

        // Gateway
        if let Some(host) = params.get("gateway-hostname") {
            settings.gateway_hostname = Some(host.clone());
        }
        if let Some(port) = params.get("gateway-port") {
            settings.gateway_port = port.parse().ok();
        }
        if let Some(user) = params.get("gateway-username") {
            settings.gateway_username = Some(user.clone());
        }
        if let Some(pass) = params.get("gateway-password") {
            settings.gateway_password = Some(pass.clone());
        }
        if let Some(domain) = params.get("gateway-domain") {
            settings.gateway_domain = Some(domain.clone());
        }

        // Resize
        if let Some(method) = params.get("resize-method") {
            settings.resize_method = ResizeMethod::from_str(method);
        }

        // Recording
        if let Some(path) = params.get("recording-path") {
            settings.recording_path = Some(path.clone());
        }
        if let Some(name) = params.get("recording-name") {
            settings.recording_name = Some(name.clone());
        }

        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn test_required_params() {
        let params = make_params(&[
            ("hostname", "server.example.com"),
            ("username", "admin"),
            ("password", "secret123"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.hostname, "server.example.com");
        assert_eq!(settings.username, "admin");
        assert_eq!(settings.password, "secret123");
        assert_eq!(settings.port, 3389); // default
    }

    #[test]
    fn test_missing_hostname() {
        let params = make_params(&[("username", "admin"), ("password", "secret")]);

        let result = RdpSettings::from_params(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("hostname"));
    }

    #[test]
    fn test_missing_username() {
        let params = make_params(&[("hostname", "server"), ("password", "secret")]);

        let result = RdpSettings::from_params(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("username"));
    }

    #[test]
    fn test_missing_password() {
        let params = make_params(&[("hostname", "server"), ("username", "admin")]);

        let result = RdpSettings::from_params(&params);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("password"));
    }

    #[test]
    fn test_port_parsing() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("port", "3390"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.port, 3390);
    }

    #[test]
    fn test_invalid_port_uses_default() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("port", "invalid"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.port, 3389);
    }

    #[test]
    fn test_size_format_parsing() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("size", "1920,1080,144"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.width, 1920);
        assert_eq!(settings.height, 1080);
        assert_eq!(settings.dpi, 144);
    }

    #[test]
    fn test_size_format_without_dpi() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("size", "1280,720"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.width, 1280);
        assert_eq!(settings.height, 720);
        assert_eq!(settings.dpi, 96); // default
    }

    #[test]
    fn test_separate_width_height() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("width", "2560"),
            ("height", "1440"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.width, 2560);
        assert_eq!(settings.height, 1440);
    }

    #[test]
    fn test_security_modes() {
        assert_eq!(SecurityMode::from_str("any"), SecurityMode::Any);
        assert_eq!(SecurityMode::from_str("nla"), SecurityMode::Nla);
        assert_eq!(SecurityMode::from_str("NLA"), SecurityMode::Nla);
        assert_eq!(SecurityMode::from_str("tls"), SecurityMode::Tls);
        assert_eq!(SecurityMode::from_str("rdp"), SecurityMode::Rdp);
        assert_eq!(SecurityMode::from_str("nla-ext"), SecurityMode::NlaExt);
        assert_eq!(SecurityMode::from_str("nlaext"), SecurityMode::NlaExt);
        assert_eq!(SecurityMode::from_str("unknown"), SecurityMode::Nla); // default
    }

    #[test]
    fn test_resize_methods() {
        assert_eq!(ResizeMethod::from_str("none"), ResizeMethod::None);
        assert_eq!(ResizeMethod::from_str("reconnect"), ResizeMethod::Reconnect);
        assert_eq!(
            ResizeMethod::from_str("display-update"),
            ResizeMethod::DisplayUpdate
        );
        assert_eq!(
            ResizeMethod::from_str("displayupdate"),
            ResizeMethod::DisplayUpdate
        );
        assert_eq!(ResizeMethod::from_str("unknown"), ResizeMethod::None);
    }

    #[test]
    fn test_boolean_params() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("ignore-cert", "true"),
            ("disable-audio", "1"),
            ("enable-printing", "true"),
            ("disable-copy", "true"),
            ("disable-paste", "1"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert!(settings.ignore_certificate);
        assert!(settings.disable_audio);
        assert!(settings.enable_printing);
        assert!(settings.disable_copy);
        assert!(settings.disable_paste);
    }

    #[test]
    fn test_domain() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("domain", "CONTOSO"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.domain, Some("CONTOSO".to_string()));
    }

    #[test]
    fn test_gateway_settings() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("gateway-hostname", "gateway.example.com"),
            ("gateway-port", "443"),
            ("gateway-username", "gwuser"),
            ("gateway-password", "gwpass"),
            ("gateway-domain", "GWDOMAIN"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(
            settings.gateway_hostname,
            Some("gateway.example.com".to_string())
        );
        assert_eq!(settings.gateway_port, Some(443));
        assert_eq!(settings.gateway_username, Some("gwuser".to_string()));
        assert_eq!(settings.gateway_password, Some("gwpass".to_string()));
        assert_eq!(settings.gateway_domain, Some("GWDOMAIN".to_string()));
    }

    #[test]
    fn test_remote_app() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("remote-app", "||notepad"),
            ("remote-app-dir", "C:\\Windows"),
            ("remote-app-args", "test.txt"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert_eq!(settings.remote_app, Some("||notepad".to_string()));
        assert_eq!(settings.remote_app_dir, Some("C:\\Windows".to_string()));
        assert_eq!(settings.remote_app_args, Some("test.txt".to_string()));
    }

    #[test]
    fn test_performance_flags() {
        let params = make_params(&[
            ("hostname", "server"),
            ("username", "admin"),
            ("password", "secret"),
            ("enable-wallpaper", "true"),
            ("enable-theming", "false"),
            ("enable-font-smoothing", "1"),
            ("enable-desktop-composition", "true"),
        ]);

        let settings = RdpSettings::from_params(&params).unwrap();
        assert!(settings.enable_wallpaper);
        assert!(!settings.enable_theming); // explicitly set to false
        assert!(settings.enable_font_smoothing);
        assert!(settings.enable_desktop_composition);
    }

    #[test]
    fn test_default_values() {
        let settings = RdpSettings::default();
        assert_eq!(settings.port, 3389);
        assert_eq!(settings.width, 1024);
        assert_eq!(settings.height, 768);
        assert_eq!(settings.color_depth, 32);
        assert_eq!(settings.dpi, 96);
        assert_eq!(settings.security_mode, SecurityMode::Nla);
        assert!(!settings.ignore_certificate);
        assert!(!settings.disable_audio);
        assert!(!settings.disable_copy);
        assert!(!settings.disable_paste);
        assert!(settings.enable_theming);
        assert!(settings.enable_font_smoothing);
        assert!(!settings.enable_wallpaper);
    }
}
