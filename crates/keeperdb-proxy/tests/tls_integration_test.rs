//! TLS Integration Tests
//!
//! These tests verify TLS functionality at the integration level.
//! Note: Full end-to-end TLS tests require actual certificates and
//! a TLS-enabled MySQL server, which would typically be done in CI
//! with a proper test infrastructure.

use keeperdb_proxy::{TlsAcceptor, TlsClientConfig, TlsConnector, TlsServerConfig, TlsVerifyMode};
use std::path::PathBuf;

/// Test that TLS configuration can be loaded from YAML
#[test]
fn test_tls_config_yaml_parsing() {
    let yaml = r#"
server:
  listen_port: 3307
  tls:
    enabled: true
    cert_path: "/path/to/cert.pem"
    key_path: "/path/to/key.pem"

target:
  host: "localhost"
  port: 3306
  tls:
    enabled: true
    verify_mode: "verify"
    ca_path: "/path/to/ca.crt"

credentials:
  username: "user"
  password: "pass"
"#;

    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();

    // Verify server TLS config
    assert!(config.server.tls.enabled);
    assert_eq!(
        config.server.tls.cert_path,
        Some(PathBuf::from("/path/to/cert.pem"))
    );
    assert_eq!(
        config.server.tls.key_path,
        Some(PathBuf::from("/path/to/key.pem"))
    );

    // Verify client TLS config
    let target = config.target.as_ref().expect("target should be present");
    assert!(target.tls.enabled);
    assert_eq!(target.tls.verify_mode, TlsVerifyMode::Verify);
    assert_eq!(target.tls.ca_path, Some(PathBuf::from("/path/to/ca.crt")));
}

/// Test that disabled TLS can be explicitly configured
#[test]
fn test_tls_disabled_config() {
    let yaml = r#"
server:
  listen_port: 3307
  tls:
    enabled: false

target:
  host: "localhost"
  port: 3306
  tls:
    enabled: false

credentials:
  username: "user"
  password: "pass"
"#;

    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();

    assert!(!config.server.tls.enabled);
    let target = config.target.as_ref().expect("target should be present");
    assert!(!target.tls.enabled);
}

/// Test that TLS defaults are applied correctly
#[test]
fn test_tls_default_config() {
    let yaml = r#"
server:
  listen_port: 3307

target:
  host: "localhost"
  port: 3306

credentials:
  username: "user"
  password: "pass"
"#;

    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();

    // TLS should be disabled by default
    assert!(!config.server.tls.enabled);
    let target = config.target.as_ref().expect("target should be present");
    assert!(!target.tls.enabled);

    // Verify mode should default to Verify
    assert_eq!(target.tls.verify_mode, TlsVerifyMode::Verify);
}

/// Test all TLS verify modes can be parsed
#[test]
fn test_tls_verify_modes() {
    // Test "verify" mode
    let yaml = r#"
target:
  host: "localhost"
  port: 3306
  tls:
    enabled: true
    verify_mode: "verify"
credentials:
  username: "u"
  password: "p"
server:
  listen_port: 3307
"#;
    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();
    let target = config.target.as_ref().expect("target should be present");
    assert_eq!(target.tls.verify_mode, TlsVerifyMode::Verify);

    // Test "verify_ca" mode
    let yaml = r#"
target:
  host: "localhost"
  port: 3306
  tls:
    enabled: true
    verify_mode: "verify_ca"
credentials:
  username: "u"
  password: "p"
server:
  listen_port: 3307
"#;
    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();
    let target = config.target.as_ref().expect("target should be present");
    assert_eq!(target.tls.verify_mode, TlsVerifyMode::VerifyCa);

    // Test "none" mode
    let yaml = r#"
target:
  host: "localhost"
  port: 3306
  tls:
    enabled: true
    verify_mode: "none"
credentials:
  username: "u"
  password: "p"
server:
  listen_port: 3307
"#;
    let config: keeperdb_proxy::Config = serde_yaml::from_str(yaml).unwrap();
    let target = config.target.as_ref().expect("target should be present");
    assert_eq!(target.tls.verify_mode, TlsVerifyMode::None);
}

/// Test TLS acceptor validation without valid certs
#[test]
fn test_tls_acceptor_validation() {
    // Missing cert_path should fail
    let config = TlsServerConfig {
        enabled: true,
        cert_path: None,
        key_path: Some(PathBuf::from("/key.pem")),
        key_password: None,
    };
    let result = TlsAcceptor::new(&config);
    assert!(result.is_err());

    // Missing key_path should fail
    let config = TlsServerConfig {
        enabled: true,
        cert_path: Some(PathBuf::from("/cert.pem")),
        key_path: None,
        key_password: None,
    };
    let result = TlsAcceptor::new(&config);
    assert!(result.is_err());
}

/// Test TLS connector validation
#[test]
fn test_tls_connector_validation() {
    // Partial client cert config should fail
    let config = TlsClientConfig {
        enabled: true,
        verify_mode: TlsVerifyMode::Verify,
        ca_path: None,
        client_cert_path: Some(PathBuf::from("/client.pem")),
        client_key_path: None, // Missing key
    };
    let result = TlsConnector::new(&config);
    assert!(result.is_err());
}

/// Test insecure TLS connector can be created
#[test]
fn test_tls_connector_insecure() {
    let result = TlsConnector::new_insecure();
    assert!(result.is_ok());
}

/// Test CLIENT_SSL capability flag
#[test]
fn test_client_ssl_capability() {
    use keeperdb_proxy::protocol::mysql::packets::CLIENT_SSL;

    // Verify the flag value matches MySQL protocol spec
    assert_eq!(CLIENT_SSL, 0x0000_0800);
}
