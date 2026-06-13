use std::{fs, path::Path};

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, OsRng},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid secret key length")]
    InvalidKeyLength,
    #[error("secret encryption failed")]
    Encrypt,
    #[error("secret decryption failed")]
    Decrypt,
    #[error("invalid nonce length")]
    InvalidNonceLength,
    #[error("invalid secret encoding: {0}")]
    Decode(#[from] base64::DecodeError),
    #[error("unsupported secret version")]
    UnsupportedVersion,
    #[error("secret is not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("secret key file io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored secret key must decode to 32 bytes")]
    InvalidStoredKeyLength,
}

pub type CryptoResult<T> = Result<T, CryptoError>;

#[derive(Clone)]
pub struct SecretBox {
    key: [u8; 32],
}

impl SecretBox {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> CryptoResult<Self> {
        let path = path.as_ref();
        if path.exists() {
            let encoded = fs::read_to_string(path)?;
            let key = decode_key(encoded.trim())?;
            return Ok(Self::new(key));
        }

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        fs::write(path, URL_SAFE_NO_PAD.encode(key))?;
        Ok(Self::new(key))
    }

    pub fn encrypt(&self, plaintext: &SecretString) -> CryptoResult<String> {
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|_| CryptoError::InvalidKeyLength)?;
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.expose_secret().as_bytes())
            .map_err(|_| CryptoError::Encrypt)?;
        Ok(format!(
            "v1:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    pub fn decrypt(&self, encoded: &str) -> CryptoResult<SecretString> {
        let mut parts = encoded.split(':');
        let version = parts.next().unwrap_or_default();
        let nonce = parts.next().unwrap_or_default();
        let ciphertext = parts.next().unwrap_or_default();
        if version != "v1" {
            return Err(CryptoError::UnsupportedVersion);
        }
        let nonce: [u8; 12] = URL_SAFE_NO_PAD
            .decode(nonce)?
            .try_into()
            .map_err(|_| CryptoError::InvalidNonceLength)?;
        let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext)?;
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|_| CryptoError::InvalidKeyLength)?;
        let nonce = Nonce::from(nonce);
        let plaintext = cipher
            .decrypt(&nonce, ciphertext.as_ref())
            .map_err(|_| CryptoError::Decrypt)?;
        Ok(SecretString::new(String::from_utf8(plaintext)?.into()))
    }
}

fn decode_key(encoded: &str) -> CryptoResult<[u8; 32]> {
    URL_SAFE_NO_PAD
        .decode(encoded)?
        .try_into()
        .map_err(|_| CryptoError::InvalidStoredKeyLength)
}
