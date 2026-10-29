//! 将已脱敏的互联领域观测投影为版本化协议

use chrono::{DateTime, Utc};
use gateway_admin::model::peer_link::{PeerLinkExport, PeerModelState as ModelState};
use gateway_core::metering::Decimal;
use gateway_protocol::peer::{
    OBJECT, PeerBudgetWindow, PeerComponentStatus, PeerKeyUsage, PeerLink, PeerModelAvailability,
    PeerModelCoverage, PeerModelState, PeerNode, PeerPlanCount, PeerPool, PeerPoolDisclosure,
    PeerPoolStates, PeerQuota, PeerQuotaAccount, PeerQuotaDisclosure, PeerQuotaWindow, PeerUsage,
    SCHEMA_VERSION,
};
use std::time::SystemTime;

pub(super) fn response(export: PeerLinkExport) -> PeerLink {
    let mut capabilities = vec!["key_usage".to_owned()];
    let pool = export.pool.map_or_else(PeerPool::default, |pool| {
        capabilities.push("pool_summary".to_owned());
        PeerPool {
            disclosure: PeerPoolDisclosure::Summary,
            observed_at: Some(export.observed_at.to_rfc3339()),
            account_count: Some(
                pool.ready
                    + pool.cooldown
                    + pool.exhausted
                    + pool.disabled
                    + pool.error
                    + pool.unknown,
            ),
            states: Some(PeerPoolStates {
                ready: pool.ready,
                cooldown: pool.cooldown,
                exhausted: pool.exhausted,
                disabled: pool.disabled,
                error: pool.error,
                unknown: pool.unknown,
            }),
            plans: pool.plans.map(|plans| {
                plans
                    .into_iter()
                    .map(|(name, count)| PeerPlanCount { name, count })
                    .collect()
            }),
        }
    });
    let quota = export.quota.map_or_else(PeerQuota::default, |accounts| {
        capabilities.extend(["quota_windows".to_owned(), "model_availability".to_owned()]);
        PeerQuota {
            disclosure: PeerQuotaDisclosure::Accounts,
            accounts: accounts
                .into_iter()
                .map(|account| PeerQuotaAccount {
                    id: account.id,
                    models: account.models,
                    observed_at: account.observed_at.map(|time| time.to_rfc3339()),
                    expires_at: account.expires_at.map(|time| time.to_rfc3339()),
                    windows: account
                        .windows
                        .into_iter()
                        .map(|window| PeerQuotaWindow {
                            key: window.key,
                            group: window.group,
                            label: window.label,
                            limit_id: window.limit_id,
                            limit_name: window.limit_name,
                            role: window.role.map(|role| role.as_str().to_owned()),
                            window_seconds: window.window_seconds,
                            used_percent: window.used_percent,
                            reset_at: window.reset_at.map(|time| time.to_rfc3339()),
                            limit_reached: window.limit_reached,
                        })
                        .collect(),
                })
                .collect(),
        }
    });
    PeerLink {
        object: OBJECT.to_owned(),
        schema_version: SCHEMA_VERSION,
        node: PeerNode {
            id: export.node.id,
            name: export.node.name,
            software: "codex-proxy-rs".to_owned(),
        },
        capabilities,
        observed_at: export.observed_at.to_rfc3339(),
        expires_at: export.expires_at.to_rfc3339(),
        usage: PeerUsage {
            status: PeerComponentStatus::Available,
            observed_at: Some(export.budget_observed_at.to_rfc3339()),
            data: Some(PeerKeyUsage {
                unit: "USD".to_owned(),
                daily: budget_window(
                    export.budget.limits.daily_usd,
                    export.budget.daily_used_usd,
                    export.budget.daily_resets_at,
                ),
                weekly: budget_window(
                    export.budget.limits.weekly_usd,
                    export.budget.weekly_used_usd,
                    export.budget.weekly_resets_at,
                ),
            }),
        },
        pool,
        quota,
        models: export
            .models
            .into_iter()
            .map(|model| PeerModelAvailability {
                model: model.model,
                coverage: if model.complete {
                    PeerModelCoverage::Complete
                } else {
                    PeerModelCoverage::Partial
                },
                state: match model.state {
                    ModelState::Available => PeerModelState::Available,
                    ModelState::Exhausted => PeerModelState::Exhausted,
                    ModelState::Unknown => PeerModelState::Unknown,
                },
                observed_at: model.observed_at.map(|time| time.to_rfc3339()),
                expires_at: model.expires_at.map(|time| time.to_rfc3339()),
            })
            .collect(),
    }
}

fn budget_window(total: Decimal, used: Decimal, resets_at: Option<SystemTime>) -> PeerBudgetWindow {
    let limited = total != Decimal::ZERO;
    PeerBudgetWindow {
        total: limited.then(|| total.canonical()),
        used: used.canonical(),
        remaining: limited.then(|| total.saturating_sub(used).canonical()),
        resets_at: resets_at.map(|time| DateTime::<Utc>::from(time).to_rfc3339()),
    }
}
