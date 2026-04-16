//! AES-256-GCM encryption for secret header values.
//!
//! Derives an encryption key from the server's `api_key` (in `secrets.toml`)
//! via HKDF-SHA256. Encrypted values are stored as base64-encoded
//! `nonce || ciphertext || tag` (12 + N + 16 bytes).

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// 32-byte encryption key derived from the api_key.
#[derive(Clone)]
pub struct EncryptionKey {
    raw: [u8; 32],
}

impl EncryptionKey {
    /// Derive an AES-256 key from the hex-encoded api_key using HKDF-SHA256.
    pub fn derive_from_api_key(api_key: &str) -> Self {
        let ikm = api_key.as_bytes();
        let salt = b"cascades-encrypted-headers-v1";
        let info = b"aes-256-gcm";

        let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
        let mut okm = [0u8; 32];
        hk.expand(info, &mut okm)
            .expect("HKDF expand failed — this should never happen with 32-byte output");
        Self { raw: okm }
    }
}

/// Encrypt a plaintext string. Returns base64-encoded `nonce || ciphertext`.
pub fn encrypt(key: &EncryptionKey, plaintext: &str) -> Result<String, CryptoError> {
    use aes_gcm::aead::OsRng;
    use aes_gcm::AeadCore;

    let cipher =
        Aes256Gcm::new_from_slice(&key.raw).map_err(|_| CryptoError::KeyInit)?;

    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| CryptoError::Encrypt)?;

    // nonce (12 bytes) || ciphertext+tag
    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce);
    combined.extend_from_slice(&ciphertext);

    Ok(base64_encode(&combined))
}

/// Decrypt a base64-encoded `nonce || ciphertext` back to plaintext.
pub fn decrypt(key: &EncryptionKey, encoded: &str) -> Result<String, CryptoError> {
    let combined = base64_decode(encoded)?;
    if combined.len() < 12 + 16 {
        return Err(CryptoError::InvalidCiphertext);
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher =
        Aes256Gcm::new_from_slice(&key.raw).map_err(|_| CryptoError::KeyInit)?;

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::Decrypt)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidUtf8)
}

/// Re-encrypt all values from `old_key` to `new_key`.
pub fn rotate_key(
    old_key: &EncryptionKey,
    new_key: &EncryptionKey,
    encrypted_values: &[String],
) -> Result<Vec<String>, CryptoError> {
    encrypted_values
        .iter()
        .map(|enc| {
            let plaintext = decrypt(old_key, enc)?;
            encrypt(new_key, &plaintext)
        })
        .collect()
}

// ─── Base64 helpers ──────────────────────────────────────────────────────────

const B64_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\n' | b'\r' | b' ' => continue,
            _ => return Err(CryptoError::InvalidBase64),
        } as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("failed to initialize cipher key")]
    KeyInit,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed (wrong key or corrupted data)")]
    Decrypt,
    #[error("ciphertext too short to contain nonce + tag")]
    InvalidCiphertext,
    #[error("decrypted data is not valid UTF-8")]
    InvalidUtf8,
    #[error("invalid base64 encoding")]
    InvalidBase64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> EncryptionKey {
        EncryptionKey::derive_from_api_key(
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
        )
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = test_key();
        let plaintext = "Bearer sk-secret-api-key-12345";
        let encrypted = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_produces_different_ciphertexts() {
        let key = test_key();
        let plaintext = "same-value";
        let e1 = encrypt(&key, plaintext).unwrap();
        let e2 = encrypt(&key, plaintext).unwrap();
        assert_ne!(e1, e2);
        assert_eq!(decrypt(&key, &e1).unwrap(), plaintext);
        assert_eq!(decrypt(&key, &e2).unwrap(), plaintext);
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let key1 = EncryptionKey::derive_from_api_key("key-one-aaaa-bbbb-cccc-dddd-eeee-fffff000");
        let key2 = EncryptionKey::derive_from_api_key("key-two-1111-2222-3333-4444-5555-66660000");
        let encrypted = encrypt(&key1, "secret").unwrap();
        assert!(decrypt(&key2, &encrypted).is_err());
    }

    #[test]
    fn empty_plaintext_roundtrips() {
        let key = test_key();
        let encrypted = encrypt(&key, "").unwrap();
        assert_eq!(decrypt(&key, &encrypted).unwrap(), "");
    }

    #[test]
    fn key_rotation() {
        let old_key =
            EncryptionKey::derive_from_api_key("old-api-key-0000111122223333444455556666");
        let new_key =
            EncryptionKey::derive_from_api_key("new-api-key-aaaabbbbccccddddeeee0000ffff");

        let values = vec!["secret-1".to_string(), "secret-2".to_string()];
        let encrypted: Vec<String> = values
            .iter()
            .map(|v| encrypt(&old_key, v).unwrap())
            .collect();

        let rotated = rotate_key(&old_key, &new_key, &encrypted).unwrap();
        assert!(decrypt(&old_key, &rotated[0]).is_err());
        assert_eq!(decrypt(&new_key, &rotated[0]).unwrap(), "secret-1");
        assert_eq!(decrypt(&new_key, &rotated[1]).unwrap(), "secret-2");
    }

    #[test]
    fn invalid_ciphertext_errors() {
        let key = test_key();
        assert!(decrypt(&key, "").is_err());
        assert!(decrypt(&key, "dG9vc2hvcnQ=").is_err());
    }

    #[test]
    fn base64_roundtrip() {
        let data = b"Hello, world! \x00\xff";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn derive_deterministic() {
        let k1 = EncryptionKey::derive_from_api_key("same-key");
        let k2 = EncryptionKey::derive_from_api_key("same-key");
        assert_eq!(k1.raw, k2.raw);
    }

    #[test]
    fn derive_different_keys_differ() {
        let k1 = EncryptionKey::derive_from_api_key("key-a");
        let k2 = EncryptionKey::derive_from_api_key("key-b");
        assert_ne!(k1.raw, k2.raw);
    }
}
