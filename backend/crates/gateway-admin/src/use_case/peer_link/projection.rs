//! 从已授权的本地账号事实派生公开汇总，保留原始观测时间

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use gateway_core::{
    account::{
        AccountStatus, AccountStatusFacts, ProviderAccountId, QuotaAccessState,
        resolve_account_status,
    },
    engine::execution::AuthenticatedClient,
};
use sha2::{Digest, Sha256};

use crate::{
    model::{
        accounts::{AccountRecord, AccountRuntimeSnapshot},
        peer_link::{
            PeerAccountQuota, PeerModelState, PeerModelSummary, PeerPoolSummary, PeerQuotaWindow,
        },
        provider_credentials::ProviderQuota,
    },
    ports::provider::ProviderAdminRegistry,
};

pub(super) fn pool_summary(
    accounts: &[AccountRecord],
    runtime: &AccountRuntimeSnapshot,
    providers: &ProviderAdminRegistry,
    share_plans: bool,
    now: DateTime<Utc>,
) -> PeerPoolSummary {
    let mut summary = PeerPoolSummary {
        plans: share_plans.then(BTreeMap::new),
        ..Default::default()
    };
    for account in accounts {
        match status(account, runtime, now) {
            AccountStatus::Normal => summary.ready += 1,
            AccountStatus::RateLimited => summary.cooldown += 1,
            AccountStatus::QuotaExhausted => summary.exhausted += 1,
            AccountStatus::Disabled => summary.disabled += 1,
            AccountStatus::Error => summary.error += 1,
        }
        if let Some(plans) = &mut summary.plans
            && let (Some(plan), Ok(provider)) = (
                &account.plan_type,
                providers.require(&account.provider_kind),
            )
        {
            *plans.entry(provider.plan_type_display(plan)).or_default() += 1;
        }
    }
    summary
}

pub(super) fn quota_and_models(
    client: &AuthenticatedClient,
    accounts: &[AccountRecord],
    quotas: &BTreeMap<String, ProviderQuota>,
    providers: &ProviderAdminRegistry,
    runtime: &AccountRuntimeSnapshot,
    models: &[gateway_core::routing::PublicModelId],
    now: DateTime<Utc>,
) -> (Vec<PeerAccountQuota>, Vec<PeerModelSummary>) {
    let snapshot = client.snapshot();
    let scope = client.policy().account_scope();
    let mut observations = BTreeMap::<String, Vec<Option<ModelObservation>>>::new();
    let mut result = Vec::with_capacity(accounts.len());
    for account in accounts {
        let Ok(id) = ProviderAccountId::new(account.id.clone()) else {
            continue;
        };
        let provider = providers.require(&account.provider_kind).ok();
        let account_models = models
            .iter()
            .filter_map(|model| {
                let upstream = snapshot.mapped_model(model.as_str());
                if !snapshot.allows_public_model_for_account(model, scope, &id)
                    || !account.model_access.allows(&upstream)
                {
                    return None;
                }
                let eligible = provider
                    .as_ref()
                    .and_then(|provider| provider.peer_model_eligible(account, &upstream));
                if eligible == Some(false) {
                    return None;
                }
                let observation = eligible.and_then(|_| model_observation(account, runtime, now));
                observations
                    .entry(model.as_str().to_owned())
                    .or_default()
                    .push(observation);
                (eligible == Some(true)).then(|| model.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        let quota = quotas.get(&account.id);
        let observed_at = quota.and_then(|quota| quota.observed_at);
        let expires_at = observed_at.map(|observed| {
            quota
                .into_iter()
                .flat_map(|quota| &quota.windows)
                .filter_map(|window| window.reset_at)
                .fold(observed + TimeDelta::minutes(30), DateTime::min)
        });
        // 标识只在当前 Key 内稳定，不能用原始账号 ID 跨节点或跨授权关联身份
        let mut digest = Sha256::new();
        digest.update(b"cpr.peer.account\0");
        digest.update(client.policy().plaintext_key().expose_for_auth().as_bytes());
        digest.update([0]);
        digest.update(account.id.as_bytes());
        let id = hex::encode(&digest.finalize()[..16]);
        result.push(PeerAccountQuota {
            id,
            models: account_models,
            observed_at,
            expires_at,
            windows: quota
                .into_iter()
                .flat_map(|quota| &quota.windows)
                .map(|window| PeerQuotaWindow {
                    key: window.key.clone(),
                    group: window.group.clone(),
                    label: window.label.clone(),
                    limit_id: window.limit_id.clone(),
                    limit_name: window.limit_name.clone(),
                    role: window.role,
                    window_seconds: window.window_seconds,
                    used_percent: window.used_percent,
                    reset_at: window.reset_at,
                    limit_reached: window.limit_reached,
                })
                .collect(),
        });
    }
    let summaries = models
        .iter()
        .map(|model| {
            let values = observations.remove(model.as_str()).unwrap_or_default();
            let complete = !values.is_empty() && values.iter().all(Option::is_some);
            let observed_at = values.iter().flatten().map(|value| value.observed_at).min();
            let expires_at = values.iter().flatten().map(|value| value.expires_at).min();
            let state = if values
                .iter()
                .flatten()
                .any(|value| value.state == PeerModelState::Available)
            {
                PeerModelState::Available
            } else if complete
                && values
                    .iter()
                    .flatten()
                    .all(|value| value.state == PeerModelState::Exhausted)
            {
                PeerModelState::Exhausted
            } else {
                PeerModelState::Unknown
            };
            PeerModelSummary {
                model: model.as_str().to_owned(),
                complete,
                state,
                observed_at,
                expires_at,
            }
        })
        .collect();
    (result, summaries)
}

struct ModelObservation {
    state: PeerModelState,
    observed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

fn model_observation(
    account: &AccountRecord,
    runtime: &AccountRuntimeSnapshot,
    now: DateTime<Utc>,
) -> Option<ModelObservation> {
    if !matches!(
        status(account, runtime, now),
        AccountStatus::Normal | AccountStatus::QuotaExhausted
    ) {
        return None;
    }
    let state = match account.quota.access() {
        QuotaAccessState::Allowed => PeerModelState::Available,
        QuotaAccessState::Exhausted => PeerModelState::Exhausted,
        QuotaAccessState::Unknown => return None,
    };
    let observed_at = DateTime::<Utc>::from(account.quota.observed_at()?);
    let expires_at = account
        .quota
        .reset_at()
        .map(DateTime::<Utc>::from)
        .map_or(observed_at + TimeDelta::minutes(30), |reset| {
            reset.min(observed_at + TimeDelta::minutes(30))
        })
        .min(now + TimeDelta::minutes(5));
    if observed_at > now || expires_at <= now {
        return None;
    }
    Some(ModelObservation {
        state,
        observed_at,
        expires_at,
    })
}

fn status(
    account: &AccountRecord,
    runtime: &AccountRuntimeSnapshot,
    now: DateTime<Utc>,
) -> AccountStatus {
    resolve_account_status(
        &AccountStatusFacts {
            enabled: account.enabled,
            credential_state: account.credential_state,
            access_token_expires_at: account.access_token_expires_at.map(Into::into),
            quota: account.quota,
            cooldown: runtime.cooldown.get(&account.id).copied(),
            last_error_reason: account.last_error_reason,
            last_error_message: None,
        },
        now.into(),
    )
    .status
}
