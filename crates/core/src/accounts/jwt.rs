use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

/// JWT 过期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtExpiry {
    /// token 已过期。
    Expired,
    /// token 仍然有效。
    Valid,
    /// token 缺失、格式错误或不包含可解析的 exp。
    MissingOrInvalid,
}

/// 按给定时间点判断 JWT 的 `exp` 是否已过期。
pub fn jwt_expiry(token: &str, now: DateTime<Utc>) -> JwtExpiry {
    let Some(exp) = jwt_exp(token) else {
        return JwtExpiry::MissingOrInvalid;
    };
    if now.timestamp() >= exp {
        JwtExpiry::Expired
    } else {
        JwtExpiry::Valid
    }
}

/// 读取 JWT `exp` 并转换成 UTC 时间。
pub fn jwt_expiration(token: &str) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(jwt_exp(token)?, 0).single()
}

fn jwt_exp(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value.get("exp")?.as_i64()
}
