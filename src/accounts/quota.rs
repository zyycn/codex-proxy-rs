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

/// Cloudflare 阻断恢复状态（从 cloudflare.rs 内联）。
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

const PATH_BLOCK_THRESHOLD: u32 = 3;
const PATH_BLOCK_STALE_AFTER: chrono::Duration = chrono::Duration::hours(1);
const CHALLENGE_BACKOFF_SECONDS: [i64; 4] = [10, 30, 90, 120];
const CHALLENGE_STALE_AFTER: chrono::Duration = chrono::Duration::hours(1);

#[derive(Debug, Clone, Copy)]
struct PathBlockState {
    count: u32,
    last_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
struct ChallengeCooldownState {
    challenge_count: u32,
    updated_at: DateTime<Utc>,
}

/// Cloudflare challenge cooldown state after recording one challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudflareChallengeCooldown {
    /// Current non-stale consecutive challenge count.
    pub challenge_count: u32,
    /// Backoff delay selected for this challenge.
    pub delay_seconds: i64,
    /// Time until which this account should be skipped.
    pub cooldown_until: DateTime<Utc>,
    /// Time at which this challenge was recorded.
    pub updated_at: DateTime<Utc>,
}

/// Tracks per-account Cloudflare path-block failures.
#[derive(Debug, Clone, Default)]
pub struct CloudflarePathBlockTracker {
    counts: Arc<RwLock<HashMap<String, PathBlockState>>>,
}

impl CloudflarePathBlockTracker {
    /// Creates an empty path-block tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one path-block failure and returns the current non-stale count.
    pub async fn record_path_block(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        let mut counts = self.counts.write().await;
        let count = counts
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count.saturating_add(1))
            .unwrap_or(1);
        counts.insert(
            account_id.to_string(),
            PathBlockState {
                count,
                last_at: now,
            },
        );
        count
    }

    /// Clears any tracked path-block failures for an account.
    pub async fn reset(&self, account_id: &str) {
        self.counts.write().await.remove(account_id);
    }

    /// Returns the current non-stale path-block count for an account.
    pub async fn count(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        self.counts
            .read()
            .await
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count)
            .unwrap_or_default()
    }

    /// Returns whether the account should be disabled at the current count.
    pub async fn should_disable(&self, account_id: &str, now: DateTime<Utc>) -> bool {
        self.count(account_id, now).await >= PATH_BLOCK_THRESHOLD
    }
}

/// Tracks per-account Cloudflare challenge cooldown escalation.
#[derive(Debug, Clone, Default)]
pub struct CloudflareChallengeCooldownTracker {
    states: Arc<RwLock<HashMap<String, ChallengeCooldownState>>>,
}

impl CloudflareChallengeCooldownTracker {
    /// Creates an empty challenge cooldown tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one challenge and returns the current non-stale cooldown state.
    pub async fn record_challenge(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> CloudflareChallengeCooldown {
        let mut states = self.states.write().await;
        let challenge_count = states
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.updated_at) <= CHALLENGE_STALE_AFTER)
            .map(|state| state.challenge_count.saturating_add(1))
            .unwrap_or(1);
        states.insert(
            account_id.to_string(),
            ChallengeCooldownState {
                challenge_count,
                updated_at: now,
            },
        );
        let delay_seconds = challenge_delay_seconds(challenge_count);
        CloudflareChallengeCooldown {
            challenge_count,
            delay_seconds,
            cooldown_until: now + chrono::Duration::seconds(delay_seconds),
            updated_at: now,
        }
    }

    /// Clears tracked challenge cooldown state for an account.
    pub async fn reset(&self, account_id: &str) {
        self.states.write().await.remove(account_id);
    }
}

fn challenge_delay_seconds(challenge_count: u32) -> i64 {
    let index = challenge_count
        .saturating_sub(1)
        .min((CHALLENGE_BACKOFF_SECONDS.len() - 1) as u32) as usize;
    CHALLENGE_BACKOFF_SECONDS[index]
}
