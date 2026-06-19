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

#[cfg(test)]
mod tests {
    use base64::Engine;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{jwt_expiration, jwt_expiry, JwtExpiry};

    #[test]
    fn jwt_expiry_should_detect_expired_valid_and_invalid_tokens() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 8, 0, 0).unwrap();

        assert_eq!(
            jwt_expiry(&test_jwt(now.timestamp() - 1), now),
            JwtExpiry::Expired
        );
        assert_eq!(
            jwt_expiry(&test_jwt(now.timestamp() + 1), now),
            JwtExpiry::Valid
        );
        assert_eq!(jwt_expiry("not-a-jwt", now), JwtExpiry::MissingOrInvalid);
    }

    #[test]
    fn jwt_expiration_should_return_exp_as_utc_datetime() {
        let expires_at = Utc.with_ymd_and_hms(2026, 6, 14, 8, 0, 0).unwrap();

        assert_eq!(
            jwt_expiration(&test_jwt(expires_at.timestamp())),
            Some(expires_at)
        );
    }

    fn test_jwt(exp: i64) -> String {
        let header = json!({"alg": "none", "typ": "JWT"});
        let payload = json!({ "exp": exp });
        format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
    }

    fn jwt_part(value: &serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }
}
