//! 身份认证相关原语：管理员密码哈希与 API Key 生成。

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::Rng;
use sha2::{Digest, Sha256};
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

/// 客户端 API Key 派生结果。
#[derive(Debug, Clone)]
pub struct GeneratedClientApiKey {
    /// 管理端可复制的完整密钥。
    pub key: String,
    /// 前缀缓存。
    pub prefix: String,
}

/// 生成新的客户端 API Key。
pub fn generate_client_api_key() -> GeneratedClientApiKey {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let key = format!("sk_{}", URL_SAFE_NO_PAD.encode(bytes));
    let prefix = key.chars().take(12).collect::<String>();
    GeneratedClientApiKey { key, prefix }
}

/// 生成新的管理员 API Key。
pub fn generate_admin_api_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("admin-{}", hex::encode(bytes))
}

/// 生成新的管理员会话令牌。
pub fn generate_admin_session_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("sess_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// 计算高熵凭据的 SHA-256 十六进制摘要。
pub fn hash_credential(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
