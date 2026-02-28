# Keeper Crypto Knowledge

## Tunnel Encryption (Phase 2 context)

**Source:** keeper-pam::use_cases::tunneling TunnelStartParams

- encryption_key: SecureBytes — HKDF-derived AES-256-GCM key (32 bytes), zeroized on drop
- nonce: SecureBytes — random nonce used for HKDF derivation (16 bytes), zeroized on drop
- turn_password: SecureString — short-lived TURN credential, zeroized on drop

### Boundary leak at CreateTubeRequest

When constructing CreateTubeRequest.settings, the HKDF-derived key bytes must be
encoded as a base64 string and inserted into a serde_json::Value::String. At this
point the key exists in heap memory as a plain String for the lifetime of the
HashMap entry.

The only safe way to handle this:
1. Build the HashMap immediately before calling create_tube()
2. Do NOT copy the key into SessionEntry or any long-lived struct
3. Drop the CreateTubeRequest (and its HashMap) immediately after create_tube() returns

The key should NOT be base64 URL_SAFE encoded here — use STANDARD encoding to match
what the WebRTC layer expects (consistent with SDP encoding rules).

## SecureString / SecureBytes (keeper-core)

- SecureString: wraps String, implements ZeroizeOnDrop
- SecureBytes: wraps Vec<u8>, implements ZeroizeOnDrop
- Secure32: [u8; 32] fixed-size, ZeroizeOnDrop
- Source: keeper-core::secure module

## Rule: No Debug/Display on secret types

TunnelStartParams has a manual Debug impl that redacts encryption_key, nonce,
turn_password. ConnectAsCredentials similarly redacts password, private_key, passphrase.
This pattern must be followed for any new struct containing SecureBytes/SecureString.
