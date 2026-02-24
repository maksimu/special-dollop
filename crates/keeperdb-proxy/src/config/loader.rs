//! Configuration loader

use super::Config;
use crate::error::{ProxyError, Result};
use std::path::Path;

/// Load configuration from a YAML file
///
/// Also applies KEEPER_GATEWAY_DBPROXY_* env var overrides after loading.
pub fn load_config(path: &Path) -> Result<Config> {
    let contents = std::fs::read_to_string(path)?;
    let mut config: Config = serde_yaml::from_str(&contents)?;
    resolve_config_env_vars(&mut config);
    apply_env_overrides(&mut config);
    config.validate().map_err(ProxyError::Config)?;
    Ok(config)
}

/// Load configuration from a YAML string (useful for testing)
///
/// Also applies KEEPER_GATEWAY_DBPROXY_* env var overrides after loading.
pub fn load_config_from_str(yaml: &str) -> Result<Config> {
    let mut config: Config = serde_yaml::from_str(yaml)?;
    resolve_config_env_vars(&mut config);
    apply_env_overrides(&mut config);
    config.validate().map_err(ProxyError::Config)?;
    Ok(config)
}

/// Apply KEEPER_GATEWAY_DBPROXY_* environment variable overrides to a config.
///
/// This follows the pam-rustwebrtc convention where each component reads
/// KEEPER_GATEWAY_* prefixed env vars. Any set env var overrides the
/// corresponding config value.
///
/// Supported env vars:
/// - `KEEPER_GATEWAY_DBPROXY_LISTEN_ADDRESS` - Override listen address
/// - `KEEPER_GATEWAY_DBPROXY_LISTEN_PORT` - Override listen port
/// - `KEEPER_GATEWAY_DBPROXY_TARGET_HOST` - Override target host (single-target mode)
/// - `KEEPER_GATEWAY_DBPROXY_TARGET_PORT` - Override target port (single-target mode)
/// - `KEEPER_GATEWAY_DBPROXY_LOG_LEVEL` - Override log level
/// - `KEEPER_GATEWAY_DBPROXY_CONNECT_TIMEOUT_SECS` - Override connect timeout
/// - `KEEPER_GATEWAY_DBPROXY_IDLE_TIMEOUT_SECS` - Override idle timeout
/// - `KEEPER_GATEWAY_DBPROXY_MAX_CONNECTIONS` - Override max connections
pub fn apply_env_overrides(config: &mut Config) {
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_LISTEN_ADDRESS") {
        debug!("Overriding listen_address from KEEPER_GATEWAY_DBPROXY_LISTEN_ADDRESS");
        config.server.listen_address = val;
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_LISTEN_PORT") {
        if let Ok(port) = val.parse::<u16>() {
            debug!("Overriding listen_port from KEEPER_GATEWAY_DBPROXY_LISTEN_PORT");
            config.server.listen_port = port;
        }
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_TARGET_HOST") {
        if let Some(ref mut target) = config.target {
            debug!("Overriding target host from KEEPER_GATEWAY_DBPROXY_TARGET_HOST");
            target.host = val;
        }
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_TARGET_PORT") {
        if let Ok(port) = val.parse::<u16>() {
            if let Some(ref mut target) = config.target {
                debug!("Overriding target port from KEEPER_GATEWAY_DBPROXY_TARGET_PORT");
                target.port = port;
            }
        }
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_LOG_LEVEL") {
        debug!("Overriding log level from KEEPER_GATEWAY_DBPROXY_LOG_LEVEL");
        config.logging.level = val;
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_CONNECT_TIMEOUT_SECS") {
        if let Ok(secs) = val.parse::<u64>() {
            debug!("Overriding connect_timeout from KEEPER_GATEWAY_DBPROXY_CONNECT_TIMEOUT_SECS");
            config.server.connect_timeout_secs = secs;
        }
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_IDLE_TIMEOUT_SECS") {
        if let Ok(secs) = val.parse::<u64>() {
            debug!("Overriding idle_timeout from KEEPER_GATEWAY_DBPROXY_IDLE_TIMEOUT_SECS");
            config.server.idle_timeout_secs = secs;
        }
    }
    if let Ok(val) = std::env::var("KEEPER_GATEWAY_DBPROXY_MAX_CONNECTIONS") {
        if let Ok(max) = val.parse::<usize>() {
            debug!("Overriding max_connections from KEEPER_GATEWAY_DBPROXY_MAX_CONNECTIONS");
            config.server.max_connections = max;
        }
    }
}

/// Resolve environment variables in a string value
///
/// Supports two syntaxes:
/// - `${VAR_NAME}` - curly brace syntax
/// - `$VAR_NAME` - simple syntax (for single variable values)
///
/// If the environment variable is not set, the original value is preserved.
fn resolve_env_var(value: &str) -> String {
    // Handle ${VAR_NAME} syntax
    if value.starts_with("${") && value.ends_with('}') {
        let var_name = &value[2..value.len() - 1];
        return match std::env::var(var_name) {
            Ok(env_value) => {
                debug!("Resolved env var {} from config", var_name);
                env_value
            }
            Err(_) => {
                debug!("Env var {} not set, keeping original value", var_name);
                value.to_string()
            }
        };
    }

    // Handle $VAR_NAME syntax (whole value must be the variable reference)
    if value.starts_with('$') && !value.contains(' ') && value.len() > 1 {
        let var_name = &value[1..];
        return match std::env::var(var_name) {
            Ok(env_value) => {
                debug!("Resolved env var {} from config", var_name);
                env_value
            }
            Err(_) => {
                debug!("Env var {} not set, keeping original value", var_name);
                value.to_string()
            }
        };
    }

    value.to_string()
}

/// Resolve environment variables in all config fields that support it
fn resolve_config_env_vars(config: &mut Config) {
    // Resolve multi-target credentials
    for target in config.targets.values_mut() {
        target.credentials.username = resolve_env_var(&target.credentials.username);
        target.credentials.password = resolve_env_var(&target.credentials.password);
        target.host = resolve_env_var(&target.host);
        if let Some(ref db) = target.database {
            target.database = Some(resolve_env_var(db));
        }
    }

    // Resolve single-target credentials (backwards compatible mode)
    if let Some(ref mut creds) = config.credentials {
        creds.username = resolve_env_var(&creds.username);
        creds.password = resolve_env_var(&creds.password);
    }

    // Also resolve single-target host/database in case those need to be dynamic
    if let Some(ref mut target) = config.target {
        target.host = resolve_env_var(&target.host);
        if let Some(ref db) = target.database {
            target.database = Some(resolve_env_var(db));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_config_minimal_single_target() {
        let yaml = r#"
server:
  listen_port: 3307

target:
  host: localhost
  port: 3306

credentials:
  username: root
  password: secret
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.server.listen_port, 3307);
        assert_eq!(config.server.listen_address, "127.0.0.1"); // default
        let target = config.target.as_ref().unwrap();
        assert_eq!(target.host, "localhost");
        assert_eq!(target.port, 3306);
        let creds = config.credentials.as_ref().unwrap();
        assert_eq!(creds.username, "root");
        assert_eq!(creds.password, "secret");
    }

    #[test]
    fn test_load_config_multi_target() {
        let yaml = r#"
server:
  listen_port: 3307

targets:
  mysql:
    host: mysql.example.com
    port: 3306
    database: testdb
    credentials:
      username: mysqluser
      password: mysqlpass
  postgresql:
    host: postgres.example.com
    port: 5432
    database: testdb
    credentials:
      username: pguser
      password: pgpass
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.server.listen_port, 3307);
        assert!(config.is_multi_target());

        // Check MySQL target
        let mysql = config.get_target("mysql").unwrap();
        assert_eq!(mysql.host, "mysql.example.com");
        assert_eq!(mysql.port, 3306);
        assert_eq!(mysql.database, Some("testdb".to_string()));
        assert_eq!(mysql.credentials.username, "mysqluser");
        assert_eq!(mysql.credentials.password, "mysqlpass");

        // Check PostgreSQL target
        let pg = config.get_target("postgresql").unwrap();
        assert_eq!(pg.host, "postgres.example.com");
        assert_eq!(pg.port, 5432);
        assert_eq!(pg.credentials.username, "pguser");
        assert_eq!(pg.credentials.password, "pgpass");
    }

    #[test]
    fn test_load_config_full_single_target() {
        let yaml = r#"
server:
  listen_address: "0.0.0.0"
  listen_port: 3307
  connect_timeout_secs: 60
  idle_timeout_secs: 600

target:
  host: db.example.com
  port: 3306
  database: mydb

credentials:
  username: admin
  password: supersecret

logging:
  level: debug
  protocol_debug: true
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.server.listen_address, "0.0.0.0");
        assert_eq!(config.server.listen_port, 3307);
        assert_eq!(config.server.connect_timeout_secs, 60);
        assert_eq!(config.server.idle_timeout_secs, 600);
        let target = config.target.as_ref().unwrap();
        assert_eq!(target.host, "db.example.com");
        assert_eq!(target.database, Some("mydb".to_string()));
        assert_eq!(config.logging.level, "debug");
        assert!(config.logging.protocol_debug);
    }

    #[test]
    fn test_load_config_gateway_mode_valid() {
        // Gateway mode: target without credentials is now valid
        // (credentials come from Gateway handshake at runtime)
        let yaml = r#"
server:
  listen_port: 3307

target:
  host: localhost
  port: 3306
"#;
        // This should succeed as gateway mode is valid
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.server.listen_port, 3307);
        // Config should report no credentials (gateway mode)
        assert!(!config.has_credentials());
    }

    #[test]
    fn test_load_config_pure_gateway_mode() {
        // Pure gateway mode: no target or credentials at all
        // Just the server config - valid for gateway mode
        let yaml = r#"
server:
  listen_port: 3307
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.server.listen_port, 3307);
        assert!(!config.has_credentials());
    }

    #[test]
    fn test_load_config_invalid_target_protocol() {
        let yaml = r#"
server:
  listen_port: 3307

targets:
  oracle:  # Invalid protocol name
    host: localhost
    port: 1521
    credentials:
      username: user
      password: pass
"#;
        let err = load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("Invalid target protocol"));
    }

    #[test]
    fn test_load_config_defaults() {
        let yaml = r#"
server:
  listen_port: 3307

target:
  host: localhost
  port: 3306

credentials:
  username: root
  password: ""
"#;
        let config = load_config_from_str(yaml).unwrap();
        // Check defaults
        assert_eq!(config.server.listen_address, "127.0.0.1");
        assert_eq!(config.server.connect_timeout_secs, 30);
        assert_eq!(config.server.idle_timeout_secs, 300);
        assert_eq!(config.logging.level, "info");
        assert!(!config.logging.protocol_debug);
    }

    #[test]
    fn test_resolve_env_var_curly_brace_syntax() {
        // Set test env var
        std::env::set_var("TEST_DB_PASSWORD", "env_secret_123");

        let result = resolve_env_var("${TEST_DB_PASSWORD}");
        assert_eq!(result, "env_secret_123");

        // Clean up
        std::env::remove_var("TEST_DB_PASSWORD");
    }

    #[test]
    fn test_resolve_env_var_simple_syntax() {
        // Set test env var
        std::env::set_var("TEST_DB_USER", "env_user");

        let result = resolve_env_var("$TEST_DB_USER");
        assert_eq!(result, "env_user");

        // Clean up
        std::env::remove_var("TEST_DB_USER");
    }

    #[test]
    fn test_resolve_env_var_not_set() {
        // Ensure env var is not set
        std::env::remove_var("NONEXISTENT_VAR_12345");

        // Should return original value when env var not set
        let result = resolve_env_var("${NONEXISTENT_VAR_12345}");
        assert_eq!(result, "${NONEXISTENT_VAR_12345}");

        let result = resolve_env_var("$NONEXISTENT_VAR_12345");
        assert_eq!(result, "$NONEXISTENT_VAR_12345");
    }

    #[test]
    fn test_resolve_env_var_plain_value() {
        // Plain values should pass through unchanged
        let result = resolve_env_var("plain_password");
        assert_eq!(result, "plain_password");

        let result = resolve_env_var("root");
        assert_eq!(result, "root");
    }

    #[test]
    fn test_load_config_with_env_vars() {
        // Set test env vars
        std::env::set_var("TEST_PROXY_USER", "admin_from_env");
        std::env::set_var("TEST_PROXY_PASS", "secret_from_env");

        let yaml = r#"
server:
  listen_port: 3307

target:
  host: localhost
  port: 3306

credentials:
  username: "${TEST_PROXY_USER}"
  password: "${TEST_PROXY_PASS}"
"#;
        let config = load_config_from_str(yaml).unwrap();
        let creds = config.credentials.as_ref().unwrap();
        assert_eq!(creds.username, "admin_from_env");
        assert_eq!(creds.password, "secret_from_env");

        // Clean up
        std::env::remove_var("TEST_PROXY_USER");
        std::env::remove_var("TEST_PROXY_PASS");
    }

    #[test]
    fn test_load_config_with_env_vars_multi_target() {
        // Set test env vars
        std::env::set_var("TEST_MYSQL_USER", "mysql_from_env");
        std::env::set_var("TEST_PG_PASS", "pg_pass_from_env");

        let yaml = r#"
server:
  listen_port: 3307

targets:
  mysql:
    host: localhost
    port: 3306
    credentials:
      username: "${TEST_MYSQL_USER}"
      password: mysqlpass
  postgresql:
    host: localhost
    port: 5432
    credentials:
      username: postgres
      password: "${TEST_PG_PASS}"
"#;
        let config = load_config_from_str(yaml).unwrap();

        let mysql = config.get_target("mysql").unwrap();
        assert_eq!(mysql.credentials.username, "mysql_from_env");

        let pg = config.get_target("postgresql").unwrap();
        assert_eq!(pg.credentials.password, "pg_pass_from_env");

        // Clean up
        std::env::remove_var("TEST_MYSQL_USER");
        std::env::remove_var("TEST_PG_PASS");
    }

    #[test]
    fn test_get_single_target_helper() {
        let yaml = r#"
server:
  listen_port: 3307

target:
  host: localhost
  port: 3306

credentials:
  username: root
  password: secret
"#;
        let config = load_config_from_str(yaml).unwrap();
        let combined = config.get_single_target().unwrap();
        assert_eq!(combined.host, "localhost");
        assert_eq!(combined.port, 3306);
        assert_eq!(combined.credentials.username, "root");
        assert_eq!(combined.credentials.password, "secret");
    }
}
