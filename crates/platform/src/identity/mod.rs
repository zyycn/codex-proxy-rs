//! 身份哈希与本地认证相关原语。

mod admin_password;
mod client_key;

pub use admin_password::{hash_admin_password, verify_admin_password};
pub use client_key::{ApiKeyHasher, GeneratedClientApiKey};

use thiserror::Error;

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
