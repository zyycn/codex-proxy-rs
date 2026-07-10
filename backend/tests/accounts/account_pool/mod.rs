use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::accounts::{
    account::{Account, AccountStatus},
    pool::{
        AccountAcquireRequest, AccountPool, AccountPoolOptions, AccountWindowUsageDelta,
        RotationStrategy,
    },
};
use serde_json::json;

mod quota;
mod selection;
mod usage_window;

fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 11, 8, 0, 0).unwrap()
}

fn acquire_account(pool: &mut AccountPool, model: &str) -> Option<Account> {
    pool.acquire_with(&AccountAcquireRequest::new(model, Utc::now()))
        .map(|acquired| acquired.account)
}

fn test_jwt(exp_offset_seconds: i64) -> String {
    let payload = json!({
        "exp": Utc::now().timestamp() + exp_offset_seconds,
    });
    crate::support::jwt::unsigned_jwt(&payload)
}
