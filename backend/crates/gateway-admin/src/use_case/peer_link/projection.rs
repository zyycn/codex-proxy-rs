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

#[expect(
    clippy::too_many_arguments,
    reason = "投影显式接收已授权事实和调用边界的响应预算"
)]
pub(super) async fn quota_and_models(
    client: &AuthenticatedClient,
    accounts: &[AccountRecord],
    quotas: &BTreeMap<String, ProviderQuota>,
    providers: &ProviderAdminRegistry,
    runtime: &AccountRuntimeSnapshot,
    models: &[gateway_core::routing::PublicModelId],
    now: DateTime<Utc>,
    max_response_bytes: usize,
) -> Result<(Vec<PeerAccountQuota>, Vec<PeerModelSummary>), super::PeerLinkError> {
    let snapshot = client.snapshot();
    let scope = client.policy().account_scope();
    let mut observations = BTreeMap::<&str, ModelObservations>::new();
    let mut model_bytes = 0usize;
    let mut result = Vec::with_capacity(accounts.len());
    for (index, account) in accounts.iter().enumerate() {
        // 大池投影不能连续占住异步执行线程，取消请求也要能及时释放并发名额
        if index % 16 == 0 {
            tokio::task::yield_now().await;
        }
        let observation = model_observation(account, runtime, now);
        let Ok(id) = ProviderAccountId::new(account.id.clone()) else {
            continue;
        };
        let provider = providers.require(&account.provider_kind).ok();
        let mut account_models = Vec::new();
        for model in models {
            let upstream = snapshot.mapped_model(model.as_str());
            if !snapshot.allows_public_model_for_account(model, scope, &id)
                || !account.model_access.allows(&upstream)
            {
                continue;
            }
            let eligible = provider
                .as_ref()
                .and_then(|provider| provider.peer_model_eligible(account, &upstream));
            if eligible == Some(false) {
                continue;
            }
            observations
                .entry(model.as_str())
                .or_default()
                .observe(eligible.and(observation));
            if eligible == Some(true) {
                // 模型名是响应字节数的下界；提前拒绝必然超限的交叉展开，不误拒稀疏授权
                model_bytes = model_bytes.saturating_add(model.as_str().len());
                if model_bytes > max_response_bytes {
                    return Err(super::PeerLinkError::SnapshotTooLarge);
                }
                account_models.push(model.as_str().to_owned());
            }
        }
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
            let complete = values.observed_at.is_some() && !values.incomplete;
            let state = if values.available {
                PeerModelState::Available
            } else if complete {
                PeerModelState::Exhausted
            } else {
                PeerModelState::Unknown
            };
            PeerModelSummary {
                model: model.as_str().to_owned(),
                complete,
                state,
                observed_at: values.observed_at,
                expires_at: values.expires_at,
            }
        })
        .collect();
    Ok((result, summaries))
}

#[derive(Clone, Copy)]
struct ModelObservation {
    state: PeerModelState,
    observed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Default)]
struct ModelObservations {
    incomplete: bool,
    available: bool,
    observed_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

impl ModelObservations {
    fn observe(&mut self, observation: Option<ModelObservation>) {
        let Some(observation) = observation else {
            self.incomplete = true;
            return;
        };
        self.available |= observation.state == PeerModelState::Available;
        self.observed_at = Some(self.observed_at.map_or(observation.observed_at, |current| {
            current.min(observation.observed_at)
        }));
        self.expires_at = Some(self.expires_at.map_or(observation.expires_at, |current| {
            current.min(observation.expires_at)
        }));
    }
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
