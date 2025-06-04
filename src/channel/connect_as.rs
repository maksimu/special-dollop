use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce as AesNonce};
use anyhow::{anyhow, Result};
use hkdf::Hkdf;
use p256::{ecdh::diffie_hellman, PublicKey as P256PublicKey, SecretKey as P256SecretKey};
use serde::Deserialize;
use sha2::Sha256;

// Structs for deserializing connect_as JSON payload
#[derive(Deserialize, Debug)]
pub(crate) struct ConnectAsUser {
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) private_key: Option<String>,
    pub(crate) private_key_passphrase: Option<String>,
    pub(crate) passphrase: Option<String>,
    pub(crate) domain: Option<String>,
    pub connect_database: Option<String>,
    pub distinguished_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct ConnectAsPayload {
    pub(crate) user: Option<ConnectAsUser>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
}

/// Decrypts the "connect as" payload.
pub(crate) fn decrypt_connect_as_payload(
    gateway_private_key_hex: &str,
    client_public_key_bytes: &[u8],
    nonce_bytes: &[u8],
    encrypted_data: &[u8],
) -> Result<ConnectAsPayload, anyhow::Error> {
    // 1. Parse gateway's private key (hex to bytes, then to P256SecretKey)
    let private_key_bytes = ::hex::decode(gateway_private_key_hex)
        .map_err(|e| anyhow!("Failed to decode gateway private key hex: {}", e))?;
    let gateway_secret_key = P256SecretKey::from_slice(&private_key_bytes)
        .map_err(|e| anyhow!("Failed to create P256SecretKey from bytes: {}", e))?;

    // 2. Parse client's public key (bytes to P256PublicKey)
    let client_public_key =
        P256PublicKey::from_sec1_bytes(client_public_key_bytes).map_err(|e| {
            anyhow!(
                "Failed to parse client public key using from_sec1_bytes. Input len: {}. Error: {}",
                client_public_key_bytes.len(),
                e
            )
        })?;

    // 3. Perform ECDH to get shared secret
    let shared_secret = diffie_hellman(
        gateway_secret_key.to_nonzero_scalar(),
        client_public_key.as_affine(),
    );

    // 4. Use HKDF (SHA256) to derive a 32-byte symmetric key for AES-256-GCM
    let hk = Hkdf::<Sha256>::new(Some(&[]), shared_secret.raw_secret_bytes().as_ref());
    let mut symmetric_key_bytes = [0u8; 32];
    hk.expand(
        b"KEEPER_CONNECT_AS_ECIES_SECP256R1_HKDF_SHA256",
        &mut symmetric_key_bytes,
    )
    .map_err(|e| anyhow!("HKDF expand error: {}", e))?;

    // 5. Decrypt using AES-256-GCM
    let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&symmetric_key_bytes);
    let cipher = Aes256Gcm::new(key);
    let nonce = AesNonce::from_slice(nonce_bytes);

    let decrypted_bytes = cipher
        .decrypt(nonce, encrypted_data)
        .map_err(|e| anyhow!("AES-GCM decryption error: {}", e))?;

    // 6. Parse decrypted bytes as JSON into ConnectAsPayload struct
    let payload: ConnectAsPayload = ::serde_json::from_slice(&decrypted_bytes)
        .map_err(|e| anyhow!("Failed to deserialize decrypted JSON payload: {}", e))?;

    Ok(payload)
}
