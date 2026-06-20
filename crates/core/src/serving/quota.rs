//! 配额检查策略。

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

/// 判断配额百分比是否达到阈值。
pub fn quota_reached(used_percent: f64, threshold: f64) -> bool {
    used_percent >= threshold
}

/// 判断配额快照的主 rate-limit 是否已经触顶。
pub fn quota_snapshot_limit_reached(quota: &Value) -> bool {
    quota
        .pointer("/rate_limit/limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 从配额快照读取主 rate-limit 的 UTC 重置时间。
pub fn quota_snapshot_reset_at(quota: &Value) -> Option<DateTime<Utc>> {
    let reset_at = quota
        .pointer("/rate_limit/reset_at")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)?;
    DateTime::<Utc>::from_timestamp(reset_at, 0)
}

/// 从配额快照读取主 rate-limit 窗口大小。
pub fn quota_snapshot_limit_window_seconds(quota: &Value) -> Option<u64> {
    quota
        .pointer("/rate_limit/limit_window_seconds")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

/// 从 Codex usage 响应中提取持久化配额快照。
pub fn quota_from_usage(usage: &Value) -> Value {
    let additional = usage
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rate_limits_by_limit_id = Map::new();

    for item in &additional {
        let Some(limit_id) = item
            .get("metered_feature")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let quota = quota_from_rate_limit(item.get("rate_limit"));
        if quota.is_null() {
            continue;
        }
        rate_limits_by_limit_id.insert(
            limit_id.to_string(),
            json!({
                "limit_id": limit_id,
                "limit_name": item.get("limit_name").cloned().unwrap_or(Value::Null),
                "allowed": quota.get("allowed").cloned().unwrap_or(Value::Null),
                "limit_reached": quota.get("limit_reached").cloned().unwrap_or(Value::Null),
                "used_percent": quota.get("used_percent").cloned().unwrap_or(Value::Null),
                "remaining_percent": quota.get("remaining_percent").cloned().unwrap_or(Value::Null),
                "reset_at": quota.get("reset_at").cloned().unwrap_or(Value::Null),
                "limit_window_seconds": quota.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
                "secondary_rate_limit": secondary_quota_from_rate_limit(item.get("rate_limit")),
            }),
        );
    }

    let additional_review = additional.iter().find(|item| {
        is_review_limit_id(item.get("metered_feature").and_then(Value::as_str))
            || is_review_limit_id(item.get("limit_name").and_then(Value::as_str))
    });
    let code_review_rate_limit = match quota_from_rate_limit(usage.get("code_review_rate_limit")) {
        Value::Null => {
            quota_from_rate_limit(additional_review.and_then(|item| item.get("rate_limit")))
        }
        quota => quota,
    };

    json!({
        "plan_type": usage.get("plan_type").cloned().unwrap_or(Value::Null),
        "rate_limit": quota_from_rate_limit(usage.get("rate_limit")),
        "secondary_rate_limit": secondary_quota_from_rate_limit(usage.get("rate_limit")),
        "code_review_rate_limit": code_review_rate_limit,
        "rate_limits_by_limit_id": if rate_limits_by_limit_id.is_empty() {
            Value::Null
        } else {
            Value::Object(rate_limits_by_limit_id)
        },
        "credits": normalize_quota_credits(usage.get("credits")),
    })
}

fn quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(rate_limit) = rate_limit.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let primary = rate_limit.get("primary_window");
    let used_percent = primary
        .and_then(|window| window.get("used_percent"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "allowed": rate_limit.get("allowed").cloned().unwrap_or(Value::Null),
        "limit_reached": rate_limit.get("limit_reached").cloned().unwrap_or(Value::Null),
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(primary.and_then(|window| window.get("used_percent"))),
        "reset_at": primary.and_then(|window| window.get("reset_at")).cloned().unwrap_or(Value::Null),
        "limit_window_seconds": primary.and_then(|window| window.get("limit_window_seconds")).cloned().unwrap_or(Value::Null),
    })
}

fn secondary_quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(secondary) = rate_limit
        .and_then(|rate_limit| rate_limit.get("secondary_window"))
        .filter(|value| !value.is_null())
    else {
        return Value::Null;
    };
    let used_percent = secondary
        .get("used_percent")
        .cloned()
        .unwrap_or(Value::Null);
    let limit_reached = secondary
        .get("used_percent")
        .and_then(Value::as_f64)
        .map(|used| used >= 100.0)
        .map(Value::Bool)
        .or_else(|| {
            rate_limit
                .and_then(|rate_limit| rate_limit.get("limit_reached"))
                .cloned()
        })
        .unwrap_or(Value::Null);
    json!({
        "limit_reached": limit_reached,
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(secondary.get("used_percent")),
        "reset_at": secondary.get("reset_at").cloned().unwrap_or(Value::Null),
        "limit_window_seconds": secondary.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_quota_credits(raw: Option<&Value>) -> Value {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let Some(balance) = raw
        .get("balance")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
    else {
        return Value::Null;
    };
    json!({
        "has_credits": raw.get("has_credits").and_then(Value::as_bool).unwrap_or(false),
        "unlimited": raw.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
        "overage_limit_reached": raw.get("overage_limit_reached").and_then(Value::as_bool).unwrap_or(false),
        "balance": balance,
    })
}

fn remaining_percent(used_percent: Option<&Value>) -> Value {
    let Some(used_percent) = used_percent.and_then(Value::as_f64) else {
        return Value::Null;
    };
    json!((100.0 - used_percent.clamp(0.0, 100.0)).round() as i64)
}

fn is_review_limit_id(value: Option<&str>) -> bool {
    let normalized = value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    normalized == "review"
        || normalized == "code_review"
        || normalized == "codex_review"
        || normalized == "codex_code_review"
        || normalized.contains("code_review")
        || normalized.contains("codex_review")
}
