// Host key verification for SSH/SFTP connections
//
// Implements known_hosts file parsing and host key verification to protect
// against MITM attacks. Supports the standard OpenSSH known_hosts format.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Host key verification configuration
#[derive(Debug, Clone, Default)]
pub struct HostKeyConfig {
    /// Whether to skip host key verification entirely (INSECURE)
    pub ignore_host_key: bool,
    /// Path to known_hosts file (default: none, must be explicitly provided)
    pub known_hosts_path: Option<String>,
    /// Whether to allow connections to hosts not in known_hosts
    pub allow_unknown_hosts: bool,
    /// Expected host key fingerprint (SHA256 base64) for pinning
    pub host_key_fingerprint: Option<String>,
}

impl HostKeyConfig {
    /// Parse host key configuration from connection parameters
    ///
    /// Supported parameters:
    /// - `ignore-host-key`: "true" to skip verification (INSECURE)
    /// - `known-hosts`: Path to known_hosts file
    /// - `allow-unknown-hosts`: "true" to allow unknown hosts
    /// - `host-key`: Expected SHA256 fingerprint (base64)
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let ignore_host_key = params
            .get("ignore-host-key")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let known_hosts_path = params.get("known-hosts").cloned();

        let allow_unknown_hosts = params
            .get("allow-unknown-hosts")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let host_key_fingerprint = params.get("host-key").cloned();

        Self {
            ignore_host_key,
            known_hosts_path,
            allow_unknown_hosts,
            host_key_fingerprint,
        }
    }

    /// Check if any verification is configured
    pub fn has_verification(&self) -> bool {
        !self.ignore_host_key
            && (self.known_hosts_path.is_some() || self.host_key_fingerprint.is_some())
    }
}

/// Result of host key verification
#[derive(Debug, Clone, PartialEq)]
pub enum HostKeyResult {
    /// Host key matches known_hosts entry
    Verified,
    /// Host key doesn't match known entry (MITM attack possible)
    Mismatch { expected: String, actual: String },
    /// Host not found in known_hosts (new host)
    UnknownHost,
    /// Verification skipped (ignore-host-key=true)
    Skipped,
    /// No verification configured
    NotConfigured,
}

impl HostKeyResult {
    /// Whether the connection should be allowed
    pub fn is_allowed(&self, config: &HostKeyConfig) -> bool {
        match self {
            HostKeyResult::Verified => true,
            HostKeyResult::Skipped => true,
            HostKeyResult::NotConfigured => true, // Allow if no verification configured
            HostKeyResult::UnknownHost => config.allow_unknown_hosts,
            HostKeyResult::Mismatch { .. } => false, // Never allow mismatches
        }
    }

    /// Get a human-readable error message for denied connections
    pub fn error_message(&self) -> Option<String> {
        match self {
            HostKeyResult::Mismatch { expected, actual } => Some(format!(
                "HOST KEY VERIFICATION FAILED!\n\
                 The host key has changed. This could indicate a MITM attack.\n\
                 Expected fingerprint: {}\n\
                 Received fingerprint: {}\n\
                 Connection refused for security.",
                expected, actual
            )),
            HostKeyResult::UnknownHost => Some(
                "Host key verification failed: Host not found in known_hosts.\n\
                 Add the host to known_hosts or set allow-unknown-hosts=true."
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// Host key verifier
///
/// Verifies SSH host keys against known_hosts entries or pinned fingerprints.
pub struct HostKeyVerifier {
    /// Configuration for host key verification
    pub config: HostKeyConfig,
    known_hosts: HashMap<String, Vec<KnownHost>>,
}

/// A single entry from a known_hosts file
#[derive(Debug, Clone)]
struct KnownHost {
    /// Key type (ssh-rsa, ssh-ed25519, ecdsa-sha2-nistp256, etc.)
    key_type: String,
    /// Base64-encoded public key
    key_data: String,
}

impl HostKeyVerifier {
    /// Create a new host key verifier from configuration
    pub fn new(config: HostKeyConfig) -> Self {
        let known_hosts_path = config.known_hosts_path.clone();

        let mut verifier = Self {
            config,
            known_hosts: HashMap::new(),
        };

        // Load known_hosts if configured
        if let Some(path) = known_hosts_path {
            if let Err(e) = verifier.load_known_hosts(&path) {
                warn!("Failed to load known_hosts from {}: {}", path, e);
            }
        }

        verifier
    }

    /// Load known_hosts file
    fn load_known_hosts(&mut self, path: &str) -> Result<(), String> {
        let path = Path::new(path);
        if !path.exists() {
            return Err(format!("File not found: {}", path.display()));
        }

        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let mut count = 0;
        for line in content.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse known_hosts line format: hostname[,hostname...] keytype base64key [comment]
            // Also handles hashed hostnames: |1|salt|hash keytype base64key
            if let Some(entry) = self.parse_known_hosts_line(line) {
                for host in entry.0 {
                    self.known_hosts
                        .entry(host)
                        .or_default()
                        .push(entry.1.clone());
                    count += 1;
                }
            }
        }

        info!("Loaded {} host key entries from {}", count, path.display());
        Ok(())
    }

    /// Parse a single known_hosts line
    fn parse_known_hosts_line(&self, line: &str) -> Option<(Vec<String>, KnownHost)> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }

        let hostnames_part = parts[0];
        let key_type = parts[1].to_string();
        let key_data = parts[2].to_string();

        // Check if this is a hashed hostname (starts with |1|)
        let hostnames = if hostnames_part.starts_with("|1|") {
            // Hashed hostname - we can't reverse it, store as-is for hash comparison
            vec![hostnames_part.to_string()]
        } else {
            // Plain hostnames, possibly comma-separated, possibly with [host]:port format
            hostnames_part.split(',').map(normalize_hostname).collect()
        };

        Some((hostnames, KnownHost { key_type, key_data }))
    }

    /// Verify a host key
    ///
    /// # Arguments
    /// * `hostname` - The hostname being connected to
    /// * `port` - The port (used for non-standard ports in known_hosts)
    /// * `key_type` - SSH key type (e.g., "ssh-ed25519", "ssh-rsa")
    /// * `key_data` - Raw public key bytes
    pub fn verify(
        &self,
        hostname: &str,
        port: u16,
        key_type: &str,
        key_data: &[u8],
    ) -> HostKeyResult {
        // Check if verification is disabled
        if self.config.ignore_host_key {
            debug!("Host key verification skipped (ignore-host-key=true)");
            return HostKeyResult::Skipped;
        }

        // Calculate fingerprint
        let fingerprint = calculate_fingerprint(key_data);
        debug!(
            "Host key fingerprint for {}:{}: SHA256:{}",
            hostname, port, fingerprint
        );

        // Check pinned fingerprint first
        if let Some(ref expected_fp) = self.config.host_key_fingerprint {
            // Normalize both fingerprints for comparison
            let expected_normalized = expected_fp
                .trim_start_matches("SHA256:")
                .trim_start_matches("sha256:");
            if fingerprint == expected_normalized {
                info!(
                    "Host key verified via pinned fingerprint for {}:{}",
                    hostname, port
                );
                return HostKeyResult::Verified;
            } else {
                return HostKeyResult::Mismatch {
                    expected: format!("SHA256:{}", expected_normalized),
                    actual: format!("SHA256:{}", fingerprint),
                };
            }
        }

        // Check known_hosts
        if self.known_hosts.is_empty() {
            if self.config.known_hosts_path.is_some() {
                // known_hosts was configured but empty/failed to load
                return HostKeyResult::UnknownHost;
            }
            return HostKeyResult::NotConfigured;
        }

        // Try different hostname formats
        let lookup_keys = generate_hostname_variants(hostname, port);

        for lookup_key in &lookup_keys {
            if let Some(entries) = self.known_hosts.get(lookup_key) {
                for entry in entries {
                    // Check if key type matches
                    if entry.key_type != key_type {
                        continue;
                    }

                    // Compare key data
                    let known_key_data = match BASE64.decode(&entry.key_data) {
                        Ok(data) => data,
                        Err(_) => continue,
                    };

                    if known_key_data == key_data {
                        info!(
                            "Host key verified via known_hosts for {}:{}",
                            hostname, port
                        );
                        return HostKeyResult::Verified;
                    } else {
                        // Key type matches but data differs - potential MITM
                        let known_fp = calculate_fingerprint(&known_key_data);
                        return HostKeyResult::Mismatch {
                            expected: format!("SHA256:{}", known_fp),
                            actual: format!("SHA256:{}", fingerprint),
                        };
                    }
                }
            }

            // Also check hashed entries
            if let Some(result) = self.check_hashed_entries(lookup_key, key_type, key_data) {
                return result;
            }
        }

        HostKeyResult::UnknownHost
    }

    /// Check against hashed known_hosts entries
    fn check_hashed_entries(
        &self,
        hostname: &str,
        key_type: &str,
        key_data: &[u8],
    ) -> Option<HostKeyResult> {
        for (stored_host, entries) in &self.known_hosts {
            if !stored_host.starts_with("|1|") {
                continue;
            }

            // Parse hashed hostname format: |1|base64salt|base64hash
            let parts: Vec<&str> = stored_host.split('|').collect();
            if parts.len() != 4 {
                continue;
            }

            let salt = match BASE64.decode(parts[2]) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let stored_hash = match BASE64.decode(parts[3]) {
                Ok(h) => h,
                Err(_) => continue,
            };

            // Compute HMAC-SHA1 of hostname with salt
            use hmac::{Hmac, Mac};
            use sha1::Sha1;

            type HmacSha1 = Hmac<Sha1>;
            let mut mac = match HmacSha1::new_from_slice(&salt) {
                Ok(m) => m,
                Err(_) => continue,
            };
            mac.update(hostname.as_bytes());
            let computed_hash = mac.finalize().into_bytes();

            if computed_hash.as_slice() == stored_hash {
                // Found matching hashed hostname
                for entry in entries {
                    if entry.key_type != key_type {
                        continue;
                    }

                    let known_key_data = match BASE64.decode(&entry.key_data) {
                        Ok(data) => data,
                        Err(_) => continue,
                    };

                    if known_key_data == key_data {
                        info!("Host key verified via hashed known_hosts entry");
                        return Some(HostKeyResult::Verified);
                    } else {
                        let known_fp = calculate_fingerprint(&known_key_data);
                        let actual_fp = calculate_fingerprint(key_data);
                        return Some(HostKeyResult::Mismatch {
                            expected: format!("SHA256:{}", known_fp),
                            actual: format!("SHA256:{}", actual_fp),
                        });
                    }
                }
            }
        }
        None
    }
}

/// Calculate SHA256 fingerprint of a public key (base64 encoded)
pub fn calculate_fingerprint(key_data: &[u8]) -> String {
    let hash = Sha256::digest(key_data);
    BASE64.encode(hash).trim_end_matches('=').to_string()
}

/// Normalize hostname from known_hosts format
fn normalize_hostname(host: &str) -> String {
    // Handle [host]:port format
    if host.starts_with('[') {
        if let Some(end_bracket) = host.find(']') {
            return host[1..end_bracket].to_string();
        }
    }
    host.to_string()
}

/// Generate hostname variants for lookup
fn generate_hostname_variants(hostname: &str, port: u16) -> Vec<String> {
    let mut variants = vec![hostname.to_string()];

    // Add [hostname]:port format for non-standard ports
    if port != 22 {
        variants.push(format!("[{}]:{}", hostname, port));
    }

    // Add lowercase variant
    let lower = hostname.to_lowercase();
    if lower != hostname {
        variants.push(lower.clone());
        if port != 22 {
            variants.push(format!("[{}]:{}", lower, port));
        }
    }

    variants
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_key_config_default() {
        let config = HostKeyConfig::default();
        assert!(!config.ignore_host_key);
        assert!(config.known_hosts_path.is_none());
        assert!(!config.allow_unknown_hosts);
        assert!(config.host_key_fingerprint.is_none());
    }

    #[test]
    fn test_host_key_config_from_params() {
        let mut params = HashMap::new();
        params.insert("ignore-host-key".to_string(), "true".to_string());
        params.insert(
            "known-hosts".to_string(),
            "/path/to/known_hosts".to_string(),
        );
        params.insert("allow-unknown-hosts".to_string(), "true".to_string());
        params.insert("host-key".to_string(), "AAAA...".to_string());

        let config = HostKeyConfig::from_params(&params);
        assert!(config.ignore_host_key);
        assert_eq!(
            config.known_hosts_path,
            Some("/path/to/known_hosts".to_string())
        );
        assert!(config.allow_unknown_hosts);
        assert_eq!(config.host_key_fingerprint, Some("AAAA...".to_string()));
    }

    #[test]
    fn test_verify_skipped() {
        let config = HostKeyConfig {
            ignore_host_key: true,
            ..Default::default()
        };
        let verifier = HostKeyVerifier::new(config.clone());

        let result = verifier.verify("example.com", 22, "ssh-ed25519", b"key_data");
        assert_eq!(result, HostKeyResult::Skipped);
        assert!(result.is_allowed(&config));
    }

    #[test]
    fn test_verify_not_configured() {
        let config = HostKeyConfig::default();
        let verifier = HostKeyVerifier::new(config.clone());

        let result = verifier.verify("example.com", 22, "ssh-ed25519", b"key_data");
        assert_eq!(result, HostKeyResult::NotConfigured);
        assert!(result.is_allowed(&config));
    }

    #[test]
    fn test_fingerprint_calculation() {
        let key_data = b"test_key_data";
        let fp = calculate_fingerprint(key_data);
        // Should be base64 SHA256 without trailing =
        assert!(!fp.contains('='));
        assert!(!fp.is_empty());
    }

    #[test]
    fn test_hostname_normalization() {
        assert_eq!(normalize_hostname("example.com"), "example.com");
        assert_eq!(normalize_hostname("[example.com]:2222"), "example.com");
        assert_eq!(normalize_hostname("[192.168.1.1]:22"), "192.168.1.1");
    }

    #[test]
    fn test_hostname_variants() {
        let variants = generate_hostname_variants("Example.COM", 22);
        assert!(variants.contains(&"Example.COM".to_string()));
        assert!(variants.contains(&"example.com".to_string()));
        assert!(!variants.iter().any(|v| v.contains(":22")));

        let variants_nonstandard = generate_hostname_variants("Example.COM", 2222);
        assert!(variants_nonstandard.contains(&"[Example.COM]:2222".to_string()));
        assert!(variants_nonstandard.contains(&"[example.com]:2222".to_string()));
    }

    #[test]
    fn test_result_error_messages() {
        let mismatch = HostKeyResult::Mismatch {
            expected: "SHA256:abc".to_string(),
            actual: "SHA256:xyz".to_string(),
        };
        assert!(mismatch.error_message().is_some());
        assert!(mismatch.error_message().unwrap().contains("MITM"));

        let unknown = HostKeyResult::UnknownHost;
        assert!(unknown.error_message().is_some());

        let verified = HostKeyResult::Verified;
        assert!(verified.error_message().is_none());
    }

    #[test]
    fn test_pinned_fingerprint_verification() {
        let key_data = b"test_key_data_12345";
        let fingerprint = calculate_fingerprint(key_data);

        let config = HostKeyConfig {
            host_key_fingerprint: Some(fingerprint.clone()),
            ..Default::default()
        };
        let verifier = HostKeyVerifier::new(config.clone());

        // Should verify with matching fingerprint
        let result = verifier.verify("example.com", 22, "ssh-ed25519", key_data);
        assert_eq!(result, HostKeyResult::Verified);

        // Should fail with different key
        let result2 = verifier.verify("example.com", 22, "ssh-ed25519", b"different_key");
        assert!(matches!(result2, HostKeyResult::Mismatch { .. }));
    }
}
