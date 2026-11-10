//! 将已保存的互联观测投影为管理端额度与同步事实

use std::time::SystemTime;

use crate::model::{
    provider_credentials::{
        ProviderQuota, ProviderQuotaWindow, ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
    },
    upstream_info::{
        ProviderUpstreamInfo, UpstreamPlanCount, UpstreamPool, UpstreamPoolStates,
        UpstreamQuotaAmount, UpstreamQuotaWindow,
    },
};
use gateway_core::account::{QuotaAccessState, UpstreamConnection, UpstreamInfoFailure};
use gateway_protocol::peer::{PeerComponentStatus, PeerLink};

use super::{PeerInfoService, quota, snapshot};

pub(crate) fn account_capacity(
    account: &crate::model::accounts::AccountRecord,
    mut capacity: crate::model::accounts::AccountCapacity,
    quota: &ProviderQuota,
) -> crate::model::accounts::AccountCapacity {
    if account.concurrency_limit.is_none()
        && let Some(info) = &quota.upstream
        && !info.stale
        && info
            .observed_at
            .is_some_and(|at| snapshot::fresh_at(at.into(), SystemTime::now()))
        && let Some(limit) = info
            .max_concurrency
            .and_then(gateway_core::account::AccountConcurrencyLimit::from_key_limit)
    {
        capacity.total_slots = Some(u64::from(limit.get()));
    }
    capacity
}

impl PeerInfoService {
    pub(super) fn project_peer(
        connection: &UpstreamConnection,
        peer: Option<&PeerLink>,
        stored_failure: Option<UpstreamInfoFailure>,
        last_error: Option<UpstreamInfoFailure>,
        now: SystemTime,
    ) -> ProviderQuota {
        let observed_at = peer
            .as_ref()
            .and_then(|peer| snapshot::datetime(&peer.observed_at));
        let stale = peer.as_ref().is_none_or(|peer| {
            !snapshot::component_fresh(Some(&peer.observed_at), Some(&peer.expires_at), now)
        });
        let limits = peer.and_then(|peer| peer.key_limits);
        let info = ProviderUpstreamInfo {
            discovered: peer.is_some() || stored_failure == Some(UpstreamInfoFailure::Unauthorized),
            observed_at,
            stale,
            connection_revision: connection.revision.clone(),
            last_error: last_error
                .or(stored_failure)
                .map(|error| error.as_str().to_owned()),
            max_concurrency: limits.and_then(|limits| limits.max_concurrency),
            requests_per_minute: limits.and_then(|limits| limits.requests_per_minute),
            pool: peer
                .as_ref()
                .and_then(|peer| peer.pool.as_ref())
                .map(|pool| {
                    let states = &pool.states;
                    UpstreamPool {
                        account_count: pool.account_count,
                        states: UpstreamPoolStates {
                            ready: states.ready,
                            cooldown: states.cooldown,
                            exhausted: states.exhausted,
                            disabled: states.disabled,
                            error: states.error,
                            unknown: states.unknown,
                        },
                        plans: pool
                            .plans
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .map(|plan| UpstreamPlanCount {
                                name: plan.name.clone(),
                                count: plan.count,
                            })
                            .collect(),
                    }
                }),
        };
        let mut windows = Vec::new();
        if let Some(peer) = &peer {
            if let Some(component) = &peer.usage
                && component.status == PeerComponentStatus::Available
                && let Some(usage) = &component.data
                && let Some(unit) = &usage.unit
            {
                for window in &usage.windows {
                    // 不限额仍展示已提供的账本窗口，只是不计算占用百分比
                    let total = window
                        .total
                        .as_deref()
                        .and_then(snapshot::decimal)
                        .filter(|value| *value > 0.0);
                    let used_percent = snapshot::decimal(&window.used)
                        .zip(total)
                        .map(|(used, total)| (used / total * 100.0).clamp(0.0, 100.0));
                    let window_observed = component
                        .observed_at
                        .as_deref()
                        .and_then(snapshot::datetime);
                    let window_stale = stale
                        || !component
                            .observed_at
                            .as_deref()
                            .is_some_and(|value| snapshot::fresh(value, now))
                        || window
                            .reset_at
                            .as_deref()
                            .and_then(snapshot::timestamp)
                            .is_some_and(|reset| reset <= now);
                    windows.push(ProviderQuotaWindow {
                        upstream: Some(UpstreamQuotaWindow {
                            account_id: None,
                            observed_at: window_observed,
                            stale: window_stale,
                            amount: Some(UpstreamQuotaAmount {
                                unit: unit.clone(),
                                used: window.used.clone(),
                                total: window.total.clone(),
                                remaining: window.remaining.clone(),
                            }),
                        }),
                        key: format!("peer:budget:{}", window.key),
                        group: "general".to_owned(),
                        label: window.label.as_deref().unwrap_or(&window.key).to_owned(),
                        limit_id: None,
                        limit_name: None,
                        role: match window.key.as_str() {
                            "daily" => Some(ProviderQuotaWindowRole::Primary),
                            "weekly" => Some(ProviderQuotaWindowRole::Secondary),
                            "monthly" => Some(ProviderQuotaWindowRole::Monthly),
                            _ => None,
                        },
                        // Key 消费窗口覆盖此连接的全部请求，本机用量按同一时间边界聚合
                        local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
                        window_seconds: window.window_seconds,
                        used_percent,
                        reset_at: window.reset_at.as_deref().and_then(snapshot::datetime),
                        limit_reached: !window_stale
                            && quota::window_access(window, now) == QuotaAccessState::Exhausted,
                        local_usage: None,
                        provider_data: None,
                    });
                }
            }
            // 池内窗口各自保留账号归属，不能将不同套餐相加为统一消费额度
            if let Some(quota) = &peer.quota {
                for (index, account) in quota.accounts.iter().enumerate() {
                    let window_observed =
                        account.observed_at.as_deref().and_then(snapshot::datetime);
                    let window_stale = stale
                        || !snapshot::component_fresh(
                            account.observed_at.as_deref(),
                            account.expires_at.as_deref(),
                            now,
                        );
                    for window in &account.windows {
                        let group = window.group.as_deref().unwrap_or("general");
                        let window_stale = window_stale
                            || window
                                .reset_at
                                .as_deref()
                                .and_then(snapshot::timestamp)
                                .is_some_and(|reset| reset <= now);
                        let scope = format!(
                            "peer:{}:{}:{}",
                            account.id.len(),
                            account.id,
                            window.limit_id.as_deref().unwrap_or(group)
                        );
                        windows.push(ProviderQuotaWindow {
                            upstream: Some(UpstreamQuotaWindow {
                                account_id: Some(account.id.clone()),
                                observed_at: window_observed,
                                stale: window_stale,
                                amount: None,
                            }),
                            key: format!("{scope}:{}", window.key),
                            group: group.to_owned(),
                            label: window.label.as_deref().unwrap_or(&window.key).to_owned(),
                            limit_id: Some(scope),
                            limit_name: Some(match window.limit_name.as_deref() {
                                Some(name) => format!("账号 {} · {name}", index + 1),
                                None => format!("账号 {}", index + 1),
                            }),
                            role: match window.role.as_deref() {
                                Some("primary") => Some(ProviderQuotaWindowRole::Primary),
                                Some("secondary") => Some(ProviderQuotaWindowRole::Secondary),
                                Some("monthly") => Some(ProviderQuotaWindowRole::Monthly),
                                _ => None,
                            },
                            local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                            window_seconds: window.window_seconds,
                            used_percent: window.used_percent,
                            reset_at: window.reset_at.as_deref().and_then(snapshot::datetime),
                            limit_reached: !window_stale && window.limit_reached,
                            local_usage: None,
                            provider_data: None,
                        });
                    }
                }
            }
        }
        ProviderQuota {
            paused_windows: Vec::new(),
            upstream: Some(info),
            plan_type: None,
            observed_at,
            refresh_token_expires_at: None,
            windows,
            credits: None,
            limit_reached: false,
            provider_data: None,
        }
    }
}
