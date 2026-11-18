use std::path::Path;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{NovaError, Result};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultKey([u8; KEY_LEN]);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VaultKey([REDACTED])")
    }
}

impl VaultKey {
    pub(crate) fn authentication_tag(&self, parts: &[&[u8]]) -> String {
        hmac_sha256_hex(&self.0, parts)
    }
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; KEY_LEN];
        getrandom::fill(&mut bytes)
            .map_err(|error| NovaError::Crypto(format!("key generation failed: {error}")))?;
        Ok(Self(bytes))
    }

    pub fn from_hex(value: &str) -> Result<Self> {
        let decoded =
            hex::decode(value.trim()).map_err(|error| NovaError::InvalidKey(error.to_string()))?;
        let bytes: [u8; KEY_LEN] = decoded.try_into().map_err(|value: Vec<u8>| {
            NovaError::InvalidKey(format!(
                "expected {KEY_LEN} bytes, received {}",
                value.len()
            ))
        })?;
        Ok(Self(bytes))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn write_new_file(&self, path: &Path) -> Result<()> {
        use std::fs::OpenOptions;
        use std::io::Write;

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(self.to_hex().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }

    pub fn read_file(path: &Path) -> Result<Self> {
        Self::from_hex(&std::fs::read_to_string(path)?)
    }

    pub(crate) fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<EncryptedBlob> {
        encrypt_with_key(&self.0, plaintext, aad)
    }

    pub(crate) fn decrypt(&self, blob: &EncryptedBlob, aad: &[u8]) -> Result<Vec<u8>> {
        decrypt_with_key(&self.0, blob, aad)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DataKey([u8; KEY_LEN]);

impl Drop for DataKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl DataKey {
    pub(crate) fn generate() -> Result<Self> {
        let mut bytes = [0_u8; KEY_LEN];
        getrandom::fill(&mut bytes)
            .map_err(|error| NovaError::Crypto(format!("data key generation failed: {error}")))?;
        Ok(Self(bytes))
    }

    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let bytes = bytes.try_into().map_err(|value: Vec<u8>| {
            NovaError::Crypto(format!(
                "invalid decrypted data key length: {}",
                value.len()
            ))
        })?;
        Ok(Self(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<EncryptedBlob> {
        encrypt_with_key(&self.0, plaintext, aad)
    }

    pub(crate) fn decrypt(&self, blob: &EncryptedBlob, aad: &[u8]) -> Result<Vec<u8>> {
        decrypt_with_key(&self.0, blob, aad)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EncryptedBlob {
    pub(crate) nonce: Vec<u8>,
    pub(crate) ciphertext: Vec<u8>,
}

fn encrypt_with_key(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> Result<EncryptedBlob> {
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| NovaError::Crypto(format!("cipher initialization failed: {error}")))?;
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce)
        .map_err(|error| NovaError::Crypto(format!("nonce generation failed: {error}")))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| NovaError::Crypto("encryption failed".into()))?;
    Ok(EncryptedBlob {
        nonce: nonce.to_vec(),
        ciphertext,
    })
}

fn decrypt_with_key(key: &[u8; KEY_LEN], blob: &EncryptedBlob, aad: &[u8]) -> Result<Vec<u8>> {
    if blob.nonce.len() != NONCE_LEN {
        return Err(NovaError::Crypto("invalid nonce length".into()));
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| NovaError::Crypto(format!("cipher initialization failed: {error}")))?;
    cipher
        .decrypt(
            XNonce::from_slice(&blob.nonce),
            Payload {
                msg: &blob.ciphertext,
                aad,
            },
        )
        .map_err(|_| NovaError::Crypto("authentication or decryption failed".into()))
}

pub(crate) fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(crate) fn hmac_sha256_hex(key: &[u8], parts: &[&[u8]]) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    for part in parts {
        mac.update(&(part.len() as u64).to_be_bytes());
        mac.update(part);
    }
    format!("hmac-sha256:{}", hex::encode(mac.finalize().into_bytes()))
}
