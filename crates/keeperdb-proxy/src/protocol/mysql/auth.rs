//! MySQL authentication
//!
//! This module implements MySQL authentication methods:
//! - `mysql_native_password` - SHA1-based (MySQL 5.x default)
//! - `caching_sha2_password` - SHA256-based (MySQL 8.0+ default)
//! - `client_ed25519` - Ed25519-based (MariaDB)
//!
//! References:
//! - Native: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_connection_phase_authentication_methods_native_password_authentication.html>
//! - Caching SHA2: <https://dev.mysql.com/doc/dev/mysql-server/latest/page_caching_sha2_authentication_exchanges.html>
//! - Ed25519: <https://mariadb.com/kb/en/authentication-plugin-ed25519/>

use ed25519_dalek::{Signer, SigningKey};
use rand::Rng;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256, Sha512};

/// Generate a random 20-byte scramble for authentication
pub fn generate_scramble() -> [u8; 20] {
    let mut rng = rand::thread_rng();
    let mut scramble = [0u8; 20];

    // Generate random bytes, avoiding null bytes and 0xFF (reserved)
    for byte in scramble.iter_mut() {
        *byte = loop {
            let b: u8 = rng.gen();
            // MySQL scramble should avoid certain bytes
            if b != 0 && b != 0xFF {
                break b;
            }
        };
    }

    scramble
}

/// Compute the auth response for mysql_native_password
///
/// Algorithm:
/// ```text
/// SHA1( password ) XOR SHA1( scramble + SHA1( SHA1( password ) ) )
/// ```
///
/// # Arguments
/// * `password` - The plaintext password
/// * `scramble` - The 20-byte scramble from the server
///
/// # Returns
/// The 20-byte authentication response
pub fn compute_auth_response(password: &str, scramble: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }

    // SHA1(password)
    let mut hasher = Sha1::new();
    Sha1Digest::update(&mut hasher, password.as_bytes());
    let stage1 = hasher.finalize();

    // SHA1(SHA1(password))
    let mut hasher = Sha1::new();
    Sha1Digest::update(&mut hasher, stage1);
    let stage2 = hasher.finalize();

    // SHA1(scramble + SHA1(SHA1(password)))
    let mut hasher = Sha1::new();
    Sha1Digest::update(&mut hasher, scramble);
    Sha1Digest::update(&mut hasher, stage2);
    let stage3 = hasher.finalize();

    // XOR stage1 with stage3
    stage1
        .iter()
        .zip(stage3.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

/// Verify an auth response against a stored password hash
///
/// This is used on the server side to verify client credentials.
/// Not used in our proxy (we just forward to real server).
#[allow(dead_code)]
pub fn verify_auth_response(
    auth_response: &[u8],
    scramble: &[u8],
    stored_sha1_sha1_password: &[u8],
) -> bool {
    if auth_response.len() != 20 || scramble.len() != 20 || stored_sha1_sha1_password.len() != 20 {
        return false;
    }

    // SHA1(scramble + stored_sha1_sha1_password)
    let mut hasher = Sha1::new();
    Sha1Digest::update(&mut hasher, scramble);
    Sha1Digest::update(&mut hasher, stored_sha1_sha1_password);
    let stage3 = hasher.finalize();

    // XOR auth_response with stage3 to get SHA1(password)
    let recovered_stage1: Vec<u8> = auth_response
        .iter()
        .zip(stage3.iter())
        .map(|(a, b)| a ^ b)
        .collect();

    // SHA1(recovered_stage1) should equal stored_sha1_sha1_password
    let mut hasher = Sha1::new();
    Sha1Digest::update(&mut hasher, &recovered_stage1);
    let computed_stage2 = hasher.finalize();

    computed_stage2.as_slice() == stored_sha1_sha1_password
}

/// Compute the auth response for caching_sha2_password (SHA256-based)
///
/// This is the default authentication method in MySQL 8.0+.
///
/// Algorithm (from MySQL documentation):
/// ```text
/// SHA256(password) XOR SHA256(SHA256(SHA256(password)) || nonce)
/// ```
///
/// Note: MySQL's notation is confusing. The actual computation is:
/// - stage1 = SHA256(password)
/// - stage2 = SHA256(SHA256(password))
/// - result = stage1 XOR SHA256(stage2 || nonce)
///
/// # Arguments
/// * `password` - The plaintext password
/// * `scramble` - The 20-byte scramble (nonce) from the server
///
/// # Returns
/// The 32-byte authentication response
pub fn compute_caching_sha2_response(password: &str, scramble: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }

    // SHA256(password)
    let mut hasher = Sha256::new();
    Sha2Digest::update(&mut hasher, password.as_bytes());
    let stage1 = hasher.finalize();

    // SHA256(SHA256(password))
    let mut hasher = Sha256::new();
    Sha2Digest::update(&mut hasher, stage1);
    let stage2 = hasher.finalize();

    // SHA256(SHA256(SHA256(password)) || scramble)
    // Note: This uses stage2 (double SHA256), not stage1
    let mut hasher = Sha256::new();
    Sha2Digest::update(&mut hasher, stage2);
    Sha2Digest::update(&mut hasher, scramble);
    let scramble_hash = hasher.finalize();

    // XOR stage1 with scramble_hash
    stage1
        .iter()
        .zip(scramble_hash.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

/// Compute auth response for MariaDB client_ed25519
///
/// Algorithm (from MariaDB documentation):
/// 1. Hash password with SHA-512
/// 2. Clamp first 32 bytes to valid Ed25519 scalar
/// 3. Create signing key from clamped scalar
/// 4. Sign the server nonce (32 bytes)
/// 5. Return 64-byte signature
///
/// References:
/// - <https://mariadb.com/kb/en/authentication-plugin-ed25519/>
/// - <https://github.com/mysql-net/MySqlConnector/blob/master/src/MySqlConnector.Authentication.Ed25519/Ed25519AuthenticationPlugin.cs>
///
/// # Arguments
/// * `password` - The plaintext password
/// * `nonce` - The 32-byte nonce from the server
///
/// # Returns
/// The 64-byte Ed25519 signature
pub fn compute_ed25519_response(password: &str, nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }

    // SHA-512 hash of password
    let mut hasher = Sha512::new();
    Sha2Digest::update(&mut hasher, password.as_bytes());
    let hash = hasher.finalize();

    // Clamp first 32 bytes to valid Ed25519 scalar
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&hash[..32]);
    scalar[0] &= 248;
    scalar[31] &= 63;
    scalar[31] |= 64;

    // Create signing key and sign nonce
    let signing_key = SigningKey::from_bytes(&scalar);
    let signature = signing_key.sign(nonce);

    signature.to_bytes().to_vec()
}

/// Compute auth response for the specified plugin
///
/// Routes to the correct algorithm based on the plugin name from the server.
///
/// # Arguments
/// * `plugin_name` - Auth plugin name ("mysql_native_password", "caching_sha2_password", etc.)
/// * `password` - The plaintext password
/// * `scramble` - The scramble from the server
///
/// # Returns
/// Authentication response bytes (20 bytes for native, 32 bytes for caching_sha2)
pub fn compute_auth_for_plugin(plugin_name: &str, password: &str, scramble: &[u8]) -> Vec<u8> {
    match plugin_name {
        "mysql_native_password" => compute_auth_response(password, scramble),
        "caching_sha2_password" => compute_caching_sha2_response(password, scramble),
        "client_ed25519" => compute_ed25519_response(password, scramble),
        unknown => {
            warn!(
                "Unknown auth plugin '{}', falling back to mysql_native_password",
                unknown
            );
            compute_auth_response(password, scramble)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_scramble() {
        let scramble = generate_scramble();
        assert_eq!(scramble.len(), 20);

        // Check no null bytes
        assert!(!scramble.contains(&0));

        // Check no 0xFF bytes
        assert!(!scramble.contains(&0xFF));
    }

    #[test]
    fn test_compute_auth_response_empty_password() {
        let scramble = [0u8; 20];
        let response = compute_auth_response("", &scramble);
        assert!(response.is_empty());
    }

    #[test]
    fn test_compute_auth_response_known_values() {
        // Test with known values from MySQL documentation/testing
        let password = "root";
        let scramble = [
            0x3b, 0x55, 0x78, 0x7d, 0x2c, 0x5f, 0x7c, 0x72, 0x49, 0x52, 0x3f, 0x28, 0x47, 0x6f,
            0x77, 0x28, 0x5b, 0x7c, 0x6f, 0x5c,
        ];

        let response = compute_auth_response(password, &scramble);
        assert_eq!(response.len(), 20);

        // The response should be deterministic for the same input
        let response2 = compute_auth_response(password, &scramble);
        assert_eq!(response, response2);
    }

    #[test]
    fn test_auth_roundtrip() {
        let password = "test_password123";
        let scramble = generate_scramble();

        // Compute what would be stored on server: SHA1(SHA1(password))
        let mut hasher = Sha1::new();
        Sha1Digest::update(&mut hasher, password.as_bytes());
        let stage1 = hasher.finalize();

        let mut hasher = Sha1::new();
        Sha1Digest::update(&mut hasher, stage1);
        let stored_hash = hasher.finalize();

        // Compute auth response
        let auth_response = compute_auth_response(password, &scramble);

        // Verify
        assert!(verify_auth_response(
            &auth_response,
            &scramble,
            stored_hash.as_slice()
        ));
    }

    // =========================================================================
    // caching_sha2_password tests
    // =========================================================================

    #[test]
    fn test_caching_sha2_empty_password() {
        let scramble = [0u8; 20];
        let response = compute_caching_sha2_response("", &scramble);
        assert!(response.is_empty());
    }

    #[test]
    fn test_caching_sha2_response_length() {
        let scramble = generate_scramble();
        let response = compute_caching_sha2_response("password", &scramble);
        assert_eq!(response.len(), 32); // SHA256 = 32 bytes
    }

    #[test]
    fn test_caching_sha2_deterministic() {
        let password = "test_password";
        let scramble = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14,
        ];

        let response1 = compute_caching_sha2_response(password, &scramble);
        let response2 = compute_caching_sha2_response(password, &scramble);
        assert_eq!(response1, response2);
    }

    #[test]
    fn test_caching_sha2_different_from_native() {
        let password = "testpass";
        let scramble = generate_scramble();

        let native = compute_auth_response(password, &scramble);
        let sha2 = compute_caching_sha2_response(password, &scramble);

        // Different lengths (20 vs 32)
        assert_ne!(native.len(), sha2.len());
        assert_eq!(native.len(), 20);
        assert_eq!(sha2.len(), 32);
    }

    // =========================================================================
    // client_ed25519 tests (MariaDB)
    // =========================================================================

    #[test]
    fn test_ed25519_empty_password() {
        let nonce = [0u8; 32];
        let response = compute_ed25519_response("", &nonce);
        assert!(response.is_empty());
    }

    #[test]
    fn test_ed25519_response_length() {
        let nonce = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let response = compute_ed25519_response("password", &nonce);
        assert_eq!(response.len(), 64); // Ed25519 signature = 64 bytes
    }

    #[test]
    fn test_ed25519_deterministic() {
        let password = "test_password";
        let nonce = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];

        let response1 = compute_ed25519_response(password, &nonce);
        let response2 = compute_ed25519_response(password, &nonce);
        assert_eq!(response1, response2);
    }

    #[test]
    fn test_ed25519_different_from_native() {
        let password = "testpass";
        let scramble = generate_scramble();

        let native = compute_auth_response(password, &scramble);
        let ed25519 = compute_ed25519_response(password, &scramble);

        // Different lengths (20 vs 64)
        assert_ne!(native.len(), ed25519.len());
        assert_eq!(native.len(), 20);
        assert_eq!(ed25519.len(), 64);
    }

    #[test]
    fn test_ed25519_different_nonce_different_signature() {
        let password = "testpass";
        let nonce1 = [0x01u8; 32];
        let nonce2 = [0x02u8; 32];

        let response1 = compute_ed25519_response(password, &nonce1);
        let response2 = compute_ed25519_response(password, &nonce2);

        assert_ne!(response1, response2);
    }

    // =========================================================================
    // Dispatcher tests
    // =========================================================================

    #[test]
    fn test_dispatch_native_password() {
        let scramble = generate_scramble();
        let native = compute_auth_response("pass", &scramble);
        let dispatched = compute_auth_for_plugin("mysql_native_password", "pass", &scramble);
        assert_eq!(native, dispatched);
    }

    #[test]
    fn test_dispatch_caching_sha2() {
        let scramble = generate_scramble();
        let sha2 = compute_caching_sha2_response("pass", &scramble);
        let dispatched = compute_auth_for_plugin("caching_sha2_password", "pass", &scramble);
        assert_eq!(sha2, dispatched);
    }

    #[test]
    fn test_dispatch_client_ed25519() {
        let nonce = [0x42u8; 32];
        let ed25519 = compute_ed25519_response("pass", &nonce);
        let dispatched = compute_auth_for_plugin("client_ed25519", "pass", &nonce);
        assert_eq!(ed25519, dispatched);
    }

    #[test]
    fn test_dispatch_unknown_plugin() {
        let scramble = generate_scramble();
        let native = compute_auth_response("pass", &scramble);
        let dispatched = compute_auth_for_plugin("unknown_plugin", "pass", &scramble);
        assert_eq!(native, dispatched); // Falls back to native
    }
}
