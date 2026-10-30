//! 将已保存的互联观测投影为管理端额度与同步事实

use std::time::SystemTime;

use crate::model::{
    provider_credentials::{
        ProviderQuota, ProviderQuotaWindow, ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
    },
    upstream_info::{
        ProviderUpstreamInfo, UpstreamNode, UpstreamPlanCount, UpstreamPool, UpstreamPoolStates,
        UpstreamQuotaAmount, UpstreamQuotaWindow, UpstreamRefreshReceipt, UpstreamSync,
        UpstreamSyncState,
    },
};
use gateway_core::account::{UpstreamInfoPlan, UpstreamInfoRecord};
use gateway_protocol::peer::{
    PeerComponentStatus, PeerLink, PeerPoolDisclosure, PeerQuotaDisclosure,
};

use super::{PeerInfoService, snapshot};

impl PeerInfoService {
    pub(crate) fn project(record: &UpstreamInfoRecord, now: SystemTime) -> ProviderQuota {
        let peer = record.snapshot.as_ref().and_then(snapshot::decode);
        Self::project_peer(&record.plan, peer.as_ref(), now)
    }

    pub(super) fn project_peer(
        plan: &UpstreamInfoPlan,
        peer: Option<&PeerLink>,
        now: SystemTime,
    ) -> ProviderQuota {
        let observed_at = peer
            .as_ref()
            .and_then(|peer| snapshot::datetime(&peer.observed_at));
        let stale = peer.as_ref().is_none_or(|peer| {
            !snapshot::component_fresh(Some(&peer.observed_at), Some(&peer.expires_at), now)
        });
        let info = ProviderUpstreamInfo {
            discovered: plan.last_success_at.is_some(),
            node: peer.as_ref().map(|peer| UpstreamNode {
                id: peer.node.id.clone(),
                name: peer.node.name.clone(),
                software: peer.node.software.clone(),
            }),
            observed_at,
            stale,
            connection_revision: plan.connection.revision.clone(),
            sync: sync_state(plan, now),
            last_error: plan.last_error.map(|error| error.as_str().to_owned()),
            pool: peer
                .as_ref()
                .filter(|peer| peer.pool.disclosure == PeerPoolDisclosure::Summary)
                .and_then(|peer| {
                    let states = peer.pool.states.as_ref()?;
                    Some(UpstreamPool {
                        account_count: peer.pool.account_count?,
                        states: UpstreamPoolStates {
                            ready: states.ready,
                            cooldown: states.cooldown,
                            exhausted: states.exhausted,
                            disabled: states.disabled,
                            error: states.error,
                            unknown: states.unknown,
                        },
                        plans: peer
                            .pool
                            .plans
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .map(|plan| UpstreamPlanCount {
                                name: plan.name.clone(),
                                count: plan.count,
                            })
                            .collect(),
                    })
                }),
        };
        let mut windows = Vec::new();
        if let Some(peer) = &peer {
            if peer.usage.status == PeerComponentStatus::Available
                && let Some(usage) = &peer.usage.data
            {
                for (key, label, seconds, window) in [
                    ("daily", "日额度", 86400, &usage.daily),
                    ("weekly", "周额度", 604800, &usage.weekly),
                ] {
                    let total = window.total.as_deref().and_then(snapshot::decimal);
                    if total.is_none_or(|value| value <= 0.0) {
                        continue;
                    }
                    let used_percent = snapshot::decimal(&window.used)
                        .zip(total)
                        .map(|(used, total)| (used / total * 100.0).clamp(0.0, 100.0));
                    let window_observed = peer
                        .usage
                        .observed_at
                        .as_deref()
                        .and_then(snapshot::datetime);
                    let window_stale = stale
                        || !peer
                            .usage
                            .observed_at
                            .as_deref()
                            .is_some_and(|value| snapshot::fresh(value, now))
                        || window
                            .resets_at
                            .as_deref()
                            .and_then(snapshot::timestamp)
                            .is_some_and(|reset| reset <= now);
                    windows.push(ProviderQuotaWindow {
                        upstream: Some(UpstreamQuotaWindow {
                            account_id: None,
                            observed_at: window_observed,
                            stale: window_stale,
                            amount: Some(UpstreamQuotaAmount {
                                unit: usage.unit.clone(),
                                used: window.used.clone(),
                                total: window.total.clone(),
                                remaining: window.remaining.clone(),
                            }),
                        }),
                        key: format!("peer:budget:{key}"),
                        group: "general".to_owned(),
                        label: label.to_owned(),
                        limit_id: None,
                        limit_name: None,
                        role: Some(if key == "weekly" {
                            ProviderQuotaWindowRole::Secondary
                        } else {
                            ProviderQuotaWindowRole::Primary
                        }),
                        // Key 消费窗口覆盖此连接的全部请求，本机用量按同一时间边界聚合
                        local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
                        window_seconds: Some(seconds),
                        used_percent,
                        reset_at: window.resets_at.as_deref().and_then(snapshot::datetime),
                        limit_reached: !window_stale && snapshot::budget_exhausted(window, now),
                        local_usage: None,
                        provider_data: None,
                    });
                }
            }
            // 池内窗口各自保留账号归属，不能将不同套餐相加为统一消费额度
            if peer.quota.disclosure == PeerQuotaDisclosure::Accounts {
                for (index, account) in peer.quota.accounts.iter().enumerate() {
                    let window_observed =
                        account.observed_at.as_deref().and_then(snapshot::datetime);
                    let window_stale = stale
                        || !snapshot::component_fresh(
                            account.observed_at.as_deref(),
                            account.expires_at.as_deref(),
                            now,
                        );
                    for window in &account.windows {
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
                            window.limit_id.as_deref().unwrap_or(&window.group)
                        );
                        windows.push(ProviderQuotaWindow {
                            upstream: Some(UpstreamQuotaWindow {
                                account_id: Some(account.id.clone()),
                                observed_at: window_observed,
                                stale: window_stale,
                                amount: None,
                            }),
                            key: format!("{scope}:{}", window.key),
                            group: window.group.clone(),
                            label: window.label.clone(),
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

    pub(crate) fn receipt(record: &UpstreamInfoPlan) -> UpstreamRefreshReceipt {
        UpstreamRefreshReceipt {
            account_id: record.connection.account_id.as_str().to_owned(),
            connection_revision: record.connection.revision.clone(),
            request_generation: record.request_generation,
            state: sync_state(record, SystemTime::now()).state,
        }
    }
}

fn sync_state(record: &UpstreamInfoPlan, now: SystemTime) -> UpstreamSync {
    UpstreamSync {
        state: if record.claimed_until.is_some_and(|expiry| expiry > now) {
            UpstreamSyncState::Running
        } else if record.request_generation > record.completed_generation {
            UpstreamSyncState::Queued
        } else {
            UpstreamSyncState::Idle
        },
        request_generation: record.request_generation,
        completed_generation: record.completed_generation,
    }
}
