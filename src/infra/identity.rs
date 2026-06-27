//! 身份哈希与本地认证相关原语 —— 管理员密码哈希、客户端 API Key 哈希。

use std::{fs, path::Path};

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::Sha256;
use thiserror::Error;

// ---------------------------------------------------------------------------
// AuthError / AuthResult
// ---------------------------------------------------------------------------

/// 身份原语错误。
#[derive(Debug, Error)]
pub enum AuthError {
    /// 密码哈希错误。
    #[error("password hash error: {0}")]
    PasswordHash(String),
    /// API Key 编码错误。
    #[error("invalid api key encoding: {0}")]
    ApiKeyEncoding(#[from] base64::DecodeError),
    /// Pepper 长度错误。
    #[error("invalid api key pepper length")]
    InvalidPepperLength,
    /// Pepper 文件读写错误。
    #[error("api key pepper file io error: {0}")]
    PepperIo(#[from] std::io::Error),
}

/// 身份原语结果。
pub type AuthResult<T> = Result<T, AuthError>;

impl From<argon2::password_hash::Error> for AuthError {
    fn from(value: argon2::password_hash::Error) -> Self {
        Self::PasswordHash(value.to_string())
    }
}

// ---------------------------------------------------------------------------
// 管理员密码
// ---------------------------------------------------------------------------

/// 生成管理员密码哈希。
pub fn hash_admin_password(password: &str) -> AuthResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

/// 验证管理员密码。
pub fn verify_admin_password(password: &str, hash: &str) -> AuthResult<bool> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// ---------------------------------------------------------------------------
// 客户端 API Key
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

/// 客户端 API Key 派生结果。
#[derive(Debug, Clone)]
pub struct GeneratedClientApiKey {
    /// 仅显示一次的明文密钥。
    pub key: String,
    /// 前缀缓存。
    pub prefix: String,
    /// 持久化哈希。
    pub key_hash: String,
}

/// API Key 哈希器。
#[derive(Debug, Clone)]
pub struct ApiKeyHasher {
    pepper: [u8; 32],
}

impl ApiKeyHasher {
    /// 使用给定 pepper 构造哈希器。
    pub fn new(pepper: [u8; 32]) -> Self {
        Self { pepper }
    }

    /// 从文件加载或生成 pepper。
    pub fn load_or_create(path: impl AsRef<Path>) -> AuthResult<Self> {
        let path = path.as_ref();
        if path.exists() {
            let encoded = fs::read_to_string(path)?;
            let pepper = URL_SAFE_NO_PAD.decode(encoded.trim())?;
            return Self::try_from_slice(&pepper);
        }

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut pepper = [0u8; 32];
        rand::rng().fill_bytes(&mut pepper);
        fs::write(path, URL_SAFE_NO_PAD.encode(pepper))?;
        Ok(Self::new(pepper))
    }

    /// 从字节切片生成哈希器。
    pub fn try_from_slice(pepper: &[u8]) -> AuthResult<Self> {
        let pepper: [u8; 32] = pepper
            .try_into()
            .map_err(|_| AuthError::InvalidPepperLength)?;
        Ok(Self::new(pepper))
    }

    /// 生成新的客户端 API Key。
    pub fn generate_client_api_key(&self, _name: &str) -> GeneratedClientApiKey {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        // sk_ 只用于客户端调用 /v1，不能复用成管理员登录密码。
        let plaintext = format!("sk_{}", URL_SAFE_NO_PAD.encode(bytes));
        let prefix = plaintext.chars().take(12).collect::<String>();
        let key_hash = self.hash_client_api_key(&plaintext);
        GeneratedClientApiKey {
            key: plaintext,
            prefix,
            key_hash,
        }
    }

    /// 计算 API Key 的 HMAC。
    pub fn hash_client_api_key(&self, plaintext: &str) -> String {
        let mut mac = self.new_mac();
        mac.update(plaintext.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// 校验 API Key 是否有效。
    pub fn verify_client_api_key(&self, plaintext: &str, key_hash: &str) -> AuthResult<bool> {
        let Some(suffix) = plaintext.strip_prefix("sk_") else {
            return Ok(false);
        };

        let decoded_suffix = URL_SAFE_NO_PAD.decode(suffix)?;
        let canonical = format!("sk_{}", URL_SAFE_NO_PAD.encode(decoded_suffix));
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
