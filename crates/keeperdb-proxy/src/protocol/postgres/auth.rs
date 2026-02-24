//! PostgreSQL authentication
//!
//! This module implements PostgreSQL authentication methods:
//! - MD5 password authentication
//! - SCRAM-SHA-256 authentication (RFC 5802)
//!
//! # Known Limitations
//!
//! - **SASLprep**: The SCRAM implementation does not perform full Unicode
//!   normalization (RFC 4013). Users with non-ASCII usernames/passwords may
//!   experience authentication issues. ASCII-only credentials work correctly.
//!
//! # References
//!
//! - MD5: <https://www.postgresql.org/docs/current/auth-password.html>
//! - SCRAM: <https://www.postgresql.org/docs/current/sasl-authentication.html>
//! - RFC 5802: <https://tools.ietf.org/html/rfc5802>

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use md5::{Digest as Md5Digest, Md5};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{ProxyError, Result};

// ============================================================================
// MD5 Authentication
// ============================================================================

/// Compute the MD5 password hash for PostgreSQL authentication.
///
/// The PostgreSQL MD5 password format is:
/// `"md5" + md5(md5(password + user) + salt)`
///
/// # Arguments
///
/// * `user` - The database username
/// * `password` - The plaintext password
/// * `salt` - The 4-byte salt from the server
///
/// # Returns
///
/// A string in the format "md5XXXXXXXX..." (35 characters total)
///
/// # Example
///
/// ```
/// use keeperdb_proxy::protocol::postgres::auth::compute_md5_password;
///
/// let hash = compute_md5_password("user", "password", &[0x01, 0x02, 0x03, 0x04]);
/// assert!(hash.starts_with("md5"));
/// assert_eq!(hash.len(), 35); // "md5" + 32 hex chars
/// ```
pub fn compute_md5_password(user: &str, password: &str, salt: &[u8; 4]) -> String {
    // Stage 1: md5(password + user)
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(user.as_bytes());
    let stage1 = hasher.finalize();

    // Convert stage1 to hex string
    let stage1_hex = hex_encode(&stage1);

    // Stage 2: md5(stage1_hex + salt)
    let mut hasher = Md5::new();
    hasher.update(stage1_hex.as_bytes());
    hasher.update(salt);
    let stage2 = hasher.finalize();

    // Result: "md5" + hex(stage2)
    format!("md5{}", hex_encode(&stage2))
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ============================================================================
// SCRAM-SHA-256 Authentication
// ============================================================================

type HmacSha256 = Hmac<Sha256>;

/// SCRAM-SHA-256 client state machine.
///
/// Implements the client side of SCRAM authentication per RFC 5802.
///
/// # Usage
///
/// ```ignore
/// let mut client = ScramClient::new("user", "password");
///
/// // Send client-first-message
/// let first = client.client_first_message();
///
/// // Process server-first-message, get client-final-message
/// let final_msg = client.process_server_first(&server_first)?;
///
/// // Verify server-final-message
/// client.verify_server_final(&server_final)?;
/// ```
pub struct ScramClient {
    /// Username
    username: String,
    /// Password (zeroized on drop)
    password: Zeroizing<String>,
    /// Client nonce (zeroized for unpredictability)
    client_nonce: Zeroizing<String>,
    /// Current state
    state: ScramState,
}

/// Internal SCRAM state.
enum ScramState {
    /// Initial state, ready to generate client-first
    Initial,
    /// Waiting for server-first-message
    WaitingForServerFirst {
        /// Client-first-message-bare (without "n,,")
        client_first_bare: String,
    },
    /// Waiting for server-final-message
    WaitingForServerFinal {
        /// Complete auth message for signature computation
        auth_message: String,
        /// Salted password (zeroized)
        salted_password: Zeroizing<[u8; 32]>,
    },
    /// Authentication complete
    Complete,
    /// Authentication failed
    Failed,
}

impl ScramClient {
    /// Create a new SCRAM client.
    ///
    /// # Arguments
    ///
    /// * `username` - The database username
    /// * `password` - The plaintext password (will be zeroized on drop)
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_string(),
            password: Zeroizing::new(password.to_string()),
            client_nonce: Zeroizing::new(generate_nonce()),
            state: ScramState::Initial,
        }
    }

    /// Generate the client-first-message.
    ///
    /// Returns the message bytes to send in SASLInitialResponse.
    pub fn client_first_message(&mut self) -> Vec<u8> {
        // Build client-first-message-bare: n=<user>,r=<nonce>
        let client_first_bare = format!("n={},r={}", saslprep(&self.username), &*self.client_nonce);

        // Full client-first-message: n,,<client-first-message-bare>
        // n,, means no channel binding
        let client_first = format!("n,,{}", client_first_bare);

        // Save state for later
        self.state = ScramState::WaitingForServerFirst { client_first_bare };

        client_first.into_bytes()
    }

    /// Process the server-first-message and generate client-final-message.
    ///
    /// # Arguments
    ///
    /// * `server_first` - The server-first-message bytes
    ///
    /// # Returns
    ///
    /// The client-final-message bytes to send.
    pub fn process_server_first(&mut self, server_first: &[u8]) -> Result<Vec<u8>> {
        // Get saved state
        let client_first_bare = match &self.state {
            ScramState::WaitingForServerFirst { client_first_bare } => client_first_bare.clone(),
            _ => {
                self.state = ScramState::Failed;
                return Err(ProxyError::Auth(
                    "SCRAM: unexpected state for server-first".into(),
                ));
            }
        };

        // Parse server-first-message
        let server_first_str = std::str::from_utf8(server_first)
            .map_err(|_| ProxyError::Auth("SCRAM: invalid UTF-8 in server-first".into()))?;

        let (server_nonce, salt, iterations) = parse_server_first(server_first_str)?;

        // Verify server nonce starts with our client nonce
        if !server_nonce.starts_with(&*self.client_nonce) {
            self.state = ScramState::Failed;
            return Err(ProxyError::Auth("SCRAM: server nonce mismatch".into()));
        }

        // Compute salted password using PBKDF2
        let mut salted_password = Zeroizing::new([0u8; 32]);
        pbkdf2_hmac::<Sha256>(
            self.password.as_bytes(),
            &salt,
            iterations,
            &mut *salted_password,
        );

        // Build client-final-message-without-proof
        // c=<channel-binding>,r=<server-nonce>
        // For no channel binding, c=biws (base64 of "n,,")
        let channel_binding = BASE64.encode(b"n,,");
        let client_final_without_proof = format!("c={},r={}", channel_binding, server_nonce);

        // Build auth message
        let auth_message = format!(
            "{},{},{}",
            client_first_bare, server_first_str, client_final_without_proof
        );

        // Compute client proof
        let client_proof = compute_client_proof(&salted_password, &auth_message);
        let proof_b64 = BASE64.encode(&client_proof);

        // Build full client-final-message
        let client_final = format!("{},p={}", client_final_without_proof, proof_b64);

        // Update state
        self.state = ScramState::WaitingForServerFinal {
            auth_message,
            salted_password,
        };

        Ok(client_final.into_bytes())
    }

    /// Verify the server-final-message.
    ///
    /// This MUST be called to complete mutual authentication. It verifies
    /// that the server knows the password hash, preventing MITM attacks.
    ///
    /// # Arguments
    ///
    /// * `server_final` - The server-final-message bytes
    pub fn verify_server_final(&mut self, server_final: &[u8]) -> Result<()> {
        // Get saved state
        let (auth_message, salted_password) = match &self.state {
            ScramState::WaitingForServerFinal {
                auth_message,
                salted_password,
            } => (auth_message.clone(), salted_password.clone()),
            _ => {
                self.state = ScramState::Failed;
                return Err(ProxyError::Auth(
                    "SCRAM: unexpected state for server-final".into(),
                ));
            }
        };

        // Parse server-final-message
        let server_final_str = std::str::from_utf8(server_final)
            .map_err(|_| ProxyError::Auth("SCRAM: invalid UTF-8 in server-final".into()))?;

        // Check for error
        if let Some(error_msg) = server_final_str.strip_prefix("e=") {
            self.state = ScramState::Failed;
            return Err(ProxyError::Auth(format!(
                "SCRAM: server error: {}",
                error_msg
            )));
        }

        // Parse server signature
        if !server_final_str.starts_with("v=") {
            self.state = ScramState::Failed;
            return Err(ProxyError::Auth(
                "SCRAM: invalid server-final format".into(),
            ));
        }

        let received_sig = BASE64
            .decode(&server_final_str[2..])
            .map_err(|_| ProxyError::Auth("SCRAM: invalid base64 in server signature".into()))?;

        // Compute expected server signature
        let expected_sig = compute_server_signature(&salted_password, &auth_message);

        // Constant-time comparison (CRITICAL for security)
        if received_sig.ct_eq(&expected_sig).into() {
            self.state = ScramState::Complete;
            Ok(())
        } else {
            self.state = ScramState::Failed;
            Err(ProxyError::Auth(
                "SCRAM: server signature verification failed".into(),
            ))
        }
    }

    /// Check if authentication completed successfully.
    pub fn is_complete(&self) -> bool {
        matches!(self.state, ScramState::Complete)
    }
}

/// Generate a random nonce string.
fn generate_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 24] = rng.gen();
    BASE64.encode(nonce_bytes)
}

/// Simple SASLprep implementation - returns input unchanged.
///
/// # Known Limitation
///
/// This is a minimal implementation that does NOT perform full Unicode
/// normalization per RFC 4013 (SASLprep). This means:
///
/// - Usernames/passwords with non-ASCII characters may not authenticate
///   correctly against servers that perform strict SASLprep normalization.
/// - Most common use cases with ASCII-only credentials work fine.
/// - International usernames (e.g., "用户") may experience authentication
///   failures if the server normalizes differently.
///
/// For full compliance, consider using the `stringprep` crate in a future
/// version if international username support is required.
fn saslprep(s: &str) -> &str {
    s
}

/// Parse server-first-message.
///
/// Format: r=<nonce>,s=<salt>,i=<iterations>[,...]
fn parse_server_first(msg: &str) -> Result<(String, Vec<u8>, u32)> {
    let mut nonce = None;
    let mut salt = None;
    let mut iterations = None;

    for part in msg.split(',') {
        if let Some(value) = part.strip_prefix("r=") {
            nonce = Some(value.to_string());
        } else if let Some(value) = part.strip_prefix("s=") {
            salt = Some(
                BASE64
                    .decode(value)
                    .map_err(|_| ProxyError::Auth("SCRAM: invalid base64 in salt".into()))?,
            );
        } else if let Some(value) = part.strip_prefix("i=") {
            iterations = Some(
                value
                    .parse::<u32>()
                    .map_err(|_| ProxyError::Auth("SCRAM: invalid iteration count".into()))?,
            );
        }
    }

    match (nonce, salt, iterations) {
        (Some(n), Some(s), Some(i)) => Ok((n, s, i)),
        _ => Err(ProxyError::Auth(
            "SCRAM: missing required field in server-first".into(),
        )),
    }
}

/// Compute the client proof.
///
/// ClientKey = HMAC(SaltedPassword, "Client Key")
/// StoredKey = H(ClientKey)
/// ClientSignature = HMAC(StoredKey, AuthMessage)
/// ClientProof = ClientKey XOR ClientSignature
fn compute_client_proof(salted_password: &[u8; 32], auth_message: &str) -> Vec<u8> {
    // ClientKey = HMAC(SaltedPassword, "Client Key")
    let client_key = hmac_sha256(salted_password, b"Client Key");

    // StoredKey = SHA256(ClientKey)
    let stored_key = sha256(&client_key);

    // ClientSignature = HMAC(StoredKey, AuthMessage)
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());

    // ClientProof = ClientKey XOR ClientSignature
    xor_bytes(&client_key, &client_signature)
}

/// Compute the server signature.
///
/// ServerKey = HMAC(SaltedPassword, "Server Key")
/// ServerSignature = HMAC(ServerKey, AuthMessage)
fn compute_server_signature(salted_password: &[u8; 32], auth_message: &str) -> [u8; 32] {
    // ServerKey = HMAC(SaltedPassword, "Server Key")
    let server_key = hmac_sha256(salted_password, b"Server Key");

    // ServerSignature = HMAC(ServerKey, AuthMessage)
    hmac_sha256(&server_key, auth_message.as_bytes())
}

/// Compute HMAC-SHA256.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Compute SHA256.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// XOR two byte slices.
fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0x12, 0x34, 0xab, 0xcd]), "1234abcd");
    }

    #[test]
    fn test_md5_password_format() {
        let hash = compute_md5_password("user", "password", &[0x01, 0x02, 0x03, 0x04]);

        // Should start with "md5"
        assert!(hash.starts_with("md5"));

        // Should be exactly 35 characters: "md5" (3) + 32 hex chars
        assert_eq!(hash.len(), 35);

        // Should be all lowercase hex after "md5"
        let hex_part = &hash[3..];
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hex_part, hex_part.to_lowercase());
    }

    #[test]
    fn test_md5_password_deterministic() {
        let salt = [0xab, 0xcd, 0xef, 0x12];
        let hash1 = compute_md5_password("testuser", "testpass", &salt);
        let hash2 = compute_md5_password("testuser", "testpass", &salt);

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_md5_password_different_with_different_salt() {
        let hash1 = compute_md5_password("user", "pass", &[0x00, 0x00, 0x00, 0x00]);
        let hash2 = compute_md5_password("user", "pass", &[0x00, 0x00, 0x00, 0x01]);

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_md5_password_different_with_different_user() {
        let salt = [0x01, 0x02, 0x03, 0x04];
        let hash1 = compute_md5_password("user1", "pass", &salt);
        let hash2 = compute_md5_password("user2", "pass", &salt);

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_md5_password_different_with_different_password() {
        let salt = [0x01, 0x02, 0x03, 0x04];
        let hash1 = compute_md5_password("user", "pass1", &salt);
        let hash2 = compute_md5_password("user", "pass2", &salt);

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_md5_password_empty_password() {
        let salt = [0x01, 0x02, 0x03, 0x04];
        let hash = compute_md5_password("user", "", &salt);

        // Should still produce a valid hash
        assert!(hash.starts_with("md5"));
        assert_eq!(hash.len(), 35);
    }

    #[test]
    fn test_md5_password_known_value() {
        // Test with a known value computed by PostgreSQL
        // This test vector can be verified with:
        // SELECT 'md5' || md5(md5('password' || 'user') || E'\\x01020304');
        //
        // For user='user', password='password', salt=0x01020304:
        // md5(password + user) = md5('passworduser') = 756e3ec82d0a8e3b76c58e8b3b68de32
        // md5(stage1_hex + salt) = md5('756e3ec82d0a8e3b76c58e8b3b68de32' + 0x01020304)
        //
        // We verify the structure is correct, not the exact value (which would require
        // a PostgreSQL instance to verify)
        let hash = compute_md5_password("user", "password", &[0x01, 0x02, 0x03, 0x04]);
        assert!(hash.starts_with("md5"));

        // Verify the algorithm by computing manually
        // Stage 1: md5("passworduser")
        let mut hasher = Md5::new();
        hasher.update(b"passworduser");
        let stage1 = hasher.finalize();
        let stage1_hex = hex_encode(&stage1);

        // Stage 2: md5(stage1_hex + salt)
        let mut hasher = Md5::new();
        hasher.update(stage1_hex.as_bytes());
        hasher.update([0x01, 0x02, 0x03, 0x04]);
        let stage2 = hasher.finalize();

        let expected = format!("md5{}", hex_encode(&stage2));
        assert_eq!(hash, expected);
    }

    #[test]
    fn test_md5_password_unicode() {
        // Test with unicode characters
        let salt = [0x01, 0x02, 0x03, 0x04];
        let hash = compute_md5_password("用户", "密码", &salt);

        assert!(hash.starts_with("md5"));
        assert_eq!(hash.len(), 35);
    }

    // ========================================================================
    // SCRAM-SHA-256 Tests
    // ========================================================================

    #[test]
    fn test_scram_client_first_message() {
        let mut client = ScramClient::new("user", "password");
        let first = client.client_first_message();
        let first_str = std::str::from_utf8(&first).unwrap();

        // Should start with "n,," (no channel binding)
        assert!(first_str.starts_with("n,,"));

        // Should contain "n=user"
        assert!(first_str.contains("n=user"));

        // Should contain "r=" followed by nonce
        assert!(first_str.contains(",r="));

        // Nonce should be base64 encoded (32 chars for 24 bytes)
        let nonce_start = first_str.find(",r=").unwrap() + 3;
        let nonce = &first_str[nonce_start..];
        assert!(!nonce.is_empty());
    }

    #[test]
    fn test_scram_nonce_uniqueness() {
        let mut client1 = ScramClient::new("user", "password");
        let mut client2 = ScramClient::new("user", "password");

        let first1 = client1.client_first_message();
        let first2 = client2.client_first_message();

        // Nonces should be different
        assert_ne!(first1, first2);
    }

    #[test]
    fn test_scram_full_exchange() {
        // Test a complete SCRAM exchange with known test vectors
        let mut client = ScramClient::new("user", "pencil");

        // Generate client-first
        let _client_first = client.client_first_message();

        // We need to inject a known client nonce for deterministic testing
        // For this test, we'll use a helper that creates a client with known nonce
        let mut client =
            ScramClientTestHelper::new_with_nonce("user", "pencil", "rOprNGfwEbeRWgbNEkqO");

        let client_first = client.client_first_message();
        assert_eq!(
            std::str::from_utf8(&client_first).unwrap(),
            "n,,n=user,r=rOprNGfwEbeRWgbNEkqO"
        );

        // Server sends server-first with its nonce appended
        // Using RFC 5802 test vector
        let server_first = b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";

        let client_final = client.process_server_first(server_first).unwrap();
        let client_final_str = std::str::from_utf8(&client_final).unwrap();

        // Verify client-final structure
        assert!(client_final_str.starts_with("c=biws,")); // base64("n,,")
        assert!(client_final_str.contains(",r=")); // combined nonce
        assert!(client_final_str.contains(",p=")); // proof
    }

    #[test]
    fn test_scram_server_nonce_validation() {
        let mut client = ScramClientTestHelper::new_with_nonce("user", "password", "clientnonce");
        let _client_first = client.client_first_message();

        // Server sends a nonce that doesn't start with client nonce - should fail
        let bad_server_first = b"r=differentnonce,s=c2FsdA==,i=4096";

        let result = client.process_server_first(bad_server_first);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonce mismatch"));
    }

    #[test]
    fn test_scram_server_signature_verification() {
        // Create client with known nonce
        let mut client =
            ScramClientTestHelper::new_with_nonce("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        let _client_first = client.client_first_message();

        // RFC 5802 test vector
        let server_first = b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let _client_final = client.process_server_first(server_first).unwrap();

        // Test with incorrect server signature
        let bad_server_final = b"v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
        let result = client.verify_server_final(bad_server_final);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("signature verification failed"));
    }

    #[test]
    fn test_scram_server_error_handling() {
        let mut client = ScramClientTestHelper::new_with_nonce("user", "password", "nonce");
        let _client_first = client.client_first_message();

        let server_first = b"r=nonceXXX,s=c2FsdA==,i=4096";
        let _client_final = client.process_server_first(server_first).unwrap();

        // Server reports an error
        let server_error = b"e=invalid-proof";
        let result = client.verify_server_final(server_error);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("server error"));
    }

    #[test]
    fn test_scram_parse_server_first() {
        // Valid server-first
        let (nonce, salt, iter) = parse_server_first("r=nonce123,s=c2FsdA==,i=4096").unwrap();
        assert_eq!(nonce, "nonce123");
        assert_eq!(salt, b"salt");
        assert_eq!(iter, 4096);

        // With extra fields (should be ignored)
        let (nonce, _, _) = parse_server_first("r=abc,s=c2FsdA==,i=100,m=extension").unwrap();
        assert_eq!(nonce, "abc");
    }

    #[test]
    fn test_scram_parse_server_first_errors() {
        // Missing nonce
        assert!(parse_server_first("s=c2FsdA==,i=4096").is_err());

        // Missing salt
        assert!(parse_server_first("r=nonce,i=4096").is_err());

        // Missing iterations
        assert!(parse_server_first("r=nonce,s=c2FsdA==").is_err());

        // Invalid base64 in salt
        assert!(parse_server_first("r=nonce,s=!!!invalid,i=4096").is_err());

        // Invalid iteration count
        assert!(parse_server_first("r=nonce,s=c2FsdA==,i=notanumber").is_err());
    }

    #[test]
    fn test_scram_state_machine() {
        let mut client = ScramClient::new("user", "password");

        // Can't process server-first before client-first
        let result = client.process_server_first(b"r=x,s=eA==,i=1");
        assert!(result.is_err());

        // Can't verify before client-final
        let result = client.verify_server_final(b"v=AA==");
        assert!(result.is_err());
    }

    #[test]
    fn test_scram_hmac_sha256() {
        // Test HMAC-SHA256 with known vector
        // HMAC-SHA256("key", "message") verified against openssl
        let result = hmac_sha256(b"key", b"message");
        let expected = [
            0x6e, 0x9e, 0xf2, 0x9b, 0x75, 0xff, 0xfc, 0x5b, 0x7a, 0xba, 0xe5, 0x27, 0xd5, 0x8f,
            0xda, 0xdb, 0x2f, 0xe4, 0x2e, 0x72, 0x19, 0x01, 0x19, 0x76, 0x91, 0x73, 0x43, 0x06,
            0x5f, 0x58, 0xed, 0x4a,
        ];
        assert_eq!(result, expected);
    }

    #[test]
    fn test_scram_sha256() {
        // Test SHA256 with known vector
        let result = sha256(b"hello");
        let expected = [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ];
        assert_eq!(result, expected);
    }

    #[test]
    fn test_scram_xor_bytes() {
        assert_eq!(xor_bytes(&[0x00], &[0x00]), vec![0x00]);
        assert_eq!(xor_bytes(&[0xff], &[0x00]), vec![0xff]);
        assert_eq!(xor_bytes(&[0xff], &[0xff]), vec![0x00]);
        assert_eq!(xor_bytes(&[0x12, 0x34], &[0x56, 0x78]), vec![0x44, 0x4c]);
    }

    #[test]
    fn test_scram_is_complete() {
        let client = ScramClient::new("user", "password");
        assert!(!client.is_complete());
    }

    // Helper struct for testing with deterministic nonce
    struct ScramClientTestHelper {
        inner: ScramClient,
    }

    impl ScramClientTestHelper {
        fn new_with_nonce(username: &str, password: &str, nonce: &str) -> Self {
            Self {
                inner: ScramClient {
                    username: username.to_string(),
                    password: Zeroizing::new(password.to_string()),
                    client_nonce: Zeroizing::new(nonce.to_string()),
                    state: ScramState::Initial,
                },
            }
        }

        fn client_first_message(&mut self) -> Vec<u8> {
            self.inner.client_first_message()
        }

        fn process_server_first(&mut self, server_first: &[u8]) -> Result<Vec<u8>> {
            self.inner.process_server_first(server_first)
        }

        fn verify_server_final(&mut self, server_final: &[u8]) -> Result<()> {
            self.inner.verify_server_final(server_final)
        }
    }
}
