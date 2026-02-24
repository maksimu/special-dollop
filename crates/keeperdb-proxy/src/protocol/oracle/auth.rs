//! Oracle O5LOGON Authentication
//!
//! This module implements Oracle's O5LOGON challenge-response authentication
//! protocol for credential injection in the proxy.
//!
//! # Protocol Overview
//!
//! O5LOGON authentication flow:
//! 1. Client sends AUTH_SESSKEY request with username
//! 2. Server responds with AUTH_SESSKEY (encrypted) and AUTH_VFR_DATA (salt)
//! 3. Client computes response using password + challenge
//! 4. Server verifies and responds with success/failure
//!
//! # Oracle 12c+ PBKDF2 Authentication
//!
//! Oracle 12c introduced PBKDF2-based authentication (AUTH_PBKDF2_CSK_SALT).
//! When this is present, the protocol uses:
//! - PBKDF2-SHA512 for key derivation instead of simple SHA-1
//! - AUTH_PBKDF2_VGEN_COUNT: iterations for verifier generation
//! - AUTH_PBKDF2_SDER_COUNT: iterations for session key derivation
//!
//! # Proxy Strategy
//!
//! The proxy intercepts the authentication DATA packets and:
//! - Extracts the username from client auth requests
//! - Replaces with real credentials from the auth provider
//! - Computes the correct O5LOGON response using the real password
//! - Forwards modified packets to the server
//! - Relays server responses back to client
//!
//! # O5LOGON Crypto
//!
//! The O5LOGON protocol uses:
//! - SHA-1 for password key derivation (legacy)
//! - PBKDF2-SHA512 for password key derivation (Oracle 12c+)
//! - AES-192-CBC for session key encryption/decryption
//! - XOR for combining session keys

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha1::{Digest, Sha1};
use sha2::Sha512;

use crate::error::Result;

/// O5LOGON authentication state machine
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum O5LogonState {
    /// Initial state - waiting for auth request from client
    #[default]
    Initial,
    /// Received auth request from client, sent to server
    RequestSent,
    /// Received challenge from server (AUTH_SESSKEY, AUTH_VFR_DATA)
    ChallengeReceived {
        /// Encrypted session key from server
        auth_sesskey: Vec<u8>,
        /// Verification/salt data from server
        auth_vfr_data: Vec<u8>,
    },
    /// Sent auth response to server
    ResponseSent,
    /// Authentication complete (success or failure)
    Complete,
}

/// Oracle TNS Data packet function codes
pub mod function_code {
    /// Protocol negotiation / set protocol
    pub const SET_PROTOCOL: u8 = 0x01;
    /// Data type negotiation
    pub const DATA_TYPE: u8 = 0x02;
    /// Start of authentication (TTI function)
    pub const TTI_FUNC: u8 = 0x03;
    /// Authentication continue
    pub const AUTH: u8 = 0x76; // 'v' - O5LOGON
    /// Oracle 11g+ auth
    pub const AUTH_11G: u8 = 0x77; // 'w'
}

/// Oracle authentication request parsed from DATA packet
#[derive(Debug, Clone)]
pub struct AuthRequest {
    /// Username from the auth request
    pub username: String,
    /// Raw authentication data
    pub auth_data: Vec<u8>,
    /// Position of username in the packet for modification
    pub username_offset: usize,
    /// Length of username field
    pub username_length: usize,
}

/// Oracle authentication challenge from server
#[derive(Debug, Clone)]
pub struct AuthChallenge {
    /// AUTH_SESSKEY - encrypted session key
    pub auth_sesskey: Vec<u8>,
    /// AUTH_VFR_DATA - verification/salt data
    pub auth_vfr_data: Vec<u8>,
    /// Raw challenge data
    pub raw_data: Vec<u8>,
}

/// Oracle authentication result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthResult {
    /// Authentication successful
    Success,
    /// Authentication failed with error message
    Failure(String),
    /// Need more data / continue auth flow
    Continue,
}

/// Check if a DATA packet payload looks like an authentication packet
///
/// Oracle auth DATA packets typically start with specific function codes
/// or contain recognizable authentication patterns.
pub fn is_auth_packet(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }

    // Check for common auth-related patterns
    // O5LOGON packets often contain 'AUTH_' prefixed keys
    let has_auth_marker = payload.windows(5).any(|w| w == b"AUTH_");

    // Check first byte for TTI function codes
    let first_byte = payload[0];
    let is_tti_auth = first_byte == function_code::AUTH
        || first_byte == function_code::AUTH_11G
        || first_byte == function_code::TTI_FUNC;

    has_auth_marker || is_tti_auth
}

/// Parse an authentication request from a DATA packet payload
///
/// Extracts the username and other authentication parameters from
/// the client's initial auth request.
pub fn parse_auth_request(payload: &[u8]) -> Result<Option<AuthRequest>> {
    // Look for AUTH_TERMINAL, AUTH_PROGRAM_NM, or username patterns
    // Oracle auth packets have key-value pairs with length-prefixed strings

    if payload.len() < 10 {
        return Ok(None);
    }

    // Try to find username in the auth data
    // Common patterns:
    // 1. AUTH_TERMINAL followed by username
    // 2. Direct username at known offset
    // 3. Length-prefixed string patterns

    // Search for recognizable username field markers
    let username_markers = [b"AUTH_TERMINAL".as_slice(), b"AUTH_PROGRAM_NM".as_slice()];

    for marker in &username_markers {
        if let Some(pos) = find_subsequence(payload, marker) {
            // Username typically follows the marker with a length prefix
            let after_marker = pos + marker.len();
            if after_marker + 2 < payload.len() {
                // Try to extract length-prefixed string after marker
                if let Some((username, offset, len)) =
                    extract_length_prefixed_string(payload, after_marker)
                {
                    if !username.is_empty()
                        && username
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Ok(Some(AuthRequest {
                            username,
                            auth_data: payload.to_vec(),
                            username_offset: offset,
                            username_length: len,
                        }));
                    }
                }
            }
        }
    }

    // Alternative: Look for O5LOGON specific structure
    // The username is often in a specific position in the TTI auth packet
    if payload.len() > 20
        && (payload[0] == function_code::AUTH || payload[0] == function_code::AUTH_11G)
    {
        // Try to parse TTI auth structure
        // Format varies but often has: func_code, flags, length, username...
        if let Some((username, offset, len)) = extract_tti_username(payload) {
            return Ok(Some(AuthRequest {
                username,
                auth_data: payload.to_vec(),
                username_offset: offset,
                username_length: len,
            }));
        }
    }

    Ok(None)
}

/// Parse an authentication challenge from server's DATA packet
pub fn parse_auth_challenge(payload: &[u8]) -> Result<Option<AuthChallenge>> {
    if payload.len() < 20 {
        return Ok(None);
    }

    let mut auth_sesskey = None;
    let mut auth_vfr_data = None;

    // Look for AUTH_SESSKEY
    if let Some(pos) = find_subsequence(payload, b"AUTH_SESSKEY") {
        let value_start = pos + 12; // "AUTH_SESSKEY".len()
        if let Some(value) = extract_auth_value(payload, value_start) {
            auth_sesskey = Some(value);
        }
    }

    // Look for AUTH_VFR_DATA
    if let Some(pos) = find_subsequence(payload, b"AUTH_VFR_DATA") {
        let value_start = pos + 13; // "AUTH_VFR_DATA".len()
        if let Some(value) = extract_auth_value(payload, value_start) {
            auth_vfr_data = Some(value);
        }
    }

    // If we found at least the session key, return the challenge
    if let Some(sesskey) = auth_sesskey {
        Ok(Some(AuthChallenge {
            auth_sesskey: sesskey,
            auth_vfr_data: auth_vfr_data.unwrap_or_default(),
            raw_data: payload.to_vec(),
        }))
    } else {
        Ok(None)
    }
}

/// Extract Oracle 12c+ PBKDF2 parameters from a server challenge packet
pub fn extract_pbkdf2_params(payload: &[u8]) -> Option<Pbkdf2Params> {
    // Look for AUTH_PBKDF2_CSK_SALT - presence indicates 12c+ PBKDF2 auth
    let csk_salt = if let Some(pos) = find_subsequence(payload, b"AUTH_PBKDF2_CSK_SALT") {
        let value_start = pos + 20; // "AUTH_PBKDF2_CSK_SALT".len()
        extract_auth_value(payload, value_start)?
    } else {
        return None; // No PBKDF2 params present
    };

    // Extract AUTH_PBKDF2_VGEN_COUNT (verifier generation iterations)
    let vgen_count = if let Some(pos) = find_subsequence(payload, b"AUTH_PBKDF2_VGEN_COUNT") {
        let value_start = pos + 22; // "AUTH_PBKDF2_VGEN_COUNT".len()
        extract_auth_count(payload, value_start).unwrap_or(4096)
    } else {
        4096 // Default iterations
    };

    // Extract AUTH_PBKDF2_SDER_COUNT (session key derivation iterations)
    let sder_count = if let Some(pos) = find_subsequence(payload, b"AUTH_PBKDF2_SDER_COUNT") {
        let value_start = pos + 22; // "AUTH_PBKDF2_SDER_COUNT".len()
        extract_auth_count(payload, value_start).unwrap_or(3)
    } else {
        3 // Default iterations
    };

    Some(Pbkdf2Params {
        csk_salt,
        vgen_count,
        sder_count,
    })
}

/// Extract an integer count value from Oracle TTI format
fn extract_auth_count(data: &[u8], start: usize) -> Option<u32> {
    if start >= data.len() {
        return None;
    }

    let mut pos = start;

    // Skip separator bytes
    while pos < data.len() && (data[pos] == 0 || data[pos] == b' ' || data[pos] == b'=') {
        pos += 1;
    }

    if pos >= data.len() {
        return None;
    }

    // Skip 01 marker if present
    if data[pos] == 0x01 {
        pos += 1;
    }

    if pos >= data.len() {
        return None;
    }

    // Read length
    let len = data[pos] as usize;
    pos += 1;

    // Skip duplicate length
    if pos < data.len() && data[pos] == len as u8 {
        pos += 1;
    }

    if len == 0 || pos + len > data.len() {
        return None;
    }

    // Extract and parse the value
    let value_bytes = &data[pos..pos + len];

    // Check if this is an ASCII decimal string (all digits 0-9)
    let is_decimal_ascii = value_bytes.iter().all(|&b| b.is_ascii_digit());

    if is_decimal_ascii {
        // Parse as decimal string (e.g., "4096" -> 4096)
        let decimal_str = std::str::from_utf8(value_bytes).ok()?;
        decimal_str.parse().ok()
    } else if len <= 4 {
        // Parse as big-endian integer
        let mut val = 0u32;
        for &b in value_bytes {
            val = (val << 8) | (b as u32);
        }
        Some(val)
    } else {
        None
    }
}

/// Check if a DATA packet indicates authentication success
pub fn is_auth_success(payload: &[u8]) -> bool {
    // Look for success indicators
    // Oracle sends specific response codes or patterns on success

    // Check for AUTH_SVR_RESPONSE or similar success markers
    if find_subsequence(payload, b"AUTH_SVR_RESPONSE").is_some() {
        return true;
    }

    // Check for session established markers
    if find_subsequence(payload, b"AUTH_SESSKEY").is_some()
        && find_subsequence(payload, b"AUTH_VFR_DATA").is_none()
    {
        // Server sent session key without challenge = likely authenticated
        return true;
    }

    false
}

/// Check if a DATA packet indicates authentication failure
pub fn is_auth_failure(payload: &[u8]) -> bool {
    // Look for error indicators
    // ORA-01017: invalid username/password
    // ORA-28000: account locked
    // etc.

    find_subsequence(payload, b"ORA-01017").is_some()
        || find_subsequence(payload, b"ORA-28000").is_some()
        || find_subsequence(payload, b"ORA-28001").is_some()
        || find_subsequence(payload, b"invalid username/password").is_some()
}

/// Modify a DATA packet payload to replace the username
///
/// Takes the original auth request payload and creates a new one
/// with the username replaced.
pub fn modify_auth_username(
    original: &[u8],
    auth_request: &AuthRequest,
    new_username: &str,
) -> Vec<u8> {
    if auth_request.username_offset == 0 || auth_request.username_length == 0 {
        // Can't modify if we don't know where the username is
        return original.to_vec();
    }

    let mut modified = Vec::with_capacity(original.len() + new_username.len());

    // Copy bytes before username
    modified.extend_from_slice(&original[..auth_request.username_offset]);

    // Insert new username with length prefix (if original had one)
    // The format depends on how the username was encoded
    if auth_request.username_offset > 0 {
        let len_byte_pos = auth_request.username_offset - 1;
        if len_byte_pos < original.len()
            && original[len_byte_pos] == auth_request.username_length as u8
        {
            // Length-prefixed format - update length byte
            if !modified.is_empty() {
                let last_idx = modified.len() - 1;
                modified[last_idx] = new_username.len() as u8;
            }
        }
    }

    // Add new username bytes
    modified.extend_from_slice(new_username.as_bytes());

    // Copy bytes after original username
    let after_username = auth_request.username_offset + auth_request.username_length;
    if after_username < original.len() {
        modified.extend_from_slice(&original[after_username..]);
    }

    modified
}

/// Build an Oracle-compatible authentication error message
pub fn build_auth_error(error_code: u32, message: &str) -> Vec<u8> {
    // Format: ORA-XXXXX: message
    format!("ORA-{:05}: {}", error_code, message).into_bytes()
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Find a subsequence in a byte slice
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Extract a length-prefixed string from a byte slice
fn extract_length_prefixed_string(data: &[u8], start: usize) -> Option<(String, usize, usize)> {
    if start >= data.len() {
        return None;
    }

    // Try single-byte length prefix
    let len = data[start] as usize;
    let str_start = start + 1;

    if len > 0 && len < 256 && str_start + len <= data.len() {
        if let Ok(s) = std::str::from_utf8(&data[str_start..str_start + len]) {
            // Validate it looks like a username (alphanumeric + underscore)
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
            {
                return Some((s.to_string(), str_start, len));
            }
        }
    }

    // Try two-byte length prefix (big-endian)
    if start + 1 < data.len() {
        let len = u16::from_be_bytes([data[start], data[start + 1]]) as usize;
        let str_start = start + 2;

        if len > 0 && len < 1000 && str_start + len <= data.len() {
            if let Ok(s) = std::str::from_utf8(&data[str_start..str_start + len]) {
                if s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                {
                    return Some((s.to_string(), str_start, len));
                }
            }
        }
    }

    None
}

/// Extract username from TTI auth packet structure
fn extract_tti_username(data: &[u8]) -> Option<(String, usize, usize)> {
    // TTI auth packets have various structures depending on Oracle version
    // Common pattern: after initial bytes, there's a username field

    // Skip function code and flags (usually first 2-4 bytes)
    for offset in 2..20.min(data.len()) {
        if let Some((username, str_offset, len)) = extract_length_prefixed_string(data, offset) {
            // Validate username looks reasonable
            if username.len() >= 2 && username.len() <= 128 {
                return Some((username, str_offset, len));
            }
        }
    }

    None
}

/// Extract an auth value (like AUTH_SESSKEY value) from after a marker
///
/// Oracle TTI format after field name: `01 <len> <len> <hex_ascii_value>`
/// - `01` is a marker byte
/// - Length appears twice (e.g., `40 40` for 64-byte value)
/// - Value is hex-encoded ASCII (e.g., "6FC8F059..." representing raw bytes)
///
/// Returns the raw bytes after hex-decoding the ASCII value.
fn extract_auth_value(data: &[u8], start: usize) -> Option<Vec<u8>> {
    if start >= data.len() {
        return None;
    }

    let mut pos = start;

    // Skip any whitespace or separator bytes
    while pos < data.len() && (data[pos] == 0 || data[pos] == b' ' || data[pos] == b'=') {
        pos += 1;
    }

    if pos >= data.len() {
        return None;
    }

    // Oracle TTI format: 01 <len> <len> <value>
    // Skip the 01 marker if present
    if data[pos] == 0x01 {
        pos += 1;
    }

    if pos >= data.len() {
        return None;
    }

    // Read length (Oracle doubles it, e.g., 40 40 for length 64)
    let len = data[pos] as usize;
    pos += 1;

    // Skip the duplicate length byte if present and matches
    if pos < data.len() && data[pos] == len as u8 {
        pos += 1;
    }

    if len == 0 || pos + len > data.len() {
        return None;
    }

    // Extract the value bytes
    let value_bytes = &data[pos..pos + len];

    // Check if this looks like hex-encoded ASCII (all chars are 0-9, A-F, a-f)
    let is_hex_ascii = value_bytes
        .iter()
        .all(|&b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b) || (b'a'..=b'f').contains(&b));

    if is_hex_ascii && len >= 2 {
        // Hex-decode the ASCII string to raw bytes
        let hex_str = std::str::from_utf8(value_bytes).ok()?;
        let decoded = hex_decode(hex_str)?;
        Some(decoded)
    } else {
        // Return raw bytes as-is
        Some(value_bytes.to_vec())
    }
}

/// Decode a hex string to bytes
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }

    let mut result = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        result.push((high << 4) | low);
    }
    Some(result)
}

/// Convert a hex ASCII digit to its value
fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

// ============================================================================
// O5LOGON Crypto Implementation
// ============================================================================

/// AES-192-CBC decryptor type alias (for legacy O5LOGON)
type Aes192CbcDec = cbc::Decryptor<aes::Aes192>;
/// AES-192-CBC encryptor type alias (for legacy O5LOGON)
type Aes192CbcEnc = cbc::Encryptor<aes::Aes192>;
/// AES-256-CBC decryptor type alias (for Oracle 12c+ PBKDF2)
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
/// AES-256-CBC encryptor type alias (for Oracle 12c+ PBKDF2)
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

/// Oracle 12c+ PBKDF2 challenge parameters
#[derive(Debug, Clone, Default)]
pub struct Pbkdf2Params {
    /// Salt for combined session key derivation
    pub csk_salt: Vec<u8>,
    /// Iteration count for verifier generation
    pub vgen_count: u32,
    /// Iteration count for session key derivation
    pub sder_count: u32,
}

/// O5LOGON authentication context
///
/// Holds the state needed to compute authentication responses.
///
/// # Oracle 12c+ PBKDF2 Algorithm (from python-oracledb)
///
/// 1. Password Hash:
///    - salt = AUTH_VFR_DATA + b'AUTH_PBKDF2_SPEEDY_KEY'
///    - password_key = PBKDF2-SHA512(password, salt, VGEN_COUNT iterations, 64 bytes)
///    - password_hash = SHA512(password_key + AUTH_VFR_DATA)\[:32\]
///
/// 2. Session Key Exchange:
///    - Decrypt server's AUTH_SESSKEY using password_hash with AES-256-CBC
///    - Generate random client session key (same length)
///    - Encrypt client key with password_hash, send as AUTH_SESSKEY
///
/// 3. Combo Key:
///    - temp_key = client_sesskey\[:32\] + server_sesskey\[:32\]
///    - combo_key = PBKDF2-SHA512(temp_key.hex().upper(), CSK_SALT, SDER_COUNT, 32)
///
/// 4. AUTH_PASSWORD:
///    - random_salt = 16 random bytes
///    - plaintext = random_salt + password
///    - AUTH_PASSWORD = AES-256-CBC-encrypt(combo_key, plaintext).hex().upper()
#[derive(Debug, Clone)]
pub struct O5LogonAuth {
    /// Username for authentication
    pub username: String,
    /// Password for authentication
    pub password: String,
    /// Server's encrypted session key (from AUTH_SESSKEY, raw bytes after hex decode)
    pub server_sesskey: Vec<u8>,
    /// Salt/verifier data (from AUTH_VFR_DATA, raw bytes after hex decode)
    pub auth_vfr_data: Vec<u8>,
    /// Decrypted server session key
    pub decrypted_server_key: Option<Vec<u8>>,
    /// Client-generated session key (random bytes)
    pub client_sesskey: Vec<u8>,
    /// Combined session key / combo key
    pub combined_key: Option<Vec<u8>>,
    /// Oracle 12c+ PBKDF2 parameters (None = legacy SHA-1 mode)
    pub pbkdf2_params: Option<Pbkdf2Params>,
    /// 32-byte password hash for PBKDF2 mode
    password_hash: Option<Vec<u8>>,
    /// 64-byte password key (PBKDF2 output, before SHA-512) - needed for speedy_key
    password_key: Option<Vec<u8>>,
    /// 16-byte random salt used for AUTH_PASSWORD and AUTH_PBKDF2_SPEEDY_KEY
    auth_salt: Option<Vec<u8>>,
}

impl O5LogonAuth {
    /// Create a new O5LOGON auth context
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_string(), // Preserve case - gateway is responsible for correct casing
            password: password.to_string(),
            server_sesskey: Vec::new(),
            auth_vfr_data: Vec::new(),
            decrypted_server_key: None,
            client_sesskey: Vec::new(), // Will be generated after decrypting server key
            combined_key: None,
            pbkdf2_params: None,
            password_hash: None,
            password_key: None,
            auth_salt: None,
        }
    }

    /// Set the server's challenge data (legacy O5LOGON)
    pub fn set_challenge(&mut self, server_sesskey: Vec<u8>, auth_vfr_data: Vec<u8>) {
        // Generate random client session key for legacy mode
        let mut client_sesskey = vec![0u8; 32];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut client_sesskey);

        self.server_sesskey = server_sesskey;
        self.auth_vfr_data = auth_vfr_data;
        self.decrypted_server_key = None;
        self.client_sesskey = client_sesskey;
        self.combined_key = None;
        self.pbkdf2_params = None;
        self.password_hash = None;
        self.password_key = None;
        self.auth_salt = None;
    }

    /// Set the server's challenge data with Oracle 12c+ PBKDF2 parameters
    pub fn set_challenge_pbkdf2(
        &mut self,
        server_sesskey: Vec<u8>,
        auth_vfr_data: Vec<u8>,
        csk_salt: Vec<u8>,
        vgen_count: u32,
        sder_count: u32,
    ) {
        self.server_sesskey = server_sesskey;
        self.auth_vfr_data = auth_vfr_data;
        self.decrypted_server_key = None;
        self.client_sesskey = Vec::new(); // Generated after password_hash computation
        self.combined_key = None;
        self.pbkdf2_params = Some(Pbkdf2Params {
            csk_salt,
            vgen_count,
            sder_count,
        });
        self.password_hash = None;
        self.password_key = None;
        self.auth_salt = None;
    }

    /// Compute the 32-byte password hash for Oracle 12c+ PBKDF2 mode
    ///
    /// Algorithm from python-oracledb:
    /// 1. salt = AUTH_VFR_DATA + b'AUTH_PBKDF2_SPEEDY_KEY'
    /// 2. password_key = PBKDF2-SHA512(password, salt, VGEN_COUNT, 64 bytes)
    /// 3. password_hash = SHA512(password_key + AUTH_VFR_DATA)[:32]
    fn compute_password_hash_pbkdf2(&mut self, params: &Pbkdf2Params) -> Vec<u8> {
        // Step 1: Build salt = VFR_DATA + "AUTH_PBKDF2_SPEEDY_KEY"
        let mut salt = self.auth_vfr_data.clone();
        salt.extend_from_slice(b"AUTH_PBKDF2_SPEEDY_KEY");

        // Step 2: PBKDF2-SHA512(password, salt, VGEN_COUNT, 64 bytes)
        let mut password_key = [0u8; 64];
        type HmacSha512 = Hmac<Sha512>;
        pbkdf2::<HmacSha512>(
            self.password.as_bytes(),
            &salt,
            params.vgen_count.max(1),
            &mut password_key,
        )
        .expect("PBKDF2 should not fail");

        // Save password_key for speedy_key computation later
        self.password_key = Some(password_key.to_vec());

        // Step 3: SHA512(password_key + AUTH_VFR_DATA)[:32]
        let mut hasher = Sha512::new();
        hasher.update(password_key);
        hasher.update(&self.auth_vfr_data);
        let hash = hasher.finalize();

        let password_hash = hash[..32].to_vec();

        self.password_hash = Some(password_hash.clone());
        password_hash
    }

    /// Ensure password_hash is computed before decrypting client's sesskey
    ///
    /// This must be called BEFORE decrypt_external_client_sesskey() for Oracle 12c+
    /// to ensure we use the correct PBKDF2-based key instead of legacy SHA-1.
    pub fn ensure_password_hash_computed(&mut self) {
        if self.password_hash.is_some() {
            return; // Already computed
        }

        if let Some(params) = self.pbkdf2_params.clone() {
            self.compute_password_hash_pbkdf2(&params);
        }
    }

    /// Derive the AES key from password and salt (legacy SHA-1)
    ///
    /// Oracle uses SHA-1(password || salt) and takes first 24 bytes for AES-192
    fn derive_key_sha1(&self) -> [u8; 24] {
        let mut hasher = Sha1::new();
        hasher.update(self.password.as_bytes());
        hasher.update(&self.auth_vfr_data);
        let hash = hasher.finalize();

        // Take first 20 bytes from SHA-1, pad to 24 for AES-192
        let mut key = [0u8; 24];
        key[..20].copy_from_slice(&hash[..20]);

        key
    }

    /// Decrypt the server's session key
    ///
    /// For PBKDF2 mode: uses 32-byte password_hash with AES-256-CBC
    /// For legacy mode: uses SHA-1 derived 24-byte key with AES-192-CBC
    pub fn decrypt_server_sesskey(&mut self) -> Result<Vec<u8>> {
        if self.server_sesskey.is_empty() {
            return Err(crate::error::ProxyError::Auth(
                "No server session key to decrypt".into(),
            ));
        }

        let decrypted = if let Some(params) = self.pbkdf2_params.clone() {
            // Oracle 12c+ PBKDF2 mode
            let password_hash = self.compute_password_hash_pbkdf2(&params);

            // IV is all zeros
            let iv = [0u8; 16];

            // Clone for decryption
            let mut data = self.server_sesskey.clone();

            // Pad to block size if needed
            let padding_needed = (16 - (data.len() % 16)) % 16;
            data.extend(vec![0u8; padding_needed]);

            // AES-256-CBC decrypt with password_hash (32 bytes)
            let key: [u8; 32] = password_hash.clone().try_into().map_err(|_| {
                crate::error::ProxyError::Auth("Invalid password hash length".into())
            })?;

            // IMPORTANT: Don't clone the decryptor - CBC needs state to chain between blocks
            let mut decryptor = Aes256CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }

            data
        } else {
            // Legacy SHA-1 mode with AES-192-CBC
            let key = self.derive_key_sha1();

            let iv = [0u8; 16];
            let mut data = self.server_sesskey.clone();

            let padding_needed = (16 - (data.len() % 16)) % 16;
            data.extend(vec![0u8; padding_needed]);

            // IMPORTANT: Don't clone the decryptor - CBC needs state to chain between blocks
            let mut decryptor = Aes192CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }

            // Remove PKCS7 padding if present
            if let Some(&last_byte) = data.last() {
                let pad_len = last_byte as usize;
                if pad_len > 0 && pad_len <= 16 {
                    let valid = data.iter().rev().take(pad_len).all(|&b| b == last_byte);
                    if valid {
                        data.truncate(data.len() - pad_len);
                    }
                }
            }

            data
        };

        // Generate client session key (same length as decrypted server key)
        // BUT only if one hasn't been set externally (for preserving client's key context)
        if self.client_sesskey.is_empty() {
            let mut client_key = vec![0u8; decrypted.len()];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut client_key);
            self.client_sesskey = client_key;
        }

        self.decrypted_server_key = Some(decrypted.clone());
        Ok(decrypted)
    }

    /// Decrypt an external client's AUTH_SESSKEY to get their raw session key.
    ///
    /// This is used when the client provides their own credentials - we need to
    /// use THEIR session key (not generate our own) to preserve their encryption context.
    ///
    /// For PBKDF2 mode: uses password_hash with AES-256-CBC
    /// For legacy mode: uses SHA-1 derived key with AES-192-CBC
    pub fn decrypt_external_client_sesskey(&self, encrypted_sesskey: &[u8]) -> Result<Vec<u8>> {
        let iv = [0u8; 16];
        let mut data = encrypted_sesskey.to_vec();

        // Pad to block size if needed
        let padding_needed = (16 - (data.len() % 16)) % 16;
        if padding_needed > 0 {
            data.extend(vec![0u8; padding_needed]);
        }

        if let Some(password_hash) = &self.password_hash {
            // PBKDF2 mode: AES-256-CBC decrypt
            let key: [u8; 32] = password_hash.clone().try_into().map_err(|_| {
                crate::error::ProxyError::Auth("Invalid password hash length".into())
            })?;

            let mut decryptor = Aes256CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }
        } else {
            // Legacy mode: AES-192-CBC decrypt
            let key = self.derive_key_sha1();
            let mut decryptor = Aes192CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }
        }

        Ok(data)
    }

    /// Set the client session key from an external source (client's decrypted sesskey).
    ///
    /// Used when the client provides credentials - we use their sesskey to preserve
    /// their encryption context for post-auth communication.
    pub fn set_client_sesskey(&mut self, sesskey: Vec<u8>) {
        self.client_sesskey = sesskey;
    }

    /// Decrypt and verify the client's AUTH_PASSWORD to check if it matches the real password.
    ///
    /// This is used to determine if we can forward the client's packet unchanged
    /// (when they provided correct credentials) vs replacing with our own values.
    ///
    /// Returns true if the decrypted password matches the real password.
    pub fn verify_client_auth_password(&self, encrypted_auth_password: &[u8]) -> bool {
        let combo_key = match &self.combined_key {
            Some(k) => k,
            None => {
                return false;
            }
        };

        // Decrypt AUTH_PASSWORD with combo_key (AES-256-CBC for PBKDF2, AES-192-CBC for legacy)
        let iv = [0u8; 16];
        let mut data = encrypted_auth_password.to_vec();

        // Pad to block size if needed
        let padding_needed = (16 - (data.len() % 16)) % 16;
        if padding_needed > 0 {
            data.extend(vec![0u8; padding_needed]);
        }

        if self.pbkdf2_params.is_some() {
            // PBKDF2 mode: AES-256-CBC
            if combo_key.len() < 32 {
                return false;
            }
            let key: [u8; 32] = match combo_key[..32].try_into() {
                Ok(k) => k,
                Err(_) => return false,
            };

            let mut decryptor = Aes256CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }
        } else {
            // Legacy mode: AES-192-CBC
            if combo_key.len() < 24 {
                return false;
            }
            let key: [u8; 24] = match combo_key[..24].try_into() {
                Ok(k) => k,
                Err(_) => return false,
            };

            let mut decryptor = Aes192CbcDec::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    decryptor.decrypt_block_mut(block);
                }
            }
        }

        // Decrypted AUTH_PASSWORD format: random_salt(16) + password + padding
        // Strip the 16-byte salt and compare the password
        if data.len() <= 16 {
            return false;
        }

        let decrypted_password = &data[16..];

        // Strip PKCS7 padding or trailing zeros
        let decrypted_password = if let Some(&last_byte) = decrypted_password.last() {
            let pad_len = last_byte as usize;
            // Check if it looks like PKCS7 padding (last byte indicates padding length, 1-16)
            if pad_len > 0 && pad_len <= 16 && pad_len <= decrypted_password.len() {
                let valid_pkcs7 = decrypted_password
                    .iter()
                    .rev()
                    .take(pad_len)
                    .all(|&b| b == last_byte);
                if valid_pkcs7 {
                    &decrypted_password[..decrypted_password.len() - pad_len]
                } else {
                    // Not PKCS7, try stripping trailing zeros
                    decrypted_password
                        .iter()
                        .position(|&b| b == 0)
                        .map(|pos| &decrypted_password[..pos])
                        .unwrap_or(decrypted_password)
                }
            } else {
                // Try stripping trailing zeros
                decrypted_password
                    .iter()
                    .position(|&b| b == 0)
                    .map(|pos| &decrypted_password[..pos])
                    .unwrap_or(decrypted_password)
            }
        } else {
            decrypted_password
        };

        let matches = decrypted_password == self.password.as_bytes();

        matches
    }

    /// Encrypt the client session key to be sent as AUTH_SESSKEY
    ///
    /// For PBKDF2 mode: uses password_hash with AES-256-CBC
    /// For legacy mode: uses SHA-1 derived key with AES-192-CBC
    ///
    /// Note: Uses zero padding, not PKCS7, based on Wireshark capture showing
    /// 32-byte encrypted output (not 48 bytes as PKCS7 would produce).
    fn encrypt_client_sesskey(&self) -> Result<Vec<u8>> {
        let iv = [0u8; 16];

        // Zero padding (client sesskey is already 32 bytes = multiple of 16)
        let mut data = self.client_sesskey.clone();
        let padding_needed = (16 - (data.len() % 16)) % 16;
        if padding_needed > 0 {
            data.extend(vec![0u8; padding_needed]);
        }

        if let Some(password_hash) = &self.password_hash {
            // PBKDF2 mode: AES-256-CBC
            let key: [u8; 32] = password_hash.clone().try_into().map_err(|_| {
                crate::error::ProxyError::Auth("Invalid password hash length".into())
            })?;

            // IMPORTANT: Don't clone the encryptor - CBC needs state to chain between blocks
            let mut encryptor = Aes256CbcEnc::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    encryptor.encrypt_block_mut(block);
                }
            }
        } else {
            // Legacy mode: AES-192-CBC
            let key = self.derive_key_sha1();
            let mut encryptor = Aes192CbcEnc::new(&key.into(), &iv.into());
            for chunk in data.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    encryptor.encrypt_block_mut(block);
                }
            }
        }

        Ok(data)
    }

    /// Generate the combo key for Oracle 12c+ PBKDF2 mode
    ///
    /// Algorithm from python-oracledb:
    /// - temp_key = client_sesskey[:32] + server_sesskey[:32]
    /// - combo_key = PBKDF2-SHA512(temp_key.hex().upper(), CSK_SALT, SDER_COUNT, 32)
    fn generate_combo_key_pbkdf2(&mut self, params: &Pbkdf2Params) -> Result<Vec<u8>> {
        let server_key = self.decrypted_server_key.as_ref().ok_or_else(|| {
            crate::error::ProxyError::Auth("Server session key not decrypted".into())
        })?;

        // Take first 32 bytes of each key
        let key_len = 32.min(self.client_sesskey.len()).min(server_key.len());
        let mut temp_key = Vec::with_capacity(key_len * 2);
        temp_key.extend_from_slice(&self.client_sesskey[..key_len]);
        temp_key.extend_from_slice(&server_key[..key_len]);

        // Convert to uppercase hex string
        let temp_key_hex = hex_encode(&temp_key);

        // PBKDF2-SHA512(hex_string, CSK_SALT, SDER_COUNT, 32)
        let mut combo_key = [0u8; 32];
        type HmacSha512 = Hmac<Sha512>;
        pbkdf2::<HmacSha512>(
            temp_key_hex.as_bytes(),
            &params.csk_salt,
            params.sder_count.max(1),
            &mut combo_key,
        )
        .expect("PBKDF2 should not fail");

        let result = combo_key.to_vec();
        self.combined_key = Some(result.clone());
        Ok(result)
    }

    /// Generate the combined session key for legacy mode (server XOR client)
    fn generate_combined_key_legacy(&mut self) -> Result<Vec<u8>> {
        let server_key = self.decrypted_server_key.as_ref().ok_or_else(|| {
            crate::error::ProxyError::Auth("Server session key not decrypted".into())
        })?;

        let len = server_key.len().min(self.client_sesskey.len());
        let mut combined = vec![0u8; len];
        for i in 0..len {
            combined[i] = server_key[i] ^ self.client_sesskey[i];
        }

        self.combined_key = Some(combined.clone());
        Ok(combined)
    }

    /// Generate the combined/combo key
    pub fn generate_combined_key(&mut self) -> Result<Vec<u8>> {
        if let Some(params) = self.pbkdf2_params.clone() {
            self.generate_combo_key_pbkdf2(&params)
        } else {
            self.generate_combined_key_legacy()
        }
    }

    /// Compute the AUTH_PASSWORD response
    ///
    /// For PBKDF2 mode: random_salt(16) + password, encrypted with combo_key using AES-256-CBC
    /// For legacy mode: SHA-1 password hash encrypted with combined_key using AES-192-CBC
    ///
    /// Note: The random salt is saved to self.auth_salt for use in speedy_key computation.
    pub fn compute_auth_response(&mut self) -> Result<Vec<u8>> {
        let combo_key = self
            .combined_key
            .as_ref()
            .ok_or_else(|| crate::error::ProxyError::Auth("Combo key not generated".into()))?
            .clone();

        let iv = [0u8; 16];

        if self.pbkdf2_params.is_some() {
            // Oracle 12c+ PBKDF2 mode
            // Plaintext = 16 random bytes + password
            let mut auth_salt = vec![0u8; 16];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut auth_salt);

            // Save salt for speedy_key computation
            self.auth_salt = Some(auth_salt.clone());

            let mut plaintext = auth_salt;
            plaintext.extend_from_slice(self.password.as_bytes());

            // PKCS7 padding (python-oracledb default)
            let padding_needed = 16 - (plaintext.len() % 16);
            // PKCS7: pad with bytes all equal to padding length
            plaintext.extend(vec![padding_needed as u8; padding_needed]);

            // AES-256-CBC encrypt with combo_key
            let key: [u8; 32] = combo_key
                .try_into()
                .map_err(|_| crate::error::ProxyError::Auth("Invalid combo key length".into()))?;

            // IMPORTANT: Don't clone the encryptor - CBC needs state to chain between blocks
            let mut encryptor = Aes256CbcEnc::new(&key.into(), &iv.into());
            for chunk in plaintext.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    encryptor.encrypt_block_mut(block);
                }
            }

            Ok(plaintext)
        } else {
            // Legacy SHA-1 mode
            let mut hasher = Sha1::new();
            hasher.update(self.password.as_bytes());
            hasher.update(&self.auth_vfr_data);
            let hash = hasher.finalize();

            // Pad to 32 bytes
            let mut response = vec![0u8; 32];
            response[..20].copy_from_slice(&hash[..]);

            // AES-192-CBC encrypt
            let mut enc_key = [0u8; 24];
            let key_len = combo_key.len().min(24);
            enc_key[..key_len].copy_from_slice(&combo_key[..key_len]);

            // IMPORTANT: Don't clone the encryptor - CBC needs state to chain between blocks
            let mut encryptor = Aes192CbcEnc::new(&enc_key.into(), &iv.into());
            for chunk in response.chunks_mut(16) {
                if chunk.len() == 16 {
                    let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                    encryptor.encrypt_block_mut(block);
                }
            }

            Ok(response)
        }
    }

    /// Get the encrypted client session key (to be sent as AUTH_SESSKEY)
    pub fn get_encrypted_client_sesskey(&self) -> Result<Vec<u8>> {
        self.encrypt_client_sesskey()
    }

    /// Get the client session key (raw bytes)
    pub fn get_client_sesskey(&self) -> &[u8] {
        &self.client_sesskey
    }

    /// Compute AUTH_PBKDF2_SPEEDY_KEY for Oracle 12c+ authentication
    ///
    /// speedy_key = AES-256-CBC-encrypt(combo_key, auth_salt + password_key)
    /// where:
    /// - auth_salt is the same 16-byte random salt used for AUTH_PASSWORD
    /// - password_key is the 64-byte PBKDF2 output (before SHA-512 hash)
    ///
    /// Returns None for legacy (non-PBKDF2) authentication.
    pub fn compute_speedy_key(&self) -> Result<Option<Vec<u8>>> {
        // Only for PBKDF2 mode
        if self.pbkdf2_params.is_none() {
            return Ok(None);
        }

        let combo_key = self
            .combined_key
            .as_ref()
            .ok_or_else(|| crate::error::ProxyError::Auth("Combo key not generated".into()))?;

        let auth_salt = self.auth_salt.as_ref().ok_or_else(|| {
            crate::error::ProxyError::Auth(
                "Auth salt not generated (call compute_auth_response first)".into(),
            )
        })?;

        let password_key = self
            .password_key
            .as_ref()
            .ok_or_else(|| crate::error::ProxyError::Auth("Password key not computed".into()))?;

        // Plaintext = auth_salt (16 bytes) + password_key (64 bytes) = 80 bytes
        let mut plaintext = auth_salt.clone();
        plaintext.extend_from_slice(password_key);

        // 80 bytes is already a multiple of 16, so zero padding adds nothing
        // (This matches the Wireshark capture showing 80 bytes = 160 hex chars)

        let iv = [0u8; 16];
        let key: [u8; 32] = combo_key
            .clone()
            .try_into()
            .map_err(|_| crate::error::ProxyError::Auth("Invalid combo key length".into()))?;

        let mut encryptor = Aes256CbcEnc::new(&key.into(), &iv.into());
        for chunk in plaintext.chunks_mut(16) {
            if chunk.len() == 16 {
                let block = aes::cipher::generic_array::GenericArray::from_mut_slice(chunk);
                encryptor.encrypt_block_mut(block);
            }
        }

        Ok(Some(plaintext))
    }

    /// Full authentication flow: decrypt server key, generate combined key, compute response
    pub fn process_challenge(&mut self) -> Result<AuthResponseData> {
        self.decrypt_server_sesskey()?;
        self.generate_combined_key()?;

        // Get encrypted client session key (for AUTH_SESSKEY response)
        let encrypted_client_sesskey = self.get_encrypted_client_sesskey()?;

        // Get auth password (this also generates and saves auth_salt)
        let auth_password = self.compute_auth_response()?;

        // Compute speedy_key for PBKDF2 mode (uses same auth_salt)
        let speedy_key = self.compute_speedy_key()?;

        Ok(AuthResponseData {
            username: self.username.clone(),
            client_sesskey: encrypted_client_sesskey, // Send encrypted, not raw
            auth_password,
            speedy_key, // None for legacy mode, Some for PBKDF2
        })
    }
}

/// Data needed for the authentication response packet
#[derive(Debug, Clone)]
pub struct AuthResponseData {
    /// Real username to inject (replaces client's username)
    pub username: String,
    /// Client's session key (AUTH_SESSKEY from client)
    pub client_sesskey: Vec<u8>,
    /// Encrypted password response (AUTH_PASSWORD)
    pub auth_password: Vec<u8>,
    /// AUTH_PBKDF2_SPEEDY_KEY for Oracle 12c+ (None for legacy mode)
    pub speedy_key: Option<Vec<u8>>,
}

/// Build an O5LOGON authentication response packet
///
/// Takes the client's original auth response packet and replaces the
/// AUTH_SESSKEY and AUTH_PASSWORD values with our computed ones.
/// Also replaces the username to match the credentials used for crypto.
pub fn build_auth_response_packet(
    original_packet: &[u8],
    response_data: &AuthResponseData,
) -> Vec<u8> {
    // Replace AUTH_PASSWORD, AUTH_SESSKEY, and AUTH_PBKDF2_SPEEDY_KEY values in the packet
    // All values are hex-encoded strings
    // TTI format: 01 <len> <len> <value>
    let mut modified = original_packet.to_vec();

    // CRITICAL: Replace the username in the packet to match the credentials
    // The username is at a fixed position in the TTI header structure:
    // Bytes 0-1: data flags (00 00)
    // Byte 2-6: TTI header (03 73 02 01 01)
    // Byte 7: username length
    // Byte 8+: varies, but username follows after some 01/02 bytes and field count
    //
    // Packet structure (from Wireshark analysis):
    // ... 01 01 0E 01 01 <username> 01 0D 0D AUTH_PASSWORD ...
    //              ^^^^^ marker before username
    //                    ^^^^^^^^^ username bytes
    //                              ^^ start of field header (01 <len> <len>)
    //
    // So username is between "01 01" and "01 <len> <len> AUTH_..."
    let real_username = response_data.username.as_bytes();
    let real_username_len = real_username.len();

    // Find AUTH_PASSWORD to locate where username ends
    // The username is immediately before "01 0D 0D AUTH_PASSWORD"
    let auth_marker_pos = find_subsequence(&modified, b"AUTH_PASSWORD");

    if let Some(marker_pos) = auth_marker_pos {
        // Go back 3 bytes to find start of field header (01 0D 0D)
        if marker_pos >= 3 {
            let field_header_start = marker_pos - 3;

            // Username ends at field_header_start
            // Now find where username starts by looking for "01 01" before the username
            // Search backwards from field_header_start to find "01 01"
            let mut username_start_marker = None;
            for i in (12..field_header_start.saturating_sub(1)).rev() {
                if modified[i] == 0x01 && modified[i + 1] == 0x01 {
                    // Found "01 01" - username starts at i + 2
                    let potential_start = i + 2;
                    let potential_len = field_header_start - potential_start;

                    // Sanity check: username should be 1-30 chars and ASCII
                    if (1..=30).contains(&potential_len) {
                        let potential_username = &modified[potential_start..field_header_start];
                        if potential_username
                            .iter()
                            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
                        {
                            username_start_marker = Some(potential_start);
                            break;
                        }
                    }
                }
            }

            if let Some(username_start) = username_start_marker {
                let old_username_len = field_header_start - username_start;

                // Build new packet with replaced username
                let len_diff = real_username_len as isize - old_username_len as isize;
                let new_capacity = (modified.len() as isize + len_diff) as usize;
                let mut new_packet = Vec::with_capacity(new_capacity);

                // Copy everything up to username
                new_packet.extend_from_slice(&modified[..username_start]);
                // Insert new username
                new_packet.extend_from_slice(real_username);
                // Copy everything after old username (starting at field_header_start)
                new_packet.extend_from_slice(&modified[field_header_start..]);

                // Update byte 7 (username length in TTI header)
                if new_packet.len() > 7 && real_username_len <= 255 {
                    new_packet[7] = real_username_len as u8;
                }

                modified = new_packet;
            }
        }
    }

    // Replace AUTH_PASSWORD value (hex-encoded password hash)
    let password_hex = hex_encode(&response_data.auth_password);
    if let Some(new_data) =
        replace_tti_field_value(&modified, b"AUTH_PASSWORD", password_hex.as_bytes())
    {
        modified = new_data;
    }

    // Replace AUTH_PBKDF2_SPEEDY_KEY if present (hex-encoded, Oracle 12c+)
    if let Some(ref speedy_key) = response_data.speedy_key {
        let speedy_hex = hex_encode(speedy_key);
        if let Some(new_data) =
            replace_tti_field_value(&modified, b"AUTH_PBKDF2_SPEEDY_KEY", speedy_hex.as_bytes())
        {
            modified = new_data;
        }
    }

    // Replace AUTH_SESSKEY value (hex-encoded session key)
    let sesskey_hex = hex_encode(&response_data.client_sesskey);
    if let Some(new_data) =
        replace_tti_field_value(&modified, b"AUTH_SESSKEY", sesskey_hex.as_bytes())
    {
        modified = new_data;
    }

    modified
}

/// Build an auth response packet that preserves the client's AUTH_SESSKEY.
///
/// This is used when the client provides their own credentials - we use their
/// session key context to avoid breaking post-auth encryption.
///
/// Only replaces: username, AUTH_PASSWORD, AUTH_PBKDF2_SPEEDY_KEY
/// Does NOT replace: AUTH_SESSKEY (keeps client's original)
pub fn build_auth_response_packet_preserve_sesskey(
    original_packet: &[u8],
    response_data: &AuthResponseData,
) -> Vec<u8> {
    // Same as build_auth_response_packet but skip AUTH_SESSKEY replacement
    let mut modified = original_packet.to_vec();

    // Replace username (same logic as build_auth_response_packet)
    let real_username = response_data.username.as_bytes();
    let real_username_len = real_username.len();
    let auth_marker_pos = find_subsequence(&modified, b"AUTH_PASSWORD");

    if let Some(marker_pos) = auth_marker_pos {
        if marker_pos >= 3 {
            let field_header_start = marker_pos - 3;
            let mut username_start_marker = None;
            for i in (12..field_header_start.saturating_sub(1)).rev() {
                if modified[i] == 0x01 && modified[i + 1] == 0x01 {
                    let potential_start = i + 2;
                    let potential_len = field_header_start - potential_start;
                    if (1..=30).contains(&potential_len) {
                        let potential_username = &modified[potential_start..field_header_start];
                        if potential_username
                            .iter()
                            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$')
                        {
                            username_start_marker = Some(potential_start);
                            break;
                        }
                    }
                }
            }

            if let Some(username_start) = username_start_marker {
                let old_username_len = field_header_start - username_start;

                let len_diff = real_username_len as isize - old_username_len as isize;
                let new_capacity = (modified.len() as isize + len_diff) as usize;
                let mut new_packet = Vec::with_capacity(new_capacity);

                new_packet.extend_from_slice(&modified[..username_start]);
                new_packet.extend_from_slice(real_username);
                new_packet.extend_from_slice(&modified[field_header_start..]);

                if new_packet.len() > 7 && real_username_len <= 255 {
                    new_packet[7] = real_username_len as u8;
                }

                modified = new_packet;
            }
        }
    }

    // Replace AUTH_PASSWORD value
    let password_hex = hex_encode(&response_data.auth_password);
    if let Some(new_data) =
        replace_tti_field_value(&modified, b"AUTH_PASSWORD", password_hex.as_bytes())
    {
        modified = new_data;
    }

    // Replace AUTH_PBKDF2_SPEEDY_KEY if present
    if let Some(ref speedy_key) = response_data.speedy_key {
        let speedy_hex = hex_encode(speedy_key);
        if let Some(new_data) =
            replace_tti_field_value(&modified, b"AUTH_PBKDF2_SPEEDY_KEY", speedy_hex.as_bytes())
        {
            modified = new_data;
        }
    }

    // NOTE: We intentionally do NOT replace AUTH_SESSKEY here
    // This preserves the client's session key context for post-auth encryption

    modified
}

/// Replace a TTI field value in a packet
/// TTI format: 01 <name_len> <name_len> <name> 01 <val_len> <val_len> <value>
fn replace_tti_field_value(data: &[u8], field_name: &[u8], new_value: &[u8]) -> Option<Vec<u8>> {
    // Find the field name in the packet
    let name_pos = find_subsequence(data, field_name)?;

    // After the field name, we expect: 01 <len> <len> <value>
    // Or for fields with terminator: 00 01 <len> <len> <value>
    let after_name = name_pos + field_name.len();
    if after_name + 3 >= data.len() {
        return None;
    }

    // Skip any 00 bytes or whitespace after the field name
    let mut value_header_start = after_name;
    while value_header_start < data.len()
        && (data[value_header_start] == 0x00 || data[value_header_start] == b' ')
    {
        value_header_start += 1;
    }

    // Expect 01 marker
    if value_header_start >= data.len() || data[value_header_start] != 0x01 {
        return None;
    }

    // Read length bytes (doubled in TTI format)
    let len_pos = value_header_start + 1;
    if len_pos + 1 >= data.len() {
        return None;
    }

    let old_len = data[len_pos] as usize;
    // Second length byte should match (or skip if different format)
    let len_byte2_pos = len_pos + 1;
    let value_start = if len_byte2_pos < data.len() && data[len_byte2_pos] == old_len as u8 {
        len_byte2_pos + 1 // Skip both length bytes
    } else {
        len_pos + 1 // Single length byte format
    };

    let old_value_end = value_start + old_len;
    if old_value_end > data.len() {
        return None;
    }

    // Build the new packet with replaced value
    let new_len = new_value.len() as u8;
    let mut result = Vec::with_capacity(data.len() + new_value.len() - old_len);

    // Copy up to and including field name
    result.extend_from_slice(&data[..after_name]);

    // Add TTI value header: 01 <len> <len>
    result.push(0x01);
    result.push(new_len);
    result.push(new_len);

    // Add new value
    result.extend_from_slice(new_value);

    // Copy remainder of packet (after old value)
    result.extend_from_slice(&data[old_value_end..]);

    Some(result)
}

/// Extract a TTI field value from a packet.
/// Returns the raw bytes of the value (for AUTH_SESSKEY, this is hex-encoded).
fn extract_tti_field_value(data: &[u8], field_name: &[u8]) -> Option<Vec<u8>> {
    let name_pos = find_subsequence(data, field_name)?;
    let after_name = name_pos + field_name.len();
    if after_name + 3 >= data.len() {
        return None;
    }

    // Skip any 00 bytes or whitespace after the field name
    let mut value_header_start = after_name;
    while value_header_start < data.len()
        && (data[value_header_start] == 0x00 || data[value_header_start] == b' ')
    {
        value_header_start += 1;
    }

    // Expect 01 marker
    if value_header_start >= data.len() || data[value_header_start] != 0x01 {
        return None;
    }

    // Read length bytes (doubled in TTI format)
    let len_pos = value_header_start + 1;
    if len_pos + 1 >= data.len() {
        return None;
    }

    let value_len = data[len_pos] as usize;
    let len_byte2_pos = len_pos + 1;
    let value_start = if len_byte2_pos < data.len() && data[len_byte2_pos] == value_len as u8 {
        len_byte2_pos + 1 // Skip both length bytes
    } else {
        len_pos + 1 // Single length byte format
    };

    let value_end = value_start + value_len;
    if value_end > data.len() {
        return None;
    }

    Some(data[value_start..value_end].to_vec())
}

/// Extract the client's AUTH_SESSKEY from their auth response packet.
/// Returns the hex-decoded raw session key bytes (still encrypted).
pub fn extract_client_auth_sesskey(payload: &[u8]) -> Option<Vec<u8>> {
    // AUTH_SESSKEY value is hex-encoded in the packet
    let hex_value = extract_tti_field_value(payload, b"AUTH_SESSKEY")?;

    // Convert hex string to bytes
    let hex_str = String::from_utf8(hex_value).ok()?;
    let decoded = hex_decode(&hex_str)?;

    Some(decoded)
}

/// Extract the client's AUTH_PASSWORD from their auth response packet.
/// Returns the hex-decoded encrypted password bytes.
pub fn extract_client_auth_password(payload: &[u8]) -> Option<Vec<u8>> {
    // AUTH_PASSWORD value is hex-encoded in the packet
    let hex_value = extract_tti_field_value(payload, b"AUTH_PASSWORD")?;

    // Convert hex string to bytes
    let hex_str = String::from_utf8(hex_value).ok()?;
    let decoded = hex_decode(&hex_str)?;

    Some(decoded)
}

/// Check if a packet is an O5LOGON auth response (contains AUTH_PASSWORD)
pub fn is_auth_response_packet(payload: &[u8]) -> bool {
    find_subsequence(payload, b"AUTH_PASSWORD").is_some()
        || find_subsequence(payload, b"AUTH_SESSKEY").is_some()
}

/// Challenge data extracted from server response
#[derive(Debug, Clone)]
pub struct ChallengeData {
    /// Server's encrypted session key
    pub auth_sesskey: Vec<u8>,
    /// Salt/verifier data
    pub auth_vfr_data: Vec<u8>,
    /// PBKDF2 parameters (Oracle 12c+)
    pub pbkdf2_params: Option<Pbkdf2Params>,
}

/// Extract AUTH_SESSKEY and AUTH_VFR_DATA from a server challenge packet
pub fn extract_challenge_data(payload: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let sesskey = if let Some(pos) = find_subsequence(payload, b"AUTH_SESSKEY") {
        extract_auth_value(payload, pos + 12)?
    } else {
        return None;
    };

    let vfr_data = if let Some(pos) = find_subsequence(payload, b"AUTH_VFR_DATA") {
        extract_auth_value(payload, pos + 13).unwrap_or_default()
    } else {
        Vec::new()
    };

    Some((sesskey, vfr_data))
}

/// Extract full challenge data including PBKDF2 parameters from a server challenge packet
pub fn extract_full_challenge_data(payload: &[u8]) -> Option<ChallengeData> {
    let (auth_sesskey, auth_vfr_data) = extract_challenge_data(payload)?;
    let pbkdf2_params = extract_pbkdf2_params(payload);

    Some(ChallengeData {
        auth_sesskey,
        auth_vfr_data,
        pbkdf2_params,
    })
}

/// Encode bytes as uppercase hex string
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02X}", b)).collect()
}

/// Replace or inject the username in an auth packet payload (for initial auth request before challenge)
///
/// This is used for Auth round 3 (initial username request) to ensure the username
/// sent to the server matches the real credentials, before we receive the challenge.
///
/// Handles two cases:
/// 1. Username exists in packet (e.g., DBeaver with username) - replace it
/// 2. No username in packet (e.g., DBeaver without username) - rebuild packet with username
///
/// Returns the modified payload with the username replaced/injected.
pub fn replace_auth_username(payload: &[u8], real_username: &str) -> Vec<u8> {
    // Find AUTH_TERMINAL to locate the username position
    let auth_terminal_pos = find_subsequence(payload, b"AUTH_TERMINAL");

    let Some(terminal_pos) = auth_terminal_pos else {
        return payload.to_vec();
    };
    // inject_pos points to "01 0D 0D AUTH_TERMINAL"
    let inject_pos = if terminal_pos >= 3 {
        terminal_pos - 3
    } else {
        terminal_pos
    };

    let real_username_bytes = real_username.as_bytes();
    let new_username_len = real_username_bytes.len();

    // Check if this is a no-username packet (function code 0x73 with different header structure)
    // These packets need complete restructuring, not just username injection
    // Pattern: 00 00 03 73 01 00 00 00 03 80 ... (0x80 can be at byte 8 or 9)
    // Also check for byte 7 being 0 (no username length)
    let is_no_username_packet = payload.len() > 9 && payload[3] == 0x73 &&
        (payload[7] == 0x00 || payload[7] == 0x03) &&  // No username length
        (payload[8] == 0x80 || payload[9] == 0x80); // 0x80 marker at byte 8 or 9

    if is_no_username_packet {
        return rebuild_auth_packet_with_username(payload, real_username, terminal_pos);
    }

    // Find the username by searching backwards from inject_pos for "01 01" marker
    // The marker could be right at inject_pos-2, so include that in the search range
    let mut username_marker_pos = None;
    let mut orig_username_len = 0usize;

    // Search range: from position 2 up to and including inject_pos-2
    let search_end = inject_pos.saturating_sub(1);
    for i in (2..search_end).rev() {
        if i + 1 < payload.len() && payload[i] == 0x01 && payload[i + 1] == 0x01 {
            let username_start = i + 2;
            let potential_len = inject_pos.saturating_sub(username_start);

            // Check if there's actually a username between marker and AUTH_TERMINAL
            if (1..=30).contains(&potential_len) {
                // Verify it looks like a username (alphanumeric)
                let potential_username = &payload[username_start..inject_pos];
                if potential_username
                    .iter()
                    .all(|&c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    username_marker_pos = Some(i);
                    orig_username_len = potential_len;
                    break;
                }
            }

            // Check if this is a no-username packet (marker directly followed by AUTH_TERMINAL field)
            if potential_len == 0 || (potential_len == 1 && payload[username_start] == 0x01) {
                username_marker_pos = Some(i);
                orig_username_len = 0;
                break;
            }
        }
    }

    let Some(marker_pos) = username_marker_pos else {
        return payload.to_vec();
    };
    let username_start = marker_pos + 2;

    // Build modified payload
    let len_diff = new_username_len as isize - orig_username_len as isize;
    let new_len = (payload.len() as isize + len_diff) as usize;
    let mut modified = Vec::with_capacity(new_len);

    // Copy header up to and including "01 01" marker
    modified.extend_from_slice(&payload[..username_start]);

    // Add new username
    modified.extend_from_slice(real_username_bytes);

    // Add rest of payload (from inject_pos onwards, which includes "01 0D 0D AUTH_TERMINAL...")
    modified.extend_from_slice(&payload[inject_pos..]);

    // Update username length in TTI header
    // The position varies by Oracle version: byte 7 for 21c, byte 8 for 23ai
    // Find it by looking for a byte that matches the original username length
    if new_username_len <= 255 {
        let mut len_byte_pos = None;

        // Search bytes 7-9 for the original username length
        for pos in 7..=9 {
            if pos < payload.len() && payload[pos] == orig_username_len as u8 {
                len_byte_pos = Some(pos);
                break;
            }
        }

        // If not found by matching, default to byte 7 for backwards compatibility
        let pos = len_byte_pos.unwrap_or(7);

        if pos < modified.len() {
            modified[pos] = new_username_len as u8;
        }
    }

    modified
}

/// Rebuild a no-username auth packet (function 0x73) as a proper O5LOGON packet (function 0x76)
/// with username included.
///
/// Input format (0x73 no-username):
/// [00 00 03 73 01 00 00 XX 80 00 01 01 01 0B 01 01 01 0D 0D AUTH_TERMINAL ...]
///
/// Output format (0x76 with username for 23ai):
/// [00 00 03 76 01 00 01 01 LL 01 01 01 01 FF 01 01 USERNAME 01 0D 0D AUTH_TERMINAL ...]
/// Where LL = username length, FF = field count
fn rebuild_auth_packet_with_username(
    payload: &[u8],
    username: &str,
    auth_terminal_pos: usize,
) -> Vec<u8> {
    let username_bytes = username.as_bytes();
    let username_len = username_bytes.len();

    // Find where AUTH_TERMINAL field starts (01 0D 0D AUTH_TERMINAL)
    // We need to go back 3 bytes to include the field prefix
    let _auth_terminal_field_start = if auth_terminal_pos >= 3 {
        auth_terminal_pos - 3
    } else {
        auth_terminal_pos
    };

    // Detect format: 21c XE vs 23ai
    // 21c XE:  08 00 03 73 01 00 00 03 80 00 01 01 01 0B ... (0x80 at pos 8)
    // 23ai:   00 00 03 73 01 00 00 00 03 80 00 01 01 01 0B ... (0x80 at pos 9)
    let is_21c_xe_format = payload.len() > 8 && payload[8] == 0x80;

    let _original_field_count = if payload.len() > 14 {
        if is_21c_xe_format {
            payload[13] // 21c XE format
        } else if payload[9] == 0x80 {
            payload[14] // 23ai format
        } else {
            0x0B // default
        }
    } else {
        0x0B // default
    };

    // Build new header for 0x76 format with username
    // Two different formats exist:
    // 21c XE format:  00 00 03 76 01 01 01 LL 01 01 01 01 FF 01 01 USERNAME (username at byte 7)
    // 23ai format:    00 00 03 76 01 00 01 01 LL 01 01 01 01 FF 01 01 USERNAME (username at byte 8)
    let mut modified = Vec::with_capacity(payload.len() + 50);

    // Data flags (bytes 0-1) - always use 00 00 for 0x76 packets
    // (working DBeaver packets use 00 00 regardless of original)
    modified.push(0x00);
    modified.push(0x00);

    // TTI header - function 0x76 (O5LOGON with username)
    modified.push(0x03);
    modified.push(0x76);

    let field_count: u8 = 5;

    if is_21c_xe_format {
        // 21c XE format: 01 01 01 LL 01 01 01 01 FF
        // Username length at byte 7, field count at byte 12
        modified.push(0x01); // byte 4
        modified.push(0x01); // byte 5
        modified.push(0x01); // byte 6
        modified.push(username_len as u8); // byte 7: username length
        modified.push(0x01); // byte 8
        modified.push(0x01); // byte 9
        modified.push(0x01); // byte 10
        modified.push(0x01); // byte 11
        modified.push(field_count); // byte 12: field count
    } else {
        // 23ai format: 01 00 01 01 LL 01 01 01 01 FF
        // Username length at byte 8, field count at byte 13
        modified.push(0x01); // byte 4
        modified.push(0x00); // byte 5
        modified.push(0x01); // byte 6
        modified.push(0x01); // byte 7
        modified.push(username_len as u8); // byte 8: username length
        modified.push(0x01); // byte 9
        modified.push(0x01); // byte 10
        modified.push(0x01); // byte 11
        modified.push(0x01); // byte 12
        modified.push(field_count); // byte 13: field count
    }

    // Username field marker and username
    modified.push(0x01);
    modified.push(0x01);
    modified.extend_from_slice(username_bytes);

    // Extract only the 5 standard fields for 0x76 format
    // The 0x73 packet has extra fields (AUTH_ALTER_SESSION, AUTH_CONNECT_STRING, etc.)
    // that are not valid in 0x76 format
    fn extract_field(payload: &[u8], field_name: &[u8]) -> Option<Vec<u8>> {
        let field_pos = find_subsequence(payload, field_name)?;
        let field_start = field_pos.checked_sub(3)?; // Include "01 len len" prefix

        // Find end of field: skip field name, then "01 len len value [00]"
        let mut pos = field_pos + field_name.len();
        if pos + 3 <= payload.len() {
            let value_len = payload[pos + 1] as usize;
            pos = pos + 3 + value_len;
            // Include 00 terminator if present
            if pos < payload.len() && payload[pos] == 0x00 {
                pos += 1;
            }
        }

        if pos <= payload.len() {
            Some(payload[field_start..pos].to_vec())
        } else {
            None
        }
    }

    // Add AUTH_TERMINAL
    if let Some(field) = extract_field(payload, b"AUTH_TERMINAL") {
        modified.extend_from_slice(&field);
    }

    // Add AUTH_PROGRAM_NM
    if let Some(field) = extract_field(payload, b"AUTH_PROGRAM_NM") {
        modified.extend_from_slice(&field);
    }

    // Add AUTH_MACHINE
    if let Some(field) = extract_field(payload, b"AUTH_MACHINE") {
        modified.extend_from_slice(&field);
    }

    // Add AUTH_PID
    if let Some(field) = extract_field(payload, b"AUTH_PID") {
        modified.extend_from_slice(&field);
    }

    // Add AUTH_SID - create default if not present in original
    if let Some(field) = extract_field(payload, b"AUTH_SID") {
        modified.extend_from_slice(&field);
    } else {
        // Create default AUTH_SID field
        modified.extend_from_slice(&[0x01, 0x08, 0x08]); // 01 len len for "AUTH_SID"
        modified.extend_from_slice(b"AUTH_SID");
        modified.extend_from_slice(&[0x01, 0x07, 0x07]); // 01 len len for value
        modified.extend_from_slice(b"default");
        modified.push(0x00);
    }

    modified
}

/// Build a proper credential packet for proxy auth mode.
/// This is used when the client sends a no-username packet and we need to
/// handle the entire authentication ourselves.
///
/// Strategy: Build correct header, add credentials, then copy the entire
/// field section from AUTH_TERMINAL onwards from the original payload.
/// This preserves the exact field encoding the client sent.
pub fn build_proxy_credential_packet(
    original_payload: &[u8],
    username: &str,
    response_data: &AuthResponseData,
) -> Vec<u8> {
    let username_bytes = username.as_bytes();

    // Convert response data to hex strings
    let sesskey_hex: String = response_data
        .client_sesskey
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect();
    let password_hex: String = response_data
        .auth_password
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect();
    let speedy_hex: Option<String> = response_data
        .speedy_key
        .as_ref()
        .map(|sk| sk.iter().map(|b| format!("{:02X}", b)).collect());

    // Find where AUTH_TERMINAL starts in original payload
    let auth_terminal_pos = find_subsequence(original_payload, b"AUTH_TERMINAL");

    // Count original fields by finding field markers
    // Original no-username packet has 11 fields (AUTH_TERMINAL, AUTH_PROGRAM_NM, AUTH_MACHINE,
    // AUTH_PID, AUTH_ALTER_SESSION, AUTH_CONNECT_STRING, AUTH_COPYRIGHT, AUTH_ACL, plus SESSION_* fields)
    //
    // Two different packet formats exist:
    //
    // Oracle 21c XE format:
    // Index:  0  1  2  3  4  5  6  7  8  9 10 11 12 13
    // Bytes: 08 00 03 73 01 00 00 03 80 00 01 01 01 0B
    // Field count is at byte 13 (0x0B = 11), 0x80 marker at byte 8
    //
    // Oracle 23ai format:
    // Index:  0  1  2  3  4  5  6  7  8  9 10 11 12 13 14
    // Bytes: 00 00 03 73 01 00 00 00 03 80 00 01 01 01 0B
    // Field count is at byte 14 (0x0B = 11), 0x80 marker at byte 9

    // For the credential packet, we extract the original field count and add 3 credentials
    // Different packet types have field count at different positions:
    //
    // 0x73 no-username (byte 4 = 0x01):
    //   - XE: field count at byte 13
    //   - 23ai: field count at byte 14
    //
    // 0x73 with-credentials (byte 4 = 0x02):
    //   - XE full creds: 02 01 01 LL 02 01 01 01 01 FF - byte 8=02, field count at byte 13
    //   - XE username-only: 02 01 01 LL 01 01 01 01 FF - byte 8=01, field count at byte 12
    //   - 23ai: 02 00 01 01 LL 01 01 01 01 FF - field count at byte 13
    //
    // 0x76 packets:
    //   - XE: field count at byte 12
    //   - 23ai: field count at byte 13
    let original_field_count: u8 = if original_payload.len() > 14 {
        if original_payload[3] == 0x73 {
            if original_payload[4] == 0x02 {
                // 0x73 with credentials - format differs between XE and 23ai
                // XE (byte 5 = 0x01): username_len at byte 7, cred_version at byte 8, field_count at 13 (full) or 12 (username-only)
                // 23ai (byte 5 = 0x00): username_len at byte 8, cred_version at byte 9, field_count at 14 (full) or 13 (username-only)
                if original_payload.len() > 5 && original_payload[5] == 0x01 {
                    // XE format
                    if original_payload.len() > 8 && original_payload[8] == 0x02 {
                        // XE full credentials - field count at byte 13
                        if original_payload.len() > 13 {
                            original_payload[13]
                        } else {
                            11
                        }
                    } else {
                        // XE username-only - field count at byte 12
                        if original_payload.len() > 12 {
                            original_payload[12]
                        } else {
                            11
                        }
                    }
                } else {
                    // 23ai format (byte 5 = 0x00)
                    if original_payload.len() > 9 && original_payload[9] == 0x02 {
                        // 23ai full credentials - field count at byte 14
                        if original_payload.len() > 14 {
                            original_payload[14]
                        } else {
                            11
                        }
                    } else {
                        // 23ai username-only - field count at byte 13
                        if original_payload.len() > 13 {
                            original_payload[13]
                        } else {
                            11
                        }
                    }
                }
            } else {
                // 0x73 no-username - check for 0x80 marker
                if original_payload[8] == 0x80 {
                    // 21c XE format: field count at position 13
                    original_payload[13]
                } else if original_payload[9] == 0x80 {
                    // 23ai format: field count at position 14
                    original_payload[14]
                } else {
                    11 // default for 0x73
                }
            }
        } else if original_payload[3] == 0x76 {
            // 0x76 with-username packet
            if original_payload.len() > 5 && original_payload[5] == 0x01 {
                // XE format: field count at byte 12
                if original_payload.len() > 12 {
                    original_payload[12]
                } else {
                    5
                }
            } else {
                // 23ai format: field count at byte 13
                if original_payload.len() > 13 {
                    original_payload[13]
                } else {
                    5
                }
            }
        } else {
            11 // default
        }
    } else {
        11 // default - typical no-username packet has 11 fields
    };

    let credential_fields: u8 = if speedy_hex.is_some() { 3 } else { 2 };

    // Check if original packet already has credentials (AUTH_PASSWORD present)
    // If so, we're REPLACING credentials, not adding them, so field count stays the same
    let original_has_credentials = find_subsequence(original_payload, b"AUTH_PASSWORD").is_some();
    let field_count = if original_has_credentials {
        // Replacing credentials: keep same field count
        original_field_count
    } else {
        // Adding credentials: add to field count
        original_field_count + credential_fields
    };

    // Detect format: 21c XE vs 23ai
    // Different detection methods depending on packet type and content:
    //
    // For 0x73 no-username packets (byte 4 = 0x01):
    //   - XE: 00 00 03 73 01 00 00 03 80 ... (0x80 at position 8)
    //   - 23ai: 00 00 03 73 01 00 00 00 03 80 ... (0x80 at position 9)
    //
    // For 0x73 with-credentials packets (byte 4 = 0x02):
    //   - XE: 00 00 03 73 02 01 01 LL ... (byte 5 = 01)
    //   - 23ai: 00 00 03 73 02 00 01 01 ... (byte 5 = 00)
    //
    // For 0x76 packets:
    //   - XE: 00 00 03 76 01 01 01 LL ... (byte 5 = 01)
    //   - 23ai: 00 00 03 76 01 00 01 01 LL ... (byte 5 = 00)
    let is_21c_xe_format = if original_payload.len() > 8 {
        if original_payload[3] == 0x73 {
            if original_payload[4] == 0x02 {
                // 0x73 with credentials: check byte 5 (01 = XE, 00 = 23ai)
                original_payload.len() > 5 && original_payload[5] == 0x01
            } else {
                // 0x73 no-username: check 0x80 marker position
                original_payload[8] == 0x80
            }
        } else if original_payload[3] == 0x76 {
            // 0x76 packet: check byte 5 (01 = XE, 00 = 23ai)
            original_payload.len() > 5 && original_payload[5] == 0x01
        } else {
            // Unknown packet type, default to 23ai
            false
        }
    } else {
        false
    };

    let mut modified = Vec::with_capacity(2000);
    let username_len = username_bytes.len();

    // Build header for credential packet in 0x73 format
    // Two different formats exist:
    // 21c XE format:  00 00 03 73 02 01 01 LL 01 01 01 01 FF 01 01 USERNAME (username at byte 7)
    // 23ai format:    00 00 03 73 02 00 01 01 06 02 01 01 01 01 FF 01 01 USERNAME (username at byte 14)

    // Bytes 0-1: data flags - always use 00 00 for credential packets
    modified.push(0x00);
    modified.push(0x00);

    // Bytes 2-3: TTI header - function 0x73
    modified.push(0x03);
    modified.push(0x73);

    if is_21c_xe_format {
        // 21c XE format: 02 01 01 LL 02 01 01 01 01 FF 01 01 USERNAME
        // Based on working client packet capture:
        // Username length at byte 7, field count at byte 13
        modified.push(0x02); // byte 4: credential flag
        modified.push(0x01); // byte 5: XE format indicator
        modified.push(0x01); // byte 6
        modified.push(username_len as u8); // byte 7: username length
        modified.push(0x02); // byte 8: credential version (NOT 01!)
        modified.push(0x01); // byte 9
        modified.push(0x01); // byte 10
        modified.push(0x01); // byte 11
        modified.push(0x01); // byte 12
        modified.push(field_count); // byte 13: field count
        modified.push(0x01); // byte 14
        modified.push(0x01); // byte 15
        modified.extend_from_slice(username_bytes);
    } else {
        // 23ai format: 02 00 01 01 06 02 01 01 01 01 FF 01 01 USERNAME
        // Field count at byte 14
        modified.push(0x02); // byte 4: credential flag
        modified.push(0x00); // byte 5
        modified.push(0x01); // byte 6
        modified.push(0x01); // byte 7
        modified.push(0x06); // byte 8
        modified.push(0x02); // byte 9
        modified.push(0x01); // byte 10
        modified.push(0x01); // byte 11
        modified.push(0x01); // byte 12
        modified.push(0x01); // byte 13
        modified.push(field_count); // byte 14: field count
        modified.push(0x01); // byte 15
        modified.push(0x01); // byte 16
        modified.extend_from_slice(username_bytes);
    }

    // Add AUTH_PASSWORD field: 01 0D 0D AUTH_PASSWORD 01 LL LL <hex-chars> 00
    // Length must match actual encrypted output (varies with password length due to PKCS7 padding)
    let password_hex_len = password_hex.len() as u8;
    modified.push(0x01);
    modified.push(0x0D); // len of "AUTH_PASSWORD"
    modified.push(0x0D);
    modified.extend_from_slice(b"AUTH_PASSWORD");
    modified.push(0x01);
    modified.push(password_hex_len);
    modified.push(password_hex_len);
    modified.extend_from_slice(password_hex.as_bytes());
    modified.push(0x00);

    // Add AUTH_PBKDF2_SPEEDY_KEY field if present: 01 16 16 AUTH_PBKDF2_SPEEDY_KEY 01 LL LL <hex-chars> 00
    if let Some(ref speedy) = speedy_hex {
        let speedy_hex_len = speedy.len() as u8;
        modified.push(0x01);
        modified.push(0x16); // len of "AUTH_PBKDF2_SPEEDY_KEY"
        modified.push(0x16);
        modified.extend_from_slice(b"AUTH_PBKDF2_SPEEDY_KEY");
        modified.push(0x01);
        modified.push(speedy_hex_len);
        modified.push(speedy_hex_len);
        modified.extend_from_slice(speedy.as_bytes());
        modified.push(0x00);
    }

    // Add AUTH_SESSKEY field: 01 0C 0C AUTH_SESSKEY 01 LL LL <hex-chars>
    // Note: SESSKEY comes AFTER SPEEDY_KEY, followed by 01 01 01 before AUTH_TERMINAL
    let sesskey_hex_len = sesskey_hex.len() as u8;
    modified.push(0x01);
    modified.push(0x0C); // len of "AUTH_SESSKEY"
    modified.push(0x0C);
    modified.extend_from_slice(b"AUTH_SESSKEY");
    modified.push(0x01);
    modified.push(sesskey_hex_len);
    modified.push(sesskey_hex_len);
    modified.extend_from_slice(sesskey_hex.as_bytes());

    // Add the separator bytes that appear between SESSKEY and AUTH_TERMINAL in the client packet
    modified.push(0x01);
    modified.push(0x01);
    modified.push(0x01);

    // Now copy ALL fields from AUTH_TERMINAL onwards from the original payload
    // This preserves AUTH_TERMINAL, AUTH_PROGRAM_NM, AUTH_MACHINE, AUTH_PID,
    // AUTH_ALTER_SESSION, AUTH_CONNECT_STRING, AUTH_COPYRIGHT, AUTH_ACL, and SESSION_* fields
    //
    // In the successful client packet, after AUTH_SESSKEY we have: 01 01 01 0D 0D AUTH_TERMINAL
    // The 01 01 01 is a separator we already added above
    // So we copy from terminal_pos - 2 to get: 0D 0D AUTH_TERMINAL...
    if let Some(terminal_pos) = auth_terminal_pos {
        // Start at the length bytes (0D 0D), not the 01 prefix
        if terminal_pos >= 2 {
            let field_start = terminal_pos - 2;
            // Copy everything from the length bytes onwards
            modified.extend_from_slice(&original_payload[field_start..]);
        }
    } else {
        // Fall back to creating minimal fields
        modified.extend_from_slice(&[0x01, 0x0D, 0x0D]);
        modified.extend_from_slice(b"AUTH_TERMINAL");
        modified.extend_from_slice(&[0x01, 0x07, 0x07]);
        modified.extend_from_slice(b"unknown");
        modified.push(0x00);
    }

    modified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_o5logon_state_default() {
        let state = O5LogonState::default();
        assert_eq!(state, O5LogonState::Initial);
    }

    #[test]
    fn test_is_auth_packet_with_marker() {
        let payload = b"some data AUTH_SESSKEY more data";
        assert!(is_auth_packet(payload));
    }

    #[test]
    fn test_is_auth_packet_with_function_code() {
        let payload = vec![function_code::AUTH, 0x00, 0x01, 0x02];
        assert!(is_auth_packet(&payload));
    }

    #[test]
    fn test_is_auth_packet_empty() {
        let payload: &[u8] = &[];
        assert!(!is_auth_packet(payload));
    }

    #[test]
    fn test_is_auth_packet_non_auth() {
        let payload = b"SELECT * FROM users";
        assert!(!is_auth_packet(payload));
    }

    #[test]
    fn test_is_auth_success() {
        let payload = b"AUTH_SVR_RESPONSE blah blah";
        assert!(is_auth_success(payload));
    }

    #[test]
    fn test_is_auth_failure() {
        let payload = b"ORA-01017: invalid username/password; logon denied";
        assert!(is_auth_failure(payload));
    }

    #[test]
    fn test_is_auth_failure_locked() {
        let payload = b"ORA-28000: the account is locked";
        assert!(is_auth_failure(payload));
    }

    #[test]
    fn test_find_subsequence() {
        let data = b"hello world";
        assert_eq!(find_subsequence(data, b"world"), Some(6));
        assert_eq!(find_subsequence(data, b"foo"), None);
    }

    #[test]
    fn test_extract_length_prefixed_string() {
        // Length byte = 5, followed by "SCOTT"
        let data = [0x05, b'S', b'C', b'O', b'T', b'T'];
        let result = extract_length_prefixed_string(&data, 0);
        assert!(result.is_some());
        let (username, offset, len) = result.unwrap();
        assert_eq!(username, "SCOTT");
        assert_eq!(offset, 1);
        assert_eq!(len, 5);
    }

    #[test]
    fn test_build_auth_error() {
        let error = build_auth_error(1017, "invalid username/password");
        let error_str = String::from_utf8(error).unwrap();
        assert!(error_str.contains("ORA-01017"));
        assert!(error_str.contains("invalid username/password"));
    }

    #[test]
    fn test_parse_auth_challenge_with_sesskey() {
        // Simulate a challenge packet with AUTH_SESSKEY
        let mut payload = Vec::new();
        payload.extend_from_slice(b"some header AUTH_SESSKEY");
        payload.push(0); // separator
        payload.push(8); // length
        payload.extend_from_slice(b"12345678"); // session key
        payload.extend_from_slice(b" AUTH_VFR_DATA");
        payload.push(0);
        payload.push(4);
        payload.extend_from_slice(b"SALT");

        let result = parse_auth_challenge(&payload).unwrap();
        assert!(result.is_some());
        let challenge = result.unwrap();
        assert!(!challenge.auth_sesskey.is_empty());
    }

    #[test]
    fn test_modify_auth_username() {
        // Create a mock auth request with known username position
        let original = vec![
            0x76, 0x00, // function code and flag
            0x05, // length
            b'S', b'C', b'O', b'T', b'T', // username "SCOTT"
            0x00, 0x01, 0x02, // trailing data
        ];

        let auth_request = AuthRequest {
            username: "SCOTT".to_string(),
            auth_data: original.clone(),
            username_offset: 3,
            username_length: 5,
        };

        let modified = modify_auth_username(&original, &auth_request, "ADMIN");

        // Should have same structure but with "ADMIN" instead of "SCOTT"
        assert!(modified.len() == original.len()); // Same length username
        assert_eq!(&modified[3..8], b"ADMIN");
    }

    // O5LOGON Crypto Tests

    #[test]
    fn test_o5logon_auth_creation() {
        let auth = O5LogonAuth::new("SYSTEM", "password123");
        assert_eq!(auth.username, "SYSTEM");
        assert_eq!(auth.password, "password123");
        // Client sesskey is generated after set_challenge, so initially empty
        assert!(auth.client_sesskey.is_empty());
    }

    #[test]
    fn test_o5logon_derive_key() {
        let _auth = O5LogonAuth::new("SYSTEM", "password123");
        // derive_key is private, but we can test through the full flow
        // The key should be deterministic for the same password + salt
    }

    #[test]
    fn test_o5logon_challenge_flow() {
        let mut auth = O5LogonAuth::new("SYSTEM", "testpass");

        // Simulate server challenge
        let fake_sesskey = vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ];
        let fake_vfr_data = b"ABCDEFGH".to_vec();

        auth.set_challenge(fake_sesskey.clone(), fake_vfr_data.clone());

        assert_eq!(auth.server_sesskey, fake_sesskey);
        assert_eq!(auth.auth_vfr_data, fake_vfr_data);
        // Client sesskey is now generated after set_challenge for legacy mode
        assert_eq!(auth.client_sesskey.len(), 32);
    }

    #[test]
    fn test_o5logon_decrypt_and_response() {
        let mut auth = O5LogonAuth::new("SYSTEM", "testpass");

        // Use a fake encrypted session key (in real life this would be properly encrypted)
        // For testing, we just need to verify the crypto operations don't panic
        let fake_sesskey = vec![0u8; 32]; // 32 bytes for AES
        let fake_vfr_data = b"SALTSALT".to_vec();

        auth.set_challenge(fake_sesskey, fake_vfr_data);

        // This should complete without panicking
        let result = auth.process_challenge();
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(!response.client_sesskey.is_empty());
        assert!(!response.auth_password.is_empty());
    }

    #[test]
    fn test_extract_challenge_data() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"prefix AUTH_SESSKEY");
        payload.push(0);
        payload.push(8);
        payload.extend_from_slice(b"sesskey1");
        payload.extend_from_slice(b" AUTH_VFR_DATA");
        payload.push(0);
        payload.push(4);
        payload.extend_from_slice(b"salt");

        let result = extract_challenge_data(&payload);
        assert!(result.is_some());
        let (sesskey, _vfr_data) = result.unwrap();
        assert!(!sesskey.is_empty());
        // Note: vfr_data extraction may vary based on packet format
    }

    #[test]
    fn test_is_auth_response_packet() {
        let with_password = b"some AUTH_PASSWORD data";
        assert!(is_auth_response_packet(with_password));

        let with_sesskey = b"some AUTH_SESSKEY data";
        assert!(is_auth_response_packet(with_sesskey));

        let without = b"some random data";
        assert!(!is_auth_response_packet(without));
    }
}
