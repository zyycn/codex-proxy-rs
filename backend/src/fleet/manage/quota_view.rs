//! 管理端账号 quota 展示视图。

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::infra::{
    format::{format_compact_percent, format_plain_number, format_tokens, nonnegative_i64_to_u64},
    time::{china_datetime, china_relative_time},
};

const FIVE_HOUR_WINDOW_SECONDS: u64 = 18_000;
const WEEK_WINDOW_SECONDS: u64 = 604_800;
const MONTH_WINDOW_SECONDS: u64 = 2_592_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountQuotaData {
    refreshed_at_display: String,
    windows: Vec<AccountQuotaWindowData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountQuotaWindowData {
    key: String,
    group: String,
    window_seconds: Option<u64>,
    label_display: String,
    used_percent: Option<f64>,
    used_percent_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_usage: Option<AccountQuotaWindowLocalUsageData>,
    reset_at_display: String,
    window_used_display: String,
    #[serde(skip)]
    reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountQuotaWindowLocalUsageData {
    request_count: i64,
    request_count_display: String,
    input_tokens: i64,
    input_tokens_display: String,
    output_tokens: i64,
    output_tokens_display: String,
    cached_tokens: i64,
    cached_tokens_display: String,
    total_tokens: i64,
    total_tokens_display: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AccountQuotaUsageWindow {
    pub(crate) key: String,
    pub(crate) start: DateTime<Utc>,
    pub(crate) end: DateTime<Utc>,
    pub(crate) window_seconds: u64,
}

impl AccountQuotaUsageWindow {
    pub(crate) fn duration_seconds(&self) -> u64 {
        self.end
            .signed_duration_since(self.start)
            .num_seconds()
            .max(0) as u64
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AccountQuotaWindowLocalUsage {
    pub(crate) request_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cached_tokens: i64,
}

impl From<AccountQuotaWindowLocalUsage> for AccountQuotaWindowLocalUsageData {
    fn from(usage: AccountQuotaWindowLocalUsage) -> Self {
        let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
        Self {
            request_count: usage.request_count,
            request_count_display: format_plain_number(nonnegative_i64_to_u64(usage.request_count)),
            input_tokens: usage.input_tokens,
            input_tokens_display: format_tokens(nonnegative_i64_to_u64(usage.input_tokens)),
            output_tokens: usage.output_tokens,
            output_tokens_display: format_tokens(nonnegative_i64_to_u64(usage.output_tokens)),
            cached_tokens: usage.cached_tokens,
            cached_tokens_display: format_tokens(nonnegative_i64_to_u64(usage.cached_tokens)),
            total_tokens,
            total_tokens_display: format_tokens(nonnegative_i64_to_u64(total_tokens)),
        }
    }
}

impl Default for AccountQuotaData {
    fn default() -> Self {
        Self {
            refreshed_at_display: "-".to_string(),
            windows: Vec::new(),
        }
    }
}

impl AccountQuotaData {
    pub(crate) fn has_high_usage(&self) -> bool {
        self.windows
            .iter()
            .any(|window| window.used_percent.is_some_and(|percent| percent >= 80.0))
    }

    pub(crate) fn usage_windows(&self) -> Vec<AccountQuotaUsageWindow> {
        self.windows
            .iter()
            .filter_map(AccountQuotaWindowData::usage_window)
            .collect()
    }

    pub(crate) fn apply_local_usage(
        &mut self,
        usage_by_window: &HashMap<String, AccountQuotaWindowLocalUsage>,
    ) {
        for window in &mut self.windows {
            window.local_usage = window.usage_window().map(|_| {
                usage_by_window
                    .get(&window.key)
                    .copied()
                    .unwrap_or_default()
                    .into()
            });
        }
    }
}

impl AccountQuotaWindowData {
    fn usage_window(&self) -> Option<AccountQuotaUsageWindow> {
        let end = self.reset_at?;
        let window_seconds = self.window_seconds?;
        let seconds = i64::try_from(window_seconds).ok()?;
        let start = end.checked_sub_signed(Duration::seconds(seconds))?;
        (start <= end).then(|| AccountQuotaUsageWindow {
            key: self.key.clone(),
            start,
            end,
            window_seconds,
        })
    }
}

pub(crate) fn quota_data(quota_json: &str, fetched_at: Option<DateTime<Utc>>) -> AccountQuotaData {
    let quota = serde_json::from_str::<Value>(quota_json).unwrap_or(Value::Null);
    let windows = quota_windows(&quota);

    AccountQuotaData {
        refreshed_at_display: china_relative_time(fetched_at, Utc::now()),
        windows,
    }
}

fn quota_windows(quota: &Value) -> Vec<AccountQuotaWindowData> {
    let mut windows = Vec::new();

    let has_monthly_limit = push_monthly_quota_window(&mut windows, quota.get("monthly_limit"));
    if let Some(snapshots) = quota.get("snapshots").and_then(Value::as_array) {
        for snapshot in snapshots {
            push_snapshot_quota_windows(&mut windows, snapshot, has_monthly_limit);
        }
    }

    windows.sort_by_key(quota_window_sort_key);
    windows
}

fn push_monthly_quota_window(
    windows: &mut Vec<AccountQuotaWindowData>,
    monthly_limit: Option<&Value>,
) -> bool {
    let Some(monthly_limit) = monthly_limit.filter(|value| !value.is_null()) else {
        return false;
    };
    let used_percent = monthly_limit
        .get("used_percent")
        .and_then(number_value)
        .map(|value| value.clamp(0.0, 100.0));
    let reset_at = monthly_limit
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0));
    let window_seconds = monthly_limit
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60))
        .or(Some(MONTH_WINDOW_SECONDS));
    if used_percent.is_none() && reset_at.is_none() {
        return false;
    }

    let key = monthly_limit
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("monthly");
    windows.push(AccountQuotaWindowData {
        key: quota_key_segment(key),
        group: "monthly".to_string(),
        window_seconds,
        label_display: "月限额".to_string(),
        used_percent,
        used_percent_display: used_percent.map_or_else(|| "-".to_string(), format_compact_percent),
        local_usage: None,
        reset_at_display: reset_at
            .as_ref()
            .map_or_else(|| "-".to_string(), china_datetime),
        window_used_display: quota_window_used_display(reset_at, window_seconds),
        reset_at,
    });
    true
}

fn push_snapshot_quota_windows(
    windows: &mut Vec<AccountQuotaWindowData>,
    snapshot: &Value,
    skip_core_monthly: bool,
) {
    let source = snapshot
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("quota");
    let source_key = snapshot_source_key(source, snapshot);
    let label_prefix = snapshot_label(snapshot);
    for role in ["primary", "secondary"] {
        let Some(window) = snapshot.get(role).filter(|value| !value.is_null()) else {
            continue;
        };
        let window_seconds = window
            .get("window_minutes")
            .and_then(Value::as_u64)
            .and_then(|minutes| minutes.checked_mul(60));
        if skip_core_monthly
            && source == "core"
            && window_seconds
                .is_some_and(|seconds| quota_window_matches(seconds, MONTH_WINDOW_SECONDS))
        {
            continue;
        }
        push_quota_window(
            windows,
            &source_key,
            role,
            label_prefix.as_deref(),
            Some(window),
        );
    }
}

fn push_quota_window(
    windows: &mut Vec<AccountQuotaWindowData>,
    source_key: &str,
    role: &str,
    label_prefix: Option<&str>,
    window: Option<&Value>,
) {
    let Some(window) = window.filter(|value| !value.is_null()) else {
        return;
    };
    let used_percent = window
        .get("used_percent")
        .and_then(number_value)
        .map(|value| value.clamp(0.0, 100.0));
    let reset_at = window
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0));
    let window_seconds = window
        .get("window_minutes")
        .and_then(Value::as_u64)
        .and_then(|minutes| minutes.checked_mul(60));
    let label_prefix = label_prefix
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if reset_at.is_none()
        && window_seconds.is_none()
        && (used_percent.is_none() || label_prefix.is_none())
    {
        return;
    }
    let base_label = quota_window_label_display(window_seconds);
    let label_display = label_prefix
        .map(|value| format!("{value} · {base_label}"))
        .unwrap_or(base_label);

    windows.push(AccountQuotaWindowData {
        key: unique_quota_window_key(windows, source_key, role, window_seconds),
        group: quota_window_group(window_seconds).to_string(),
        window_seconds,
        label_display,
        used_percent,
        used_percent_display: used_percent.map_or_else(|| "-".to_string(), format_compact_percent),
        local_usage: None,
        reset_at_display: reset_at
            .as_ref()
            .map_or_else(|| "-".to_string(), china_datetime),
        window_used_display: quota_window_used_display(reset_at, window_seconds),
        reset_at,
    });
}

fn snapshot_source_key(source: &str, snapshot: &Value) -> String {
    let label = snapshot
        .get("limit_name")
        .and_then(Value::as_str)
        .or_else(|| snapshot.get("metered_feature").and_then(Value::as_str))
        .unwrap_or(source);
    format!("{}-{}", quota_key_segment(source), quota_key_segment(label))
}

fn snapshot_label(snapshot: &Value) -> Option<String> {
    let source = snapshot.get("source").and_then(Value::as_str);
    if source == Some("core") {
        return None;
    }
    let label = snapshot
        .get("limit_name")
        .and_then(Value::as_str)
        .or_else(|| snapshot.get("metered_feature").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if label.eq_ignore_ascii_case("codex") {
        return None;
    }
    if is_review_limit_label(Some(label)) {
        return Some("代码审查".to_string());
    }
    Some(label.to_string())
}

fn unique_quota_window_key(
    windows: &[AccountQuotaWindowData],
    source_key: &str,
    role: &str,
    window_seconds: Option<u64>,
) -> String {
    let bucket = quota_window_key_part(window_seconds).unwrap_or(role);
    let key = format!("{source_key}-{bucket}");
    if windows.iter().any(|window| window.key == key) {
        format!("{key}-{role}")
    } else {
        key
    }
}

fn quota_window_key_part(window_seconds: Option<u64>) -> Option<&'static str> {
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            Some("five-hour")
        }
        Some(seconds) if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => Some("weekly"),
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => Some("monthly"),
        _ => None,
    }
}

fn quota_key_segment(value: &str) -> String {
    let mut segment = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            segment.push(ch.to_ascii_lowercase());
        } else if !segment.ends_with('-') {
            segment.push('-');
        }
    }
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "quota".to_string()
    } else {
        segment.to_string()
    }
}

fn quota_window_sort_key(window: &AccountQuotaWindowData) -> (u8, u64, String) {
    let group_order = match window.group.as_str() {
        "monthly" => 0,
        "shortTerm" => 1,
        _ => 2,
    };
    (
        group_order,
        window.window_seconds.unwrap_or(0),
        window.key.clone(),
    )
}

fn quota_window_group(window_seconds: Option<u64>) -> &'static str {
    match window_seconds {
        Some(seconds) if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => "monthly",
        Some(seconds)
            if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS)
                || quota_window_matches(seconds, WEEK_WINDOW_SECONDS) =>
        {
            "shortTerm"
        }
        _ => "other",
    }
}

fn quota_window_matches(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

fn quota_window_label_display(window_seconds: Option<u64>) -> String {
    let Some(window_seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_string();
    };
    match window_seconds {
        seconds if quota_window_matches(seconds, FIVE_HOUR_WINDOW_SECONDS) => {
            "5小时限额".to_string()
        }
        seconds if quota_window_matches(seconds, WEEK_WINDOW_SECONDS) => "周限额".to_string(),
        seconds if quota_window_matches(seconds, MONTH_WINDOW_SECONDS) => "月限额".to_string(),
        seconds if seconds % 86_400 == 0 => format!("{}天限额", seconds / 86_400),
        seconds if seconds % 3_600 == 0 => format!("{}小时限额", seconds / 3_600),
        seconds => format!("{}分钟限额", seconds.div_ceil(60)),
    }
}

fn quota_window_used_display(
    reset_at: Option<DateTime<Utc>>,
    window_seconds: Option<u64>,
) -> String {
    let (Some(reset_at), Some(window_seconds)) = (reset_at, window_seconds) else {
        return "-".to_string();
    };
    let remaining = reset_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .max(0)
        .cast_unsigned();
    let used = window_seconds.saturating_sub(remaining);
    format!(
        "{} / {}",
        format_duration_days(used),
        format_duration_days(window_seconds)
    )
}

fn format_duration_days(seconds: u64) -> String {
    let days = seconds as f64 / 86_400.0;
    if days >= 1.0 {
        format!("{days:.1}d")
    } else {
        format!("{:.1}h", seconds as f64 / 3_600.0)
    }
}

fn number_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        .filter(|value| value.is_finite())
}

fn is_review_limit_label(value: Option<&str>) -> bool {
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
