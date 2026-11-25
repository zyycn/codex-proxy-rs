//! 管理端额度快照与历史请求观测的只读投影

use crate::credential::{
    CodexAccountQuotaSnapshot, CodexQuotaWindow, CodexQuotaWindowKind, CodexQuotaWindowRole,
};
use chrono::{DateTime, Utc};
use gateway_admin::model::provider_credentials::{
    ProviderDocument, ProviderQuota, ProviderQuotaCredits, ProviderQuotaWindow,
    ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
};
use gateway_core::account::OpaqueProviderData;
use serde_json::{Map, Number, Value};

pub(super) fn empty_quota() -> ProviderQuota {
    ProviderQuota {
        paused_windows: Vec::new(),
        upstream: None,
        credits: None,
        plan_type: None,
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    }
}

pub(super) fn project_quota(
    snapshot: Option<CodexAccountQuotaSnapshot>,
    limit_reached: bool,
    limits: &gateway_core::account::AccountQuotaSettings,
) -> ProviderQuota {
    let mut quota = snapshot
        .map(|s| project_quota_snapshot(s, limits))
        .unwrap_or_else(empty_quota);
    quota.limit_reached = limit_reached;
    quota
}

fn project_quota_snapshot(
    snapshot: CodexAccountQuotaSnapshot,
    limits: &gateway_core::account::AccountQuotaSettings,
) -> ProviderQuota {
    let mut provider_data = Map::new();
    provider_data.insert(
        "remaining_percent".to_owned(),
        snapshot
            .fact()
            .remaining_percent()
            .map_or(Value::Null, |value| Value::Number(Number::from(value))),
    );
    provider_data.insert(
        "exhausted".to_owned(),
        Value::Bool(snapshot.quota().is_exhausted()),
    );
    let windows: Vec<ProviderQuotaWindow> = snapshot
        .windows()
        .iter()
        .filter(|window| should_project_quota_window(window))
        .map(|window| ProviderQuotaWindow {
            upstream: None,
            key: window.key().to_owned(),
            group: quota_group(window.kind()).to_owned(),
            label: codex_quota_window_label(window.kind(), window.role(), window.window_seconds()),
            limit_id: Some(window.source().to_owned()),
            limit_name: window.limit_name().map(str::to_owned),
            role: Some(quota_role(window.role())),
            local_usage_attribution: if window.is_account_wide() {
                QuotaLocalUsageAttribution::AccountWide
            } else {
                QuotaLocalUsageAttribution::Unavailable
            },
            window_seconds: window.window_seconds(),
            used_percent: window.used_percent(),
            reset_at: window.reset_at(),
            limit_reached: window.limit_reached(),
            local_usage: None,
            provider_data: None,
        })
        .collect();
    // 快照级 limit_reached 只看滚动后的窗口触顶：顶层标记是观测事实，不能
    // 在窗口全部过期后继续维持限流
    let limit_reached = quota_windows_limit_reached(&windows);
    ProviderQuota {
        paused_windows: snapshot
            .limit_usage()
            .filter(|(key, used)| limits.reached(key, Some(*used)))
            .map(|(key, _)| key.to_owned())
            .collect(),
        upstream: None,
        plan_type: snapshot.plan_type().map(str::to_owned),
        observed_at: Some(DateTime::<Utc>::from(snapshot.observed_at())),
        refresh_token_expires_at: None,
        windows,
        credits: snapshot.credits().map(|credits| ProviderQuotaCredits {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            balance: credits.balance.clone(),
        }),
        limit_reached,
        provider_data: Some(ProviderDocument::new(OpaqueProviderData::new(
            provider_data,
        ))),
    }
}
/// `secondary_window` 有时只是上游的空占位：没有时长、重置时间，也没有用量
/// 它不能被可靠翻译成 5 小时或周额度，因此不应占用账号面板；带有实际事实的
/// 次级窗口（时长、重置、非零用量或触顶）仍完整保留
fn should_project_quota_window(window: &CodexQuotaWindow) -> bool {
    window.role() != CodexQuotaWindowRole::Secondary
        || window.window_seconds().is_some()
        || window.reset_at().is_some()
        || window.used_percent().is_some_and(|used| used > 0.0)
        || window.limit_reached()
}

fn quota_windows_limit_reached(windows: &[ProviderQuotaWindow]) -> bool {
    windows.iter().any(|window| window.limit_reached)
}

const fn quota_group(kind: CodexQuotaWindowKind) -> &'static str {
    match kind {
        CodexQuotaWindowKind::Monthly => "monthly",
        CodexQuotaWindowKind::ShortTerm | CodexQuotaWindowKind::Weekly => "shortTerm",
        CodexQuotaWindowKind::Other => "other",
    }
}

pub(super) const fn quota_role(role: CodexQuotaWindowRole) -> ProviderQuotaWindowRole {
    match role {
        CodexQuotaWindowRole::Primary => ProviderQuotaWindowRole::Primary,
        CodexQuotaWindowRole::Secondary => ProviderQuotaWindowRole::Secondary,
        CodexQuotaWindowRole::Monthly => ProviderQuotaWindowRole::Monthly,
    }
}

/// 按窗口时长显示汉化额度名；额度桶名称通过独立字段投影
fn codex_quota_window_label(
    kind: CodexQuotaWindowKind,
    role: CodexQuotaWindowRole,
    window_seconds: Option<u64>,
) -> String {
    match kind {
        CodexQuotaWindowKind::Monthly => "月额度".to_owned(),
        CodexQuotaWindowKind::Weekly => "周额度".to_owned(),
        CodexQuotaWindowKind::ShortTerm => "5小时额度".to_owned(),
        CodexQuotaWindowKind::Other => custom_quota_window_label(window_seconds, role),
    }
}

fn custom_quota_window_label(window_seconds: Option<u64>, role: CodexQuotaWindowRole) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        // 与官方客户端一致：没有时长时不臆测为 5 小时或周额度；保留
        // primary/secondary 语义，让用户知道这是上游未标明时长的独立窗口
        return match role {
            CodexQuotaWindowRole::Primary => "主额度".to_owned(),
            CodexQuotaWindowRole::Secondary => "次级额度".to_owned(),
            CodexQuotaWindowRole::Monthly => "月额度".to_owned(),
        };
    };
    if seconds % 86_400 == 0 {
        format!("{}日额度", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时额度", seconds / 3_600)
    } else {
        format!("{}分钟额度", seconds.div_ceil(60))
    }
}

pub(super) fn history_window(
    document: &ProviderDocument,
    key: &str,
) -> Option<(crate::credential::CodexQuotaWindow, Option<String>)> {
    let headers = document
        .expose_to_provider()
        .expose_to_provider()
        .get("rateLimitHeaders")?;
    let headers: Vec<(String, String)> = serde::Deserialize::deserialize(headers).ok()?;
    let parsed = gateway_protocol::openai::events::parse_rate_limit_headers(&headers)?;
    let window = crate::credential::observed_quota_window(&parsed, key)?;
    Some((window, parsed.plan_type))
}

pub(super) fn history_reading(
    document: &ProviderDocument,
    key: &str,
) -> Option<gateway_admin::model::quota_history::QuotaReading> {
    let (window, plan_type) = history_window(document, key)?;
    Some(gateway_admin::model::quota_history::QuotaReading {
        plan_type,
        used_percent: window.used_percent()?,
        reset_at: window.reset_at(),
    })
}
