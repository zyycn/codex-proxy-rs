use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::upstream::accounts::{
    model::AccountStatus,
    pool::{
        AccountAcquireRequest, AccountPool, AccountPoolOptions, AccountWindowUsageDelta,
        RotationStrategy,
    },
};
use serde_json::{json, Value};

mod quota;
mod selection;
mod usage_window;

fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 11, 8, 0, 0).unwrap()
}

fn test_jwt(exp_offset_seconds: i64) -> String {
    let payload = json!({
        "exp": Utc::now().timestamp() + exp_offset_seconds,
    });
    let header = json!({ "alg": "none", "typ": "JWT" });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload),)
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}
