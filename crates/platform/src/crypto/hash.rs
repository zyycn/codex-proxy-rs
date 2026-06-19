use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// HMAC-SHA256 类型别名。
pub type HmacSha256 = Hmac<Sha256>;

/// 计算 HMAC-SHA256 并以 URL-safe base64 输出。
pub fn hmac_sha256_base64(key: &[u8], payload: &[u8]) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(payload);
    Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// 常数时间比较两个字节切片。
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.iter()
        .zip(right)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}
