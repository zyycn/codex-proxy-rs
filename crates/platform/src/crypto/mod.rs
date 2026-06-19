//! 加密原语。

/// HMAC 和常数时间比较辅助。
pub mod hash;
mod secret_box;

pub use secret_box::{CryptoError, CryptoResult, SecretBox};
