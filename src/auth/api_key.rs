use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::auth::error::{AuthError, AuthResult};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct GeneratedClientApiKey {
    pub plaintext: String,
    pub prefix: String,
    pub key_hash: String,
}

#[derive(Debug, Clone)]
pub struct ApiKeyHasher {
    pepper: [u8; 32],
}

impl ApiKeyHasher {
    pub fn new(pepper: [u8; 32]) -> Self {
        Self { pepper }
    }

    pub fn try_from_slice(pepper: &[u8]) -> AuthResult<Self> {
        let pepper: [u8; 32] = pepper
            .try_into()
            .map_err(|_| AuthError::InvalidPepperLength)?;
        Ok(Self::new(pepper))
    }

    pub fn generate_client_api_key(&self, _name: &str) -> GeneratedClientApiKey {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        // cpr_ 只用于客户端调用 /v1，不能复用成管理员登录密码。
        let plaintext = format!("cpr_{}", URL_SAFE_NO_PAD.encode(bytes));
        let prefix = plaintext.chars().take(12).collect::<String>();
        let key_hash = self.hash_client_api_key(&plaintext);
        GeneratedClientApiKey {
            plaintext,
            prefix,
            key_hash,
        }
    }

    pub fn hash_client_api_key(&self, plaintext: &str) -> String {
        let mut mac = self.new_mac();
        mac.update(plaintext.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    pub fn verify_client_api_key(&self, plaintext: &str, key_hash: &str) -> AuthResult<bool> {
        let Some(suffix) = plaintext.strip_prefix("cpr_") else {
            return Ok(false);
        };

        let decoded_suffix = URL_SAFE_NO_PAD.decode(suffix)?;
        let canonical = format!("cpr_{}", URL_SAFE_NO_PAD.encode(decoded_suffix));
        let expected = URL_SAFE_NO_PAD.decode(key_hash)?;
        let mut mac = self.new_mac();
        mac.update(canonical.as_bytes());
        Ok(mac.verify_slice(&expected).is_ok())
    }

    fn new_mac(&self) -> HmacSha256 {
        match HmacSha256::new_from_slice(&self.pepper) {
            Ok(mac) => mac,
            Err(error) => unreachable!("HMAC accepts any key size: {error}"),
        }
    }
}
