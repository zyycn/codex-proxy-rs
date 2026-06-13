use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtExpiry {
    Expired,
    Valid,
    MissingOrInvalid,
}

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

    use super::{jwt_expiry, JwtExpiry};

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

    fn test_jwt(exp: i64) -> String {
        let header = json!({"alg": "none", "typ": "JWT"});
        let payload = json!({ "exp": exp });
        format!("{}.{}.", jwt_part(&header), jwt_part(&payload))
    }

    fn jwt_part(value: &serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }
}
