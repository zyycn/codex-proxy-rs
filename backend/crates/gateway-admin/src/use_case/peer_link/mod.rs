//! CPR 互联只读取认证范围内的本地事实，不参与推理准入或上游刷新

mod projection;
mod rate_limit;
pub mod upstream;

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use gateway_core::{engine::execution::AuthenticatedClient, policy::ClientPeerSharing};
use tokio::sync::Semaphore;

use crate::{
    model::{
        AdminError,
        peer_link::{PeerLinkExport, PeerLinkLimits},
        provider_credentials::ProviderQuotaReadRequest,
    },
    ports::{
        provider::ProviderAdminRegistry,
        store::{AccountRuntimeStore, AccountStore, ClientKeyStore},
    },
};

use super::map_store_error;

const MAX_CONCURRENT_SNAPSHOTS: usize = 8;

/// 查询失败保持独立于推理错误，不改变账号或 Key 状态
#[derive(Debug, thiserror::Error)]
pub enum PeerLinkError {
    #[error("client API key is unavailable")]
    Unauthorized,
    #[error("peer sharing is not allowed")]
    Forbidden,
    #[error("peer query rate limit reached")]
    RateLimited { retry_after: u64 },
    #[error("peer snapshot exceeds the supported bounds")]
    SnapshotTooLarge,
    #[error("peer snapshot is unavailable")]
    Unavailable(#[from] AdminError),
}

#[async_trait]
pub trait PeerLinkService: Send + Sync {
    async fn snapshot(
        &self,
        client: &AuthenticatedClient,
        limits: PeerLinkLimits,
    ) -> Result<PeerLinkExport, PeerLinkError>;
}

pub(crate) struct DefaultPeerLinkService {
    keys: Arc<dyn ClientKeyStore>,
    accounts: Arc<dyn AccountStore>,
    runtime: Arc<dyn AccountRuntimeStore>,
    providers: ProviderAdminRegistry,
    rate_limit: rate_limit::QueryRateLimit,
    snapshot_slots: Semaphore,
}

impl DefaultPeerLinkService {
    pub(crate) fn new(
        keys: Arc<dyn ClientKeyStore>,
        accounts: Arc<dyn AccountStore>,
        runtime: Arc<dyn AccountRuntimeStore>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            keys,
            accounts,
            runtime,
            providers,
            rate_limit: Default::default(),
            snapshot_slots: Semaphore::new(MAX_CONCURRENT_SNAPSHOTS),
        }
    }
}

#[async_trait]
impl PeerLinkService for DefaultPeerLinkService {
    async fn snapshot(
        &self,
        client: &AuthenticatedClient,
        limits: PeerLinkLimits,
    ) -> Result<PeerLinkExport, PeerLinkError> {
        self.rate_limit.acquire(client.policy().key_id())?;
        let frozen_sharing = client.policy().peer_sharing();
        // 当前权限只能收窄冻结范围，重查询在读取 Key 分组前拒绝且不排队
        let _permit = if frozen_sharing.share_pool || frozen_sharing.share_quota {
            Some(
                self.snapshot_slots
                    .try_acquire()
                    .map_err(|_| PeerLinkError::RateLimited { retry_after: 1 })?,
            )
        } else {
            None
        };
        let key = self
            .keys
            .get_client_key(client.policy().key_id())
            .await
            .map_err(|error| map_store_error(error, "client API key"))?
            .filter(|key| key.enabled)
            .ok_or(PeerLinkError::Unauthorized)?;
        let budget_observed_at = Utc::now();
        let sharing = intersect_sharing(frozen_sharing, key.peer_sharing);
        if !sharing.enabled || key.groups.is_empty() {
            return Err(PeerLinkError::Forbidden);
        }
        let node = self
            .keys
            .peer_node_identity()
            .await
            .map_err(|error| map_store_error(error, "peer node"))?;
        let now = Utc::now();
        let mut result = PeerLinkExport {
            node,
            observed_at: now,
            expires_at: now + TimeDelta::minutes(5),
            budget: key.budget,
            budget_observed_at,
            pool: None,
            quota: None,
            models: Vec::new(),
        };
        if !sharing.share_pool && !sharing.share_quota {
            return Ok(result);
        }

        let ids = client
            .policy()
            .account_scope()
            .allowed_account_ids()
            .take(limits.max_accounts.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        if ids.len() > limits.max_accounts {
            return Err(PeerLinkError::SnapshotTooLarge);
        }
        let public_models = if sharing.share_quota {
            client
                .snapshot()
                .public_models_for_scope(client.policy().account_scope())
        } else {
            Vec::new()
        };
        if public_models.len() > limits.max_models {
            return Err(PeerLinkError::SnapshotTooLarge);
        }
        let groups = key
            .groups
            .iter()
            .filter(|group| group.enabled)
            .map(|group| group.id.clone())
            .collect::<Vec<_>>();
        let accounts = self
            .accounts
            .load_accounts_in_enabled_groups(&ids, &groups)
            .await
            .map_err(|error| map_store_error(error, "peer accounts"))?
            .into_iter()
            .filter(|account| {
                self.providers
                    .require(&account.provider_kind)
                    .is_ok_and(|provider| provider.is_direct_account(&account.authentication_kind))
            })
            .collect::<Vec<_>>();
        let account_ids = accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let runtime = self
            .runtime
            .account_rate_limits(&account_ids)
            .await
            .map_err(|error| map_store_error(error, "peer account runtime"))?;
        let mut quotas = BTreeMap::new();
        if sharing.share_quota {
            let mut window_count = 0usize;
            let mut by_provider = BTreeMap::new();
            for account in &accounts {
                by_provider
                    .entry(&account.provider_kind)
                    .or_insert_with(Vec::new)
                    .push(account);
            }
            for (kind, accounts) in by_provider {
                let Ok(provider) = self.providers.require(kind) else {
                    continue;
                };
                let requests = accounts
                    .iter()
                    .map(|account| ProviderQuotaReadRequest {
                        account,
                        rolling_usage: None,
                    })
                    .collect::<Vec<_>>();
                for (account, quota) in accounts
                    .into_iter()
                    .zip(provider.read_quotas(&requests).await)
                {
                    if let Ok(quota) = quota {
                        window_count = window_count.saturating_add(quota.windows.len());
                        if window_count > limits.max_quota_windows {
                            return Err(PeerLinkError::SnapshotTooLarge);
                        }
                        quotas.insert(account.id.clone(), quota);
                    }
                }
            }
        }
        let now = Utc::now();
        result.observed_at = now;
        result.expires_at = now + TimeDelta::minutes(5);
        if sharing.share_pool {
            result.pool = Some(projection::pool_summary(
                &accounts,
                &runtime,
                &self.providers,
                sharing.share_plans,
                now,
            ));
        }
        if sharing.share_quota {
            let (quota, mut models) = projection::quota_and_models(
                client,
                &accounts,
                &quotas,
                &self.providers,
                &runtime,
                &public_models,
                now,
            );
            // 发布窗口内缺失或已变更的账号不能被静默剔除后推导整个模型池耗尽
            if accounts.len() != ids.len() {
                for model in &mut models {
                    model.complete = false;
                    if model.state == crate::model::peer_link::PeerModelState::Exhausted {
                        model.state = crate::model::peer_link::PeerModelState::Unknown;
                    }
                }
            }
            result.quota = Some(quota);
            result.models = models;
        }
        Ok(result)
    }
}

fn intersect_sharing(frozen: ClientPeerSharing, current: ClientPeerSharing) -> ClientPeerSharing {
    ClientPeerSharing {
        enabled: frozen.enabled && current.enabled,
        share_pool: frozen.share_pool && current.share_pool,
        share_quota: frozen.share_quota && current.share_quota,
        share_plans: frozen.share_plans && current.share_plans,
    }
}
