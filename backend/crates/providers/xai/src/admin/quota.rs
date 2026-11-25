//! 将 xAI 额度观测与本地滚动用量投影到管理合同

use crate::credential::{GrokQuotaPeriodKind, GrokQuotaSnapshot};
use chrono::{DateTime, Utc};
use gateway_admin::model::provider_credentials::{
    ProviderDocument, ProviderQuota, ProviderQuotaWindow, QuotaLocalUsageAttribution,
};
use gateway_core::account::OpaqueProviderData;
use serde_json::{Map, Number, Value};

pub(super) fn project_quota(
    snapshot: Option<GrokQuotaSnapshot>,
    quota_exhausted: bool,
    refresh_token_expires_at: Option<DateTime<Utc>>,
    rolling_usage: Option<gateway_admin::model::accounts::AccountUsage>,
) -> ProviderQuota {
    let billing = snapshot.as_ref().map(GrokQuotaSnapshot::billing);
    let window = if let Some(billing) = billing.filter(|billing| billing.has_authoritative_quota())
    {
        let period_kind = billing.period_kind();
        let window_seconds = quota_window_seconds(billing.period_start(), billing.period_end());
        let mut data = Map::new();
        data.insert(
            "periodStart".to_owned(),
            billing
                .period_start()
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
        data.insert(
            "periodEnd".to_owned(),
            billing
                .period_end()
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
        for (key, value) in [
            ("onDemandCapCents", billing.on_demand_cap_cents()),
            ("onDemandUsedCents", billing.on_demand_used_cents()),
            ("prepaidBalanceCents", billing.prepaid_balance_cents()),
        ] {
            data.insert(
                key.to_owned(),
                value.map_or(Value::Null, |value| Value::Number(Number::from(value))),
            );
        }
        ProviderQuotaWindow {
            upstream: None,
            key: billing.period_type().unwrap_or("billing").to_owned(),
            group: quota_group(period_kind).to_owned(),
            label: xai_quota_window_label(period_kind, window_seconds),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds,
            used_percent: billing.used_percent(),
            reset_at: billing.period_end().and_then(parse_utc),
            limit_reached: billing
                .used_percent()
                .is_some_and(|used| used.is_finite() && used >= 100.0),
            local_usage: None,
            provider_data: Some(ProviderDocument::new(OpaqueProviderData::new(data))),
        }
    } else {
        // 本地滚动统计独立于上游额度观察，导入后尚无快照时也保留请求图与日用量
        ProviderQuotaWindow {
            upstream: None,
            key: "free-rolling-24h".to_owned(),
            group: "shortTerm".to_owned(),
            label: "日限额".to_owned(),
            limit_id: None,
            limit_name: None,
            role: None,
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(crate::GROK_FREE_ROLLING_WINDOW_SECONDS),
            used_percent: None,
            reset_at: None,
            limit_reached: false,
            local_usage: rolling_usage,
            provider_data: None,
        }
    };
    ProviderQuota {
        paused_windows: Vec::new(),
        upstream: None,
        credits: None,
        plan_type: billing
            .and_then(|billing| billing.plan_type())
            .map(str::to_owned),
        observed_at: snapshot.as_ref().map(GrokQuotaSnapshot::observed_at),
        refresh_token_expires_at,
        windows: vec![window],
        limit_reached: quota_exhausted,
        provider_data: None,
    }
}

const fn quota_group(kind: GrokQuotaPeriodKind) -> &'static str {
    match kind {
        GrokQuotaPeriodKind::Weekly => "shortTerm",
        GrokQuotaPeriodKind::Monthly => "monthly",
        GrokQuotaPeriodKind::Other => "other",
    }
}

fn xai_quota_window_label(kind: GrokQuotaPeriodKind, window_seconds: Option<u64>) -> String {
    match kind {
        GrokQuotaPeriodKind::Weekly => "周限额".to_owned(),
        GrokQuotaPeriodKind::Monthly => "月限额".to_owned(),
        GrokQuotaPeriodKind::Other => custom_quota_window_label(window_seconds),
    }
}

fn custom_quota_window_label(window_seconds: Option<u64>) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_owned();
    };
    if seconds % 86_400 == 0 {
        format!("{}日限额", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时限额", seconds / 3_600)
    } else {
        format!("{}分钟限额", seconds.div_ceil(60))
    }
}

fn quota_window_seconds(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let start = start.and_then(parse_utc)?;
    let end = end.and_then(parse_utc)?;
    end.signed_duration_since(start)
        .num_seconds()
        .try_into()
        .ok()
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}
