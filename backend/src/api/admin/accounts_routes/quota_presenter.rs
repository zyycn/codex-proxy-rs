//! typed quota read model 的管理端 presenter。

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    api::admin::accounts_routes::query::{AccountQuotaReadModel, AccountQuotaWindowLocalUsage},
    fleet::quota::{QuotaWindowKind, QuotaWindowRole},
    infra::{
        format::{
            format_compact_percent, format_plain_number, format_tokens, nonnegative_i64_to_u64,
        },
        time::{china_datetime, china_relative_time},
    },
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AccountQuotaData {
    refreshed_at_display: String,
    windows: Vec<AccountQuotaWindowData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountQuotaWindowData {
    key: String,
    group: &'static str,
    window_seconds: Option<u64>,
    label_display: String,
    used_percent: Option<f64>,
    used_percent_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_usage: Option<AccountQuotaWindowLocalUsageData>,
    reset_at_display: String,
    window_used_display: String,
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

pub(super) fn quota_data(read: AccountQuotaReadModel) -> AccountQuotaData {
    let now = Utc::now();
    AccountQuotaData {
        refreshed_at_display: china_relative_time(read.fetched_at, now),
        windows: read
            .windows
            .into_iter()
            .map(|read| {
                let window = read.quota.window;
                let window_seconds = window.window_seconds();
                let reset_at = window.reset_datetime();
                let used_percent = window.used_percent;
                AccountQuotaWindowData {
                    key: read.quota.key,
                    group: quota_window_group(window.kind),
                    window_seconds,
                    label_display: quota_window_label(
                        read.quota.source.as_str(),
                        read.quota.limit_name.as_deref(),
                        read.quota.metered_feature.as_deref(),
                        read.quota.role,
                        window.kind,
                        window_seconds,
                    ),
                    used_percent,
                    used_percent_display: used_percent
                        .map_or_else(|| "-".to_string(), format_compact_percent),
                    local_usage: read.local_usage.map(Into::into),
                    reset_at_display: reset_at
                        .as_ref()
                        .map_or_else(|| "-".to_string(), china_datetime),
                    window_used_display: quota_window_used_display(reset_at, window_seconds, now),
                }
            })
            .collect(),
    }
}

fn quota_window_group(kind: QuotaWindowKind) -> &'static str {
    match kind {
        QuotaWindowKind::Monthly => "monthly",
        QuotaWindowKind::ShortTerm | QuotaWindowKind::Weekly => "shortTerm",
        QuotaWindowKind::Other => "other",
    }
}

fn quota_window_label(
    source: &str,
    limit_name: Option<&str>,
    metered_feature: Option<&str>,
    role: QuotaWindowRole,
    kind: QuotaWindowKind,
    window_seconds: Option<u64>,
) -> String {
    if role == QuotaWindowRole::Monthly {
        return "月限额".to_string();
    }
    let base = match kind {
        QuotaWindowKind::ShortTerm => "5小时限额".to_string(),
        QuotaWindowKind::Weekly => "周限额".to_string(),
        QuotaWindowKind::Monthly => "月限额".to_string(),
        QuotaWindowKind::Other => custom_window_label(window_seconds),
    };
    let label = limit_name
        .or(metered_feature)
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("codex"));
    if source == "core" || label.is_none() {
        return base;
    }
    let label = label.unwrap_or_default();
    let label = if is_review_limit_label(label) {
        "代码审查"
    } else {
        label
    };
    format!("{label} · {base}")
}

fn custom_window_label(window_seconds: Option<u64>) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_string();
    };
    if seconds % 86_400 == 0 {
        format!("{}天限额", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时限额", seconds / 3_600)
    } else {
        format!("{}分钟限额", seconds.div_ceil(60))
    }
}

fn quota_window_used_display(
    reset_at: Option<DateTime<Utc>>,
    window_seconds: Option<u64>,
    now: DateTime<Utc>,
) -> String {
    let (Some(reset_at), Some(window_seconds)) = (reset_at, window_seconds) else {
        return "-".to_string();
    };
    let remaining = reset_at
        .signed_duration_since(now)
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

fn is_review_limit_label(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    normalized == "review"
        || normalized == "code_review"
        || normalized == "codex_review"
        || normalized == "codex_code_review"
        || normalized.contains("code_review")
        || normalized.contains("codex_review")
}
