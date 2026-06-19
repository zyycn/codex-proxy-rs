//! SQLite 账号 token 加解密辅助。

use secrecy::SecretString;

use codex_proxy_platform::crypto::{CryptoResult, SecretBox};

/// 加密账号 token。
pub fn encrypt_token(secret_box: &SecretBox, token: &SecretString) -> CryptoResult<String> {
    secret_box.encrypt(token)
}
