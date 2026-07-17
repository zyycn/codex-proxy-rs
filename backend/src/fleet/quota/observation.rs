//! 上游 adapter 交给 fleet 的 typed quota observation。

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    MONTH_WINDOW_MINUTES, MonthlyQuotaLimit, QuotaCredits, QuotaLimitSnapshot, QuotaSnapshot,
    QuotaSpendControl, QuotaWindow, QuotaWindowKind, remaining_percent,
};

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindowObservation {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub reset_at: Option<i64>,
    pub used: Option<Value>,
    pub limit: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaLimitObservation {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub metered_feature: Option<String>,
    pub allowed: Option<bool>,
    pub limit_reached: Option<bool>,
    pub primary: Option<QuotaWindowObservation>,
    pub secondary: Option<QuotaWindowObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCreditsObservation {
    pub has_credits: bool,
    pub unlimited: bool,
    pub overage_limit_reached: bool,
    pub balance: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaObservation {
    pub limits: BTreeMap<String, QuotaLimitObservation>,
    pub active_limit: Option<String>,
    pub credits: Option<QuotaCreditsObservation>,
    pub spend_control: Option<QuotaSpendControl>,
    pub plan_type: Option<String>,
    pub promo_message: Option<String>,
    pub rate_limit_reached_type: Option<String>,
}

/// 将 typed observation 合并为代理内部 quota 快照。
pub fn quota_from_observation(
    observation: &QuotaObservation,
    plan_type: Option<&str>,
    existing: Option<&QuotaSnapshot>,
) -> QuotaSnapshot {
    let mut limits = observation
        .limits
        .values()
        .filter_map(quota_limit_from_observation)
        .collect::<Vec<_>>();

    if let Some(existing) = existing {
        for limit in &existing.limits {
            if !observation
                .limits
                .keys()
                .any(|limit_id| quota_limit_matches(limit, limit_id))
            {
                limits.push(limit.clone());
            }
        }
    }

    let spend_control = observation
        .spend_control
        .clone()
        .or_else(|| existing.and_then(|quota| quota.spend_control.clone()));
    let monthly_limit = spend_control
        .as_ref()
        .and_then(monthly_limit_from_spend_control)
        .or_else(|| monthly_limit_from_limits(&limits))
        .or_else(|| existing.and_then(|quota| quota.monthly_limit.clone()));
    let credits = observation
        .credits
        .as_ref()
        .map(|credits| QuotaCredits {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            overage_limit_reached: credits.overage_limit_reached,
            balance: credits.balance.clone(),
        })
        .or_else(|| existing.and_then(|quota| quota.credits.clone()));

    QuotaSnapshot {
        plan_type: observation
            .plan_type
            .clone()
            .or_else(|| plan_type.map(ToString::to_string))
            .or_else(|| existing.and_then(|quota| quota.plan_type.clone()))
            .or_else(|| Some("unknown".to_string())),
        active_limit: observation
            .active_limit
            .clone()
            .or_else(|| existing.and_then(|quota| quota.active_limit.clone())),
        limits,
        monthly_limit,
        credits,
        spend_control,
        promo_message: observation
            .promo_message
            .clone()
            .or_else(|| existing.and_then(|quota| quota.promo_message.clone())),
        rate_limit_reached_type: observation
            .rate_limit_reached_type
            .clone()
            .or_else(|| existing.and_then(|quota| quota.rate_limit_reached_type.clone())),
        extensions: existing
            .map(|quota| quota.extensions.clone())
            .unwrap_or_default(),
    }
}

fn quota_limit_from_observation(details: &QuotaLimitObservation) -> Option<QuotaLimitSnapshot> {
    let (source, metered_feature) = quota_identity(&details.limit_id);
    let limit_name = details
        .limit_name
        .clone()
        .or_else(|| (source != "core").then(|| details.limit_id.clone()));
    let primary = details.primary.as_ref().map(quota_window_from_observation);
    let secondary = details
        .secondary
        .as_ref()
        .map(quota_window_from_observation);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    let blocked = details.limit_reached.unwrap_or(false)
        || details.allowed.is_some_and(|allowed| !allowed)
        || primary.as_ref().is_some_and(QuotaWindow::is_limit_reached)
        || secondary
            .as_ref()
            .is_some_and(QuotaWindow::is_limit_reached);
    Some(QuotaLimitSnapshot {
        source: source.to_string(),
        limit_name,
        metered_feature: details
            .metered_feature
            .clone()
            .or_else(|| metered_feature.map(ToString::to_string)),
        allowed: details.allowed,
        limit_reached: details.limit_reached,
        blocked,
        primary,
        secondary,
    })
}

fn quota_window_from_observation(window: &QuotaWindowObservation) -> QuotaWindow {
    QuotaWindow::from_usage(
        window.used_percent,
        window.reset_at,
        window.window_minutes,
        window.used.clone(),
        window.limit.clone(),
    )
}

fn quota_identity(limit_id: &str) -> (&'static str, Option<&str>) {
    if limit_id == "codex" {
        ("core", None)
    } else if is_review_limit_name(limit_id) {
        ("code_review", Some(limit_id))
    } else {
        ("additional", Some(limit_id))
    }
}

fn quota_limit_matches(limit: &QuotaLimitSnapshot, limit_id: &str) -> bool {
    let (source, metered_feature) = quota_identity(limit_id);
    if limit.source != source {
        return false;
    }
    source == "core"
        || limit.metered_feature.as_deref() == metered_feature
        || limit.limit_name.as_deref() == Some(limit_id)
}

fn monthly_limit_from_limits(limits: &[QuotaLimitSnapshot]) -> Option<MonthlyQuotaLimit> {
    let window = limits
        .iter()
        .filter(|limit| limit.source == "core")
        .flat_map(QuotaLimitSnapshot::windows)
        .find(|window| window.kind == QuotaWindowKind::Monthly)?;
    Some(MonthlyQuotaLimit {
        key: Some("core-monthly".to_string()),
        source: Some("rate_limit".to_string()),
        used_percent: window.used_percent,
        remaining_percent: window.remaining_percent.map(|value| value as f64),
        reset_at: window.reset_at,
        window_minutes: window.window_minutes,
        limit_reached: window.is_limit_reached(),
        used: window.used.clone(),
        limit: window.limit.clone(),
        used_credits: window.used_credits.clone(),
        limit_credits: window.limit_credits.clone(),
    })
}

fn monthly_limit_from_spend_control(
    spend_control: &QuotaSpendControl,
) -> Option<MonthlyQuotaLimit> {
    let individual = spend_control.individual_limit.as_ref()?;
    let used_percent = individual.used_percent.unwrap_or(0.0).clamp(0.0, 100.0);
    Some(MonthlyQuotaLimit {
        key: Some("spend-control-monthly".to_string()),
        source: Some("spend_control".to_string()),
        used_percent: Some(used_percent),
        remaining_percent: individual
            .remaining_percent
            .or_else(|| Some(remaining_percent(used_percent) as f64)),
        reset_at: individual.reset_at,
        window_minutes: Some(MONTH_WINDOW_MINUTES),
        limit_reached: spend_control.reached || used_percent >= 100.0,
        used: individual.used.clone(),
        limit: individual.limit.clone(),
        used_credits: individual.used.clone(),
        limit_credits: individual.limit.clone(),
    })
}

fn is_review_limit_name(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "review" | "code_review" | "codex_review" | "codex_code_review"
    )
}
