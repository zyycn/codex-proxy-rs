//! 配额检查策略。

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

const MONTH_WINDOW_MINUTES: u64 = 43_200;
const MONTH_WINDOW_SECONDS: u64 = 2_592_000;

/// 判断配额快照是否已经触顶。
pub fn quota_snapshot_limit_reached(quota: &Value) -> bool {
    core_snapshot_blocked(quota)
        || spend_control_limit_reached(quota)
        || monthly_limit_reached(quota)
        || credits_overage_limit_reached(quota)
}

/// 从配额快照读取 UTC 重置时间。
pub fn quota_snapshot_reset_at(quota: &Value) -> Option<DateTime<Utc>> {
    let reset_at = if monthly_limit_reached(quota) || spend_control_limit_reached(quota) {
        quota
            .pointer("/monthly_limit/reset_at")
            .and_then(positive_i64)
            .or_else(|| blocking_core_bucket(quota).and_then(bucket_reset_at))
    } else {
        blocking_core_bucket(quota).and_then(bucket_reset_at)
    }?;
    DateTime::<Utc>::from_timestamp(reset_at, 0)
}

/// 从配额快照读取窗口大小。
pub fn quota_snapshot_limit_window_seconds(quota: &Value) -> Option<u64> {
    if monthly_limit_reached(quota) || spend_control_limit_reached(quota) {
        return Some(MONTH_WINDOW_SECONDS);
    }
    blocking_core_bucket(quota)
        .and_then(bucket_window_minutes)
        .and_then(|minutes| minutes.checked_mul(60))
}

/// 从 Codex usage 响应中提取持久化配额快照。
pub fn quota_from_usage(usage: &Value) -> Value {
    let mut snapshots = Vec::new();
    if let Some(snapshot) = quota_snapshot_from_rate_limit(
        "core",
        usage.get("rate_limit_name").and_then(trimmed_str),
        usage.get("metered_feature").and_then(trimmed_str),
        usage.get("rate_limit"),
    ) {
        snapshots.push(snapshot);
    }

    if let Some(additional) = usage
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for item in additional {
            let Some(limit_name) = item.get("limit_name").and_then(trimmed_str) else {
                continue;
            };
            if let Some(snapshot) = quota_snapshot_from_rate_limit(
                "additional",
                Some(limit_name),
                item.get("metered_feature").and_then(trimmed_str),
                item.get("rate_limit"),
            ) {
                snapshots.push(snapshot);
            }
        }
    }

    let monthly_limit = monthly_limit_from_spend_control(usage.get("spend_control"))
        .or_else(|| monthly_limit_from_snapshots(&snapshots))
        .unwrap_or(Value::Null);

    json!({
        "plan_type": usage.get("plan_type").cloned().unwrap_or(Value::Null),
        "snapshots": snapshots,
        "monthly_limit": monthly_limit,
        "credits": normalize_quota_credits(usage.get("credits")),
        "spend_control": normalize_spend_control(usage.get("spend_control")),
    })
}

fn quota_snapshot_from_rate_limit(
    source: &str,
    limit_name: Option<&str>,
    metered_feature: Option<&str>,
    rate_limit: Option<&Value>,
) -> Option<Value> {
    let rate_limit = rate_limit.filter(|value| !value.is_null())?;
    let primary = window_from_rate_limit(rate_limit.get("primary_window"));
    let secondary = window_from_rate_limit(rate_limit.get("secondary_window"));
    if primary.is_null() && secondary.is_null() {
        return None;
    }
    Some(json!({
        "source": source,
        "limit_name": limit_name,
        "metered_feature": metered_feature,
        "allowed": rate_limit.get("allowed").cloned().unwrap_or(Value::Null),
        "limit_reached": rate_limit.get("limit_reached").cloned().unwrap_or(Value::Null),
        "blocked": rate_limit_blocked(rate_limit)
            || window_limit_reached(&primary)
            || window_limit_reached(&secondary),
        "primary": primary,
        "secondary": secondary,
    }))
}

fn window_from_rate_limit(window: Option<&Value>) -> Value {
    let Some(window) = window.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let used_percent = window
        .get("used_percent")
        .and_then(number_value)
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let window_minutes = window
        .get("limit_window_seconds")
        .and_then(number_value)
        .filter(|seconds| *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64((seconds / 60.0).round()).ok())
        .map(|duration| duration.as_secs());
    json!({
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(used_percent),
        "reset_at": window.get("reset_at").and_then(positive_i64),
        "window_minutes": window_minutes,
        "limit_reached": used_percent >= 100.0,
    })
}

fn normalize_quota_credits(raw: Option<&Value>) -> Value {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    json!({
        "has_credits": raw.get("has_credits").and_then(Value::as_bool).unwrap_or(false),
        "unlimited": raw.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
        "overage_limit_reached": raw.get("overage_limit_reached").and_then(Value::as_bool).unwrap_or(false),
        "balance": raw.get("balance").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_spend_control(raw: Option<&Value>) -> Value {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    raw.clone()
}

fn monthly_limit_from_spend_control(spend_control: Option<&Value>) -> Option<Value> {
    let individual_limit = spend_control?
        .get("individual_limit")
        .filter(|value| !value.is_null())?;
    let used_percent = individual_limit
        .get("used_percent")
        .and_then(number_value)
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    Some(json!({
        "key": "spend-control-monthly",
        "source": "spend_control",
        "used_percent": used_percent,
        "remaining_percent": individual_limit
            .get("remaining_percent")
            .and_then(number_value)
            .unwrap_or_else(|| remaining_percent(used_percent) as f64),
        "reset_at": individual_limit.get("reset_at").and_then(positive_i64),
        "window_minutes": MONTH_WINDOW_MINUTES,
        "limit_reached": spend_control_limit_reached_value(spend_control.unwrap_or(&Value::Null), used_percent),
        "used_credits": individual_limit.get("used").cloned().unwrap_or(Value::Null),
        "limit_credits": individual_limit.get("limit").cloned().unwrap_or(Value::Null),
    }))
}

fn monthly_limit_from_snapshots(snapshots: &[Value]) -> Option<Value> {
    for snapshot in snapshots {
        if snapshot.get("source").and_then(Value::as_str) != Some("core") {
            continue;
        }
        for key in ["primary", "secondary"] {
            let Some(bucket) = snapshot.get(key).filter(|value| !value.is_null()) else {
                continue;
            };
            if bucket
                .get("window_minutes")
                .and_then(Value::as_u64)
                .is_some_and(|minutes| window_minutes_match(minutes, MONTH_WINDOW_MINUTES))
            {
                return Some(json!({
                    "key": "core-monthly",
                    "source": "rate_limit",
                    "used_percent": bucket.get("used_percent").cloned().unwrap_or(Value::Null),
                    "remaining_percent": bucket.get("remaining_percent").cloned().unwrap_or(Value::Null),
                    "reset_at": bucket.get("reset_at").cloned().unwrap_or(Value::Null),
                    "window_minutes": bucket.get("window_minutes").cloned().unwrap_or(Value::Null),
                    "limit_reached": bucket.get("limit_reached").cloned().unwrap_or(Value::Bool(false)),
                    "used_credits": Value::Null,
                    "limit_credits": Value::Null,
                }));
            }
        }
    }
    None
}

fn remaining_percent(used_percent: f64) -> i64 {
    (100.0 - used_percent.clamp(0.0, 100.0)).round() as i64
}

fn rate_limit_blocked(rate_limit: &Value) -> bool {
    rate_limit
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || rate_limit
            .get("allowed")
            .and_then(Value::as_bool)
            .is_some_and(|allowed| !allowed)
}

fn window_limit_reached(window: &Value) -> bool {
    window
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn spend_control_limit_reached(quota: &Value) -> bool {
    quota
        .pointer("/spend_control/reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || quota
            .pointer("/spend_control/individual_limit/used_percent")
            .and_then(number_value)
            .is_some_and(|used_percent| used_percent >= 100.0)
}

fn spend_control_limit_reached_value(spend_control: &Value, used_percent: f64) -> bool {
    spend_control
        .get("reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || used_percent >= 100.0
}

fn monthly_limit_reached(quota: &Value) -> bool {
    quota
        .pointer("/monthly_limit/limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn core_snapshot_blocked(quota: &Value) -> bool {
    quota
        .get("snapshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|snapshot| {
            snapshot.get("source").and_then(Value::as_str) == Some("core")
                && snapshot
                    .get("blocked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
}

fn blocking_core_bucket(quota: &Value) -> Option<&Value> {
    quota
        .get("snapshots")
        .and_then(Value::as_array)?
        .iter()
        .filter(|snapshot| snapshot.get("source").and_then(Value::as_str) == Some("core"))
        .flat_map(|snapshot| {
            ["primary", "secondary"]
                .into_iter()
                .filter_map(|key| snapshot.get(key))
        })
        .find(|bucket| {
            !bucket.is_null()
                && bucket
                    .get("limit_reached")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
}

fn bucket_reset_at(bucket: &Value) -> Option<i64> {
    bucket.get("reset_at").and_then(positive_i64)
}

fn bucket_window_minutes(bucket: &Value) -> Option<u64> {
    bucket
        .get("window_minutes")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn credits_overage_limit_reached(quota: &Value) -> bool {
    quota
        .pointer("/credits/overage_limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| value.is_finite())
}

fn positive_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|value| *value > 0)
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value > 0)
}

fn trimmed_str(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn window_minutes_match(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

mod runtime;

pub use runtime::{
    QuotaRefreshServiceError, QuotaRefreshServiceResult, QuotaRefreshSummary,
    RuntimeQuotaRefreshService,
};
