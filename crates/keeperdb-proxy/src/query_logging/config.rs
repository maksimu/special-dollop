use serde::Deserialize;

/// Top-level query logging configuration.
///
/// Added to the root `Config` struct under the `query_logging` key.
/// When absent or `enabled: false`, query logging is completely disabled
/// with zero runtime overhead.
///
/// In pipe-based mode (gateway), log entries are written as JSON Lines to
/// a named pipe (FIFO) created by the Python Gateway. The Python side
/// handles encryption and upload.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryLoggingConfig {
    /// Master enable/disable switch.
    #[serde(default)]
    pub enabled: bool,

    /// Whether to capture SQL query text in log entries.
    /// When false, only metadata (command type, duration, etc.) is logged.
    #[serde(default = "default_true")]
    pub include_query_text: bool,

    /// Maximum query text length before truncation.
    #[serde(default = "default_max_query_length")]
    pub max_query_length: usize,

    /// Pipe path for query log output (standalone/non-gateway mode).
    /// In gateway mode, the pipe path comes from the handshake.
    pub pipe_path: Option<String>,
}

impl Default for QueryLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            include_query_text: true,
            max_query_length: default_max_query_length(),
            pipe_path: None,
        }
    }
}

/// Per-connection logging config, delivered via Gateway handshake.
/// Overrides global defaults on a per-tunnel basis.
#[derive(Debug, Clone)]
pub struct ConnectionLoggingConfig {
    pub query_logging_enabled: bool,
    pub include_query_text: bool,
    pub max_query_length: usize,
    pub pipe_path: Option<String>,
}

impl Default for ConnectionLoggingConfig {
    fn default() -> Self {
        Self {
            query_logging_enabled: false,
            include_query_text: true,
            max_query_length: default_max_query_length(),
            pipe_path: None,
        }
    }
}

impl ConnectionLoggingConfig {
    /// Convert to `Option<Self>`, returning `Some(self)` only if query logging is enabled.
    ///
    /// This is used in protocol handlers to short-circuit when logging is disabled,
    /// avoiding unnecessary `QueryLogContext` creation.
    pub fn into_enabled(self) -> Option<Self> {
        if self.query_logging_enabled {
            Some(self)
        } else {
            None
        }
    }

    /// Create a per-connection config from global defaults.
    pub fn from_global(global: &QueryLoggingConfig) -> Self {
        Self {
            query_logging_enabled: global.enabled,
            include_query_text: global.include_query_text,
            max_query_length: global.max_query_length,
            pipe_path: global.pipe_path.clone(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_query_length() -> usize {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_disabled() {
        let config = QueryLoggingConfig::default();
        assert!(!config.enabled);
        assert!(config.include_query_text);
        assert_eq!(config.max_query_length, 10_000);
        assert!(config.pipe_path.is_none());
    }

    #[test]
    fn test_yaml_deserialization_minimal() {
        let yaml = "enabled: true";
        let config: QueryLoggingConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert!(config.include_query_text); // default true
        assert_eq!(config.max_query_length, 10_000); // default
    }

    #[test]
    fn test_yaml_deserialization_full() {
        let yaml = r#"
enabled: true
include_query_text: false
max_query_length: 5000
pipe_path: /tmp/query.pipe
"#;
        let config: QueryLoggingConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert!(!config.include_query_text);
        assert_eq!(config.max_query_length, 5000);
        assert_eq!(config.pipe_path.as_deref(), Some("/tmp/query.pipe"));
    }

    #[test]
    fn test_connection_logging_from_global() {
        let global = QueryLoggingConfig {
            enabled: true,
            include_query_text: false,
            max_query_length: 2000,
            pipe_path: Some("/tmp/query.pipe".to_string()),
        };

        let conn = ConnectionLoggingConfig::from_global(&global);
        assert!(conn.query_logging_enabled);
        assert!(!conn.include_query_text);
        assert_eq!(conn.max_query_length, 2000);
        assert_eq!(conn.pipe_path.as_deref(), Some("/tmp/query.pipe"));
    }

    #[test]
    fn test_into_enabled_when_enabled() {
        let config = ConnectionLoggingConfig {
            query_logging_enabled: true,
            ..Default::default()
        };
        assert!(config.into_enabled().is_some());
    }

    #[test]
    fn test_into_enabled_when_disabled() {
        let config = ConnectionLoggingConfig::default();
        assert!(!config.query_logging_enabled);
        assert!(config.into_enabled().is_none());
    }
}
